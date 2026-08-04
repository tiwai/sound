// SPDX-License-Identifier: GPL-2.0

//! Abstractions for USB chapter 9.
//!
//! C header: [`include/linux/usb/ch9.h`](srctree/include/linux/usb/ch9.h)

use crate::fmt;

/// USB interface class code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct InterfaceClass(u8);

impl InterfaceClass {
    /// Create an [`InterfaceClass`] from a raw `u8` class code.
    pub const fn from_raw(class: u8) -> Self {
        Self(class)
    }

    /// Get the raw `u8` class code value.
    pub const fn as_raw(self) -> u8 {
        self.0
    }
}

macro_rules! define_all_usb_classes {
    (
        $($variant:ident = $binding:expr,)+
    ) => {
        impl InterfaceClass {
            $(
                #[allow(missing_docs)]
                pub const $variant: Self = Self($binding as u8);
            )+
        }

        impl fmt::Display for InterfaceClass {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                match self {
                    $(
                        &Self::$variant => write!(f, stringify!($variant)),
                    )+
                    _ => <Self as fmt::Debug>::fmt(self, f),
                }
            }
        }
    };
}

define_all_usb_classes! {
    PER_INTERFACE           = bindings::USB_CLASS_PER_INTERFACE,
    AUDIO                   = bindings::USB_CLASS_AUDIO,
    COMM                    = bindings::USB_CLASS_COMM,
    HID                     = bindings::USB_CLASS_HID,
    PHYSICAL                = bindings::USB_CLASS_PHYSICAL,
    STILL_IMAGE             = bindings::USB_CLASS_STILL_IMAGE,
    PRINTER                 = bindings::USB_CLASS_PRINTER,
    MASS_STORAGE            = bindings::USB_CLASS_MASS_STORAGE,
    HUB                     = bindings::USB_CLASS_HUB,
    CDC_DATA                = bindings::USB_CLASS_CDC_DATA,
    CSCID                   = bindings::USB_CLASS_CSCID,
    CONTENT_SEC             = bindings::USB_CLASS_CONTENT_SEC,
    VIDEO                   = bindings::USB_CLASS_VIDEO,
    WIRELESS_CONTROLLER     = bindings::USB_CLASS_WIRELESS_CONTROLLER,
    PERSONAL_HEALTHCARE     = bindings::USB_CLASS_PERSONAL_HEALTHCARE,
    AUDIO_VIDEO             = bindings::USB_CLASS_AUDIO_VIDEO,
    BILLBOARD               = bindings::USB_CLASS_BILLBOARD,
    USB_TYPE_C_BRIDGE       = bindings::USB_CLASS_USB_TYPE_C_BRIDGE,
    MCTP                    = bindings::USB_CLASS_MCTP,
    MISC                    = bindings::USB_CLASS_MISC,
    APP_SPEC                = bindings::USB_CLASS_APP_SPEC,
    VENDOR_SPEC             = bindings::USB_CLASS_VENDOR_SPEC,
}

/// USB interface descriptor.
///
/// Wraps the C `struct usb_interface_descriptor` defined in
/// `include/uapi/linux/usb/ch9.h`. Corresponds to USB 2.0 spec §9.6.5,
/// table 9-12.
#[repr(transparent)]
pub struct InterfaceDescriptor(bindings::usb_interface_descriptor);

impl InterfaceDescriptor {
    /// Returns the size of this descriptor in bytes.
    #[allow(non_snake_case)]
    pub fn bLength(&self) -> u8 {
        self.0.bLength
    }

    /// Returns the descriptor type (`USB_DT_INTERFACE`).
    #[allow(non_snake_case)]
    pub fn bDescriptorType(&self) -> u8 {
        self.0.bDescriptorType
    }

    /// Returns the interface number (zero-based).
    #[allow(non_snake_case)]
    pub fn bInterfaceNumber(&self) -> u8 {
        self.0.bInterfaceNumber
    }

    /// Returns the alternate setting number.
    #[allow(non_snake_case)]
    pub fn bAlternateSetting(&self) -> u8 {
        self.0.bAlternateSetting
    }

    /// Returns the number of endpoints used by this interface (excluding
    /// the default control endpoint).
    #[allow(non_snake_case)]
    pub fn bNumEndpoints(&self) -> u8 {
        self.0.bNumEndpoints
    }

    /// Returns the interface class code.
    #[allow(non_snake_case)]
    pub fn bInterfaceClass(&self) -> InterfaceClass {
        InterfaceClass(self.0.bInterfaceClass)
    }

    /// Returns the interface subclass code.
    #[allow(non_snake_case)]
    pub fn bInterfaceSubClass(&self) -> u8 {
        self.0.bInterfaceSubClass
    }

    /// Returns the interface protocol code.
    #[allow(non_snake_case)]
    pub fn bInterfaceProtocol(&self) -> u8 {
        self.0.bInterfaceProtocol
    }

    /// Returns the index of the string descriptor describing this
    /// interface.
    #[allow(non_snake_case)]
    pub fn iInterface(&self) -> u8 {
        self.0.iInterface
    }
}

/// USB endpoint descriptor.
///
/// Wraps the C `struct usb_endpoint_descriptor` defined in
/// `include/uapi/linux/usb/ch9.h`. Corresponds to USB 2.0 spec §9.6.6,
/// table 9-13.
#[repr(transparent)]
pub struct EndpointDescriptor(bindings::usb_endpoint_descriptor);

impl EndpointDescriptor {
    /// Returns the endpoint address (direction + endpoint number).
    #[allow(non_snake_case)]
    pub fn bEndpointAddress(&self) -> u8 {
        self.0.bEndpointAddress
    }

    /// Returns the endpoint attributes (transfer type).
    #[allow(non_snake_case)]
    pub fn bmAttributes(&self) -> u8 {
        self.0.bmAttributes
    }

    /// Returns the maximum packet size for this endpoint.
    #[allow(non_snake_case)]
    pub fn wMaxPacketSize(&self) -> u16 {
        self.0.wMaxPacketSize
    }

    /// Returns the interval for isochronous/interrupt endpoints.
    #[allow(non_snake_case)]
    pub fn bInterval(&self) -> u8 {
        self.0.bInterval
    }
}

/// USB interface association descriptor.
///
/// Wraps the C `struct usb_interface_assoc_descriptor`. Corresponds to
/// USB ECN: Interface Association Descriptor.
#[repr(transparent)]
pub struct InterfaceAssociationDescriptor(bindings::usb_interface_assoc_descriptor);

impl InterfaceAssociationDescriptor {
    /// Returns the first interface number of the associated interfaces.
    #[allow(non_snake_case)]
    pub fn bFirstInterface(&self) -> u8 {
        self.0.bFirstInterface
    }

    /// Returns the number of contiguous interfaces associated with this function.
    #[allow(non_snake_case)]
    pub fn bInterfaceCount(&self) -> u8 {
        self.0.bInterfaceCount
    }

    /// Returns the class code.
    #[allow(non_snake_case)]
    pub fn bFunctionClass(&self) -> u8 {
        self.0.bFunctionClass
    }

    /// Returns the subclass code.
    #[allow(non_snake_case)]
    pub fn bFunctionSubClass(&self) -> u8 {
        self.0.bFunctionSubClass
    }

    /// Returns the protocol code.
    #[allow(non_snake_case)]
    pub fn bFunctionProtocol(&self) -> u8 {
        self.0.bFunctionProtocol
    }
}

/// USB control request (SETUP packet).
///
/// Wraps the C `struct usb_ctrlrequest` defined in
/// `include/uapi/linux/usb/ch9.h`. Corresponds to USB 2.0 spec §9.3,
/// table 9-2.
#[repr(transparent)]
pub struct CtrlRequest(bindings::usb_ctrlrequest);

impl CtrlRequest {
    /// Creates a new control request from its constituent fields.
    pub const fn new(
        requesttype: RequestType,
        request: u8,
        value: u16,
        index: u16,
        length: u16,
    ) -> Self {
        Self(bindings::usb_ctrlrequest {
            bRequestType: requesttype.0,
            bRequest: request,
            wValue: value.to_le(),
            wIndex: index.to_le(),
            wLength: length.to_le(),
        })
    }

    /// Returns the data-transfer direction encoded in the setup packet.
    pub fn direction(&self) -> Direction {
        if self.requesttype() & Direction::In as u8 == 0 {
            Direction::Out
        } else {
            Direction::In
        }
    }

    /// Returns the `bRequestType` field.
    pub fn requesttype(&self) -> u8 {
        self.0.bRequestType
    }

    /// Returns the `bRequest` field.
    pub fn request(&self) -> u8 {
        self.0.bRequest
    }

    /// Returns the `wValue` field (native endian).
    pub fn value(&self) -> u16 {
        u16::from_le(self.0.wValue)
    }

    /// Returns the `wIndex` field (native endian).
    pub fn index(&self) -> u16 {
        u16::from_le(self.0.wIndex)
    }

    /// Returns the `wLength` field (native endian).
    pub fn length(&self) -> u16 {
        u16::from_le(self.0.wLength)
    }
}

/// USB data transfer direction for a control request.
///
/// Used in the `bRequestType` field of a SETUP packet
/// (USB 2.0 spec §9.3, table 9-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Direction {
    /// Host-to-device.
    Out = bindings::USB_DIR_OUT as u8,
    /// Device-to-host.
    In = bindings::USB_DIR_IN as u8,
}

/// USB request type for a control request.
///
/// Used in the `bmRequestType` field of a SETUP packet to distinguish
/// standard, class, and vendor-specific requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Type {
    /// Standard request defined by the USB specification.
    Standard = bindings::USB_TYPE_STANDARD as u8,
    /// Class-specific request defined by a USB class specification.
    Class = bindings::USB_TYPE_CLASS as u8,
    /// Vendor-specific request.
    Vendor = bindings::USB_TYPE_VENDOR as u8,
    /// Reserved for future use.
    Reserved = bindings::USB_TYPE_RESERVED as u8,
}

/// USB setup packet request type (`bmRequestType`).
///
/// Encodes the direction, type, and recipient of a control request.
pub struct RequestType(u8);

/// USB request recipient for a control request.
///
/// Used in the `bmRequestType` field of a SETUP packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Recipient {
    /// Recipient is the device.
    Device = bindings::USB_RECIP_DEVICE as u8,
    /// Recipient is an interface.
    Interface = bindings::USB_RECIP_INTERFACE as u8,
    /// Recipient is an endpoint.
    Endpoint = bindings::USB_RECIP_ENDPOINT as u8,
    /// None of the above.
    Other = bindings::USB_RECIP_OTHER as u8,
}

impl RequestType {
    /// Creates a [`RequestType`] from a direction, type, and recipient.
    ///
    /// The three fields are packed into a single `u8` per the USB
    /// specification (USB 2.0 spec §9.3, table 9-2).
    pub const fn new(dir: Direction, r#type: Type, recipient: Recipient) -> Self {
        Self(dir as u8 | r#type as u8 | recipient as u8)
    }
}
