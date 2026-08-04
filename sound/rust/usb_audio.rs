// SPDX-License-Identifier: GPL-2.0

//! Rust USB Audio Class driver.
//!
//! Implements the PCM streaming path of the USB Audio Class (UAC 1/2) for
//! Linux.  Provides playback and capture for generic USB audio devices.
//!
//! AudioControl mixer, MIDI, MIDI2, and Media Controller support are deferred.

#[path = "usb_audio/types.rs"]
mod types;
#[path = "usb_audio/validate.rs"]
mod validate;
#[path = "usb_audio/helper.rs"]
mod helper;
#[path = "usb_audio/quirks.rs"]
mod quirks;
#[path = "usb_audio/implicit.rs"]
mod implicit;
#[path = "usb_audio/format.rs"]
mod format;
#[path = "usb_audio/clock.rs"]
mod clock;
#[path = "usb_audio/endpoint.rs"]
mod endpoint;
#[path = "usb_audio/stream.rs"]
mod stream;
#[path = "usb_audio/pcm.rs"]
mod pcm;
#[path = "usb_audio/mixer.rs"]
mod mixer;
#[path = "usb_audio/card.rs"]
mod card;

use kernel::{driver, prelude::*, usb};

// Custom InPlaceModule so we can initialise REGISTER_MUTEX before
// usb_register() fires and any probe() can be triggered.
#[pin_data]
struct UsbAudioModule {
    #[pin]
    _driver: driver::Registration<usb::Adapter<card::UsbAudioDriver>>,
}

impl kernel::InPlaceModule for UsbAudioModule {
    fn init(
        module: &'static ThisModule,
    ) -> impl PinInit<Self, Error> {
        // SAFETY: Called exactly once at module load time, before any probe().
        unsafe { card::REGISTER_MUTEX.init() };

        try_pin_init!(Self {
            _driver <- driver::Registration::new(
                <UsbAudioModule as kernel::ModuleMetadata>::NAME,
                module,
            ),
        })
    }
}

module! {
    type: UsbAudioModule,
    name: "snd_rust_usb_audio",
    authors: ["Rust for Linux contributors"],
    description: "Rust USB Audio Class driver (PCM streaming path)",
    license: "GPL",
    params: {
        lowlatency: bool {
            default: true,
            description: "Enable low latency playback (default: yes).",
        },
        autoclock: bool {
            default: true,
            description: "Enable auto-clock selection for UAC2 devices (default: yes).",
        },
    },
}
