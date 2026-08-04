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
    sync::{
        aref::AlwaysRefCounted,
        Arc,
        ArcBorrow, //
    },
    time::Delta,
    types::Opaque,
    usb::ch9::{
        CtrlRequest,
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
    ops::Deref,
    ptr::{
        self,
        NonNull, //
    },
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

    /// Returns the interface number (`bInterfaceNumber`).
    pub fn interface_number(&self) -> u32 {
        self.cur_altsetting().number() as u32
    }

    /// Returns the USB device that this interface belongs to.
    pub fn usb_device(&self) -> &Device {
        // SAFETY: `self.as_raw()` is valid by the type invariants.
        let usb_dev = unsafe { bindings::interface_to_usbdev(self.as_raw()) };

        // SAFETY: For a valid `struct usb_interface` pointer, the above call to
        // `interface_to_usbdev()` guarantees to return a valid pointer to a `struct usb_device`.
        // `Device` is `#[repr(transparent)]` over `struct usb_device`.
        unsafe { &*(usb_dev.cast()) }
    }

    /// Returns the interface association descriptor if it exists.
    pub fn intf_assoc(&self) -> Option<&ch9::InterfaceAssociationDescriptor> {
        let assoc = self.inner().intf_assoc;
        if assoc.is_null() {
            None
        } else {
            // SAFETY: `assoc` is a valid pointer if non-null, owned by the USB core.
            // `InterfaceAssociationDescriptor` is `#[repr(transparent)]` over the raw struct.
            Some(unsafe { &*(assoc.cast()) })
        }
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

    /// Returns the interface subclass (`bInterfaceSubClass`).
    pub fn subclass(&self) -> u8 {
        self.desc().bInterfaceSubClass()
    }

    /// Returns the interface protocol (`bInterfaceProtocol`).
    pub fn protocol(&self) -> u8 {
        self.desc().bInterfaceProtocol()
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

crate::impl_flags!(
    /// URB transfer flags.
    ///
    /// These correspond to the `URB_*` constants in `include/linux/usb.h`.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct TransferFlags(u32);

    /// Represents a single URB transfer flag.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum TransferFlag {
        /// Short packet flag: return an error if the packet is shorter than
        /// expected.
        ShortNotOk = bindings::URB_SHORT_NOT_OK,
        /// Isochronous ASAP flag: schedule the isochronous transfer as soon as
        /// possible.
        IsoAsap = bindings::URB_ISO_ASAP,
        /// Do not perform a DMA mapping for the transfer buffer.
        NoTransferDmaMap = bindings::URB_NO_TRANSFER_DMA_MAP,
        /// Send a zero-length packet at the end of the transfer.
        ZeroPacket = bindings::URB_ZERO_PACKET,
        /// Do not interrupt the CPU when the URB completes.
        NoInterrupt = bindings::URB_NO_INTERRUPT,
    }
);

/// A USB pipe encoding endpoint type, direction, device address, and
/// endpoint number into a single `u32`.
///
/// Pipe encoding follows the kernel's `PIPE_*` macros used by the USB
/// core for control, bulk, isochronous, and interrupt transfers.
#[derive(Clone, Copy)]
pub struct Pipe(u32);

impl Pipe {
    /// Create a host-to-device (OUT) control pipe (endpoint 0).
    pub fn new_send_control_pipe(dev: &Device) -> Self {
        Self(bindings::PIPE_CONTROL << 30 | dev.devnum() << 8)
    }

    /// Create a device-to-host (IN) control pipe (endpoint 0).
    pub fn new_receive_control_pipe(dev: &Device) -> Self {
        Self(bindings::PIPE_CONTROL << 30 | dev.devnum() << 8 | bindings::USB_DIR_IN)
    }

    /// Create a device-to-host (IN) isochronous pipe.
    pub fn new_receive_isoc_pipe(dev: &Device, endpoint: &HostEndpoint) -> Self {
        Self(
            bindings::PIPE_ISOCHRONOUS << 30
                | dev.devnum() << 8
                | u32::from(endpoint.endpoint_number()) << 15
                | bindings::USB_DIR_IN,
        )
    }

    /// Create a host-to-device (OUT) isochronous pipe.
    pub fn new_send_isoc_pipe(dev: &Device, endpoint: &HostEndpoint) -> Self {
        Self(
            bindings::PIPE_ISOCHRONOUS << 30
                | dev.devnum() << 8
                | u32::from(endpoint.endpoint_number()) << 15,
        )
    }

    /// Create a host-to-device (OUT) bulk pipe.
    pub fn new_send_bulk_pipe(dev: &Device, endpoint: &HostEndpoint) -> Self {
        Self(
            bindings::PIPE_BULK << 30
                | dev.devnum() << 8
                | u32::from(endpoint.endpoint_number()) << 15,
        )
    }

    /// Create a device-to-host (IN) bulk pipe.
    pub fn new_receive_bulk_pipe(dev: &Device, endpoint: &HostEndpoint) -> Self {
        Self(
            bindings::PIPE_BULK << 30
                | dev.devnum() << 8
                | u32::from(endpoint.endpoint_number()) << 15
                | bindings::USB_DIR_IN,
        )
    }

    /// Create a host-to-device (OUT) interrupt pipe.
    pub fn new_send_int_pipe(dev: &Device, endpoint: &HostEndpoint) -> Self {
        Self(
            bindings::PIPE_INTERRUPT << 30
                | dev.devnum() << 8
                | u32::from(endpoint.endpoint_number()) << 15,
        )
    }

    /// Create a device-to-host (IN) interrupt pipe.
    pub fn new_receive_int_pipe(dev: &Device, endpoint: &HostEndpoint) -> Self {
        Self(
            bindings::PIPE_INTERRUPT << 30
                | dev.devnum() << 8
                | u32::from(endpoint.endpoint_number()) << 15
                | bindings::USB_DIR_IN,
        )
    }
}

/// A single isochronous packet descriptor within an URB.
///
/// Wraps `struct usb_iso_packet_descriptor` from the C USB core.
#[repr(transparent)]
pub struct IsoPacketDescriptor(bindings::usb_iso_packet_descriptor);

impl IsoPacketDescriptor {
    /// Returns the offset of the packet's data within the transfer buffer.
    pub fn offset(&self) -> u32 {
        self.0.offset
    }

    /// Returns the length of the packet in bytes.
    pub fn length(&self) -> u32 {
        self.0.length
    }

    /// Returns the actual number of bytes transferred in this packet.
    ///
    /// Valid only after the URB completes.
    pub fn actual_length(&self) -> u32 {
        self.0.actual_length
    }

    /// Returns the per-packet completion status.
    ///
    /// Valid only after the URB completes.
    pub fn status(&self) -> i32 {
        self.0.status
    }
}

/// Trait implemented by all URB state marker types.
///
/// Each state specifies pre-cleanup behaviour that runs before the
/// underlying allocation is freed.
pub trait UrbState {
    /// Called before the URB allocation is freed.
    fn pre_drop(urb: &mut bindings::urb);
}

/// Marker type for an idle (unsubmitted) URB.
pub struct Idle;
/// Marker type for an active (submitted, in-flight) URB.
pub struct Active;

impl UrbState for Idle {
    fn pre_drop(_urb: &mut bindings::urb) {}
}
impl UrbState for Active {
    fn pre_drop(urb: &mut bindings::urb) {
        // SAFETY: `urb` is a valid pointer to an initialized `struct urb`.
        unsafe { bindings::usb_kill_urb(urb) }
    }
}

/// A USB Request Block (URB).
///
/// This structure wraps the C [`struct urb`] and provides a safe
/// abstraction for USB transfers.
///
/// [`struct urb`]: https://www.kernel.org/doc/html/latest/driver-api/usb/usb.html#c.urb
#[repr(transparent)]
pub struct Urb<T>(Opaque<bindings::urb>, PhantomData<T>);

impl<T> Urb<T> {
    fn as_raw(&self) -> *mut bindings::urb {
        self.0.get()
    }

    fn inner(&self) -> &bindings::urb {
        // SAFETY: The type invariants guarantee that `self.0` wraps a valid
        // `struct urb`.
        unsafe { &*self.as_raw() }
    }

    fn status(&self) -> i32 {
        self.inner().status
    }

    /// Returns a borrow of the driver-private context data, if any.
    pub fn context(&self) -> Option<ArcBorrow<'_, T>> {
        let context = self.inner().context;
        if context.is_null() {
            None
        } else {
            // SAFETY: `context` was initialized by `Arc::into_raw` in `init_common`.
            Some(unsafe { ArcBorrow::from_raw(context.cast()) })
        }
    }
}

/// A handle to a [`struct urb`] allocated via `usb_alloc_urb`.
///
/// Created by [`Urb::new_bulk`], [`Urb::new_isoc`], etc. The URB is
/// owned by this handle — dropping the handle frees the allocation.
///
/// Use [`Urb::submit`] to transition to [`UrbHandle<T, Active>`].
pub struct UrbHandle<T, S: UrbState = Idle> {
    /// Pointer to the underlying C `struct urb`.
    urb: NonNull<bindings::urb>,
    /// State marker.
    _state: PhantomData<S>,
    /// Type of driver-private context data.
    _ty: PhantomData<T>,
}

// SAFETY: The underlying urb is always reference-counted and can be released from any thread.
unsafe impl<T> Send for UrbHandle<T, Active> {}

impl<T, S: UrbState> Deref for UrbHandle<T, S> {
    type Target = Urb<T>;

    fn deref(&self) -> &Self::Target {
        // SAFETY: `Urb<T>` is a `#[repr(transparent)]` wrapper of `struct urb`,
        unsafe { &*(self.urb.as_ptr() as *const Urb<T>) }
    }
}

impl<T, S: UrbState> Drop for UrbHandle<T, S> {
    fn drop(&mut self) {
        // SAFETY: `self.as_raw()` points to a valid, initialized C `struct urb`.
        let urb: &mut bindings::urb = unsafe { &mut *self.as_raw() };
        S::pre_drop(urb);

        if !urb.context.is_null() {
            // SAFETY: After `pre_drop` the URB is idle, so it is safe to
            // reclaim the context data.
            unsafe {
                drop(Arc::from_raw(urb.context.cast::<T>()));
            }
        }

        if !urb.setup_packet.is_null() {
            // SAFETY: The setup packet was allocated via `KBox::into_raw` in
            // `init_common` and `urb.setup_packet` is still valid.
            unsafe {
                drop(KBox::from_raw(urb.setup_packet.cast::<CtrlRequest>()));
            }
        }

        if !urb.transfer_buffer.is_null() {
            // SAFETY: The transfer buffer was allocated via `KBox::into_raw` in
            // `init_common` and `urb.transfer_buffer` is still valid.
            unsafe {
                drop(KBox::from_raw(ptr::slice_from_raw_parts_mut(
                    urb.transfer_buffer.cast::<u8>(),
                    urb.transfer_buffer_length as usize,
                )));
            }
        }

        // SAFETY: `urb` points to a valid, initialized `struct urb`
        // and is not in-flight.
        unsafe { bindings::usb_free_urb(ptr::from_mut(urb)) };
    }
}

/// A completed URB whose status must be checked before accessing data.
///
/// The driver receives this in its completion handler. Call
/// [`check`](UrbResult::check) to verify the transfer succeeded.
pub struct UrbResult<'a, T> {
    /// The pinned URB reference delivered by the trampoline.
    urb: Pin<&'a mut Urb<T>>,
}

impl<'a, T> Deref for UrbResult<'a, T> {
    type Target = Urb<T>;

    fn deref(&self) -> &Self::Target {
        &self.urb
    }
}

impl<'a, T> UrbResult<'a, T> {
    /// Re-submit the URB from a completion handler.
    ///
    /// Consumes this handle, transferring ownership to the kernel.
    /// This is intentionally private, since a driver should always
    /// check the result in the completion handler.
    fn resubmit(self, mem_flags: kernel::alloc::Flags) -> Result {
        // SAFETY: `self.urb.as_raw()` points to a valid, initialized C `struct urb`.
        to_result(unsafe { bindings::usb_submit_urb(self.as_raw(), mem_flags.as_raw()) })
    }

    /// Check the completion status and grant access to the URB data.
    pub fn check(&mut self) -> Result<UrbData<'_, T>> {
        if self.status() != 0 {
            Err(Error::from_errno(self.status()))
        } else {
            Ok(UrbData {
                urb: self.urb.as_mut(),
            })
        }
    }

    /// Check the completion status, granting data access on success or
    /// resubmitting the URB on failure.
    pub fn check_or_resubmit(
        self,
        mem_flags: kernel::alloc::Flags,
    ) -> Result<UrbData<'a, T>, Result> {
        if self.status() != 0 {
            Err(self.resubmit(mem_flags))
        } else {
            Ok(UrbData { urb: self.urb })
        }
    }
}

/// A successfully completed URB whose data is safe to read.
pub struct UrbData<'a, T> {
    /// The pinned URB reference.
    urb: Pin<&'a mut Urb<T>>,
}

impl<'a, T> Deref for UrbData<'a, T> {
    type Target = Urb<T>;

    fn deref(&self) -> &Self::Target {
        &self.urb
    }
}

impl<'a, T> UrbData<'a, T> {
    /// Returns the number of bytes actually transferred.
    ///
    /// For isochronous URBs this is the sum of all packet
    /// `actual_length` values.
    pub fn actual_length(&self) -> u32 {
        self.inner().actual_length
    }

    /// Returns the transfer buffer as a byte slice.
    pub fn transfer_buffer(&self) -> &[u8] {
        let urb = self.inner();
        if urb.transfer_buffer.is_null() {
            &[]
        } else {
            // SAFETY: The transfer buffer was set in `init_common`.
            // The pointer and length are valid for the lifetime of the `Urb`.
            unsafe {
                slice::from_raw_parts(
                    urb.transfer_buffer as *const u8,
                    urb.transfer_buffer_length as usize,
                )
            }
        }
    }

    /// Returns the ISO frame descriptors for this URB.
    pub fn iso_frame_descs(&self) -> &[IsoPacketDescriptor] {
        let urb = self.inner();

        if urb.number_of_packets == 0 {
            &[]
        } else {
            let data = urb.iso_frame_desc.as_ptr().cast::<IsoPacketDescriptor>();

            // SAFETY: The `iso_frame_desc` flexible array was allocated as
            // part of the `usb_alloc_urb` allocation. `number_of_packets`
            // is the corresponding length.
            unsafe { slice::from_raw_parts(data, urb.number_of_packets as usize) }
        }
    }

    /// Extracts the payload data for a given ISO packet descriptor.
    ///
    /// Returns `Err` if the packet status is non-zero.
    pub fn data_from_iso_packet_desc(
        &self,
        iso_packet_desc: &IsoPacketDescriptor,
    ) -> Result<&[u8]> {
        if iso_packet_desc.status() != 0 {
            return Err(Error::from_errno(iso_packet_desc.status()));
        }
        let urb = self.inner();
        // SAFETY: `iso_packet_desc.offset()` was computed in
        // `init_common` and lies within the transfer buffer.
        let data =
            unsafe { (urb.transfer_buffer.cast::<u8>()).add(iso_packet_desc.offset() as usize) };

        // SAFETY: After URB completion `actual_length()` reflects the
        // valid bytes in the packet. The slice is within the transfer
        // buffer allocation. The packet status was verified above.
        unsafe {
            Ok(slice::from_raw_parts(
                data,
                iso_packet_desc.actual_length() as usize,
            ))
        }
    }

    /// Re-submit the URB from a completion handler.
    ///
    /// Consumes this handle, transferring ownership to the kernel.
    pub fn resubmit(self, mem_flags: kernel::alloc::Flags) -> Result {
        // SAFETY: `self.as_raw()` points to a valid, initialized C `struct urb`.
        to_result(unsafe { bindings::usb_submit_urb(self.as_raw(), mem_flags.as_raw()) })
    }
}

/// Trampoline function to call safe completion handlers.
///
/// # Safety
///
/// `urb_ptr` must point to a valid, initialized `struct urb` whose
/// `context` and `rust_complete` fields were set by [`Urb::init_common`].
unsafe extern "C" fn urb_complete_trampoline<T>(urb_ptr: *mut bindings::urb) {
    // SAFETY: `urb_ptr` is a valid pointer provided by the USB core.
    // `rust_complete` was set to a `fn(UrbResult<'_, T>)` when initialized.
    let complete: fn(UrbResult<'_, T>) = unsafe { core::mem::transmute((*urb_ptr).rust_complete) };
    // SAFETY: `urb_ptr` points to a valid `struct urb`.
    let urb = unsafe { &mut *urb_ptr.cast() };
    // SAFETY: The data `urb` references is never moved.
    let urb = unsafe { Pin::new_unchecked(urb) };
    complete(UrbResult { urb });
}

impl<T> Urb<T> {
    #[allow(clippy::too_many_arguments)]
    fn init_common(
        mem_flags: kernel::alloc::Flags,
        intf: &Interface<device::Bound>,
        pipe: Pipe,
        setup_packet: Option<KBox<CtrlRequest>>,
        transfer_buffer: Option<KBox<[u8]>>,
        context_data: Option<Arc<T>>,
        complete: fn(UrbResult<'_, T>),
        number_of_packets: u32,
        iso_packet_len: u16,
        transfer_flags: TransferFlags,
        interval: i32,
    ) -> Result<Pin<UrbHandle<T, Idle>>> {
        // SAFETY: `usb_alloc_urb` allocates a `struct urb` + ISO frame.
        let urb_ptr =
            unsafe { bindings::usb_alloc_urb(number_of_packets as c_int, mem_flags.as_raw()) };
        if urb_ptr.is_null() {
            return Err(ENOMEM);
        }

        // SAFETY: `urb_ptr` points to allocated and zero-initialized memory
        // of the correct layout for `struct urb` + ISO tail.
        let urb = unsafe { &mut *urb_ptr };

        let dev: &Device<device::Bound> = intf.as_ref();

        urb.complete = Some(urb_complete_trampoline::<T>);
        urb.dev = dev.as_raw();
        urb.pipe = pipe.0;
        urb.number_of_packets = number_of_packets as c_int;
        urb.transfer_flags = u32::from(transfer_flags);
        urb.interval = interval;

        // Set up ISO frame descriptors.
        if number_of_packets > 0 {
            // SAFETY: `urb_ptr` was allocated with `number_of_packets` ISO
            // descriptors via `usb_alloc_urb`. `as_mut_slice` yields a valid
            // mutable slice of that length.
            let descs = unsafe { urb.iso_frame_desc.as_mut_slice(number_of_packets as usize) };
            for (i, desc) in descs.iter_mut().enumerate() {
                let pkt_len = u32::from(iso_packet_len);
                desc.offset = (i as u32) * pkt_len;
                desc.length = pkt_len;
            }
        }

        if let Some(sp) = setup_packet {
            urb.setup_packet = KBox::into_raw(sp).cast::<u8>();
        }

        if let Some(tb) = transfer_buffer {
            let len = tb.len();
            urb.transfer_buffer_length = len as u32;
            urb.transfer_buffer = KBox::into_raw(tb).cast::<core::ffi::c_void>();
        }

        if let Some(data) = context_data {
            urb.context = Arc::into_raw(data).cast_mut().cast();
        }

        urb.rust_complete = complete as *mut core::ffi::c_void;

        let urb_handle = UrbHandle {
            // SAFETY: `urb_ptr` is guaranteed non-null by the null check above.
            urb: unsafe { NonNull::new_unchecked(urb_ptr) },
            _state: PhantomData,
            _ty: PhantomData,
        };

        // SAFETY: `urb_handle.urb` is never moved.
        Ok(unsafe { Pin::new_unchecked(urb_handle) })
    }

    /// Submit the URB for execution.
    ///
    /// On success the caller receives an [`UrbHandle<T, Active>`] which
    /// holds the resources for the in-flight URB. Dropping it cancels the
    /// URB and frees the allocation.
    pub fn submit(
        self: Pin<UrbHandle<T, Idle>>,
        mem_flags: kernel::alloc::Flags,
    ) -> Result<UrbHandle<T, Active>> {
        // SAFETY: The urb pointed to is not moved.
        let handle = unsafe { Pin::into_inner_unchecked(self) };
        // SAFETY: `handle.as_raw()` points to a valid, initialized `struct urb`.
        let result = unsafe { bindings::usb_submit_urb(handle.as_raw(), mem_flags.as_raw()) };

        if result == 0 {
            let urb = handle.urb;
            core::mem::forget(handle);
            Ok(UrbHandle {
                urb,
                _state: PhantomData,
                _ty: PhantomData,
            })
        } else {
            Err(Error::from_errno(result))
        }
    }

    /// Creates a new bulk URB.
    pub fn new_bulk(
        mem_flags: kernel::alloc::Flags,
        intf: &Interface<device::Bound>,
        pipe: Pipe,
        transfer_buffer: KBox<[u8]>,
        context_data: Option<Arc<T>>,
        complete: fn(UrbResult<'_, T>),
        transfer_flags: TransferFlags,
    ) -> Result<Pin<UrbHandle<T, Idle>>> {
        Self::init_common(
            mem_flags,
            intf,
            pipe,
            None,
            Some(transfer_buffer),
            context_data,
            complete,
            0,
            0,
            transfer_flags,
            0,
        )
    }

    /// Creates a new interrupt URB.
    #[allow(clippy::too_many_arguments)]
    pub fn new_int(
        mem_flags: kernel::alloc::Flags,
        intf: &Interface<device::Bound>,
        pipe: Pipe,
        transfer_buffer: KBox<[u8]>,
        context_data: Option<Arc<T>>,
        complete: fn(UrbResult<'_, T>),
        transfer_flags: TransferFlags,
        interval: i32,
    ) -> Result<Pin<UrbHandle<T, Idle>>> {
        Self::init_common(
            mem_flags,
            intf,
            pipe,
            None,
            Some(transfer_buffer),
            context_data,
            complete,
            0,
            0,
            transfer_flags,
            interval,
        )
    }

    /// Creates a new control URB.
    #[allow(clippy::too_many_arguments)]
    pub fn new_ctrl(
        mem_flags: kernel::alloc::Flags,
        intf: &Interface<device::Bound>,
        pipe: Pipe,
        setup_packet: KBox<CtrlRequest>,
        transfer_buffer: Option<KBox<[u8]>>,
        context_data: Option<Arc<T>>,
        complete: fn(UrbResult<'_, T>),
        transfer_flags: TransferFlags,
    ) -> Result<Pin<UrbHandle<T, Idle>>> {
        Self::init_common(
            mem_flags,
            intf,
            pipe,
            Some(setup_packet),
            transfer_buffer,
            context_data,
            complete,
            0,
            0,
            transfer_flags,
            0,
        )
    }

    /// Creates a new isochronous URB.
    #[allow(clippy::too_many_arguments)]
    pub fn new_isoc(
        mem_flags: kernel::alloc::Flags,
        intf: &Interface<device::Bound>,
        pipe: Pipe,
        transfer_buffer: KBox<[u8]>,
        context_data: Option<Arc<T>>,
        complete: fn(UrbResult<'_, T>),
        number_of_packets: u32,
        iso_packet_len: u16,
        transfer_flags: TransferFlags,
        interval: i32,
    ) -> Result<Pin<UrbHandle<T, Idle>>> {
        // Reject URBs whose buffer is too small to hold all packets.
        let needed = (number_of_packets as usize).saturating_mul(iso_packet_len as usize);
        if transfer_buffer.len() < needed {
            return Err(EINVAL);
        }

        Self::init_common(
            mem_flags,
            intf,
            pipe,
            None,
            Some(transfer_buffer),
            context_data,
            complete,
            number_of_packets,
            iso_packet_len,
            transfer_flags,
            interval,
        )
    }
}

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

    fn inner(&self) -> &bindings::usb_device {
        // SAFETY: The type invariants guarantee that `self.0` wraps a valid
        // `struct usb_device`.
        unsafe { &*self.as_raw() }
    }

    /// Returns the USB device number assigned by the bus.
    fn devnum(&self) -> u32 {
        self.inner().devnum as u32
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

    /// Send a USB control message synchronously.
    ///
    /// Wraps `usb_control_msg`. The pipe direction is inferred from the setup
    /// packet's [`Direction`]. The optional `data` buffer is
    /// written to for IN transfers or read from for OUT transfers.
    ///
    /// Returns the number of bytes transferred on success.
    pub fn control_msg(
        &self,
        setup: &CtrlRequest,
        data: Option<&mut [u8]>,
        timeout: Delta,
    ) -> Result<i32> {
        let pipe = match setup.direction() {
            Direction::In => Pipe::new_receive_control_pipe(self),
            Direction::Out => Pipe::new_send_control_pipe(self),
        };
        let (buf, len) = match data {
            Some(d) => (d.as_mut_ptr().cast::<core::ffi::c_void>(), d.len() as u16),
            None => (ptr::null_mut(), 0),
        };
        let timeout_ms = timeout.as_millis() as i32;

        // SAFETY: `self.as_raw()` returns a valid `struct usb_device` pointer.
        let ret = unsafe {
            bindings::usb_control_msg(
                self.as_raw(),
                pipe.0,
                setup.request(),
                setup.requesttype(),
                setup.value(),
                setup.index(),
                buf,
                len,
                timeout_ms,
            )
        };

        if ret >= 0 {
            Ok(ret)
        } else {
            Err(Error::from_errno(ret))
        }
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
