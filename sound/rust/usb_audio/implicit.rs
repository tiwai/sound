// SPDX-License-Identifier: GPL-2.0

//! Implicit feedback feedback endpoint detection.
//!
//! Corresponds to `sound/usb/implicit.c`.

// The implicit feedback path is fully implemented but not yet wired into the
// stream setup code; all items in this module are scaffolding for that work.
#![allow(dead_code)]

use kernel::usb;
use kernel::usb::ch9::EndpointDescriptor;
use crate::types::{
    AudioFormat, UAC_VERSION_2,
    QUIRK_FLAG_SKIP_IMPLICIT_FB, QUIRK_FLAG_GENERIC_IMPLICIT_FB,
    QUIRK_FLAG_PLAYBACK_FIRST,
    USB_CLASS_AUDIO, USB_SUBCLASS_AUDIOSTREAMING,
};

//
// USB endpoint / class constants
//
const USB_DIR_IN: u8 = 0x80;
const USB_CLASS_VENDOR_SPEC: u8 = 0xff;
const USB_ENDPOINT_XFERTYPE_MASK: u8 = 0x03;
const USB_ENDPOINT_XFER_ISOC: u8 = 0x01;
const USB_ENDPOINT_SYNCTYPE: u8 = 0x0c;
const USB_ENDPOINT_SYNC_ASYNC: u8 = 0x04;
const USB_ENDPOINT_USAGE_MASK: u8 = 0x30;
const USB_ENDPOINT_USAGE_IMPLICIT_FB: u8 = 0x20;

//
// Quirk lookup table
//
struct PlaybackImplicitFbQuirk {
    usb_id: u32,
    iface_class: u8,
    kind: ImplicitFbType,
}

enum ImplicitFbType {
    None,
    Generic,
    Fixed { ep_num: u8, iface: u8 },
    Both { ep_num: u8, iface: u8 },
}

static PLAYBACK_IMPLICIT_FB_QUIRKS: &[PlaybackImplicitFbQuirk] = &[
    // playback_implicit_fb_quirks - currently empty; future expansion
];

//
// Interface lookup helper using the new upstream USB abstraction
//
/// Looks up the host interface descriptor for `ifnum`:`altsetting` using the
/// upstream `usb::ifnum_to_if` API (returns `&Interface`) and then walks
/// the altsettings slice to find the matching one.
fn get_host_interface(
    dev: &usb::Device,
    ifnum: u8,
    altsetting: u8,
) -> Option<&usb::HostInterface> {
    let iface = usb::ifnum_to_if(dev, ifnum)?;
    for hi in iface.altsettings() {
        if hi.alternate_setting() == altsetting {
            return Some(hi);
        }
    }
    None
}

//
// Descriptor / endpoint helpers
//
fn is_isoc_out(epd: &EndpointDescriptor) -> bool {
    (epd.bmAttributes() & USB_ENDPOINT_XFERTYPE_MASK == USB_ENDPOINT_XFER_ISOC)
        && (epd.bEndpointAddress() & USB_DIR_IN == 0)
}

fn is_isoc_in(epd: &EndpointDescriptor) -> bool {
    (epd.bmAttributes() & USB_ENDPOINT_XFERTYPE_MASK == USB_ENDPOINT_XFER_ISOC)
        && (epd.bEndpointAddress() & USB_DIR_IN != 0)
}

/// Updates format with implicit feedback sync information.
fn add_implicit_fb_sync_ep(
    fmt: &mut AudioFormat,
    sync_ep: u8,
    sync_ep_idx: u8,
    sync_iface: u8,
    alts: Option<&usb::HostInterface>,
) -> bool {
    fmt.implicit_fb = true;
    fmt.sync_ep = sync_ep;
    fmt.sync_ep_idx = sync_ep_idx;
    fmt.sync_iface = sync_iface;

    if let Some(hi) = alts {
        fmt.sync_altsetting = hi.alternate_setting();
    } else {
        fmt.sync_altsetting = 0;
    }
    true
}

//
// Roland vendor-class helpers
//
fn roland_sanity_check_iface(alts: &usb::HostInterface) -> bool {
    alts.class().as_raw() == USB_CLASS_VENDOR_SPEC && !alts.endpoints().is_empty()
}

//
// Implicit feedback discovery logic
//

/// Checks `ifnum`:`altsetting` for a UAC2 implicit feedback source.
fn add_generic_uac2_implicit_fb(
    dev: &usb::Device,
    fmt: &mut AudioFormat,
    ifnum: u8,
    altsetting: u8,
) -> bool {
    let alts = match get_host_interface(dev, ifnum, altsetting) {
        Some(p) => p,
        None => return false,
    };
    if alts.class().as_raw() != USB_CLASS_AUDIO
        || alts.subclass() != USB_SUBCLASS_AUDIOSTREAMING
        || alts.protocol() != UAC_VERSION_2
        || alts.endpoints().is_empty()
    {
        return false;
    }
    let epd = alts.endpoints()[0].desc();
    if !is_isoc_in(epd) {
        return false;
    }
    if epd.bmAttributes() & USB_ENDPOINT_USAGE_MASK != USB_ENDPOINT_USAGE_IMPLICIT_FB {
        return false;
    }
    add_implicit_fb_sync_ep(fmt, epd.bEndpointAddress(), 0, ifnum, Some(alts))
}

/// Checks a single adjacent interface/altsetting for a generic async ISO IN EP.
fn add_generic_implicit_fb_inner(
    dev: &usb::Device,
    fmt: &mut AudioFormat,
    iface: u8,
    altset: u8,
) -> bool {
    let alts = match get_host_interface(dev, iface, altset) {
        Some(p) => p,
        None => return false,
    };
    let class = alts.class().as_raw();
    if (class != USB_CLASS_VENDOR_SPEC && class != USB_CLASS_AUDIO)
        || alts.endpoints().is_empty()
    {
        return false;
    }
    let epd = alts.endpoints()[0].desc();
    if !is_isoc_in(epd) {
        return false;
    }
    if epd.bmAttributes() & USB_ENDPOINT_SYNCTYPE != USB_ENDPOINT_SYNC_ASYNC {
        return false;
    }
    add_implicit_fb_sync_ep(fmt, epd.bEndpointAddress(), 0, iface, Some(alts))
}

/// Generic implicit feedback: looks at the adjacent interface (+/-1).
fn add_generic_implicit_fb(
    dev: &usb::Device,
    fmt: &mut AudioFormat,
    alts: &usb::HostInterface,
) -> bool {
    if fmt.ep_attr & USB_ENDPOINT_SYNCTYPE != USB_ENDPOINT_SYNC_ASYNC {
        return false;
    }
    let ifnum = alts.number();
    let altset = alts.alternate_setting();

    if ifnum < u8::MAX
        && add_generic_implicit_fb_inner(dev, fmt, ifnum + 1, altset)
    {
        return true;
    }
    if ifnum > 0 {
        add_generic_implicit_fb_inner(dev, fmt, ifnum - 1, altset)
    } else {
        false
    }
}

/// Roland vendor-class implicit feedback detection (playback side).
fn add_roland_implicit_fb(
    dev: &usb::Device,
    fmt: &mut AudioFormat,
    alts: &usb::HostInterface,
    quirk_flags: &mut u32,
) -> bool {
    if !roland_sanity_check_iface(alts) {
        return false;
    }
    let epd0 = alts.endpoints()[0].desc();
    if !is_isoc_out(epd0) {
        return false;
    }
    if epd0.bmAttributes() & USB_ENDPOINT_SYNCTYPE != USB_ENDPOINT_SYNC_ASYNC {
        return false;
    }
    let ifnum = alts.number();
    let altset = alts.alternate_setting();
    if ifnum >= u8::MAX {
        return false;
    }
    let cap_alts = match get_host_interface(dev, ifnum + 1, altset) {
        Some(p) => p,
        None => return false,
    };
    if !roland_sanity_check_iface(cap_alts) {
        return false;
    }
    let epd_cap = cap_alts.endpoints()[0].desc();
    if !is_isoc_in(epd_cap) {
        return false;
    }
    if epd_cap.bmAttributes() & USB_ENDPOINT_SYNCTYPE != USB_ENDPOINT_SYNC_ASYNC {
        return false;
    }
    *quirk_flags |= QUIRK_FLAG_PLAYBACK_FIRST;
    add_implicit_fb_sync_ep(fmt, epd_cap.bEndpointAddress(), 0, ifnum + 1, Some(cap_alts))
}

/// Pioneer devices: playback and capture share the same iface:altset.
fn is_pioneer_implicit_fb(usb_id: u32, alts: &usb::HostInterface) -> bool {
    let vendor = usb_id >> 16;
    if vendor != 0x2b73 && vendor != 0x08e4 {
        return false;
    }
    if alts.class().as_raw() != USB_CLASS_VENDOR_SPEC || alts.endpoints().len() != 2 {
        return false;
    }
    let epd0 = alts.endpoints()[0].desc();
    if !is_isoc_out(epd0) {
        return false;
    }
    if epd0.bmAttributes() & USB_ENDPOINT_SYNCTYPE != USB_ENDPOINT_SYNC_ASYNC {
        return false;
    }
    let epd1 = alts.endpoints()[1].desc();
    if !is_isoc_in(epd1) {
        return false;
    }
    let attr1 = epd1.bmAttributes();
    if attr1 & USB_ENDPOINT_SYNCTYPE != USB_ENDPOINT_SYNC_ASYNC {
        return false;
    }
    let usage = attr1 & USB_ENDPOINT_USAGE_MASK;
    usage == 0x00 || usage == USB_ENDPOINT_USAGE_IMPLICIT_FB
}

//
// Public entry point
//
/// Sets up implicit feedback sync EP information on `fmt`.
///
/// Returns `true` if an implicit feedback sync EP was configured.
pub(crate) fn parse_implicit_fb_quirk(
    dev: &usb::Device,
    usb_id: u32,
    quirk_flags: &mut u32,
    fmt: &mut AudioFormat,
    alts: &usb::HostInterface,
) -> bool {
    if *quirk_flags & QUIRK_FLAG_SKIP_IMPLICIT_FB != 0 {
        return false;
    }

    // Capture side: only FIXED quirk table (currently empty).
    if fmt.endpoint & USB_DIR_IN != 0 {
        return false;
    }

    // 1. Device quirk table.
    let iface_class = alts.class().as_raw();
    for entry in PLAYBACK_IMPLICIT_FB_QUIRKS {
        if entry.usb_id != usb_id {
            continue;
        }
        if entry.iface_class != 0 && entry.iface_class != iface_class {
            continue;
        }
        return match entry.kind {
            ImplicitFbType::None => false,
            ImplicitFbType::Generic => {
                add_generic_implicit_fb(dev, fmt, alts)
            }
            ImplicitFbType::Fixed { ep_num, iface } => {
                add_implicit_fb_sync_ep(fmt, ep_num, 0, iface, None)
            }
            ImplicitFbType::Both { ep_num: _, iface: _ } => {
                *quirk_flags |= QUIRK_FLAG_PLAYBACK_FIRST;
                add_generic_implicit_fb(dev, fmt, alts)
            }
        };
    }

    let attr = fmt.ep_attr & USB_ENDPOINT_SYNCTYPE;

    // 2. Generic UAC2 implicit feedback (adjacent iface, same altset).
    if attr == USB_ENDPOINT_SYNC_ASYNC
        && alts.class().as_raw() == USB_CLASS_AUDIO
        && alts.protocol() == UAC_VERSION_2
        && alts.endpoints().len() == 1
    {
        let ifnum = alts.number();
        let altset = alts.alternate_setting();
        if ifnum < u8::MAX
            && add_generic_uac2_implicit_fb(dev, fmt, ifnum + 1, altset)
        {
            return true;
        }
    }

    // 3. Roland / BOSS vendor-class implicit feedback.
    if usb_id >> 16 == 0x0582 {
        if add_roland_implicit_fb(dev, fmt, alts, quirk_flags) {
            return true;
        }
    }

    // 4. Pioneer devices.
    if is_pioneer_implicit_fb(usb_id, alts) {
        *quirk_flags |= QUIRK_FLAG_PLAYBACK_FIRST;
        let epd1 = alts.endpoints()[1].desc();
        let ifnum = alts.number();
        return add_implicit_fb_sync_ep(fmt, epd1.bEndpointAddress(), 1, ifnum, Some(alts));
    }

    // 5. Generic implicit feedback flag.
    if *quirk_flags & QUIRK_FLAG_GENERIC_IMPLICIT_FB != 0 {
        return add_generic_implicit_fb(dev, fmt, alts);
    }

    false
}
