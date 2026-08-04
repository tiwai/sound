// SPDX-License-Identifier: GPL-2.0

//! ALSA PCM callbacks for the USB audio driver.
//!
//! Corresponds to `sound/usb/pcm.c`.
//!
//! Stub for skeleton commit; full implementation in the stream/PCM commit.

use kernel::bindings;
use kernel::sound::pcm::{Ops, Substream, TriggerCommand};

use crate::stream::UsbStream;

impl Ops for UsbStream {
    const NONATOMIC: bool = true;

    fn trigger(&self, _substream: &Substream, _cmd: TriggerCommand) -> kernel::error::Result {
        Ok(())
    }

    fn pointer(&self, _substream: &Substream) -> bindings::snd_pcm_uframes_t {
        0
    }
}
