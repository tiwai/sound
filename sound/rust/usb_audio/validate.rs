// SPDX-License-Identifier: GPL-2.0

//! Minimal USB audio descriptor validation.
//!
//! Corresponds to `sound/usb/validate.c`.  The full C version validates every
//! field of every descriptor type; here we only check the minimum length so
//! that descriptor parsing code can safely access the fields it needs.

/// Returns `true` if a class-specific interface descriptor of subtype `sub`
/// and claimed `bLength` bytes meets the minimum size requirements.
///
/// Unknown subtypes are accepted (returns `true`).
pub(crate) fn validate_audio_desc(buf: &[u8], protocol: u8, sub: u8) -> bool {
    use crate::types::*;

    // We only do length validation; the buf slice already embeds the true length.
    // The minimum length for any descriptor is 2 bytes (bLength + bDescriptorType).
    if buf.len() < 2 {
        return false;
    }

    let blen = buf[0] as usize;
    if blen > buf.len() {
        return false;
    }

    // Subtype-specific minimums (UAC1).
    let min = match protocol {
        UAC_VERSION_1 => match sub {
            UAC_AS_GENERAL => 7,
            UAC_FORMAT_TYPE => 8,
            _ => 3,
        },
        UAC_VERSION_2 => match sub {
            UAC_AS_GENERAL => 16,
            UAC_FORMAT_TYPE => 6,
            _ => 3,
        },
        _ => 3,
    };

    blen >= min
}
