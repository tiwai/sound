// SPDX-License-Identifier: GPL-2.0

//! ALSA sound subsystem abstractions.
//!
//! Provides Rust bindings for the core ALSA APIs: sound card management
//!
//! C headers:
//! - [`include/sound/core.h`](srctree/include/sound/core.h)

pub mod card;

pub use card::{Card, OwnedCard};
pub use crate::{new_sound_card, new_owned_sound_card};
