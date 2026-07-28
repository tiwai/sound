// SPDX-License-Identifier: GPL-2.0

//! Port I/O (PIO) — abstractions for inb/outb-style port access.
//!
//! C header: [`include/asm-generic/io.h`](srctree/include/asm-generic/io.h)

use crate::bindings;
use super::{Io, IoCapable, IoKnownSize};

/// A port I/O region.
///
/// `SIZE` is the size of the port range in bytes. If `SIZE > 0`, the infallible
/// [`Io::read`] / [`Io::write`] family is available with compile-time bounds
/// checking; otherwise only the fallible [`Io::try_read`] / [`Io::try_write`]
/// family is accessible.
///
/// # Invariant
///
/// `base` is the first port number of a valid, allocated I/O port range of at
/// least `SIZE` bytes.
pub struct Pio<const SIZE: usize = 0> {
    base: u32,
}

impl<const SIZE: usize> Pio<SIZE> {
    /// Creates a [`Pio`] instance wrapping the port range `[base, base + SIZE)`.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `[base, base + SIZE)` is a valid I/O port
    /// range that has been allocated (e.g. via `pci_request_regions` or
    /// `request_region`), and that this range remains valid for the entire
    /// lifetime of the returned [`Pio`] value.
    pub unsafe fn new(base: u32) -> Self {
        Self { base }
    }
}

impl<const SIZE: usize> Io for Pio<SIZE> {
    #[inline]
    fn addr(&self) -> usize {
        self.base as usize
    }

    #[inline]
    fn maxsize(&self) -> usize {
        SIZE
    }
}

impl<const SIZE: usize> IoKnownSize for Pio<SIZE> {
    const MIN_SIZE: usize = SIZE;
}

impl<const SIZE: usize> IoCapable<u8> for Pio<SIZE> {
    unsafe fn io_read(&self, address: usize) -> u8 {
        // SAFETY: By the trait invariant `address` is a valid port I/O address.
        // Port numbers fit in u32 on all supported architectures.
        unsafe { bindings::inb(address as u32) }
    }

    unsafe fn io_write(&self, value: u8, address: usize) {
        // SAFETY: By the trait invariant `address` is a valid port I/O address.
        unsafe { bindings::outb(value, address as u32) }
    }
}

impl<const SIZE: usize> IoCapable<u16> for Pio<SIZE> {
    unsafe fn io_read(&self, address: usize) -> u16 {
        // SAFETY: By the trait invariant `address` is a valid port I/O address.
        unsafe { bindings::inw(address as u32) }
    }

    unsafe fn io_write(&self, value: u16, address: usize) {
        // SAFETY: By the trait invariant `address` is a valid port I/O address.
        unsafe { bindings::outw(value, address as u32) }
    }
}

impl<const SIZE: usize> IoCapable<u32> for Pio<SIZE> {
    unsafe fn io_read(&self, address: usize) -> u32 {
        // SAFETY: By the trait invariant `address` is a valid port I/O address.
        unsafe { bindings::inl(address as u32) }
    }

    unsafe fn io_write(&self, value: u32, address: usize) {
        // SAFETY: By the trait invariant `address` is a valid port I/O address.
        unsafe { bindings::outl(value, address as u32) }
    }
}
