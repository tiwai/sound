// SPDX-License-Identifier: GPL-2.0

//! Device quirk flags lookup.
//!
//! Corresponds to parts of `sound/usb/quirks.c` and `quirks-table.h`.
//! Only flag-based quirks are implemented; structural quirks are deferred.

use crate::types::*;
use kernel::prelude::*;

// Helper to determine if a byte is whitespace.
fn is_whitespace(b: u8) -> bool {
    b == b' ' || b == b'\t' || b == b'\n' || b == b'\r'
}

// Helper to trim leading/trailing whitespace.
fn trim_bytes(mut bytes: &[u8]) -> &[u8] {
    while let Some((&first, rest)) = bytes.split_first() {
        if is_whitespace(first) {
            bytes = rest;
        } else {
            break;
        }
    }
    while let Some((&last, rest)) = bytes.split_last() {
        if is_whitespace(last) {
            bytes = rest;
        } else {
            break;
        }
    }
    bytes
}

// Parse u16 hex value.
fn parse_hex_u16(bytes: &[u8]) -> Option<u16> {
    if bytes.is_empty() {
        return None;
    }
    let mut val: u16 = 0;
    for &b in bytes {
        let digit = match b {
            b'0'..=b'9' => (b - b'0') as u16,
            b'a'..=b'f' => (b - b'a' + 10) as u16,
            b'A'..=b'F' => (b - b'A' + 10) as u16,
            _ => return None,
        };
        if val > 0x0fff {
            return None;
        }
        val = (val << 4) | digit;
    }
    Some(val)
}

// Check case-insensitive equality.
fn eq_ignore_ascii_case(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    for i in 0..a.len() {
        if a[i].to_ascii_lowercase() != b[i].to_ascii_lowercase() {
            return false;
        }
    }
    true
}

// Check if string ID (hex or wildcard *) matches actual ID.
fn match_id(id_bytes: &[u8], actual_id: u16) -> bool {
    let cleaned = trim_bytes(id_bytes);
    if cleaned == b"*" {
        return true;
    }
    if let Some(parsed) = parse_hex_u16(cleaned) {
        parsed == actual_id
    } else {
        false
    }
}

// Mapping of flag names to bit flags.
const KNOWN_FLAGS: &[(&[u8], u32)] = &[
    (b"get_sample_rate", QUIRK_FLAG_GET_SAMPLE_RATE),
    (b"share_media_device", QUIRK_FLAG_SHARE_MEDIA_DEVICE),
    (b"align_transfer", QUIRK_FLAG_ALIGN_TRANSFER),
    (b"tx_length", QUIRK_FLAG_TX_LENGTH),
    (b"playback_first", QUIRK_FLAG_PLAYBACK_FIRST),
    (b"skip_clock_selector", QUIRK_FLAG_SKIP_CLOCK_SELECTOR),
    (b"ignore_clock_source", QUIRK_FLAG_IGNORE_CLOCK_SOURCE),
    (b"itf_usb_dsd_dac", QUIRK_FLAG_ITF_USB_DSD_DAC),
    (b"ctl_msg_delay", QUIRK_FLAG_CTL_MSG_DELAY),
    (b"ctl_msg_delay_1m", QUIRK_FLAG_CTL_MSG_DELAY_1M),
    (b"ctl_msg_delay_5m", QUIRK_FLAG_CTL_MSG_DELAY_5M),
    (b"iface_delay", QUIRK_FLAG_IFACE_DELAY),
    (b"validate_rates", QUIRK_FLAG_VALIDATE_RATES),
    (b"disable_autosuspend", QUIRK_FLAG_DISABLE_AUTOSUSPEND),
    (b"ignore_ctl_error", QUIRK_FLAG_IGNORE_CTL_ERROR),
    (b"dsd_raw", QUIRK_FLAG_DSD_RAW),
    (b"set_iface_first", QUIRK_FLAG_SET_IFACE_FIRST),
    (b"generic_implicit_fb", QUIRK_FLAG_GENERIC_IMPLICIT_FB),
    (b"skip_implicit_fb", QUIRK_FLAG_SKIP_IMPLICIT_FB),
    (b"iface_skip_close", QUIRK_FLAG_IFACE_SKIP_CLOSE),
    (b"force_iface_reset", QUIRK_FLAG_FORCE_IFACE_RESET),
    (b"fixed_rate", QUIRK_FLAG_FIXED_RATE),
    (b"mic_res_16", QUIRK_FLAG_MIC_RES_16),
    (b"mic_res_384", QUIRK_FLAG_MIC_RES_384),
    (b"mixer_playback_min_mute", QUIRK_FLAG_MIXER_PLAYBACK_MIN_MUTE),
];

// Apply flags to the target bitmask.
fn apply_flags(flags_bytes: &[u8], flags: &mut u32) {
    let mut mask_flags = 0u32;
    let mut unmask_flags = 0u32;
    for flag_part in flags_bytes.split(|&c| c == b'|') {
        let mut flag = trim_bytes(flag_part);
        if flag.is_empty() {
            continue;
        }
        let is_unmask = if flag.starts_with(b"!") {
            flag = &flag[1..];
            true
        } else {
            false
        };
        let mut found = false;
        for &(name, bit) in KNOWN_FLAGS {
            if eq_ignore_ascii_case(flag, name) {
                if is_unmask {
                    unmask_flags |= bit;
                } else {
                    mask_flags |= bit;
                }
                found = true;
                break;
            }
        }
        if !found {
            let flag_bstr = kernel::str::BStr::from_bytes(flag);
            kernel::pr_warn!("snd_rust_usb_audio: unknown flag {} while parsing param quirks\n", flag_bstr);
        }
    }
    *flags &= !unmask_flags;
    *flags |= mask_flags;
}

/// Returns the `QUIRK_FLAG_*` bitmask for a given USB `usb_id`
/// (`(vendor << 16) | product`).
///
/// An empty table is used until device-specific entries are added.
pub(crate) fn quirk_flags_for_id(usb_id: u32) -> u32 {
    let mut flags = 0; // TODO: populate from quirks-table.h entries.

    // Apply custom quirks module parameter
    let quirks_param = crate::module_parameters::quirks.value();
    let quirks_bytes = quirks_param.as_bytes();

    let vendor = (usb_id >> 16) as u16;
    let product = (usb_id & 0xffff) as u16;

    for entry in quirks_bytes.split(|&c| c == b';') {
        let entry = trim_bytes(entry);
        if entry.is_empty() {
            continue;
        }
        let mut parts = entry.split(|&c| c == b':');
        let Some(vid_bytes) = parts.next() else { continue; };
        let Some(pid_bytes) = parts.next() else { continue; };
        let Some(flags_bytes) = parts.next() else { continue; };

        if !match_id(vid_bytes, vendor) {
            continue;
        }
        if !match_id(pid_bytes, product) {
            continue;
        }
        apply_flags(flags_bytes, &mut flags);
    }

    flags
}

/// Returns true if this USB interface should be treated as an audio streaming
/// interface even though it lacks the standard class code.
#[allow(dead_code)]
pub(crate) fn is_audio_iface_quirk(_usb_id: u32) -> bool {
    false
}
