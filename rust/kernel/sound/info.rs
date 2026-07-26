// SPDX-License-Identifier: GPL-2.0

//! ALSA procfs (snd_info) text-mode bindings.
//!
//! Provides [`InfoBuffer`] for use in proc read/write callbacks, the
//! [`TextOps`] trait that drivers implement, and [`TextOpsTable`]
//! which holds the static C-callable trampolines.
//!
//! Register an entry with [`Card::ro_proc_new`] or [`Card::rw_proc_new`],
//! passing an [`Arc`] of the chip data.  The `Arc` is cloned into the entry's
//! `private_data` field and released automatically via `private_free` when the
//! card is freed -- no `unsafe` is required at the call site.
//!
//! C header: [`include/sound/info.h`](srctree/include/sound/info.h)

use crate::{bindings, error::to_result, prelude::*, sync::Arc};
use core::marker::PhantomData;
use super::card::Card;

//
// InfoBuffer
//
/// Wraps `struct snd_info_buffer` for text-mode proc callbacks.
///
/// Implements [`core::fmt::Write`] so drivers can use [`write!`] /
/// [`writeln!`] to emit text in a proc read callback.  For write callbacks
/// use [`InfoBuffer::get_line`] and [`InfoBuffer::get_str`].
pub struct InfoBuffer(*mut bindings::snd_info_buffer);

impl InfoBuffer {
    /// Wraps a raw `snd_info_buffer` pointer.
    ///
    /// # Safety
    ///
    /// `ptr` must be a valid, non-null pointer to a `snd_info_buffer` that
    /// remains valid for the lifetime of the returned [`InfoBuffer`].
    pub(crate) unsafe fn from_raw(ptr: *mut bindings::snd_info_buffer) -> Self {
        Self(ptr)
    }

    /// In text mode the `buffer` field is a `struct seq_file *` (the macro
    /// `snd_iprintf` casts it explicitly).
    fn seq_file(&self) -> *mut bindings::seq_file {
        // SAFETY: snd_info_buffer.buffer is a seq_file* when content == TEXT.
        unsafe { (*self.0).buffer as *mut bindings::seq_file }
    }

    /// Read one line from the proc write buffer (write callbacks only).
    ///
    /// Fills `line` with the next NUL-terminated line.  Returns `true` if a
    /// line was read, `false` at end-of-input.
    pub fn get_line(&mut self, line: &mut [u8]) -> bool {
        // SAFETY: self.0 is valid; line is a writable slice of the given length.
        let ret = unsafe {
            bindings::snd_info_get_line(
                self.0,
                line.as_mut_ptr(),
                line.len() as core::ffi::c_int,
            )
        };
        ret == 0
    }

    /// Parse the first whitespace-delimited token from `src` into `dest`.
    ///
    /// Returns a subslice of `src` starting just after the consumed token.
    pub fn get_str<'a>(&self, dest: &mut [u8], src: &'a [u8]) -> &'a [u8] {
        // SAFETY: dest and src are valid slices; snd_info_get_str returns a
        // pointer into src.
        let rest = unsafe {
            bindings::snd_info_get_str(
                dest.as_mut_ptr(),
                src.as_ptr(),
                dest.len() as core::ffi::c_int,
            )
        };
        let offset = unsafe { rest.offset_from(src.as_ptr()) };
        &src[offset.max(0) as usize..]
    }
}

impl core::fmt::Write for InfoBuffer {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        // SAFETY: seq_file is valid; s.as_ptr()/s.len() are a valid slice.
        unsafe {
            bindings::seq_write(
                self.seq_file(),
                s.as_ptr() as *const core::ffi::c_void,
                s.len(),
            );
        }
        Ok(())
    }
}

//
// TextOps trait + static dispatch table
//
/// Driver callbacks for a text-mode ALSA proc entry.
///
/// Implement this on the chip struct and register the entry with
/// [`Card::ro_proc_new`] or [`Card::rw_proc_new`].
pub trait TextOps: Sync {
    /// Called when userspace reads the proc file.
    fn read(&self, buf: &mut InfoBuffer);

    /// Called when userspace writes to the proc file (rw entries only).
    fn write(&self, _buf: &mut InfoBuffer) {}
}

/// Static dispatch table for [`TextOps`].
///
/// Create one static instance per chip type and pass it to
/// [`Card::ro_proc_new`] / [`Card::rw_proc_new`]:
///
/// ```ignore
/// static MY_PROC_OPS: TextOpsTable<MyChip> = TextOpsTable::new();
/// card.ro_proc_new(c"mydev", chip_arc.clone(), &MY_PROC_OPS)?;
/// ```
pub struct TextOpsTable<T: TextOps>(PhantomData<T>);

// SAFETY: PhantomData<T> is zero-sized; the table itself holds no data.
unsafe impl<T: TextOps> Sync for TextOpsTable<T> {}

impl<T: TextOps> TextOpsTable<T> {
    /// Creates a new table.  `const fn` so it can be used in `static` items.
    pub const fn new() -> Self {
        Self(PhantomData)
    }

    /// C-callable read trampoline.
    ///
    /// # Safety
    ///
    /// Called by the ALSA core with valid `entry` and `buf`.
    /// `entry->private_data` must be a `*const T` obtained from
    /// [`Arc::into_raw`] and still valid (i.e., the Arc has not been dropped).
    pub unsafe extern "C" fn c_read(
        entry: *mut bindings::snd_info_entry,
        buf: *mut bindings::snd_info_buffer,
    ) {
        let data = unsafe { (*entry).private_data as *const T };
        T::read(unsafe { &*data }, unsafe { &mut InfoBuffer::from_raw(buf) });
    }

    /// C-callable write trampoline.
    ///
    /// # Safety
    ///
    /// Same preconditions as [`c_read`](Self::c_read).
    pub unsafe extern "C" fn c_write(
        entry: *mut bindings::snd_info_entry,
        buf: *mut bindings::snd_info_buffer,
    ) {
        let data = unsafe { (*entry).private_data as *const T };
        T::write(unsafe { &*data }, unsafe { &mut InfoBuffer::from_raw(buf) });
    }
}

//
// Arc lifetime management via private_free
//
/// `private_free` callback: reclaims the [`Arc`] stored in `entry->private_data`.
///
/// # Safety
///
/// Must only be used as `private_free` when `entry->private_data` was set
/// from `Arc::<T>::into_raw()`.
unsafe extern "C" fn arc_private_free<T>(entry: *mut bindings::snd_info_entry) {
    // SAFETY: private_data was set from Arc::into_raw(); we are the unique
    // owner of this reference count increment.
    let _ = unsafe { Arc::from_raw((*entry).private_data as *const T) };
}

//
// Card registration helpers (safe for callers)
//
impl Card {
    /// Registers a read-only text proc entry for this card.
    ///
    /// Clones an [`Arc`] reference into the entry's `private_data` and sets
    /// `private_free` to drop it when the card is freed.  No `unsafe` is
    /// required at the call site.
    ///
    /// The entry is automatically removed when the card is freed.
    pub fn ro_proc_new<T>(
        &self,
        name: &CStr,
        data: Arc<T>,
        _table: &'static TextOpsTable<T>,
    ) -> Result
    where
        T: TextOps + Sync + Send + 'static,
    {
        self.proc_new_impl(name, data, Some(TextOpsTable::<T>::c_read), None)
    }

    /// Registers a read-write text proc entry for this card.
    ///
    /// Same as [`ro_proc_new`](Self::ro_proc_new) but also registers the
    /// write callback.
    pub fn rw_proc_new<T>(
        &self,
        name: &CStr,
        data: Arc<T>,
        _table: &'static TextOpsTable<T>,
    ) -> Result
    where
        T: TextOps + Sync + Send + 'static,
    {
        self.proc_new_impl(name, data, Some(TextOpsTable::<T>::c_read), Some(TextOpsTable::<T>::c_write))
    }

    /// Common implementation for [`ro_proc_new`] and [`rw_proc_new`].
    fn proc_new_impl<T>(
        &self,
        name: &CStr,
        data: Arc<T>,
        read: Option<unsafe extern "C" fn(*mut bindings::snd_info_entry, *mut bindings::snd_info_buffer)>,
        write: Option<unsafe extern "C" fn(*mut bindings::snd_info_entry, *mut bindings::snd_info_buffer)>,
    ) -> Result
    where
        T: TextOps + Sync + Send + 'static,
    {
        // Create the entry under card->proc_root.  It is automatically added
        // to the card's proc child list and freed when the card is freed.
        // SAFETY: self.as_raw() is valid; name is a valid C string;
        // (*self.as_raw()).proc_root is the card's proc directory entry.
        let entry = unsafe {
            bindings::snd_info_create_card_entry(
                self.as_raw(),
                name.as_char_ptr(),
                (*self.as_raw()).proc_root,
            )
        };
        if entry.is_null() {
            return Err(ENOMEM);
        }

        // Convert Arc to a raw pointer (leaks one reference count increment).
        // The reference will be reclaimed by arc_private_free when the entry
        // is freed (either on snd_info_register failure path via card cleanup,
        // or on normal card removal).
        let ptr = Arc::into_raw(data);

        // SAFETY: entry is non-null and freshly created; we set all the
        // relevant fields before registering.  The union is in text content
        // mode (SNDRV_INFO_CONTENT_TEXT = 0, the default).
        unsafe {
            (*entry).private_data = ptr as *mut core::ffi::c_void;
            (*entry).private_free = Some(arc_private_free::<T>);
            (*entry).c.text.read = read;
            (*entry).c.text.write = write;
        }

        // Register the entry with the proc filesystem.  On failure the entry
        // remains in the card's child list; arc_private_free will be called
        // via the card's own cleanup.
        to_result(unsafe { bindings::snd_info_register(entry) })
    }
}
