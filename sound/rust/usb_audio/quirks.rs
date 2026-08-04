// SPDX-License-Identifier: GPL-2.0

//! Device quirk flags lookup.
//!
//! Corresponds to parts of `sound/usb/quirks.c` and `quirks-table.h`.
//! Only flag-based quirks are implemented; structural quirks are deferred.


/// Returns the `QUIRK_FLAG_*` bitmask for a given USB `usb_id`
/// (`(vendor << 16) | product`).
///
/// An empty table is used until device-specific entries are added.
pub(crate) fn quirk_flags_for_id(_usb_id: u32) -> u32 {
    // TODO: populate from quirks-table.h entries.
    0
}

/// Returns true if this USB interface should be treated as an audio streaming
/// interface even though it lacks the standard class code.
#[allow(dead_code)]
pub(crate) fn is_audio_iface_quirk(_usb_id: u32) -> bool {
    false
}
