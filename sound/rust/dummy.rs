// SPDX-License-Identifier: GPL-2.0

//! Rust dummy sound driver sample.
//!
//! Demonstrates the Rust ALSA bindings: card creation, PCM device, and mixer
//! controls backed by a faux device.  An hrtimer fires every PCM period to
//! call `snd_pcm_period_elapsed()` and simulate real-time playback/capture.
//!
//! After loading the module:
//!
//! ```sh
//! cat /proc/asound/cards
//! aplay -l
//! amixer -c RustDummy contents
//! ```

use kernel::{
    faux,
    impl_has_hr_timer,
    prelude::*,
    Module,
    sound::control::{ElemInfo, ElemType, ElemValue, KControlOps},
    sound::info,
    sound::pcm,
    sync::{
        atomic::{ordering, Atomic},
        new_mutex, Arc, ArcBorrow, Mutex,
    },
    time::{
        hrtimer::{
            ArcHrTimerHandle, HrTimer, HrTimerCallback, HrTimerCallbackContext, HrTimerPointer,
            HrTimerRestart, RelativeMode,
        },
        Delta, Monotonic,
    },
    bindings,
};

//
// PCM hardware capabilities
//
const DUMMY_HW: pcm::Hardware = pcm::Hardware {
    info: (bindings::SNDRV_PCM_INFO_MMAP
        | bindings::SNDRV_PCM_INFO_INTERLEAVED
        | bindings::SNDRV_PCM_INFO_MMAP_VALID
        | bindings::SNDRV_PCM_INFO_RESUME) as u32,
    formats: (bindings::SNDRV_PCM_FMTBIT_S16_LE
              | bindings::SNDRV_PCM_FMTBIT_S32_LE) as u64,
    rates: (bindings::SNDRV_PCM_RATE_44100 | bindings::SNDRV_PCM_RATE_48000) as u32,
    rate_min: 44100,
    rate_max: 48000,
    channels_min: 1,
    channels_max: 2,
    buffer_bytes_max: 65536,
    period_bytes_min: 64,
    period_bytes_max: 65536,
    periods_min: 1,
    periods_max: 1024,
    fifo_size: 0,
};

//
// Timer state (one per chip, shared between PCM ops and hrtimer callback)
//
/// Per-chip timer state.  Lives inside an `Arc` so the hrtimer callback can
/// borrow it after `trigger()` returns.
#[pin_data]
struct DummyTimer {
    /// Intrusive hrtimer node.
    #[pin]
    timer: HrTimer<DummyTimer>,
    /// Substream currently running, or inactive when stopped.
    substream: pcm::AtomicSubstreamHandle,
    /// Period duration in nanoseconds (set at trigger-start).
    period_ns: Atomic<i64>,
    /// Period size in frames (for pointer() calculation).
    period_size: Atomic<bindings::snd_pcm_uframes_t>,
    /// Buffer size in frames (for pointer() wrap-around).
    buffer_size: Atomic<bindings::snd_pcm_uframes_t>,
    /// Count of periods elapsed since trigger-start.
    elapsed: Atomic<u64>,
}


impl DummyTimer {
    fn new() -> impl PinInit<Self, Error> {
        try_pin_init!(Self {
            timer <- HrTimer::new(),
            substream: pcm::AtomicSubstreamHandle::new(),
            period_ns: Atomic::new(0),
            period_size: Atomic::new(0),
            buffer_size: Atomic::new(0),
            elapsed: Atomic::new(0),
        })
    }
}

impl HrTimerCallback for DummyTimer {
    type Pointer<'a> = Arc<Self>;

    fn run(this: ArcBorrow<'_, Self>, mut ctx: HrTimerCallbackContext<'_, Self>) -> HrTimerRestart {
        if !this.substream.is_active(ordering::Acquire) {
            return HrTimerRestart::NoRestart;
        }
        let period_ns = this.period_ns.load(ordering::Relaxed);
        ctx.forward_now(Delta::from_nanos(period_ns));
        this.elapsed.fetch_add(1, ordering::Relaxed);
        this.substream.period_elapsed(ordering::Acquire);
        // period_elapsed() may have called trigger(Stop) via snd_pcm_drain_done
        // (stream end), which clears substream.  Re-check so we don't re-arm
        // a stopped timer.
        if this.substream.is_active(ordering::Acquire) {
            HrTimerRestart::Restart
        } else {
            HrTimerRestart::NoRestart
        }
    }
}

impl_has_hr_timer! {
    impl HasHrTimer<DummyTimer> for DummyTimer {
        mode: RelativeMode<Monotonic>,
        field: self.timer
    }
}

//
// Chip state
//
struct ChipState {
    /// Mixer state
    master_volume: i64,
    master_switch: bool,
    /// Keeps the timer alive while a stream is running.
    timer_handle: Option<ArcHrTimerHandle<DummyTimer>>,
    /// Mutable copy of the PCM hardware constraints (updated via proc).
    pcm_hw: pcm::Hardware,
}

impl ChipState {
    const fn new() -> Self {
        Self {
            master_volume: 70,
            master_switch: true,
            timer_handle: None,
            pcm_hw: DUMMY_HW,
        }
    }
}

//
// Chip: PCM ops + chip state + reference to timer
//
#[pin_data]
struct DummyChip {
    #[pin]
    chip: Mutex<ChipState>,
    /// Shared timer state; the Arc is cloned to start the hrtimer.
    timer: Arc<DummyTimer>,
}

impl DummyChip {
    fn new(timer: Arc<DummyTimer>) -> impl PinInit<Self, Error> {
        try_pin_init!(Self {
            chip <- new_mutex!(ChipState::new()),
            timer: timer,
        })
    }
}

impl pcm::Ops for DummyChip {
    // trigger() takes the chip Mutex (a sleeping lock), so it must run in a
    // sleepable (non-atomic) context.
    const NONATOMIC: bool = true;

    fn open(&self, substream: &pcm::Substream) -> Result {
        let hw = self.chip.lock().pcm_hw;
        substream.runtime().set_hw(&hw);
        Ok(())
    }

    fn trigger(&self, substream: &pcm::Substream, cmd: pcm::TriggerCommand) -> Result {
        match cmd {
            pcm::TriggerCommand::Start |
            pcm::TriggerCommand::Resume => {
                let runtime = substream.runtime();
                let rate = runtime.rate() as i64;
                let period_size = runtime.period_size();
                let buffer_size = runtime.buffer_size();

                let period_ns = if rate > 0 {
                    (period_size as i64).saturating_mul(1_000_000_000) / rate
                } else {
                    20_000_000 // 20 ms fallback
                };

                self.timer.period_size.store(period_size, ordering::Relaxed);
                self.timer.buffer_size.store(buffer_size, ordering::Relaxed);
                self.timer.period_ns.store(period_ns, ordering::Relaxed);
                self.timer.elapsed.store(0, ordering::Relaxed);
                // Publish substream pointer before starting the timer.
                self.timer.substream.store(substream, ordering::Release);

                let handle = self.timer.clone().start(Delta::from_nanos(period_ns));
                self.chip.lock().timer_handle = Some(handle);
            }
            pcm::TriggerCommand::Stop |
            pcm::TriggerCommand::PausePush |
            pcm::TriggerCommand::Suspend => {
                // Clear substream so the callback returns NoRestart on its
                // next (or current) invocation.
                self.timer.substream.clear(ordering::Release);
                // Use try-cancel (non-blocking) so this is safe when called
                // from within the hrtimer callback (e.g. via
                // snd_pcm_drain_done -> snd_pcm_do_stop).  hrtimer_cancel
                // would deadlock on the hrtimer base lock already held by the
                // interrupt.  The handle is kept alive so sync_stop() can
                // complete the blocking synchronisation from process context.
                if let Some(handle) = self.chip.lock().timer_handle.as_mut() {
                    handle.try_cancel();
                }
            }
            pcm::TriggerCommand::PauseRelease => {
                // Resume: re-arm the timer with the stored period.
                let period_ns = self.timer.period_ns.load(ordering::Relaxed);
                self.timer.substream.store(substream, ordering::Release);
                let handle = self.timer.clone().start(Delta::from_nanos(period_ns));
                self.chip.lock().timer_handle = Some(handle);
            }
            _ => {}
        }
        Ok(())
    }

    fn sync_stop(&self, _substream: &pcm::Substream) -> Result {
        // Called from process context after trigger(Stop).  Drop the handle
        // here to invoke hrtimer_cancel(), which blocks until any in-flight
        // callback has returned.  Safe here because we are not in IRQ context.
        self.chip.lock().timer_handle = None;
        Ok(())
    }

    fn pointer(&self, _substream: &pcm::Substream) -> bindings::snd_pcm_uframes_t {
        let elapsed = self.timer.elapsed.load(ordering::Relaxed);
        let period_size = self.timer.period_size.load(ordering::Relaxed);
        let buffer_size = self.timer.buffer_size.load(ordering::Relaxed);
        if buffer_size == 0 || period_size == 0 {
            return 0;
        }
        // Return the frame position at the start of the current period.
        ((elapsed as u64).wrapping_mul(period_size as u64) % buffer_size as u64)
            as bindings::snd_pcm_uframes_t
    }
}

static DUMMY_PCM_OPS: pcm::OpsTable<DummyChip> = pcm::OpsTable::new();

//
// Mixer controls
//
struct MasterVolume(Arc<DummyChip>);

impl MasterVolume {
    fn chip(&self) -> &DummyChip {
        &self.0
    }
}

impl KControlOps for MasterVolume {
    fn info(&self, info: &mut ElemInfo) -> Result {
        info.set_type_count(ElemType::Integer, 1);
        info.set_integer_range(0, 100, 1);
        Ok(())
    }

    fn get(&self, value: &mut ElemValue) -> Result {
        let v = self.chip().chip.lock().master_volume;
        value.set_integer(0, v as c_long);
        Ok(())
    }

    fn put(&self, value: &ElemValue) -> Result<bool> {
        let new_val = (value.integer(0) as i64).clamp(0, 100);
        let mut state = self.chip().chip.lock();
        if state.master_volume == new_val {
            return Ok(false);
        }
        state.master_volume = new_val;
        Ok(true)
    }
}

struct MasterSwitch(Arc<DummyChip>);

impl MasterSwitch {
    fn chip(&self) -> &DummyChip {
        &self.0
    }
}

impl KControlOps for MasterSwitch {
    fn info(&self, info: &mut ElemInfo) -> Result {
        info.set_type_count(ElemType::Boolean, 1);
        Ok(())
    }

    fn get(&self, value: &mut ElemValue) -> Result {
        let v = self.chip().chip.lock().master_switch;
        value.set_boolean(0, v);
        Ok(())
    }

    fn put(&self, value: &ElemValue) -> Result<bool> {
        let new_val = value.boolean(0);
        let mut state = self.chip().chip.lock();
        if state.master_switch == new_val {
            return Ok(false);
        }
        state.master_switch = new_val;
        Ok(true)
    }
}

//
// Proc interface (CONFIG_SND_DEBUG only, matching the C driver)
//
#[cfg(CONFIG_SND_DEBUG = "y")]
static DUMMY_PROC_OPS: info::TextOpsTable<DummyChip> = info::TextOpsTable::new();

/// Return the slice up to the first NUL byte (or the whole slice).
#[cfg(CONFIG_SND_DEBUG = "y")]
fn cstr_slice(s: &[u8]) -> &[u8] {
    match s.iter().position(|&b| b == 0) {
        Some(n) => &s[..n],
        None    => s,
    }
}

/// Parse a u64 from a C-string slice (handles `0x` hex prefix or decimal).
#[cfg(CONFIG_SND_DEBUG = "y")]
fn parse_u64(s: &[u8]) -> Option<u64> {
    let s = core::str::from_utf8(s).ok()?.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()
    } else {
        s.parse().ok()
    }
}

#[cfg(CONFIG_SND_DEBUG = "y")]
impl info::TextOps for DummyChip {
    fn read(&self, buf: &mut info::InfoBuffer) {
        use core::fmt::Write;
        let hw = self.chip.lock().pcm_hw;

        // formats: hex bitmask + human-readable format names
        let _ = write!(buf, "formats {:#x}", hw.formats);
        for i in 0i32..64 {
            if hw.formats & (1u64 << i) != 0 {
                let _ = write!(buf, " ");
                if let Ok(s) = pcm::format_name(i).to_str() {
                    let _ = write!(buf, "{}", s);
                }
            }
        }
        let _ = writeln!(buf);

        // rates: hex bitmask + human-readable rate names
        let _ = write!(buf, "rates {:#x}", hw.rates);
        if hw.rates & bindings::SNDRV_PCM_RATE_CONTINUOUS != 0 {
            let _ = write!(buf, " continuous");
        }
        if hw.rates & bindings::SNDRV_PCM_RATE_KNOT != 0 {
            let _ = write!(buf, " knot");
        }
        const RATES: &[(u32, u32)] = &[
            (bindings::SNDRV_PCM_RATE_5512,   5512),
            (bindings::SNDRV_PCM_RATE_8000,   8000),
            (bindings::SNDRV_PCM_RATE_11025,  11025),
            (bindings::SNDRV_PCM_RATE_16000,  16000),
            (bindings::SNDRV_PCM_RATE_22050,  22050),
            (bindings::SNDRV_PCM_RATE_32000,  32000),
            (bindings::SNDRV_PCM_RATE_44100,  44100),
            (bindings::SNDRV_PCM_RATE_48000,  48000),
            (bindings::SNDRV_PCM_RATE_64000,  64000),
            (bindings::SNDRV_PCM_RATE_88200,  88200),
            (bindings::SNDRV_PCM_RATE_96000,  96000),
            (bindings::SNDRV_PCM_RATE_176400, 176400),
            (bindings::SNDRV_PCM_RATE_192000, 192000),
        ];
        for &(bit, hz) in RATES {
            if hw.rates & bit != 0 {
                let _ = write!(buf, " {}", hz);
            }
        }
        let _ = writeln!(buf);

        let _ = writeln!(buf, "rate_min {}", hw.rate_min);
        let _ = writeln!(buf, "rate_max {}", hw.rate_max);
        let _ = writeln!(buf, "channels_min {}", hw.channels_min);
        let _ = writeln!(buf, "channels_max {}", hw.channels_max);
        let _ = writeln!(buf, "buffer_bytes_max {}", hw.buffer_bytes_max);
        let _ = writeln!(buf, "period_bytes_min {}", hw.period_bytes_min);
        let _ = writeln!(buf, "period_bytes_max {}", hw.period_bytes_max);
        let _ = writeln!(buf, "periods_min {}", hw.periods_min);
        let _ = writeln!(buf, "periods_max {}", hw.periods_max);
    }

    fn write(&self, buf: &mut info::InfoBuffer) {
        let mut line = [0u8; 64];
        while buf.get_line(&mut line) {
            let mut name_buf = [0u8; 20];
            let rest = buf.get_str(&mut name_buf, &line);
            let name = cstr_slice(&name_buf);

            let mut val_buf = [0u8; 20];
            buf.get_str(&mut val_buf, rest);
            let Some(val) = parse_u64(cstr_slice(&val_buf)) else { continue };

            let mut hw = self.chip.lock();
            match name {
                b"formats"          => hw.pcm_hw.formats          = val,
                b"rates"            => hw.pcm_hw.rates             = val as u32,
                b"rate_min"         => hw.pcm_hw.rate_min          = val as u32,
                b"rate_max"         => hw.pcm_hw.rate_max          = val as u32,
                b"channels_min"     => hw.pcm_hw.channels_min      = val as u32,
                b"channels_max"     => hw.pcm_hw.channels_max      = val as u32,
                b"buffer_bytes_max" => hw.pcm_hw.buffer_bytes_max  = val as usize,
                b"period_bytes_min" => hw.pcm_hw.period_bytes_min  = val as usize,
                b"period_bytes_max" => hw.pcm_hw.period_bytes_max  = val as usize,
                b"periods_min"      => hw.pcm_hw.periods_min       = val as u32,
                b"periods_max"      => hw.pcm_hw.periods_max       = val as u32,
                _ => {}
            }
        }
    }
}

//
// Module
//
struct DummySoundModule {
    _chip: Arc<DummyChip>,
    _reg: faux::Registration,
}

impl Module for DummySoundModule {
    fn init(_module: &'static ThisModule) -> Result<Self> {
        let reg = faux::Registration::new(c"rust-sound-dummy", None)?;
        let dev = reg.as_ref();

        let timer = Arc::pin_init(DummyTimer::new(), GFP_KERNEL)?;
        let chip = Arc::pin_init(DummyChip::new(timer), GFP_KERNEL)?;

        let card = kernel::new_sound_card!(dev.as_ref(), c"RustDummy")?;
        card.set_driver(c"RustDummySnd");
        card.set_short_name(c"Rust Dummy");
        card.set_long_name(c"Rust Dummy Sound Card");
        card.set_mixer_name(c"Rust Dummy Mixer");

        let pcm = pcm::Pcm::new(card, c"Rust Dummy PCM", 0, 1, 1)?;
        pcm.set_private_data_arc(chip.clone());
        pcm.set_ops::<DummyChip>(pcm::StreamDir::Playback, &DUMMY_PCM_OPS);
        pcm.set_ops::<DummyChip>(pcm::StreamDir::Capture, &DUMMY_PCM_OPS);
        pcm.set_managed_buffer_continuous(65536)?;

        card.add_mixer_control(c"Master Playback Volume", MasterVolume(chip.clone()))?;
        card.add_mixer_control(c"Master Playback Switch", MasterSwitch(chip.clone()))?;

        #[cfg(CONFIG_SND_DEBUG = "y")]
        card.rw_proc_new(c"dummy_pcm", chip.clone(), &DUMMY_PROC_OPS)?;

        card.register()?;

        dev_info!(dev, "Rust dummy sound card registered\n");

        Ok(Self {
            _chip: chip,
            _reg: reg,
        })
    }
}

impl Drop for DummySoundModule {
    fn drop(&mut self) {
        pr_info!("Rust dummy sound driver: card removed\n");
    }
}

module! {
    type: DummySoundModule,
    name: "snd_rust_dummy",
    authors: ["Rust for Linux contributors"],
    description: "Rust dummy ALSA sound driver",
    license: "GPL",
}
