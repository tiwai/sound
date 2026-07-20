// SPDX-License-Identifier: GPL-2.0

//! Sound card mixer control abstraction.
//!
//! C header: [`include/sound/control.h`](srctree/include/sound/control.h)

use crate::{
    bindings,
    error::to_result,
    prelude::*,
    str::CStr,
};
use super::card::Card;

/// Mixer control element interface.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ElemIface {
    /// Global card control.
    Card,
    /// Hardware-dependent control.
    Hwdep,
    /// Virtual mixer control.
    Mixer,
    /// PCM-associated control.
    Pcm,
    /// Raw MIDI control.
    Rawmidi,
    /// Timer control.
    Timer,
    /// Sequencer control.
    Sequencer,
}

impl ElemIface {
    fn as_c(&self) -> u32 {
        match self {
            Self::Card => bindings::SNDRV_CTL_ELEM_IFACE_CARD,
            Self::Hwdep => bindings::SNDRV_CTL_ELEM_IFACE_HWDEP,
            Self::Mixer => bindings::SNDRV_CTL_ELEM_IFACE_MIXER,
            Self::Pcm => bindings::SNDRV_CTL_ELEM_IFACE_PCM,
            Self::Rawmidi => bindings::SNDRV_CTL_ELEM_IFACE_RAWMIDI,
            Self::Timer => bindings::SNDRV_CTL_ELEM_IFACE_TIMER,
            Self::Sequencer => bindings::SNDRV_CTL_ELEM_IFACE_SEQUENCER,
        }
    }
}

/// Mixer control element type.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ElemType {
    /// Boolean on/off control.
    Boolean,
    /// 32-bit integer control.
    Integer,
    /// Enumerated control.
    Enumerated,
    /// Byte array control.
    Bytes,
    /// 64-bit integer control.
    Integer64,
}

impl ElemType {
    fn as_c(&self) -> u32 {
        match self {
            Self::Boolean => bindings::SNDRV_CTL_ELEM_TYPE_BOOLEAN,
            Self::Integer => bindings::SNDRV_CTL_ELEM_TYPE_INTEGER,
            Self::Enumerated => bindings::SNDRV_CTL_ELEM_TYPE_ENUMERATED,
            Self::Bytes => bindings::SNDRV_CTL_ELEM_TYPE_BYTES,
            Self::Integer64 => bindings::SNDRV_CTL_ELEM_TYPE_INTEGER64,
        }
    }
}

/// Access flags for mixer controls.
pub mod access {
    /// Control is readable.
    pub const READ: u32 = bindings::SNDRV_CTL_ELEM_ACCESS_READ;
    /// Control is writable.
    pub const WRITE: u32 = bindings::SNDRV_CTL_ELEM_ACCESS_WRITE;
    /// Control is readable and writable.
    pub const READWRITE: u32 = READ | WRITE;
    /// Value changes at each read (volatile).
    pub const VOLATILE: u32 = bindings::SNDRV_CTL_ELEM_ACCESS_VOLATILE;
    /// TLV data is readable.
    pub const TLV_READ: u32 = bindings::SNDRV_CTL_ELEM_ACCESS_TLV_READ;
}

/// Control element info - filled by the `info` callback.
///
/// Wraps `struct snd_ctl_elem_info`.
pub struct ElemInfo(*mut bindings::snd_ctl_elem_info);

impl ElemInfo {
    fn as_raw(&mut self) -> *mut bindings::snd_ctl_elem_info {
        self.0
    }

    /// Sets the element type and count.
    pub fn set_type_count(&mut self, elem_type: ElemType, count: u32) {
        unsafe {
            (*self.as_raw()).type_ = elem_type.as_c() as i32;
            (*self.as_raw()).count = count;
        }
    }

    /// Sets the integer range (min, max, step) for `ElemType::Integer`.
    pub fn set_integer_range(&mut self, min: c_long, max: c_long, step: c_long) {
        unsafe {
            let value = &mut (*self.as_raw()).value;
            value.integer.min = min;
            value.integer.max = max;
            value.integer.step = step;
        }
    }

    /// Sets the 64-bit integer range (min, max, step) for `ElemType::Integer64`.
    pub fn set_integer64_range(&mut self, min: i64, max: i64, step: i64) {
        unsafe {
            let value = &mut (*self.as_raw()).value;
            value.integer64.min = min;
            value.integer64.max = max;
            value.integer64.step = step;
        }
    }

    /// Returns the currently requested enumerated item index.
    pub fn enumerated_item(&self) -> u32 {
        unsafe { (*self.0).value.enumerated.item }
    }

    /// Sets the total number of enumerated items.
    pub fn set_enumerated_items(&mut self, items: u32) {
        unsafe {
            (*self.0).value.enumerated.items = items;
        }
    }

    /// Sets the name of the requested enumerated item.
    ///
    /// Copies up to 63 bytes and ensures null-termination.
    pub fn set_enumerated_name(&mut self, name: &CStr) {
        unsafe {
            let dst = &mut (*self.0).value.enumerated.name;
            // SAFETY: `dst` is a valid fixed-size array of 64 bytes.
            name.copy_to_raw(dst.as_mut_ptr() as *mut u8, 64);
        }
    }
}

/// Control element value - filled/read by `get`/`put` callbacks.
///
/// Wraps `struct snd_ctl_elem_value`.
pub struct ElemValue(*mut bindings::snd_ctl_elem_value);

impl ElemValue {
    fn as_raw(&self) -> *mut bindings::snd_ctl_elem_value {
        self.0
    }

    /// Returns the integer value at `index`.
    pub fn integer(&self, index: usize) -> c_long {
        unsafe { (*self.as_raw()).value.integer.value[index] }
    }

    /// Sets the integer value at `index`.
    pub fn set_integer(&mut self, index: usize, val: c_long) {
        unsafe { (*self.as_raw()).value.integer.value[index] = val };
    }

    /// Returns the 64-bit integer value at `index`.
    pub fn integer64(&self, index: usize) -> i64 {
        unsafe { (*self.as_raw()).value.integer64.value[index] }
    }

    /// Sets the 64-bit integer value at `index`.
    pub fn set_integer64(&mut self, index: usize, val: i64) {
        unsafe { (*self.as_raw()).value.integer64.value[index] = val };
    }

    /// Returns the boolean value at `index`.
    pub fn boolean(&self, index: usize) -> bool {
        self.integer(index) != 0
    }

    /// Sets the boolean value at `index`.
    pub fn set_boolean(&mut self, index: usize, val: bool) {
        self.set_integer(index, val as c_long);
    }

    /// Returns the enumerated item index at `index`.
    pub fn enumerated(&self, index: usize) -> u32 {
        unsafe { (*self.as_raw()).value.enumerated.item[index] }
    }

    /// Sets the enumerated item index at `index`.
    pub fn set_enumerated(&mut self, index: usize, val: u32) {
        unsafe { (*self.as_raw()).value.enumerated.item[index] = val };
    }
}

/// Implemented by driver types that provide mixer control callbacks.
pub trait KControlOps: Send + Sync + 'static {
    /// Fills in `info` with the control's type, range, and count.
    fn info(&self, info: &mut ElemInfo) -> Result;

    /// Reads the current control value into `value`.
    fn get(&self, value: &mut ElemValue) -> Result;

    /// Writes a new control value from `value`.
    ///
    /// Returns `true` if the value changed.
    fn put(&self, value: &ElemValue) -> Result<bool>;
}

// Internal wrapper: stores the Rust ops implementation.
// Stored as `*mut KControlData` in `kcontrol->private_data`.
struct KControlData(KBox<dyn KControlOps>);

impl KControlData {
    unsafe extern "C" fn info_trampoline(
        kctl: *mut bindings::snd_kcontrol,
        uinfo: *mut bindings::snd_ctl_elem_info,
    ) -> core::ffi::c_int {
        let data = unsafe { &*((*kctl).private_data as *const KControlData) };
        let mut info = ElemInfo(uinfo);
        match data.0.info(&mut info) {
            Ok(()) => 0,
            Err(e) => e.to_errno(),
        }
    }

    unsafe extern "C" fn get_trampoline(
        kctl: *mut bindings::snd_kcontrol,
        uvalue: *mut bindings::snd_ctl_elem_value,
    ) -> core::ffi::c_int {
        let data = unsafe { &*((*kctl).private_data as *const KControlData) };
        let mut value = ElemValue(uvalue);
        match data.0.get(&mut value) {
            Ok(()) => 0,
            Err(e) => e.to_errno(),
        }
    }

    unsafe extern "C" fn put_trampoline(
        kctl: *mut bindings::snd_kcontrol,
        uvalue: *mut bindings::snd_ctl_elem_value,
    ) -> core::ffi::c_int {
        let data = unsafe { &*((*kctl).private_data as *const KControlData) };
        let value = ElemValue(uvalue);
        match data.0.put(&value) {
            Ok(changed) => changed as core::ffi::c_int,
            Err(e) => e.to_errno(),
        }
    }

    unsafe extern "C" fn free_trampoline(kctl: *mut bindings::snd_kcontrol) {
        // SAFETY: private_data was set to a raw pointer from KBox::into_raw.
        drop(unsafe { KBox::from_raw((*kctl).private_data as *mut KControlData) });
    }
}

/// Configuration for registering a mixer control with the card.
pub struct KControlConfig<'a> {
    /// Interface type.
    pub iface: ElemIface,
    /// Control name.
    pub name: &'a CStr,
    /// Control index (default 0).
    pub index: u32,
    /// Access flags (default `READWRITE`).
    pub access: u32,
    /// Number of elements in this control (default 1).
    pub count: u32,
    /// Whether to bump the index automatically on name/ID collision (default false).
    pub bump_on_collision: bool,
}

impl<'a> KControlConfig<'a> {
    /// Creates a configuration with common defaults for the given interface type and control name.
    pub fn new(iface: ElemIface, name: &'a CStr) -> Self {
        Self {
            iface,
            name,
            index: 0,
            access: access::READWRITE,
            count: 1,
            bump_on_collision: false,
        }
    }
}

impl Card {
    /// Registers a standard mixer control with default settings.
    pub fn add_mixer_control(&self, name: &CStr, ops: impl KControlOps) -> Result<Handle> {
        self.add_kcontrol(KControlConfig::new(ElemIface::Mixer, name), ops)
    }

    /// Registers a control with custom settings.
    pub fn add_control(&self, iface: ElemIface, name: &CStr, ops: impl KControlOps) -> Result<Handle> {
        self.add_kcontrol(KControlConfig::new(iface, name), ops)
    }

    /// Low-level entry point for fully custom controls.
    pub fn add_kcontrol(&self, config: KControlConfig<'_>, ops: impl KControlOps) -> Result<Handle> {
        let data = KBox::new(
            KControlData(KBox::new(ops, GFP_KERNEL)? as KBox<dyn KControlOps>),
            GFP_KERNEL,
        )?;
        let data_ptr = KBox::into_raw(data);

        let template = bindings::snd_kcontrol_new {
            iface: config.iface.as_c() as i32,
            device: 0,
            subdevice: 0,
            name: config.name.as_char_ptr(),
            index: config.index,
            access: config.access,
            count: config.count,
            info: Some(KControlData::info_trampoline),
            get: Some(KControlData::get_trampoline),
            put: Some(KControlData::put_trampoline),
            tlv: bindings::snd_kcontrol_new__bindgen_ty_1 {
                p: core::ptr::null(),
            },
            private_value: 0,
        };

        // SAFETY: template is a valid snd_kcontrol_new on the stack.
        let kctl = unsafe {
            bindings::snd_ctl_new1(&template, data_ptr as *mut core::ffi::c_void)
        };

        if kctl.is_null() {
            // snd_ctl_new1 failed; free data ourselves.
            drop(unsafe { KBox::from_raw(data_ptr) });
            return Err(ENOMEM);
        }

        if config.bump_on_collision {
            while !unsafe { bindings::snd_ctl_find_id(self.as_raw(), &(*kctl).id) }.is_null() {
                unsafe { (*kctl).id.index += 1; }
            }
        }

        // Install private_free so ALSA calls our destructor on both success and failure.
        unsafe { (*kctl).private_free = Some(KControlData::free_trampoline) };

        // Transfer ownership to the card.
        to_result(unsafe { bindings::snd_ctl_add(self.as_raw(), kctl) })?;

        Ok(Handle(kctl))
    }

    /// Notifies userspace that a control's value has changed.
    ///
    /// `mask` is typically `SNDRV_CTL_EVENT_MASK_VALUE`.
    pub fn notify(&self, ctl: &Handle, mask: u32) {
        unsafe {
            bindings::snd_ctl_notify(self.as_raw(), mask, core::ptr::addr_of_mut!((*ctl.0).id));
        }
    }

    /// Notifies userspace that a control's value has changed.
    ///
    /// This is a simplified version of [`notify`] with `mask` set to `SNDRV_CTL_EVENT_MASK_VALUE`.
    pub fn notify_value(&self, ctl: &Handle) {
        self.notify(ctl, bindings::SNDRV_CTL_EVENT_MASK_VALUE);
    }
}

/// Non-owning handle to a registered mixer control.
///
/// Valid only while the card that owns it is alive.
pub struct Handle(*mut bindings::snd_kcontrol);

// SAFETY: Handle is a raw pointer to a card-owned object protected by ALSA locking.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}
