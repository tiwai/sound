// SPDX-License-Identifier: GPL-2.0

//! PCM (digital audio) abstraction.
//!
//! C header: [`include/sound/pcm.h`](srctree/include/sound/pcm.h)

use crate::{
    bindings,
    error::to_result,
    prelude::*,
    str::CStr,
    sync::{atomic::{ordering, Atomic}},
    types::Opaque,
};
use super::card::Card;

use core::marker::PhantomData;

/// PCM stream direction.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum StreamDir {
    /// Playback stream (audio output).
    Playback,
    /// Capture stream (audio input).
    Capture,
}

impl StreamDir {
    fn as_c_int(self) -> core::ffi::c_int {
        match self {
            Self::Playback => bindings::SNDRV_PCM_STREAM_PLAYBACK as _,
            Self::Capture => bindings::SNDRV_PCM_STREAM_CAPTURE as _,
        }
    }

    fn from_c(v: core::ffi::c_int) -> Self {
        if v == bindings::SNDRV_PCM_STREAM_CAPTURE as core::ffi::c_int {
            Self::Capture
        } else {
            Self::Playback
        }
    }
}

/// PCM trigger command.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TriggerCommand {
    /// Start the stream.
    Start,
    /// Stop the stream.
    Stop,
    /// Pause the stream (push state).
    PausePush,
    /// Resume from pause.
    PauseRelease,
    /// Suspend (system sleep).
    Suspend,
    /// Resume from suspend.
    Resume,
    /// Drain (wait for playback to finish).
    Drain,
    /// Unknown command value.
    Unknown(i32),
}

impl TriggerCommand {
    fn from_c(cmd: core::ffi::c_int) -> Self {
        match cmd {
            x if x == bindings::SNDRV_PCM_TRIGGER_START as i32 => Self::Start,
            x if x == bindings::SNDRV_PCM_TRIGGER_STOP as i32 => Self::Stop,
            x if x == bindings::SNDRV_PCM_TRIGGER_PAUSE_PUSH as i32 => Self::PausePush,
            x if x == bindings::SNDRV_PCM_TRIGGER_PAUSE_RELEASE as i32 => Self::PauseRelease,
            x if x == bindings::SNDRV_PCM_TRIGGER_SUSPEND as i32 => Self::Suspend,
            x if x == bindings::SNDRV_PCM_TRIGGER_RESUME as i32 => Self::Resume,
            x if x == bindings::SNDRV_PCM_TRIGGER_DRAIN as i32 => Self::Drain,
            x => Self::Unknown(x),
        }
    }
}

/// Hardware capabilities reported by the driver in the `open` callback.
///
/// Mirrors `struct snd_pcm_hardware`.
#[derive(Copy, Clone)]
pub struct Hardware {
    /// `SNDRV_PCM_INFO_*` flags.
    pub info: u32,
    /// Supported formats bitmask (`SNDRV_PCM_FMTBIT_*`).
    pub formats: u64,
    /// Supported rates bitmask (`SNDRV_PCM_RATE_*`).
    pub rates: u32,
    /// Minimum sample rate in Hz.
    pub rate_min: u32,
    /// Maximum sample rate in Hz.
    pub rate_max: u32,
    /// Minimum number of channels.
    pub channels_min: u32,
    /// Maximum number of channels.
    pub channels_max: u32,
    /// Maximum DMA buffer size in bytes.
    pub buffer_bytes_max: usize,
    /// Minimum period size in bytes.
    pub period_bytes_min: usize,
    /// Maximum period size in bytes.
    pub period_bytes_max: usize,
    /// Minimum number of periods.
    pub periods_min: u32,
    /// Maximum number of periods.
    pub periods_max: u32,
    /// FIFO size in bytes (0 if none).
    pub fifo_size: usize,
}

impl Hardware {
    fn to_c(&self) -> bindings::snd_pcm_hardware {
        bindings::snd_pcm_hardware {
            info: self.info,
            formats: self.formats as _,
            subformats: 0,
            rates: self.rates,
            rate_min: self.rate_min,
            rate_max: self.rate_max,
            channels_min: self.channels_min,
            channels_max: self.channels_max,
            buffer_bytes_max: self.buffer_bytes_max,
            period_bytes_min: self.period_bytes_min,
            period_bytes_max: self.period_bytes_max,
            periods_min: self.periods_min,
            periods_max: self.periods_max,
            fifo_size: self.fifo_size,
        }
    }
}

/// PCM runtime state - valid while the substream is open.
///
/// Wraps `struct snd_pcm_runtime *`.
pub struct Runtime(*mut bindings::snd_pcm_runtime);

impl Runtime {
    fn as_raw(&self) -> *mut bindings::snd_pcm_runtime {
        self.0
    }

    /// Sets the hardware capabilities for this substream (call in `open`).
    pub fn set_hw(&self, hw: &Hardware) {
        // SAFETY: `self.0` is valid during the open callback.
        unsafe { (*self.as_raw()).hw = hw.to_c() };
    }

    /// Returns the negotiated sample format (valid after hw_params).
    pub fn format(&self) -> i32 {
        unsafe { (*self.as_raw()).format }
    }

    /// Returns the negotiated sample rate in Hz (valid after hw_params).
    pub fn rate(&self) -> u32 {
        unsafe { (*self.as_raw()).rate }
    }

    /// Returns the number of channels (valid after hw_params).
    pub fn channels(&self) -> u32 {
        unsafe { (*self.as_raw()).channels }
    }

    /// Returns the period size in frames (valid after hw_params).
    pub fn period_size(&self) -> bindings::snd_pcm_uframes_t {
        unsafe { (*self.as_raw()).period_size }
    }

    /// Returns the buffer size in frames (valid after hw_params).
    pub fn buffer_size(&self) -> bindings::snd_pcm_uframes_t {
        unsafe { (*self.as_raw()).buffer_size }
    }

    /// Returns the DMA buffer virtual address (valid after hw_params).
    pub fn dma_area(&self) -> *mut u8 {
        unsafe { (*self.as_raw()).dma_area as *mut u8 }
    }

    /// Returns the total DMA buffer size in bytes.
    pub fn dma_bytes(&self) -> usize {
        unsafe { (*self.as_raw()).dma_bytes }
    }

    /// Returns the DMA buffer physical address (valid after hw_params).
    pub fn dma_addr(&self) -> u32 {
        unsafe { (*self.as_raw()).dma_addr as u32 }
    }

    /// Returns the width in bits of the negotiated sample format.
    ///
    /// Returns 8 for 8-bit formats, 16 for 16-bit formats, etc.
    pub fn format_width(&self) -> i32 {
        unsafe {
            bindings::snd_pcm_format_width((*self.as_raw()).format)
        }
    }

    /// Returns the raw `*mut snd_pcm_runtime` pointer.
    pub fn as_raw_runtime(&self) -> *mut bindings::snd_pcm_runtime {
        self.as_raw()
    }

    /// Sets per-runtime driver private data with a custom free function.
    ///
    /// # Safety
    ///
    /// `data` must remain valid until `free_fn` is called.
    pub unsafe fn set_private(
        &self,
        data: *mut core::ffi::c_void,
        free_fn: unsafe extern "C" fn(*mut bindings::snd_pcm_runtime),
    ) {
        unsafe {
            (*self.as_raw()).private_data = data;
            (*self.as_raw()).private_free = Some(free_fn);
        }
    }

    /// Returns the per-runtime private data pointer.
    pub fn private_data(&self) -> *mut core::ffi::c_void {
        unsafe { (*self.as_raw()).private_data }
    }

    /// Returns the frame size in bits (channels x sample-width), valid after hw_params.
    pub fn frame_bits(&self) -> u32 {
        unsafe { (*self.as_raw()).frame_bits }
    }

    /// Adds a list-based hardware parameter constraint to a runtime.
    ///
    /// The list must remain valid for the duration of the constraint (static lifetime).
    ///
    /// # Safety
    ///
    /// `constraint` must point to a static (module-lifetime) struct whose embedded
    /// `list` pointer is also static.  The kernel stores `constraint` verbatim as
    /// `rule->private` and dereferences it on every `hw_refine` call; a stack-
    /// allocated struct will dangle after `open()` returns.
    pub unsafe fn hw_constraint_list(
        &self,
        cond: u32,
        var: i32,
        constraint: &'static bindings::snd_pcm_hw_constraint_list,
    ) -> crate::error::Result {
        crate::error::to_result(unsafe {
            bindings::snd_pcm_hw_constraint_list(self.as_raw(), cond, var, constraint)
        })
    }

    /// Adds a ratnum-based hardware parameter constraint to a runtime.
    ///
/// # Safety
    ///
    /// Same lifetime requirement as `hw_constraint_list`: `constraint` and the
    /// `rats` array it points to must both be static.
    pub unsafe fn hw_constraint_ratnums(
        &self,
        cond: u32,
        var: i32,
        constraint: &'static bindings::snd_pcm_hw_constraint_ratnums,
    ) -> crate::error::Result {
        crate::error::to_result(unsafe {
            bindings::snd_pcm_hw_constraint_ratnums(self.as_raw(), cond, var, constraint)
        })
    }
}

/// A PCM substream - represents one open audio stream.
///
/// Wraps `struct snd_pcm_substream *`.
#[repr(transparent)]
pub struct Substream(Opaque<bindings::snd_pcm_substream>);

impl Substream {
    fn as_raw(&self) -> *mut bindings::snd_pcm_substream {
        self.0.get()
    }

    /// Returns the raw substream pointer.
    pub fn as_ptr(&self) -> *mut bindings::snd_pcm_substream {
        self.0.get()
    }

    /// # Safety
    ///
    /// `ptr` must point to a valid, live `snd_pcm_substream`.
    unsafe fn from_raw<'a>(ptr: *mut bindings::snd_pcm_substream) -> &'a Substream {
        unsafe { &*ptr.cast::<Substream>() }
    }

    /// Returns the stream direction.
    pub fn stream(&self) -> StreamDir {
        StreamDir::from_c(unsafe { (*self.as_raw()).stream })
    }

    /// Returns the PCM device index (e.g., 0 for device 0, 1 for device 1).
    pub fn pcm_device(&self) -> i32 {
        unsafe { (*(*self.as_raw()).pcm).device }
    }

    /// Returns the PCM runtime (only valid while the substream is open).
    pub fn runtime(&self) -> Runtime {
        Runtime(unsafe { (*self.as_raw()).runtime })
    }

    /// Notifies ALSA that a PCM period has elapsed.
    pub fn period_elapsed(&self) {
        // SAFETY: self is a valid snd_pcm_substream.
        unsafe { bindings::snd_pcm_period_elapsed(self.as_raw()) }
    }

    /// Returns the raw `private_data` pointer.
    pub fn private_data(&self) -> *mut core::ffi::c_void {
        unsafe { (*self.as_raw()).private_data }
    }
}

/// Implemented by driver types that provide PCM callbacks.
///
/// # Safety
///
/// When `NONATOMIC = false`, `trigger` and `pointer` are called under the PCM
/// stream spinlock and must not sleep. When `NONATOMIC = true`, a mutex is
/// used instead and all callbacks may sleep.
pub trait Ops: Send + Sync {
    /// If `true`, all PCM operations use a mutex (non-atomic mode).
    const NONATOMIC: bool = false;

    /// Called when the substream is opened.
    fn open(&self, substream: &Substream) -> Result {
        let _ = substream;
        Ok(())
    }

    /// Called when the substream is closed.
    fn close(&self, substream: &Substream) -> Result {
        let _ = substream;
        Ok(())
    }

    /// Called to set hardware parameters.
    fn hw_params(
        &self,
        substream: &Substream,
        params: *mut bindings::snd_pcm_hw_params,
    ) -> Result {
        let _ = (substream, params);
        Ok(())
    }

    /// Called to release hardware resources after `hw_params`.
    fn hw_free(&self, substream: &Substream) -> Result {
        let _ = substream;
        Ok(())
    }

    /// Called to prepare the hardware for playback/capture.
    fn prepare(&self, substream: &Substream) -> Result {
        let _ = substream;
        Ok(())
    }

    /// Called to start/stop/pause the hardware.
    ///
    /// Must not sleep when `NONATOMIC = false`.
    fn trigger(&self, substream: &Substream, cmd: TriggerCommand) -> Result;

    /// Returns the current hardware pointer position in frames.
    fn pointer(&self, substream: &Substream) -> bindings::snd_pcm_uframes_t;

    /// Called in process context after `trigger(Stop)` to wait for hardware idle.
    fn sync_stop(&self, substream: &Substream) -> Result {
        let _ = substream;
        Ok(())
    }
}

/// A C-compatible ops table for a `T: Ops` implementation.
///
/// Declare as a `static` for a stable address:
/// ```ignore
/// static MY_OPS: OpsTable<MyChip> = OpsTable::new();
/// ```
pub struct OpsTable<T: Ops> {
    ops: core::cell::UnsafeCell<bindings::snd_pcm_ops>,
    _phantom: PhantomData<T>,
}

// SAFETY: The table holds only C function pointers.
unsafe impl<T: Ops> Send for OpsTable<T> {}
unsafe impl<T: Ops> Sync for OpsTable<T> {}

impl<T: Ops> OpsTable<T> {
    /// Creates a new ops table. Suitable for `static` initialisation.
    pub const fn new() -> Self {
        // SAFETY: zeroed `snd_pcm_ops` is valid (all NULL/0 fields), and we
        // overwrite the fields we care about with valid function pointers.
        let ops = bindings::snd_pcm_ops {
            open: Some(trampoline_open::<T>),
            close: Some(trampoline_close::<T>),
            ioctl: Some(bindings::snd_pcm_lib_ioctl),
            hw_params: Some(trampoline_hw_params::<T>),
            hw_free: Some(trampoline_hw_free::<T>),
            prepare: Some(trampoline_prepare::<T>),
            trigger: Some(trampoline_trigger::<T>),
            sync_stop: Some(trampoline_sync_stop::<T>),
            pointer: Some(trampoline_pointer::<T>),
            get_time_info: None,
            fill_silence: None,
            copy: None,
            page: None,
            mmap: None,
            ack: None,
        };
        Self {
            ops: core::cell::UnsafeCell::new(ops),
            _phantom: PhantomData,
        }
    }

    pub(crate) fn as_ptr(&self) -> *const bindings::snd_pcm_ops {
        self.ops.get() as *const _
    }
}

// Trampolines cast `substream->private_data` to `*const T` and call the trait.
// The driver must set `pcm->private_data` before registering the card.

unsafe extern "C" fn trampoline_open<T: Ops>(
    substream: *mut bindings::snd_pcm_substream,
) -> core::ffi::c_int {
    let sub = unsafe { Substream::from_raw(substream) };
    let chip = unsafe { &*((*substream).private_data as *const T) };
    match chip.open(sub) {
        Ok(()) => 0,
        Err(e) => e.to_errno(),
    }
}

unsafe extern "C" fn trampoline_close<T: Ops>(
    substream: *mut bindings::snd_pcm_substream,
) -> core::ffi::c_int {
    let sub = unsafe { Substream::from_raw(substream) };
    let chip = unsafe { &*((*substream).private_data as *const T) };
    match chip.close(sub) {
        Ok(()) => 0,
        Err(e) => e.to_errno(),
    }
}

unsafe extern "C" fn trampoline_hw_params<T: Ops>(
    substream: *mut bindings::snd_pcm_substream,
    params: *mut bindings::snd_pcm_hw_params,
) -> core::ffi::c_int {
    let sub = unsafe { Substream::from_raw(substream) };
    let chip = unsafe { &*((*substream).private_data as *const T) };
    match chip.hw_params(sub, params) {
        Ok(()) => 0,
        Err(e) => e.to_errno(),
    }
}

unsafe extern "C" fn trampoline_hw_free<T: Ops>(
    substream: *mut bindings::snd_pcm_substream,
) -> core::ffi::c_int {
    let sub = unsafe { Substream::from_raw(substream) };
    let chip = unsafe { &*((*substream).private_data as *const T) };
    match chip.hw_free(sub) {
        Ok(()) => 0,
        Err(e) => e.to_errno(),
    }
}

unsafe extern "C" fn trampoline_prepare<T: Ops>(
    substream: *mut bindings::snd_pcm_substream,
) -> core::ffi::c_int {
    let sub = unsafe { Substream::from_raw(substream) };
    let chip = unsafe { &*((*substream).private_data as *const T) };
    match chip.prepare(sub) {
        Ok(()) => 0,
        Err(e) => e.to_errno(),
    }
}

unsafe extern "C" fn trampoline_trigger<T: Ops>(
    substream: *mut bindings::snd_pcm_substream,
    cmd: core::ffi::c_int,
) -> core::ffi::c_int {
    let sub = unsafe { Substream::from_raw(substream) };
    let chip = unsafe { &*((*substream).private_data as *const T) };
    match chip.trigger(sub, TriggerCommand::from_c(cmd)) {
        Ok(()) => 0,
        Err(e) => e.to_errno(),
    }
}

unsafe extern "C" fn trampoline_sync_stop<T: Ops>(
    substream: *mut bindings::snd_pcm_substream,
) -> core::ffi::c_int {
    let sub = unsafe { Substream::from_raw(substream) };
    let chip = unsafe { &*((*substream).private_data as *const T) };
    match chip.sync_stop(sub) {
        Ok(()) => 0,
        Err(e) => e.to_errno(),
    }
}

unsafe extern "C" fn trampoline_pointer<T: Ops>(
    substream: *mut bindings::snd_pcm_substream,
) -> bindings::snd_pcm_uframes_t {
    let sub = unsafe { Substream::from_raw(substream) };
    let chip = unsafe { &*((*substream).private_data as *const T) };
    chip.pointer(sub)
}

/// A PCM device.
///
/// Wraps `struct snd_pcm`. Owned by the card - do not free explicitly.
#[repr(transparent)]
pub struct Pcm(Opaque<bindings::snd_pcm>);

impl Pcm {
    /// Creates a new PCM device on `card`.
    pub fn new<'a>(
        card: &'a Card,
        id: &CStr,
        device: i32,
        playback_count: i32,
        capture_count: i32,
    ) -> Result<&'a Pcm> {
        let mut pcm_ptr: *mut bindings::snd_pcm = core::ptr::null_mut();

        // SAFETY: `card.as_raw()` is a valid card; `id` is a valid C string.
        to_result(unsafe {
            bindings::snd_pcm_new(
                card.as_raw(),
                id.as_char_ptr(),
                device,
                playback_count,
                capture_count,
                &mut pcm_ptr,
            )
        })?;

        // SAFETY: on success pcm_ptr is a valid non-null snd_pcm owned by the card.
        Ok(unsafe { &*pcm_ptr.cast::<Pcm>() })
    }

    /// Returns the underlying raw `snd_pcm` pointer.
    pub fn as_raw(&self) -> *mut bindings::snd_pcm {
        self.0.get()
    }

    /// Installs a PCM ops table for the given stream direction.
    ///
    /// `table` must remain valid for the lifetime of this PCM device.
    pub fn set_ops<T: Ops>(&self, dir: StreamDir, table: &OpsTable<T>) {
        if T::NONATOMIC {
            unsafe { (*self.as_raw()).nonatomic = true };
        }
        unsafe {
            bindings::snd_pcm_set_ops(self.as_raw(), dir.as_c_int(), table.as_ptr());
        }
    }

    /// Sets `pcm->private_data` - copied to `substream->private_data` at open.
    ///
    /// # Safety
    ///
    /// `data` must point to a valid `T` that remains alive for the lifetime
    /// of all substreams.
    pub unsafe fn set_private_data<T>(&self, data: *const T) {
        unsafe { (*self.as_raw()).private_data = data as *mut core::ffi::c_void };
    }

    /// Sets up managed DMA buffers for all substreams.
    pub fn set_managed_buffer_all(
        &self,
        dma_type: u32,
        dev: *mut bindings::device,
        size: usize,
        max: usize,
    ) -> Result {
        to_result(unsafe {
            bindings::snd_pcm_set_managed_buffer_all(
                self.as_raw(),
                dma_type as i32,
                dev,
                size,
                max,
            )
        })
    }

    /// Convenience: set up continuous (vmalloc-based) managed buffers.
    pub fn set_managed_buffer_continuous(&self, size: usize) -> Result {
        self.set_managed_buffer_all(
            bindings::SNDRV_DMA_TYPE_CONTINUOUS,
            core::ptr::null_mut(),
            size,
            size,
        )
    }

    /// Set up DMA-mapped managed buffers using the given device (for PCI drivers).
    pub fn set_managed_buffer_dev<Ctx: super::super::device::DeviceContext>(
        &self,
        dev: &super::super::device::Device<Ctx>,
        size: usize,
        max: usize,
    ) -> Result {
        self.set_managed_buffer_all(bindings::SNDRV_DMA_TYPE_DEV, dev.as_raw(), size, max)
    }
}

/// Returns the name of a PCM sample format as a static C string.
pub fn format_name(fmt: i32) -> &'static CStr {
    // SAFETY: `snd_pcm_format_name` always returns a valid, statically-allocated,
    // NUL-terminated C string for any format value, including unknown ones.
    unsafe { CStr::from_char_ptr(bindings::snd_pcm_format_name(fmt as bindings::snd_pcm_format_t)) }
}

/// Non-owning handle to a PCM substream, for spinlock-guarded interrupt context.
///
/// Drivers that store a substream pointer under a spinlock and need to call
/// [`period_elapsed`][Self::period_elapsed] after releasing the lock should use
/// this type instead of a raw pointer.  The null state represents "no active
/// stream".
///
/// # Invariants
///
/// The stored pointer is either null or was derived from `&PcmSubstream` during
/// an `open` callback.  ALSA guarantees the substream object remains valid until
/// after the matching `close` callback returns, so any non-null pointer held by
/// this handle is safe to pass to `snd_pcm_period_elapsed` for the lifetime of
/// the stream.
#[derive(Copy, Clone)]
pub struct SubstreamHandle(*mut bindings::snd_pcm_substream);

// SAFETY: The raw pointer is only dereferenced inside period_elapsed(), which
// is always called without holding any driver lock.
unsafe impl Send for SubstreamHandle {}

impl Default for SubstreamHandle {
    fn default() -> Self {
        Self::none()
    }
}

impl SubstreamHandle {
    /// Creates an active handle from a substream reference.
    pub fn new(sub: &Substream) -> Self {
        Self(sub.as_ptr())
    }

    /// Creates a null (inactive) handle.
    pub const fn none() -> Self {
        Self(core::ptr::null_mut())
    }

    /// Updates this handle to point to `sub`.
    pub fn set(&mut self, sub: &Substream) {
        self.0 = sub.as_ptr();
    }

    /// Clears this handle to null (inactive).
    pub fn clear(&mut self) {
        self.0 = core::ptr::null_mut();
    }

    /// Notifies ALSA that a period has elapsed; no-op if the handle is inactive.
    ///
    /// Must be called without holding any driver spinlock, as ALSA internally
    /// acquires the PCM stream lock.
    pub fn period_elapsed(self) {
        if !self.0.is_null() {
            // SAFETY: The pointer was derived from a valid &Substream at open
            // time. ALSA keeps it live through close. No driver lock is held.
            unsafe { bindings::snd_pcm_period_elapsed(self.0) }
        }
    }
}

/// Atomically stored PCM substream handle, for lock-free interrupt/timer context.
///
/// Use this when the substream pointer must be accessed from interrupt or timer
/// context without holding a spinlock (e.g. an hrtimer callback that needs to
/// call [`period_elapsed`][Self::period_elapsed]).
///
/// # Invariants
///
/// The stored pointer is either null or was derived from `&Substream` during
/// an `open` or `trigger(Start)` callback with `Release` ordering.  ALSA
/// guarantees the substream object remains valid until after the stream is
/// stopped and closed.
pub struct AtomicSubstreamHandle(Atomic<*const Substream>);

// SAFETY: The pointer is only dereferenced inside period_elapsed(), which
// uses Acquire ordering and calls snd_pcm_period_elapsed() without any driver
// lock held. Atomic<*const Substream> provides the necessary synchronisation.
unsafe impl Send for AtomicSubstreamHandle {}
// SAFETY: All accesses go through atomic operations; concurrent use from
// multiple threads is safe by construction.
unsafe impl Sync for AtomicSubstreamHandle {}

impl AtomicSubstreamHandle {
    /// Creates an inactive (null) handle.
    pub const fn new() -> Self {
        Self(Atomic::new(core::ptr::null()))
    }

    /// Sets this handle to point to `sub` with the given memory ordering.
    pub fn store<Ord: ordering::ReleaseOrRelaxed>(&self, sub: &Substream, order: Ord) {
        self.0.store(sub as *const Substream, order);
    }

    /// Clears this handle to null with the given memory ordering.
    pub fn clear<Ord: ordering::ReleaseOrRelaxed>(&self, order: Ord) {
        self.0.store(core::ptr::null(), order);
    }

    /// Returns `true` if a substream is currently set.
    pub fn is_active<Ord: ordering::AcquireOrRelaxed>(&self, order: Ord) -> bool {
        !self.0.load(order).is_null()
    }

    /// Notifies ALSA that a period has elapsed; no-op if the handle is inactive.
    ///
    /// Uses `order` for the atomic load.  Must be called without holding any
    /// driver lock.
    pub fn period_elapsed<Ord: ordering::AcquireOrRelaxed>(&self, order: Ord) {
        let ptr = self.0.load(order);
        if !ptr.is_null() {
            // SAFETY: The pointer was stored from a valid &Substream with
            // Release ordering; ALSA keeps it live through close; no driver
            // lock is held.
            unsafe { bindings::snd_pcm_period_elapsed(ptr.cast_mut().cast()) }
        }
    }
}

/// Hardware parameter variable indices (mirrors `SNDRV_PCM_HW_PARAM_*`).
pub mod hw_param {
    /// Sample rate (`SNDRV_PCM_HW_PARAM_RATE`).
    pub const RATE: i32 = crate::bindings::SNDRV_PCM_HW_PARAM_RATE as i32;
}

// SAFETY: Pcm is owned by the ALSA card with appropriate locking.
unsafe impl Send for Pcm {}
unsafe impl Sync for Pcm {}
