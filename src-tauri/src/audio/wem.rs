//! Wwise WEM (RIFF/PCM) header writer, encoder, and decoder.

use super::pcm::{ConvertError, Result};
use super::wav::{parse_riff, write_canonical_wav};

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
pub(crate) fn build_wem_header(pcm_len: u32, sample_rate: u32) -> Vec<u8> {
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

/// Build a stereo WEM from raw 16-bit PCM bytes.
/// Mono input is upmixed by duplicating each sample to both channels.
pub(super) fn pcm_to_wem(pcm: &[u8], channels: u16, sample_rate: u32) -> Result<(Vec<u8>, u32)> {
    if channels != 1 && channels != 2 {
        return Err(ConvertError::UnsupportedFormat(format!(
            "expected 1 or 2 channels, got {channels}"
        )));
    }

    if channels == 1 {
        let stereo_size = pcm.len() as u32 * 2;
        let mut out = build_wem_header(stereo_size, sample_rate);
        out.reserve(stereo_size as usize);
        for sample in pcm.chunks_exact(2) {
            out.extend_from_slice(sample); // left
            out.extend_from_slice(sample); // right
        }
        Ok((out, sample_rate))
    } else {
        let mut out = build_wem_header(pcm.len() as u32, sample_rate);
        out.extend_from_slice(pcm);
        Ok((out, sample_rate))
    }
}

/// Convert raw WEM bytes to a standard WAV file in memory.
///
/// Uncompressed PCM WEMs (everything this app builds) decode in-process via the shared
/// RIFF parser. Wwise-compressed WEMs (Vorbis/Opus/ADPCM) make `parse_riff` return
/// `UnsupportedFormat`; those fall back to the user-supplied vgmstream-cli when `vgmstream`
/// is `Some`, and otherwise return a clear "configure vgmstream" error.
pub(crate) fn wem_to_wav(wem: &[u8], vgmstream: Option<&std::path::Path>) -> Result<Vec<u8>> {
    match try_pcm_wem_to_wav(wem) {
        Ok(wav) => Ok(wav),
        Err(ConvertError::UnsupportedFormat(_)) => match vgmstream {
            Some(bin) => super::vgmstream::decode_bytes_to_wav(bin, wem),
            None => Err(ConvertError::Vgmstream(
                "this WEM uses a compressed codec. Set a vgmstream-cli path in Settings to extract it."
                    .into(),
            )),
        },
        Err(e) => Err(e),
    }
}

/// In-process decode for uncompressed PCM WEMs. The WEM format uses the same RIFF chunk
/// structure as WAV, so the shared [`parse_riff`] finds the `fmt ` and `data` chunks; we then
/// emit a canonical 44-byte WAV header followed by the raw PCM data.
fn try_pcm_wem_to_wav(wem: &[u8]) -> Result<Vec<u8>> {
    let info = parse_riff(wem)?;

    if info.channels == 0 || info.channels > 2 {
        return Err(ConvertError::UnsupportedFormat(format!(
            "expected 1 or 2 channels in WEM, got {}",
            info.channels
        )));
    }
    if info.bits_per_sample != 16 {
        return Err(ConvertError::UnsupportedFormat(format!(
            "expected 16-bit WEM, got {}-bit",
            info.bits_per_sample
        )));
    }

    let end = info.data_offset + info.data_size as usize;
    if end > wem.len() {
        return Err(ConvertError::InvalidWav(
            "data chunk extends past end of WEM".into(),
        ));
    }

    let pcm = &wem[info.data_offset..end];
    Ok(write_canonical_wav(
        pcm,
        info.channels,
        info.sample_rate,
        info.bits_per_sample,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal RIFF/WAVE whose `fmt ` declares a non-PCM codec tag, so `parse_riff`
    /// returns `UnsupportedFormat` (the compressed-WEM path).
    fn non_pcm_riff() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&0u32.to_le_bytes());
        v.extend_from_slice(b"WAVE");
        v.extend_from_slice(b"fmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&0x0166u16.to_le_bytes()); // non-PCM codec tag
        v.extend_from_slice(&2u16.to_le_bytes());
        v.extend_from_slice(&48_000u32.to_le_bytes());
        v.extend_from_slice(&0u32.to_le_bytes());
        v.extend_from_slice(&4u16.to_le_bytes());
        v.extend_from_slice(&16u16.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&0u32.to_le_bytes());
        v
    }

    #[test]
    fn pcm_wem_decodes_without_vgmstream() {
        let mut wem = build_wem_header(16, 48_000);
        wem.extend_from_slice(&[0u8; 16]);
        let result = wem_to_wav(&wem, None);
        assert!(
            result.is_ok(),
            "uncompressed PCM WEM should decode in-process"
        );
    }

    #[test]
    fn compressed_wem_without_vgmstream_returns_configure_error() {
        match wem_to_wav(&non_pcm_riff(), None) {
            Err(ConvertError::Vgmstream(_)) => {}
            other => panic!("expected a Vgmstream error, got {other:?}"),
        }
    }
}
