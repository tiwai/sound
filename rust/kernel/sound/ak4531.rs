// SPDX-License-Identifier: GPL-2.0

//! AK4531 codec abstraction.
//!
//! Wraps the ALSA AK4531 codec API (`include/sound/ak4531_codec.h`).
//! The primary consumer is the ENS1370 driver.
//!
//! # Usage overview
//!
//! 1. Implement [`Ak4531Ops`] on your chip type.
//! 2. Create an [`Ak4531Template`] wrapping the private data pointer.
//! 3. Call [`ak4531_mixer`] to register the codec.
//! 4. Use the returned [`Ak4531`] handle to control power states (suspend/resume).

use kernel::{bindings, prelude::*};

use super::card::Card;

//
// Trait: Ak4531Ops
//
/// Hardware-side callbacks an AK4531 host controller must supply.
pub trait Ak4531Ops: Sync {
    /// Write `val` to AK4531 register `reg`.
    fn write(&self, reg: u16, val: u16);
}

//
// Trampoline
//
unsafe extern "C" fn trampoline_write<T: Ak4531Ops>(
    ak4531: *mut bindings::snd_ak4531,
    reg: u16,
    val: u16,
) {
    // SAFETY: private_data points to T (set via Ak4531Template::private_data).
    let chip = unsafe { &*((*ak4531).private_data as *const T) };
    chip.write(reg, val);
}

//
// Ak4531Template
//
/// Template used to create an AK4531 codec.
pub struct Ak4531Template {
    /// Pointer passed back to the ops callbacks as `ak4531->private_data`.
    /// Typically `Arc::as_ptr(&chip_arc) as *mut _`.
    pub private_data: *mut core::ffi::c_void,
}

// SAFETY: private_data is only stored and used in the trampoline callback;
// no concurrent mutation occurs.
unsafe impl Send for Ak4531Template {}

//
// Ak4531
//
/// Handle to a live AK4531 codec.
///
/// Created by [`ak4531_mixer`]. The codec is owned by the card's devres
/// and is freed automatically when the card is freed.
#[derive(Clone, Copy)]
pub struct Ak4531(*mut bindings::snd_ak4531);

// SAFETY: AK4531 codec state is serialised by the ALSA layer (reg_mutex).
unsafe impl Send for Ak4531 {}
unsafe impl Sync for Ak4531 {}

impl Ak4531 {
    /// Suspend the codec (saves register state, powers down sections).
    pub fn suspend(&self) {
        // SAFETY: pointer is valid; function is safe to call from PM context.
        unsafe { bindings::snd_ak4531_suspend(self.0) };
    }

    /// Resume the codec (restores registers).
    pub fn resume(&self) {
        // SAFETY: same as above.
        unsafe { bindings::snd_ak4531_resume(self.0) };
    }

    /// Raw pointer to the underlying `snd_ak4531`.
    pub fn as_mut_ptr(&self) -> *mut bindings::snd_ak4531 {
        self.0
    }
}

/// Create and initialise one AK4531 codec on `card`.
///
/// Internally calls `snd_ak4531_mixer()` which performs codec identification,
/// initialization, and ALSA mixer control registration.
pub fn ak4531_mixer<T: Ak4531Ops>(
    card: &Card,
    template: &Ak4531Template,
) -> Result<Ak4531> {
    let mut ak4531_tmpl: bindings::snd_ak4531 = unsafe { core::mem::zeroed() };
    ak4531_tmpl.write = Some(trampoline_write::<T>);
    ak4531_tmpl.private_data = template.private_data;

    let mut ak4531_out: *mut bindings::snd_ak4531 = core::ptr::null_mut();
    // SAFETY: card pointer is valid; ak4531_tmpl is stack-allocated temporary.
    let ret = unsafe {
        bindings::snd_ak4531_mixer(card.as_raw(), &mut ak4531_tmpl, &mut ak4531_out)
    };
    if ret < 0 {
        return Err(Error::from_errno(ret));
    }
    Ok(Ak4531(ak4531_out))
}
