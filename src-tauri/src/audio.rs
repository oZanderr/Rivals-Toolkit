//! Audio decode/encode pipeline for the sound mod builder. Detects WAV/OGG by magic bytes and packs to Wwise WEM with optional gain.

mod ogg;
mod pcm;
pub(crate) mod vgmstream;
mod wav;
mod wem;

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use tauri::State;

use crate::settings::SettingsState;

pub(crate) use pcm::{ConvertError, WavValidation};
pub(crate) use wem::{build_wem_header, wem_to_wav};

use pcm::{Result, apply_gain_in_place, db_to_linear, needs_scaling, peak_dbfs_i16_le};
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

    // Peak scan only meaningful for the 16-bit PCM we know how to convert.
    // For other bit depths the converter rejects the file anyway.
    let peak_dbfs = if info.bits_per_sample == 16 {
        let end = info.data_offset + info.data_size as usize;
        if end <= data.len() {
            peak_dbfs_i16_le(&data[info.data_offset..end])
        } else {
            f32::NEG_INFINITY
        }
    } else {
        f32::NEG_INFINITY
    };

    Ok(WavValidation {
        channels: info.channels,
        sample_rate: info.sample_rate,
        bits_per_sample: info.bits_per_sample,
        duration,
        peak_dbfs,
    })
}

#[tauri::command]
pub(crate) fn validate_wav(path: String) -> std::result::Result<WavValidation, String> {
    validate_audio(Path::new(&path)).map_err(|e| e.to_string())
}

/// Confirm a user-supplied path is a working vgmstream-cli, returning its version banner.
#[tauri::command]
pub(crate) async fn validate_vgmstream(path: String) -> std::result::Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        vgmstream::probe_version(Path::new(&path)).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Whether a usable vgmstream-cli is configured (gates the Asset Manager extract action).
#[tauri::command]
pub(crate) fn vgmstream_available(state: State<'_, SettingsState>) -> bool {
    let configured = state.lock().ok().and_then(|s| s.vgmstream_path.clone());
    vgmstream::resolve(configured.as_deref()).is_some()
}

/// Extract a `.wem` entry from a pak/utoc and decode it to a `.wav` in `output_dir`.
#[tauri::command]
pub(crate) async fn extract_wem_entry_as_wav(
    state: State<'_, SettingsState>,
    source_path: String,
    entry: String,
    is_utoc: bool,
    output_dir: String,
) -> std::result::Result<String, String> {
    let configured = state.lock().ok().and_then(|s| s.vgmstream_path.clone());
    tauri::async_runtime::spawn_blocking(move || {
        let bin = vgmstream::resolve(configured.as_deref())
            .ok_or_else(|| "vgmstream is not configured. Set its path in Settings.".to_string())?;
        extract_wem_as_wav(&source_path, &entry, is_utoc, &output_dir, &bin)
    })
    .await
    .map_err(|e| e.to_string())?
}

static EXTRACT_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Read the raw `.wem` to a temp file via the existing pak/utoc extractor, decode it, and write
/// `<stem>.wav` into `output_dir`. Returns the written WAV path.
fn extract_wem_as_wav(
    source: &str,
    entry: &str,
    is_utoc: bool,
    output_dir: &str,
    bin: &Path,
) -> std::result::Result<String, String> {
    let stem = Path::new(entry)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("audio");

    let tmp = std::env::temp_dir().join(format!(
        "rivals_wem_{}_{}.wem",
        std::process::id(),
        EXTRACT_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let tmp_str = tmp.to_string_lossy().into_owned();

    if is_utoc {
        crate::pak::extract_utoc_file(source, entry, &tmp_str)?;
    } else {
        crate::pak::extract_single_file(source, entry, &tmp_str)?;
    }

    let wav = vgmstream::decode_file_to_wav(bin, &tmp, None).map_err(|e| e.to_string());
    let _ = fs::remove_file(&tmp);
    let wav = wav?;

    let out_path = Path::new(output_dir).join(format!("{stem}.wav"));
    fs::write(&out_path, wav).map_err(|e| e.to_string())?;
    Ok(out_path.to_string_lossy().into_owned())
}
