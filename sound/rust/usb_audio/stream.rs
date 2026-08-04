// SPDX-License-Identifier: GPL-2.0

//! USB audio stream and substream management.
//!
//! Corresponds to `sound/usb/stream.c`.

use kernel::prelude::*;
use kernel::{bindings, usb, sync::Arc};
use core::sync::atomic::{AtomicI32, AtomicPtr};
use core::cell::UnsafeCell;

use crate::types::*;
use crate::card::{UsbAudioChip, UsbAudioChipState};
use crate::endpoint::UsbEndpoint;
use kernel::sound::{card::Card, pcm::{Pcm, StreamDir}};

pub struct SubstreamXfer {
    pub hwptr_done: u32,
    pub transfer_done: u32,
    pub frame_limit: u32,
    pub inflight_bytes: u32,
    pub last_frame_number: u32,
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
            last_frame_number: 0,
            period_elapsed_pending: 0,
            trigger_tstamp_pending: false,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::zero();
    }
}

pub struct UsbSubstream {
    pub direction: i32,
    pub ep_num: u8,
    pub formats: u64,
    pub num_formats: u32,
    pub fmt_type: u8,
    pub fmt_list: KVec<AudioFormat>,
    pub cur_audiofmt: UnsafeCell<*const AudioFormat>,
    pub data_endpoint: UnsafeCell<*mut UsbEndpoint>,
    pub sync_endpoint: UnsafeCell<*mut UsbEndpoint>,
    pub pcm_substream: AtomicPtr<bindings::snd_pcm_substream>,
    pub running: AtomicI32,
    pub lowlatency_playback: AtomicI32,
    pub buffer_bytes: UnsafeCell<u32>,
    pub xfer: UnsafeCell<SubstreamXfer>,
    pub dev: *mut bindings::usb_device,
    pub speed: u32,
}

unsafe impl Send for UsbSubstream {}
unsafe impl Sync for UsbSubstream {}

impl UsbSubstream {
    pub fn new(dev: *mut bindings::usb_device, speed: u32, direction: i32) -> Self {
        Self {
            direction,
            ep_num: 0,
            formats: 0,
            num_formats: 0,
            fmt_type: 0,
            fmt_list: KVec::new(),
            cur_audiofmt: UnsafeCell::new(core::ptr::null()),
            data_endpoint: UnsafeCell::new(core::ptr::null_mut()),
            sync_endpoint: UnsafeCell::new(core::ptr::null_mut()),
            pcm_substream: AtomicPtr::new(core::ptr::null_mut()),
            running: AtomicI32::new(0),
            lowlatency_playback: AtomicI32::new(0),
            buffer_bytes: UnsafeCell::new(0),
            xfer: UnsafeCell::new(SubstreamXfer::zero()),
            dev,
            speed,
        }
    }

    pub fn data_ep(&self) -> *mut UsbEndpoint {
        unsafe { *self.data_endpoint.get() }
    }

    pub fn sync_ep(&self) -> *mut UsbEndpoint {
        unsafe { *self.sync_endpoint.get() }
    }

    pub(crate) fn prepare_urb(
        &self,
        ctx: &crate::endpoint::UrbCtx,
        mut urb: core::pin::Pin<&mut usb::Urb<crate::endpoint::UrbCtx>>,
        in_stream_lock: bool,
    ) -> i32 {
        0
    }

    pub(crate) fn retire_urb(
        &self,
        urb: core::pin::Pin<&mut usb::Urb<crate::endpoint::UrbCtx>>,
    ) {
    }

    pub fn cur_fmt(&self) -> *const AudioFormat {
        unsafe { *self.cur_audiofmt.get() }
    }
}

pub struct UsbStream {
    pub chip: Arc<UsbAudioChip>,
    pub pcm: UnsafeCell<*mut bindings::snd_pcm>,
    pub pcm_index: i32,
    pub fmt_type: u8,
    pub substream: [UsbSubstream; 2],
}

unsafe impl Send for UsbStream {}
unsafe impl Sync for UsbStream {}

use kernel::sound::pcm::OpsTable;
pub static USB_AUDIO_OPS: OpsTable<UsbStream> = OpsTable::new();
