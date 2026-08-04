// SPDX-License-Identifier: GPL-2.0

//! USB audio endpoint URB engine.
//!
//! Corresponds to `sound/usb/endpoint.c`.
//!
//! Uses the upstream Rust USB abstraction (`usb::Urb<T>` typestate) so that
//! each URB's transfer buffer is owned by the `UrbHandle` and freed on drop,
//! and the completion callback receives a safe `UrbResult<'_, T>` instead of
//! a raw `*mut bindings::urb`.

#![allow(dead_code)]

use kernel::prelude::*;
use kernel::{bindings, device, usb};
use kernel::sync::Arc;
use core::sync::atomic::{AtomicI32, AtomicPtr, Ordering};
use core::cell::UnsafeCell;

use crate::types::{
    AudioFormat,
    EP_STATE_STOPPED, EP_STATE_RUNNING, EP_STATE_STOPPING,
    SND_USB_ENDPOINT_TYPE_DATA, SND_USB_ENDPOINT_TYPE_SYNC,
    MAX_URBS, MAX_PACKS, MAX_PACKS_HS, SYNC_URBS, MAX_QUEUE,
    UAC_FORMAT_TYPE_II,
    QUIRK_FLAG_TX_LENGTH,
};
use crate::stream::UsbSubstream;

// Terminal USB error codes (negated errno values).
const ECONNRESET_ERRNO: i32 = -104;
const ESHUTDOWN_ERRNO: i32 = -108;

const UAC_EP_CS_ATTR_FILL_MAX: u8 = 0x80;

//
// Callback type aliases (C-callable PCM hooks)
//
/// Fill one outbound URB with PCM audio data (or silence).
/// Arguments: `(data_subs, urb, in_stream_lock)` -> 0 or negative errno.
pub(crate) type PrepareUrb = fn(&UsbSubstream, core::pin::Pin<&mut usb::Urb<UrbCtx>>, bool) -> i32;

/// Retire a completed inbound or outbound URB.
/// Arguments: `(data_subs, urb)`.
pub(crate) type RetireUrb = fn(&UsbSubstream, core::pin::Pin<&mut usb::Urb<UrbCtx>>);

//
// FreqState - Q16.16 packet-sizing engine
//
/// Interrupt-context frequency-tracking state.
///
/// Serialised by the endpoint spinlock in the C driver; here wrapped in
/// `UnsafeCell` - callers must uphold the same discipline.
pub(crate) struct FreqState {
    pub phase:        u32, // fractional phase accumulator
    pub freqn:        u32, // nominal Q16.16 samples-per-(micro)frame
    pub freqm:        u32, // momentary Q16.16 rate (updated from sync EP)
    pub freqmax:      u32, // max Q16.16 rate (freqn + 50 %)
    pub freqshift:    i32, // sync feedback format shift; i32::MIN = unknown
    pub sample_accum: u32, // fractional sample accumulator
    pub sample_rem:   u32, // rate % pps
    pub pps:          u32, // packets per second (1000 full-speed, 8000 HS)
}

//
// UrbCtx - per-URB driver context
//
/// Context attached to every ISO URB via `Arc<UrbCtx>`.
///
/// # Lifetime invariant
///
/// `ep` is a non-owning raw pointer to the owning `UsbEndpoint`.  The
/// endpoint always outlives all of its URBs: `endpoint_stop` kills every
/// URB synchronously (the `UrbHandle<T, Active>` Drop calls `usb_kill_urb`)
/// before the endpoint fields are freed.
pub(crate) struct UrbCtx {
    /// Index within `UsbEndpoint::urb_handles`.
    pub index:       usize,
    /// Non-owning back-pointer to the containing endpoint.
    pub ep:          *const UsbEndpoint,
    /// Non-owning back-pointer to the containing substream.
    pub subs:        *const UsbSubstream,
    /// Number of ISO packets in this URB.
    pub packets:     i32,
    /// Bytes dequeued from the PCM ring buffer (set by the prepare callback).
    pub queued:      core::sync::atomic::AtomicI32,
    /// Per-packet frame-count overrides for the implicit-feedback path.
    pub packet_size: [u32; MAX_PACKS_HS],
}

// SAFETY: raw pointer serialised by USB HC completion + endpoint state machine.
unsafe impl Send for UrbCtx {}
unsafe impl Sync for UrbCtx {}

impl UrbCtx {
    /// Returns a reference to the owning endpoint.
    ///
    /// # Safety
    /// The caller must ensure that the containing endpoint is still alive.
    /// In practice, the endpoint's stop/release synchronization guarantees that
    /// all active URBs are killed synchronously before the endpoint itself is destroyed.
    pub(crate) unsafe fn endpoint<'a>(&self) -> &'a UsbEndpoint {
        // SAFETY: The caller guarantees `self.ep` is valid and the endpoint is alive.
        unsafe { &*self.ep }
    }
}

//
// PacketInfoEntry - implicit-feedback ring buffer entry
//
pub(crate) struct PacketInfoEntry {
    pub packets:     i32,
    pub packet_size: [u32; MAX_PACKS_HS],
}

//
// UsbEndpoint
//
/// A USB audio data or sync endpoint with its URB pool and streaming state.
///
/// The `urb_handles` field is declared **first** so that it is dropped
/// before any other fields; each `UrbHandle<UrbCtx, Active>` Drop calls
/// `usb_kill_urb` (synchronous), ensuring no completion handler accesses
/// freed endpoint state.
pub(crate) struct UsbEndpoint {
    // Active URB handles (MUST be first for correct drop ordering)
    pub urb_handles: UnsafeCell<KVec<usb::UrbHandle<UrbCtx, usb::Active>>>,

    // Identity
    pub ep_num:      u8,
    pub ep_type:     i32,   // SND_USB_ENDPOINT_TYPE_{DATA,SYNC}
    pub is_out:      bool,  // true = playback / host-to-device
    pub dev:         kernel::sync::aref::ARef<usb::Device>,

    pub iface:       u8,
    pub altsetting:  u8,
    pub syncmaxsize: u32,
    pub syncinterval: u8,

    // State machine
    pub state:   AtomicI32,  // EP_STATE_{STOPPED,RUNNING,STOPPING}
    pub running: AtomicI32,  // start/stop reference count

    // Frequency state (UnsafeCell; spinlock in C)
    pub freq: UnsafeCell<FreqState>,

    // Format parameters
    pub stride:         u32,
    pub silence_value:  u8,
    pub packsize:       [u32; 2],  // [min, max] frames per packet
    pub maxframesize:   u32,
    pub curframesize:   u32,
    pub curpacksize:    u32,
    pub maxpacksize:    u32,
    pub max_urb_frames: u32,
    pub datainterval:   u8,
    pub fill_max:       bool,

    // hw_params snapshot (set once in set_params)
    pub cur_rate:           u32,
    pub cur_frame_bytes:    u32,
    pub cur_period_bytes:   u32,
    pub cur_period_frames:  u32,
    pub cur_buffer_periods: u32,
    pub cur_audiofmt:       *const AudioFormat,

    // Behaviour flags
    pub implicit_fb_sync:    bool,
    pub lowlatency_playback: bool,
    pub tenor_fb_quirk:      bool,
    pub skip_packets:        i32,
    pub fixed_rate:          bool,

    // Setup flags
    pub need_prepare: bool,
    pub need_setup:   bool,
    pub iface_altset: AtomicI32,   // currently active altsetting (0 = deselected)

    // URB pool sizing (computed by {data,sync}_ep_set_params,
    // consumed by endpoint_start)
    pub nurbs:        usize, // number of URBs to submit
    pub urb_packs:    u32,   // ISO packets per URB
    pub urb_packets:  i32,   // urb_packs + UAC type-II bonus
    pub urb_buf_size: u32,   // transfer buffer bytes per URB
    pub urb_maxpkt:   u32,   // max bytes per ISO packet (= iso_packet_len)

    // Implicit-feedback packet-info ring buffer
    pub next_packet:        [PacketInfoEntry; MAX_URBS],
    pub next_packet_head:   u32,
    pub next_packet_queued: u32,

    // Cross-endpoint links
    pub sync_source: AtomicPtr<UsbEndpoint>,
    pub sync_sink:   AtomicPtr<UsbEndpoint>,

    // Chip shutdown flag
    pub shutdown: *const AtomicI32,

    pub quirk_flags: u32,
}

// SAFETY: UnsafeCell / AtomicI32 / raw-pointer fields; thread-safety
// is enforced by the USB HC completion serialisation + atomic state machine.
unsafe impl Send for UsbEndpoint {}
unsafe impl Sync for UsbEndpoint {}

//
// Constructor
//
impl UsbEndpoint {
    pub(crate) fn device(&self) -> &usb::Device {
        &self.dev
    }

    pub(crate) fn ep_state(&self) -> i32 {
        self.state.load(Ordering::Relaxed)
    }

    pub(crate) fn is_stopped(&self) -> bool {
        self.ep_state() == EP_STATE_STOPPED
    }

    /// Returns the current audio format reference, if set.
    pub(crate) fn cur_audiofmt(&self) -> Option<&AudioFormat> {
        if self.cur_audiofmt.is_null() {
            None
        } else {
            // SAFETY: `cur_audiofmt` is guaranteed to be valid for the duration
            // of the streaming state once configured.
            Some(unsafe { &*self.cur_audiofmt })
        }
    }

    /// Returns a mutable reference to the frequency tracking state.
    ///
    /// # Safety
    /// Caller must ensure that no other threads are concurrently accessing
    /// the frequency tracking state.
    pub(crate) unsafe fn freq_mut(&self) -> &mut FreqState {
        // SAFETY: Caller guarantees exclusive access.
        unsafe { &mut *self.freq.get() }
    }

    /// Returns an immutable reference to the frequency tracking state.
    ///
    /// # Safety
    /// Caller must ensure that no other threads are concurrently writing to
    /// the frequency tracking state.
    pub(crate) unsafe fn freq_ref(&self) -> &FreqState {
        // SAFETY: Caller guarantees no concurrent writes.
        unsafe { &*self.freq.get() }
    }

    /// Returns a mutable reference to the URB handles vector.
    ///
    /// # Safety
    /// Caller must ensure that no other threads are concurrently accessing
    /// the URB handles vector.
    pub(crate) unsafe fn urb_handles_mut(&self) -> &mut KVec<usb::UrbHandle<UrbCtx, usb::Active>> {
        // SAFETY: Caller guarantees exclusive access.
        unsafe { &mut *self.urb_handles.get() }
    }

    /// Returns true if the chip is in shutdown state.
    pub(crate) fn is_shutdown(&self) -> bool {
        if self.shutdown.is_null() {
            true
        } else {
            // SAFETY: `shutdown` pointer is guaranteed to be valid and outlive the endpoint.
            unsafe { &*self.shutdown }.load(Ordering::Relaxed) != 0
        }
    }

    /// Returns a reference to the bound interface.
    ///
    /// # Safety
    /// Caller must ensure that the interface is bound and valid.
    pub(crate) unsafe fn bound_interface(&self) -> Result<&usb::Interface<device::Bound>> {
        let intf = usb::ifnum_to_if(&self.dev, self.iface).ok_or(EINVAL)?;
        // SAFETY: `Interface<device::Normal>` has the same layout as `Interface<device::Bound>`
        // and the caller guarantees the interface is bound and valid.
        Ok(unsafe { &*(intf as *const usb::Interface as *const usb::Interface<device::Bound>) })
    }

    /// Allocates a new endpoint on the kernel heap.
    ///
    /// # Safety
    /// `shutdown` must be valid and outlive the endpoint.
    pub(crate) unsafe fn try_new(
        dev:          kernel::sync::aref::ARef<usb::Device>,
        ep_num:       u8,
        ep_type:      i32,
        is_out:       bool,
        iface:        u8,
        altsetting:   u8,
        syncmaxsize:  u32,
        syncinterval: u8,
        quirk_flags:  u32,
        shutdown:     *const AtomicI32,
    ) -> Result<KBox<Self>> {
        const EMPTY_ENTRY: PacketInfoEntry = PacketInfoEntry {
            packets:     0,
            packet_size: [0u32; MAX_PACKS_HS],
        };
        let ep = UsbEndpoint {
            urb_handles:    UnsafeCell::new(KVec::new()),
            ep_num, ep_type, is_out, dev, iface, altsetting,
            syncmaxsize, syncinterval,
            state:          AtomicI32::new(EP_STATE_STOPPED),
            running:        AtomicI32::new(0),
            freq: UnsafeCell::new(FreqState {
                phase: 0, freqn: 0, freqm: 0, freqmax: 0,
                freqshift: i32::MIN, sample_accum: 0, sample_rem: 0, pps: 0,
            }),
            stride: 0, silence_value: 0, packsize: [0; 2],
            maxframesize: 0, curframesize: 0, curpacksize: 0,
            maxpacksize: 0, max_urb_frames: 0, datainterval: 0, fill_max: false,
            cur_rate: 0, cur_frame_bytes: 0, cur_period_bytes: 0,
            cur_period_frames: 0, cur_buffer_periods: 0,
            cur_audiofmt: core::ptr::null(),
            implicit_fb_sync: false, lowlatency_playback: false,
            tenor_fb_quirk: false, skip_packets: 0, fixed_rate: false,
            need_prepare: true, need_setup: true, iface_altset: AtomicI32::new(0),
            nurbs: 0, urb_packs: 0, urb_packets: 0,
            urb_buf_size: 0, urb_maxpkt: 0,
            next_packet: [EMPTY_ENTRY; MAX_URBS],
            next_packet_head: 0, next_packet_queued: 0,
            sync_source: AtomicPtr::new(core::ptr::null_mut()),
            sync_sink:   AtomicPtr::new(core::ptr::null_mut()),
            shutdown, quirk_flags,
        };
        Ok(KBox::new(ep, GFP_KERNEL)?)
    }
}

//
// Q16.16 rate helpers
//
/// Hz -> Q16.16 full-speed format (samples/frame * 2^16).
fn get_usb_full_speed_rate(rate: u32) -> u32 { ((rate << 13) + 62) / 125 }

/// Hz -> Q16.16 high-speed format (samples/uframe * 2^16).
fn get_usb_high_speed_rate(rate: u32) -> u32 { ((rate << 10) + 62) / 125 }

//
// Packet-size calculation
//
fn next_packet_size(ep: &UsbEndpoint, avail: u32) -> i32 {
    if ep.fill_max { return ep.maxframesize as i32; }
    // SAFETY: Called under serialized completion handler context.
    let fs = unsafe { ep.freq_mut() };
    let accum = fs.sample_accum + fs.sample_rem;
    let (frames, new_accum) = if accum >= fs.pps {
        (ep.packsize[1], accum - fs.pps)
    } else {
        (ep.packsize[0], accum)
    };
    let frames = frames.min(ep.maxframesize);
    if avail > 0 && frames >= avail { return EAGAIN.to_errno(); }
    fs.sample_accum = new_accum;
    frames as i32
}

fn synced_next_packet_size(ep: &UsbEndpoint, avail: u32) -> i32 {
    if ep.fill_max { return ep.maxframesize as i32; }
    // SAFETY: Called under serialized completion handler context.
    let fs = unsafe { ep.freq_mut() };
    let phase = (fs.phase & 0xffff) + (fs.freqm << ep.datainterval as u32);
    let frames = (phase >> 16).min(ep.maxframesize);
    if avail > 0 && frames >= avail { return EAGAIN.to_errno(); }
    fs.phase = phase;
    frames as i32
}

// next_packet_size is now a method of UsbEndpoint.

//
// URB preparation helpers
// These operate on raw `*mut bindings::urb` obtained via `urb_handle.as_raw()`.
//

pub(crate) fn prepare_silent_urb(ep: &UsbEndpoint, ctx: &UrbCtx, urb: &mut usb::Urb<UrbCtx>) -> i32 {
    let buf_size = urb.transfer_buffer_length();
    let has_tx = ep.quirk_flags & QUIRK_FLAG_TX_LENGTH != 0 && ep.is_out;
    let extra: u32 = if has_tx { 4 } else { 0 };
    let mut offs: u32 = 0;
    let mut i = 0i32;

    let (buf, descs) = urb.isoc_buffers_mut();

    while i < ctx.packets {
        let len = ep.next_packet_size(ctx, i as usize, 0);
        if len < 0 { break; }
        let bytes = len as u32 * ep.stride;
        if offs + bytes + extra > buf_size { break; }

        let d = &mut descs[i as usize];
        d.set_offset(offs);
        d.set_length(bytes + extra);

        if has_tx {
            let start = offs as usize;
            buf[start..start + 4].copy_from_slice(&bytes.to_le_bytes());
            offs += 4;
        }

        let start = offs as usize;
        buf[start..start + bytes as usize].fill(ep.silence_value);

        offs += bytes;
        i += 1;
    }
    if offs == 0 { return EPIPE.to_errno(); }

    urb.set_number_of_packets(i);
    urb.set_transfer_buffer_length(offs);
    0
}

fn prepare_outbound_urb(
    ep:             &UsbEndpoint,
    ctx:            &UrbCtx,
    urb:            &mut usb::Urb<UrbCtx>,
    in_stream_lock: bool,
) -> i32 {
    match ep.ep_type {
        SND_USB_ENDPOINT_TYPE_DATA => {
            // SAFETY: The substream is guaranteed to outlive all active URBs on this endpoint.
            let subs = unsafe { &*ctx.subs };
            // SAFETY: `urb` is valid, initialized, and its lifetime is managed by the endpoint's URB pool.
            let urb_pin = unsafe { core::pin::Pin::new_unchecked(urb) };
            subs.prepare_urb(ctx, urb_pin, in_stream_lock)
        }
        SND_USB_ENDPOINT_TYPE_SYNC => {
            // SAFETY: Accessed under endpoint sequencing guarantees.
            let fs = unsafe { ep.freq_ref() };
            let hi = ep.dev.speed() >= bindings::usb_device_speed_USB_SPEED_HIGH;
            let (cp, descs) = urb.isoc_buffers_mut();
            let d = &mut descs[0];
            d.set_offset(0);
            if hi {
                d.set_length(4);
                cp[0] = (fs.freqn      ) as u8;
                cp[1] = (fs.freqn >>  8) as u8;
                cp[2] = (fs.freqn >> 16) as u8;
                cp[3] = (fs.freqn >> 24) as u8;
            } else {
                d.set_length(3);
                cp[0] = (fs.freqn >>  2) as u8;
                cp[1] = (fs.freqn >> 10) as u8;
                cp[2] = (fs.freqn >> 18) as u8;
            }
            0
        }
        _ => 0,
    }
}

fn prepare_inbound_urb(ep: &UsbEndpoint, ctx: &UrbCtx, urb: &mut usb::Urb<UrbCtx>) -> i32 {
    match ep.ep_type {
        SND_USB_ENDPOINT_TYPE_DATA => {
            let mut offs: u32 = 0;
            let descs = urb.iso_frame_descs_mut();
            for i in 0..ctx.packets as usize {
                let d = &mut descs[i];
                d.set_offset(offs);
                d.set_length(ep.curpacksize);
                offs += ep.curpacksize;
            }
            urb.set_transfer_buffer_length(offs);
            urb.set_number_of_packets(ctx.packets);
        }
        SND_USB_ENDPOINT_TYPE_SYNC => {
            let descs = urb.iso_frame_descs_mut();
            let d = &mut descs[0];
            d.set_length(4_u32.min(ep.syncmaxsize));
            d.set_offset(0);
        }
        _ => {}
    }
    0
}

//
// Implicit-feedback sink query
//
// implicit_feedback_sink is now a method of UsbEndpoint.

//
// ISO URB completion callback
//
/// ISO URB completion handler - called by the USB core (interrupt context).
///
/// Receives a safe `UrbResult` wrapping the completed URB; retrieves the
/// endpoint back-pointer from `Arc<UrbCtx>`, checks state, and re-submits
/// (or lets the URB drop if the endpoint is stopping).
fn snd_complete_urb(result: usb::UrbResult<'_, UrbCtx>) {
    let ctx_borrow = result.context().unwrap();
    // SAFETY: The endpoint is guaranteed to outlive all its active URBs.
    let ep = unsafe { ctx_borrow.endpoint() };
    // SAFETY: The substream is guaranteed to outlive all active URBs on this endpoint.
    let subs = unsafe { &*ctx_borrow.subs };

    let status = result.status();
    let terminal = status == ENOENT.to_errno()
        || status == ENODEV.to_errno()
        || status == ECONNRESET_ERRNO
        || status == ESHUTDOWN_ERRNO;

    if terminal
        || ep.is_shutdown()
        || ep.state.load(Ordering::Acquire) != EP_STATE_RUNNING
    {
        // Dropping `result` here lets the URB stay idle (not resubmitted).
        return;
    }

    // Convert the ArcBorrow into an owned Arc to decouple its lifetime from `result`.
    let ctx = Arc::from(ctx_borrow);

    if ep.is_out {
        // TX (playback) path
        // check_or_resubmit: on non-zero status, resubmits automatically and
        // returns Err; on success, returns Ok(UrbData).
        let Ok(mut urb_data) = result.check_or_resubmit(GFP_ATOMIC) else {
            return;
        };

        let urb = urb_data.urb_mut();
        // SAFETY: `urb` is valid, initialized, and its lifetime is managed by the endpoint's URB pool.
        let urb_pin = unsafe { core::pin::Pin::new_unchecked(&mut *urb) };
        subs.retire_urb(urb_pin);

        let do_resubmit = if ep.state.load(Ordering::Acquire) == EP_STATE_RUNNING
            && !ep.is_shutdown()
        {
            let ret = prepare_outbound_urb(ep, &*ctx, urb, false);
            ret >= 0 && ep.state.load(Ordering::Acquire) == EP_STATE_RUNNING
        } else {
            false
        };

        if do_resubmit {
            let _ = urb_data.resubmit(GFP_ATOMIC);
        }
        // If do_resubmit is false, urb_data is dropped (URB stays idle).
    } else {
        // RX (capture) path
        let Ok(mut urb_data) = result.check_or_resubmit(GFP_ATOMIC) else {
            return;
        };

        let urb = urb_data.urb_mut();
        // SAFETY: `urb` is valid, initialized, and its lifetime is managed by the endpoint's URB pool.
        let urb_pin = unsafe { core::pin::Pin::new_unchecked(&mut *urb) };
        subs.retire_urb(urb_pin);

        let do_resubmit = if ep.state.load(Ordering::Acquire) == EP_STATE_RUNNING
            && !ep.is_shutdown()
        {
            prepare_inbound_urb(ep, &*ctx, urb);
            true
        } else {
            false
        };

        if do_resubmit {
            let _ = urb_data.resubmit(GFP_ATOMIC);
        }
    }
}

//
// Helpers
//
fn div_round_up(n: u32, d: u32) -> u32 { (n + d - 1) / d }

//
// URB pool parameter computation (no URB allocation here)
//

fn data_ep_set_params(ep: &mut UsbEndpoint) -> Result<()> {
    let fmt_type = ep.cur_audiofmt().ok_or(EINVAL)?.fmt_type;

    let is_full = ep.dev.speed() == bindings::usb_device_speed_USB_SPEED_FULL;
    let is_out  = ep.is_out;
    let has_tx  = ep.quirk_flags & QUIRK_FLAG_TX_LENGTH != 0 && is_out;

    ep.stride = ep.cur_frame_bytes;

    let fs = ep.freq.get_mut();
    fs.freqmax = fs.freqn + (fs.freqn >> 1);

    let mut maxsize = (((fs.freqmax << ep.datainterval as u32) + 0xffff) >> 16)
        * ep.cur_frame_bytes;
    if has_tx { maxsize += 4; }
    if ep.maxpacksize > 0 && ep.maxpacksize < maxsize {
        let data_max = ep.maxpacksize - if has_tx { 4 } else { 0 };
        fs.freqmax = (data_max / ep.cur_frame_bytes) << (16 - ep.datainterval as u32);
        maxsize = ep.maxpacksize;
    }
    ep.curpacksize = if ep.fill_max { ep.maxpacksize } else { maxsize };

    let (packs_per_ms, max_ppu): (u32, u32) = if !is_full {
        (8 >> ep.datainterval as u32, MAX_PACKS_HS as u32)
    } else {
        (1, MAX_PACKS as u32)
    };
    let max_ppu = 1_u32.max(max_ppu >> ep.datainterval as u32);

    let (urb_packs, nurbs) = if !is_out || ep.implicit_fb_sync {
        let mut up = max_ppu.min(packs_per_ms);
        while up > 1 && up * maxsize >= ep.cur_period_bytes { up >>= 1; }
        (up, MAX_URBS as u32)
    } else {
        let minsize = ((fs.freqn >> (16 - ep.datainterval as u32))
            * ep.cur_frame_bytes).max(1);
        let max_pp_period = div_round_up(ep.cur_period_bytes, minsize);
        let urbs_per_period = div_round_up(max_pp_period, max_ppu);
        let up = div_round_up(max_pp_period, urbs_per_period);
        ep.max_urb_frames = div_round_up(ep.cur_period_frames, urbs_per_period);
        let max_u = (MAX_URBS as u32).min(MAX_QUEUE as u32 * packs_per_ms / up);
        (up, max_u.min(urbs_per_period * ep.cur_buffer_periods))
    };

    let nurbs   = nurbs as usize;
    // Packets per URB, plus a bonus packet for UAC Type-II format.
    let packets = urb_packs as i32
        + if fmt_type == UAC_FORMAT_TYPE_II { 1 } else { 0 };
    // Buffer large enough for `packets` * `maxsize` bytes each.
    let buf_size = maxsize * packets as u32;

    ep.nurbs        = nurbs;
    ep.urb_packs    = urb_packs;
    ep.urb_packets  = packets;
    ep.urb_buf_size = buf_size;
    ep.urb_maxpkt   = maxsize;

    ep.maxframesize = ep.maxpacksize / ep.cur_frame_bytes;
    ep.curframesize = ep.curpacksize / ep.cur_frame_bytes;
    ep.packsize[0]  = ep.packsize[0].min(ep.maxframesize);
    ep.packsize[1]  = ep.packsize[1].min(ep.maxframesize);
    Ok(())
}

fn sync_ep_set_params(ep: &mut UsbEndpoint) -> Result<()> {
    // Sync URBs each have a 4-byte buffer for the frequency feedback packet.
    let pkt = 4_u32.min(ep.syncmaxsize);
    ep.nurbs        = SYNC_URBS;
    ep.urb_packs    = 1;
    ep.urb_packets  = 1;
    ep.urb_buf_size = pkt;
    ep.urb_maxpkt   = pkt;
    Ok(())
}

//
// Public API
//

impl UsbEndpoint {
    /// Allocates URB pool parameters for the endpoint.
    ///
    /// Must be called with the chip mutex held and the endpoint stopped.
    /// No URBs are submitted here; call `start` to submit.
    pub(crate) fn set_params(
        &mut self,
        fmt:            &AudioFormat,
        rate:           u32,
        period_bytes:   u32,
        period_frames:  u32,
        buffer_periods: u32,
        frame_bytes:    u32,
    ) -> Result<()> {
        if !self.need_setup
            && self.cur_rate == rate
            && self.cur_frame_bytes == frame_bytes
            && self.cur_period_frames == period_frames
            && self.cur_period_bytes == period_bytes
            && self.cur_buffer_periods == buffer_periods
        {
            return Ok(());
        }

        // Clear any previously submitted URBs first.
        self.urb_handles.get_mut().clear();
        self.iface_altset.store(-1, Ordering::Relaxed);

        self.cur_audiofmt       = fmt;
        self.cur_rate           = rate;
        self.cur_period_bytes   = period_bytes;
        self.cur_period_frames  = period_frames;
        self.cur_buffer_periods = buffer_periods;
        self.cur_frame_bytes    = frame_bytes;
        self.datainterval       = fmt.datainterval;
        self.maxpacksize        = fmt.maxpacksize;
        self.fill_max           = fmt.attributes & UAC_EP_CS_ATTR_FILL_MAX != 0;

        let is_full = self.dev.speed() == bindings::usb_device_speed_USB_SPEED_FULL;

        let fs = self.freq.get_mut();
        if is_full {
            fs.freqn = get_usb_full_speed_rate(rate);
            fs.pps   = 1000 >> fmt.datainterval;
        } else {
            fs.freqn = get_usb_high_speed_rate(rate);
            fs.pps   = 8000 >> fmt.datainterval;
        }
        fs.sample_rem = rate % fs.pps;
        self.packsize[0] = rate / fs.pps;
        self.packsize[1] = (rate + fs.pps - 1) / fs.pps;

        if self.packsize[1] > self.maxpacksize {
            pr_info!(
                "snd_usb_audio: EP 0x{:x}: maxpacksize {} too small for rate {} / pps {}\n",
                self.ep_num, self.maxpacksize, rate, fs.pps,
            );
            return Err(EINVAL);
        }
        fs.freqm    = fs.freqn;
        fs.freqshift = i32::MIN;
        fs.phase     = 0;

        match self.ep_type {
            SND_USB_ENDPOINT_TYPE_DATA => data_ep_set_params(self)?,
            SND_USB_ENDPOINT_TYPE_SYNC => sync_ep_set_params(self)?,
            _ => return Err(EINVAL),
        }

        self.need_setup = false;
        Ok(())
    }

    /// Activates the USB interface altsetting for this endpoint.
    pub(crate) fn set_interface(&self, set: bool) -> Result {
        let altset = if set { self.altsetting } else { 0u8 };
        if self.iface_altset.load(Ordering::Relaxed) == altset as i32 { return Ok(()); }
        // SAFETY: The interface is bound during probe and is valid for the duration of the endpoint.
        let intf = unsafe { self.bound_interface()? };
        intf.set_interface(altset)?;
        self.iface_altset.store(altset as i32, Ordering::Relaxed);
        Ok(())
    }

    /// Submits the URB pool for this endpoint.
    ///
    /// Reference-counted: the first caller creates and submits all URBs.
    ///
    /// # Safety
    /// `self` must be valid and configured via `set_params`.
    pub(crate) fn start(&self, subs: &UsbSubstream) -> Result<()> {
        if self.is_shutdown() { return Err(EBADF); }

        if self.running.fetch_add(1, Ordering::AcqRel) != 0 { return Ok(()); }

        {
            // SAFETY: Called under serialized start.
            let fs = unsafe { self.freq_mut() };
            fs.phase = 0;
            fs.sample_accum = 0;
        }

        if self.state.compare_exchange(
            EP_STATE_STOPPED, EP_STATE_RUNNING, Ordering::SeqCst, Ordering::Relaxed,
        ).is_err() {
            self.running.fetch_sub(1, Ordering::AcqRel);
            return Err(EPIPE);
        }

        if let Err(e) = self.set_interface(true) {
            self.running.fetch_sub(1, Ordering::AcqRel);
            self.state.store(EP_STATE_STOPPED, Ordering::Release);
            return Err(e);
        }

        // SAFETY: The interface is bound during probe and is valid for the duration of the endpoint.
        let intf = unsafe { self.bound_interface()? };

        let cur_alt = intf.cur_altsetting();
        let host_ep = cur_alt
            .endpoints()
            .iter()
            .find(|e| e.desc().bEndpointAddress() == self.ep_num)
            .ok_or(EINVAL)?;

        let pipe = if self.is_out {
            usb::Pipe::new_send_isoc_pipe(&self.dev, host_ep)
        } else {
            usb::Pipe::new_receive_isoc_pipe(&self.dev, host_ep)
        };

        let interval: i32 = if self.ep_type == SND_USB_ENDPOINT_TYPE_SYNC {
            1i32 << self.syncinterval as u32
        } else {
            1i32 << self.datainterval as u32
        };

        // iso_packet_len for new_isoc: chosen so packets * iso_packet_len <= buf_size.
        let iso_packet_len: u16 = if self.urb_packets > 0 {
            (self.urb_buf_size / self.urb_packets as u32)
                .min(u16::MAX as u32) as u16
        } else {
            0
        };

        // SAFETY: Called under serialized start.
        let urb_handles = unsafe { self.urb_handles_mut() };

        if urb_handles.reserve(self.nurbs, GFP_KERNEL).is_err() {
            self.stop(false);
            return Err(ENOMEM);
        }

        let mut submitted = 0usize;

        for i in 0..self.nurbs {
            let buf_size = self.urb_buf_size as usize;
            if buf_size == 0 {
                self.stop(false);
                return Err(EINVAL);
            }
            // Allocate the zeroed transfer buffer.
            let buf = match KBox::new_zeroed_slice(buf_size, GFP_KERNEL) {
                Ok(b)  => b,
                Err(e) => {
                    self.stop(false);
                    return Err(e.into());
                }
            };

            // Build the per-URB context Arc.
            let ctx = match Arc::new(
                UrbCtx {
                    index:       i,
                    ep:          self as *const UsbEndpoint,
                    subs:        subs as *const UsbSubstream,
                    packets:     self.urb_packets,
                    queued:      core::sync::atomic::AtomicI32::new(0),
                    packet_size: [0u32; MAX_PACKS_HS],
                },
                GFP_KERNEL,
            ) {
                Ok(c)  => c,
                Err(_) => {
                    self.stop(false);
                    return Err(ENOMEM);
                }
            };

            // Create the idle URB handle.
            let mut handle = match usb::Urb::<UrbCtx>::new_isoc(
                GFP_KERNEL,
                intf,
                pipe,
                buf,
                Some(ctx.clone()),
                snd_complete_urb,
                self.urb_packets as u32,
                iso_packet_len,
                usb::TransferFlag::IsoAsap.into(),
                interval,
            ) {
                Ok(h)  => h,
                Err(e) => {
                    self.stop(false);
                    return Err(e);
                }
            };

            // Prepare initial frame descriptors (fills in offsets, lengths, data).
            // Safe to mutably prepare because it is Idle.
            // SAFETY: `handle` is Idle (not submitted) so we can mutably project it
            // to access its fields mutably.
            let handle_mut = unsafe { handle.as_mut().get_unchecked_mut() };
            let ret = if self.is_out {
                prepare_outbound_urb(self, &*ctx, handle_mut, true)
            } else {
                prepare_inbound_urb(self, &*ctx, handle_mut)
            };

            if ret < 0 {
                if ret == EAGAIN.to_errno() { break; }
                self.stop(false);
                return Err(EPIPE);
            }

            if self.is_shutdown() {
                self.stop(false);
                return Err(ENODEV);
            }

            // Submit: transitions Idle -> Active.
            let active = match handle.submit(GFP_KERNEL) {
                Ok(a)  => a,
                Err(e) => {
                    self.stop(false);
                    return Err(e);
                }
            };

            // Store the active handle (pre-reserved, so push does not allocate).
            if urb_handles.push(active, GFP_KERNEL).is_err() {
                self.stop(false);
                return Err(ENOMEM);
            }

            submitted += 1;
        }

        if submitted == 0 {
            self.stop(false);
            return Err(EPIPE);
        }
        Ok(())
    }

    /// Asynchronously signals the endpoint to stop resubmitting URBs.
    /// Safe to call from atomic/interrupt context.
    pub(crate) fn stop_async(&self) {
        let _ = self.state.compare_exchange(
            EP_STATE_RUNNING, EP_STATE_STOPPING, Ordering::SeqCst, Ordering::Relaxed,
        );
    }

    /// Stops streaming.  Reference-counted: the last caller kills all URBs.
    ///
    /// Dropping `UrbHandle<T, Active>` calls `usb_kill_urb` (synchronous),
    /// so this function may sleep.
    pub(crate) fn stop(&self, _keep_pending: bool) {
        if self.running.load(Ordering::Acquire) == 0 { return; }
        if self.running.fetch_sub(1, Ordering::AcqRel) != 1 { return; }

        let _ = self.state.compare_exchange(
            EP_STATE_RUNNING, EP_STATE_STOPPING, Ordering::SeqCst, Ordering::Relaxed,
        );

        // Dropping each `UrbHandle<UrbCtx, Active>` calls `usb_kill_urb`, which
        // blocks until the completion handler returns and then the URB is idle.
        // SAFETY: Called under serialized stop context.
        let urb_handles = unsafe { self.urb_handles_mut() };
        urb_handles.clear();

        self.state.store(EP_STATE_STOPPED, Ordering::Release);

        let _ = self.set_interface(false);
    }

    /// Ensures all pending stops have completed.
    ///
    /// With the new URB ownership model `stop` is already synchronous
    /// (`usb_kill_urb` in UrbHandle Drop blocks until complete), so this just
    /// ensures the state machine reaches STOPPED.
    pub(crate) fn sync_pending_stop(&self) {
        // Transition STOPPING -> STOPPED if not already done.
        let _ = self.state.compare_exchange(
            EP_STATE_STOPPING, EP_STATE_STOPPED, Ordering::SeqCst, Ordering::Relaxed,
        );
    }

    /// Kills and frees all URBs and resets the endpoint.
    pub(crate) fn release(&mut self) {
        self.urb_handles.get_mut().clear();
        self.state.store(EP_STATE_STOPPED, Ordering::Release);
    }

    /// Returns `true` if this is a playback endpoint driven by implicit feedback.
    pub(crate) fn implicit_feedback_sink(&self) -> bool {
        self.implicit_fb_sync && self.is_out
    }

    /// Returns the frame count for the next packet, honouring per-packet overrides.
    pub(crate) fn next_packet_size(
        &self,
        ctx:   &UrbCtx,
        idx:   usize,
        avail: u32,
    ) -> i32 {
        let packet = ctx.packet_size.get(idx).copied().unwrap_or(0);
        if packet != 0 {
            let packet = packet.min(self.maxframesize);
            if avail > 0 && packet >= avail { return EAGAIN.to_errno(); }
            return packet as i32;
        }
        if !self.sync_source.load(Ordering::Relaxed).is_null() {
            synced_next_packet_size(self, avail)
        } else {
            next_packet_size(self, avail)
        }
    }
}
