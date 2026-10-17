//! Reads and writes loose INI files on disk, keeping the encoding and line endings they came with.

use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];
const UTF16_LE_BOM: &[u8] = &[0xFF, 0xFE];
const UTF16_BE_BOM: &[u8] = &[0xFE, 0xFF];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IniEncoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
}

impl IniEncoding {
    fn of(bytes: &[u8]) -> Self {
        if bytes.starts_with(UTF8_BOM) {
            Self::Utf8Bom
        } else if bytes.starts_with(UTF16_LE_BOM) {
            Self::Utf16Le
        } else if bytes.starts_with(UTF16_BE_BOM) {
            Self::Utf16Be
        } else {
            Self::Utf8
        }
    }

    fn encode(self, text: &str) -> Vec<u8> {
        match self {
            Self::Utf8 => text.as_bytes().to_vec(),
            Self::Utf8Bom => [UTF8_BOM, text.as_bytes()].concat(),
            Self::Utf16Le => UTF16_LE_BOM
                .iter()
                .copied()
                .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
                .collect(),
            Self::Utf16Be => UTF16_BE_BOM
                .iter()
                .copied()
                .chain(text.encode_utf16().flat_map(u16::to_be_bytes))
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IniText {
    pub content: String,
    pub encoding: IniEncoding,
    /// Milliseconds since the Unix epoch, handed back to [`write`] to tell whether the file
    /// changed on disk in between.
    pub modified: Option<u64>,
}

fn modified_ms(meta: &fs::Metadata) -> Option<u64> {
    let since = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    u64::try_from(since.as_millis()).ok()
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn decode_utf16(units: &[u8], from: fn([u8; 2]) -> u16) -> Result<String, String> {
    if !units.len().is_multiple_of(2) {
        return Err("is UTF-16 with an odd number of bytes".to_string());
    }
    let units: Vec<u16> = units
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| from([pair[0], pair[1]]))
        .collect();
    String::from_utf16(&units).map_err(|e| format!("is not valid UTF-16: {e}"))
}

pub fn read(path: &Path) -> Result<IniText, String> {
    let bytes = fs::read(path).map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    let meta = fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let encoding = IniEncoding::of(&bytes);
    let content = match encoding {
        IniEncoding::Utf8 => String::from_utf8(bytes).map_err(|e| {
            format!(
                "{} is not valid UTF-8 and has no byte order mark: {e}",
                name_of(path)
            )
        }),
        IniEncoding::Utf8Bom => String::from_utf8(bytes[UTF8_BOM.len()..].to_vec())
            .map_err(|e| format!("{} is not valid UTF-8: {e}", name_of(path))),
        IniEncoding::Utf16Le => decode_utf16(&bytes[UTF16_LE_BOM.len()..], u16::from_le_bytes)
            .map_err(|e| format!("{} {e}", name_of(path))),
        IniEncoding::Utf16Be => decode_utf16(&bytes[UTF16_BE_BOM.len()..], u16::from_be_bytes)
            .map_err(|e| format!("{} {e}", name_of(path))),
    }?;
    Ok(IniText {
        content,
        encoding,
        modified: modified_ms(&meta),
    })
}

/// The encoding the file at `path` has now, from its first bytes; UTF-8 for one not there yet.
fn encoding_on_disk(path: &Path) -> IniEncoding {
    let mut head = [0u8; 3];
    let Ok(mut file) = fs::File::open(path) else {
        return IniEncoding::Utf8;
    };
    let read = file.read(&mut head).unwrap_or(0);
    IniEncoding::of(&head[..read])
}

/// Writes `content` to the `.ini` at `path` in the encoding it already has, through a temporary
/// file beside it so a failed write never leaves it cut short. With `expected_modified`, a file
/// that changed on disk since it was read is refused rather than written over. Says when it was
/// written, for the next save to check against.
pub fn write(path: &Path, content: &str, expected_modified: Option<u64>) -> Result<u64, String> {
    let name = name_of(path);
    if !name.to_ascii_lowercase().ends_with(".ini") {
        return Err(format!("{name} is not an .ini file"));
    }
    if let Ok(meta) = fs::metadata(path) {
        if meta.permissions().readonly() {
            return Err(format!(
                "{name} is read-only. Remove the read-only attribute and try again."
            ));
        }
        if expected_modified.is_some() && modified_ms(&meta) != expected_modified {
            return Err(format!(
                "{name} changed on disk since it was opened. Reload it first."
            ));
        }
    }
    let bytes = encoding_on_disk(path).encode(content);
    let temporary = path.with_file_name(format!(".{name}.tmp"));
    fs::write(&temporary, bytes).map_err(|error| {
        format!(
            "Could not write {}: {error}{}",
            temporary.display(),
            crate::mods::held_open_hint(&error)
        )
    })?;
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(format!(
            "Could not write {}: {error}{}",
            path.display(),
            crate::mods::held_open_hint(&error)
        ));
    }
    fs::metadata(path)
        .ok()
        .and_then(|meta| modified_ms(&meta))
        .ok_or_else(|| format!("{name} was written but its time could not be read"))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{Duration, SystemTime};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "rivals-ini-file-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            if let Ok(entries) = fs::read_dir(&self.0) {
                for entry in entries.flatten() {
                    if let Ok(meta) = entry.metadata() {
                        let mut permissions = meta.permissions();
                        #[allow(clippy::permissions_set_readonly_false)]
                        permissions.set_readonly(false);
                        let _ = fs::set_permissions(entry.path(), permissions);
                    }
                }
            }
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn round_trip(bytes: &[u8], encoding: IniEncoding) {
        let scratch = Scratch::new();
        let path = scratch.0.join("Config.ini");
        fs::write(&path, bytes).unwrap();

        let text = read(&path).expect("read");
        assert_eq!(text.encoding, encoding);
        assert_eq!(text.content, "[General]\nlog = 1\n");
        write(&path, &text.content, text.modified).expect("write");

        assert_eq!(fs::read(&path).unwrap(), bytes);
        let leftovers: Vec<_> = fs::read_dir(&scratch.0).unwrap().flatten().collect();
        assert_eq!(leftovers.len(), 1, "no temporary file left behind");
    }

    #[test]
    fn utf8_comes_back_as_it_went_in() {
        round_trip(b"[General]\nlog = 1\n", IniEncoding::Utf8);
    }

    #[test]
    fn a_byte_order_mark_is_kept() {
        round_trip(b"\xEF\xBB\xBF[General]\nlog = 1\n", IniEncoding::Utf8Bom);
    }

    #[test]
    fn utf16_is_written_back_as_utf16() {
        let mut bytes = UTF16_LE_BOM.to_vec();
        bytes.extend(
            "[General]\nlog = 1\n"
                .encode_utf16()
                .flat_map(u16::to_le_bytes),
        );
        round_trip(&bytes, IniEncoding::Utf16Le);
    }

    #[test]
    fn invalid_utf8_without_a_mark_is_refused() {
        let scratch = Scratch::new();
        let path = scratch.0.join("Bad.ini");
        fs::write(&path, b"key=\xFF\xFE\xFD").unwrap();
        // A file starting FF FE reads as UTF-16, so the bad bytes go after a valid start.
        let refused = read(&path).expect_err("not UTF-8");
        assert!(refused.contains("not valid UTF-8"), "{refused}");
    }

    #[test]
    fn only_ini_files_that_can_be_written_are() {
        let scratch = Scratch::new();
        let text = scratch.0.join("notes.txt");
        assert!(write(&text, "x", None).is_err());
        assert!(!text.exists());

        let locked = scratch.0.join("Locked.ini");
        fs::write(&locked, b"old").unwrap();
        let mut permissions = fs::metadata(&locked).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&locked, permissions).unwrap();
        let refused = write(&locked, "new", None).expect_err("read-only");
        assert!(refused.contains("read-only"), "{refused}");
        assert_eq!(fs::read(&locked).unwrap(), b"old");
    }

    #[test]
    fn a_file_changed_since_it_was_read_is_not_written_over() {
        let scratch = Scratch::new();
        let path = scratch.0.join("Plugin.ini");
        fs::write(&path, b"mine").unwrap();
        let text = read(&path).expect("read");

        fs::write(&path, b"the plugin's").unwrap();
        let later = SystemTime::now() + Duration::from_secs(5);
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(later)
            .unwrap();

        let refused = write(&path, "edited", text.modified).expect_err("stale");
        assert!(refused.contains("changed on disk"), "{refused}");
        assert_eq!(fs::read(&path).unwrap(), b"the plugin's");

        let fresh = read(&path).expect("reload");
        let written = write(&path, "edited", fresh.modified).expect("write");
        assert_eq!(read(&path).unwrap().modified, Some(written));
    }

    #[test]
    fn a_new_file_is_plain_utf8() {
        let scratch = Scratch::new();
        let path = scratch.0.join("New.ini");
        write(&path, "[A]\r\nb=1", None).expect("write");
        assert_eq!(fs::read(&path).unwrap(), b"[A]\r\nb=1");
    }
}
