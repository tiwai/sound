// SPDX-License-Identifier: GPL-2.0

//! USB audio clock and sample rate management.
//!
//! Corresponds to `sound/usb/clock.c` and the pitch control section of `pcm.c`.

use kernel::prelude::*;
use kernel::{device, time::Delta, usb};
use kernel::usb::ch9::{CtrlRequest, Direction, Recipient, RequestType, Type};
use crate::types::{AudioFormat, UAC_VERSION_1, UAC_VERSION_2};

//
// UAC1 control request codes
//
const UAC_SET_CUR: u8 = 0x01; // UAC_SET_ | UAC__CUR
const UAC_GET_CUR: u8 = 0x81; // UAC_GET_ | UAC__CUR

//
// UAC2 control request codes
//
const UAC2_CS_CUR: u8 = 0x01;
const UAC2_CS_CONTROL_SAM_FREQ: u16 = 0x01;
const UAC2_EP_CS_PITCH: u16 = 0x01;

//
// Endpoint / interface CS attribute flags
//
const UAC_EP_CS_ATTR_SAMPLE_RATE: u8   = 0x01;
const UAC_EP_CS_ATTR_PITCH_CONTROL: u8 = 0x02;

//
// Timeout for USB control messages (matches USB_CTRL_GET/SET_TIMEOUT = 5000 ms)
//
const USB_CTRL_TIMEOUT: Delta = Delta::from_millis(5000);

//
// UAC1 sample rate
//
/// Sets the sample rate on a UAC1 device via SET_CUR on the data endpoint,
/// then optionally reads it back via GET_CUR to verify.
fn set_sample_rate_v1(
    dev: &usb::Device<device::Bound>,
    quirk_flags: u32,
    sample_rate_read_error: &mut u32,
    fmt: &AudioFormat,
    rate: u32,
) -> Result<()> {
    if fmt.attributes & UAC_EP_CS_ATTR_SAMPLE_RATE == 0 {
        return Ok(());
    }

    let mut data = KBox::new(
        [
            (rate & 0xff) as u8,
            ((rate >> 8) & 0xff) as u8,
            ((rate >> 16) & 0xff) as u8,
        ],
        GFP_KERNEL,
    )?;

    // SET_CUR: write the 3-byte LE rate to the data endpoint.
    dev.control_msg(
        &CtrlRequest::new(
            RequestType::new(Direction::Out, Type::Class, Recipient::Endpoint),
            UAC_SET_CUR,
            (UAC_EP_CS_ATTR_SAMPLE_RATE as u16) << 8,
            fmt.endpoint as u16,
            0,
        ),
        Some(&mut *data),
        USB_CTRL_TIMEOUT,
    ).map_err(|e| {
        pr_err!(
            "snd_usb_audio: {}:{}: cannot set freq {} to ep {:#x}\n",
            fmt.iface, fmt.altsetting, rate, fmt.endpoint
        );
        e
    })?;

    use crate::types::QUIRK_FLAG_GET_SAMPLE_RATE;
    if quirk_flags & QUIRK_FLAG_GET_SAMPLE_RATE != 0 {
        return Ok(());
    }
    if *sample_rate_read_error > 2 {
        return Ok(());
    }

    // GET_CUR: read back what the device is actually running at.
    match dev.control_msg(
        &CtrlRequest::new(
            RequestType::new(Direction::In, Type::Class, Recipient::Endpoint),
            UAC_GET_CUR,
            (UAC_EP_CS_ATTR_SAMPLE_RATE as u16) << 8,
            fmt.endpoint as u16,
            0,
        ),
        Some(&mut *data),
        USB_CTRL_TIMEOUT,
    ) {
        Err(_) => {
            pr_info!(
                "snd_usb_audio: {}:{}: cannot get freq at ep {:#x}\n",
                fmt.iface, fmt.altsetting, fmt.endpoint
            );
            *sample_rate_read_error += 1;
            return Ok(());
        }
        Ok(_) => {}
    }

    let crate_hz = data[0] as u32 | (data[1] as u32) << 8 | (data[2] as u32) << 16;
    if crate_hz == 0 {
        pr_info!("snd_usb_audio: failed to read current rate; disabling check\n");
        *sample_rate_read_error = 3;
        return Ok(());
    }
    if crate_hz != rate {
        pr_warn!(
            "snd_usb_audio: current rate {} differs from requested {}\n",
            crate_hz, rate
        );
    }
    Ok(())
}

//
// UAC2 sample rate
//
/// Sets the sample rate on a UAC2 device via SET_CUR on the clock source entity,
/// then reads it back via GET_CUR and returns the device's reported rate.
///
/// `ctrl_intf_num` is `bInterfaceNumber` of the AudioControl interface.
fn set_sample_rate_v2(
    dev: &usb::Device<device::Bound>,
    ctrl_intf_num: u8,
    fmt: &AudioFormat,
    rate: u32,
) -> Result<i32> {
    let clock = fmt.clock as u16;
    let index = (ctrl_intf_num as u16) | (clock << 8);

    // SET_CUR: write the 4-byte LE rate to the clock source entity.
    let mut data_le = KBox::new((rate as u32).to_le_bytes(), GFP_KERNEL)?;
    dev.control_msg(
        &CtrlRequest::new(
            RequestType::new(Direction::Out, Type::Class, Recipient::Interface),
            UAC2_CS_CUR,
            UAC2_CS_CONTROL_SAM_FREQ << 8,
            index,
            0,
        ),
        Some(&mut *data_le),
        USB_CTRL_TIMEOUT,
    ).map_err(|e| {
        pr_err!(
            "snd_usb_audio: {}:{}: cannot set freq {} (v2): err {}\n",
            fmt.iface, fmt.altsetting, rate, e.to_errno()
        );
        e
    })?;

    // GET_CUR: read back what the device is actually running at.
    let mut readback = KBox::new(0u32.to_le_bytes(), GFP_KERNEL)?;
    match dev.control_msg(
        &CtrlRequest::new(
            RequestType::new(Direction::In, Type::Class, Recipient::Interface),
            UAC2_CS_CUR,
            UAC2_CS_CONTROL_SAM_FREQ << 8,
            index,
            0,
        ),
        Some(&mut *readback),
        USB_CTRL_TIMEOUT,
    ) {
        Ok(_) => Ok(u32::from_le_bytes(*readback) as i32),
        Err(_) => {
            pr_warn!(
                "snd_usb_audio: {}:{}: cannot read freq (v2)\n",
                fmt.iface, fmt.altsetting
            );
            Ok(0)
        }
    }
}

//
// Public entry points
//
/// Sets the sample rate on the USB device for the given format and rate.
///
/// For UAC1, writes to the data endpoint's class-specific attribute.
/// For UAC2, writes to the clock source entity via the AudioControl interface.
///
/// `ctrl_intf_num` is the `bInterfaceNumber` of the AudioControl interface
/// (used for UAC2 only; ignored for UAC1).
///
/// `sample_rate_read_error` tracks consecutive GET_CUR failures for UAC1;
/// once it exceeds 2, readback verification is skipped.
pub(crate) fn init_sample_rate(
    dev: &usb::Device<device::Bound>,
    ctrl_intf_num: u8,
    quirk_flags: u32,
    sample_rate_read_error: &mut u32,
    fmt: &AudioFormat,
    rate: u32,
) -> Result<i32> {
    match fmt.protocol {
        UAC_VERSION_1 => {
            set_sample_rate_v1(dev, quirk_flags, sample_rate_read_error, fmt, rate)?;
            Ok(rate as i32)
        }
        UAC_VERSION_2 => {
            set_sample_rate_v2(dev, ctrl_intf_num, fmt, rate)
        }
        _ => Ok(rate as i32), // UAC3 / unknown: no-op
    }
}

/// Enables pitch control on the data endpoint if the format supports it.
#[allow(dead_code)]
pub(crate) fn init_pitch(
    dev: &usb::Device<device::Bound>,
    fmt: &AudioFormat,
) -> Result<()> {
    if fmt.attributes & UAC_EP_CS_ATTR_PITCH_CONTROL == 0 {
        return Ok(());
    }
    pr_info!(
        "snd_usb_audio: {}:{}: enabling PITCH for EP {:#x}\n",
        fmt.iface, fmt.altsetting, fmt.endpoint
    );

    let mut data = KBox::new([1u8], GFP_KERNEL)?;
    match fmt.protocol {
        UAC_VERSION_1 => {
            dev.control_msg(
                &CtrlRequest::new(
                    RequestType::new(Direction::Out, Type::Class, Recipient::Endpoint),
                    UAC_SET_CUR,
                    (UAC_EP_CS_ATTR_PITCH_CONTROL as u16) << 8,
                    fmt.endpoint as u16,
                    0,
                ),
                Some(&mut *data),
                USB_CTRL_TIMEOUT,
            )?;
        }
        UAC_VERSION_2 => {
            dev.control_msg(
                &CtrlRequest::new(
                    RequestType::new(Direction::Out, Type::Class, Recipient::Endpoint),
                    UAC2_CS_CUR,
                    UAC2_EP_CS_PITCH << 8,
                    0,
                    0,
                ),
                Some(&mut *data),
                USB_CTRL_TIMEOUT,
            )?;
        }
        _ => {}
    }
    Ok(())
}
