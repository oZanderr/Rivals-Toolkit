//! OGG Vorbis decoder. Decodes an OGG Vorbis stream to raw 16-bit PCM via lewton.

use std::io::Cursor;

use lewton::inside_ogg::OggStreamReader;

use super::pcm::WavValidation;

/// Decoded PCM output from an OGG Vorbis stream.
pub(super) struct DecodedOgg {
    pub channels: u16,
    pub sample_rate: u32,
    /// Raw 16-bit little-endian PCM bytes (interleaved if stereo).
    pub pcm_bytes: Vec<u8>,
}

/// Fully decode an OGG Vorbis buffer to 16-bit PCM.
pub(super) fn decode_ogg(data: &[u8]) -> Result<DecodedOgg, String> {
    let mut reader = OggStreamReader::new(Cursor::new(data))
        .map_err(|e| format!("Failed to open OGG stream: {e}"))?;

    let channels = reader.ident_hdr.audio_channels as u16;
    let sample_rate = reader.ident_hdr.audio_sample_rate;

    if channels == 0 || channels > 2 {
        return Err(format!(
            "Unsupported OGG channel count: {channels} (expected 1 or 2)"
        ));
    }

    let mut pcm_samples: Vec<i16> = Vec::new();
    while let Some(packet) = reader
        .read_dec_packet_itl()
        .map_err(|e| format!("OGG decode error: {e}"))?
    {
        pcm_samples.extend_from_slice(&packet);
    }

    let pcm_bytes: Vec<u8> = pcm_samples.iter().flat_map(|s| s.to_le_bytes()).collect();

    Ok(DecodedOgg {
        channels,
        sample_rate,
        pcm_bytes,
    })
}

/// Validate an OGG Vorbis buffer (reads headers, counts samples, scans peak).
pub(super) fn validate_ogg(data: &[u8]) -> Result<WavValidation, String> {
    let mut reader = OggStreamReader::new(Cursor::new(data))
        .map_err(|e| format!("Failed to open OGG stream: {e}"))?;

    let channels = reader.ident_hdr.audio_channels as u16;
    let sample_rate = reader.ident_hdr.audio_sample_rate;

    let mut total_samples: u64 = 0;
    let mut peak: i32 = 0;
    while let Some(packet) = reader
        .read_dec_packet_itl()
        .map_err(|e| format!("OGG decode error: {e}"))?
    {
        total_samples += packet.len() as u64;
        for s in &packet {
            let abs = (*s as i32).unsigned_abs() as i32;
            if abs > peak {
                peak = abs;
            }
        }
    }

    let total_frames = if channels > 0 {
        total_samples / channels as u64
    } else {
        0
    };
    let duration = if sample_rate > 0 {
        total_frames as f64 / sample_rate as f64
    } else {
        0.0
    };
    let peak_dbfs = if peak == 0 {
        f32::NEG_INFINITY
    } else {
        20.0 * (peak as f32 / i16::MAX as f32).log10()
    };

    Ok(WavValidation {
        channels,
        sample_rate,
        bits_per_sample: 16,
        duration,
        peak_dbfs,
    })
}
