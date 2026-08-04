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
// Endpoint type (data vs. sync)
//
pub(crate) const SND_USB_ENDPOINT_TYPE_DATA: i32 = 0;
pub(crate) const SND_USB_ENDPOINT_TYPE_SYNC: i32 = 1;

//
// Endpoint state machine
//
pub(crate) const EP_STATE_STOPPED: i32 = 0;
pub(crate) const EP_STATE_RUNNING: i32 = 1;
pub(crate) const EP_STATE_STOPPING: i32 = 2;

//
// Quirk flags (QUIRK_FLAG_* bitmask, mirrors usbaudio.h QUIRK_TYPE_* enum)
//
/// Use GET_CUR for setting sample rate (some devices expect it).
pub(crate) const QUIRK_FLAG_GET_SAMPLE_RATE: u32     = 1 << 0;
/// Share the media device with the video driver.
pub(crate) const QUIRK_FLAG_SHARE_MEDIA_DEVICE: u32  = 1 << 1;
/// Samples need alignment to 4-byte boundaries.
pub(crate) const QUIRK_FLAG_ALIGN_TRANSFER: u32      = 1 << 2;
/// Force tx_length_quirk.
pub(crate) const QUIRK_FLAG_TX_LENGTH: u32           = 1 << 3;
/// Start playback stream before capture (for sync setups).
pub(crate) const QUIRK_FLAG_PLAYBACK_FIRST: u32      = 1 << 4;
/// Skip clock selector; go directly to clock source.
pub(crate) const QUIRK_FLAG_SKIP_CLOCK_SELECTOR: u32 = 1 << 5;
/// Ignore clock source validity bit.
pub(crate) const QUIRK_FLAG_IGNORE_CLOCK_SOURCE: u32 = 1 << 6;
/// DSD/DoP capable device (via interface DSD).
pub(crate) const QUIRK_FLAG_ITF_USB_DSD_DAC: u32     = 1 << 7;
/// Add delays after USB control messages.
pub(crate) const QUIRK_FLAG_CTL_MSG_DELAY: u32       = 1 << 8;
/// Add 1 ms delay after USB control messages.
pub(crate) const QUIRK_FLAG_CTL_MSG_DELAY_1M: u32    = 1 << 9;
/// Add 5 ms delay after USB control messages.
pub(crate) const QUIRK_FLAG_CTL_MSG_DELAY_5M: u32    = 1 << 10;
/// Add delay after set_interface call.
pub(crate) const QUIRK_FLAG_IFACE_DELAY: u32         = 1 << 11;
/// Validate rate table by probing each rate.
pub(crate) const QUIRK_FLAG_VALIDATE_RATES: u32      = 1 << 12;
/// Disable autosuspend.
pub(crate) const QUIRK_FLAG_DISABLE_AUTOSUSPEND: u32 = 1 << 13;
/// Ignore errors from SET_CUR for sample rate.
pub(crate) const QUIRK_FLAG_IGNORE_CTL_ERROR: u32    = 1 << 14;
/// Force raw DSD mode.
pub(crate) const QUIRK_FLAG_DSD_RAW: u32             = 1 << 15;
/// Set interface before starting streams.
pub(crate) const QUIRK_FLAG_SET_IFACE_FIRST: u32     = 1 << 16;
/// Generic implicit feedback (try to detect from any capture endpoint).
pub(crate) const QUIRK_FLAG_GENERIC_IMPLICIT_FB: u32 = 1 << 17;
/// Disable implicit feedback detection entirely.
pub(crate) const QUIRK_FLAG_SKIP_IMPLICIT_FB: u32    = 1 << 18;
/// Skip interface close on inactive streams.
pub(crate) const QUIRK_FLAG_IFACE_SKIP_CLOSE: u32    = 1 << 19;
/// Force interface reset on stream stop.
pub(crate) const QUIRK_FLAG_FORCE_IFACE_RESET: u32   = 1 << 20;
/// Device supports only a fixed sample rate.
pub(crate) const QUIRK_FLAG_FIXED_RATE: u32          = 1 << 21;
/// Microphone reports 16-bit only.
pub(crate) const QUIRK_FLAG_MIC_RES_16: u32          = 1 << 22;
/// Microphone reports 384 kHz only.
pub(crate) const QUIRK_FLAG_MIC_RES_384: u32         = 1 << 23;
/// Force minimum mute value for mixer playback.
pub(crate) const QUIRK_FLAG_MIXER_PLAYBACK_MIN_MUTE: u32 = 1 << 24;

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

//
// PacketInfo - per-packet metadata in the next_packet ring buffer
//
/// Describes the content of one ISO packet that is ready to submit.
pub(crate) struct PacketInfo {
    /// Byte offset of this packet in the transfer buffer.
    pub offset: u32,
    /// Number of *frames* (not bytes) in this packet.
    pub frames: u32,
    /// Total byte length of this packet.
    pub bytes: u32,
    /// Byte count actually consumed from the PCM ring buffer.
    pub actual_length: u32,
}
