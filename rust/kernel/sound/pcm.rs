// SPDX-License-Identifier: GPL-2.0

//! PCM (digital audio) abstraction.
//!
//! C header: [`include/sound/pcm.h`](srctree/include/sound/pcm.h)

use crate::{
    bindings,
    error::to_result,
    prelude::*,
    str::CStr,
    sync::{atomic::{ordering, Atomic}, Arc, ArcBorrow},
    types::Opaque,
};
use core::ffi::c_void;
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
    /// `constraint` must be a `static` - the kernel stores a pointer to it and
    /// dereferences it on every `hw_refine` call.
    pub fn hw_constraint_list(
        &self,
        cond: u32,
        var: i32,
        constraint: &'static HwConstraintList,
    ) -> crate::error::Result {
        // SAFETY: constraint is 'static with valid inner pointers
        // guaranteed by HwConstraintList::new().
        crate::error::to_result(unsafe {
            bindings::snd_pcm_hw_constraint_list(self.as_raw(), cond, var, &constraint.inner)
        })
    }

    /// Adds a ratnum-based hardware parameter constraint to a runtime.
    ///
    /// `constraint` must be a `static` - same lifetime requirement as
    /// [`hw_constraint_list`].
    pub fn hw_constraint_ratnums(
        &self,
        cond: u32,
        var: i32,
        constraint: &'static HwConstraintRatnums,
    ) -> crate::error::Result {
        // SAFETY: constraint is 'static with valid inner pointers
        // guaranteed by HwConstraintRatnums::new().
        crate::error::to_result(unsafe {
            bindings::snd_pcm_hw_constraint_ratnums(self.as_raw(), cond, var, &constraint.inner)
        })
    }

    /// Constrains a runtime parameter to the range \[`min`, `max`\].
    ///
    /// Mirrors `snd_pcm_hw_constraint_minmax()`.
    pub fn hw_constraint_minmax(
        &self,
        var: i32,
        min: u32,
        max: u32,
    ) -> crate::error::Result {
        crate::error::to_result(unsafe {
            bindings::snd_pcm_hw_constraint_minmax(self.as_raw(), var as _, min, max)
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

    /// Notifies ALSA that a PCM period has elapsed while holding the stream lock.
    pub fn period_elapsed_under_stream_lock(&self) {
        // SAFETY: self is a valid snd_pcm_substream.
        unsafe { bindings::snd_pcm_period_elapsed_under_stream_lock(self.as_raw()) }
    }

    /// Returns the raw `private_data` pointer.
    pub fn private_data(&self) -> *mut core::ffi::c_void {
        unsafe { (*self.as_raw()).private_data }
    }

    /// Returns a clone of the [`Arc<T>`] previously stored with
    /// [`Pcm::set_private_data_arc`].
    ///
    /// # Safety
    ///
    /// The caller must guarantee that `set_private_data_arc::<T>` was used when
    /// registering the PCM this substream belongs to, and that the Arc is still
    /// alive.
    pub unsafe fn clone_private_arc<T: Send + Sync + 'static>(&self) -> Arc<T> {
        let ptr = unsafe { (*self.as_raw()).private_data as *const T };
        // SAFETY: ptr was stored by set_private_data_arc via Arc::into_raw.
        // ArcBorrow::from_raw is non-owning and coexists with the existing owner.
        let borrow = unsafe { ArcBorrow::from_raw(ptr) };
        Arc::from(borrow)
    }
}

/// Hardware parameters negotiated during PCM setup.
///
/// Wraps `struct snd_pcm_hw_params`.
#[repr(transparent)]
pub struct HwParams(Opaque<bindings::snd_pcm_hw_params>);

impl HwParams {
    /// Returns the raw pointer to the underlying C struct.
    pub fn as_raw(&self) -> *mut bindings::snd_pcm_hw_params {
        self.0.get()
    }

    /// # Safety
    ///
    /// `ptr` must point to a valid, live `snd_pcm_hw_params`.
    pub unsafe fn from_raw<'a>(ptr: *mut bindings::snd_pcm_hw_params) -> &'a HwParams {
        // SAFETY: The caller guarantees `ptr` is valid and live for `'a`.
        unsafe { &*ptr.cast::<HwParams>() }
    }

    /// Returns the negotiated sample rate in Hz.
    pub fn rate(&self) -> u32 {
        // SAFETY: `self.as_raw()` is a valid `snd_pcm_hw_params` pointer.
        unsafe { bindings::params_rate(self.as_raw()) }
    }

    /// Returns the negotiated number of channels.
    pub fn channels(&self) -> u32 {
        // SAFETY: `self.as_raw()` is a valid `snd_pcm_hw_params` pointer.
        unsafe { bindings::params_channels(self.as_raw()) }
    }

    /// Returns the negotiated sample format.
    pub fn format(&self) -> i32 {
        // SAFETY: `self.as_raw()` is a valid `snd_pcm_hw_params` pointer.
        unsafe { bindings::params_format(self.as_raw()) }
    }

    /// Returns the negotiated period size in frames.
    pub fn period_size(&self) -> u32 {
        // SAFETY: `self.as_raw()` is a valid `snd_pcm_hw_params` pointer.
        unsafe { bindings::params_period_size(self.as_raw()) }
    }

    /// Returns the negotiated buffer size in frames.
    pub fn buffer_size(&self) -> u32 {
        // SAFETY: `self.as_raw()` is a valid `snd_pcm_hw_params` pointer.
        unsafe { bindings::params_buffer_size(self.as_raw()) }
    }

    /// Returns the negotiated number of periods.
    pub fn periods(&self) -> u32 {
        // SAFETY: `self.as_raw()` is a valid `snd_pcm_hw_params` pointer.
        unsafe { bindings::params_periods(self.as_raw()) }
    }

    /// Returns a view of the interval for the given hw_param variable.
    ///
    /// `var` must be an interval-type variable (`hw_param::RATE`,
    /// `hw_param::CHANNELS`, `hw_param::PERIOD_TIME`, etc.).
    pub fn interval(&self, var: i32) -> HwInterval {
        HwInterval(unsafe { bindings::hw_param_interval(self.as_raw(), var) })
    }

    /// Returns a view of the mask for the given hw_param variable.
    ///
    /// `var` must be a mask-type variable (`hw_param::FORMAT`).
    pub fn mask(&self, var: i32) -> HwMask {
        HwMask(unsafe { bindings::hw_param_mask(self.as_raw(), var) })
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
        params: &HwParams,
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
    // SAFETY: The C kernel guarantees `params` is valid and live for the duration of this call.
    let hw_params = unsafe { HwParams::from_raw(params) };
    match chip.hw_params(sub, hw_params) {
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

    /// Sets `pcm->private_data` from an [`Arc`], transferring ownership to the PCM.
    ///
    /// The Arc reference count is incremented; a `private_free` callback is
    /// installed to drop it when the PCM device is freed. The stored `*const T`
    /// is compatible with the trampolines in [`PcmOpsTable`].
    pub fn set_private_data_arc<T: Send + Sync + 'static>(&self, data: Arc<T>) {
        let ptr = Arc::into_raw(data);
        // SAFETY: ptr is a valid Arc-derived *const T; pcm_private_arc_free<T>
        // will reconstruct and drop the Arc when the PCM is freed.
        unsafe {
            (*self.as_raw()).private_data = ptr as *mut core::ffi::c_void;
            (*self.as_raw()).private_free = Some(pcm_private_arc_free::<T>);
        }
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

    /// Suspends all active substreams of a PCM device.
    ///
    /// Calls `trigger(Suspend)` on each running substream and transitions them to
    /// the `SUSPENDED` state.  Most drivers do **not** need to call this - the PCM
    /// device type's own PM callback invokes it automatically.  Only call it
    /// explicitly when the driver needs to suspend streams before the PCM device's
    /// PM path runs.
    pub fn suspend_all(&self) -> Result {
        // SAFETY: pcm.as_raw() is a valid snd_pcm.
        to_result(unsafe { bindings::snd_pcm_suspend_all(self.as_raw()) })
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
/// The stored pointer is either null or was derived from `&Substream` during
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

    /// Notifies ALSA that a PCM period has elapsed while holding the stream lock.
    pub fn period_elapsed_under_stream_lock(self) {
        if !self.0.is_null() {
            // SAFETY: The pointer was derived from a valid &Substream at open
            // time. ALSA keeps it live through close.
            unsafe { bindings::snd_pcm_period_elapsed_under_stream_lock(self.0) }
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

    /// Notifies ALSA that a PCM period has elapsed while holding the stream lock.
    pub fn period_elapsed_under_stream_lock<Ord: ordering::AcquireOrRelaxed>(&self, order: Ord) {
        let ptr = self.0.load(order);
        if !ptr.is_null() {
            // SAFETY: The pointer was stored from a valid &Substream with
            // Release ordering; ALSA keeps it live through close; no driver
            // lock is held.
            unsafe { bindings::snd_pcm_period_elapsed_under_stream_lock(ptr.cast_mut().cast()) }
        }
    }
}

/// Hardware parameter variable indices (mirrors `SNDRV_PCM_HW_PARAM_*`).
pub mod hw_param {
    /// Sample format (`SNDRV_PCM_HW_PARAM_FORMAT`).
    pub const FORMAT: i32 = crate::bindings::SNDRV_PCM_HW_PARAM_FORMAT as i32;
    /// Sample rate (`SNDRV_PCM_HW_PARAM_RATE`).
    pub const RATE: i32 = crate::bindings::SNDRV_PCM_HW_PARAM_RATE as i32;
    /// Number of channels (`SNDRV_PCM_HW_PARAM_CHANNELS`).
    pub const CHANNELS: i32 = crate::bindings::SNDRV_PCM_HW_PARAM_CHANNELS as i32;
    /// Period duration in microseconds (`SNDRV_PCM_HW_PARAM_PERIOD_TIME`).
    pub const PERIOD_TIME: i32 = crate::bindings::SNDRV_PCM_HW_PARAM_PERIOD_TIME as i32;
    /// Period size in frames (`SNDRV_PCM_HW_PARAM_PERIOD_SIZE`).
    pub const PERIOD_SIZE: i32 = crate::bindings::SNDRV_PCM_HW_PARAM_PERIOD_SIZE as i32;
    /// Number of periods (`SNDRV_PCM_HW_PARAM_PERIODS`).
    pub const PERIODS: i32 = crate::bindings::SNDRV_PCM_HW_PARAM_PERIODS as i32;
    /// Buffer duration in microseconds (`SNDRV_PCM_HW_PARAM_BUFFER_TIME`).
    pub const BUFFER_TIME: i32 = crate::bindings::SNDRV_PCM_HW_PARAM_BUFFER_TIME as i32;
    /// Buffer size in frames (`SNDRV_PCM_HW_PARAM_BUFFER_SIZE`).
    pub const BUFFER_SIZE: i32 = crate::bindings::SNDRV_PCM_HW_PARAM_BUFFER_SIZE as i32;
}

/// Safe view of a `snd_interval` inside a `HwParams`.
///
/// Obtained from [`HwParams::interval`]; valid only for the duration of the
/// enclosing [`HwRule::apply`] call.
pub struct HwInterval(*mut bindings::snd_interval);

impl HwInterval {
    /// Returns the interval minimum.
    pub fn min(&self) -> u32 {
        unsafe { (*self.0).min }
    }

    /// Returns the interval maximum.
    pub fn max(&self) -> u32 {
        unsafe { (*self.0).max }
    }

    /// Returns true if the minimum endpoint is open (excluded).
    pub fn openmin(&self) -> bool {
        unsafe { (*self.0).openmin() != 0 }
    }

    /// Returns true if the maximum endpoint is open (excluded).
    pub fn openmax(&self) -> bool {
        unsafe { (*self.0).openmax() != 0 }
    }

    /// Clamps the minimum to `v` and clears the open-min flag.
    pub fn set_min(&self, v: u32) {
        unsafe {
            (*self.0).min = v;
            (*self.0).set_openmin(0);
        }
    }

    /// Clamps the maximum to `v` and clears the open-max flag.
    pub fn set_max(&self, v: u32) {
        unsafe {
            (*self.0).max = v;
            (*self.0).set_openmax(0);
        }
    }

    /// Marks the interval as empty (no valid value exists).
    pub fn set_empty(&self) {
        unsafe { (*self.0).set_empty(1) }
    }

    /// Returns true if the interval has been marked empty.
    pub fn is_empty(&self) -> bool {
        unsafe { (*self.0).empty() != 0 }
    }

    /// Returns true if `val` is a valid point in this interval.
    ///
    /// Mirrors `snd_interval_test()`.
    pub fn test(&self, val: u32) -> bool {
        let min = self.min();
        let max = self.max();
        if val < min || val > max {
            return false;
        }
        if self.openmin() && val == min {
            return false;
        }
        if self.openmax() && val == max {
            return false;
        }
        true
    }
}

/// Safe view of a `snd_mask` inside a `HwParams`.
///
/// Obtained from [`HwParams::mask`]; valid only for the duration of the
/// enclosing [`HwRule::apply`] call.
pub struct HwMask(*mut bindings::snd_mask);

impl HwMask {
    /// Returns the mask as a 64-bit bitmask (bits\[0\] | bits\[1\] << 32).
    pub fn bits_u64(&self) -> u64 {
        unsafe {
            let b = &(*self.0).bits;
            (b[0] as u64) | ((b[1] as u64) << 32)
        }
    }

    /// ANDs the mask with `bits`; returns `true` if the mask changed.
    ///
    /// Returns `false` and leaves the mask empty if the result is all-zero
    /// (no valid format remains after intersection).
    pub fn and_u64(&self, bits: u64) -> bool {
        unsafe {
            let b = &mut (*self.0).bits;
            let old0 = b[0];
            let old1 = b[1];
            b[0] &= bits as u32;
            b[1] &= (bits >> 32) as u32;
            old0 != b[0] || old1 != b[1]
        }
    }

    /// Returns true if the mask is empty (no bits set in bits\[0..=1\]).
    pub fn is_empty(&self) -> bool {
        unsafe {
            let b = &(*self.0).bits;
            b[0] == 0 && b[1] == 0
        }
    }

    /// Returns true if format bit `bit` is set in the mask.
    pub fn test(&self, bit: u32) -> bool {
        if bit >= 64 {
            return false;
        }
        unsafe { (*self.0).bits[(bit / 32) as usize] & (1 << (bit % 32)) != 0 }
    }
}

/// A trait for PCM hardware parameter constraint rules.
///
/// Implement this on a driver-specific type and register instances with
/// [`Runtime::store_hw_rules`] + [`HwRuleHandle::add_rule`].
pub trait HwRule: Send + Sync {
    /// Applies the constraint to `params`.
    ///
    /// Returns `1` if any parameter interval/mask changed, `0` if unchanged,
    /// or a negative errno if the constraint cannot be satisfied.
    fn apply(&self, params: &HwParams) -> i32;
}

// Generic C-callback trampoline for HwRule; invisible to drivers.
unsafe extern "C" fn hw_rule_trampoline<T: HwRule>(
    params: *mut bindings::snd_pcm_hw_params,
    rule: *mut bindings::snd_pcm_hw_rule,
) -> core::ffi::c_int {
    let obj = unsafe { &*((*rule).private as *const T) };
    // SAFETY: params is valid for the duration of the callback.
    obj.apply(unsafe { HwParams::from_raw(params) })
}

/// Typed handle to a heap-allocated rule set stored in a [`Runtime`].
///
/// Obtained from [`Runtime::store_hw_rules`]. Use [`HwRuleHandle::add_rule`]
/// to register individual constraint callbacks without any `unsafe` at the
/// call site.
pub struct HwRuleHandle<T>(*mut T);

// SAFETY: HwRuleHandle contains a raw pointer to a heap allocation whose
// ownership is held by the Runtime; it is only used during the open callback
// (single-threaded w.r.t. the runtime) and dropped when the runtime is freed.
unsafe impl<T: Send> Send for HwRuleHandle<T> {}
unsafe impl<T: Sync> Sync for HwRuleHandle<T> {}

impl Runtime {
    /// Transfers ownership of `rules` into the runtime's `private_data`.
    ///
    /// The rule set is freed automatically when the runtime is freed (including
    /// error paths that unwind through the PCM core), so rule pointers obtained
    /// from the returned handle are guaranteed to remain valid for the runtime's
    /// lifetime.
    pub fn store_hw_rules<U: Send + 'static>(&self, rules: crate::alloc::KBox<U>) -> HwRuleHandle<U> {
        unsafe extern "C" fn free_cb<U>(rt: *mut bindings::snd_pcm_runtime) {
            let ptr = unsafe { (*rt).private_data as *mut U };
            // SAFETY: ptr was set from KBox::into_raw in store_hw_rules.
            unsafe { drop(crate::alloc::KBox::from_raw(ptr)); }
            unsafe { (*rt).private_data = core::ptr::null_mut(); }
        }
        let raw = crate::alloc::KBox::into_raw(rules);
        // SAFETY: raw is a fresh heap pointer from KBox::into_raw; set_private
        // is called exactly once per runtime.
        unsafe { self.set_private(raw as *mut c_void, free_cb::<U>); }
        HwRuleHandle(raw)
    }
}

impl<T> HwRuleHandle<T> {
    /// Registers a hw_rule callback for the field of the rule set returned by `field`.
    ///
    /// The rule set is already stored in the runtime (via [`Runtime::store_hw_rules`])
    /// at a stable heap address, so this method is safe to call.
    pub fn add_rule<R, F>(
        &self,
        runtime: &Runtime,
        cond: u32,
        var: i32,
        field: F,
        dep0: i32,
        dep1: i32,
        dep2: i32,
        dep3: i32,
    ) -> crate::error::Result
    where
        R: HwRule,
        F: for<'a> FnOnce(&'a T) -> &'a R,
    {
        // SAFETY: self.0 was set from Box::into_raw in store_hw_rules; the Box
        // is owned by the runtime's private_data and will outlive any callback.
        let rule_ref: &R = unsafe { field(&*self.0) };
        let rule_ptr = rule_ref as *const R as *mut c_void;
        to_result(unsafe {
            bindings::snd_pcm_hw_rule_add(
                runtime.as_raw(), cond, var,
                Some(hw_rule_trampoline::<R>),
                rule_ptr,
                dep0, dep1, dep2, dep3,
            )
        })
    }
}

/// Safe wrapper for a list-based PCM hardware parameter constraint.
///
/// Construct as a `static` and pass a reference to [`hw_constraint_list`].
pub struct HwConstraintList {
    inner: bindings::snd_pcm_hw_constraint_list,
}

impl HwConstraintList {
    /// Creates a new constraint list from a static rate array.
    pub const fn new(mask: u32, list: &'static [u32]) -> Self {
        Self {
            inner: bindings::snd_pcm_hw_constraint_list {
                mask,
                count: list.len() as u32,
                list: list.as_ptr(),
            },
        }
    }
}

// SAFETY: `list` points to static immutable data; the struct itself has no
// interior mutability.
unsafe impl Sync for HwConstraintList {}

/// Safe wrapper for a rational-number PCM hardware parameter constraint.
///
/// Construct as a `static` and pass a reference to [`hw_constraint_ratnums`].
pub struct HwConstraintRatnums {
    inner: bindings::snd_pcm_hw_constraint_ratnums,
}

impl HwConstraintRatnums {
    /// Creates a new ratnum constraint from a static `snd_ratnum` array.
    pub const fn new(rats: &'static [bindings::snd_ratnum]) -> Self {
        Self {
            inner: bindings::snd_pcm_hw_constraint_ratnums {
                nrats: rats.len() as i32,
                rats: rats.as_ptr(),
            },
        }
    }
}

// SAFETY: `rats` points to static immutable data; the struct itself has no
// interior mutability.
unsafe impl Sync for HwConstraintRatnums {}

// SAFETY: Pcm is owned by the ALSA card with appropriate locking.
unsafe impl Send for Pcm {}
unsafe impl Sync for Pcm {}

/// `private_free` callback that drops the [`Arc<T>`] stored in `pcm->private_data`.
unsafe extern "C" fn pcm_private_arc_free<T>(pcm: *mut bindings::snd_pcm) {
    // SAFETY: private_data was set by set_private_data_arc and has not been
    // freed yet (this callback is called exactly once by the ALSA core).
    let ptr = unsafe { (*pcm).private_data as *const T };
    if !ptr.is_null() {
        // SAFETY: ptr was produced by Arc::into_raw in set_private_data_arc.
        drop(unsafe { Arc::from_raw(ptr) });
        unsafe { (*pcm).private_data = core::ptr::null_mut() };
    }
}

/// A devres-managed DMA buffer allocated by the ALSA core.
///
/// Wraps `struct snd_dma_buffer *` returned by `snd_devm_alloc_pages`.
/// The allocation is automatically freed when the associated device is removed.
pub struct DmaBuffer(*mut bindings::snd_dma_buffer);

impl DmaBuffer {
    /// Allocates a DMA-coherent buffer using `snd_devm_alloc_pages`.
    ///
    /// Returns `Err(ENOMEM)` if allocation fails.
    pub fn alloc_dev<Ctx: crate::device::DeviceContext>(
        dev: &crate::device::Device<Ctx>,
        dma_type: u32,
        size: usize,
    ) -> crate::error::Result<Self> {
        // SAFETY: dev is a valid kernel device; snd_devm_alloc_pages manages
        // its own lifetime via devres.
        let ptr = unsafe {
            bindings::snd_devm_alloc_pages(dev.as_raw(), dma_type as i32, size)
        };
        if ptr.is_null() {
            Err(ENOMEM)
        } else {
            Ok(Self(ptr))
        }
    }

    /// Returns the physical (bus) address of the DMA buffer.
    pub fn addr(&self) -> u64 {
        // SAFETY: self.0 is non-null and valid for the device lifetime.
        unsafe { (*self.0).addr }
    }

    /// Returns the virtual (CPU-side) address of the DMA buffer.
    pub fn area(&self) -> *mut u8 {
        // SAFETY: self.0 is non-null and valid for the device lifetime.
        unsafe { (*self.0).area as *mut u8 }
    }
}
