// SPDX-License-Identifier: GPL-2.0

//! USB audio format descriptor parsing.
//!
//! Corresponds to `sound/usb/format.c`.

use kernel::{
    bindings,
    prelude::*,
};
use crate::types::{AudioFormat, UAC_VERSION_1, UAC_VERSION_2, UAC_FORMAT_TYPE_I};

//
// UAC1 format type I codes
//
const UAC_FMT_I_UNDEFINED:  u64 = 0;
const UAC_FMT_I_PCM:        u64 = 1;
const UAC_FMT_I_PCM8:       u64 = 2;
const UAC_FMT_I_IEEE_FLOAT: u64 = 3;
const UAC_FMT_I_ALAW:       u64 = 4;
const UAC_FMT_I_MULAW:      u64 = 5;

const UAC2_FORMAT_TYPE_I_RAW_DATA: u64 = 1 << 31;

//
// ALSA PCM rate bits
//

/// Maps a nominal sample rate to a `SNDRV_PCM_RATE_*` bit.
pub(crate) fn rate_to_rate_bit(rate: u32) -> u32 {
    match rate {
        5512   => bindings::SNDRV_PCM_RATE_5512,
        8000   => bindings::SNDRV_PCM_RATE_8000,
        11025  => bindings::SNDRV_PCM_RATE_11025,
        16000  => bindings::SNDRV_PCM_RATE_16000,
        22050  => bindings::SNDRV_PCM_RATE_22050,
        32000  => bindings::SNDRV_PCM_RATE_32000,
        44100  => bindings::SNDRV_PCM_RATE_44100,
        48000  => bindings::SNDRV_PCM_RATE_48000,
        64000  => bindings::SNDRV_PCM_RATE_64000,
        88200  => bindings::SNDRV_PCM_RATE_88200,
        96000  => bindings::SNDRV_PCM_RATE_96000,
        176400 => bindings::SNDRV_PCM_RATE_176400,
        192000 => bindings::SNDRV_PCM_RATE_192000,
        352800 => bindings::SNDRV_PCM_RATE_352800,
        384000 => bindings::SNDRV_PCM_RATE_384000,
        _      => bindings::SNDRV_PCM_RATE_KNOT,
    }
}

//
// format type -> ALSA fmtbits
//
/// Converts UAC format bits and sample geometry to ALSA FMTBIT mask.
fn map_format_i_bits(
    usb_id: u32,
    fp: &AudioFormat,
    format: u64,
    sample_width: u32,
    mut sample_bytes: u32,
    mut pcm_formats: u64,
) -> u64 {
    if pcm_formats == 0
        && (format == 0 || format == 1 << UAC_FMT_I_UNDEFINED)
    {
        pr_info!(
            "snd_usb_audio: {:04x}:{:04x} {}:{}: format type 0, using PCM\n",
            usb_id >> 16, usb_id & 0xffff, fp.iface, fp.altsetting
        );
        return map_format_i_bits(
            usb_id, fp, 1u64 << UAC_FMT_I_PCM,
            sample_width, sample_bytes, pcm_formats,
        );
    }

    if format & (1 << UAC_FMT_I_PCM) != 0 {
        // Edirol / Roland subframe-size fixup.
        if (usb_id == 0x0582_0016 || usb_id == 0x0582_000c)
            && sample_width == 24 && sample_bytes == 2
        {
            sample_bytes = 3;
        } else if sample_width > sample_bytes * 8 {
            pr_info!(
                "snd_usb_audio: {:04x}:{:04x} {}:{}: sample bitwidth {} in over sample bytes {}\n",
                usb_id >> 16, usb_id & 0xffff, fp.iface, fp.altsetting,
                sample_width, sample_bytes
            );
        }
        pcm_formats |= match sample_bytes {
            1 => bindings::SNDRV_PCM_FMTBIT_S8 as u64,
            2 => bindings::SNDRV_PCM_FMTBIT_S16_LE as u64,
            3 => bindings::SNDRV_PCM_FMTBIT_S24_3LE as u64,
            4 => bindings::SNDRV_PCM_FMTBIT_S32_LE as u64,
            _ => {
                pr_info!(
                    "snd_usb_audio: {:04x}:{:04x} {}:{}: unsupported sample {} bytes\n",
                    usb_id >> 16, usb_id & 0xffff, fp.iface, fp.altsetting,
                    sample_bytes
                );
                0
            }
        };
    }
    if format & (1 << UAC_FMT_I_PCM8) != 0 {
        // Dallas DS4201 reports U8 but is actually S8.
        if usb_id == 0x04fa_4201 {
            pcm_formats |= bindings::SNDRV_PCM_FMTBIT_S8 as u64;
        } else {
            pcm_formats |= bindings::SNDRV_PCM_FMTBIT_U8 as u64;
        }
    }
    if format & (1 << UAC_FMT_I_IEEE_FLOAT) != 0 {
        pcm_formats |= bindings::SNDRV_PCM_FMTBIT_FLOAT_LE as u64;
    }
    if format & (1 << UAC_FMT_I_ALAW) != 0 {
        pcm_formats |= bindings::SNDRV_PCM_FMTBIT_A_LAW as u64;
    }
    if format & (1 << UAC_FMT_I_MULAW) != 0 {
        pcm_formats |= bindings::SNDRV_PCM_FMTBIT_MU_LAW as u64;
    }
    pcm_formats
}

/// Parses the format tag and descriptor into ALSA `SNDRV_PCM_FMTBIT_*` bits.
///
/// `fmt` is the raw descriptor byte slice.
/// `format` is `wFormatTag` (UAC1) or `bmFormats` (UAC2).
fn parse_audio_format_i_type(
    usb_id: u32,
    fp: &mut AudioFormat,
    mut format: u64,
    fmt: &[u8],
) -> u64 {
    match fp.protocol {
        UAC_VERSION_1 => {
            // offset 5 = bSubframeSize, offset 6 = bBitResolution
            if fmt.len() < 8 {
                return 0;
            }
            if format >= 64 {
                pr_info!(
                    "snd_usb_audio: {:04x}:{:04x} {}:{}: invalid format type {:#x}, using PCM\n",
                    usb_id >> 16, usb_id & 0xffff, fp.iface, fp.altsetting, format
                );
                format = UAC_FMT_I_PCM;
            }
            let sample_bytes = fmt[5] as u32;
            let sample_width = fmt[6] as u32;
            fp.fmt_bits = sample_width;
            fp.fmt_sz   = sample_bytes;
            format = 1u64 << format;
            map_format_i_bits(usb_id, fp, format, sample_width, sample_bytes, 0)
        }
        UAC_VERSION_2 => {
            // offset 4 = bSubslotSize, offset 5 = bBitResolution
            if fmt.len() < 9 {
                return 0;
            }
            let sample_bytes = fmt[4] as u32;
            let sample_width = fmt[5] as u32;
            fp.fmt_bits = sample_width;
            fp.fmt_sz   = sample_bytes;
            let mut extra: u64 = 0;
            if format & UAC2_FORMAT_TYPE_I_RAW_DATA != 0 {
                extra |= bindings::SNDRV_PCM_FMTBIT_SPECIAL as u64;
                fp.dsd_raw = true;
                format &= !UAC2_FORMAT_TYPE_I_RAW_DATA;
            }
            format <<= 1;
            map_format_i_bits(usb_id, fp, format, sample_width, sample_bytes, extra)
        }
        _ => 0,
    }
}

//
// UAC1 sample rate table parsing
//
/// Reads a 3-byte little-endian value from `buf`.
fn combine_triple(buf: &[u8]) -> u32 {
    if buf.len() < 3 {
        return 0;
    }
    buf[0] as u32 | (buf[1] as u32) << 8 | (buf[2] as u32) << 16
}

/// Recomputes `rate_min`, `rate_max`, and `rates` from the populated `rate_table`.
fn set_rate_table_min_max(fp: &mut AudioFormat) {
    fp.rate_min = u32::MAX;
    fp.rate_max = 0;
    fp.rates = 0;
    for &rate in fp.rate_table.iter() {
        fp.rate_min = fp.rate_min.min(rate);
        fp.rate_max = fp.rate_max.max(rate);
        fp.rates |= rate_to_rate_bit(rate);
    }
}

/// Parses UAC1 sample rate(s) from the format descriptor byte slice.
///
/// `offset` is where `bSamFreqType` starts (7 for Type I).
fn parse_audio_format_rates_v1(
    usb_id: u32,
    fp: &mut AudioFormat,
    fmt: &[u8],
    offset: usize,
) -> Result<()> {
    if fmt.len() < offset + 1 {
        return Err(EINVAL);
    }
    let nr_rates = fmt[offset] as usize;
    let min_len = offset + 1 + 3 * if nr_rates != 0 { nr_rates } else { 2 };
    if fmt.len() < min_len {
        pr_err!(
            "snd_usb_audio: {:04x}:{:04x} {}:{}: invalid UAC_FORMAT_TYPE desc\n",
            usb_id >> 16, usb_id & 0xffff, fp.iface, fp.altsetting
        );
        return Err(EINVAL);
    }

    if nr_rates != 0 {
        for r in 0..nr_rates {
            let idx = offset + 1 + r * 3;
            let rate = combine_triple(&fmt[idx..]);
            if rate == 0 {
                continue;
            }
            fp.rate_table.push(rate, GFP_KERNEL).map_err(|_| ENOMEM)?;
        }
        if fp.rate_table.is_empty() {
            pr_info!(
                "snd_usb_audio: {:04x}:{:04x} {}:{}: all rates were zero\n",
                usb_id >> 16, usb_id & 0xffff, fp.iface, fp.altsetting
            );
            return Err(EINVAL);
        }
        set_rate_table_min_max(fp);
    } else {
        fp.rates    = bindings::SNDRV_PCM_RATE_CONTINUOUS;
        fp.rate_min = combine_triple(&fmt[offset + 1..]);
        fp.rate_max = combine_triple(&fmt[offset + 4..]);
    }
    Ok(())
}

//
// UAC2 sample rate parsing (stub - requires clock.rs Phase 5)
//
fn parse_audio_format_rates_v2(
    _dev: &kernel::usb::Device,
    fp: &mut AudioFormat,
) -> Result<()> {
    // Phase 5 implements the UAC2_CS_RANGE clock-source query.
    fp.rates    = bindings::SNDRV_PCM_RATE_CONTINUOUS;
    fp.rate_min = 8000;
    fp.rate_max = 192000;
    pr_info!(
        "snd_usb_audio: {}:{}: UAC2 rate query deferred (Phase 5)\n",
        fp.iface, fp.altsetting
    );
    Ok(())
}

//
// Top-level entry point
//
/// Parses a UAC1 or UAC2 Type I audio format descriptor into `fp`.
///
/// `fmt` is the raw descriptor byte slice (starting at `bLength`).
/// `format` is `wFormatTag` (UAC1) or `bmFormats` (UAC2).
pub(crate) fn parse_audio_format(
    dev: &kernel::usb::Device,
    usb_id: u32,
    fp: &mut AudioFormat,
    fmt: &[u8],
    format: u64,
) -> Result<()> {
    let fmt_type = if fmt.len() >= 4 { fmt[3] } else { 0 };
    if fmt_type != UAC_FORMAT_TYPE_I {
        return Err(ENODEV);
    }
    fp.fmt_type = fmt_type;

    let fmtbits = parse_audio_format_i_type(usb_id, fp, format, fmt);
    if fmtbits == 0 {
        pr_info!(
            "snd_usb_audio: {:04x}:{:04x} {}:{}: cannot determine format bits\n",
            usb_id >> 16, usb_id & 0xffff, fp.iface, fp.altsetting
        );
        return Err(EINVAL);
    }
    fp.formats = fmtbits;

    match fp.protocol {
        UAC_VERSION_1 => {
            if fmt.len() < 5 {
                return Err(EINVAL);
            }
            fp.channels = fmt[4] as u32;
            parse_audio_format_rates_v1(usb_id, fp, fmt, 7)?;
        }
        UAC_VERSION_2 => {
            // Channels are set by the caller from the AS general descriptor.
            parse_audio_format_rates_v2(dev, fp)?;
        }
        _ => return Err(ENODEV),
    }

    if fp.channels < 1 {
        pr_err!(
            "snd_usb_audio: {:04x}:{:04x} {}:{}: invalid channel count {}\n",
            usb_id >> 16, usb_id & 0xffff, fp.iface, fp.altsetting, fp.channels
        );
        return Err(EINVAL);
    }

    Ok(())
}
