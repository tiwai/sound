// SPDX-License-Identifier: GPL-2.0

//! ALSA sound subsystem abstractions.
//!
//! Provides Rust bindings for the core ALSA APIs: sound card management and
//! mixer controls.
//!
//! C headers:
//! - [`include/sound/control.h`](srctree/include/sound/control.h)
//! - [`include/sound/core.h`](srctree/include/sound/core.h)

pub mod card;
pub mod control;

pub use card::{Card, OwnedCard};
pub use control::{KControlConfig, KControlOps, ElemIface, ElemType};
pub use crate::{new_sound_card, new_owned_sound_card};
