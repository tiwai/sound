// SPDX-License-Identifier: GPL-2.0

//! Core type definitions for the Rust USB audio driver.

// Many constants and fields mirror the C driver's full API surface and are
// intentional placeholders for features not yet implemented in Rust.
#![allow(dead_code)]

use kernel::prelude::*;

//
// URB / packet constants
//
/// Maximum number of data URBs per endpoint.
pub(crate) const MAX_URBS: usize = 8;
/// Maximum ISO packets per URB (full-speed).
pub(crate) const MAX_PACKS: usize = 6;
/// Maximum ISO packets per URB (high-speed: 8x microframes).
pub(crate) const MAX_PACKS_HS: usize = MAX_PACKS * 8;
/// Number of sync URBs per sync endpoint.
pub(crate) const SYNC_URBS: usize = 4;
/// Maximum queue depth for low-latency playback.
pub(crate) const MAX_QUEUE: usize = 18;

//
// UAC protocol versions (stored in AudioFormat::protocol)
//
pub(crate) const UAC_VERSION_1: u8 = 0x00;
pub(crate) const UAC_VERSION_2: u8 = 0x20;
pub(crate) const UAC_VERSION_3: u8 = 0x30;

//
// USB class / subclass constants
//
pub(crate) const USB_CLASS_AUDIO: u8 = 0x01;
pub(crate) const USB_SUBCLASS_AUDIOCONTROL: u8 = 0x01;
pub(crate) const USB_SUBCLASS_AUDIOSTREAMING: u8 = 0x02;
pub(crate) const USB_SUBCLASS_MIDISTREAMING: u8 = 0x03;

//
// Card limits
//
/// Maximum USB interfaces that can be attached to one ALSA card.
pub(crate) const MAX_CARD_INTERFACES: usize = 16;

//
// UAC AudioControl descriptor subtypes (bDescriptorSubtype in AC interface)
//
/// AudioControl header descriptor subtype (UAC1: `uac1_ac_header_descriptor`).
pub(crate) const UAC_HEADER: u8 = 0x01;

//
// UAC descriptor subtypes (AudioStreaming interface)
//
pub(crate) const UAC_AS_GENERAL: u8 = 0x01;
pub(crate) const UAC_FORMAT_TYPE: u8 = 0x02;
pub(crate) const UAC_FORMAT_SPECIFIC: u8 = 0x03;

pub(crate) const UAC_FORMAT_TYPE_I: u8 = 0x01;
pub(crate) const UAC_FORMAT_TYPE_II: u8 = 0x02;
pub(crate) const UAC_FORMAT_TYPE_III: u8 = 0x03;

//
// AudioFormat - parsed representation of one USB alternate setting
//
/// Parsed audio format descriptor for one USB alternate setting.
///
/// Corresponds to `struct audioformat` in `sound/usb/card.h`.
pub(crate) struct AudioFormat {
    /// ALSA format bitmask (one or more `SNDRV_PCM_FMTBIT_*` bits).
    pub formats: u64,
    pub channels: u32,
    pub fmt_type: u8,
    /// Significant bits per sample (e.g. 24 for 24-bit audio).
    pub fmt_bits: u32,
    /// Container byte size (e.g. 4 for 24-bit packed in 32 bits).
    pub fmt_sz: u32,
    /// USB interface number.
    pub iface: u8,
    pub altsetting: u8,
    /// Endpoint address (number + direction bit).
    pub endpoint: u8,
    /// bmAttributes of the class-specific endpoint descriptor (UAC_EP_CS_ATTR_*).
    pub attributes: u8,
    /// bmAttributes of the endpoint descriptor.
    pub ep_attr: u8,
    /// Index of the data endpoint in the altsetting's endpoint list.
    pub ep_idx: u8,
    /// Sync endpoint address (0 if none).
    pub sync_ep: u8,
    pub sync_iface: u8,
    pub sync_altsetting: u8,
    pub sync_ep_idx: u8,
    /// True if this format participates as an implicit feedback sink.
    pub implicit_fb: bool,
    /// Data interval (exponent for high-speed: 2^n microframes).
    pub datainterval: u8,
    /// UAC protocol version (`UAC_VERSION_*`).
    pub protocol: u8,
    /// Maximum packet size in bytes.
    pub maxpacksize: u32,
    /// Supported rates bitmask (`SNDRV_PCM_RATE_*`).
    pub rates: u32,
    pub rate_min: u32,
    pub rate_max: u32,
    /// Explicit rate table (for `SNDRV_PCM_RATE_KNOT`).
    pub rate_table: KVec<u32>,
    /// UAC2/3 clock entity ID for sample rate control.
    pub clock: u8,
    /// DSD-over-PCM (DoP) mode.
    pub dsd_dop: bool,
    pub dsd_bitrev: bool,
    pub dsd_raw: bool,
}

impl AudioFormat {
    /// Returns the number of bytes per PCM frame at the stored `fmt_sz`.
    pub(crate) fn frame_bytes(&self) -> u32 {
        self.channels * self.fmt_sz
    }
}
