// SPDX-License-Identifier: GPL-2.0

// USB Requests

/// USB request value to read to a register on the STK1150.
#[allow(dead_code)]
pub(crate) const REQ_READ_REG: u8 = 0x00;

/// USB request value to write to a register on the STK1150.
pub(crate) const REQ_WRITE_REG: u8 = 0x01;

// Registers (accessed via REQ_READ_REG and REQ_WRITE_REG)

/// GPIO Control Register.
///
/// b31:    EEPROM Disable
/// b25-16: DIR
/// b9-0:   VALUE
pub(crate) const GPIO_CTRL: u16 = 0x0000;

/// Audio Control Register 0.
pub(crate) const AC97_CTRL: u16 = 0x0500;

/// I2S Control Register.
pub(crate) const I2S_CTRL: u16 = 0x050C;
