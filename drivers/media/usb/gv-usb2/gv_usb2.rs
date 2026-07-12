// SPDX-License-Identifier: GPL-2.0

//! Rust GV-USB2 driver.

use crate::driver::GvUsb2Driver;

kernel::module_usb_driver! {
    type: GvUsb2Driver,
    name: "gv_usb2",
    authors: ["Colin Braun"],
    description: "GV-USB2 Composite-USB Video Capture Device",
    license: "GPL v2",
}

mod driver;
mod regs;
