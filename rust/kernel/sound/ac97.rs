// SPDX-License-Identifier: GPL-2.0

//! AC97 codec abstraction.
//!
//! Wraps the ALSA AC97 bus and codec API (`include/sound/ac97_codec.h`).
//! The primary consumers are PCI sound drivers that attach an AC97 codec
//! chip via an AC-link (e.g. Intel ICH).
//!
//! # Usage overview
//!
//! 1. Implement [`Ac97BusOps`] on your chip type.
//! 2. Declare a `static` [`Ac97BusOpsTable`] for that type.
//! 3. In probe, call [`ac97_bus`] to create the bus.
//! 4. For each codec, fill an [`Ac97Template`] and call [`ac97_mixer`].
//! 5. Use the returned [`Ac97`] handles for suspend/resume and rate queries.

use core::marker::PhantomData;

use kernel::{bindings, prelude::*};

use super::card::Card;

//
// Trait: Ac97BusOps
//
/// Hardware-side callbacks an AC97 host controller must supply.
///
/// Only `write` and `read` are mandatory; the rest default to no-ops.
/// Set the corresponding `HAS_*` const to `true` and override the method
/// to activate the optional callbacks.
pub trait Ac97BusOps: Sync {
    /// Whether this type provides a cold-reset implementation.
    const HAS_RESET: bool = false;
    /// Whether this type provides a warm-reset implementation.
    const HAS_WARM_RESET: bool = false;

    /// Write `val` to AC97 register `reg` on codec `codec_num`.
    fn write(&self, codec_num: u16, reg: u16, val: u16);

    /// Read from AC97 register `reg` on codec `codec_num`.
    fn read(&self, codec_num: u16, reg: u16) -> u16;

    /// Cold reset (optional). Called by the AC97 layer during codec init.
    fn reset(&self) {}

    /// Warm reset (optional). Called by the AC97 layer on resume.
    fn warm_reset(&self) {}
}

//
// Trampolines
//
unsafe extern "C" fn trampoline_write<T: Ac97BusOps>(
    ac97: *mut bindings::snd_ac97,
    reg: u16,
    val: u16,
) {
    // SAFETY: private_data points to T (set via Ac97Template::private_data).
    // num is the codec index set by the AC97 layer.
    let chip = unsafe { &*((*ac97).private_data as *const T) };
    let codec_num = unsafe { (*ac97).num };
    chip.write(codec_num, reg, val);
}

unsafe extern "C" fn trampoline_read<T: Ac97BusOps>(
    ac97: *mut bindings::snd_ac97,
    reg: u16,
) -> u16 {
    let chip = unsafe { &*((*ac97).private_data as *const T) };
    let codec_num = unsafe { (*ac97).num };
    chip.read(codec_num, reg)
}

unsafe extern "C" fn trampoline_reset<T: Ac97BusOps>(ac97: *mut bindings::snd_ac97) {
    let chip = unsafe { &*((*ac97).private_data as *const T) };
    chip.reset();
}

unsafe extern "C" fn trampoline_warm_reset<T: Ac97BusOps>(ac97: *mut bindings::snd_ac97) {
    let chip = unsafe { &*((*ac97).private_data as *const T) };
    chip.warm_reset();
}

//
// Ac97BusOpsTable
//
/// Const-initializable table of AC97 bus callbacks.
///
/// Declare one `static` of this type per chip type and pass a reference
/// to [`ac97_bus`].
///
/// # Example
/// ```
/// static MYDRV_AC97_OPS: Ac97BusOpsTable<MyChip> = Ac97BusOpsTable::new();
/// ```
pub struct Ac97BusOpsTable<T>(bindings::snd_ac97_bus_ops, PhantomData<fn(T)>);

// SAFETY: the table only stores function pointers (no mutable state).
unsafe impl<T: Ac97BusOps> Sync for Ac97BusOpsTable<T> {}

impl<T: Ac97BusOps> Ac97BusOpsTable<T> {
    /// Create a new ops table populated with trampolines for `T`.
    pub const fn new() -> Self {
        Self(
            bindings::snd_ac97_bus_ops {
                reset: if T::HAS_RESET {
                    Some(trampoline_reset::<T>)
                } else {
                    None
                },
                warm_reset: if T::HAS_WARM_RESET {
                    Some(trampoline_warm_reset::<T>)
                } else {
                    None
                },
                write: Some(trampoline_write::<T>),
                read: Some(trampoline_read::<T>),
                wait: None,
                init: None,
            },
            PhantomData,
        )
    }

    /// Return a C-compatible pointer to the ops table.
    ///
    /// The table must have `'static` lifetime (i.e. be a `static` item).
    pub fn as_ptr(&'static self) -> *const bindings::snd_ac97_bus_ops {
        &self.0 as *const _
    }
}

//
// Ac97Bus
//
/// Handle to a live AC97 bus.
///
/// Created by [`ac97_bus`]. The bus is owned by the ALSA card's devres and
/// is freed automatically when the card is freed - do not drop this handle
/// before the card is deregistered.
pub struct Ac97Bus(*mut bindings::snd_ac97_bus);

// SAFETY: raw pointer is an ALSA-managed object; access is serialised by the
// AC97 layer internally (bus_lock spinlock).
unsafe impl Send for Ac97Bus {}
unsafe impl Sync for Ac97Bus {}

impl Ac97Bus {
    /// Override the default AC-link clock (48000 Hz).
    pub fn set_clock(&self, clock: u32) {
        // SAFETY: pointer is valid for the card lifetime.
        unsafe { (*self.0).clock = clock };
    }

    /// Return the codec at index `idx` (0 = primary, 1 = secondary), if present.
    pub fn codec(&self, idx: usize) -> Option<Ac97> {
        if idx >= 4 {
            return None;
        }
        // SAFETY: pointer and codec[] are valid for the card lifetime.
        let p = unsafe { (*self.0).codec[idx] };
        if p.is_null() {
            None
        } else {
            Some(Ac97(p))
        }
    }

    /// Raw pointer to the underlying `snd_ac97_bus`. Use for low-level FFI.
    pub fn as_mut_ptr(&self) -> *mut bindings::snd_ac97_bus {
        self.0
    }
}

//
// Ac97Template
//
/// Template used to create one AC97 codec via [`ac97_mixer`].
pub struct Ac97Template {
    /// Codec index (0 = primary, 1 = secondary).
    pub num: u16,
    /// Driver capability flags (`AC97_SCAP_*`).
    pub scaps: u32,
    /// PCI device pointer for quirk matching (may be null).
    pub pci: *mut bindings::pci_dev,
    /// Pointer passed back to the bus-ops callbacks as `ac97->private_data`.
    /// Typically `Arc::as_ptr(&chip_arc) as *mut _`.
    pub private_data: *mut core::ffi::c_void,
}

// SAFETY: raw pointers here are only stored and later recovered; no aliased
// mutation occurs outside of the AC97 layer's own synchronisation.
unsafe impl Send for Ac97Template {}

impl Ac97Template {
    fn build(&self) -> bindings::snd_ac97_template {
        let mut t: bindings::snd_ac97_template = unsafe { core::mem::zeroed() };
        t.private_data = self.private_data;
        t.pci = self.pci;
        t.num = self.num;
        t.scaps = self.scaps;
        t
    }
}

//
// Ac97
//
/// Handle to a live AC97 codec.
///
/// Created by [`ac97_mixer`]. Like [`Ac97Bus`], the object is owned by the
/// card's devres.
pub struct Ac97(*mut bindings::snd_ac97);

// SAFETY: AC97 codec state is serialised by the AC97 layer (reg_mutex).
unsafe impl Send for Ac97 {}
unsafe impl Sync for Ac97 {}

impl Ac97 {
    /// Suspend the codec (saves register state, powers down AC-link sections).
    pub fn suspend(&self) {
        // SAFETY: pointer is valid; function is safe to call from PM context.
        unsafe { bindings::snd_ac97_suspend(self.0) };
    }

    /// Resume the codec (re-issues cold/warm reset, restores registers).
    pub fn resume(&self) {
        // SAFETY: same as above.
        unsafe { bindings::snd_ac97_resume(self.0) };
    }

    /// Extended feature identification register value (register 0x28).
    /// Use `AC97_EI_VRA` etc. to test capabilities.
    pub fn ext_id(&self) -> u16 {
        unsafe { (*self.0).ext_id }
    }

    /// Supported sample rates for the given stream index (`AC97_RATES_*`).
    /// Returns a bitmask of `SNDRV_PCM_RATE_*` flags.
    pub fn rates(&self, stream_idx: usize) -> u32 {
        unsafe { (*self.0).rates[stream_idx] }
    }

    /// Set the hardware sample rate for the given AC97 rate register
    /// (`AC97_PCM_FRONT_DAC_RATE`, `AC97_PCM_LR_ADC_RATE`, etc.).
    pub fn set_rate(&self, reg: i32, rate: u32) -> Result {
        // SAFETY: ac97 pointer is valid; snd_ac97_set_rate is re-entrant
        // under the codec's reg_mutex.
        let ret = unsafe { bindings::snd_ac97_set_rate(self.0, reg, rate) };
        if ret < 0 {
            Err(Error::from_errno(ret))
        } else {
            Ok(())
        }
    }

    /// Raw pointer to the underlying `snd_ac97`.
    pub fn as_mut_ptr(&self) -> *mut bindings::snd_ac97 {
        self.0
    }
}

//
// Free functions
//
/// Create an AC97 bus attached to `card`.
///
/// `ops` must be a `'static` reference so the C side can hold the pointer
/// for the lifetime of the card.
///
/// `private_data` is stored in `bus->private_data` and recovered in the
/// bus-ops trampolines via `ac97->private_data` (set by the template).
pub fn ac97_bus<T: Ac97BusOps>(
    card: &Card,
    ops: &'static Ac97BusOpsTable<T>,
    private_data: *mut T,
) -> Result<Ac97Bus> {
    let mut bus_ptr: *mut bindings::snd_ac97_bus = core::ptr::null_mut();
    // SAFETY: card pointer is valid; ops pointer is 'static; snd_ac97_bus
    // allocates and registers the bus as SNDRV_DEV_BUS (freed with card).
    let ret = unsafe {
        bindings::snd_ac97_bus(
            card.as_raw(),
            0,
            ops.as_ptr(),
            private_data as *mut core::ffi::c_void,
            &mut bus_ptr,
        )
    };
    if ret < 0 {
        return Err(Error::from_errno(ret));
    }
    Ok(Ac97Bus(bus_ptr))
}

/// Create and initialise one AC97 codec on `bus`.
///
/// Internally calls `snd_ac97_mixer()` which performs codec identification,
/// patch application, and ALSA mixer control registration.
pub fn ac97_mixer(bus: &Ac97Bus, template: &Ac97Template) -> Result<Ac97> {
    let mut tmpl = template.build();
    let mut ac97_ptr: *mut bindings::snd_ac97 = core::ptr::null_mut();
    // SAFETY: bus pointer is valid; tmpl is stack-allocated temporary.
    let ret = unsafe { bindings::snd_ac97_mixer(bus.0, &mut tmpl, &mut ac97_ptr) };
    if ret < 0 {
        return Err(Error::from_errno(ret));
    }
    Ok(Ac97(ac97_ptr))
}
