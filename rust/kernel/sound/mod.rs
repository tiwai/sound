// SPDX-License-Identifier: GPL-2.0

//! ALSA sound subsystem abstractions.
//!
//! Provides Rust bindings for the core ALSA APIs: sound card management,
//! PCM digital audio, mixer controls, and proc interfaces.
//!
//! C headers:
//! - [`include/sound/control.h`](srctree/include/sound/control.h)
//! - [`include/sound/core.h`](srctree/include/sound/core.h)
//! - [`include/sound/info.h`](srctree/include/sound/info.h)
//! - [`include/sound/pcm.h`](srctree/include/sound/pcm.h)

#[cfg(CONFIG_SND_AK4531_CODEC = "y")]
pub mod ak4531;
pub mod card;
pub mod control;
pub mod info;
pub mod pcm;

pub use card::{Card, OwnedCard};
pub use control::{KControlConfig, KControlOps, ElemIface, ElemType};
pub use pcm::{Pcm};
pub use crate::{new_sound_card, new_owned_sound_card};
