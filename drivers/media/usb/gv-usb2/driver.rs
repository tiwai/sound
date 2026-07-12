// SPDX-License-Identifier: GPL-2.0

use crate::regs;
use kernel::{
    debugfs::{
        self,
        BinaryWriter, //
    },
    device,
    prelude::*,
    sync::{
        new_spinlock,
        Arc,
        SpinLock, //
    },
    time::Delta,
    usb,
};

/// The number of isochronous urbs to submit.
const NUM_URBS: usize = 4;

/// The expected max packet size for audio.
const MAX_AUDIO_PACKET_SIZE: u16 = 256;

/// The GV-USB2 USB driver struct.
///
/// Created during probe and stored as the driver's private data.
pub(crate) struct GvUsb2Driver {
    /// Shared audio state.
    _audio: Arc<AudioState>,
    /// Pool of isochronous URBs submitted for audio capture.
    _urbs: [Option<usb::UrbHandle<AudioState, usb::Active>>; NUM_URBS],
    /// The debugfs file exposing captured audio to userspace.
    _capture_file: Pin<KBox<debugfs::File<IsocCapture>>>,
    /// The debugfs directory node.
    _debugfs_dir: debugfs::Dir,
}

/// Shared state for audio capture.
#[pin_data]
struct AudioState {
    /// Ring buffer for streaming audio data from URBs to userspace.
    #[pin]
    ring: SpinLock<AudioRingBuffer>,
}

/// A ring buffer for audio data.
///
/// Used to transfer audio data from the URB completions to userspace
/// via debugfs.
struct AudioRingBuffer {
    /// Audio buffer, sized to a power of two.
    buf: KBox<[u8]>,
    /// Producer index.
    head: u32,
    /// Consumer index.
    tail: u32,
    /// Mask to handle ring buffer wrap-around. Must be capacity - 1.
    mask: u32,
}

impl AudioRingBuffer {
    /// Write data into the ring buffer.
    ///
    /// Returns the number of bytes written.
    fn write(&mut self, data: &[u8]) -> usize {
        let capacity = self.mask as usize + 1;
        let fill = self.head.wrapping_sub(self.tail) as usize;

        if fill >= capacity {
            return 0;
        }

        let to_write = core::cmp::min(capacity - fill, data.len());
        if to_write == 0 {
            return 0;
        }

        let mask = self.mask as usize;
        let head_idx = self.head as usize & mask;
        let space_to_end = capacity - head_idx;

        if to_write <= space_to_end {
            self.buf[head_idx..head_idx + to_write].copy_from_slice(&data[..to_write]);
        } else {
            let (first, rest) = data.split_at(space_to_end);
            self.buf[head_idx..capacity].copy_from_slice(first);
            self.buf[..to_write - space_to_end].copy_from_slice(rest);
        }

        self.head = self.head.wrapping_add(to_write as u32);

        to_write
    }

    /// Read data from the ring buffer.
    ///
    /// Returns the number of bytes copied into `buf`.
    fn read(&mut self, buf: &mut [u8]) -> usize {
        let capacity = self.mask as usize + 1;
        let fill = self.head.wrapping_sub(self.tail) as usize;

        if fill == 0 {
            return 0;
        }

        let to_read = core::cmp::min(fill, buf.len());
        let mask = self.mask as usize;
        let tail_idx = self.tail as usize & mask;
        let space_to_end = capacity - tail_idx;

        if to_read <= space_to_end {
            buf[..to_read].copy_from_slice(&self.buf[tail_idx..tail_idx + to_read]);
        } else {
            let first_len = space_to_end;
            let second_len = to_read - space_to_end;
            buf[..first_len].copy_from_slice(&self.buf[tail_idx..capacity]);
            buf[first_len..to_read].copy_from_slice(&self.buf[..second_len]);
        }

        self.tail = self.tail.wrapping_add(to_read as u32);

        to_read
    }
}

/// Debugfs reader that drains the audio ring buffer.
struct IsocCapture {
    /// Shared audio state whose ring buffer is read on each `read()` call.
    state: Arc<AudioState>,
}

impl BinaryWriter for IsocCapture {
    fn write_to_slice(
        &self,
        writer: &mut kernel::uaccess::UserSliceWriter,
        offset: &mut kernel::fs::file::Offset,
    ) -> Result<usize> {
        let mut buf = [0u8; 4096];
        let n = match self.state.ring.try_lock() {
            Some(mut guard) => guard.read(&mut buf),
            None => 0,
        };
        if n == 0 {
            return Ok(0);
        }
        writer.write_slice_file(&buf[..n], offset)
    }
}

kernel::usb_device_table!(
    USB_TABLE,
    MODULE_USB_TABLE,
    <GvUsb2Driver as usb::Driver>::IdInfo,
    [(usb::DeviceId::from_id(0x04BB, 0x0532), ()),]
);

impl usb::Driver for GvUsb2Driver {
    type IdInfo = ();
    type Data<'bound> = Self;
    const ID_TABLE: usb::IdTable<Self::IdInfo> = &USB_TABLE;

    /// Probe the GV-USB2 device.
    ///
    /// Matches only audio interface 2 (isochronous endpoint 4).
    /// Switches to altsetting 1, initializes the ADC, submits four isochronous
    /// URBs, and exposes the audio stream via debugfs.
    fn probe<'bound>(
        intf: &'bound usb::Interface<device::Core<'_>>,
        _id: &usb::DeviceId,
        _info: &'bound Self::IdInfo,
    ) -> impl PinInit<Self, Error> + 'bound {
        let dev: &device::Device<device::Core<'_>> = intf.as_ref();

        dev_dbg!(
            dev,
            "INTERFACE {}: num altsettings = {}\n",
            intf.cur_altsetting().number(),
            intf.altsettings().len(),
        );
        for endpoint in intf.cur_altsetting().endpoints() {
            dev_dbg!(
                dev,
                "  endpoint dir: {:?}, endpoint type: {:?}\n",
                endpoint.endpoint_dir(),
                endpoint.endpoint_type()
            );
        }

        if intf.cur_altsetting().class() == usb::ch9::InterfaceClass::AUDIO {
            // Audio class is supported by snd-usb-audio, reject the interface.
            return Err(ENODEV);
        }

        // Interface 2 is the audio one for gv-usb2, skip the others
        if intf.cur_altsetting().number() != 2 {
            return Err(ENODEV);
        }

        // Switch to altsetting 1 (full-bandwidth).
        intf.set_interface(1)?;

        let audio_ep = find_audio_endpoint(intf)?;
        dev_dbg!(
            dev,
            "Found audio endpoint: {}\n",
            audio_ep.endpoint_number()
        );

        let buf = KBox::new([0; 32_768], GFP_KERNEL)?;
        let audio: Arc<AudioState> = Arc::pin_init(
            pin_init!(AudioState {
                ring <- new_spinlock!(AudioRingBuffer {
                    buf,
                    head: 0,
                    tail: 0,
                    mask: 32_767,
                }),
            }),
            GFP_KERNEL,
        )?;

        // Initialize the audio ADC: GPIO, AC97 disable, I2S enable.
        reset_adc(intf)?;

        let urbs = submit_audio_urbs(intf, audio_ep, &audio)?;

        let dir = debugfs::Dir::new(c"gv-usb2");
        let file = KBox::pin_init(
            dir.read_binary_file(
                c"audio_raw",
                IsocCapture {
                    state: audio.clone(),
                },
            ),
            GFP_KERNEL,
        )?;

        let driver = Self {
            _audio: audio,
            _urbs: urbs,
            _capture_file: file,
            _debugfs_dir: dir,
        };

        dev_dbg!(dev, "GV-USB2 successfully initialized\n");
        Ok(driver)
    }

    /// Disconnect the GV-USB2 device.
    ///
    /// All resources are cleaned up when dropped.
    fn disconnect<'bound>(intf: &'bound usb::Interface<device::Core<'_>>, _data: Pin<&Self>) {
        let dev: &device::Device<device::Core<'_>> = intf.as_ref();
        dev_dbg!(dev, "GV-USB2 disconnected\n");
    }
}

/// Write a vendor-specific control register on the GV-USB2 device.
///
/// Uses a vendor-type control request (`REQ_WRITE_REG`) to write the
/// given `value` to the given `reg` address.
fn write_reg(intf: &usb::Interface<device::Bound>, reg: u16, value: u8) -> Result {
    let req = usb::ch9::CtrlRequest::new(
        usb::ch9::RequestType::new(
            usb::ch9::Direction::Out,
            usb::ch9::Type::Vendor,
            usb::ch9::Recipient::Device,
        ),
        regs::REQ_WRITE_REG,
        u16::from(value),
        reg,
        0,
    );
    let dev: &usb::Device<device::Bound> = intf.as_ref();
    dev.control_msg(&req, None, Delta::from_millis(1_000))
        .map(|_| ())
}

/// Initialize the audio ADC on the GV-USB2.
///
/// Configures GPIO direction and output values, disables the AC97 codec,
/// and enables the I2S interface.
fn reset_adc(intf: &usb::Interface<device::Bound>) -> Result {
    // GPIO: bits 4+5 as outputs; SOUND_ENABLE=0 (active), SDA_RESET=1 (inactive)
    write_reg(intf, regs::GPIO_CTRL + 2, 0x30)?;
    write_reg(intf, regs::GPIO_CTRL, 0x10)?;
    // Disable AC97, enable I2S
    write_reg(intf, regs::AC97_CTRL, 0x00)?;
    write_reg(intf, regs::I2S_CTRL, 0x01)
}

/// Completion callback for isochronous audio URBs.
///
/// Copies received audio data into the ring buffer and resubmits the URB.
fn audio_complete(result: usb::UrbResult<'_, AudioState>) {
    let Ok(urb_data) = result.check_or_resubmit(GFP_ATOMIC) else {
        return;
    };

    if let Some(state) = urb_data.context() {
        let mut ring_guard = state.ring.lock();
        for desc in urb_data.iso_frame_descs() {
            if let Ok(data) = urb_data.data_from_iso_packet_desc(desc) {
                ring_guard.write(data);
            }
        }
    }

    let _ = urb_data.resubmit(GFP_ATOMIC);
}

/// Create and submit isochronous URBs for audio capture.
///
/// URBs are resubmitted continuously by `audio_complete`.
fn submit_audio_urbs(
    intf: &usb::Interface<device::Bound>,
    ep: &usb::HostEndpoint,
    audio: &Arc<AudioState>,
) -> Result<[Option<usb::UrbHandle<AudioState, usb::Active>>; NUM_URBS]> {
    const PACKETS_PER_URB: u32 = 8;
    const URB_BUF_SIZE: usize = (PACKETS_PER_URB as usize) * (MAX_AUDIO_PACKET_SIZE as usize);

    let dev: &usb::Device<device::Bound> = intf.as_ref();
    let pipe = usb::Pipe::new_receive_isoc_pipe(dev, ep);
    let mut urbs = [const { None }; NUM_URBS];
    for active_urb in urbs.iter_mut().take(NUM_URBS) {
        let buf = KBox::new([0u8; URB_BUF_SIZE], GFP_KERNEL)?;
        let urb_handle = usb::Urb::<AudioState>::new_isoc(
            GFP_KERNEL,
            intf,
            pipe,
            buf,
            Some(audio.clone()),
            audio_complete,
            PACKETS_PER_URB,
            MAX_AUDIO_PACKET_SIZE,
            usb::TransferFlag::IsoAsap.into(),
            i32::from(ep.interval()),
        )?;
        active_urb.replace(urb_handle.submit(GFP_KERNEL)?);
    }
    Ok(urbs)
}

/// Attempt to find the isochronous audio IN endpoint on the interface.
fn find_audio_endpoint(intf: &usb::Interface) -> Result<&usb::HostEndpoint> {
    for altsetting in intf.altsettings() {
        for ep in altsetting.endpoints() {
            if ep.endpoint_dir() == usb::ch9::Direction::In
                && ep.endpoint_type() == usb::EndpointType::Isoc
                && ep.endpoint_number() == 4
                && ep.maxp() == MAX_AUDIO_PACKET_SIZE
            {
                return Ok(ep);
            }
        }
    }
    Err(ENODEV)
}
