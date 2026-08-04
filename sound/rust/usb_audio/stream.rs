// SPDX-License-Identifier: GPL-2.0

//! USB audio stream and substream management.
//!
//! Corresponds to `sound/usb/stream.c`.

use kernel::prelude::*;
use kernel::{bindings, usb, sync::{Arc, LockedBy, Mutex, SpinLock, new_spinlock}};
use core::sync::atomic::{AtomicI32, AtomicPtr, AtomicU32, Ordering};
use core::cell::Cell;

use crate::types::*;
use crate::card::{UsbAudioChip, UsbAudioChipState};
use crate::endpoint::UsbEndpoint;
use kernel::sound::{card::Card, pcm::{Pcm, StreamDir}};

pub(crate) struct SubstreamXfer {
    pub hwptr_done: u32,
    pub transfer_done: u32,
    pub frame_limit: u32,
    pub inflight_bytes: u32,
    pub period_elapsed_pending: u32,
    pub trigger_tstamp_pending: bool,
}

impl SubstreamXfer {
    const fn zero() -> Self {
        Self {
            hwptr_done: 0,
            transfer_done: 0,
            frame_limit: 0,
            inflight_bytes: 0,
            period_elapsed_pending: 0,
            trigger_tstamp_pending: false,
        }
    }

    pub(crate) fn reset(&mut self) {
        *self = Self::zero();
    }
}

#[pin_data]
pub(crate) struct UsbSubstream {
    pub direction: i32,
    pub ep_num: Cell<u8>,
    pub formats: Cell<u64>,
    pub num_formats: Cell<u32>,
    pub fmt_type: Cell<u8>,
    pub fmt_list: LockedBy<KVec<AudioFormat>, UsbAudioChipState>,
    pub cur_audiofmt: AtomicPtr<AudioFormat>,
    pub data_endpoint: AtomicPtr<UsbEndpoint>,
    pub sync_endpoint: AtomicPtr<UsbEndpoint>,
    pub pcm_substream: AtomicPtr<bindings::snd_pcm_substream>,
    pub running: AtomicI32,
    pub lowlatency_playback: AtomicI32,
    pub buffer_bytes: AtomicU32,
    #[pin]
    pub xfer: SpinLock<SubstreamXfer>,
    pub dev: *mut bindings::usb_device,
    pub speed: u32,
}

unsafe impl Send for UsbSubstream {}
unsafe impl Sync for UsbSubstream {}

impl UsbSubstream {
    pub(crate) fn new(
        dev: *mut bindings::usb_device,
        speed: u32,
        direction: i32,
        mutex: &Mutex<UsbAudioChipState>,
    ) -> impl PinInit<Self> + '_ {
        pin_init!(Self {
            direction,
            ep_num: Cell::new(0),
            formats: Cell::new(0),
            num_formats: Cell::new(0),
            fmt_type: Cell::new(0),
            fmt_list: LockedBy::new(mutex, KVec::new()),
            cur_audiofmt: AtomicPtr::new(core::ptr::null_mut()),
            data_endpoint: AtomicPtr::new(core::ptr::null_mut()),
            sync_endpoint: AtomicPtr::new(core::ptr::null_mut()),
            pcm_substream: AtomicPtr::new(core::ptr::null_mut()),
            running: AtomicI32::new(0),
            lowlatency_playback: AtomicI32::new(0),
            buffer_bytes: AtomicU32::new(0),
            xfer <- new_spinlock!(SubstreamXfer::zero()),
            dev,
            speed,
        })
    }

    pub(crate) fn data_ep(&self) -> Option<&UsbEndpoint> {
        let ptr = self.data_endpoint.load(Ordering::Acquire);
        if ptr.is_null() {
            None
        } else {
            // SAFETY: The endpoint ptr, once set, is guaranteed to be valid and outlive the substream.
            Some(unsafe { &*ptr })
        }
    }

    pub(crate) fn sync_ep(&self) -> Option<&UsbEndpoint> {
        let ptr = self.sync_endpoint.load(Ordering::Acquire);
        if ptr.is_null() {
            None
        } else {
            // SAFETY: The endpoint ptr, once set, is guaranteed to be valid and outlive the substream.
            Some(unsafe { &*ptr })
        }
    }

    pub(crate) fn start_endpoints(&self) -> Result<()> {
        let ep = self.data_ep().ok_or(EINVAL)?;
        ep.start(self)?;
        if let Some(sep) = self.sync_ep() {
            if let Err(e) = sep.start(self) {
                sep.stop(false);
                ep.stop(false);
                return Err(e);
            }
        }
        Ok(())
    }

    pub(crate) fn stop_endpoints(&self, keep_pending: bool) {
        if let Some(sep) = self.sync_ep() {
            sep.stop(keep_pending);
        }
        if let Some(ep) = self.data_ep() {
            ep.stop(keep_pending);
        }
    }

    pub(crate) fn stop_endpoints_async(&self) {
        if let Some(ep) = self.data_ep() {
            ep.stop_async();
        }
        if let Some(sep) = self.sync_ep() {
            sep.stop_async();
        }
    }

    pub(crate) fn sync_pending_stops(&self) {
        if let Some(sep) = self.sync_ep() {
            sep.sync_pending_stop();
        }
        if let Some(ep) = self.data_ep() {
            ep.sync_pending_stop();
        }
    }

    pub(crate) fn prepare_urb(
        &self,
        ctx: &crate::endpoint::UrbCtx,
        mut urb: core::pin::Pin<&mut usb::Urb<crate::endpoint::UrbCtx>>,
        in_stream_lock: bool,
    ) -> i32 {
        use crate::pcm::{prepare_playback_urb_fn, prepare_capture_urb_fn};
        if self.direction == 0 {
            if self.running.load(Ordering::Acquire) != 0 {
                prepare_playback_urb_fn(self, urb, in_stream_lock)
            } else {
                let ep = unsafe { ctx.endpoint() };
                let urb_mut = unsafe { urb.as_mut().get_unchecked_mut() };
                crate::endpoint::prepare_silent_urb(ep, ctx, urb_mut)
            }
        } else {
            prepare_capture_urb_fn(self, urb, in_stream_lock)
        }
    }

    pub(crate) fn retire_urb(
        &self,
        urb: core::pin::Pin<&mut usb::Urb<crate::endpoint::UrbCtx>>,
    ) {
        use crate::pcm::{retire_playback_urb_fn, retire_capture_urb_fn};
        if self.direction == 0 {
            retire_playback_urb_fn(self, urb);
        } else {
            if self.running.load(Ordering::Acquire) != 0 {
                retire_capture_urb_fn(self, urb);
            }
        }
    }

    pub(crate) fn cur_fmt(&self) -> *const AudioFormat {
        self.cur_audiofmt.load(Ordering::Acquire)
    }

    pub(crate) fn xfer(&self) -> &SpinLock<SubstreamXfer> {
        &self.xfer
    }

    pub(crate) fn buffer_bytes(&self) -> u32 {
        self.buffer_bytes.load(Ordering::Acquire)
    }

    pub(crate) fn set_cur_audiofmt(&self, fmt: *const AudioFormat) {
        self.cur_audiofmt.store(fmt as *mut AudioFormat, Ordering::Release);
    }

    pub(crate) fn set_data_endpoint(&self, ep: *mut UsbEndpoint) {
        self.data_endpoint.store(ep, Ordering::Release);
    }

    pub(crate) fn set_sync_endpoint(&self, ep: *mut UsbEndpoint) {
        self.sync_endpoint.store(ep, Ordering::Release);
    }

    pub(crate) fn set_buffer_bytes(&self, bytes: u32) {
        self.buffer_bytes.store(bytes, Ordering::Release);
    }
}

#[pin_data]
pub(crate) struct UsbStream {
    pub chip: Arc<UsbAudioChip>,
    pub pcm: *mut bindings::snd_pcm,
    pub pcm_index: i32,
    pub fmt_type: Cell<u8>,
    #[pin]
    pub substream_play: UsbSubstream,
    #[pin]
    pub substream_cap: UsbSubstream,
}

unsafe impl Send for UsbStream {}
unsafe impl Sync for UsbStream {}

impl UsbStream {
    pub(crate) fn substream(&self, dir: StreamDir) -> &UsbSubstream {
        match dir {
            StreamDir::Playback => &self.substream_play,
            StreamDir::Capture => &self.substream_cap,
        }
    }

    pub(crate) fn substream_by_dir_idx(&self, dir_idx: usize) -> &UsbSubstream {
        if dir_idx == 0 {
            &self.substream_play
        } else {
            &self.substream_cap
        }
    }
}

use kernel::sound::pcm::OpsTable;
pub(crate) static USB_AUDIO_OPS: OpsTable<UsbStream> = OpsTable::new();

/// Parse an AudioStreaming interface and register its audio formats.
///
/// Iterates every alternate setting of `iface`, looking for valid UAC1/2
/// isochronous streaming interfaces with class-specific descriptors, and
/// registers each valid `AudioFormat` via `add_audio_stream`.
pub(crate) fn parse_audio_interface(
    chip: &Arc<UsbAudioChip>,
    state: &mut UsbAudioChipState,
    card: &Card,
    dev: &kernel::usb::Device,
    iface: &kernel::usb::Interface<impl kernel::device::DeviceContext>,
    usb_id: u32,
    speed: u32,
) -> Result<()> {
    for alts in iface.altsettings() {
        if alts.class().as_raw() != USB_CLASS_AUDIO
            || alts.subclass() != USB_SUBCLASS_AUDIOSTREAMING
            || alts.endpoints().is_empty()
        {
            continue;
        }

        let ep = &alts.endpoints()[0];
        let ep_desc = ep.desc();
        let wmax = u16::from_le(ep_desc.wMaxPacketSize());

        if wmax == 0 { continue; }

        if ep.endpoint_type() != kernel::usb::EndpointType::Isoc {
            continue;
        }

        let stream_dir: i32 = if ep.endpoint_dir() == kernel::usb::ch9::Direction::In { 1 } else { 0 };

        let protocol = match alts.protocol() {
            UAC_VERSION_1 | UAC_VERSION_2 => alts.protocol(),
            _ => UAC_VERSION_1,
        };

        let iface_no = alts.number();
        let altno = alts.alternate_setting();

        let extra = alts.extra();
        if extra.len() < 3 { continue; }

        let as_off = match crate::helper::find_csint_desc(extra, None, UAC_AS_GENERAL) {
            Some(o) => o,
            None => continue,
        };
        let as_desc = &extra[as_off..];
        if !crate::validate::validate_audio_desc(as_desc, protocol, UAC_AS_GENERAL) { continue; }

        let (format_val, channels_uac2): (u64, u32) = if protocol == UAC_VERSION_1 {
            let tag = u16::from_le_bytes([as_desc[5], as_desc[6]]) as u64;
            (tag, 0)
        } else {
            let bm = u32::from_le_bytes([as_desc[6], as_desc[7], as_desc[8], as_desc[9]]) as u64;
            let ch = as_desc[10] as u32;
            (bm, ch)
        };

        let fmt_off = match crate::helper::find_csint_desc(extra, None, UAC_FORMAT_TYPE) {
            Some(o) => o,
            None => continue,
        };
        let fmt = &extra[fmt_off..];
        if !crate::validate::validate_audio_desc(fmt, protocol, UAC_FORMAT_TYPE) { continue; }

        const USB_DT_CS_ENDPOINT: u8 = 0x25;
        const EP_GENERAL_SUBTYPE: u8 = 0x01;
        let cs_ep_attrs: u8 = {
            let ep_extra = ep.extra();
            if let Some(off) = crate::helper::find_desc(ep_extra, None, USB_DT_CS_ENDPOINT) {
                if ep_extra.len() >= off + 4 && ep_extra[off + 2] == EP_GENERAL_SUBTYPE {
                    ep_extra[off + 3]
                } else {
                    0
                }
            } else if let Some(off) = crate::helper::find_desc(extra, None, USB_DT_CS_ENDPOINT) {
                if extra.len() >= off + 4 && extra[off + 2] == EP_GENERAL_SUBTYPE {
                    extra[off + 3]
                } else {
                    0
                }
            } else {
                0
            }
        };

        let datainterval: u8 =
            if speed == bindings::usb_device_speed_USB_SPEED_HIGH as u32 {
                let bi = ep_desc.bInterval();
                if bi >= 1 && bi <= 4 { bi - 1 } else { 0 }
            } else {
                0
            };

        let mut fp = AudioFormat {
            formats: 0,
            channels: if protocol == UAC_VERSION_2 { channels_uac2 } else { 0 },
            fmt_type: UAC_FORMAT_TYPE_I,
            fmt_bits: 0,
            fmt_sz: 0,
            iface: iface_no,
            altsetting: altno,
            endpoint: ep_desc.bEndpointAddress(),
            attributes: cs_ep_attrs,
            ep_attr: ep_desc.bmAttributes(),
            ep_idx: 0,
            sync_ep: 0,
            sync_iface: 0,
            sync_altsetting: 0,
            sync_ep_idx: 0,
            implicit_fb: false,
            datainterval,
            protocol,
            maxpacksize: wmax as u32,
            rates: 0,
            rate_min: 0,
            rate_max: 0,
            rate_table: KVec::new(),
            clock: 0,
            dsd_dop: false,
            dsd_bitrev: false,
            dsd_raw: false,
        };

        if crate::format::parse_audio_format(dev, usb_id, &mut fp, fmt, format_val).is_err() {
            continue;
        }

        let mut quirk_flags = chip.quirk_flags.load(core::sync::atomic::Ordering::Relaxed);
        if crate::implicit::parse_implicit_fb_quirk(
            dev,
            usb_id,
            &mut quirk_flags,
            &mut fp,
            alts,
        ) {
            chip.quirk_flags.store(quirk_flags, core::sync::atomic::Ordering::Relaxed);
        }

        pr_info!(
            "snd_rust_usb_audio: {}:{}: dir={} {}ch {}-{}Hz ep={:#x}\n",
            iface_no, altno, stream_dir, fp.channels, fp.rate_min, fp.rate_max, fp.endpoint,
        );

        add_audio_stream(chip, state, card, dev, speed, stream_dir, fp)?;
    }

    Ok(())
}

pub(crate) fn add_audio_stream(
    chip: &Arc<UsbAudioChip>,
    state: &mut UsbAudioChipState,
    card: &Card,
    dev: &kernel::usb::Device,
    speed: u32,
    stream_dir: i32,
    fp: AudioFormat,
) -> Result<()> {
    let dir_idx = stream_dir as usize;
    let fmt_type = fp.fmt_type;
    let ep_num = fp.endpoint;

    let mut found_idx = None;
    for (i, s) in state.pcm_list.iter().enumerate() {
        if s.fmt_type.get() != fmt_type { continue; }
        let subs = s.substream_by_dir_idx(dir_idx);
        if subs.ep_num.get() == ep_num {
            found_idx = Some(i);
            break;
        }
    }

    if let Some(i) = found_idx {
        let s = state.pcm_list[i].clone();
        let subs = s.substream_by_dir_idx(dir_idx);
        subs.formats.set(subs.formats.get() | fp.formats);
        subs.num_formats.set(subs.num_formats.get() + 1);
        subs.fmt_list.access_mut(state).push(fp, GFP_KERNEL)?;
        return Ok(());
    }

    let mut empty_idx = None;
    for (i, s) in state.pcm_list.iter().enumerate() {
        if s.fmt_type.get() != fmt_type { continue; }
        let subs = s.substream_by_dir_idx(dir_idx);
        if subs.ep_num.get() == 0 {
            empty_idx = Some(i);
            break;
        }
    }

    if let Some(i) = empty_idx {
        let s = state.pcm_list[i].clone();
        let subs = s.substream_by_dir_idx(dir_idx);

        let pcm = unsafe { &*s.pcm.cast::<Pcm>() };
        let dir = if stream_dir == 0 { StreamDir::Playback } else { StreamDir::Capture };
        pcm.new_stream(
            dir,
            1,
            bindings::SNDRV_DMA_TYPE_VMALLOC,
            core::ptr::null_mut(),
            0,
            0,
        )?;

        pcm.set_ops(dir, &USB_AUDIO_OPS);

        subs.ep_num.set(ep_num);
        subs.formats.set(fp.formats);
        subs.num_formats.set(1);
        subs.fmt_type.set(fmt_type);
        subs.fmt_list.access_mut(state).push(fp, GFP_KERNEL)?;
        return Ok(());
    }

    let pcm_index = state.pcm_devs;
    let pcm_name = if pcm_index == 0 { c"USB Audio" } else { c"USB Audio #N" };
    let (pb_count, cap_count) = if stream_dir == 0 { (1, 0) } else { (0, 1) };
    let pcm = Pcm::new(card, pcm_name, pcm_index, pb_count, cap_count)?;

    let stream = Arc::pin_init(
        pin_init!(UsbStream {
            chip: Arc::clone(chip),
            pcm: pcm.as_raw(),
            pcm_index,
            fmt_type: Cell::new(fmt_type),
            substream_play <- UsbSubstream::new(dev.as_raw(), speed, 0, &chip.mutex),
            substream_cap <- UsbSubstream::new(dev.as_raw(), speed, 1, &chip.mutex),
        }),
        GFP_KERNEL,
    )?;

    let subs = if stream_dir == 0 { &stream.substream_play } else { &stream.substream_cap };
    subs.ep_num.set(ep_num);
    subs.formats.set(fp.formats);
    subs.num_formats.set(1);
    subs.fmt_type.set(fmt_type);
    subs.fmt_list.access_mut(state).push(fp, GFP_KERNEL)?;

    pcm.set_ops(StreamDir::Playback, &USB_AUDIO_OPS);
    pcm.set_ops(StreamDir::Capture, &USB_AUDIO_OPS);
    pcm.set_managed_buffer_all(
        bindings::SNDRV_DMA_TYPE_VMALLOC,
        core::ptr::null_mut(),
        0,
        0,
    )?;
    
    pcm.set_private_data_arc(stream.clone());

    state.pcm_list.push(stream, GFP_KERNEL)?;
    state.pcm_devs += 1;

    Ok(())
}

pub(crate) fn find_format(
    fmt_list: &KVec<AudioFormat>,
    pcm_format: i32,
    rate: u32,
    channels: u32,
) -> Option<*const AudioFormat> {
    let mut found: Option<*const AudioFormat> = None;
    let mut cur_attr = 0i32;

    for fp in fmt_list.iter() {
        let fbit = 1u64 << (pcm_format as u64 & 63);
        if fp.formats & fbit == 0 { continue; }
        if fp.channels != channels { continue; }
        if rate < fp.rate_min || rate > fp.rate_max { continue; }
        if fp.rates & bindings::SNDRV_PCM_RATE_CONTINUOUS == 0 {
            let mut ok = false;
            for &r in fp.rate_table.iter() {
                if r == rate { ok = true; break; }
            }
            if !ok { continue; }
        }
        let attr = fp.ep_attr as i32;
        if found.is_none() || attr > cur_attr {
            found = Some(fp as *const _);
            cur_attr = attr;
        }
    }
    found
}
