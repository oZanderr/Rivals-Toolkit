//! Audio decode/encode pipeline for the hitsound builder. Detects WAV/OGG by magic bytes and packs to Wwise WEM with optional gain.

mod ogg;
mod pcm;
mod wav;
mod wem;

use std::fs;
use std::path::Path;

pub(crate) use pcm::{ConvertError, WavValidation};
pub(crate) use wem::{build_wem_header, wem_to_wav};

use pcm::{Result, apply_gain_in_place, db_to_linear, needs_scaling};
use wav::parse_riff;
use wem::pcm_to_wem;

/// Convert an audio file (WAV or OGG Vorbis) to in-memory WEM bytes,
/// scaling samples by `gain_db` before packing. 0 dB passes through;
/// over-amplification clamps to i16 range.
///
/// Format is detected by magic bytes: `RIFF` = WAV, `OggS` = OGG Vorbis.
pub(crate) fn convert_to_bytes_with_gain(input: &Path, gain_db: f32) -> Result<(Vec<u8>, u32)> {
    let data = fs::read(input)?;
    let gain_linear = db_to_linear(gain_db);
    let scale = needs_scaling(gain_linear);

    if data.starts_with(b"OggS") {
        let mut decoded = ogg::decode_ogg(&data).map_err(ConvertError::UnsupportedFormat)?;
        if scale {
            apply_gain_in_place(&mut decoded.pcm_bytes, gain_linear);
        }
        return pcm_to_wem(&decoded.pcm_bytes, decoded.channels, decoded.sample_rate);
    }

    let info = parse_riff(&data)?;

    if info.channels != 1 && info.channels != 2 {
        return Err(ConvertError::UnsupportedFormat(format!(
            "expected 1 or 2 channels, got {}",
            info.channels
        )));
    }
    if info.bits_per_sample != 16 {
        return Err(ConvertError::UnsupportedFormat(format!(
            "expected 16-bit samples, got {}-bit",
            info.bits_per_sample
        )));
    }

    let end = info.data_offset + info.data_size as usize;
    if end > data.len() {
        return Err(ConvertError::InvalidWav(
            "data chunk extends past end of file".into(),
        ));
    }

    if scale {
        let mut pcm = data[info.data_offset..end].to_vec();
        apply_gain_in_place(&mut pcm, gain_linear);
        pcm_to_wem(&pcm, info.channels, info.sample_rate)
    } else {
        let pcm = &data[info.data_offset..end];
        pcm_to_wem(pcm, info.channels, info.sample_rate)
    }
}

/// Validate an audio file (WAV or OGG Vorbis) without converting it.
pub(crate) fn validate_audio(input: &Path) -> Result<WavValidation> {
    let data = fs::read(input)?;

    if data.starts_with(b"OggS") {
        return ogg::validate_ogg(&data).map_err(ConvertError::UnsupportedFormat);
    }

    let info = parse_riff(&data)?;

    let bytes_per_sample = info.bits_per_sample as u32 / 8;
    let bytes_per_frame = bytes_per_sample * info.channels as u32;
    let total_frames = if bytes_per_frame > 0 {
        info.data_size / bytes_per_frame
    } else {
        0
    };
    let duration = if info.sample_rate > 0 {
        total_frames as f64 / info.sample_rate as f64
    } else {
        0.0
    };

    Ok(WavValidation {
        channels: info.channels,
        sample_rate: info.sample_rate,
        bits_per_sample: info.bits_per_sample,
        duration,
    })
}

#[tauri::command]
pub(crate) fn validate_wav(path: String) -> std::result::Result<WavValidation, String> {
    validate_audio(Path::new(&path)).map_err(|e| e.to_string())
}
