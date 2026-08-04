// SPDX-License-Identifier: GPL-2.0

//! USB audio chip management and driver registration.
//!
//! Corresponds to `sound/usb/card.c`.

use kernel::{
    bindings,
    device,
    prelude::*,
    sync::{new_mutex, aref::ARef, Arc, Mutex, Refcount},
    usb,
};

use core::{
    ptr::null_mut,
    sync::atomic::Ordering,
};

use crate::types::{
    UAC_HEADER,
    UAC_VERSION_1, UAC_VERSION_2, UAC_VERSION_3,
    USB_CLASS_AUDIO, USB_SUBCLASS_AUDIOCONTROL,
};

//
// Global chip registry
//
// Protected by REGISTER_MUTEX (a global kernel Mutex), which is initialised
// in the module init function before the USB driver is registered.
//
// Mirrors the C driver's list + `register_mutex`.
//
/// State stored in the global registry, protected by [`REGISTER_MUTEX`].
pub(crate) struct RegistryInner {
    pub chips: KVec<Arc<UsbAudioChip>>,
}

impl RegistryInner {
    pub(crate) const fn new() -> Self {
        Self {
            chips: KVec::new(),
        }
    }

    /// Find an existing chip for `dev`, returning its clone.
    fn find_by_dev(
        &self,
        dev: &usb::Device,
    ) -> Option<Arc<UsbAudioChip>> {
        for chip in self.chips.iter() {
            if chip.dev.as_raw() == dev.as_raw() {
                return Some(Arc::clone(chip));
            }
        }
        None
    }
}

kernel::sync::global_lock! {
    // SAFETY: Initialised in module init (UsbAudioModule::init) before
    // usb_register() is called, so before any probe() invocation.
    pub(crate) unsafe(uninit) static REGISTER_MUTEX: Mutex<RegistryInner> = RegistryInner::new();
}

//
// Chip state (protected by the per-chip mutex)
//
/// Per-card mutable state protected by the chip mutex.
pub(crate) struct UsbAudioChipState {
    pub pcm_devs: i32,
    pub ctrl_intf: *mut bindings::usb_host_interface,
    pub sample_rate_read_error: u32,
}

// SAFETY: All raw pointer fields are accessed only while the mutex is held and
// the USB device is valid (between probe and disconnect).
unsafe impl Send for UsbAudioChipState {}

//
// Main chip struct
//
/// Top-level USB audio chip object.
///
/// Corresponds to `struct snd_usb_audio` in `sound/usb/usbaudio.h`.
#[pin_data]
pub(crate) struct UsbAudioChip {
    pub dev: ARef<usb::Device>,
    pub usb_id: u32,
    pub quirk_flags: core::sync::atomic::AtomicU32,
    pub lowlatency: bool,
    pub autoclock: bool,
    pub shutdown: core::sync::atomic::AtomicI32,
    pub active: core::sync::atomic::AtomicI32,
    /// Owned representation of the ALSA card to manage its lifetime via RAII.
    #[pin]
    pub card: Mutex<Option<kernel::sound::OwnedCard>>,
    /// Number of USB AudioControl interfaces currently attached to this chip.
    pub num_interfaces: Refcount,
    #[pin]
    pub mutex: Mutex<UsbAudioChipState>,
}

// SAFETY: UsbAudioChip is referenced only through Arc, and the dev reference
// is valid for the lifetime of the USB device (between probe and disconnect).
unsafe impl Send for UsbAudioChip {}
unsafe impl Sync for UsbAudioChip {}

impl UsbAudioChip {
    pub(crate) fn device(&self) -> &usb::Device {
        &self.dev
    }

    pub(crate) fn bound_device(&self) -> &usb::Device<device::Bound> {
        // SAFETY: The device is bound during probe, and is held alive by the
        // reference to the chip. This is valid for the lifetime of the chip.
        unsafe { &*(self.dev.as_raw() as *const _) }
    }
}

//
// USB driver implementation
//
/// USB audio driver.
pub(crate) struct UsbAudioDriver;

/// Device info shared across all ID table entries.
pub(crate) struct UsbAudioIdInfo;

kernel::usb_device_table!(
    USB_AUDIO_TABLE,
    UsbAudioIdInfo,
    [
        // AudioControl interface, UAC1 (bInterfaceProtocol == 0x00).
        (
            usb::DeviceId::from_interface_info(
                USB_CLASS_AUDIO,
                USB_SUBCLASS_AUDIOCONTROL,
                UAC_VERSION_1,
            ),
            UsbAudioIdInfo
        ),
        // AudioControl interface, UAC2 (bInterfaceProtocol == 0x20).
        (
            usb::DeviceId::from_interface_info(
                USB_CLASS_AUDIO,
                USB_SUBCLASS_AUDIOCONTROL,
                UAC_VERSION_2,
            ),
            UsbAudioIdInfo
        ),
        // AudioControl interface, UAC3 (bInterfaceProtocol == 0x30).
        (
            usb::DeviceId::from_interface_info(
                USB_CLASS_AUDIO,
                USB_SUBCLASS_AUDIOCONTROL,
                UAC_VERSION_3,
            ),
            UsbAudioIdInfo
        ),
    ]
);

/// Data stored per probed USB interface.
#[pin_data]
pub(crate) struct UsbAudioData {
    pub chip: Arc<UsbAudioChip>,
}

// SAFETY: UsbAudioData holds an Arc which is Send.
unsafe impl Send for UsbAudioData {}

/// Core probe logic: find or create the `UsbAudioChip` for `dev`, parse all
/// AudioStreaming interfaces, create mixer controls, and register the card.
///
/// Returns the (possibly shared) `Arc<UsbAudioChip>`.
///
fn do_probe(
    interface: &usb::Interface<device::Core<'_>>,
    dev: &usb::Device,
    usb_id: u32,
    quirk_flags: u32,
    ctrlif: u32,
    speed: u32,
) -> Result<Arc<UsbAudioChip>> {
    // Critical section: look up or create chip
    let mut registry = REGISTER_MUTEX.lock();

    let (chip, is_new) = if let Some(existing) = registry.find_by_dev(dev) {
        // Second (or later) AudioControl interface on the same physical USB
        // device: share the existing card.
        existing.num_interfaces.inc();
        (existing, false)
    } else {
        // New USB device: create chip and ALSA card.
        // Allocate ALSA card (non-devres; freed in disconnect).
        let owned_card = kernel::new_owned_sound_card!(interface.as_ref(), c"USB-Audio")?;

        // Configure card metadata.
        owned_card.set_driver(c"USB-Audio-Rust");
        owned_card.set_short_name(c"USB Audio");
        owned_card.set_long_name(c"Rust USB Audio");

        let chip = Arc::pin_init(
            pin_init!(UsbAudioChip {
                dev:            ARef::from(dev),
                usb_id,
                quirk_flags:    core::sync::atomic::AtomicU32::new(quirk_flags),
                lowlatency:     crate::module_parameters::lowlatency.value(),
                autoclock:      crate::module_parameters::autoclock.value(),
                shutdown:       core::sync::atomic::AtomicI32::new(0),
                active:         core::sync::atomic::AtomicI32::new(0),
                card <- new_mutex!(Some(owned_card)),
                num_interfaces: Refcount::new(1),
                mutex <- new_mutex!(UsbAudioChipState {
                    pcm_devs:  0,
                    ctrl_intf: null_mut(),
                    sample_rate_read_error: 0,
                }),
            }),
            GFP_KERNEL,
        )?;

        registry.chips.push(Arc::clone(&chip), GFP_KERNEL)?;
        (chip, true)
    };

    // Release registry lock before slow descriptor parsing.
    drop(registry);

    // Register card (first probe only)
    if is_new {
        let ret = {
            let card_guard = chip.card.lock();
            card_guard.as_ref().unwrap().register()
        };
        if let Err(e) = ret {
            // Registration failed: clean up the registry slot and the card.
            let mut registry = REGISTER_MUTEX.lock();
            registry.chips.retain(|c| !Arc::ptr_eq(c, &chip));
            drop(registry);

            let card_opt = chip.card.lock().take();
            if let Some(c) = card_opt {
                c.free();
            }
            return Err(e);
        }
        pr_info!(
            "snd_rust_usb_audio: registered ALSA card for {:04x}:{:04x}\n",
            usb_id >> 16,
            usb_id & 0xffff,
        );
    }

    Ok(chip)
}

impl usb::Driver for UsbAudioDriver {
    type IdInfo = UsbAudioIdInfo;
    type Data<'bound> = UsbAudioData;

    const ID_TABLE: usb::IdTable<Self::IdInfo> = &USB_AUDIO_TABLE;

    fn probe<'bound>(
        interface: &'bound usb::Interface<device::Core<'_>>,
        _id: &usb::DeviceId,
        _info: Option<&'bound Self::IdInfo>,
    ) -> impl PinInit<Self::Data<'bound>, Error> + 'bound {
        let usb_dev = interface.usb_device();
        let usb_id = ((usb_dev.vendor() as u32) << 16) | (usb_dev.product() as u32);
        let quirk_flags = crate::quirks::quirk_flags_for_id(usb_id);
        let ctrlif = interface.interface_number();
        let speed = usb_dev.speed();

        pr_info!(
            "snd_rust_usb_audio: probe {:04x}:{:04x} ctrlif={}\n",
            usb_id >> 16,
            usb_id & 0xffff,
            ctrlif,
        );

        try_pin_init!(UsbAudioData {
            chip: do_probe(
                interface,
                usb_dev, usb_id, quirk_flags, ctrlif, speed,
            )?,
        })
    }

    fn disconnect<'bound>(
        _interface: &'bound usb::Interface<device::Core<'_>>,
        data: Pin<&Self::Data<'bound>>,
    ) {
        let chip = &data.chip;

        // Decrement interface counter.  When it reaches zero this is the last
        // AudioControl interface for this device - tear down the card.
        if chip.num_interfaces.dec_and_test() {
            // Remove from global registry.
            {
                let mut registry = REGISTER_MUTEX.lock();
                registry.chips.retain(|c| !Arc::ptr_eq(c, chip));
            }
            // Hand the card to ALSA; freed once all userspace handles close.
            let card_opt = chip.card.lock().take();
            if let Some(c) = card_opt {
                c.free_when_closed();
            }
        }

        pr_info!("snd_rust_usb_audio: disconnected\n");
    }
}
