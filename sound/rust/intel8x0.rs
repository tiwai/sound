// SPDX-License-Identifier: GPL-2.0

//! Rust Intel ICH AC97 sound driver.
//!
//! Targets the QEMU `intel-ac97` / `ich9-intel-hda` ICH emulation.
//! Implements:
//!   - Two PCM devices: device 0 (playback + capture), device 1 (mic capture)
//!   - AC97 codec mixer via snd_ac97_mixer()
//!   - PCI INTx IRQ handler
//!   - Basic PM suspend/resume
//!
//! Skipped vs C driver: ICH4 extras, SiS7012, NForce, ALi, modem, SPDIF DMA,
//! multi-channel surround, quirk tables.

use kernel::{
    bindings,
    device,
    io::Io,
    irq,
    pci,
    prelude::*,
    sound::{self, ac97, info, pcm},
    sync::{new_spinlock, Arc, SpinLock},
    time::delay::{fsleep, udelay},
};

//
// Register map - busmaster (BAR1) offsets
//
// Per-channel offsets within each channel block
const ICH_REG_OFF_BDBAR: u32 = 0x00; // dword: BD list base address
const ICH_REG_OFF_CIV:   u32 = 0x04; // byte:  current index value
const ICH_REG_OFF_LVI:   u32 = 0x05; // byte:  last valid index
const ICH_REG_OFF_SR:    u32 = 0x06; // byte:  status register
const ICH_REG_OFF_PICB:  u32 = 0x08; // word:  position in current buffer
const ICH_REG_OFF_CR:    u32 = 0x0b; // byte:  control register

// Channel base offsets in busmaster space
const ICH_REG_PI_BASE: u32 = 0x00; // PCM in  (capture)
const ICH_REG_PO_BASE: u32 = 0x10; // PCM out (playback)
const ICH_REG_MC_BASE: u32 = 0x20; // Mic in

// Global control / status
const ICH_REG_GLOB_CNT: u32 = 0x2c;
const ICH_REG_GLOB_STA: u32 = 0x30;
const ICH_REG_ACC_SEMA: u32 = 0x34;

// Status register bits (ICH_REG_OFF_SR)
const ICH_FIFOE: u8 = 0x10; // FIFO error
const ICH_BCIS:  u8 = 0x08; // buffer completion interrupt status
const ICH_LVBCI: u8 = 0x04; // last valid buffer completion interrupt
const ICH_DCH:   u8 = 0x01; // DMA controller halted

// Control register bits (ICH_REG_OFF_CR)
const ICH_IOCE:      u8 = 0x10; // interrupt on completion enable
const ICH_STARTBM:   u8 = 0x01; // start busmaster
const ICH_RESETREGS: u8 = 0x02; // reset busmaster registers

// Global control bits (ICH_REG_GLOB_CNT)
const ICH_PCM_246_MASK: u32 = 0x0030_0000; // channel count mask
const ICH_ACLINK:       u32 = 0x0000_0008; // AC-link shutdown
const ICH_AC97WARM:     u32 = 0x0000_0004; // warm reset
const ICH_AC97COLD:     u32 = 0x0000_0002; // cold reset

// Global status bits (ICH_REG_GLOB_STA)
const ICH_PCR:    u32 = 0x0000_0100; // primary codec ready   (SDIN0)
const ICH_SCR:    u32 = 0x0000_0200; // secondary codec ready (SDIN1)
const ICH_RCS:    u32 = 0x0000_8000; // read completion status (timeout)
const ICH_MCINT:  u32 = 0x0000_0080; // MIC capture interrupt
const ICH_POINT:  u32 = 0x0000_0040; // PCM out interrupt
const ICH_PIINT:  u32 = 0x0000_0020; // PCM in interrupt
const ICH_GSCI:   u32 = 0x0000_0001; // GPI status change interrupt

// Codec semaphore
const ICH_CAS: u8 = 0x01;

// BD table constants
const ICH_MAX_FRAGS: usize = 32;
const ICH_LVI_MASK:  usize = ICH_MAX_FRAGS - 1;

// AC97 scap flags
const AC97_SCAP_SKIP_MODEM:  u32 = 1 << 5;
const AC97_SCAP_POWER_SAVE:  u32 = 1 << 11;

//
// Channel indices
//
const ICHD_PCMIN:  usize = 0;
const ICHD_PCMOUT: usize = 1;
const ICHD_MIC:    usize = 2;
const ICHD_COUNT:  usize = 3;

// INFO flags for snd_pcm_hardware.info
const INFO_FLAGS: u32 = bindings::SNDRV_PCM_INFO_MMAP
    | bindings::SNDRV_PCM_INFO_INTERLEAVED
    | bindings::SNDRV_PCM_INFO_BLOCK_TRANSFER
    | bindings::SNDRV_PCM_INFO_MMAP_VALID
    | bindings::SNDRV_PCM_INFO_PAUSE
    | bindings::SNDRV_PCM_INFO_RESUME;

//
// Per-channel DMA state (protected by SpinLock in ChipState)
//
struct Ichdev {
    reg_offset: u32,       // channel register block offset in busmaster space
    bdbar_area:  usize,    // virtual addr of BD table for this channel (as usize)
    bdbar_addr:  u32,      // DMA bus address of BD table
    physbuf:     u32,      // DMA bus address of current PCM buffer
    size:        u32,      // total buffer bytes
    fragsize:    u32,      // period bytes
    fragsize1:   u32,      // effective period bytes (= fragsize/2 if size==fragsize)
    frags:       u32,      // number of fragments
    lvi:         usize,    // last valid index (0..31)
    lvi_frag:    usize,    // which frag lvi points at
    civ:         usize,    // current index value shadow
    ack:         i32,      // period elapsed countdown
    ack_reload:  i32,      // countdown reload value
    position:    u32,      // byte position of start of current period
    substream:   pcm::SubstreamHandle,
    prepared:    bool,
    int_sta_mask: u32,     // GLOB_STA bit for this channel
}


impl Ichdev {
    const fn new(
        reg_offset: u32,
        bdbar_area: usize,
        bdbar_addr: u32,
        int_sta_mask: u32,
    ) -> Self {
        Ichdev {
            reg_offset,
            bdbar_area,
            bdbar_addr,
            physbuf: 0,
            size: 0,
            fragsize: 0,
            fragsize1: 0,
            frags: 0,
            lvi: 0,
            lvi_frag: 0,
            civ: 0,
            ack: 0,
            ack_reload: 0,
            position: 0,
            substream: pcm::SubstreamHandle::none(),
            prepared: false,
            int_sta_mask,
        }
    }
}

struct ChipState {
    ichdevs: [Ichdev; ICHD_COUNT],
}

//
// Chip struct
//
#[pin_data]
struct Intel8x0 {
    addr: pci::DevresBar<0x100>,  // AC97 mixer registers (BAR0)
    bm:   pci::DevresBar<0x40>,   // busmaster DMA registers (BAR1)
    #[pin]
    state: SpinLock<ChipState>,
}

//
// I/O port helpers
//
impl Intel8x0 {
    // AC97 mixer register space (BAR0)
    fn addr_inw(&self, offset: u32) -> u16 {
        self.addr.try_access().expect("intel8x0: AC97 BAR revoked")
            .try_read16(offset as usize).expect("intel8x0: AC97 I/O offset out of range")
    }
    fn addr_outw(&self, val: u16, offset: u32) {
        self.addr.try_access().expect("intel8x0: AC97 BAR revoked")
            .try_write16(val, offset as usize).expect("intel8x0: AC97 I/O offset out of range")
    }

    // Busmaster register space (BAR1)
    fn bm_inb(&self, offset: u32) -> u8 {
        self.bm.try_access().expect("intel8x0: busmaster BAR revoked")
            .try_read8(offset as usize).expect("intel8x0: busmaster I/O offset out of range")
    }
    fn bm_inw(&self, offset: u32) -> u16 {
        self.bm.try_access().expect("intel8x0: busmaster BAR revoked")
            .try_read16(offset as usize).expect("intel8x0: busmaster I/O offset out of range")
    }
    fn bm_inl(&self, offset: u32) -> u32 {
        self.bm.try_access().expect("intel8x0: busmaster BAR revoked")
            .try_read32(offset as usize).expect("intel8x0: busmaster I/O offset out of range")
    }
    fn bm_outb(&self, val: u8, offset: u32) {
        self.bm.try_access().expect("intel8x0: busmaster BAR revoked")
            .try_write8(val, offset as usize).expect("intel8x0: busmaster I/O offset out of range")
    }
    fn bm_outl(&self, val: u32, offset: u32) {
        self.bm.try_access().expect("intel8x0: busmaster BAR revoked")
            .try_write32(val, offset as usize).expect("intel8x0: busmaster I/O offset out of range")
    }
}

//
// AC97 codec access
//
impl Intel8x0 {
    /// Acquire the codec access semaphore.
    ///
    /// Checks that the requested codec is ready, then polls ACC_SEMA
    /// for up to 1 ms (100 x 10 us).
    fn codec_semaphore(&self, codec_num: u16) -> Result {
        // Determine the codec-ready bit for this codec index.
        let ready_bit = match codec_num {
            0 => ICH_PCR,
            1 => ICH_SCR,
            _ => return Err(EIO),
        };
        if self.bm_inl(ICH_REG_GLOB_STA) & ready_bit == 0 {
            return Err(EIO);
        }
        let mut tries = 100u32;
        loop {
            if self.bm_inb(ICH_REG_ACC_SEMA) & ICH_CAS == 0 {
                return Ok(());
            }
            if tries == 0 {
                break;
            }
            tries -= 1;
            // SAFETY: udelay with a small constant is always safe.
            udelay(kernel::time::Delta::from_micros(10));
        }
        // Clear semaphore flag by reading register 0, then continue anyway.
        let _ = self.addr_inw(0);
        Err(EBUSY)
    }
}

/// Implement Ac97BusOps so this chip can serve as an AC97 host controller.
impl ac97::Ac97BusOps for Intel8x0 {
    fn write(&self, codec_num: u16, reg: u16, val: u16) {
        if self.codec_semaphore(codec_num).is_err() {
            pr_err!("intel8x0: semaphore not ready (codec {}, reg 0x{:x})\n",
                    codec_num, reg);
        }
        self.addr_outw(val, reg as u32 + codec_num as u32 * 0x80);
    }

    fn read(&self, codec_num: u16, reg: u16) -> u16 {
        if self.codec_semaphore(codec_num).is_err() {
            pr_err!("intel8x0: semaphore not ready (codec {}, reg 0x{:x})\n",
                    codec_num, reg);
            return 0xffff;
        }
        let val = self.addr_inw(reg as u32 + codec_num as u32 * 0x80);
        let sta = self.bm_inl(ICH_REG_GLOB_STA);
        if sta & ICH_RCS != 0 {
            // Clear RCS, preserve other R/WC bits
            self.bm_outl(sta & !(ICH_PCR | ICH_SCR | ICH_GSCI), ICH_REG_GLOB_STA);
            pr_err!("intel8x0: read timeout (codec {}, reg 0x{:x})\n",
                    codec_num, reg);
            return 0xffff;
        }
        val
    }
}

static INTEL8X0_AC97_OPS: ac97::Ac97BusOpsTable<Intel8x0> = ac97::Ac97BusOpsTable::new();

//
// AC97 cold / warm reset
//
impl Intel8x0 {
    /// Perform a cold (probe=true) or warm (probe=false) AC97 reset
    /// and wait for at least one codec to become ready.
    fn chip_init(&self, probing: bool) -> Result {
        // Clear pending status bits
        let sta = self.bm_inl(ICH_REG_GLOB_STA);
        self.bm_outl(sta & (ICH_RCS | ICH_MCINT | ICH_POINT | ICH_PIINT),
                     ICH_REG_GLOB_STA);

        // Issue cold or warm reset
        let cnt = self.bm_inl(ICH_REG_GLOB_CNT);
        let cnt = cnt & !(ICH_ACLINK | ICH_PCM_246_MASK);
        let cnt = if cnt & ICH_AC97COLD == 0 {
            cnt | ICH_AC97COLD
        } else {
            cnt | ICH_AC97WARM
        };
        self.bm_outl(cnt, ICH_REG_GLOB_CNT);

        // Wait for warm-reset bit to clear (up to 500 ms)
        let mut ms = 500u32;
        loop {
            if self.bm_inl(ICH_REG_GLOB_CNT) & ICH_AC97WARM == 0 {
                break;
            }
            if ms == 0 {
                pr_err!("intel8x0: AC97 warm reset still in progress\n");
                return Err(EIO);
            }
            ms -= 1;
            fsleep(kernel::time::Delta::from_micros(1000));
        }

        if probing {
            // Wait for at least one codec ready (up to 1 s)
            let mut ms = 1000u32;
            loop {
                let ready = self.bm_inl(ICH_REG_GLOB_STA) & (ICH_PCR | ICH_SCR);
                if ready != 0 {
                    break;
                }
                if ms == 0 {
                    pr_err!("intel8x0: codec not ready [0x{:x}]\n",
                            self.bm_inl(ICH_REG_GLOB_STA));
                    return Err(EIO);
                }
                ms -= 1;
                fsleep(kernel::time::Delta::from_micros(1000));
            }
        }
        Ok(())
    }
}

//
// BD table setup
//
impl Intel8x0 {
    /// Write the Buffer Descriptor table and reset the DMA channel registers.
    ///
    /// Must be called with `state` lock held.
    fn setup_periods(&self, dev: &mut Ichdev) {
        let bdbar = dev.bdbar_area as *mut u32;

        if dev.size == dev.fragsize {
            // Single-period case: split the buffer in half.
            dev.ack_reload = 2;
            dev.ack = 2;
            dev.fragsize1 = dev.fragsize >> 1;
            dev.frags = 2;
            // Fill all 32 entries alternating between two half-buffers.
            let half0 = dev.physbuf;
            let half1 = dev.physbuf + dev.fragsize1;
            let samples = dev.fragsize1 >> 1; // pos_shift = 1 (16-bit samples)
            let ctl = 0x8000_0000u32 | samples;
            let mut idx = 0usize;
            while idx < ICH_MAX_FRAGS * 2 {
                unsafe {
                    bdbar.add(idx).write_volatile(half0.to_le());
                    bdbar.add(idx + 1).write_volatile(ctl.to_le());
                    bdbar.add(idx + 2).write_volatile(half1.to_le());
                    bdbar.add(idx + 3).write_volatile(ctl.to_le());
                }
                idx += 4;
            }
        } else {
            dev.ack_reload = 1;
            dev.ack = 1;
            dev.fragsize1 = dev.fragsize;
            dev.frags = dev.size / dev.fragsize;
            let samples = dev.fragsize >> 1; // 16-bit samples
            let ctl = 0x8000_0000u32 | samples;
            for i in 0..ICH_MAX_FRAGS {
                let frag = (i as u32) % dev.frags;
                let addr = dev.physbuf + frag * dev.fragsize;
                unsafe {
                    bdbar.add(i * 2).write_volatile(addr.to_le());
                    bdbar.add(i * 2 + 1).write_volatile(ctl.to_le());
                }
            }
        }

        // Write BDBAR, reset CIV to 0, set LVI to max
        self.bm_outl(dev.bdbar_addr, dev.reg_offset + ICH_REG_OFF_BDBAR);
        self.bm_outb(0, dev.reg_offset + ICH_REG_OFF_CIV);
        self.bm_outb(ICH_LVI_MASK as u8, dev.reg_offset + ICH_REG_OFF_LVI);
        dev.lvi = ICH_LVI_MASK;
        dev.lvi_frag = ICH_LVI_MASK % dev.frags as usize;
        dev.civ = 0;
        dev.position = 0;

        // Clear interrupt flags
        self.bm_outb(ICH_FIFOE | ICH_BCIS | ICH_LVBCI, dev.reg_offset + ICH_REG_OFF_SR);
    }
}

//
// IRQ handler
//
impl irq::Handler for Intel8x0 {
    fn handle(&self) -> irq::IrqReturn {
        let sta = self.bm_inl(ICH_REG_GLOB_STA);
        if sta == 0xffff_ffff {
            // Device not yet resumed
            return irq::IrqReturn::None;
        }

        let int_mask = ICH_MCINT | ICH_POINT | ICH_PIINT;
        if sta & int_mask == 0 {
            if sta != 0 {
                // Ack spurious status bits
                self.bm_outl(sta, ICH_REG_GLOB_STA);
            }
            return irq::IrqReturn::None;
        }

        // Under the spinlock: advance position/LVI and collect substreams to notify.
        let subs = {
            let mut state = self.state.lock();
            let mut result = [pcm::SubstreamHandle::none(); ICHD_COUNT];

            for ichd in 0..ICHD_COUNT {
                let dev = &mut state.ichdevs[ichd];
                if sta & dev.int_sta_mask == 0 || !dev.prepared {
                    continue;
                }
                let sr = self.bm_inb(dev.reg_offset + ICH_REG_OFF_SR);
                let civ = self.bm_inb(dev.reg_offset + ICH_REG_OFF_CIV) as usize;

                let step = if sr & ICH_BCIS == 0 {
                    0
                } else if civ == dev.civ {
                    // CIV unchanged: DMA wrapped; advance by 1
                    dev.civ = (dev.civ + 1) & ICH_LVI_MASK;
                    1
                } else {
                    let s = civ.wrapping_sub(dev.civ) & ICH_LVI_MASK;
                    dev.civ = civ;
                    s
                };

                // Advance position and BD ring
                dev.position = dev.position.wrapping_add((step as u32) * dev.fragsize1);
                dev.position %= dev.size;
                dev.lvi = dev.lvi.wrapping_add(step) & ICH_LVI_MASK;
                self.bm_outb(dev.lvi as u8, dev.reg_offset + ICH_REG_OFF_LVI);

                // Rewrite physical addresses for newly consumed BD entries
                let bdbar = dev.bdbar_area as *mut u32;
                for i in 0..step {
                    let lvi_i = dev.lvi.wrapping_sub(step).wrapping_add(i + 1) & ICH_LVI_MASK;
                    dev.lvi_frag = (dev.lvi_frag + 1) % dev.frags as usize;
                    let addr = dev.physbuf + dev.lvi_frag as u32 * dev.fragsize1;
                    // Word 1 (IOC + samples) was written in setup_periods and does not change.
                    unsafe { bdbar.add(lvi_i * 2).write_volatile(addr.to_le()) };
                }

                // Decrement ack, signal period elapsed when it reaches 0
                dev.ack -= step as i32;
                if dev.ack <= 0 {
                    dev.ack += dev.ack_reload;
                    result[ichd] = dev.substream;
                }

                // Ack SR bits
                self.bm_outb(sr & (ICH_FIFOE | ICH_BCIS | ICH_LVBCI),
                             dev.reg_offset + ICH_REG_OFF_SR);
            }
            result
        };

        // Ack GLOB_STA outside the lock
        self.bm_outl(sta & int_mask, ICH_REG_GLOB_STA);

        // Call period_elapsed outside the lock
        for sub in subs {
            sub.period_elapsed();
        }

        irq::IrqReturn::Handled
    }
}

/// Newtype wrapping Arc<Intel8x0> so we can implement the local-crate Handler trait on it.
struct Intel8x0Arc(Arc<Intel8x0>);

impl irq::Handler for Intel8x0Arc {
    fn handle(&self) -> irq::IrqReturn {
        self.0.handle()
    }
}

//
// PCM hardware descriptors
//
const STREAM_HW_BASE: pcm::Hardware = pcm::Hardware {
    info: INFO_FLAGS,
    formats: bindings::SNDRV_PCM_FMTBIT_S16_LE as u64,
    rates: bindings::SNDRV_PCM_RATE_48000,
    rate_min: 48000,
    rate_max: 48000,
    channels_min: 2,
    channels_max: 2,
    buffer_bytes_max: 128 * 1024,
    period_bytes_min: 32,
    period_bytes_max: 128 * 1024,
    periods_min: 1,
    periods_max: 1024,
    fifo_size: 0,
};

//
// PCM ops helpers
//
/// Determine the DMA channel index from a substream.
fn ichd_for(sub: &pcm::Substream) -> usize {
    match (sub.pcm_device(), sub.stream()) {
        (0, pcm::StreamDir::Playback) => ICHD_PCMOUT,
        (0, pcm::StreamDir::Capture)  => ICHD_PCMIN,
        (_, _)                   => ICHD_MIC,
    }
}


//
// pcm::Ops implementation
//
impl pcm::Ops for Intel8x0 {
    fn open(&self, sub: &pcm::Substream) -> Result {
        let ichd = ichd_for(sub);
        let runtime = sub.runtime();

        // Build per-stream hardware descriptor, possibly with variable rates.
        let mut hw = STREAM_HW_BASE;

        // Use rates from the primary AC97 codec if available.
        // Since we don't have direct access to ac97 here (stored in
        // Intel8x0Data, not Intel8x0), we provide a conservative default.
        // Actual rate programming happens in hw_params.
        // TODO: store ac97 rates in a shared atomic or in Intel8x0.
        if ichd == ICHD_MIC {
            hw.channels_min = 1;
            hw.channels_max = 1;
        }

        runtime.set_hw(&hw);

        let mut state = self.state.lock();
        state.ichdevs[ichd].substream = pcm::SubstreamHandle::new(sub);
        Ok(())
    }

    fn close(&self, sub: &pcm::Substream) -> Result {
        let ichd = ichd_for(sub);
        let mut state = self.state.lock();
        state.ichdevs[ichd].substream.clear();
        state.ichdevs[ichd].prepared = false;
        Ok(())
    }

    fn prepare(&self, sub: &pcm::Substream) -> Result {
        let ichd = ichd_for(sub);
        let runtime = sub.runtime();

        let physbuf   = runtime.dma_addr() as u32;
        let size      = runtime.dma_bytes() as u32;
        let frame_bits = runtime.frame_bits();
        let fragsize  = runtime.period_size() as u32 * frame_bits / 8;

        {
            let mut state = self.state.lock();
            let dev = &mut state.ichdevs[ichd];
            dev.physbuf  = physbuf;
            dev.size     = size;
            dev.fragsize = fragsize;
            dev.prepared = true;

            // setup_periods also sets fragsize1, frags, ack, ack_reload
            self.setup_periods(dev);
        }
        Ok(())
    }

    fn trigger(&self, sub: &pcm::Substream, cmd: pcm::TriggerCommand) -> Result {
        let ichd = ichd_for(sub);
        {
            let state = self.state.lock();
            let dev = &state.ichdevs[ichd];
            let port = dev.reg_offset;

            match cmd {
                pcm::TriggerCommand::Start
                | pcm::TriggerCommand::Resume
                | pcm::TriggerCommand::PauseRelease => {
                    self.bm_outb(ICH_IOCE | ICH_STARTBM, port + ICH_REG_OFF_CR);
                }
                pcm::TriggerCommand::Stop | pcm::TriggerCommand::Suspend => {
                    self.bm_outb(0, port + ICH_REG_OFF_CR);
                    // Poll until DMA halted, then reset
                    let mut tries = 10000u32;
                    while self.bm_inb(port + ICH_REG_OFF_SR) & ICH_DCH == 0 {
                        if tries == 0 { break; }
                        tries -= 1;
                    }
                    self.bm_outb(ICH_RESETREGS, port + ICH_REG_OFF_CR);
                }
                pcm::TriggerCommand::PausePush => {
                    self.bm_outb(ICH_IOCE, port + ICH_REG_OFF_CR);
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn pointer(&self, sub: &pcm::Substream) -> bindings::snd_pcm_uframes_t {
        let ichd = ichd_for(sub);
        let result = {
            let state = self.state.lock();
            let dev = &state.ichdevs[ichd];

            if !dev.prepared || dev.fragsize1 == 0 {
                0
            } else {
                // PICB counts remaining 16-bit samples in the current BD entry.
                // bytes_remaining = picb * 2  (pos_shift = 1)
                let picb = self.bm_inw(dev.reg_offset + ICH_REG_OFF_PICB) as u32;
                let bytes_remaining = (picb * 2).min(dev.fragsize1);
                let ptr = dev.position + dev.fragsize1 - bytes_remaining;
                (ptr % dev.size) as bindings::snd_pcm_uframes_t
            }
        };
        result
    }
}

static INTEL8X0_PCM_OPS: pcm::OpsTable<Intel8x0> = pcm::OpsTable::new();

//
// Proc interface
//
static INTEL8X0_PROC_OPS: info::TextOpsTable<Intel8x0> = info::TextOpsTable::new();

impl info::TextOps for Intel8x0 {
    fn read(&self, buf: &mut info::InfoBuffer) {
        use core::fmt::Write;
        let glob_cnt = self.bm_inl(ICH_REG_GLOB_CNT);
        let glob_sta = self.bm_inl(ICH_REG_GLOB_STA);
        let _ = writeln!(buf, "Intel8x0\n");
        let _ = writeln!(buf, "Global control        : 0x{:08x}", glob_cnt);
        let _ = writeln!(buf, "Global status         : 0x{:08x}", glob_sta);
        let _ = write!(buf, "AC'97 codecs ready    :");
        if glob_sta & (ICH_PCR | ICH_SCR) != 0 {
            if glob_sta & ICH_PCR != 0 { let _ = write!(buf, " primary"); }
            if glob_sta & ICH_SCR != 0 { let _ = write!(buf, " secondary"); }
        } else {
            let _ = write!(buf, " none");
        }
        let _ = writeln!(buf);
    }
}

//
// Driver data (per PCI device)
//
struct Intel8x0Data<'card> {
    _chip_arc: Arc<Intel8x0>,
    _irq: Pin<KBox<irq::Registration<'card, Intel8x0Arc>>>,
    _vecs: pci::IrqVectorRegistration<'card>,
    _card: &'card sound::Card,
    _ac97_bus: ac97::Ac97Bus,
    _pm: kernel::pm::Registration<'card, Intel8x0Driver<'card>>,
}

/// Initialise the AC97 mixer and return bus + codec handles.
fn mixer_init(
    chip_arc: &Arc<Intel8x0>,
    card: &sound::Card,
) -> Result<(ac97::Ac97Bus, [Option<ac97::Ac97>; 2])> {
    let chip = chip_arc.as_ref();

    // Detect present codecs from GLOB_STA
    let sta = chip.bm_inl(ICH_REG_GLOB_STA);
    let ncodecs: usize = if sta & ICH_SCR != 0 { 2 } else { 1 };

    let bus = ac97::ac97_bus(
        card,
        &INTEL8X0_AC97_OPS,
        Arc::as_ptr(chip_arc) as *mut Intel8x0,
    )?;

    let mut codecs: [Option<ac97::Ac97>; 2] = [None, None];
    for i in 0..ncodecs {
        let tmpl = ac97::Ac97Template {
            num: i as u16,
            scaps: AC97_SCAP_SKIP_MODEM | AC97_SCAP_POWER_SAVE,
            pci: core::ptr::null_mut(), // not needed for QEMU (no quirk matching)
            private_data: Arc::as_ptr(chip_arc) as *mut core::ffi::c_void,
        };
        codecs[i] = Some(ac97::ac97_mixer(&bus, &tmpl)?);
    }
    Ok((bus, codecs))
}

//
// PCI driver
//
struct Intel8x0Driver<'card>(core::marker::PhantomData<&'card ()>);

kernel::pci_device_table!(
    INTEL8X0_TABLE,
    <Intel8x0Driver<'static> as pci::Driver>::IdInfo,
    [
        (pci::DeviceId::from_id(pci::Vendor::INTEL, 0x2415), ()), // 82801AA (ICH)   - QEMU default
        (pci::DeviceId::from_id(pci::Vendor::INTEL, 0x2425), ()), // 82801AB (ICH0)
        (pci::DeviceId::from_id(pci::Vendor::INTEL, 0x2445), ()), // 82801BA (ICH2)
        (pci::DeviceId::from_id(pci::Vendor::INTEL, 0x2485), ()), // 82801CA (ICH3)
    ]
);

impl<'card> pci::Driver for Intel8x0Driver<'card> {
    type IdInfo = ();
    type Data<'bound> = Intel8x0Data<'bound>;

    const ID_TABLE: pci::IdTable<()> = &INTEL8X0_TABLE;

    const PM_OPS: Option<&'static bindings::dev_pm_ops> = Some(&kernel::pm::PMContext::<Self>::PM_OPS);

    fn probe<'bound>(
        pdev: &'bound pci::Device<device::Core<'_>>,
        _id_info: Option<&'bound Self::IdInfo>,
    ) -> impl PinInit<Intel8x0Data<'bound>, Error> + 'bound {
        // Enable the device and map BARs.
        pdev.enable_device_mem()?;
        pdev.set_master();

        let addr = pdev.iomap_region_sized::<0x100>(0, c"Intel8x0")?.into_devres()?;
        let bm   = pdev.iomap_region_sized::<0x40>(1, c"Intel8x0")?.into_devres()?;

        // Allocate DMA memory for all 3 BD tables (3 x 32 entries x 8 bytes = 768 B).
        let bdbars = pcm::DmaBuffer::alloc_dev(
            pdev.as_ref(),
            bindings::SNDRV_DMA_TYPE_DEV,
            ICHD_COUNT * ICH_MAX_FRAGS * 8,
        )?;
        let bdbars_area = bdbars.area() as usize;
        let bdbars_dma  = bdbars.addr() as u32;

        // Build the initial Ichdev array.
        let ichdevs = [
            Ichdev::new(
                ICH_REG_PI_BASE,
                bdbars_area + ICHD_PCMIN  * ICH_MAX_FRAGS * 8,
                bdbars_dma   + (ICHD_PCMIN  * ICH_MAX_FRAGS * 8) as u32,
                ICH_PIINT,
            ),
            Ichdev::new(
                ICH_REG_PO_BASE,
                bdbars_area + ICHD_PCMOUT * ICH_MAX_FRAGS * 8,
                bdbars_dma   + (ICHD_PCMOUT * ICH_MAX_FRAGS * 8) as u32,
                ICH_POINT,
            ),
            Ichdev::new(
                ICH_REG_MC_BASE,
                bdbars_area + ICHD_MIC    * ICH_MAX_FRAGS * 8,
                bdbars_dma   + (ICHD_MIC    * ICH_MAX_FRAGS * 8) as u32,
                ICH_MCINT,
            ),
        ];

        // Allocate the chip Arc.
        let chip_arc = Arc::<Intel8x0>::pin_init(
            try_pin_init!(Intel8x0 {
                addr,
                bm,
                state <- new_spinlock!(ChipState { ichdevs }),
            }),
            GFP_KERNEL,
        )?;

        // AC97 cold reset + wait for codec ready.
        chip_arc.chip_init(true)?;

        // Allocate INTx IRQ vector.
        let vecs = pdev.alloc_irq_vectors(
            1, 1,
            pci::IrqTypes::default().with(pci::IrqType::Intx),
        )?;
        let (irq_req, vecs) = vecs.into_irq_request(0)?;

        // SAFETY: The returned Registration is not leaked (stored in Intel8x0Data).
        let irq_reg = unsafe {
            KBox::pin_init(
                irq::Registration::new(
                    irq_req,
                    irq::Flags::SHARED,
                    c"intel8x0",
                    Ok::<Intel8x0Arc, Error>(Intel8x0Arc(chip_arc.clone())),
                ),
                GFP_KERNEL,
            )
        }?;

        // Create the ALSA card.
        let card = kernel::new_sound_card!(pdev.as_ref(), c"Intel8x0")?;
        card.set_driver(c"ICH");
        card.set_short_name(c"Intel ICH");
        card.set_long_name(c"Intel ICH AC97");

        // AC97 mixer init
        let (ac97_bus, ac97_codecs) = mixer_init(&chip_arc, card)?;

        // PCM device 0: stereo playback + stereo capture
        let pcm0 = pcm::Pcm::new(card, c"Intel ICH", 0, 1, 1)?;
        pcm0.set_private_data_arc(chip_arc.clone());
        pcm0.set_ops::<Intel8x0>(pcm::StreamDir::Playback, &INTEL8X0_PCM_OPS);
        pcm0.set_ops::<Intel8x0>(pcm::StreamDir::Capture,  &INTEL8X0_PCM_OPS);
        pcm0.set_managed_buffer_dev(pdev.as_ref(), 64 * 1024, 128 * 1024)?;

        // PCM device 1: mono mic capture
        let pcm1 = pcm::Pcm::new(card, c"Intel ICH - MIC ADC", 1, 0, 1)?;
        pcm1.set_private_data_arc(chip_arc.clone());
        pcm1.set_ops::<Intel8x0>(pcm::StreamDir::Capture, &INTEL8X0_PCM_OPS);
        pcm1.set_managed_buffer_dev(pdev.as_ref(), 64 * 1024, 128 * 1024)?;

        card.ro_proc_new(c"intel8x0", chip_arc.clone(), &INTEL8X0_PROC_OPS)?;

        card.register()?;

        let pm_payload = Intel8x0PmPayload {
            chip_arc: chip_arc.clone(),
            card,
            ac97: ac97_codecs,
        };

        let pm = kernel::pm::Registration::new(
            pdev.as_ref(),
            None,
            None,
            Some(pm_payload),
        )?;
        pm.ctx().enable(kernel::pm::RuntimePMState::RESUMED)?;

        pr_info!("intel8x0: registered\n");

        Ok(Intel8x0Data {
            _chip_arc: chip_arc,
            _irq: irq_reg,
            _vecs: vecs,
            _card: card,
            _ac97_bus: ac97_bus,
            _pm: pm,
        })
    }
}

struct Intel8x0PmPayload<'card> {
    chip_arc: Arc<Intel8x0>,
    card: &'card sound::Card,
    ac97: [Option<ac97::Ac97>; 2],
}

// SAFETY: All payload data is thread-safe.
unsafe impl Send for Intel8x0PmPayload<'_> {}
unsafe impl Sync for Intel8x0PmPayload<'_> {}

#[vtable]
impl<'card> kernel::pm::PMOps for Intel8x0Driver<'card> {
    type DeviceType = pci::Device<device::Bound>;
    type RuntimePayloadType = Intel8x0PmPayload<'card>;

    fn runtime_suspend<'a>(
        _dev: &'a Self::DeviceType,
        payload: Option<Self::RuntimePayloadType>,
    ) -> Result<Option<Self::RuntimePayloadType>, (Option<Self::RuntimePayloadType>, Error)> {
        if let Some(ref p) = payload {
            p.card.power_change_state(sound::card::POWER_D3HOT);
            for ac97 in p.ac97.iter().flatten() {
                ac97.suspend();
            }
        }
        Ok(payload)
    }

    fn runtime_resume<'a>(
        _dev: &'a Self::DeviceType,
        payload: Option<Self::RuntimePayloadType>,
    ) -> Result<Option<Self::RuntimePayloadType>, (Option<Self::RuntimePayloadType>, Error)> {
        if let Some(ref p) = payload {
            if let Err(e) = p.chip_arc.chip_init(false) {
                return Err((payload, e));
            }
            for ac97 in p.ac97.iter().flatten() {
                ac97.resume();
            }
            p.card.power_change_state(sound::card::POWER_D0);
        }
        Ok(payload)
    }
}

kernel::module_pci_driver! {
    type: Intel8x0Driver<'static>,
    name: "snd_rust_intel8x0",
    authors: ["Rust for Linux contributors"],
    description: "Intel ICH AC97 sound driver (Rust)",
    license: "GPL",
    alias: ["pci:v00008086d00002415sv*sd*bc*sc*i*"],
}
