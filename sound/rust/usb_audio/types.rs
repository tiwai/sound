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
