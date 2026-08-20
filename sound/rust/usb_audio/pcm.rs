// SPDX-License-Identifier: GPL-2.0

//! ALSA PCM callbacks for the USB audio driver.
//!
//! Corresponds to `sound/usb/pcm.c`.

use kernel::prelude::*;
use kernel::{bindings, sync::Arc, usb};
use core::sync::atomic::Ordering;

use crate::types::*;
use crate::card::{UsbAudioChip, UsbAudioChipState};
use crate::stream::{UsbStream, UsbSubstream};
use crate::endpoint::{UsbEndpoint, UrbCtx};
use kernel::sound::pcm::{
    hw_param, Hardware, HwInterval, HwMask, HwParams, HwRule, HwRuleHandle,
    Ops, Substream, StreamDir, TriggerCommand,
};

fn usb_hardware() -> Hardware {
    Hardware {
        info: bindings::SNDRV_PCM_INFO_MMAP
            | bindings::SNDRV_PCM_INFO_MMAP_VALID
            | bindings::SNDRV_PCM_INFO_BATCH
            | bindings::SNDRV_PCM_INFO_INTERLEAVED
            | bindings::SNDRV_PCM_INFO_BLOCK_TRANSFER
            | bindings::SNDRV_PCM_INFO_PAUSE,
        formats: 0,
        rates: 0,
        rate_min: 8000,
        rate_max: 192000,
        channels_min: 1,
        channels_max: 256,
        buffer_bytes_max: i32::MAX as usize,
        period_bytes_min: 64,
        period_bytes_max: i32::MAX as usize,
        periods_min: 2,
        periods_max: 1024,
        fifo_size: 0,
    }
}

fn dir_index(dir: StreamDir) -> usize {
    match dir { StreamDir::Playback => 0, StreamDir::Capture => 1 }
}

//
// URB prepare - playback
//
pub(crate) fn prepare_playback_urb_fn(
    subs: &UsbSubstream,
    mut urb: core::pin::Pin<&mut usb::Urb<UrbCtx>>,
    in_stream_lock: bool,
) -> i32 {
    let mut xfer_guard = subs.xfer().lock();
    let xfer = &mut *xfer_guard;
    let buffer_bytes = subs.buffer_bytes();

    let pcm_sub = subs.pcm_substream.load(Ordering::Relaxed);
    if pcm_sub.is_null() { return EAGAIN.to_errno(); }
    // SAFETY: pcm_sub is valid and initialized.
    let pcm_sub_ref = unsafe { &*(pcm_sub as *const Substream) };
    let runtime = pcm_sub_ref.runtime();
    if runtime.as_raw_runtime().is_null() { return EAGAIN.to_errno(); }

    let ctx = match urb.context() {
        Some(c) => kernel::sync::Arc::from(c),
        None => return EAGAIN.to_errno(),
    };
    let ep_ref = unsafe { &*ctx.ep };
    let stride = ep_ref.stride;
    let period_size = runtime.period_size() as u32;
    let mut frame_limit = xfer.frame_limit + ep_ref.max_urb_frames;
    let urb_buf_size = ep_ref.urb_buf_size;

    let mut frames = 0u32;
    let mut period_elapsed = false;

    // SAFETY: Not structurally pinned, just setting fields and buffer data.
    let urb_mut = unsafe { urb.as_mut().get_unchecked_mut() };
    urb_mut.set_number_of_packets(ctx.packets as i32);
    urb_mut.set_transfer_buffer_length(urb_buf_size);

    let mut actual_packets = 0;

    {
        let (_, descs) = urb_mut.isoc_buffers_mut();
        for i in 0..ctx.packets as usize {
            let counts = ep_ref.next_packet_size(&ctx, i, u32::MAX);
            if counts < 0 { break; }
            let counts = counts as u32;

            let offset = frames * stride;
            let length = counts * stride;
            if offset + length > urb_buf_size { break; }

            descs[i].set_offset(offset);
            descs[i].set_length(length);
            actual_packets += 1;

            frames += counts;
            xfer.transfer_done += counts;
            if xfer.transfer_done >= period_size {
                xfer.transfer_done -= period_size;
                frame_limit = 0;
                period_elapsed = true;
            }
            if period_elapsed
                || (xfer.transfer_done >= frame_limit
                    && !ep_ref.implicit_feedback_sink())
            {
                break;
            }
        }
    }

    urb_mut.set_number_of_packets(actual_packets as i32);

    if frames == 0 { return EAGAIN.to_errno(); }

    xfer.frame_limit = frame_limit;
    let bytes = frames * stride;
    let hwptr = xfer.hwptr_done as usize;
    let dma_area = runtime.dma_area();

    urb_mut.set_transfer_buffer_length(bytes);

    {
        let buf = urb_mut.transfer_buffer_mut();
        // SAFETY: dma_area is valid for buffer_bytes.
        let dma_slice = unsafe { core::slice::from_raw_parts(dma_area, buffer_bytes as usize) };

        if hwptr + bytes as usize > buffer_bytes as usize {
            let bytes1 = buffer_bytes as usize - hwptr;
            buf[..bytes1].copy_from_slice(&dma_slice[hwptr..hwptr + bytes1]);
            buf[bytes1..bytes as usize].copy_from_slice(&dma_slice[..bytes as usize - bytes1]);
        } else {
            buf[..bytes as usize].copy_from_slice(&dma_slice[hwptr..hwptr + bytes as usize]);
        }
    }

    xfer.hwptr_done += bytes;
    if xfer.hwptr_done >= buffer_bytes { xfer.hwptr_done -= buffer_bytes; }
    xfer.inflight_bytes += bytes;
    ctx.queued.store(bytes as i32, Ordering::Relaxed);

    if period_elapsed {
        let running = subs.running.load(Ordering::Relaxed) != 0;
        let lowlat = subs.lowlatency_playback.load(Ordering::Relaxed) != 0;
        if lowlat && !running {
            xfer.period_elapsed_pending = 1;
        } else {
            core::mem::drop(xfer_guard);
            if in_stream_lock {
                pcm_sub_ref.period_elapsed_under_stream_lock();
            } else {
                pcm_sub_ref.period_elapsed();
            }
        }
    }
    0
}

//
// URB retire - playback
//
pub(crate) fn retire_playback_urb_fn(
    subs: &UsbSubstream,
    urb: core::pin::Pin<&mut usb::Urb<UrbCtx>>,
) {
    let mut xfer_guard = subs.xfer().lock();
    let xfer = &mut *xfer_guard;

    let ctx = match urb.context() {
        Some(c) => c,
        None => return,
    };

    let queued = ctx.queued.load(Ordering::Relaxed);
    if queued > 0 {
        if xfer.inflight_bytes >= queued as u32 {
            xfer.inflight_bytes -= queued as u32;
        } else {
            xfer.inflight_bytes = 0;
        }
    }

    let pcm_sub = subs.pcm_substream.load(Ordering::Relaxed);
    if subs.running.load(Ordering::Relaxed) != 0 {
        let pending = xfer.period_elapsed_pending;
        xfer.period_elapsed_pending = 0;
        if pending != 0 && !pcm_sub.is_null() {
            // SAFETY: pcm_sub is valid and initialized.
            let pcm_sub_ref = unsafe { &*(pcm_sub as *const Substream) };
            core::mem::drop(xfer_guard);
            pcm_sub_ref.period_elapsed();
        }
    }
}

//
// URB prepare - capture
//
pub(crate) fn prepare_capture_urb_fn(
    _subs: &UsbSubstream,
    mut urb: core::pin::Pin<&mut usb::Urb<UrbCtx>>,
    _in_stream_lock: bool,
) -> i32 {
    let ctx = match urb.context() {
        Some(c) => kernel::sync::Arc::from(c),
        None => return EAGAIN.to_errno(),
    };
    let ep = unsafe { &*ctx.ep };

    // SAFETY: Not structurally pinned, just setting fields and buffer data.
    let urb_mut = unsafe { urb.as_mut().get_unchecked_mut() };
    urb_mut.set_number_of_packets(ctx.packets as i32);

    let descs = urb_mut.iso_frame_descs_mut();
    for i in 0..ctx.packets as usize {
        descs[i].set_offset((i as u32) * ep.curpacksize);
        descs[i].set_length(ep.curpacksize);
    }
    urb_mut.set_transfer_buffer_length(ctx.packets as u32 * ep.curpacksize);
    0
}

//
// URB retire - capture
//
pub(crate) fn retire_capture_urb_fn(
    subs: &UsbSubstream,
    mut urb: core::pin::Pin<&mut usb::Urb<UrbCtx>>,
) {
    let mut xfer_guard = subs.xfer().lock();
    let xfer = &mut *xfer_guard;
    let buffer_bytes = subs.buffer_bytes();

    let pcm_sub = subs.pcm_substream.load(Ordering::Relaxed);
    if pcm_sub.is_null() { return; }
    // SAFETY: pcm_sub is valid and initialized.
    let pcm_sub_ref = unsafe { &*(pcm_sub as *const Substream) };
    let runtime = pcm_sub_ref.runtime();
    if runtime.as_raw_runtime().is_null() { return; }

    let stride = (runtime.frame_bits() >> 3) as usize;
    let period_size = runtime.period_size() as u32;
    let dma_area = runtime.dma_area();
    
    // SAFETY: Not structurally pinned.
    let urb_mut = unsafe { urb.as_mut().get_unchecked_mut() };
    let (buf, descs) = urb_mut.isoc_buffers_mut();
    
    let mut period_elapsed = false;

    // SAFETY: dma_area is valid for buffer_bytes.
    let dma_slice = unsafe { core::slice::from_raw_parts_mut(dma_area, buffer_bytes as usize) };

    for desc in descs.iter() {
        if desc.status() != 0 { continue; }
        let plen = desc.actual_length() as usize;
        let poff = desc.offset() as usize;
        if plen == 0 || poff + plen > buf.len() {
            continue;
        }
        
        let src = &buf[poff..poff + plen];
        let hwptr = xfer.hwptr_done as usize;

        if hwptr + plen > buffer_bytes as usize {
            let b1 = buffer_bytes as usize - hwptr;
            dma_slice[hwptr..hwptr + b1].copy_from_slice(&src[..b1]);
            dma_slice[..plen - b1].copy_from_slice(&src[b1..]);
        } else {
            dma_slice[hwptr..hwptr + plen].copy_from_slice(src);
        }

        xfer.hwptr_done += plen as u32;
        if xfer.hwptr_done >= buffer_bytes { xfer.hwptr_done -= buffer_bytes; }

        if stride > 0 {
            let frames = (plen / stride) as u32;
            xfer.transfer_done += frames;
            if xfer.transfer_done >= period_size {
                xfer.transfer_done -= period_size;
                period_elapsed = true;
            }
        }
    }

    core::mem::drop(xfer_guard);
    if period_elapsed {
        pcm_sub_ref.period_elapsed();
    }
}

//
// PCM hw_params constraint rules
//

// Shared context for all six hw_rule callbacks.
#[derive(Clone)]
struct UsbHwRuleBase {
    stream: Arc<UsbStream>,
    dir: StreamDir,
}

impl UsbHwRuleBase {
    fn subs(&self) -> &UsbSubstream {
        self.stream.substream(self.dir)
    }
}

struct RateRule(UsbHwRuleBase);
struct ChannelsRule(UsbHwRuleBase);
struct FormatRule(UsbHwRuleBase);
struct PeriodTimeRule(UsbHwRuleBase);
struct PeriodSizeRule(UsbHwRuleBase);
struct PeriodsRule(UsbHwRuleBase);

// Heap-allocated container stored in runtime->private_data via store_hw_rules.
struct UsbHwRuleSet {
    rate: RateRule,
    channels: ChannelsRule,
    format: FormatRule,
    period_time: PeriodTimeRule,
    period_size: PeriodSizeRule,
    periods: PeriodsRule,
}

// Returns true if fp overlaps the current (possibly wide) hw_params constraints.
// Mirrors hw_check_valid_format() from sound/usb/pcm.c.
fn check_valid_format(
    fp: &AudioFormat,
    params: &HwParams,
    is_full_speed: bool,
) -> bool {
    // Format: at least one bit must overlap.
    let fmts = params.mask(hw_param::FORMAT);
    if fp.formats & fmts.bits_u64() == 0 {
        return false;
    }
    // Channels: fp.channels must fall within the current interval.
    let ct = params.interval(hw_param::CHANNELS);
    if fp.channels < ct.min() || fp.channels > ct.max() {
        return false;
    }
    // Rate: fp's range must overlap the current rate interval.
    let it = params.interval(hw_param::RATE);
    if fp.rate_min > it.max() || (fp.rate_min == it.max() && it.openmax()) {
        return false;
    }
    if fp.rate_max < it.min() || (fp.rate_max == it.min() && it.openmin()) {
        return false;
    }
    // Period time: skip for full-speed (125us base doesn't apply).
    if !is_full_speed && fp.datainterval > 0 {
        let ptime = 125u32 * (1u32 << fp.datainterval);
        let pt = params.interval(hw_param::PERIOD_TIME);
        if ptime > pt.max() || (ptime == pt.max() && pt.openmax()) {
            return false;
        }
    }
    true
}

// Clamps `it` to [rmin, rmax]; returns 1 if changed, 0 unchanged, EINVAL if empty.
// Mirrors apply_hw_params_minmax() from sound/usb/pcm.c.
fn apply_interval(it: &HwInterval, rmin: u32, rmax: u32) -> i32 {
    if rmin > rmax {
        it.set_empty();
        return EINVAL.to_errno();
    }
    let mut changed = 0i32;
    if it.min() < rmin {
        it.set_min(rmin);
        changed = 1;
    }
    if it.max() > rmax {
        it.set_max(rmax);
        changed = 1;
    }
    if it.is_empty() {
        return EINVAL.to_errno();
    }
    changed
}

// Intersects the format mask with `fbits`; returns 1 if changed, EINVAL if empty.
fn apply_format_bits(mask: &HwMask, fbits: u64) -> i32 {
    if fbits == 0 {
        return EINVAL.to_errno();
    }
    let changed = mask.and_u64(fbits);
    if mask.is_empty() {
        return EINVAL.to_errno();
    }
    changed as i32
}

impl HwRule for RateRule {
    fn apply(&self, params: &HwParams) -> i32 {
        let subs = self.0.subs();
        let it = params.interval(hw_param::RATE);
        let is_full = subs.speed == bindings::usb_device_speed_USB_SPEED_FULL as u32;

        // If the data endpoint is already in use at a specific rate, constrain to it.
        if let Some(ep) = subs.data_ep() {
            if ep.cur_rate > 0 {
                return apply_interval(&it, ep.cur_rate, ep.cur_rate);
            }
        }
        if let Some(ep) = subs.sync_ep() {
            if ep.cur_rate > 0 {
                return apply_interval(&it, ep.cur_rate, ep.cur_rate);
            }
        }

        let state = self.0.stream.chip.mutex.lock();
        let fmt_list = subs.fmt_list.access(&*state);
        let mut rmin = u32::MAX;
        let mut rmax = 0u32;
        for fp in fmt_list.iter() {
            if !check_valid_format(fp, params, is_full) {
                continue;
            }
            if !fp.rate_table.is_empty() {
                for &r in fp.rate_table.iter() {
                    if it.test(r) {
                        rmin = rmin.min(r);
                        rmax = rmax.max(r);
                    }
                }
            } else {
                rmin = rmin.min(fp.rate_min);
                rmax = rmax.max(fp.rate_max);
            }
        }
        apply_interval(&it, rmin, rmax)
    }
}

impl HwRule for ChannelsRule {
    fn apply(&self, params: &HwParams) -> i32 {
        let subs = self.0.subs();
        let it = params.interval(hw_param::CHANNELS);
        let is_full = subs.speed == bindings::usb_device_speed_USB_SPEED_FULL as u32;

        let state = self.0.stream.chip.mutex.lock();
        let fmt_list = subs.fmt_list.access(&*state);
        let mut cmin = u32::MAX;
        let mut cmax = 0u32;
        for fp in fmt_list.iter() {
            if !check_valid_format(fp, params, is_full) {
                continue;
            }
            cmin = cmin.min(fp.channels);
            cmax = cmax.max(fp.channels);
        }
        apply_interval(&it, cmin, cmax)
    }
}

impl HwRule for FormatRule {
    fn apply(&self, params: &HwParams) -> i32 {
        let subs = self.0.subs();
        let mask = params.mask(hw_param::FORMAT);
        let is_full = subs.speed == bindings::usb_device_speed_USB_SPEED_FULL as u32;

        let state = self.0.stream.chip.mutex.lock();
        let fmt_list = subs.fmt_list.access(&*state);
        let mut fbits = 0u64;
        for fp in fmt_list.iter() {
            if !check_valid_format(fp, params, is_full) {
                continue;
            }
            fbits |= fp.formats;
        }
        apply_format_bits(&mask, fbits)
    }
}

impl HwRule for PeriodTimeRule {
    fn apply(&self, params: &HwParams) -> i32 {
        let subs = self.0.subs();
        let it = params.interval(hw_param::PERIOD_TIME);
        let is_full = subs.speed == bindings::usb_device_speed_USB_SPEED_FULL as u32;

        let state = self.0.stream.chip.mutex.lock();
        let fmt_list = subs.fmt_list.access(&*state);
        let mut min_datainterval = u8::MAX;
        for fp in fmt_list.iter() {
            if !check_valid_format(fp, params, is_full) {
                continue;
            }
            min_datainterval = min_datainterval.min(fp.datainterval);
        }
        if min_datainterval == u8::MAX {
            it.set_empty();
            return EINVAL.to_errno();
        }
        let pmin = 125u32 * (1u32 << min_datainterval);
        apply_interval(&it, pmin, u32::MAX)
    }
}

impl HwRule for PeriodSizeRule {
    fn apply(&self, params: &HwParams) -> i32 {
        let subs = self.0.subs();
        let it = params.interval(hw_param::PERIOD_SIZE);
        let is_full = subs.speed == bindings::usb_device_speed_USB_SPEED_FULL as u32;

        // If the data endpoint is already committed, constrain period size to it.
        if let Some(ep) = subs.data_ep() {
            if ep.cur_period_frames > 0 {
                return apply_interval(&it, ep.cur_period_frames, ep.cur_period_frames);
            }
        }
        // Check sync endpoint for implicit_fb formats.
        let state = self.0.stream.chip.mutex.lock();
        let fmt_list = subs.fmt_list.access(&*state);
        let mut rmin = u32::MAX;
        let mut rmax = 0u32;
        for fp in fmt_list.iter() {
            if !fp.implicit_fb || !check_valid_format(fp, params, is_full) {
                continue;
            }
            if let Some(ep) = subs.sync_ep() {
                if ep.cur_period_frames > 0 {
                    rmin = rmin.min(ep.cur_period_frames);
                    rmax = rmax.max(ep.cur_period_frames);
                }
            }
        }
        if rmax == 0 {
            return 0;
        }
        apply_interval(&it, rmin, rmax)
    }
}

impl HwRule for PeriodsRule {
    fn apply(&self, params: &HwParams) -> i32 {
        let subs = self.0.subs();
        let it = params.interval(hw_param::PERIODS);
        let is_full = subs.speed == bindings::usb_device_speed_USB_SPEED_FULL as u32;

        // If the data endpoint is already committed, constrain periods to it.
        if let Some(ep) = subs.data_ep() {
            if ep.cur_buffer_periods > 0 {
                return apply_interval(&it, ep.cur_buffer_periods, ep.cur_buffer_periods);
            }
        }
        // Check sync endpoint for implicit_fb formats.
        let state = self.0.stream.chip.mutex.lock();
        let fmt_list = subs.fmt_list.access(&*state);
        let mut rmin = u32::MAX;
        let mut rmax = 0u32;
        for fp in fmt_list.iter() {
            if !fp.implicit_fb || !check_valid_format(fp, params, is_full) {
                continue;
            }
            if let Some(ep) = subs.sync_ep() {
                if ep.cur_buffer_periods > 0 {
                    rmin = rmin.min(ep.cur_buffer_periods);
                    rmax = rmax.max(ep.cur_buffer_periods);
                }
            }
        }
        if rmax == 0 {
            return 0;
        }
        apply_interval(&it, rmin, rmax)
    }
}

//
// Endpoint lifecycle helpers
//
fn find_or_create_ep(
    state: &mut UsbAudioChipState,
    chip: &Arc<UsbAudioChip>,
    fmt: &AudioFormat,
    rate: u32,
    period_bytes: u32,
    period_frames: u32,
    buffer_periods: u32,
    frame_bytes: u32,
) -> Result<*mut UsbEndpoint> {
    for ep_box in state.ep_list.iter_mut() {
        let ep = &mut **ep_box;
        if ep.iface == fmt.iface
            && ep.altsetting == fmt.altsetting
            && ep.ep_num == fmt.endpoint
        {
            ep.set_params(
                fmt,
                rate, period_bytes, period_frames, buffer_periods, frame_bytes,
            )?;
            return Ok(ep as *mut UsbEndpoint);
        }
    }

    let is_out = fmt.endpoint & bindings::USB_DIR_IN as u8 == 0;

    let mut new_ep = unsafe {
        UsbEndpoint::try_new(
            chip.dev.clone(),
            fmt.endpoint,
            SND_USB_ENDPOINT_TYPE_DATA,
            is_out,
            fmt.iface,
            fmt.altsetting,
            fmt.maxpacksize,
            fmt.datainterval,
            chip.quirk_flags.load(core::sync::atomic::Ordering::Relaxed),
            &chip.shutdown as *const _,
        )
    }?;

    new_ep.set_params(
        fmt,
        rate, period_bytes, period_frames, buffer_periods, frame_bytes,
    )?;

    let ep_ptr = &mut *new_ep as *mut UsbEndpoint;
    state.ep_list.push(new_ep, GFP_KERNEL)?;
    Ok(ep_ptr)
}

fn lowlatency_playback_available(
    sub: &Substream,
    subs: &UsbSubstream,
    chip: &UsbAudioChip,
) -> bool {
    if sub.stream() == StreamDir::Capture {
        return false;
    }
    if !chip.lowlatency {
        return false;
    }
    let raw = sub.runtime().as_raw_runtime();
    if raw.is_null() {
        return false;
    }
    // SAFETY: raw is non-null and valid.
    let is_free_wheel = unsafe { (*raw).stop_threshold > (*raw).buffer_size };
    if is_free_wheel {
        return false;
    }
    if let Some(ep) = subs.data_ep() {
        if ep.implicit_feedback_sink() {
            return false;
        }
    }
    true
}

//
fn do_open(stream: &UsbStream, sub: &Substream) -> Result {
    let subs = stream.substream(sub.stream());

    subs.pcm_substream.store(sub.as_ptr(), Ordering::Release);

    let is_full = subs.speed == bindings::usb_device_speed_USB_SPEED_FULL as u32;
    let mut rates = 0u32;
    let mut rate_min = u32::MAX;
    let mut rate_max = 0u32;
    let mut ch_min = u32::MAX;
    let mut ch_max = 0u32;
    let mut has_implicit_fb = false;
    let mut ptmin = u32::MAX;

    let state = stream.chip.mutex.lock();
    for fp in subs.fmt_list.access(&*state).iter() {
        rates |= fp.rates;
        rate_min = rate_min.min(fp.rate_min);
        rate_max = rate_max.max(fp.rate_max);
        ch_min = ch_min.min(fp.channels);
        ch_max = ch_max.max(fp.channels);
        if fp.implicit_fb {
            has_implicit_fb = true;
        }
        // Mirrors: pt = 125 * (1 << fp->datainterval) in setup_hw_info().
        // datainterval = 0 yields 125us (the base high-speed USB interval).
        let pt = 125u32 * (1u32 << (fp.datainterval as u32).min(20));
        ptmin = ptmin.min(pt);
    }
    drop(state);

    // Full-speed devices have a fixed 1ms packet interval.
    if is_full {
        ptmin = 1000;
    }
    // If ptmin == 1000 the period_time rule adds nothing useful; skip it.
    let param_period_time = if ptmin < 1000 {
        hw_param::PERIOD_TIME
    } else {
        -1
    };

    let mut info = usb_hardware().info;
    if has_implicit_fb {
        info |= bindings::SNDRV_PCM_INFO_JOINT_DUPLEX;
    }

    let hw = Hardware {
        formats: subs.formats.get(),
        rates,
        rate_min,
        rate_max,
        info: info,
        channels_min: ch_min,
        channels_max: ch_max,
        ..usb_hardware()
    };

    let runtime = sub.runtime();
    runtime.set_hw(&hw);

    // SAFETY: set_private_data_arc::<UsbStream> was called when registering
    // this PCM device, and the Arc is alive for the duration of the stream.
    let stream_arc: Arc<UsbStream> = unsafe { sub.clone_private_arc::<UsbStream>() };
    let base = UsbHwRuleBase { stream: stream_arc, dir: sub.stream() };

    let rule_set = KBox::new(UsbHwRuleSet {
        rate:        RateRule(base.clone()),
        channels:    ChannelsRule(base.clone()),
        format:      FormatRule(base.clone()),
        period_time: PeriodTimeRule(base.clone()),
        period_size: PeriodSizeRule(base.clone()),
        periods:     PeriodsRule(base),
    }, GFP_KERNEL)?;

    // Transfer ownership to the runtime; private_free handles cleanup.
    let handle: HwRuleHandle<UsbHwRuleSet> = runtime.store_hw_rules(rule_set);

    // Minimum period_time constraint from datainterval.
    runtime.hw_constraint_minmax(hw_param::PERIOD_TIME, ptmin, u32::MAX)?;

    // Cross-parameter consistency rules.
    handle.add_rule(&runtime, 0, hw_param::RATE,
        |rs| &rs.rate,
        hw_param::RATE, hw_param::FORMAT, hw_param::CHANNELS, param_period_time)?;
    handle.add_rule(&runtime, 0, hw_param::CHANNELS,
        |rs| &rs.channels,
        hw_param::CHANNELS, hw_param::FORMAT, hw_param::RATE, param_period_time)?;
    handle.add_rule(&runtime, 0, hw_param::FORMAT,
        |rs| &rs.format,
        hw_param::FORMAT, hw_param::RATE, hw_param::CHANNELS, param_period_time)?;

    if param_period_time >= 0 {
        handle.add_rule(&runtime, 0, hw_param::PERIOD_TIME,
            |rs| &rs.period_time,
            hw_param::FORMAT, hw_param::CHANNELS, hw_param::RATE, -1)?;
    }

    // Cap period and buffer durations to 1s and 2s respectively.
    runtime.hw_constraint_minmax(hw_param::PERIOD_TIME, 0, 1_000_000)?;
    runtime.hw_constraint_minmax(hw_param::BUFFER_TIME, 0, 2_000_000)?;

    // Implicit-feedback period_size and periods constraints.
    handle.add_rule(&runtime, 0, hw_param::PERIOD_SIZE,
        |rs| &rs.period_size,
        hw_param::PERIOD_SIZE, -1, -1, -1)?;
    handle.add_rule(&runtime, 0, hw_param::PERIODS,
        |rs| &rs.periods,
        hw_param::PERIODS, -1, -1, -1)?;

    Ok(())
}

// Ops implementation
//
impl Ops for UsbStream {
    const NONATOMIC: bool = true;

    fn open(&self, sub: &Substream) -> Result {
        self.chip.autoresume()?;
        match do_open(self, sub) {
            Ok(()) => Ok(()),
            Err(e) => {
                self.chip.autosuspend();
                Err(e)
            }
        }
    }

    fn close(&self, sub: &Substream) -> Result {
        let subs = self.substream(sub.stream());
        subs.pcm_substream.store(core::ptr::null_mut(), Ordering::Release);
        self.chip.autosuspend();
        Ok(())
    }

    fn hw_params(&self, sub: &Substream, params: &HwParams) -> Result {
        let subs = self.substream(sub.stream());
        let chip = &self.chip;

        let rate = params.rate();
        let channels = params.channels();
        let pcm_format = params.format();
        let period_size = params.period_size();
        let buffer_size = params.buffer_size();

        let mut state = chip.mutex.lock();
        let fmt_ptr =
            crate::stream::find_format(subs.fmt_list.access(&*state), pcm_format, rate, channels)
                .ok_or(EINVAL)?;
        let fmt = unsafe { &*fmt_ptr };

        let frame_bytes = fmt.channels * fmt.fmt_sz;
        let period_bytes = period_size * frame_bytes;
        let buffer_periods = if period_size > 0 { buffer_size / period_size } else { 2 };

        let ep_ptr = find_or_create_ep(
            &mut state, chip,
            fmt, rate,
            period_bytes, period_size, buffer_periods, frame_bytes,
        )?;

        subs.set_cur_audiofmt(fmt_ptr);
        subs.set_data_endpoint(ep_ptr);
        subs.set_buffer_bytes(buffer_size * frame_bytes);
        Ok(())
    }

    fn hw_free(&self, sub: &Substream) -> Result {
        let subs = self.substream(sub.stream());
        subs.stop_endpoints(false);
        subs.sync_pending_stops();
        subs.set_cur_audiofmt(core::ptr::null());
        subs.set_data_endpoint(core::ptr::null_mut());
        subs.set_sync_endpoint(core::ptr::null_mut());
        Ok(())
    }

    fn prepare(&self, sub: &Substream) -> Result {
        let subs = self.substream(sub.stream());
        let chip = &self.chip;

        if chip.shutdown.load(Ordering::Relaxed) != 0 { return Err(ENODEV); }

        let mut xfer_guard = subs.xfer().lock();
        xfer_guard.reset();
        core::mem::drop(xfer_guard);

        let lowlat = lowlatency_playback_available(sub, subs, chip);
        subs.lowlatency_playback.store(if lowlat { 1 } else { 0 }, Ordering::Release);

        let runtime = sub.runtime();
        let fmt_ptr = subs.cur_fmt();
        if !fmt_ptr.is_null() {
            // SAFETY: fmt_ptr is valid.
            let fmt = unsafe { &*fmt_ptr };
            let frame_bytes = fmt.channels * fmt.fmt_sz;
            subs.set_buffer_bytes(runtime.buffer_size() as u32 * frame_bytes);

            // 1. Activate the interface alternate setting first with deselect workaround!
            if let Some(ep) = subs.data_ep() {
                ep.set_interface(false)?; // Deselect first!
                ep.set_interface(true)?;  // Re-select!
            }
            if let Some(sep) = subs.sync_ep() {
                sep.set_interface(false)?;
                sep.set_interface(true)?;
            }

            let mut state = chip.mutex.lock();

            // Set the sample rate on the physically active USB device endpoint.
            let altsetting = crate::helper::find_ctrl_interface(chip.device())?;
            let ctrl_intf_num = altsetting.number();
            let quirk_flags = chip.quirk_flags.load(Ordering::Relaxed);

            crate::clock::init_sample_rate(
                chip.bound_device(),
                ctrl_intf_num,
                quirk_flags,
                &mut state.sample_rate_read_error,
                fmt,
                runtime.rate(),
            )?;
            core::mem::drop(state);
        }

        if dir_index(sub.stream()) == 0 && subs.lowlatency_playback.load(Ordering::Relaxed) == 0 {
            subs.start_endpoints()?;
        }
        Ok(())
    }

    fn trigger(&self, sub: &Substream, cmd: TriggerCommand) -> Result {
        let subs = self.substream(sub.stream());
        let idx = dir_index(sub.stream());

        match cmd {
            TriggerCommand::Start => {
                subs.xfer().lock().trigger_tstamp_pending = true;
                if idx == 0 {
                    subs.running.store(1, Ordering::Release);
                    if subs.lowlatency_playback.load(Ordering::Relaxed) != 0 {
                        subs.start_endpoints()?;
                    }
                } else {
                    subs.start_endpoints()?;
                    subs.running.store(1, Ordering::Release);
                }
            }
            TriggerCommand::PauseRelease => {
                subs.running.store(1, Ordering::Release);
            }
            TriggerCommand::Suspend | TriggerCommand::Stop => {
                subs.stop_endpoints_async();
                subs.running.store(0, Ordering::Release);
            }
            TriggerCommand::PausePush => {
                subs.running.store(0, Ordering::Release);
            }
            _ => return Err(EINVAL),
        }
        Ok(())
    }

    fn pointer(&self, sub: &Substream) -> bindings::snd_pcm_uframes_t {
        if self.chip.shutdown.load(Ordering::Relaxed) != 0 {
            return !0;
        }
        let subs = self.substream(sub.stream());
        let hwptr = subs.xfer().lock().hwptr_done;
        let fmt = subs.cur_fmt();
        if fmt.is_null() { return 0; }
        // SAFETY: fmt is valid.
        let frame_bytes = unsafe { (*fmt).channels * (*fmt).fmt_sz } as usize;
        if frame_bytes == 0 { return 0; }
        (hwptr as usize / frame_bytes) as bindings::snd_pcm_uframes_t
    }

    fn sync_stop(&self, sub: &Substream) -> Result {
        let subs = self.substream(sub.stream());
        subs.stop_endpoints(false);
        subs.sync_pending_stops();
        Ok(())
    }
}
