// SPDX-License-Identifier: GPL-2.0

//! Sound card abstraction.
//!
//! C header: [`include/sound/core.h`](srctree/include/sound/core.h)

use crate::{
    bindings,
    device,
    error::to_result,
    prelude::*,
    types::Opaque,
    ThisModule,
};

/// A sound card.
///
/// Wraps `struct snd_card`. The card lifetime is managed by devres - it will
/// be freed when the parent device is released, not by Rust's drop.
///
/// # Invariants
///
/// A [`Card`] instance always points to a valid `struct snd_card` that was
/// allocated via `snd_devm_card_new()` and is still alive (i.e., the parent
/// device has not been removed yet).
#[repr(transparent)]
pub struct Card(Opaque<bindings::snd_card>);

impl Card {
    /// Creates a new sound card managed by the device's devres.
    ///
    /// The card lifetime is tied to `parent` - it will be freed automatically
    /// when the parent device is removed.
    pub fn new<'a, Ctx: device::DeviceContext>(
        parent: &'a device::Device<Ctx>,
        index: i32,
        id: &CStr,
        module: &'static ThisModule,
    ) -> Result<&'a Card> {
        let mut card_ptr: *mut bindings::snd_card = core::ptr::null_mut();

        // SAFETY: `parent.as_raw()` is a valid device pointer; `id` is a valid
        // C string; `card_ptr` is a local that will be filled by the call.
        to_result(unsafe {
            bindings::snd_devm_card_new(
                parent.as_raw(),
                index,
                id.as_char_ptr(),
                module.as_ptr(),
                0,
                &mut card_ptr,
            )
        })?;

        // SAFETY: on success `card_ptr` is a valid non-null pointer to a
        // `snd_card`. Its lifetime is tied to `parent` via devres.
        Ok(unsafe { &*card_ptr.cast::<Card>() })
    }

    /// Registers the card, making it visible to userspace.
    pub fn register(&self) -> Result {
        // SAFETY: `self.as_raw()` is a valid `snd_card` pointer.
        to_result(unsafe { bindings::snd_card_register(self.as_raw()) })
    }

    /// Sets the `driver` field (short driver name, max 15 chars).
    pub fn set_driver(&self, s: &CStr) {
        // SAFETY: `self.as_raw()` is valid; `driver` is a fixed-size array of 16 bytes.
        unsafe {
            s.copy_to_raw(
                (*self.as_raw()).driver.as_mut_ptr() as *mut u8,
                16,
            );
        }
    }

    /// Sets the `shortname` field (max 31 chars).
    pub fn set_short_name(&self, s: &CStr) {
        // SAFETY: `self.as_raw()` is valid; `shortname` is a fixed-size array of 32 bytes.
        unsafe {
            s.copy_to_raw(
                (*self.as_raw()).shortname.as_mut_ptr() as *mut u8,
                32,
            );
        }
    }

    /// Sets the `longname` field (max 79 chars).
    pub fn set_long_name(&self, s: &CStr) {
        // SAFETY: `self.as_raw()` is valid; `longname` is a fixed-size array of 80 bytes.
        unsafe {
            s.copy_to_raw(
                (*self.as_raw()).longname.as_mut_ptr() as *mut u8,
                80,
            );
        }
    }

    /// Sets the `mixername` field (max 79 chars).
    pub fn set_mixer_name(&self, s: &CStr) {
        // SAFETY: `self.as_raw()` is valid; `mixername` is a fixed-size array of 80 bytes.
        unsafe {
            s.copy_to_raw(
                (*self.as_raw()).mixername.as_mut_ptr() as *mut u8,
                80,
            );
        }
    }

    /// Returns the raw `*mut snd_card` pointer.
    ///
    /// # Safety
    ///
    /// The pointer is valid for the lifetime of `self`. Callers must not
    /// store or use it beyond that lifetime.
    pub fn as_raw(&self) -> *mut bindings::snd_card {
        self.0.get()
    }

}

// SAFETY: `Card` holds only an `Opaque<snd_card>`, whose thread-safety is
// ensured by the ALSA core (card-level mutex/spinlock protects all fields).
unsafe impl Send for Card {}
// SAFETY: All mutable access is serialized by ALSA's own locking.
unsafe impl Sync for Card {}
