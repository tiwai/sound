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
use core::ptr::NonNull;
use core::ops::Deref;

/// Power state: fully on (`SNDRV_CTL_POWER_D0`).
pub const POWER_D0: u32 = bindings::SNDRV_CTL_POWER_D0 as u32;
/// Power state: suspended with power retained (`SNDRV_CTL_POWER_D3hot`).
pub const POWER_D3HOT: u32 = bindings::SNDRV_CTL_POWER_D3hot as u32;

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

    /// Changes the card power state and wakes any waiters.
    ///
    /// Call with [`POWER_D3HOT`] in a driver suspend callback and with
    /// [`POWER_D0`] in a driver resume callback to block/unblock userspace
    /// API calls during system sleep transitions.
    pub fn power_change_state(&self, state: u32) {
        // snd_power_change_state() is an inline C function so bindgen does not
        // emit it into bindings_generated.rs.  The helper in rust/helpers/sound.c
        // wraps it; declare the symbol here so the linker resolves it directly
        // without waiting for bindings_helpers_generated.rs to be regenerated.
        #[allow(improper_ctypes)]
        extern "C" {
            #[link_name = "rust_helper_snd_power_change_state"]
            fn snd_power_change_state_helper(
                card: *mut bindings::snd_card,
                state: core::ffi::c_uint,
            );
        }
        // SAFETY: self.as_raw() is a valid snd_card for the lifetime of self.
        unsafe { snd_power_change_state_helper(self.as_raw(), state as _) };
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

/// An owned, RAII-managed sound card.
///
/// Automatically calls `snd_card_free` on drop unless explicitly consumed via
/// [`OwnedCard::free_when_closed`] or [`OwnedCard::free`].
pub struct OwnedCard(NonNull<bindings::snd_card>);

impl OwnedCard {
    /// Allocates a new manually-managed sound card.
    pub fn new<Ctx: device::DeviceContext>(
        parent: &device::Device<Ctx>,
        index: i32,
        id: &CStr,
        module: &'static ThisModule,
    ) -> Result<Self> {
        let mut card_ptr: *mut bindings::snd_card = core::ptr::null_mut();

        // SAFETY: `parent.as_raw()` is a valid device pointer; `id` is a valid
        // C string; `card_ptr` is a local that will be filled by the call.
        to_result(unsafe {
            bindings::snd_card_new(
                parent.as_raw(),
                index,
                id.as_char_ptr(),
                module.as_ptr(),
                0,
                &mut card_ptr,
            )
        })?;

        // SAFETY: on success `card_ptr` is valid and non-null.
        let non_null = NonNull::new(card_ptr).ok_or(ENOMEM)?;
        Ok(Self(non_null))
    }

    /// Immediately frees the sound card.
    pub fn free(self) {
        let this = core::mem::ManuallyDrop::new(self);
        // SAFETY: `this.0` is a valid pointer to `snd_card` owned by this struct.
        unsafe { bindings::snd_card_free(this.0.as_ptr()) };
    }

    /// Hands the card over to ALSA. It will be freed once all userspace handles are closed.
    pub fn free_when_closed(self) {
        let this = core::mem::ManuallyDrop::new(self);
        // SAFETY: `this.0` is a valid pointer to `snd_card` owned by this struct.
        unsafe { bindings::snd_card_free_when_closed(this.0.as_ptr()) };
    }
}

impl Drop for OwnedCard {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a valid pointer to `snd_card` owned by this struct.
        unsafe { bindings::snd_card_free(self.0.as_ptr()) };
    }
}

// SAFETY: `OwnedCard` is a unique wrapper around `snd_card`. Since `Card` is Send,
// `OwnedCard` is safe to send.
unsafe impl Send for OwnedCard {}
// SAFETY: `OwnedCard` is safe to share because all operations are thread-safe or serialized by ALSA core locks.
unsafe impl Sync for OwnedCard {}

impl Deref for OwnedCard {
    type Target = Card;

    fn deref(&self) -> &Self::Target {
        // SAFETY: `self.0` contains a valid pointer to a `snd_card` allocated by ALSA.
        // Since `deref` takes `&self` and ownership has not been consumed via `free` or
        // `free_when_closed`, the card is guaranteed to be active.
        unsafe { &*self.0.as_ptr().cast::<Card>() }
    }
}

/// Creates a new sound card managed by the device's devres, automatically passing `&crate::THIS_MODULE`.
///
/// If `index` is omitted, it defaults to `-1` for automatic assignment.
#[macro_export]
macro_rules! new_sound_card {
    ($parent:expr, $id:expr) => {
        $crate::new_sound_card!($parent, -1, $id)
    };
    ($parent:expr, $index:expr, $id:expr) => {
        $crate::sound::Card::new($parent, $index, $id, ::kernel::module::this_module::<crate::LocalModule>())
    };
}

/// Creates a new manually-managed sound card, automatically passing `&crate::THIS_MODULE`.
///
/// If `index` is omitted, it defaults to `-1` for automatic assignment.
#[macro_export]
macro_rules! new_owned_sound_card {
    ($parent:expr, $id:expr) => {
        $crate::new_owned_sound_card!($parent, -1, $id)
    };
    ($parent:expr, $index:expr, $id:expr) => {
        $crate::sound::OwnedCard::new($parent, $index, $id, ::kernel::module::this_module::<crate::LocalModule>())
    };
}
