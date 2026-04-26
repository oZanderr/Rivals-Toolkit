//! Shared types and PCM helpers for the audio pipeline.

#[derive(Debug)]
pub(crate) enum ConvertError {
    Io(std::io::Error),
    /// Malformed RIFF/WAV structure.
    InvalidWav(String),
    /// Audio format that cannot be converted (e.g. 24-bit, surround).
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

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct WavValidation {
    pub channels: u16,
    pub sample_rate: u32,
    pub bits_per_sample: u16,
    pub duration: f64,
    /// Peak sample amplitude in dBFS (0 = full-scale, -inf = silent).
    /// Used by the UI to predict post-gain clipping before building.
    pub peak_dbfs: f32,
}

/// Scan 16-bit LE PCM and return the peak in dBFS (negative for sub-peak,
/// `f32::NEG_INFINITY` for silent input).
pub(crate) fn peak_dbfs_i16_le(pcm: &[u8]) -> f32 {
    let mut peak: i32 = 0;
    for chunk in pcm.chunks_exact(2) {
        let s = i16::from_le_bytes([chunk[0], chunk[1]]);
        let abs = (s as i32).unsigned_abs() as i32;
        if abs > peak {
            peak = abs;
        }
    }
    if peak == 0 {
        return f32::NEG_INFINITY;
    }
    20.0 * (peak as f32 / i16::MAX as f32).log10()
}

/// Convert a gain in decibels to a linear sample multiplier.
/// 0 dB = 1.0 (no change). +6 dB ≈ 2.0, -6 dB ≈ 0.5.
pub(crate) fn db_to_linear(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// Returns true when `gain_linear` is far enough from 1.0 to require sample scaling.
pub(crate) fn needs_scaling(gain_linear: f32) -> bool {
    (gain_linear - 1.0).abs() >= 1e-4
}

/// Scale 16-bit LE PCM samples in place by a linear gain, clamping to i16 range.
/// No-ops when gain is within 1e-4 of unity.
pub(crate) fn apply_gain_in_place(pcm: &mut [u8], gain_linear: f32) {
    if !needs_scaling(gain_linear) {
        return;
    }
    for chunk in pcm.chunks_exact_mut(2) {
        let s = i16::from_le_bytes([chunk[0], chunk[1]]) as f32;
        let scaled = (s * gain_linear)
            .round()
            .clamp(i16::MIN as f32, i16::MAX as f32) as i16;
        let bytes = scaled.to_le_bytes();
        chunk[0] = bytes[0];
        chunk[1] = bytes[1];
    }
}
