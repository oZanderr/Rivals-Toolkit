//! WAV → WEM converter for Wwise PCM soundbanks.
//!
//! Converts a 16-bit PCM WAV file (mono or stereo) to a WEM file by replacing
//! the WAV header with a Wwise-compatible RIFF header. Mono inputs are
//! automatically upmixed to stereo by duplicating each sample.

use std::fs;
use std::path::Path;

#[derive(Debug)]
pub(crate) enum ConvertError {
    Io(std::io::Error),
    /// Malformed RIFF/WAV structure.
    InvalidWav(String),
    /// Audio format that cannot be converted (e.g. mono, 24-bit).
    UnsupportedFormat(String),
}

impl std::fmt::Display for ConvertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConvertError::Io(e) => write!(f, "IO error: {e}"),
            ConvertError::InvalidWav(msg) => write!(f, "Invalid WAV: {msg}"),
            ConvertError::UnsupportedFormat(msg) => write!(f, "Unsupported format: {msg}"),
        }
    }
}

impl std::error::Error for ConvertError {}

impl From<std::io::Error> for ConvertError {
    fn from(e: std::io::Error) -> Self {
        ConvertError::Io(e)
    }
}

pub(crate) type Result<T> = std::result::Result<T, ConvertError>;

struct WavInfo {
    channels: u16,
    sample_rate: u32,
    bits_per_sample: u16,
    /// Byte offset where the raw PCM data begins.
    data_offset: usize,
    /// Size of the raw PCM data in bytes.
    data_size: u32,
}

#[inline]
fn read_u16_le(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

#[inline]
fn read_u32_le(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

/// Walks the RIFF/WAV chunk structure and extracts format + data info.
fn parse_wav(data: &[u8]) -> Result<WavInfo> {
    if data.len() < 12 {
        return Err(ConvertError::InvalidWav(
            "file too small for RIFF header".into(),
        ));
    }
    if &data[0..4] != b"RIFF" {
        return Err(ConvertError::InvalidWav("missing RIFF tag".into()));
    }
    if &data[8..12] != b"WAVE" {
        return Err(ConvertError::InvalidWav("missing WAVE form type".into()));
    }

    let mut channels: Option<u16> = None;
    let mut sample_rate: Option<u32> = None;
    let mut bits_per_sample: Option<u16> = None;
    let mut data_offset: Option<usize> = None;
    let mut data_size: Option<u32> = None;

    let mut pos = 12usize;

    while pos + 8 <= data.len() {
        let chunk_id = &data[pos..pos + 4];
        let chunk_size = read_u32_le(data, pos + 4) as usize;
        let chunk_data_start = pos + 8;

        if chunk_id == b"fmt " {
            if chunk_size < 16 {
                return Err(ConvertError::InvalidWav("fmt chunk too small".into()));
            }
            let format_tag = read_u16_le(data, chunk_data_start);
            // 1 = PCM, 0xFFFE = WAVE_FORMAT_EXTENSIBLE (PCM sub-type)
            if format_tag != 1 && format_tag != 0xFFFE {
                return Err(ConvertError::UnsupportedFormat(format!(
                    "format tag {format_tag:#06X} is not PCM"
                )));
            }
            channels = Some(read_u16_le(data, chunk_data_start + 2));
            sample_rate = Some(read_u32_le(data, chunk_data_start + 4));
            bits_per_sample = Some(read_u16_le(data, chunk_data_start + 14));
        } else if chunk_id == b"data" {
            data_offset = Some(chunk_data_start);
            data_size = Some(chunk_size as u32);
        }

        // Advance to next chunk (sizes are word-aligned in RIFF)
        pos = chunk_data_start + ((chunk_size + 1) & !1);
    }

    Ok(WavInfo {
        channels: channels.ok_or_else(|| ConvertError::InvalidWav("no fmt chunk found".into()))?,
        sample_rate: sample_rate
            .ok_or_else(|| ConvertError::InvalidWav("no fmt chunk found".into()))?,
        bits_per_sample: bits_per_sample
            .ok_or_else(|| ConvertError::InvalidWav("no fmt chunk found".into()))?,
        data_offset: data_offset
            .ok_or_else(|| ConvertError::InvalidWav("no data chunk found".into()))?,
        data_size: data_size
            .ok_or_else(|| ConvertError::InvalidWav("no data chunk found".into()))?,
    })
}

/// Stereo WEM header template (96 bytes).
///
/// Wwise-style RIFF container wrapping raw 16-bit PCM stereo.
/// RIFF-size, sample-rate, and avg-bytes-per-sec fields are patched at runtime
/// by [`build_wem_header`]; everything else is fixed.
const STEREO_HEADER_TEMPLATE: [u8; 96] = [
    // "RIFF" + placeholder size + "WAVE"
    0x52, 0x49, 0x46, 0x46, 0x00, 0x00, 0x00, 0x00, 0x57, 0x41, 0x56, 0x45,
    // "fmt " chunk  (size = 0x18 = 24, format = 0xFFFE extensible, channels = 2)
    0x66, 0x6D, 0x74, 0x20, 0x18, 0x00, 0x00, 0x00, 0xFE, 0xFF, 0x02, 0x00,
    // sample rate 48000 (0xBB80 LE) — patched at runtime
    0x80, 0xBB, 0x00, 0x00, // avg bytes/sec — patched at runtime
    0x00, 0x77, 0x01, 0x00, // block align = 4, bits per sample = 16
    0x04, 0x00, 0x10, 0x00, // extensible extra fields
    0x06, 0x00, 0x00, 0x00, 0x02, 0x31, 0x00, 0x00,
    // "hash" sub-chunk (16 bytes of opaque game-specific data)
    0x68, 0x61, 0x73, 0x68, 0x10, 0x00, 0x00, 0x00, 0x46, 0x26, 0xE0, 0xBF, 0x91, 0x29, 0x78, 0xDD,
    0x78, 0x67, 0x99, 0x9C, 0xA4, 0x66, 0xBA, 0x21,
    // "junk" sub-chunk (12 bytes payload)
    0x6A, 0x75, 0x6E, 0x6B, 0x0C, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // "JUNK" sub-chunk (4 bytes payload)
    0x4A, 0x55, 0x4E, 0x4B, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// Builds the complete WEM file header with RIFF size, sample rate,
/// avg-bytes/sec, and data-chunk size patched for the given audio.
fn build_wem_header(pcm_len: u32, sample_rate: u32) -> Vec<u8> {
    let riff_size: u32 = 80 + pcm_len;

    let mut header = Vec::with_capacity(104);
    header.extend_from_slice(&STEREO_HEADER_TEMPLATE[0..4]); // "RIFF"
    header.extend_from_slice(&riff_size.to_le_bytes()); // patched size
    header.extend_from_slice(&STEREO_HEADER_TEMPLATE[8..]); // rest of template
    header.extend_from_slice(b"data");
    header.extend_from_slice(&pcm_len.to_le_bytes());

    // Patch sample rate at offset 24..28
    header[24..28].copy_from_slice(&sample_rate.to_le_bytes());
    // Patch avg bytes/sec at offset 28..32  (sample_rate × 4 bytes per stereo frame)
    let avg_bytes_sec: u32 = sample_rate * 4;
    header[28..32].copy_from_slice(&avg_bytes_sec.to_le_bytes());

    header
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct WavValidation {
    pub channels: u16,
    pub sample_rate: u32,
    pub bits_per_sample: u16,
    pub duration: f64,
}

/// Convert a WAV file directly to in-memory WEM bytes.
pub(crate) fn convert_to_bytes(input: &Path) -> Result<(Vec<u8>, u32)> {
    let data = fs::read(input)?;
    let info = parse_wav(&data)?;

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

    let pcm = &data[info.data_offset..end];

    if info.channels == 1 {
        // Upmix mono → stereo by duplicating each 16-bit sample.
        let stereo_size = info.data_size * 2;
        let mut out = build_wem_header(stereo_size, info.sample_rate);
        out.reserve(stereo_size as usize);
        for sample in pcm.chunks_exact(2) {
            out.extend_from_slice(sample); // left
            out.extend_from_slice(sample); // right
        }
        Ok((out, info.sample_rate))
    } else {
        let mut out = build_wem_header(info.data_size, info.sample_rate);
        out.extend_from_slice(pcm);
        Ok((out, info.sample_rate))
    }
}

/// Validate a WAV file without converting it.
/// Returns (channels, sample_rate, bits_per_sample, duration_secs).
pub(crate) fn validate_wav(input: &Path) -> Result<WavValidation> {
    let data = fs::read(input)?;
    let info = parse_wav(&data)?;

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
