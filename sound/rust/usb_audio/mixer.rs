// SPDX-License-Identifier: GPL-2.0

//! USB audio mixer control implementation.
//!
//! Corresponds to `sound/usb/mixer.c`.
//!
//! Stub for skeleton commit; full implementation in the mixer commit.

use kernel::prelude::*;
use kernel::sound::card::Card;
use crate::card::UsbAudioChip;
use kernel::sync::Arc;

pub fn create_mixer(_chip: &Arc<UsbAudioChip>, _card: &Card) -> Result<()> {
    Ok(())
}
