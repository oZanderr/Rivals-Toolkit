//! Decrypts and inflates an engine log's records into the text the game wrote, under the bundled or a manual key.

use aes::Aes128;
use aes::cipher::{BlockEncrypt, KeyInit};
use flate2::{Decompress, FlushDecompress, Status};
use serde::{Deserialize, Serialize};

use super::container::{self, Container, Record};

/// The AES-128 key and CTR IV the shipping build writes its log under. Build constants, unchanged
/// across at least builds 3922109 and 3939753.
const BUNDLED_KEY: [u8; 16] = *b"ezFAgrXnhBOZgFFD";
const BUNDLED_IV: [u8; 16] = *b"_NMxNi0DK7_qHC_i";

/// Written between the last record's text and the plaintext trailer, as `marvel-log.py` writes
/// it, so line numbers agree with that script's output.
pub const TRAILER_SEPARATOR: &str =
    "----- <SHM_LOG_BEGIN> (plaintext shared-memory trailer) -----\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeySource {
    Bundled,
    Manual,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogKey {
    pub key: [u8; 16],
    pub iv: [u8; 16],
    pub source: KeySource,
}

fn parse_hex_16(name: &str, text: &str) -> Result<[u8; 16], String> {
    let cleaned: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = hex::decode(&cleaned).map_err(|_| format!("the log {name} is not hex"))?;
    bytes
        .try_into()
        .map_err(|_| format!("the log {name} must be 16 bytes (32 hex characters)"))
}

impl LogKey {
    pub fn bundled() -> Self {
        Self {
            key: BUNDLED_KEY,
            iv: BUNDLED_IV,
            source: KeySource::Bundled,
        }
    }

    pub fn manual(key: &str, iv: &str) -> Result<Self, String> {
        Ok(Self {
            key: parse_hex_16("key", key)?,
            iv: parse_hex_16("IV", iv)?,
            source: KeySource::Manual,
        })
    }

    /// The manual key when both halves are set, else the bundled one.
    pub fn resolve(key: Option<&str>, iv: Option<&str>) -> Result<Self, String> {
        match (
            key.filter(|k| !k.trim().is_empty()),
            iv.filter(|v| !v.trim().is_empty()),
        ) {
            (Some(key), Some(iv)) => Self::manual(key, iv),
            (None, None) => Ok(Self::bundled()),
            _ => Err("the manual log key needs both a key and an IV".to_string()),
        }
    }

    /// AES-128-CTR with a big-endian 128-bit counter starting at the IV, as Crypto++'s
    /// `CTR_Mode` runs it. Every record starts the counter over.
    fn apply(&self, data: &mut [u8]) {
        const BATCH: usize = 64;
        let cipher = Aes128::new(&self.key.into());
        let mut counter = u128::from_be_bytes(self.iv);
        let mut stream = [aes::Block::default(); BATCH];
        for chunk in data.chunks_mut(16 * BATCH) {
            let blocks = chunk.len().div_ceil(16);
            for block in &mut stream[..blocks] {
                *block = counter.to_be_bytes().into();
                counter = counter.wrapping_add(1);
            }
            cipher.encrypt_blocks(&mut stream[..blocks]);
            for (at, byte) in chunk.iter_mut().enumerate() {
                *byte ^= stream[at / 16][at % 16];
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordKind {
    Zlib,
    /// A tail record cut short when the game died mid-write; what inflated is kept.
    ZlibPartial,
    /// Looked like zlib and would not inflate at all: the wrong key, as a rule.
    ZlibBroken,
    /// The engine encrypts the text uncompressed when compression fails.
    Raw,
}

pub fn decrypt_record(key: &LogKey, payload: &[u8]) -> (Vec<u8>, RecordKind) {
    let mut plain = payload.to_vec();
    key.apply(&mut plain);
    if plain.first() != Some(&0x78) {
        return (plain, RecordKind::Raw);
    }
    match inflate(&plain) {
        (out, true) => (out, RecordKind::Zlib),
        (out, false) if !out.is_empty() => (out, RecordKind::ZlibPartial),
        _ => (plain, RecordKind::ZlibBroken),
    }
}

/// Inflates a zlib stream, saying whether it reached its end: a reader stops quietly at a
/// truncated stream's last byte.
fn inflate(input: &[u8]) -> (Vec<u8>, bool) {
    let mut stream = Decompress::new(true);
    let mut out = Vec::with_capacity(input.len() * 8);
    loop {
        if out.capacity() - out.len() < 32 * 1024 {
            out.reserve(out.capacity().max(64 * 1024));
        }
        let consumed = usize::try_from(stream.total_in()).unwrap_or(input.len());
        let produced = out.len();
        let rest = input.get(consumed..).unwrap_or_default();
        match stream.decompress_vec(rest, &mut out, FlushDecompress::None) {
            Ok(Status::StreamEnd) => return (out, true),
            Ok(_) if stream.total_in() as usize == consumed && out.len() == produced => {
                return (out, false);
            }
            Ok(_) => {}
            Err(_) => return (out, false),
        }
    }
}

/// Whether the key opens this record: it inflates, or it is readable text. A wrong key turns a
/// raw record into noise that is never valid UTF-8 for long.
fn key_opens(text: &[u8], kind: RecordKind) -> bool {
    match kind {
        RecordKind::Zlib | RecordKind::ZlibPartial => true,
        RecordKind::ZlibBroken => false,
        RecordKind::Raw => {
            let sample = &text[..text.len().min(4096)];
            let sample = match std::str::from_utf8(sample) {
                Ok(sample) => sample,
                // A character cut by the end of the sample, not a bad byte.
                Err(e) if e.error_len().is_none() => {
                    std::str::from_utf8(&sample[..e.valid_up_to()]).unwrap_or_default()
                }
                Err(_) => return false,
            };
            let total = sample.chars().count().max(1);
            let printable = sample
                .chars()
                .filter(|c| !c.is_control() || c.is_whitespace())
                .count();
            printable * 100 >= total * 95
        }
    }
}

pub fn wrong_key_error(key: &LogKey) -> String {
    match key.source {
        KeySource::Bundled => "The bundled log key does not open this log. A game patch has \
            likely changed it; enter the new key in Settings."
            .to_string(),
        KeySource::Manual => "The manual log key in Settings does not open this log. Correct it, \
            or clear it to use the bundled key."
            .to_string(),
    }
}

/// Decrypts the given records, refusing when the first one shows the key is wrong.
pub fn decrypt_records(
    key: &LogKey,
    records: &[Record<'_>],
) -> Result<Vec<(Vec<u8>, RecordKind)>, String> {
    let mut out = Vec::with_capacity(records.len());
    for (index, record) in records.iter().enumerate() {
        let (text, kind) = decrypt_record(key, record.payload);
        if index == 0 && !key_opens(&text, kind) {
            return Err(wrong_key_error(key));
        }
        out.push((text, kind));
    }
    Ok(out)
}

#[derive(Debug, Clone)]
pub struct DecodedLog {
    pub bytes: Vec<u8>,
    /// None when the log had no records to decrypt.
    pub key_source: Option<KeySource>,
    pub records: usize,
    /// Where the trailer's text starts in `bytes`, after the separator line.
    pub trailer_offset: Option<usize>,
    pub notes: Vec<String>,
}

/// The whole text of a log file, records first and then the trailer, as `marvel-log.py` writes it.
pub fn decode(blob: &[u8], key: &LogKey) -> Result<DecodedLog, String> {
    let parsed = container::parse(blob)?;
    decode_container(&parsed, key)
}

pub fn decode_container(parsed: &Container<'_>, key: &LogKey) -> Result<DecodedLog, String> {
    let texts = decrypt_records(key, &parsed.records)?;
    let mut notes: Vec<String> = parsed.note.iter().cloned().collect();
    let partial = texts
        .iter()
        .filter(|(_, kind)| matches!(kind, RecordKind::ZlibPartial | RecordKind::ZlibBroken))
        .count();
    if partial > 0 {
        notes.push(format!(
            "{partial} record(s) were cut short and are only partly readable"
        ));
    }

    let capacity = texts.iter().map(|(t, _)| t.len()).sum::<usize>()
        + parsed
            .trailer
            .map_or(0, |t| t.len() + TRAILER_SEPARATOR.len() + 1);
    let mut bytes = Vec::with_capacity(capacity);
    let last_ends_in_newline = texts.last().is_none_or(|(t, _)| t.ends_with(b"\n"));
    for (text, _) in &texts {
        bytes.extend_from_slice(text);
    }
    let mut trailer_offset = None;
    if let Some(trailer) = parsed.trailer {
        if !texts.is_empty() && !last_ends_in_newline {
            bytes.push(b'\n');
        }
        bytes.extend_from_slice(TRAILER_SEPARATOR.as_bytes());
        trailer_offset = Some(bytes.len());
        bytes.extend_from_slice(trailer);
    }
    Ok(DecodedLog {
        bytes,
        key_source: (!parsed.records.is_empty()).then_some(key.source),
        records: parsed.records.len(),
        trailer_offset,
        notes,
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
pub(crate) mod tests {
    use std::io::Write;

    use flate2::Compression;
    use flate2::write::ZlibEncoder;

    use super::super::container::tests::frame;
    use super::*;

    pub(crate) fn seal(key: &LogKey, text: &[u8], compress: bool) -> Vec<u8> {
        let mut payload = if compress {
            let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
            encoder.write_all(text).unwrap();
            encoder.finish().unwrap()
        } else {
            text.to_vec()
        };
        key.apply(&mut payload);
        payload
    }

    /// A log file the game could have written: each text a compressed, encrypted record.
    pub(crate) fn log_file(key: &LogKey, texts: &[&str], trailer: Option<&str>) -> Vec<u8> {
        let sealed: Vec<Vec<u8>> = texts
            .iter()
            .map(|t| seal(key, t.as_bytes(), true))
            .collect();
        let payloads: Vec<&[u8]> = sealed.iter().map(Vec::as_slice).collect();
        frame(&payloads, trailer.map(str::as_bytes))
    }

    #[test]
    fn the_counter_matches_aes_ctr_from_the_iv() {
        // NIST SP 800-38A F.5.1, CTR-AES128.Encrypt, first block.
        let key = LogKey {
            key: hex::decode("2b7e151628aed2a6abf7158809cf4f3c")
                .unwrap()
                .try_into()
                .unwrap(),
            iv: hex::decode("f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff")
                .unwrap()
                .try_into()
                .unwrap(),
            source: KeySource::Manual,
        };
        let mut block =
            hex::decode("6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e51")
                .unwrap();
        key.apply(&mut block);
        assert_eq!(
            hex::encode(block),
            "874d6191b620e3261bef6864990db6ce9806f66b7970fdff8617187bb9fffdff"
        );
    }

    #[test]
    fn records_and_trailer_join_as_the_script_writes_them() {
        let key = LogKey::bundled();
        let blob = log_file(&key, &["first\n", "second"], Some("tail\n"));
        let decoded = decode(&blob, &key).unwrap();
        let expected = format!("first\nsecond\n{TRAILER_SEPARATOR}tail\n");
        assert_eq!(String::from_utf8(decoded.bytes.clone()).unwrap(), expected);
        assert_eq!(decoded.records, 2);
        assert_eq!(decoded.key_source, Some(KeySource::Bundled));
        assert_eq!(
            decoded.trailer_offset,
            Some(expected.len() - "tail\n".len())
        );
    }

    #[test]
    fn a_raw_record_opens_with_the_right_key() {
        let key = LogKey::bundled();
        let payload = seal(&key, b"LogInit: Display: plain text\n", false);
        let blob = frame(&[&payload], None);
        let decoded = decode(&blob, &key).unwrap();
        assert_eq!(decoded.bytes, b"LogInit: Display: plain text\n");
    }

    #[test]
    fn a_cut_record_keeps_what_inflated() {
        let key = LogKey::bundled();
        let text = "line of log text\n".repeat(400);
        let mut payload = seal(&key, text.as_bytes(), true);
        payload.truncate(payload.len() - 20);
        let blob = frame(&[&payload], None);
        let decoded = decode(&blob, &key).unwrap();
        assert!(!decoded.bytes.is_empty());
        assert!(text.as_bytes().starts_with(&decoded.bytes));
        assert_eq!(decoded.notes.len(), 1);
    }

    #[test]
    fn the_wrong_key_is_refused_naming_where_it_came_from() {
        let blob = log_file(&LogKey::bundled(), &["LogInit: Display: hello\n"], None);
        let manual = LogKey::manual(&"11".repeat(16), &"22".repeat(16)).unwrap();
        let error = decode(&blob, &manual).unwrap_err();
        assert!(error.contains("manual log key"));
    }

    #[test]
    fn a_manual_key_wins_and_half_a_key_is_refused() {
        let manual = LogKey::resolve(Some(&"ab".repeat(16)), Some(&"cd".repeat(16))).unwrap();
        assert_eq!(manual.source, KeySource::Manual);
        assert_eq!(manual.key, [0xab; 16]);
        assert_eq!(
            LogKey::resolve(None, Some("")).unwrap().source,
            KeySource::Bundled
        );
        assert!(LogKey::resolve(Some(&"ab".repeat(16)), None).is_err());
        assert!(LogKey::resolve(Some("abc"), Some("def")).is_err());
    }

    #[test]
    fn a_log_with_no_records_needs_no_key() {
        let blob = frame(&[], Some(b"ring"));
        let manual = LogKey::manual(&"11".repeat(16), &"22".repeat(16)).unwrap();
        let decoded = decode(&blob, &manual).unwrap();
        assert_eq!(decoded.key_source, None);
        assert_eq!(
            decoded.bytes,
            format!("{TRAILER_SEPARATOR}ring").into_bytes()
        );
    }
}
