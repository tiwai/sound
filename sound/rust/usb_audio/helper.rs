// SPDX-License-Identifier: GPL-2.0

//! USB audio descriptor search utilities.
//!
//! Corresponds to `sound/usb/helper.c` and parts of `sound/usb/helper.h`.

use kernel::{bindings, usb};
use kernel::prelude::*;

//
// Descriptor search
//
/// Parses a raw descriptor byte slice and finds the first descriptor of type
/// `dtype` that starts strictly *after* the byte at `after_offset` (or from
/// the beginning if `after_offset` is `None`).
///
/// Returns the byte offset of the found descriptor within `buf`, or `None`.
pub(crate) fn find_desc(buf: &[u8], after_offset: Option<usize>, dtype: u8) -> Option<usize> {
    // Advance past the descriptor at `after_offset` by its bLength, not by 1.
    let start = after_offset.map_or(0, |o| o + buf.get(o).copied().unwrap_or(0) as usize);
    let mut p = start;

    while p < buf.len() {
        if buf.len() - p < 2 {
            return None;
        }
        let blen = buf[p] as usize;
        if blen < 2 {
            return None;
        }
        if p + blen > buf.len() {
            return None;
        }
        if buf[p + 1] == dtype {
            if after_offset.map_or(true, |a| p > a) {
                return Some(p);
            }
        }
        p += blen;
    }
    None
}

/// Finds the next class-specific interface descriptor (`USB_DT_CS_INTERFACE`)
/// with the given subtype byte, searching strictly after `after_offset`.
///
/// Returns the offset of the found descriptor within `buf`, or `None`.
pub(crate) fn find_csint_desc(buf: &[u8], after_offset: Option<usize>, subtype: u8) -> Option<usize> {
    let cs = bindings::USB_DT_CS_INTERFACE as u8;
    let mut cur = after_offset;

    loop {
        match find_desc(buf, cur, cs) {
            None => return None,
            Some(off) => {
                // Need at least 3 bytes (bLength, bDescriptorType, bDescriptorSubtype).
                if buf.len() - off >= 3 && buf[off] >= 3 && buf[off + 2] == subtype {
                    return Some(off);
                }
                cur = Some(off);
            }
        }
    }
}

//
// Speed helpers
//
/// Returns the USB speed of the device as a `usb_device_speed` enum value.
#[allow(dead_code)]
pub(crate) fn get_speed(dev: &kernel::usb::Device) -> u32 {
    dev.speed()
}

/// Returns true if the device is high-speed or faster (interval is in microframes).
#[allow(dead_code)]
pub(crate) fn is_highspeed_or_faster(dev: &kernel::usb::Device) -> bool {
    let speed = get_speed(dev);
    matches!(
        speed,
        x if x == bindings::usb_device_speed_USB_SPEED_HIGH
          || x == bindings::usb_device_speed_USB_SPEED_SUPER
          || x == bindings::usb_device_speed_USB_SPEED_SUPER_PLUS
    )
}

//
// Byte combining helpers
//
/// Combines little-endian bytes into a `u32` value (up to 4 bytes).
pub(crate) fn combine_bytes_le(bytes: &[u8]) -> u32 {
    let mut val: u32 = 0;
    for (i, &b) in bytes.iter().enumerate().take(4) {
        val |= (b as u32) << (8 * i);
    }
    val
}

//
// Host interface helpers
//
/// Looks up a `usb_host_interface *` by interface number and alternate setting index.
///
/// Uses the safe [`usb::ifnum_to_if`] abstraction to locate the interface,
/// then searches its alternate settings for the requested alternate setting number.
/// Returns a raw pointer to the matching `usb_host_interface`, or `None`.
#[allow(dead_code)]
pub(crate) fn get_host_interface(
    dev: &kernel::usb::Device,
    ifnum: u32,
    altsetting: u32,
) -> Option<*mut bindings::usb_host_interface> {
    let iface = usb::ifnum_to_if(dev, ifnum as u8)?;
    iface
        .altsettings()
        .iter()
        .find(|alt| u32::from(alt.alternate_setting()) == altsetting)
        .map(|alt| alt.as_raw())
}

/// Scan USB interfaces 0..15 for the AudioControl one.
/// Returns the reference to the found AudioControl interface alternate setting.
pub(crate) fn find_ctrl_interface(dev: &usb::Device) -> Result<&usb::HostInterface> {
    for ifnum in 0u8..16 {
        let iface = match usb::ifnum_to_if(dev, ifnum) {
            Some(p) => p,
            None => continue,
        };
        let altsetting = iface.cur_altsetting();

        // Check for Audio class.
        if altsetting.class() != usb::ch9::InterfaceClass::AUDIO {
            continue;
        }

        if altsetting.subclass() != crate::types::USB_SUBCLASS_AUDIOCONTROL {
            continue;
        }

        let extra = altsetting.extra();
        if extra.is_empty() { continue; }

        return Ok(altsetting);
    }
    Err(ENODEV)
}
