// SPDX-License-Identifier: GPL-2.0

//! Rust ENS1370 (ES1370) PCI sound driver.
//!
//! Targets the QEMU `es1370` audiodev emulation.  Implements:
//!   - Two PCM devices (device 0: DAC2+ADC variable-rate; device 1: DAC1 fixed-rate)
//!   - AK4531 legacy codec mixer
//!   - PCI IRQ handler
//!
//! Skipped: ENS1371 (AC97), MIDI, joystick.

use kernel::{
    bindings,
    device,
    io::Io,
    irq,
    pci,
    prelude::*,
    sound::{self, info, pcm, ak4531},
    sound::control::{self, KControlOps, ElemInfo, ElemType, ElemValue },
    sync::{new_spinlock, Arc, SpinLock},
};

//
// ENS1370 register map
//
const ES_REG_CONTROL:    u32 = 0x00;
const ES_REG_STATUS:     u32 = 0x04;
const ES_REG_MEM_PAGE:   u32 = 0x0c;
const ES_REG_1370_CODEC: u32 = 0x10;
const ES_REG_SERIAL:     u32 = 0x20;
const ES_REG_DAC1_COUNT: u32 = 0x24;
const ES_REG_DAC2_COUNT: u32 = 0x28;
const ES_REG_ADC_COUNT:  u32 = 0x2c;
// Page-mapped registers
const ES_REG_DAC1_FRAME:    u32 = 0x30; // page DAC (0x0c)
const ES_REG_DAC1_SIZE:     u32 = 0x34;
const ES_REG_DAC2_FRAME:    u32 = 0x38;
const ES_REG_DAC2_SIZE:     u32 = 0x3c;
const ES_REG_ADC_FRAME:     u32 = 0x30; // page ADC (0x0d)
const ES_REG_ADC_SIZE:      u32 = 0x34;
const ES_REG_PHANTOM_FRAME: u32 = 0x38;
const ES_REG_PHANTOM_COUNT: u32 = 0x3c;

// CONTROL bits
const ES_DAC2_EN:       u32 = 1 << 5;
const ES_DAC1_EN:       u32 = 1 << 6;
const ES_ADC_EN:        u32 = 1 << 4;
const ES_1370_CDC_EN:   u32 = 1 << 1;
const ES_1370_WTSRSELM: u32 = 0x03u32 << 12;
const ES_1370_PCLKDIVM: u32 = 0x1fffu32 << 16;
const ES_1370_XCTL0:    u32 = 1 << 8;
const ES_1370_XCTL1:    u32 = 1 << 30;
const ES_JYSTK_EN:      u32 = 1 << 2;

const fn es_wtsrsel(o: u32)  -> u32 { (o & 0x03) << 12 }
const fn es_pclkdivo(o: u32) -> u32 { (o & 0x1fff) << 16 }

const ES_1370_SRCLOCK: u32 = 1411200;
const fn es_srtodiv(x: u32) -> u32 { ES_1370_SRCLOCK / x - 2 }

// STATUS bits
const ES_INTR:       u32 = 1u32 << 31;
const ES_1370_CSTAT: u32 = 1 << 10;
const ES_DAC1_IRQ:   u32 = 1 << 2;
const ES_DAC2_IRQ:   u32 = 1 << 1;
const ES_ADC_IRQ:    u32 = 1 << 0;

// SERIAL bits
const ES_P1_INT_EN:   u32 = 1 << 8;
const ES_P2_INT_EN:   u32 = 1 << 9;
const ES_R1_INT_EN:   u32 = 1 << 10;
const ES_P1_PAUSE:    u32 = 1 << 11;
const ES_P2_PAUSE:    u32 = 1 << 12;
const ES_P1_LOOP_SEL: u32 = 1 << 13;
const ES_P2_LOOP_SEL: u32 = 1 << 14;
const ES_R1_LOOP_SEL: u32 = 1 << 15;
const ES_P2_DAC_SEN:  u32 = 1 << 6;
const ES_P1_SCT_RLD:  u32 = 1 << 7;
const ES_P2_END_INCM: u32 = 0x07u32 << 19;
const ES_P2_ST_INCM:  u32 = 0x07u32 << 16;

// Mode masks in SERIAL (P1: bits 1:0, P2: bits 3:2, R1: bits 5:4)
const ES_P1_MODEM: u32 = 0x03;
const ES_P2_MODEM: u32 = 0x03 << 2;
const ES_R1_MODEM: u32 = 0x03 << 4;

const fn es_p1_modeo(o: u32)    -> u32 { o & 0x03 }
const fn es_p2_modeo(o: u32)    -> u32 { (o & 0x03) << 2 }
const fn es_r1_modeo(o: u32)    -> u32 { (o & 0x03) << 4 }
const fn es_p2_end_inco(o: u32) -> u32 { (o & 0x07) << 19 }
const fn es_p2_st_inco(o: u32)  -> u32 { (o & 0x07) << 16 }

// Extract current DMA byte position from a SIZE register read
const fn es_fcurr_counti(i: u32) -> u32 { (i >> 14) & 0x3fffc }
const fn es_mem_pageo(o: u32)    -> u32 { o & 0x0f }

const ES_PAGE_DAC: u32 = 0x0c;
const ES_PAGE_ADC: u32 = 0x0d;

// Tracks which stream owns the shared PCLKDIV divider
const ES_MODE_PLAY2:   u32 = 0x0002;
const ES_MODE_CAPTURE: u32 = 0x0004;

// AK4531 reset register index
const AK4531_RESET: u16 = 0x16;

// Pack codec reg+val into a 16-bit port write
const fn es_codec_write(reg: u16, val: u16) -> u16 { (reg << 8) | (val & 0xff) }

// snd_ensoniq_sample_shift[mode]: mode = stereo<<0 | 16bit<<1
const SAMPLE_SHIFT: [u32; 4] = [0, 1, 1, 2];

//
// PCM hardware descriptors
//
// SNDRV_PCM_FORMAT_U8 = 1, SNDRV_PCM_FORMAT_S16_LE = 2 (C #define with __force cast)
const FMTBITS_U8_S16: u64 = (1u64 << 1) | (1u64 << 2);

const INFO_FLAGS: u32 = bindings::SNDRV_PCM_INFO_MMAP
    | bindings::SNDRV_PCM_INFO_INTERLEAVED
    | bindings::SNDRV_PCM_INFO_BLOCK_TRANSFER
    | bindings::SNDRV_PCM_INFO_MMAP_VALID
    | bindings::SNDRV_PCM_INFO_PAUSE
    | bindings::SNDRV_PCM_INFO_SYNC_START;

const DAC1_HW: pcm::Hardware = pcm::Hardware {
    info: INFO_FLAGS,
    formats: FMTBITS_U8_S16,
    rates: (bindings::SNDRV_PCM_RATE_KNOT
        | bindings::SNDRV_PCM_RATE_11025
        | bindings::SNDRV_PCM_RATE_22050
        | bindings::SNDRV_PCM_RATE_44100) as u32,
    rate_min: 5512,
    rate_max: 44100,
    channels_min: 1,
    channels_max: 2,
    buffer_bytes_max: 128 * 1024,
    period_bytes_min: 64,
    period_bytes_max: 128 * 1024,
    periods_min: 1,
    periods_max: 1024,
    fifo_size: 0,
};

const DAC2_ADC_HW: pcm::Hardware = pcm::Hardware {
    info: INFO_FLAGS,
    formats: FMTBITS_U8_S16,
    rates: (bindings::SNDRV_PCM_RATE_CONTINUOUS
        | bindings::SNDRV_PCM_RATE_8000_48000) as u32,
    rate_min: 4000,
    rate_max: 48000,
    channels_min: 1,
    channels_max: 2,
    buffer_bytes_max: 128 * 1024,
    period_bytes_min: 64,
    period_bytes_max: 128 * 1024,
    periods_min: 1,
    periods_max: 1024,
    fifo_size: 0,
};

// Fixed-rate list for DAC1 hw_constraint_list
static DAC1_RATES: [u32; 4] = [5512, 11025, 22050, 44100];
static DAC1_RATE_CONSTRAINT: pcm::HwConstraintList =
    pcm::HwConstraintList::new(0, &DAC1_RATES);

// Rational clock for DAC2/ADC hw_constraint_ratnums
static ES1370_CLOCKS: [bindings::snd_ratnum; 1] = [bindings::snd_ratnum {
    num: ES_1370_SRCLOCK,
    den_min: 29,   // ~48.7 kHz max, capped at 48000
    den_max: 353,  // ~4000 Hz min
    den_step: 1,
}];
static ES1370_RATNUM_CONSTRAINT: pcm::HwConstraintRatnums =
    pcm::HwConstraintRatnums::new(&ES1370_CLOCKS);

//
// Chip state
//
struct EnsoniqState {
    ctrl:         u32,
    sctrl:        u32,
    pclkdiv_lock: u32, // ES_MODE_PLAY2 | ES_MODE_CAPTURE
    playback1_sub: pcm::SubstreamHandle,
    playback2_sub: pcm::SubstreamHandle,
    capture_sub:   pcm::SubstreamHandle,
}

#[pin_data]
struct Ensoniq {
    bar:          pci::DevresBar<0x40>,
    dma_bug_addr: u32,
    #[pin]
    state: SpinLock<EnsoniqState>,
}

impl Ensoniq {
    fn new(bar: pci::DevresBar<0x40>, dma_bug_addr: u32, init_ctrl: u32) -> impl PinInit<Self, Error> {
        try_pin_init!(Self {
            bar,
            dma_bug_addr,
            state <- new_spinlock!(EnsoniqState {
                ctrl:  init_ctrl,
                sctrl: 0,
                pclkdiv_lock: 0,
                playback1_sub: pcm::SubstreamHandle::none(),
                playback2_sub: pcm::SubstreamHandle::none(),
                capture_sub:   pcm::SubstreamHandle::none(),
            }),
        })
    }

    fn inl(&self, offset: u32) -> u32 {
        self.bar.try_access()
            .expect("ens1370: BAR revoked")
            .try_read32(offset as usize)
            .expect("ens1370: I/O offset out of range")
    }

    fn outl(&self, val: u32, offset: u32) {
        self.bar.try_access()
            .expect("ens1370: BAR revoked")
            .try_write32(val, offset as usize)
            .expect("ens1370: I/O offset out of range")
    }

    fn outw(&self, val: u16, offset: u32) {
        self.bar.try_access()
            .expect("ens1370: BAR revoked")
            .try_write16(val, offset as usize)
            .expect("ens1370: I/O offset out of range")
    }

    fn chip_init(&self) {
        let ctrl = self.state.lock().ctrl;
        self.outl(ctrl, ES_REG_CONTROL);
        self.outl(0, ES_REG_SERIAL);
        self.outl(es_mem_pageo(ES_PAGE_ADC), ES_REG_MEM_PAGE);
        self.outl(self.dma_bug_addr, ES_REG_PHANTOM_FRAME);
        self.outl(0, ES_REG_PHANTOM_COUNT);
    }

    // Poll CSTAT up to 0xffff times then write the codec register.
    fn codec_write(&self, reg: u16, val: u16) {
        let mut tries = 0xffff_u32;
        while tries > 0 {
            if self.inl(ES_REG_STATUS) & ES_1370_CSTAT == 0 {
                self.outw(es_codec_write(reg, val), ES_REG_1370_CODEC);
                return;
            }
            tries -= 1;
        }
        pr_err!("ENS1370: codec write timeout, status = 0x{:x}\n",
            self.inl(ES_REG_STATUS));
    }
}

//
// AK4531 Ops
//
impl ak4531::Ak4531Ops for Ensoniq {
    fn write(&self, reg: u16, val: u16) {
        self.codec_write(reg, val);
    }
}

//
// IRQ handler
//
impl irq::Handler for Ensoniq {
    fn handle(&self) -> irq::IrqReturn {
        let status = self.inl(ES_REG_STATUS);
        if status & ES_INTR == 0 {
            return irq::IrqReturn::None;
        }

        // Ack IRQ: momentarily clear the INT_EN bits then restore.
        let (p1_sub, p2_sub, c_sub) = {
            let state = self.state.lock();
            let mut s = state.sctrl;
            if status & ES_DAC1_IRQ != 0 { s &= !ES_P1_INT_EN; }
            if status & ES_DAC2_IRQ != 0 { s &= !ES_P2_INT_EN; }
            if status & ES_ADC_IRQ  != 0 { s &= !ES_R1_INT_EN; }
            self.outl(s, ES_REG_SERIAL);
            self.outl(state.sctrl, ES_REG_SERIAL);
            (state.playback1_sub, state.playback2_sub, state.capture_sub)
        };

        // period_elapsed must be called outside the spinlock to avoid ABBA
        // deadlock with ALSA's PCM spinlock.
        if status & ES_DAC1_IRQ != 0 { p1_sub.period_elapsed(); }
        if status & ES_DAC2_IRQ != 0 { p2_sub.period_elapsed(); }
        if status & ES_ADC_IRQ  != 0 { c_sub.period_elapsed();  }

        irq::IrqReturn::Handled
    }
}

/// Newtype wrapping Arc<Ensoniq> so we can implement the local-crate Handler trait on it.
struct EnsoniqArc(Arc<Ensoniq>);

impl irq::Handler for EnsoniqArc {
    fn handle(&self) -> irq::IrqReturn {
        self.0.handle()
    }
}

//
// PCM ops
//
static ENS1370_PCM_OPS: pcm::OpsTable<Ensoniq> = pcm::OpsTable::new();

impl pcm::Ops for Ensoniq {
    // trigger only does port I/O - no sleeping.
    const NONATOMIC: bool = false;

    fn open(&self, substream: &pcm::Substream) -> Result {
        let runtime = substream.runtime();
        let dev = substream.pcm_device();
        let dir = substream.stream();
        match (dev, dir) {
            (1, pcm::StreamDir::Playback) => {
                runtime.set_hw(&DAC1_HW);
                runtime.hw_constraint_list(0, pcm::hw_param::RATE, &DAC1_RATE_CONSTRAINT)?;
                self.state.lock().playback1_sub = pcm::SubstreamHandle::new(substream);
            }
            (0, pcm::StreamDir::Playback) => {
                runtime.set_hw(&DAC2_ADC_HW);
                runtime.hw_constraint_ratnums(0, pcm::hw_param::RATE, &ES1370_RATNUM_CONSTRAINT)?;
                self.state.lock().playback2_sub = pcm::SubstreamHandle::new(substream);
            }
            (0, pcm::StreamDir::Capture) => {
                runtime.set_hw(&DAC2_ADC_HW);
                runtime.hw_constraint_ratnums(0, pcm::hw_param::RATE, &ES1370_RATNUM_CONSTRAINT)?;
                self.state.lock().capture_sub = pcm::SubstreamHandle::new(substream);
            }
            _ => return Err(EINVAL),
        }
        Ok(())
    }

    fn close(&self, substream: &pcm::Substream) -> Result {
        let dev = substream.pcm_device();
        let dir = substream.stream();
        let mut state = self.state.lock();
        match (dev, dir) {
            (1, pcm::StreamDir::Playback) => {
                state.playback1_sub.clear();
            }
            (0, pcm::StreamDir::Playback) => {
                state.pclkdiv_lock &= !ES_MODE_PLAY2;
                state.playback2_sub.clear();
            }
            (0, pcm::StreamDir::Capture) => {
                state.pclkdiv_lock &= !ES_MODE_CAPTURE;
                state.capture_sub.clear();
            }
            _ => return Err(EINVAL),
        }
        Ok(())
    }

    fn prepare(&self, substream: &pcm::Substream) -> Result {
        let runtime      = substream.runtime();
        let dev          = substream.pcm_device();
        let dir          = substream.stream();
        let dma_bytes    = runtime.dma_bytes() as u32;
        let dma_addr     = runtime.dma_addr();
        let rate         = runtime.rate();
        let channels     = runtime.channels();
        let fmt_width    = runtime.format_width();

        let frame_bits   = runtime.frame_bits();
        let period_bytes = runtime.period_size() as u32 * frame_bits / 8;

        // mode for SAMPLE_SHIFT: bit0 = stereo, bit1 = 16-bit
        let mode = (if fmt_width == 16 { 2u32 } else { 0 })
                 | (if channels > 1   { 1u32 } else { 0 });

        let mut state = self.state.lock();

        match (dev, dir) {
            (1, pcm::StreamDir::Playback) => {
                state.ctrl &= !ES_DAC1_EN;
                self.outl(state.ctrl, ES_REG_CONTROL);

                self.outl(es_mem_pageo(ES_PAGE_DAC), ES_REG_MEM_PAGE);
                self.outl(dma_addr, ES_REG_DAC1_FRAME);
                self.outl((dma_bytes >> 2) - 1, ES_REG_DAC1_SIZE);

                state.sctrl &= !(ES_P1_LOOP_SEL | ES_P1_PAUSE | ES_P1_SCT_RLD | ES_P1_MODEM);
                state.sctrl |= ES_P1_INT_EN | es_p1_modeo(mode);
                self.outl(state.sctrl, ES_REG_SERIAL);

                self.outl((period_bytes >> SAMPLE_SHIFT[mode as usize]) - 1, ES_REG_DAC1_COUNT);

                state.ctrl &= !ES_1370_WTSRSELM;
                state.ctrl |= match rate {
                    5512  => es_wtsrsel(0),
                    11025 => es_wtsrsel(1),
                    22050 => es_wtsrsel(2),
                    _     => es_wtsrsel(3),
                };
                self.outl(state.ctrl, ES_REG_CONTROL);
            }

            (0, pcm::StreamDir::Playback) => {
                state.ctrl &= !ES_DAC2_EN;
                self.outl(state.ctrl, ES_REG_CONTROL);

                self.outl(es_mem_pageo(ES_PAGE_DAC), ES_REG_MEM_PAGE);
                self.outl(dma_addr, ES_REG_DAC2_FRAME);
                self.outl((dma_bytes >> 2) - 1, ES_REG_DAC2_SIZE);

                state.sctrl &= !(ES_P2_LOOP_SEL | ES_P2_PAUSE | ES_P2_DAC_SEN
                    | ES_P2_END_INCM | ES_P2_ST_INCM | ES_P2_MODEM);
                state.sctrl |= ES_P2_INT_EN | es_p2_modeo(mode)
                    | es_p2_end_inco(if mode & 2 != 0 { 2 } else { 1 })
                    | es_p2_st_inco(0);
                self.outl(state.sctrl, ES_REG_SERIAL);

                self.outl((period_bytes >> SAMPLE_SHIFT[mode as usize]) - 1, ES_REG_DAC2_COUNT);

                if state.pclkdiv_lock & ES_MODE_CAPTURE == 0 {
                    state.ctrl &= !ES_1370_PCLKDIVM;
                    state.ctrl |= es_pclkdivo(es_srtodiv(rate));
                    state.pclkdiv_lock |= ES_MODE_PLAY2;
                }
                self.outl(state.ctrl, ES_REG_CONTROL);
            }

            (0, pcm::StreamDir::Capture) => {
                state.ctrl &= !ES_ADC_EN;
                self.outl(state.ctrl, ES_REG_CONTROL);

                self.outl(es_mem_pageo(ES_PAGE_ADC), ES_REG_MEM_PAGE);
                self.outl(dma_addr, ES_REG_ADC_FRAME);
                self.outl((dma_bytes >> 2) - 1, ES_REG_ADC_SIZE);

                state.sctrl &= !(ES_R1_LOOP_SEL | ES_R1_MODEM);
                state.sctrl |= ES_R1_INT_EN | es_r1_modeo(mode);
                self.outl(state.sctrl, ES_REG_SERIAL);

                self.outl((period_bytes >> SAMPLE_SHIFT[mode as usize]) - 1, ES_REG_ADC_COUNT);

                if state.pclkdiv_lock & ES_MODE_PLAY2 == 0 {
                    state.ctrl &= !ES_1370_PCLKDIVM;
                    state.ctrl |= es_pclkdivo(es_srtodiv(rate));
                    state.pclkdiv_lock |= ES_MODE_CAPTURE;
                }
                self.outl(state.ctrl, ES_REG_CONTROL);
            }

            _ => return Err(EINVAL),
        }
        Ok(())
    }

    fn trigger(&self, substream: &pcm::Substream, cmd: pcm::TriggerCommand) -> Result {
        let dev = substream.pcm_device();
        let dir = substream.stream();

        let en_bit = match (dev, dir) {
            (1, pcm::StreamDir::Playback) => ES_DAC1_EN,
            (0, pcm::StreamDir::Playback) => ES_DAC2_EN,
            (0, pcm::StreamDir::Capture)  => ES_ADC_EN,
            _ => return Err(EINVAL),
        };
        let pause_bit = match (dev, dir) {
            (1, pcm::StreamDir::Playback) => ES_P1_PAUSE,
            (0, pcm::StreamDir::Playback) => ES_P2_PAUSE,
            _                        => 0,
        };

        let mut state = self.state.lock();
        match cmd {
            pcm::TriggerCommand::Start => {
                state.ctrl |= en_bit;
                self.outl(state.ctrl, ES_REG_CONTROL);
            }
            pcm::TriggerCommand::Stop |
            pcm::TriggerCommand::Suspend => {
                state.ctrl &= !en_bit;
                self.outl(state.ctrl, ES_REG_CONTROL);
            }
            pcm::TriggerCommand::PausePush => {
                state.sctrl |= pause_bit;
                self.outl(state.sctrl, ES_REG_SERIAL);
            }
            pcm::TriggerCommand::PauseRelease => {
                state.sctrl &= !pause_bit;
                self.outl(state.sctrl, ES_REG_SERIAL);
            }
            _ => return Err(EINVAL),
        }
        Ok(())
    }

    fn pointer(&self, substream: &pcm::Substream) -> bindings::snd_pcm_uframes_t {
        let dev     = substream.pcm_device();
        let dir     = substream.stream();
        let runtime = substream.runtime();

        let (en_bit, page, size_reg) = match (dev, dir) {
            (1, pcm::StreamDir::Playback) => (ES_DAC1_EN, ES_PAGE_DAC, ES_REG_DAC1_SIZE),
            (0, pcm::StreamDir::Playback) => (ES_DAC2_EN, ES_PAGE_DAC, ES_REG_DAC2_SIZE),
            (0, pcm::StreamDir::Capture)  => (ES_ADC_EN,  ES_PAGE_ADC, ES_REG_ADC_SIZE),
            _                        => return 0,
        };

        {
            let state = self.state.lock();
            if state.ctrl & en_bit == 0 {
                return 0;
            }
        }

        self.outl(es_mem_pageo(page), ES_REG_MEM_PAGE);
        let raw      = self.inl(size_reg);
        let byte_pos = es_fcurr_counti(raw);

        let frame_bits = runtime.frame_bits();
        if frame_bits == 0 { return 0; }
        (byte_pos * 8 / frame_bits) as bindings::snd_pcm_uframes_t
    }
}

//
// XCTL mixer controls (Line-In jack switch / Mic +5V bias)
//
struct XctlOps {
    chip: Arc<Ensoniq>, // keeps chip alive as long as the control exists
    mask: u32,
}

impl KControlOps for XctlOps {
    fn info(&self, info: &mut ElemInfo) -> Result {
        info.set_type_count(ElemType::Boolean, 1);
        Ok(())
    }

    fn get(&self, value: &mut ElemValue) -> Result {
        let state = self.chip.state.lock();
        value.set_boolean(0, state.ctrl & self.mask != 0);
        Ok(())
    }

    fn put(&self, value: &ElemValue) -> Result<bool> {
        let new = value.boolean(0);
        let mut state = self.chip.state.lock();
        let was_set = state.ctrl & self.mask != 0;
        if was_set != new {
            if new { state.ctrl |= self.mask; } else { state.ctrl &= !self.mask; }
            self.chip.outl(state.ctrl, ES_REG_CONTROL);
            return Ok(true);
        }
        Ok(false)
    }
}

//
// Mixer init
//
fn mixer_init(chip_arc: &Arc<Ensoniq>, card: &sound::Card) -> Result<ak4531::Ak4531> {
    // Hard-reset the AK4531 codec.
    chip_arc.outw(es_codec_write(AK4531_RESET, 0x02), ES_REG_1370_CODEC);
    chip_arc.outw(es_codec_write(AK4531_RESET, 0x03), ES_REG_1370_CODEC);

    // Register the AK4531 mixer - the C driver creates all per-channel controls.
    let chip_ptr = Arc::as_ptr(chip_arc) as *mut core::ffi::c_void;
    let ak4531_tmpl = ak4531::Ak4531Template { private_data: chip_ptr };
    let ak4531 = ak4531::ak4531_mixer::<Ensoniq>(card, &ak4531_tmpl)?;

    // XCTL0: PCM 0 output also on Line-In jack
    card.add_control(
        control::ElemIface::Card,
        c"PCM 0 Output also on Line-In Jack",
        XctlOps { chip: chip_arc.clone(), mask: ES_1370_XCTL0 },
    )?;
    // XCTL1: Mic +5V bias
    card.add_control(
        control::ElemIface::Card,
        c"Mic +5V bias",
        XctlOps { chip: chip_arc.clone(), mask: ES_1370_XCTL1 },
    )?;

    Ok(ak4531)
}

//
// Proc interface
//
static ENS1370_PROC_OPS: info::TextOpsTable<Ensoniq> = info::TextOpsTable::new();

impl info::TextOps for Ensoniq {
    fn read(&self, buf: &mut info::InfoBuffer) {
        use core::fmt::Write;
        let ctrl = self.state.lock().ctrl;
        let _ = writeln!(buf, "Ensoniq AudioPCI ES1370\n");
        let _ = writeln!(buf, "Joystick enable  : {}",
            if ctrl & ES_JYSTK_EN != 0 { "on" } else { "off" });
        let _ = writeln!(buf, "MIC +5V bias     : {}",
            if ctrl & ES_1370_XCTL1 != 0 { "on" } else { "off" });
        let _ = writeln!(buf, "Line In to AOUT  : {}",
            if ctrl & ES_1370_XCTL0 != 0 { "on" } else { "off" });
    }
}

//
// PCI driver
//
kernel::pci_device_table!(
    ENS1370_TABLE,
    <Ens1370Driver<'static> as pci::Driver>::IdInfo,
    [(pci::DeviceId::from_id(pci::Vendor::ENSONIQ, 0x5000), ())]
);

struct Ens1370Driver<'card>(core::marker::PhantomData<&'card ()>);

/// Driver data: owns the chip Arc, the IRQ registration, and a card reference.
struct Ens1370Data<'card> {
    chip_arc: Arc<Ensoniq>,
    _ak4531: ak4531::Ak4531,
    _irq: Pin<KBox<irq::Registration<'card, EnsoniqArc>>>,
    _vecs: pci::IrqVectorRegistration<'card>,
    _card: &'card sound::Card,
    _pm: kernel::pm::Registration<'card, Ens1370Driver<'card>>,
}

impl<'card> pci::Driver for Ens1370Driver<'card> {
    type IdInfo = ();
    type Data<'bound> = Ens1370Data<'bound>;

    const ID_TABLE: pci::IdTable<()> = &ENS1370_TABLE;

    const PM_OPS: Option<&'static bindings::dev_pm_ops> = Some(&kernel::pm::PMContext::<Self>::PM_OPS);

    fn probe<'bound>(
        pdev: &'bound pci::Device<device::Core<'_>>,
        _id_info: Option<&'bound Self::IdInfo>,
    ) -> impl PinInit<Ens1370Data<'bound>, Error> + 'bound {
        pdev.enable_device_mem()?;
        pdev.set_master();

        let bar = pdev.iomap_region_sized::<0x40>(0, c"ENS1370")?;
        let bar = bar.into_devres()?;

        // 16-byte phantom DMA buffer (ES1370 DMA engine bug workaround).
        let dma_buf = pcm::DmaBuffer::alloc_dev(
            pdev.as_ref(),
            bindings::SNDRV_DMA_TYPE_DEV,
            16,
        )?;
        let dma_bug_addr = dma_buf.addr() as u32;

        // Initial CONTROL: codec enabled, PCLKDIV set for 8 kHz default.
        let init_ctrl = ES_1370_CDC_EN | es_pclkdivo(es_srtodiv(8000));

        // Allocate Arc<Ensoniq>. Kernel's Arc::pin_init returns Arc<T> directly.
        let chip_arc = Arc::<Ensoniq>::pin_init(
            Ensoniq::new(bar, dma_bug_addr, init_ctrl),
            GFP_KERNEL,
        )?;

        chip_arc.chip_init();

        // Allocate an INTx IRQ vector.  `into_irq_request` consumes `vecs` and returns
        // an `IrqRequest<'bound>` whose lifetime comes from the stored device reference
        // inside `IrqVectorRegistration`, not from a borrow of the local variable.
        // Both are returned and stored so drop ordering is preserved for MSI/MSI-X.
        let vecs = pdev.alloc_irq_vectors(
            1, 1,
            pci::IrqTypes::default().with(pci::IrqType::Intx),
        )?;
        let (irq_req, vecs) = vecs.into_irq_request(0)?;

        // SAFETY: The returned Registration is not leaked (stored in Ens1370Data).
        let irq_reg = unsafe {
            KBox::pin_init(
                irq::Registration::new(
                    irq_req,
                    irq::Flags::SHARED,
                    c"ENS1370",
                    Ok::<EnsoniqArc, Error>(EnsoniqArc(chip_arc.clone())),
                ),
                GFP_KERNEL,
            )
        }?;

        // Create sound card (devres-managed).
        let card = kernel::new_sound_card!(pdev.as_ref(), c"ENS1370")?;
        card.set_driver(c"ENS1370");
        card.set_short_name(c"Ensoniq AudioPCI");
        card.set_long_name(c"Ensoniq AudioPCI ENS1370");
        card.set_mixer_name(c"Ensoniq AudioPCI Mixer");

        let ak4531 = mixer_init(&chip_arc, card)?;

        // PCM device 0: DAC2 playback + ADC capture (variable rate).
        let pcm0 = pcm::Pcm::new(card, c"ENS1370 DAC2/ADC", 0, 1, 1)?;
        pcm0.set_private_data_arc(chip_arc.clone());
        pcm0.set_ops::<Ensoniq>(pcm::StreamDir::Playback, &ENS1370_PCM_OPS);
        pcm0.set_ops::<Ensoniq>(pcm::StreamDir::Capture,  &ENS1370_PCM_OPS);
        pcm0.set_managed_buffer_dev(pdev.as_ref(), 64 * 1024, 128 * 1024)?;

        // PCM device 1: DAC1 playback only (fixed rate).
        let pcm1 = pcm::Pcm::new(card, c"ENS1370 DAC1", 1, 1, 0)?;
        pcm1.set_private_data_arc(chip_arc.clone());
        pcm1.set_ops::<Ensoniq>(pcm::StreamDir::Playback, &ENS1370_PCM_OPS);
        pcm1.set_managed_buffer_dev(pdev.as_ref(), 64 * 1024, 128 * 1024)?;

        card.ro_proc_new(c"audiopci", chip_arc.clone(), &ENS1370_PROC_OPS)?;

        card.register()?;

        let pm_payload = Ens1370PmPayload {
            chip_arc: chip_arc.clone(),
            ak4531,
            card,
        };

        let pm = kernel::pm::Registration::new(
            pdev.as_ref(),
            None,
            None,
            Some(pm_payload),
        )?;
        pm.ctx().enable(kernel::pm::RuntimePMState::RESUMED)?;

        dev_info!(pdev, "ENS1370 sound card registered\n");

        Ok(Ens1370Data { chip_arc, _ak4531: ak4531, _irq: irq_reg, _vecs: vecs, _card: card, _pm: pm })
    }
}

struct Ens1370PmPayload<'card> {
    chip_arc: Arc<Ensoniq>,
    ak4531: ak4531::Ak4531,
    card: &'card sound::Card,
}

// SAFETY: All payload data is thread-safe.
unsafe impl Send for Ens1370PmPayload<'_> {}
unsafe impl Sync for Ens1370PmPayload<'_> {}

#[vtable]
impl<'card> kernel::pm::PMOps for Ens1370Driver<'card> {
    type DeviceType = pci::Device<device::Bound>;
    type RuntimePayloadType = Ens1370PmPayload<'card>;

    fn runtime_suspend<'a>(
        _dev: &'a Self::DeviceType,
        payload: Option<Self::RuntimePayloadType>,
    ) -> Result<Option<Self::RuntimePayloadType>, (Option<Self::RuntimePayloadType>, Error)> {
        if let Some(ref p) = payload {
            // Hard-reset the AK4531 codec.
            p.chip_arc.outw(es_codec_write(AK4531_RESET, 0x02), ES_REG_1370_CODEC);
            p.chip_arc.outw(es_codec_write(AK4531_RESET, 0x03), ES_REG_1370_CODEC);
            p.ak4531.suspend();

            // The PCM device's own PM callback has already called trigger(Suspend)
            // on all running substreams, stopping DMA.  We only need to signal the
            // power state change to block new userspace API calls.
            p.card.power_change_state(sound::card::POWER_D3HOT);
        }
        Ok(payload)
    }

    fn runtime_resume<'a>(
        _dev: &'a Self::DeviceType,
        payload: Option<Self::RuntimePayloadType>,
    ) -> Result<Option<Self::RuntimePayloadType>, (Option<Self::RuntimePayloadType>, Error)> {
        if let Some(ref p) = payload {
            p.chip_arc.chip_init();
            p.ak4531.resume();
            p.card.power_change_state(sound::card::POWER_D0);
        }
        Ok(payload)
    }
}

impl Drop for Ens1370Data<'_> {
    fn drop(&mut self) {
        // Disable all DMA engines and silence interrupts.
        self.chip_arc.outl(0, ES_REG_CONTROL);
        self.chip_arc.outl(0, ES_REG_SERIAL);
    }
}

//
// Module registration
//
kernel::module_pci_driver! {
    type: Ens1370Driver<'static>,
    name: "snd_rust_ens1370",
    authors: ["Rust for Linux contributors"],
    description: "Rust ENS1370 PCI sound driver",
    license: "GPL",
    alias: ["pci:v00001274d00005000sv*sd*bc*sc*i*"],
}
