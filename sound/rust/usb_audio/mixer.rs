// SPDX-License-Identifier: GPL-2.0

//! USB audio mixer control implementation.
//!
//! Implements the AudioControl interface parsing and ALSA control registration
//! for UAC1 and UAC2 devices.  Covers feature units (volume/mute) and selector
//! units (input source selection).
//!
//! Corresponds to `sound/usb/mixer.c`.

use kernel::{bindings, prelude::*, sync::Arc, time::Delta};
use kernel::usb::ch9::{CtrlRequest, Direction, Recipient, RequestType, Type};
use kernel::sound::control::{self, KControlConfig, KControlOps, ElemInfo, ElemType, ElemValue};
use kernel::sync::LockedBy;

use crate::card::{UsbAudioChip, UsbAudioChipState};
use crate::helper::{combine_bytes_le, find_csint_desc, find_desc};
use crate::types::{UAC_VERSION_1, UAC_VERSION_2, QUIRK_FLAG_IGNORE_CTL_ERROR};

//
// Constants: UAC descriptor subtypes
//
const UAC_OUTPUT_TERMINAL: u8  = 0x03;
const UAC_FEATURE_UNIT: u8     = 0x06;
const UAC_INPUT_TERMINAL: u8   = 0x02;
const UAC_SELECTOR_UNIT: u8    = 0x05;
const UAC2_CLOCK_SELECTOR: u8  = 0x0b;

const USB_DT_CS_INTERFACE: u8 = bindings::USB_DT_CS_INTERFACE as u8;

//
// Constants: UAC1/UAC2 Feature Unit control selectors
//
const UAC_FU_MUTE: u8           = 0x01;
const UAC_FU_VOLUME: u8         = 0x02;
const UAC_FU_BASS: u8           = 0x03;
const UAC_FU_MID: u8            = 0x04;
const UAC_FU_TREBLE: u8         = 0x05;
const UAC_FU_GRAPHIC_EQ: u8    = 0x06; // not implemented
const UAC_FU_AUTO_GAIN: u8      = 0x07;
const UAC_FU_DELAY: u8          = 0x08;
const UAC_FU_BASS_BOOST: u8     = 0x09;
const UAC_FU_LOUDNESS: u8       = 0x0a;
const UAC2_FU_INPUT_GAIN: u8    = 0x0b;
const UAC2_FU_INPUT_GAIN_PAD: u8 = 0x0c;
const UAC2_FU_PHASE_INVERTER: u8 = 0x0d;

// UAC2 Selector/Clock controls
const UAC2_CX_CLOCK_SELECTOR: u8 = 0x01;
const UAC2_SU_SELECTOR: u8       = 0x01;

//
// Constants: UAC request codes
//
const UAC_GET_CUR: u8  = 0x81;
const UAC_GET_MIN: u8  = 0x82;
const UAC_GET_MAX: u8  = 0x83;
const UAC_GET_RES: u8  = 0x84;
const UAC_SET_CUR: u8  = 0x01;
const UAC2_CS_CUR: u8  = 0x01;
const UAC2_CS_RANGE: u8 = 0x02;

//
// Constants: mixer value types
//
const USB_MIXER_BOOLEAN: i32     = 0;
const USB_MIXER_INV_BOOLEAN: i32 = 1;
const USB_MIXER_S8: i32          = 2;
const USB_MIXER_U8: i32          = 3;
const USB_MIXER_S16: i32         = 4;
const USB_MIXER_U16: i32         = 5;
const USB_MIXER_S32: i32         = 6;
const USB_MIXER_U32: i32         = 7;

const MAX_CHANNELS: usize = 64;

// Name buffer capacity - SNDRV_CTL_ELEM_ID_NAME_MAXLEN
const CTL_NAME_LEN: usize = bindings::SNDRV_CTL_ELEM_ID_NAME_MAXLEN as usize;

// Default timeout for USB audio control messages (5 seconds, matching C driver).
const USB_CTRL_TIMEOUT: Delta = Delta::from_millis(5000);

//
// Feature control info table
//
struct FeatureControlInfo {
    control: u8,
    name: &'static str,
    val_type: i32,
    val_type_uac2: i32, // -1 = same as val_type
}

static AUDIO_FEATURE_INFO: &[FeatureControlInfo] = &[
    FeatureControlInfo { control: UAC_FU_MUTE,             name: "Mute",                   val_type: USB_MIXER_INV_BOOLEAN, val_type_uac2: -1 },
    FeatureControlInfo { control: UAC_FU_VOLUME,           name: "Volume",                 val_type: USB_MIXER_S16,         val_type_uac2: -1 },
    FeatureControlInfo { control: UAC_FU_BASS,             name: "Tone Control - Bass",    val_type: USB_MIXER_S8,          val_type_uac2: -1 },
    FeatureControlInfo { control: UAC_FU_MID,              name: "Tone Control - Mid",     val_type: USB_MIXER_S8,          val_type_uac2: -1 },
    FeatureControlInfo { control: UAC_FU_TREBLE,           name: "Tone Control - Treble",  val_type: USB_MIXER_S8,          val_type_uac2: -1 },
    FeatureControlInfo { control: UAC_FU_GRAPHIC_EQ,      name: "Graphic Equalizer",      val_type: USB_MIXER_S8,          val_type_uac2: -1 },
    FeatureControlInfo { control: UAC_FU_AUTO_GAIN,        name: "Auto Gain Control",      val_type: USB_MIXER_BOOLEAN,     val_type_uac2: -1 },
    FeatureControlInfo { control: UAC_FU_DELAY,            name: "Delay Control",          val_type: USB_MIXER_U16,         val_type_uac2: USB_MIXER_U32 },
    FeatureControlInfo { control: UAC_FU_BASS_BOOST,       name: "Bass Boost",             val_type: USB_MIXER_BOOLEAN,     val_type_uac2: -1 },
    FeatureControlInfo { control: UAC_FU_LOUDNESS,         name: "Loudness",               val_type: USB_MIXER_BOOLEAN,     val_type_uac2: -1 },
    FeatureControlInfo { control: UAC2_FU_INPUT_GAIN,      name: "Input Gain Control",     val_type: USB_MIXER_S16,         val_type_uac2: -1 },
    FeatureControlInfo { control: UAC2_FU_INPUT_GAIN_PAD,  name: "Input Gain Pad Control", val_type: USB_MIXER_S16,         val_type_uac2: -1 },
    FeatureControlInfo { control: UAC2_FU_PHASE_INVERTER,  name: "Phase Inverter Control", val_type: USB_MIXER_BOOLEAN,     val_type_uac2: -1 },
];

//
// Terminal type name lookup
//
fn term_name_from_type(t: u16) -> Option<&'static str> {
    match t {
        0x0100..=0x01ff => Some("PCM"),
        0x0200..=0x02ff => Some("Mic"),
        0x0300          => Some("Output"),
        0x0301          => Some("Speaker"),
        0x0302          => Some("Headphone"),
        0x0303          => Some("HMD Audio"),
        0x0304          => Some("Desktop Speaker"),
        0x0305          => Some("Room Speaker"),
        0x0306          => Some("Com Speaker"),
        0x0307          => Some("LFE"),
        0x0400..=0x04ff => Some("Headset"),
        0x0500..=0x05ff => Some("Phone"),
        0x0601          => Some("Analog In"),
        0x0602          => Some("Digital In"),
        0x0603          => Some("Line"),
        0x0605          => Some("IEC958 In"),
        0x0703          => Some("CD"),
        0x0704          => Some("DAT"),
        _               => None,
    }
}

//
// Input terminal descriptor info (for naming controls)
//
struct AudioTerm {
    term_type: u16,
    name_idx: u8,
}

//
// Mixer element info (per-control state, heap-allocated, owned by kcontrol)
//
struct MixerElemInfo {
    chip: Arc<UsbAudioChip>,
    ctrl_intf_num: u8,
    protocol: u8,
    unit_id: u8,
    control: u8,
    val_type: i32,
    cmask: u64,
    channels: i32,
    ch_readonly: u32,
    master_readonly: i32,
    min: i32,
    max: i32,
    res: i32,
    max_exposed: i32,
    db_min: i32,
    db_max: i32,
    initialized: bool,
    ignore_ctl_error: bool,
    cached: u64,
    cache_val: [i32; MAX_CHANNELS + 1],
}

impl MixerElemInfo {
    fn new(chip: Arc<UsbAudioChip>, ctrl_intf_num: u8, protocol: u8, unit_id: u8) -> Self {
        let ignore = (chip.quirk_flags.load(core::sync::atomic::Ordering::Relaxed) & QUIRK_FLAG_IGNORE_CTL_ERROR) != 0;
        Self {
            chip,
            ctrl_intf_num,
            protocol,
            unit_id,
            control: 0,
            val_type: USB_MIXER_S16,
            cmask: 0,
            channels: 0,
            ch_readonly: 0,
            master_readonly: 0,
            min: 0,
            max: 0,
            res: 1,
            max_exposed: 0,
            db_min: 0,
            db_max: 0,
            initialized: false,
            ignore_ctl_error: ignore,
            cached: 0,
            cache_val: [0; MAX_CHANNELS + 1],
        }
    }

    fn val_size_uac1(&self) -> usize {
        if self.val_type >= USB_MIXER_S16 { 2 } else { 1 }
    }

    fn val_size_uac2(&self) -> usize {
        match self.val_type {
            USB_MIXER_S32 | USB_MIXER_U32 => 4,
            USB_MIXER_S16 | USB_MIXER_U16 => 2,
            _ => 1,
        }
    }

    fn convert_signed(&self, val: i32) -> i32 {
        match self.val_type {
            USB_MIXER_BOOLEAN     => (val != 0) as i32,
            USB_MIXER_INV_BOOLEAN => (val == 0) as i32,
            USB_MIXER_U8          => val & 0xff,
            USB_MIXER_S8 => {
                let v = val & 0xff;
                if v >= 0x80 { v - 0x100 } else { v }
            }
            USB_MIXER_U16 => val & 0xffff,
            USB_MIXER_S16 => {
                let v = val & 0xffff;
                if v >= 0x8000 { v - 0x10000 } else { v }
            }
            _ => val,
        }
    }

    fn convert_bytes(&self, val: i32) -> i32 {
        match self.val_type {
            USB_MIXER_BOOLEAN | USB_MIXER_INV_BOOLEAN => (val != 0) as i32,
            USB_MIXER_S8 | USB_MIXER_U8   => val & 0xff,
            USB_MIXER_S16 | USB_MIXER_U16 => val & 0xffff,
            _ => val,
        }
    }

    fn get_relative_value(&self, val: i32) -> i32 {
        let res = if self.res == 0 { 1 } else { self.res };
        if val < self.min {
            0
        } else if val >= self.max {
            (self.max - self.min + res - 1) / res
        } else {
            (val - self.min) / res
        }
    }

    fn get_abs_value(&self, val: i32) -> i32 {
        if val < 0 { return self.min; }
        let res = if self.res == 0 { 1 } else { self.res };
        (val * res + self.min).min(self.max)
    }

    fn ctrl_index(&self) -> u16 {
        (self.ctrl_intf_num as u16) | ((self.unit_id as u16) << 8)
    }

    /// UAC1 GET request.
    fn get_ctl_v1(&self, request: u8, validx: u16) -> Result<i32> {
        let dev = self.chip.bound_device();
        let val_len = self.val_size_uac1();
        let mut buf = KBox::new([0u8; 2], GFP_KERNEL)?;
        let setup = CtrlRequest::new(
            RequestType::new(Direction::In, Type::Class, Recipient::Interface),
            request,
            validx,
            self.ctrl_index(),
            val_len as u16,
        );

        for _ in 0..10 {
            let ret = dev.control_msg(&setup, Some(&mut buf[..val_len]), USB_CTRL_TIMEOUT);
            match ret {
                Ok(len) if len == val_len as i32 => {
                    let raw = combine_bytes_le(&buf[..val_len]) as i32;
                    return Ok(self.convert_signed(raw));
                }
                Err(e) if e.to_errno() == -(bindings::ETIMEDOUT as i32) => {
                    return Err(ETIMEDOUT);
                }
                _ => {}
            }
        }
        Err(EINVAL)
    }

    /// UAC2 GET request.
    fn get_ctl_v2(&self, request: u8, validx: u16) -> Result<i32> {
        let dev = self.chip.bound_device();
        let val_size = self.val_size_uac2();
        let range_size = 2 + 3 * val_size;
        let buf_size = if request == UAC_GET_CUR { val_size } else { range_size };
        let mut buf = KBox::new([0u8; 14], GFP_KERNEL)?;
        let b_request = if request == UAC_GET_CUR { UAC2_CS_CUR } else { UAC2_CS_RANGE };
        let setup = CtrlRequest::new(
            RequestType::new(Direction::In, Type::Class, Recipient::Interface),
            b_request,
            validx,
            self.ctrl_index(),
            buf_size as u16,
        );

        dev.control_msg(&setup, Some(&mut buf[..buf_size]), USB_CTRL_TIMEOUT)?;

        let val_bytes = match request {
            UAC_GET_CUR => &buf[..val_size],
            UAC_GET_MIN => &buf[2..2 + val_size],
            UAC_GET_MAX => &buf[2 + val_size..2 + 2 * val_size],
            UAC_GET_RES => &buf[2 + 2 * val_size..2 + 3 * val_size],
            _           => return Err(EINVAL),
        };
        let raw = combine_bytes_le(val_bytes) as i32;
        Ok(self.convert_signed(raw))
    }

    fn get_ctl(&self, request: u8, validx: u16) -> Result<i32> {
        if self.protocol == UAC_VERSION_1 {
            self.get_ctl_v1(request, validx)
        } else {
            self.get_ctl_v2(request, validx)
        }
    }

    /// Read current value for one channel (with value cache).
    /// channel = 0 -> master, 1..N -> individual channels.
    fn get_cur_mix_value(&mut self, channel: usize, idx: usize) -> Result<i32> {
        let bit = 1u64 << channel;
        if self.cached & bit != 0 {
            return Ok(self.cache_val[idx]);
        }
        let validx = ((self.control as u16) << 8) | (channel as u16);
        let val = self.get_ctl(UAC_GET_CUR, validx)?;
        self.cached |= bit;
        self.cache_val[idx] = val;
        Ok(val)
    }

    /// Write a value to one channel and update cache.
    fn set_cur_mix_value(&mut self, channel: usize, idx: usize, value: i32) -> Result<()> {
        if channel == 0 {
            if self.master_readonly != 0 { return Ok(()); }
        } else if self.ch_readonly & (1 << (channel - 1)) != 0 {
            return Ok(());
        }
        let validx = ((self.control as u16) << 8) | (channel as u16);
        self.set_ctl_value(UAC_SET_CUR, validx, value)?;
        self.cached |= 1u64 << channel;
        self.cache_val[idx] = value;
        Ok(())
    }

    /// Low-level USB SET request.
    fn set_ctl_value(&self, request: u8, validx: u16, value_set: i32) -> Result<()> {
        let dev = self.chip.bound_device();
        let val_set = self.convert_bytes(value_set);
        let mut buf = KBox::new([
            (val_set & 0xff) as u8,
            ((val_set >> 8) & 0xff) as u8,
            ((val_set >> 16) & 0xff) as u8,
            ((val_set >> 24) & 0xff) as u8,
        ], GFP_KERNEL)?;
        let (val_len, b_request) = if self.protocol == UAC_VERSION_1 {
            (self.val_size_uac1(), request)
        } else {
            (self.val_size_uac2(), UAC2_CS_CUR)
        };
        let setup = CtrlRequest::new(
            RequestType::new(Direction::Out, Type::Class, Recipient::Interface),
            b_request,
            validx,
            self.ctrl_index(),
            val_len as u16,
        );

        for _ in 0..10 {
            let ret = dev.control_msg(&setup, Some(&mut buf[..val_len]), USB_CTRL_TIMEOUT);
            if ret.is_ok() { return Ok(()); }
            if let Err(e) = ret {
                if e.to_errno() == -(bindings::ETIMEDOUT as i32) {
                    return Err(ETIMEDOUT);
                }
            }
        }
        Err(EINVAL)
    }

    /// Query device for min/max/res and pre-cache current value.
    fn init_min_max(&mut self) -> Result<()> {
        if self.val_type == USB_MIXER_BOOLEAN || self.val_type == USB_MIXER_INV_BOOLEAN {
            self.min = 0;
            self.max = 1;
            self.res = 1;
            self.max_exposed = 1;
            self.initialized = true;
            return Ok(());
        }

        let minchn: u16 = if self.cmask != 0 {
            let mut ch = 0u16;
            for i in 0..MAX_CHANNELS {
                if self.cmask & (1u64 << i) != 0 {
                    ch = i as u16 + 1;
                    break;
                }
            }
            ch
        } else {
            0
        };

        let vmax = ((self.control as u16) << 8) | minchn;
        let max_v = self.get_ctl(UAC_GET_MAX, vmax).unwrap_or(1);
        let min_v = self.get_ctl(UAC_GET_MIN, vmax).unwrap_or(0);
        let res_v = self.get_ctl(UAC_GET_RES, vmax).unwrap_or(1);

        self.max = max_v;
        self.min = min_v;
        self.res = if res_v == 0 { 1 } else { res_v };

        // dB conversion: USB uses 1/256 dB, ALSA uses 1/100 dB.
        self.db_min = (self.convert_signed(self.min) * 100) / 256;
        self.db_max = (self.convert_signed(self.max) * 100) / 256;

        if self.db_min > self.db_max {
            if self.db_min < 0 { self.db_max = 0; } else { self.db_min = 0; }
        }
        if self.db_max <= -9600 {
            self.db_min = 0;
            self.db_max = 0;
        }

        if self.max <= self.min {
            return Err(EINVAL);
        }

        self.max_exposed = (self.max - self.min + self.res - 1) / self.res;
        self.initialized = true;

        // Pre-cache current values.
        if self.cmask == 0 {
            let _ = self.get_cur_mix_value(0, 0);
        } else {
            let mut idx = 0usize;
            for ch in 0..MAX_CHANNELS {
                if self.cmask & (1u64 << ch) != 0 {
                    let _ = self.get_cur_mix_value(ch + 1, idx);
                    idx += 1;
                }
            }
        }

        Ok(())
    }
}

//
// ALSA control implementations using KControlOps
//
struct FeatureCtl {
    chip: Arc<UsbAudioChip>,
    cval: LockedBy<MixerElemInfo, UsbAudioChipState>,
}

impl KControlOps for FeatureCtl {
    fn info(&self, info: &mut ElemInfo) -> Result {
        let guard = self.chip.mutex.lock();
        let cval = self.cval.access(&*guard);
        let is_bool = cval.val_type == USB_MIXER_BOOLEAN
            || cval.val_type == USB_MIXER_INV_BOOLEAN;
        let elem_type = if is_bool {
            ElemType::Boolean
        } else {
            ElemType::Integer
        };
        info.set_type_count(elem_type, cval.channels as u32);
        if !is_bool {
            info.set_integer_range(0, cval.max_exposed as c_long, 0);
        }
        Ok(())
    }

    fn get(&self, value: &mut ElemValue) -> Result {
        let mut guard = self.chip.mutex.lock();
        let cval = self.cval.access_mut(&mut *guard);
        if cval.cmask != 0 {
            let mut cnt = 0usize;
            for ch in 0..MAX_CHANNELS {
                if cval.cmask & (1u64 << ch) == 0 { continue; }
                let val = match cval.get_cur_mix_value(ch + 1, cnt) {
                    Ok(v) => v,
                    Err(e) => {
                        if cval.ignore_ctl_error { return Ok(()); }
                        return Err(e);
                    }
                };
                let rval = cval.get_relative_value(val);
                value.set_integer(cnt, rval as c_long);
                cnt += 1;
            }
        } else {
            let val = match cval.get_cur_mix_value(0, 0) {
                Ok(v) => v,
                Err(e) => {
                    if cval.ignore_ctl_error { return Ok(()); }
                    return Err(e);
                }
            };
            let rval = cval.get_relative_value(val);
            value.set_integer(0, rval as c_long);
        }
        Ok(())
    }

    fn put(&self, value: &ElemValue) -> Result<bool> {
        let mut guard = self.chip.mutex.lock();
        let cval = self.cval.access_mut(&mut *guard);
        let mut changed = false;

        if cval.cmask != 0 {
            let mut cnt = 0usize;
            for ch in 0..MAX_CHANNELS {
                if cval.cmask & (1u64 << ch) == 0 { continue; }
                let oval = match cval.get_cur_mix_value(ch + 1, cnt) {
                    Ok(v) => v,
                    Err(e) => {
                        if cval.ignore_ctl_error { return Ok(false); }
                        return Err(e);
                    }
                };
                let uval = value.integer(cnt) as i32;
                if uval < 0 || uval > cval.max_exposed {
                    return Err(EINVAL);
                }
                let nval = cval.get_abs_value(uval);
                if oval != nval {
                    if let Err(e) = cval.set_cur_mix_value(ch + 1, cnt, nval) {
                        if cval.ignore_ctl_error { return Ok(false); }
                        return Err(e);
                    }
                    changed = true;
                }
                cnt += 1;
            }
        } else {
            let oval = match cval.get_cur_mix_value(0, 0) {
                Ok(v) => v,
                Err(e) => {
                    if cval.ignore_ctl_error { return Ok(false); }
                    return Err(e);
                }
            };
            let uval = value.integer(0) as i32;
            if uval < 0 || uval > cval.max_exposed {
                return Err(EINVAL);
            }
            let nval = cval.get_abs_value(uval);
            if oval != nval {
                if let Err(e) = cval.set_cur_mix_value(0, 0, nval) {
                    if cval.ignore_ctl_error { return Ok(false); }
                    return Err(e);
                }
                changed = true;
            }
        }
        Ok(changed)
    }
}

struct SelectorCtl {
    chip: Arc<UsbAudioChip>,
    cval: LockedBy<MixerElemInfo, UsbAudioChipState>,
    num_items: usize,
    item_names: KVec<KVec<u8>>,
}

impl KControlOps for SelectorCtl {
    fn info(&self, info: &mut ElemInfo) -> Result {
        let req_item = info.enumerated_item() as usize;
        let item_idx = req_item.min(self.num_items.saturating_sub(1));

        info.set_type_count(ElemType::Enumerated, 1);
        info.set_enumerated_items(self.num_items as u32);

        if item_idx < self.item_names.len() {
            let name_bytes = &self.item_names[item_idx];
            let name_cstr = CStr::from_bytes_with_nul(name_bytes).map_err(|_| EINVAL)?;
            info.set_enumerated_name(name_cstr);
        }
        Ok(())
    }

    fn get(&self, value: &mut ElemValue) -> Result {
        let mut guard = self.chip.mutex.lock();
        let cval = self.cval.access_mut(&mut *guard);
        let validx = (cval.control as u16) << 8;
        let val = match cval.get_ctl(UAC_GET_CUR, validx) {
            Ok(v) => v,
            Err(e) => {
                if cval.ignore_ctl_error { return Ok(()); }
                return Err(e);
            }
        };
        let idx = (val - 1).max(0).min(self.num_items as i32 - 1) as u32;
        value.set_enumerated(0, idx);
        Ok(())
    }

    fn put(&self, value: &ElemValue) -> Result<bool> {
        let mut guard = self.chip.mutex.lock();
        let cval = self.cval.access_mut(&mut *guard);
        let new_idx = value.enumerated(0) as i32;
        if new_idx < 0 || new_idx >= self.num_items as i32 {
            return Err(EINVAL);
        }
        let validx = (cval.control as u16) << 8;
        let oval = match cval.get_ctl(UAC_GET_CUR, validx) {
            Ok(v) => v,
            Err(e) => {
                if cval.ignore_ctl_error { return Ok(false); }
                return Err(e);
            }
        };
        let new_val = new_idx + 1; // 1-based on wire
        if oval == new_val { return Ok(false); }
        if let Err(e) = cval.set_ctl_value(UAC_SET_CUR, validx, new_val) {
            if cval.ignore_ctl_error { return Ok(false); }
            return Err(e);
        }
        Ok(true)
    }
}

//
// Mixer build context (equivalent to C's struct mixer_build)
//
struct MixerBuild<'a> {
    chip: &'a Arc<UsbAudioChip>,
    card: &'a kernel::sound::card::Card,
    ctrl_intf_num: u8,
    protocol: u8,
    buf: &'a [u8],
    unitbitmap: [u64; 4], // 256-bit visited set (MAX_ID_ELEMS = 256)
    oterm: AudioTerm,
}

impl<'a> MixerBuild<'a> {
    fn new(
        chip: &'a Arc<UsbAudioChip>,
        card: &'a kernel::sound::card::Card,
        ctrl_intf_num: u8,
        protocol: u8,
        buf: &'a [u8],
    ) -> Self {
        Self {
            chip,
            card,
            ctrl_intf_num,
            protocol,
            buf,
            unitbitmap: [0u64; 4],
            oterm: AudioTerm { term_type: 0, name_idx: 0 },
        }
    }

    /// Mark a unit as visited.  Returns true if it was already visited.
    fn test_and_set_unit(&mut self, id: u8) -> bool {
        let word = (id as usize) / 64;
        let bit  = 1u64 << (id as usize % 64);
        if self.unitbitmap[word] & bit != 0 { return true; }
        self.unitbitmap[word] |= bit;
        false
    }

    fn new_elem(&self, unit_id: u8) -> MixerElemInfo {
        MixerElemInfo::new(
            Arc::clone(self.chip),
            self.ctrl_intf_num,
            self.protocol,
            unit_id,
        )
    }

    /// Find the CS_INTERFACE descriptor for a unit with the given bUnitID.
    fn find_unit(&self, unit_id: u8) -> Option<&[u8]> {
        let mut cur: Option<usize> = None;
        loop {
            let off = find_desc(self.buf, cur, USB_DT_CS_INTERFACE)?;
            let d = &self.buf[off..];
            // CS_INTERFACE descriptors have bLength, bDescriptorType, bDescriptorSubtype,
            // bUnitID at offset 3.
            if d.len() >= 4 && d[0] >= 4 && d[3] == unit_id {
                return Some(d);
            }
            cur = Some(off);
        }
    }

    fn get_term_name_bytes(&self, iterm: &AudioTerm, buf: &mut [u8]) -> usize {
        if let Ok(len) = self.chip.dev.string(iterm.name_idx, buf) {
            if len > 0 { return len; }
        }
        if let Some(s) = term_name_from_type(iterm.term_type) {
            let b = s.as_bytes();
            let n = b.len().min(buf.len().saturating_sub(1));
            buf[..n].copy_from_slice(&b[..n]);
            if n < buf.len() { buf[n] = 0; }
            return n;
        }
        0
    }

    /// Build a null-terminated control name into `name_buf[..CTL_NAME_LEN]`.
    fn build_feature_name(
        &self,
        iterm: &AudioTerm,
        control: u8,
        fi_name: &str,
        name_buf: &mut [u8; CTL_NAME_LEN],
    ) {
        let mut len = self.get_term_name_bytes(iterm, name_buf);
        if len == 0 {
            len = self.get_term_name_bytes(&self.oterm, name_buf);
        }
        if len == 0 {
            let s = b"Feature";
            name_buf[..s.len()].copy_from_slice(s);
            len = s.len();
        }

        // Append direction suffix for mute/volume.
        if control == UAC_FU_MUTE || control == UAC_FU_VOLUME {
            let dir: &[u8] = if (self.oterm.term_type & 0xff00) == 0x0100 {
                b" Capture"
            } else {
                b" Playback"
            };
            let add = dir.len().min(name_buf.len().saturating_sub(1).saturating_sub(len));
            name_buf[len..len + add].copy_from_slice(&dir[..add]);
            len += add;
        }

        // Control-type suffix (e.g. " Switch", " Volume", " Tone Control - Bass").
        let sfx: &[u8] = match control {
            UAC_FU_MUTE   => b" Switch",
            UAC_FU_VOLUME => b" Volume",
            _ if !fi_name.is_empty() => {
                // Prepend space before feature-info name.
                let space = b" ";
                let add = space.len().min(name_buf.len().saturating_sub(1).saturating_sub(len));
                name_buf[len..len + add].copy_from_slice(&space[..add]);
                len += add;
                fi_name.as_bytes()
            }
            _ => b"",
        };
        let add = sfx.len().min(name_buf.len().saturating_sub(1).saturating_sub(len));
        name_buf[len..len + add].copy_from_slice(&sfx[..add]);
        len += add;
        name_buf[len] = 0;
    }

    // Register a feature kcontrol
    fn add_feature_ctl(
        &mut self,
        unit_id: u8,
        control: u8,
        cmask: u64,
        fi: &FeatureControlInfo,
        iterm: &AudioTerm,
        readonly_mask: u32,
    ) -> Result<()> {
        if control == UAC_FU_GRAPHIC_EQ { return Ok(()); }

        let mut cval = self.new_elem(unit_id);
        cval.control  = control;
        cval.cmask    = cmask;
        cval.val_type = if self.protocol == UAC_VERSION_1 || fi.val_type_uac2 < 0 {
            fi.val_type
        } else {
            fi.val_type_uac2
        };

        if cmask == 0 {
            cval.channels       = 1;
            cval.master_readonly = readonly_mask as i32;
        } else {
            cval.channels    = (0..MAX_CHANNELS).filter(|&i| cmask & (1u64 << i) != 0).count() as i32;
            cval.ch_readonly = readonly_mask;
        }

        // Skip bogus ranges.
        if let Err(_) = cval.init_min_max() {
            return Ok(());
        }

        // Build name.
        let mut name_buf = [0u8; CTL_NAME_LEN];
        self.build_feature_name(iterm, control, fi.name, &mut name_buf);

        // Determine access flags.
        let access = if readonly_mask != 0 && cmask == 0 {
            control::access::READ
        } else {
            control::access::READWRITE
        };

        let name_nul = name_buf.iter().position(|&b| b == 0).unwrap_or(name_buf.len());
        let name_cstr = CStr::from_bytes_with_nul(&name_buf[..=name_nul]).map_err(|_| EINVAL)?;

        let ops = FeatureCtl {
            chip: Arc::clone(self.chip),
            cval: LockedBy::new(&self.chip.mutex, cval),
        };

        self.card.add_kcontrol(
            KControlConfig {
                access,
                bump_on_collision: true,
                ..KControlConfig::new(control::ElemIface::Mixer, name_cstr)
            },
            ops,
        )?;

        Ok(())
    }

    // Lookup input terminal info for naming
    fn check_input_term(&self, source_id: u8) -> Option<AudioTerm> {
        let desc = self.find_unit(source_id)?;
        if desc.len() < 3 { return None; }
        let subtype = desc[2];

        if subtype == UAC_INPUT_TERMINAL {
            let (ttype, name_idx) = if self.protocol == UAC_VERSION_1 && desc.len() >= 12 {
                (u16::from_le_bytes([desc[4], desc[5]]), desc[11])
            } else if self.protocol == UAC_VERSION_2 && desc.len() >= 16 {
                (u16::from_le_bytes([desc[4], desc[5]]), desc[15])
            } else {
                return None;
            };
            return Some(AudioTerm { term_type: ttype, name_idx });
        }

        // Recurse through feature/selector/mixer units.
        let next = match subtype {
            s if s == UAC_FEATURE_UNIT && desc.len() >= 5 => desc[4],
            s if (s == UAC_SELECTOR_UNIT || s == UAC2_CLOCK_SELECTOR) && desc.len() >= 6 => desc[5],
            _ => return None,
        };
        self.check_input_term(next)
    }

    // Unit parse dispatcher
    fn parse_unit(&mut self, unit_id: u8) -> Result<()> {
        if self.test_and_set_unit(unit_id) { return Ok(()); }

        let desc_data: &[u8] = match self.find_unit(unit_id) {
            Some(d) if d.len() >= 3 => d,
            _ => return Ok(()),
        };

        // We need to copy relevant bytes out because we can't hold a reference
        // into self.buf while also calling &mut self methods.
        let subtype = desc_data[2];
        let desc_copy: KVec<u8> = {
            let mut v = KVec::new();
            v.extend_from_slice(desc_data, GFP_KERNEL)
                .map_err(|_| ENOMEM)?;
            v
        };
        let desc = &desc_copy[..];

        match subtype {
            UAC_INPUT_TERMINAL => Ok(()),
            UAC_FEATURE_UNIT   => self.parse_feature_unit(unit_id, desc),
            s if s == UAC_SELECTOR_UNIT || s == UAC2_CLOCK_SELECTOR => {
                self.parse_selector_unit(unit_id, desc)
            }
            // Mixer unit: just recurse into sources, don't add controls here.
            0x04 => self.parse_mixer_sources(desc),
            _ => Ok(()),
        }
    }

    fn parse_mixer_sources(&mut self, desc: &[u8]) -> Result<()> {
        if desc.len() < 5 { return Ok(()); }
        let n_pins = desc[4] as usize;
        for i in 0..n_pins {
            if 5 + i < desc.len() {
                let _ = self.parse_unit(desc[5 + i]);
            }
        }
        Ok(())
    }

    // Feature unit
    fn parse_feature_unit(&mut self, unit_id: u8, desc: &[u8]) -> Result<()> {
        // Recurse into the source unit.
        if desc.len() >= 5 {
            let _ = self.parse_unit(desc[4]);
        }

        let iterm = if desc.len() >= 5 {
            self.check_input_term(desc[4]).unwrap_or(AudioTerm { term_type: 0, name_idx: 0 })
        } else {
            AudioTerm { term_type: 0, name_idx: 0 }
        };

        // Decode bControlSize and channel count.
        // UAC1: bLength = 7 + csize*(1+n_ch) -> n_ch = (bLength-7)/csize - 1
        // UAC2: bLength = 6 + 4*(1+n_ch)    -> n_ch = (bLength-6)/4    - 1
        let (csize, channels, bma_offset): (usize, usize, usize) = if self.protocol == UAC_VERSION_1 {
            if desc.len() < 7 { return Ok(()); }
            let cs = desc[5] as usize;
            if cs == 0 { return Ok(()); }
            let ch = ((desc[0] as usize).saturating_sub(7) / cs).saturating_sub(1);
            (cs, ch, 6)
        } else {
            if desc.len() < 6 { return Ok(()); }
            let ch = ((desc[0] as usize).saturating_sub(6) / 4).saturating_sub(1);
            (4, ch, 5)
        };

        if channels > MAX_CHANNELS { return Ok(()); }

        let master_bits: u32 = if bma_offset + csize <= desc.len() {
            combine_bytes_le(&desc[bma_offset..bma_offset + csize])
        } else {
            0
        };

        if self.protocol == UAC_VERSION_1 {
            for fi in AUDIO_FEATURE_INFO {
                let i = (fi.control - 1) as usize;
                let mut ch_bits = 0u64;
                for j in 0..channels {
                    let off = bma_offset + csize * (j + 1);
                    if off + csize <= desc.len() {
                        let mask = combine_bytes_le(&desc[off..off + csize]);
                        if mask & (1u32 << i) != 0 {
                            ch_bits |= 1u64 << j;
                        }
                    }
                }
                if ch_bits != 0 {
                    let _ = self.add_feature_ctl(unit_id, fi.control, ch_bits, fi, &iterm, 0);
                }
                if master_bits & (1u32 << i) != 0 {
                    let _ = self.add_feature_ctl(unit_id, fi.control, 0, fi, &iterm, 0);
                }
            }
        } else {
            // UAC2: bits are paired (readable | writeable).
            for fi in AUDIO_FEATURE_INFO {
                let read_bit  = (fi.control as u32 - 1) * 2;
                let write_bit = read_bit + 1;

                let m_readable  = master_bits & (1u32 << read_bit)  != 0;
                let m_writeable = master_bits & (1u32 << write_bit) != 0;

                let mut ch_bits = 0u64;
                let mut ch_ro   = 0u32;
                for j in 0..channels {
                    let off = bma_offset + 4 * (j + 1);
                    if off + 4 <= desc.len() {
                        let mask = combine_bytes_le(&desc[off..off + 4]);
                        if mask & (1u32 << read_bit) != 0 {
                            ch_bits |= 1u64 << j;
                            if mask & (1u32 << write_bit) == 0 { ch_ro |= 1u32 << j; }
                        }
                    }
                }

                if ch_bits != 0 {
                    let _ = self.add_feature_ctl(unit_id, fi.control, ch_bits, fi, &iterm, ch_ro);
                }
                if m_readable {
                    let mro = if m_writeable { 0 } else { 1 };
                    let _ = self.add_feature_ctl(unit_id, fi.control, 0, fi, &iterm, mro);
                }
            }
        }

        Ok(())
    }

    // Selector / clock selector unit
    fn parse_selector_unit(&mut self, unit_id: u8, desc: &[u8]) -> Result<()> {
        if desc.len() < 5 { return Ok(()); }
        let n_pins = desc[4] as usize;

        // Recurse into all source units.
        for i in 0..n_pins {
            if 5 + i < desc.len() {
                let _ = self.parse_unit(desc[5 + i]);
            }
        }

        if n_pins < 2 { return Ok(()); } // single source - no selector needed

        // Build item names.
        let mut item_names: KVec<KVec<u8>> = KVec::new();
        for i in 0..n_pins {
            let src = if 5 + i < desc.len() { desc[5 + i] } else { 0 };
            let mut name: KVec<u8> = KVec::new();

            let mut written = false;
            if let Some(iterm) = self.check_input_term(src) {
                let mut tmp = [0u8; 64];
                let len = self.get_term_name_bytes(&iterm, &mut tmp);
                if len > 0 {
                    name.extend_from_slice(&tmp[..len], GFP_KERNEL)?;
                    name.push(0, GFP_KERNEL)?;
                    written = true;
                }
            }
            if !written {
                // Fallback: "Input N" written as bytes.
                write_input_name(&mut name, i + 1)?;
            }
            item_names.push(name, GFP_KERNEL)?;
        }

        // Control state.
        let mut cval = self.new_elem(unit_id);
        cval.val_type     = USB_MIXER_U8;
        cval.channels     = 1;
        cval.min          = 1;
        cval.max          = n_pins as i32;
        cval.res          = 1;
        cval.max_exposed  = n_pins as i32 - 1;
        cval.initialized  = true;
        let subtype = desc[2];
        cval.control = if self.protocol == UAC_VERSION_2 {
            if subtype == UAC2_CLOCK_SELECTOR { UAC2_CX_CLOCK_SELECTOR } else { UAC2_SU_SELECTOR }
        } else {
            0
        };

        // Build control name.
        let mut name_buf = [0u8; CTL_NAME_LEN];
        let isel_idx = if 5 + n_pins < desc.len() { desc[5 + n_pins] } else { 0 };
        let mut nlen = self.chip.dev.string(isel_idx, &mut name_buf).unwrap_or(0);
        if nlen == 0 { nlen = self.get_term_name_bytes(&self.oterm, &mut name_buf); }
        if nlen == 0 { let s = b"USB"; name_buf[..s.len()].copy_from_slice(s); nlen = s.len(); }

        let sfx: &[u8] = if subtype == UAC2_CLOCK_SELECTOR {
            b" Clock Source"
        } else if (self.oterm.term_type & 0xff00) == 0x0100 {
            b" Capture Source"
        } else {
            b" Playback Source"
        };
        let add = sfx.len().min(name_buf.len().saturating_sub(1).saturating_sub(nlen));
        name_buf[nlen..nlen + add].copy_from_slice(&sfx[..add]);
        nlen += add;
        name_buf[nlen] = 0;

        // Create kcontrol.
        let name_nul = name_buf.iter().position(|&b| b == 0).unwrap_or(name_buf.len());
        let name_cstr = CStr::from_bytes_with_nul(&name_buf[..=name_nul]).map_err(|_| EINVAL)?;

        let ops = SelectorCtl {
            chip: Arc::clone(self.chip),
            cval: LockedBy::new(&self.chip.mutex, cval),
            num_items: n_pins,
            item_names,
        };

        self.card.add_kcontrol(
            KControlConfig {
                bump_on_collision: true,
                ..KControlConfig::new(control::ElemIface::Mixer, name_cstr)
            },
            ops,
        )?;

        Ok(())
    }
}

//
// Small helpers
//
/// Write "Input N" (1-based) as a null-terminated byte string into `out`.
fn write_input_name(out: &mut KVec<u8>, n: usize) -> Result<()> {
    let prefix = b"Input ";
    out.extend_from_slice(prefix, GFP_KERNEL)?;
    // Write decimal digits for n (small number, at most 2-3 digits).
    let mut digits = [0u8; 4];
    let mut pos = digits.len();
    let mut val = n;
    loop {
        pos -= 1;
        digits[pos] = b'0' + (val % 10) as u8;
        val /= 10;
        if val == 0 { break; }
    }
    out.extend_from_slice(&digits[pos..], GFP_KERNEL)?;
    out.push(0, GFP_KERNEL)?;
    Ok(())
}

//
// Public entry point
//
/// Locate the AudioControl interface and register all ALSA mixer controls.
///
/// Called during probe after PCM streams are set up.  Non-fatal on failure.
pub(crate) fn create_mixer(
    chip: &Arc<UsbAudioChip>,
    card: &kernel::sound::card::Card,
) -> Result<()> {
    let altsetting = crate::helper::find_ctrl_interface(chip.device())?;
    let ctrl_intf_num = altsetting.number();
    let protocol = altsetting.protocol();
    let buf = altsetting.extra();

    // Set card mixer name.
    card.set_mixer_name(c"USB Mixer");

    let mut build = MixerBuild::new(chip, card, ctrl_intf_num, protocol, buf);

    // Walk output terminal descriptors to seed the parse.
    let mut cur_off: Option<usize> = None;
    loop {
        let off = match find_csint_desc(build.buf, cur_off, UAC_OUTPUT_TERMINAL) {
            Some(o) => o,
            None    => break,
        };
        let desc = &build.buf[off..];
        if desc.len() < 9 {
            cur_off = Some(off);
            continue;
        }

        let terminal_id  = desc[3];
        let source_id    = desc[7];
        let term_type_v  = u16::from_le_bytes([desc[4], desc[5]]);
        let name_idx = match protocol {
            UAC_VERSION_1 if desc.len() >= 9  => desc[8],
            UAC_VERSION_2 if desc.len() >= 12 => desc[11],
            _ => 0,
        };

        build.oterm = AudioTerm { term_type: term_type_v, name_idx };
        let _ = build.test_and_set_unit(terminal_id);
        let _ = build.parse_unit(source_id);

        // UAC2: also parse the associated clock source.
        if protocol == UAC_VERSION_2 && desc.len() >= 11 {
            let clock_id = desc[9];
            let _ = build.parse_unit(clock_id);
        }

        cur_off = Some(off);
    }

    pr_info!(
        "snd_rust_usb_audio: mixer initialised (protocol=0x{:02x}, intf={})\n",
        protocol, ctrl_intf_num,
    );
    Ok(())
}
