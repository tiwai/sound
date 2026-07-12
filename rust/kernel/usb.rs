// SPDX-License-Identifier: GPL-2.0
// SPDX-FileCopyrightText: Copyright (C) 2025 Collabora Ltd.

//! Abstractions for the USB bus.
//!
//! C header: [`include/linux/usb.h`](srctree/include/linux/usb.h)

use crate::{
    bindings,
    device,
    device_id::{
        RawDeviceId,
        RawDeviceIdIndex, //
    },
    driver,
    error::{
        from_result,
        to_result, //
    },
    prelude::*,
    sync::aref::AlwaysRefCounted,
    types::Opaque,
    usb::ch9::{
        Direction,
        EndpointDescriptor,
        InterfaceClass,
        InterfaceDescriptor, //
    },
    ThisModule, //
};
use core::{
    marker::PhantomData,
    mem::offset_of,
    ptr::NonNull, //
    slice, //
};

pub mod ch9;

/// An adapter for the registration of USB drivers.
pub struct Adapter<T: Driver>(T);

// SAFETY:
// - `bindings::usb_driver` is a C type declared as `repr(C)`.
// - `T::Data` is the type of the driver's device private data.
// - `struct usb_driver` embeds a `struct device_driver`.
// - `DEVICE_DRIVER_OFFSET` is the correct byte offset to the embedded `struct device_driver`.
unsafe impl<T: Driver> driver::DriverLayout for Adapter<T> {
    type DriverType = bindings::usb_driver;
    type DriverData<'bound> = T::Data<'bound>;
    const DEVICE_DRIVER_OFFSET: usize = core::mem::offset_of!(Self::DriverType, driver);
}

// SAFETY: A call to `unregister` for a given instance of `DriverType` is guaranteed to be valid if
// a preceding call to `register` has been successful.
unsafe impl<T: Driver> driver::RegistrationOps for Adapter<T> {
    unsafe fn register(
        udrv: &Opaque<Self::DriverType>,
        name: &'static CStr,
        module: &'static ThisModule,
    ) -> Result {
        // SAFETY: It's safe to set the fields of `struct usb_driver` on initialization.
        unsafe {
            (*udrv.get()).name = name.as_char_ptr();
            (*udrv.get()).probe = Some(Self::probe_callback);
            (*udrv.get()).disconnect = Some(Self::disconnect_callback);
            (*udrv.get()).id_table = T::ID_TABLE.as_ptr();
        }

        // SAFETY: `udrv` is guaranteed to be a valid `DriverType`.
        to_result(unsafe {
            bindings::usb_register_driver(udrv.get(), module.as_ptr(), name.as_char_ptr())
        })
    }

    unsafe fn unregister(udrv: &Opaque<Self::DriverType>) {
        // SAFETY: `udrv` is guaranteed to be a valid `DriverType`.
        unsafe { bindings::usb_deregister(udrv.get()) };
    }
}

impl<T: Driver> Adapter<T> {
    extern "C" fn probe_callback(
        intf: *mut bindings::usb_interface,
        id: *const bindings::usb_device_id,
    ) -> kernel::ffi::c_int {
        // SAFETY: The USB core only ever calls the probe callback with a valid pointer to a
        // `struct usb_interface` and `struct usb_device_id`.
        //
        // INVARIANT: `intf` is valid for the duration of `probe_callback()`.
        let intf = unsafe { &*intf.cast::<Interface<device::CoreInternal<'_>>>() };

        from_result(|| {
            // SAFETY: `DeviceId` is a `#[repr(transparent)]` wrapper of `struct usb_device_id` and
            // does not add additional invariants, so it's safe to transmute.
            let id = unsafe { &*id.cast::<DeviceId>() };

            // SAFETY: `id` comes from `T::ID_TABLE` which is of type `IdArray<_, T::IdInfo>`. It
            // can also come from dynamic IDs, which will ensure that `driver_data` exists in
            // `T::ID_TABLE` or is 0.
            let info = unsafe { id.info_unchecked_opt::<T::IdInfo>() };
            let data = T::probe(intf, id, info);

            let dev: &device::Device<device::CoreInternal<'_>> = intf.as_ref();
            dev.set_drvdata(data)?;
            Ok(0)
        })
    }

    extern "C" fn disconnect_callback(intf: *mut bindings::usb_interface) {
        // SAFETY: The USB core only ever calls the disconnect callback with a valid pointer to a
        // `struct usb_interface`.
        //
        // INVARIANT: `intf` is valid for the duration of `disconnect_callback()`.
        let intf = unsafe { &*intf.cast::<Interface<device::CoreInternal<'_>>>() };

        let dev: &device::Device<device::CoreInternal<'_>> = intf.as_ref();

        // SAFETY: `disconnect_callback` is only ever called after a successful call to
        // `probe_callback`, hence it's guaranteed that `Device::set_drvdata()` has been called
        // and stored a `Pin<KBox<T::Data<'_>>>`.
        let data = unsafe { dev.drvdata_borrow::<T::Data<'_>>() };

        T::disconnect(intf, data);
    }
}

/// Abstraction for the USB device ID structure, i.e. [`struct usb_device_id`].
///
/// [`struct usb_device_id`]: https://docs.kernel.org/driver-api/basics.html#c.usb_device_id
#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct DeviceId(bindings::usb_device_id);

impl DeviceId {
    /// Equivalent to C's `USB_DEVICE` macro.
    pub const fn from_id(vendor: u16, product: u16) -> Self {
        Self(bindings::usb_device_id {
            match_flags: bindings::USB_DEVICE_ID_MATCH_DEVICE as u16,
            idVendor: vendor,
            idProduct: product,
            ..pin_init::zeroed()
        })
    }

    /// Equivalent to C's `USB_DEVICE_VER` macro.
    pub const fn from_device_ver(vendor: u16, product: u16, bcd_lo: u16, bcd_hi: u16) -> Self {
        Self(bindings::usb_device_id {
            match_flags: bindings::USB_DEVICE_ID_MATCH_DEVICE_AND_VERSION as u16,
            idVendor: vendor,
            idProduct: product,
            bcdDevice_lo: bcd_lo,
            bcdDevice_hi: bcd_hi,
            ..pin_init::zeroed()
        })
    }

    /// Equivalent to C's `USB_DEVICE_INFO` macro.
    pub const fn from_device_info(class: u8, subclass: u8, protocol: u8) -> Self {
        Self(bindings::usb_device_id {
            match_flags: bindings::USB_DEVICE_ID_MATCH_DEV_INFO as u16,
            bDeviceClass: class,
            bDeviceSubClass: subclass,
            bDeviceProtocol: protocol,
            ..pin_init::zeroed()
        })
    }

    /// Equivalent to C's `USB_INTERFACE_INFO` macro.
    pub const fn from_interface_info(class: u8, subclass: u8, protocol: u8) -> Self {
        Self(bindings::usb_device_id {
            match_flags: bindings::USB_DEVICE_ID_MATCH_INT_INFO as u16,
            bInterfaceClass: class,
            bInterfaceSubClass: subclass,
            bInterfaceProtocol: protocol,
            ..pin_init::zeroed()
        })
    }

    /// Equivalent to C's `USB_DEVICE_INTERFACE_CLASS` macro.
    pub const fn from_device_interface_class(vendor: u16, product: u16, class: u8) -> Self {
        Self(bindings::usb_device_id {
            match_flags: (bindings::USB_DEVICE_ID_MATCH_DEVICE
                | bindings::USB_DEVICE_ID_MATCH_INT_CLASS) as u16,
            idVendor: vendor,
            idProduct: product,
            bInterfaceClass: class,
            ..pin_init::zeroed()
        })
    }

    /// Equivalent to C's `USB_DEVICE_INTERFACE_PROTOCOL` macro.
    pub const fn from_device_interface_protocol(vendor: u16, product: u16, protocol: u8) -> Self {
        Self(bindings::usb_device_id {
            match_flags: (bindings::USB_DEVICE_ID_MATCH_DEVICE
                | bindings::USB_DEVICE_ID_MATCH_INT_PROTOCOL) as u16,
            idVendor: vendor,
            idProduct: product,
            bInterfaceProtocol: protocol,
            ..pin_init::zeroed()
        })
    }

    /// Equivalent to C's `USB_DEVICE_INTERFACE_NUMBER` macro.
    pub const fn from_device_interface_number(vendor: u16, product: u16, number: u8) -> Self {
        Self(bindings::usb_device_id {
            match_flags: (bindings::USB_DEVICE_ID_MATCH_DEVICE
                | bindings::USB_DEVICE_ID_MATCH_INT_NUMBER) as u16,
            idVendor: vendor,
            idProduct: product,
            bInterfaceNumber: number,
            ..pin_init::zeroed()
        })
    }

    /// Equivalent to C's `USB_DEVICE_AND_INTERFACE_INFO` macro.
    pub const fn from_device_and_interface_info(
        vendor: u16,
        product: u16,
        class: u8,
        subclass: u8,
        protocol: u8,
    ) -> Self {
        Self(bindings::usb_device_id {
            match_flags: (bindings::USB_DEVICE_ID_MATCH_INT_INFO
                | bindings::USB_DEVICE_ID_MATCH_DEVICE) as u16,
            idVendor: vendor,
            idProduct: product,
            bInterfaceClass: class,
            bInterfaceSubClass: subclass,
            bInterfaceProtocol: protocol,
            ..pin_init::zeroed()
        })
    }
}

// SAFETY: `DeviceId` is a `#[repr(transparent)]` wrapper of `usb_device_id` and does not add
// additional invariants, so it's safe to transmute to `RawType`.
unsafe impl RawDeviceId for DeviceId {
    type RawType = bindings::usb_device_id;
}

// SAFETY: `DRIVER_DATA_OFFSET` is the offset to the `driver_info` field.
unsafe impl RawDeviceIdIndex for DeviceId {
    const DRIVER_DATA_OFFSET: usize = core::mem::offset_of!(bindings::usb_device_id, driver_info);
}

/// [`IdTable`](kernel::device_id::IdTable) type for USB.
pub type IdTable<T> = &'static dyn kernel::device_id::IdTable<DeviceId, T>;

/// Create a USB `IdTable` with its alias for modpost.
#[macro_export]
macro_rules! usb_device_table {
    ($($tt:tt)*) => {
        $crate::module_device_table!("usb", $crate::usb::DeviceId, $($tt)*);
    };
}

/// The USB driver trait.
///
/// # Examples
///
///```
/// # use kernel::{bindings, device::Core, usb};
/// use kernel::prelude::*;
///
/// struct MyDriver;
///
/// kernel::usb_device_table!(
///     USB_TABLE,
///     <MyDriver as usb::Driver>::IdInfo,
///     [
///         (usb::DeviceId::from_id(0x1234, 0x5678), ()),
///         (usb::DeviceId::from_id(0xabcd, 0xef01), ()),
///     ]
/// );
///
/// impl usb::Driver for MyDriver {
///     type IdInfo = ();
///     type Data<'bound> = Self;
///     const ID_TABLE: usb::IdTable<Self::IdInfo> = &USB_TABLE;
///
///     fn probe<'bound>(
///         _interface: &'bound usb::Interface<Core<'_>>,
///         _id: &usb::DeviceId,
///         _info: Option<&'bound Self::IdInfo>,
///     ) -> impl PinInit<Self::Data<'bound>, Error> + 'bound {
///         Err(ENODEV)
///     }
///
///     fn disconnect<'bound>(
///         _interface: &'bound usb::Interface<Core<'_>>,
///         _data: Pin<&Self::Data<'bound>>,
///     ) {
///     }
/// }
///```
pub trait Driver {
    /// The type holding information about each one of the device ids supported by the driver.
    type IdInfo: 'static;

    /// The type of the driver's bus device private data.
    type Data<'bound>: Send + 'bound;

    /// The table of device ids supported by the driver.
    const ID_TABLE: IdTable<Self::IdInfo>;

    /// USB driver probe.
    ///
    /// Called when a new USB interface is bound to this driver.
    /// Implementers should attempt to initialize the interface here.
    fn probe<'bound>(
        interface: &'bound Interface<device::Core<'_>>,
        id: &DeviceId,
        id_info: Option<&'bound Self::IdInfo>,
    ) -> impl PinInit<Self::Data<'bound>, Error> + 'bound;

    /// USB driver disconnect.
    ///
    /// Called when the USB interface is about to be unbound from this driver.
    fn disconnect<'bound>(
        interface: &'bound Interface<device::Core<'_>>,
        data: Pin<&Self::Data<'bound>>,
    );
}

/// A USB interface.
///
/// This structure represents the Rust abstraction for a C [`struct usb_interface`].
/// The implementation abstracts the usage of a C [`struct usb_interface`] passed
/// in from the C side.
///
/// # Invariants
///
/// An [`Interface`] instance represents a valid [`struct usb_interface`] created
/// by the C portion of the kernel.
///
/// [`struct usb_interface`]: https://www.kernel.org/doc/html/latest/driver-api/usb/usb.html#c.usb_interface
#[repr(transparent)]
pub struct Interface<Ctx: device::DeviceContext = device::Normal>(
    Opaque<bindings::usb_interface>,
    PhantomData<Ctx>,
);

impl<Ctx: device::DeviceContext> Interface<Ctx> {
    fn as_raw(&self) -> *mut bindings::usb_interface {
        self.0.get()
    }

    fn inner(&self) -> &bindings::usb_interface {
        // SAFETY: The type invariants guarantee that `self.0` wraps a valid
        // `struct usb_interface`.
        unsafe { &*self.as_raw() }
    }

    /// Returns the current alternate setting for this interface.
    pub fn cur_altsetting(&self) -> &HostInterface {
        // SAFETY: `cur_altsetting` is a valid `struct usb_host_interface`
        // pointer provided by the USB core. `HostInterface` is
        // `#[repr(transparent)]` over it.
        unsafe { &*(self.inner().cur_altsetting as *const HostInterface) }
    }

    /// Returns all alternate settings for this interface.
    pub fn altsettings(&self) -> &[HostInterface] {
        // SAFETY: `altsetting` is a valid array of `num_altsetting`
        // entries provided by the USB core. `HostInterface` is
        // `#[repr(transparent)]` over `usb_host_interface`.
        unsafe {
            slice::from_raw_parts(
                self.inner().altsetting as *const HostInterface,
                self.inner().num_altsetting as usize,
            )
        }
    }
}

impl Interface<device::Bound> {
    /// Select an alternate setting for this interface.
    ///
    /// On success the device switches to the given alternate setting,
    /// which may change the set of active endpoints. This is a convenience
    /// wrapper around [`Device<Bound>::set_interface`].
    pub fn set_interface(&self, altsetting: u8) -> Result {
        let dev: &Device<device::Bound> = self.as_ref();
        dev.set_interface(self.cur_altsetting().number(), altsetting)
    }
}

/// Abstraction for the USB Host Interface structure, i.e. `struct usb_host_interface`.
#[repr(transparent)]
pub struct HostInterface(Opaque<bindings::usb_host_interface>);

impl HostInterface {
    fn inner(&self) -> &bindings::usb_host_interface {
        // SAFETY: The type invariants guarantee that `self.0` wraps a valid
        // `struct usb_host_interface`.
        unsafe { &*self.0.get() }
    }

    /// Returns the interface descriptor.
    fn desc(&self) -> &InterfaceDescriptor {
        // SAFETY: `desc` is a valid `struct usb_interface_descriptor`
        // embedded in `usb_host_interface`. `InterfaceDescriptor` is
        // `#[repr(transparent)]` over it.
        unsafe { &*((core::ptr::from_ref(&self.inner().desc)).cast()) }
    }

    /// Returns the list of endpoints in this alternate setting.
    pub fn endpoints(&self) -> &[HostEndpoint] {
        // SAFETY: `endpoint` is a valid array of `bNumEndpoints` entries.
        // `HostEndpoint` is `#[repr(transparent)]` over
        // `usb_host_endpoint`.
        unsafe {
            core::ptr::slice_from_raw_parts(
                self.inner().endpoint as *const HostEndpoint,
                self.desc().bNumEndpoints() as usize,
            )
            .as_ref()
            .unwrap_or(&[])
        }
    }

    /// Returns the interface number (`bInterfaceNumber`).
    pub fn number(&self) -> u8 {
        self.desc().bInterfaceNumber()
    }

    /// Returns the alternate setting number (`bAlternateSetting`).
    pub fn alternate_setting(&self) -> u8 {
        self.desc().bAlternateSetting()
    }

    /// Returns the interface class (`bInterfaceClass`).
    pub fn class(&self) -> InterfaceClass {
        self.desc().bInterfaceClass()
    }
}

/// USB endpoint transfer type.
///
/// Maps to the `bmAttributes` field of the endpoint descriptor
/// (`USB_ENDPOINT_XFER_*` constants).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EndpointType {
    /// Control endpoint.
    Control = bindings::USB_ENDPOINT_XFER_CONTROL as u8,
    /// Isochronous endpoint.
    Isoc = bindings::USB_ENDPOINT_XFER_ISOC as u8,
    /// Bulk endpoint.
    Bulk = bindings::USB_ENDPOINT_XFER_BULK as u8,
    /// Interrupt endpoint.
    Int = bindings::USB_ENDPOINT_XFER_INT as u8,
}

/// Abstraction for the USB Host Endpoint structure, i.e. [`struct usb_host_endpoint`].
///
/// [`struct usb_host_endpoint`]: https://docs.kernel.org/driver-api/usb/usb.html#c.usb_host_endpoint
#[repr(transparent)]
pub struct HostEndpoint(Opaque<bindings::usb_host_endpoint>);

impl HostEndpoint {
    fn inner(&self) -> &bindings::usb_host_endpoint {
        // SAFETY: The type invariants guarantee that `self.0` wraps a valid
        // `struct usb_host_endpoint`.
        unsafe { &*self.0.get() }
    }

    /// Returns the endpoint descriptor.
    fn desc(&self) -> &EndpointDescriptor {
        // SAFETY: `desc` is a valid `struct usb_endpoint_descriptor`
        // embedded in `usb_host_endpoint`. `EndpointDescriptor` is
        // `#[repr(transparent)]` over it.
        unsafe { &*(core::ptr::from_ref(&self.inner().desc).cast()) }
    }

    /// Returns the direction of this endpoint (IN or OUT).
    pub fn endpoint_dir(&self) -> Direction {
        if self.desc().bEndpointAddress() & Direction::In as u8 == 0 {
            Direction::Out
        } else {
            Direction::In
        }
    }

    /// Returns the endpoint number (0-15).
    pub fn endpoint_number(&self) -> u8 {
        self.desc().bEndpointAddress() & bindings::USB_ENDPOINT_NUMBER_MASK as u8
    }

    /// Returns the transfer type of this endpoint.
    pub fn endpoint_type(&self) -> EndpointType {
        let val = self.desc().bmAttributes() & bindings::USB_ENDPOINT_XFERTYPE_MASK as u8;
        // SAFETY: `bmAttributes` masked with `USB_ENDPOINT_XFERTYPE_MASK`
        // is guaranteed to be 0-3, which maps exactly to the four
        // `EndpointType` variants.
        unsafe { core::mem::transmute::<u8, EndpointType>(val) }
    }

    /// Returns the interval for interrupt and isochronous endpoints.
    pub fn interval(&self) -> u8 {
        self.desc().bInterval()
    }

    /// Returns the maximum packet size for this endpoint.
    pub fn maxp(&self) -> u16 {
        u16::from_le(self.desc().wMaxPacketSize()) & bindings::USB_ENDPOINT_MAXP_MASK as u16
    }

    /// Returns the high-speed multiplier for isochronous endpoints.
    pub fn maxp_mult(&self) -> u16 {
        (u16::from_le(self.desc().wMaxPacketSize()) & bindings::USB_EP_MAXP_MULT_MASK as u16)
            >> bindings::USB_EP_MAXP_MULT_SHIFT
    }
}

// SAFETY: `usb::Interface` is a transparent wrapper of `struct usb_interface`.
// The offset is guaranteed to point to a valid device field inside `usb::Interface`.
unsafe impl<Ctx: device::DeviceContext> device::AsBusDevice<Ctx> for Interface<Ctx> {
    const OFFSET: usize = offset_of!(bindings::usb_interface, dev);
}

// SAFETY: `Interface` is a transparent wrapper of a type that doesn't depend on
// `Interface`'s generic argument.
kernel::impl_device_context_deref!(unsafe { Interface });
kernel::impl_device_context_into_aref!(Interface);

impl<Ctx: device::DeviceContext> AsRef<device::Device<Ctx>> for Interface<Ctx> {
    fn as_ref(&self) -> &device::Device<Ctx> {
        // SAFETY: By the type invariant of `Self`, `self.as_raw()` is a pointer to a valid
        // `struct usb_interface`.
        let dev = unsafe { &raw mut ((*self.as_raw()).dev) };

        // SAFETY: `dev` points to a valid `struct device`.
        unsafe { device::Device::from_raw(dev) }
    }
}

impl<Ctx: device::DeviceContext> AsRef<Device<Ctx>> for Interface<Ctx> {
    fn as_ref(&self) -> &Device<Ctx> {
        // SAFETY: `self.as_raw()` is valid by the type invariants.
        let usb_dev = unsafe { bindings::interface_to_usbdev(self.as_raw()) };

        // SAFETY: For a valid `struct usb_interface` pointer, the above call to
        // `interface_to_usbdev()` guarantees to return a valid pointer to a `struct usb_device`.
        unsafe { &*(usb_dev.cast()) }
    }
}

// SAFETY: Instances of `Interface` are always reference-counted.
unsafe impl AlwaysRefCounted for Interface {
    #[inline]
    fn inc_ref(&self) {
        // SAFETY: The invariants of `Interface` guarantee that `self.as_raw()`
        // returns a valid `struct usb_interface` pointer, for which we will
        // acquire a new refcount.
        unsafe { bindings::usb_get_intf(self.as_raw()) };
    }

    #[inline]
    unsafe fn dec_ref(obj: NonNull<Self>) {
        // SAFETY: The safety requirements guarantee that the refcount is non-zero.
        unsafe { bindings::usb_put_intf(obj.cast().as_ptr()) }
    }
}

// SAFETY: A `Interface` is always reference-counted and can be released from any thread.
unsafe impl Send for Interface {}

// SAFETY: It is safe to send a &Interface to another thread because we do not
// allow any mutation through a shared reference.
unsafe impl Sync for Interface {}

/// A USB device.
///
/// This structure represents the Rust abstraction for a C [`struct usb_device`].
/// The implementation abstracts the usage of a C [`struct usb_device`] passed in
/// from the C side.
///
/// # Invariants
///
/// A [`Device`] instance represents a valid [`struct usb_device`] created by the C portion of the
/// kernel.
///
/// [`struct usb_device`]: https://www.kernel.org/doc/html/latest/driver-api/usb/usb.html#c.usb_device
#[repr(transparent)]
pub struct Device<Ctx: device::DeviceContext = device::Normal>(
    Opaque<bindings::usb_device>,
    PhantomData<Ctx>,
);

impl<Ctx: device::DeviceContext> Device<Ctx> {
    fn as_raw(&self) -> *mut bindings::usb_device {
        self.0.get()
    }
}

impl Device<device::Bound> {
    /// Select an alternate setting for the given interface.
    ///
    /// On success the device switches the given interface to the given alternate setting,
    /// which may change the set of active endpoints.
    pub fn set_interface(&self, interface: u8, altsetting: u8) -> Result {
        // SAFETY: `self.as_raw()` is a valid `struct usb_device` pointer by the type
        // invariants. `usb_set_interface` is safe to call on a bound device.
        to_result(unsafe {
            bindings::usb_set_interface(self.as_raw(), i32::from(interface), i32::from(altsetting))
        })
    }
}

// SAFETY: `Device` is a transparent wrapper of a type that doesn't depend on `Device`'s generic
// argument.
kernel::impl_device_context_deref!(unsafe { Device });
kernel::impl_device_context_into_aref!(Device);

// SAFETY: Instances of `Device` are always reference-counted.
unsafe impl AlwaysRefCounted for Device {
    #[inline]
    fn inc_ref(&self) {
        // SAFETY: The invariants of `Device` guarantee that `self.as_raw()`
        // returns a valid `struct usb_device` pointer, for which we will
        // acquire a new refcount.
        unsafe { bindings::usb_get_dev(self.as_raw()) };
    }

    #[inline]
    unsafe fn dec_ref(obj: NonNull<Self>) {
        // SAFETY: The safety requirements guarantee that the refcount is non-zero.
        unsafe { bindings::usb_put_dev(obj.cast().as_ptr()) }
    }
}

impl<Ctx: device::DeviceContext> AsRef<device::Device<Ctx>> for Device<Ctx> {
    fn as_ref(&self) -> &device::Device<Ctx> {
        // SAFETY: By the type invariant of `Self`, `self.as_raw()` is a pointer to a valid
        // `struct usb_device`.
        let dev = unsafe { &raw mut ((*self.as_raw()).dev) };

        // SAFETY: `dev` points to a valid `struct device`.
        unsafe { device::Device::from_raw(dev) }
    }
}

// SAFETY: A `Device` is always reference-counted and can be released from any thread.
unsafe impl Send for Device {}

// SAFETY: It is safe to send a &Device to another thread because we do not
// allow any mutation through a shared reference.
unsafe impl Sync for Device {}

// SAFETY: Same as `Device<Normal>` -- the underlying `struct usb_device` is the same;
// `Bound` is a zero-sized type-state marker that does not affect thread safety.
unsafe impl Sync for Device<device::Bound> {}

/// Declares a kernel module that exposes a single USB driver.
///
/// # Examples
///
/// ```ignore
/// module_usb_driver! {
///     type: MyDriver,
///     name: "Module name",
///     author: ["Author name"],
///     description: "Description",
///     license: "GPL v2",
/// }
/// ```
#[macro_export]
macro_rules! module_usb_driver {
    ($($f:tt)*) => {
        $crate::module_driver!(<T>, $crate::usb::Adapter<T>, { $($f)* });
    }
}
