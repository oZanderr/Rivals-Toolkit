//! One `.locres` translation file as the game writes it: its namespaces and keys in file order,
//! each with the hash of the source it translates and the translation, read and written back byte
//! for byte.

/// The GUID a versioned file starts with; one without it is the legacy layout.
const MAGIC: [u8; 16] = [
    0x0E, 0x14, 0x74, 0x75, 0x67, 0x4A, 0x03, 0xFC, 0x4A, 0x15, 0x90, 0x9D, 0xC3, 0x37, 0x7F, 0x1B,
];

/// `ELocResVersion`: compact files share one string array, optimized ones hash every namespace and
/// key with CRC-32, and the latest hash them with CityHash64 over UTF-16.
const COMPACT: u8 = 1;
const OPTIMIZED_CRC32: u8 = 2;
const OPTIMIZED_CITYHASH: u8 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Locres {
    pub version: u8,
    pub namespaces: Vec<Namespace>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Namespace {
    pub name: String,
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub key: String,
    /// `StrCrc32` of the source the translation was made from. The game shows the translation only
    /// while it matches the text's current source.
    pub source_hash: u32,
    pub text: String,
}

impl Locres {
    pub fn read(bytes: &[u8]) -> Result<Self, String> {
        let mut at = Reader { bytes, at: 0 };
        let version = if bytes.len() >= 16 && bytes[..16] == MAGIC {
            at.at = 16;
            at.u8()?
        } else {
            0
        };
        if version > OPTIMIZED_CITYHASH {
            return Err(format!(
                "a .locres of version {version}, newer than this reads"
            ));
        }
        let mut strings = Vec::new();
        if version >= COMPACT {
            let offset = at.i64()?;
            if offset >= 0 {
                let mut array = Reader {
                    bytes,
                    at: usize::try_from(offset).map_err(|_| "a string array past the file")?,
                };
                for _ in 0..array.count()? {
                    strings.push(array.fstring()?);
                    if version >= OPTIMIZED_CRC32 {
                        array.i32()?;
                    }
                }
            }
        }
        if version >= OPTIMIZED_CRC32 {
            at.u32()?;
        }
        let mut namespaces = Vec::new();
        for _ in 0..at.count()? {
            let name = at.text_key(version)?;
            let mut entries = Vec::new();
            for _ in 0..at.count()? {
                let key = at.text_key(version)?;
                let source_hash = at.u32()?;
                let text = if version >= COMPACT {
                    let index = at.i32()?;
                    usize::try_from(index)
                        .ok()
                        .and_then(|index| strings.get(index))
                        .cloned()
                        .ok_or_else(|| format!("{name}/{key} names string {index}"))?
                } else {
                    at.fstring()?
                };
                entries.push(Entry {
                    key,
                    source_hash,
                    text,
                });
            }
            namespaces.push(Namespace { name, entries });
        }
        Ok(Self {
            version,
            namespaces,
        })
    }

    /// The file's bytes. Translations are shared in the string array in the order they are first
    /// met, as the engine writes them.
    pub fn write(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.version >= COMPACT {
            out.extend_from_slice(&MAGIC);
            out.push(self.version);
        }
        let offset_at = out.len();
        if self.version >= COMPACT {
            out.extend_from_slice(&0i64.to_le_bytes());
        }
        if self.version >= OPTIMIZED_CRC32 {
            let entries: usize = self.namespaces.iter().map(|ns| ns.entries.len()).sum();
            out.extend_from_slice(&(entries as u32).to_le_bytes());
        }
        let mut strings: Vec<(&str, i32)> = Vec::new();
        let mut index_of: std::collections::HashMap<&str, usize> = Default::default();
        out.extend_from_slice(&(self.namespaces.len() as u32).to_le_bytes());
        for namespace in &self.namespaces {
            self.text_key(&mut out, &namespace.name);
            out.extend_from_slice(&(namespace.entries.len() as u32).to_le_bytes());
            for entry in &namespace.entries {
                self.text_key(&mut out, &entry.key);
                out.extend_from_slice(&entry.source_hash.to_le_bytes());
                if self.version >= COMPACT {
                    let index = *index_of.entry(entry.text.as_str()).or_insert_with(|| {
                        strings.push((entry.text.as_str(), 0));
                        strings.len() - 1
                    });
                    strings[index].1 += 1;
                    out.extend_from_slice(&(index as i32).to_le_bytes());
                } else {
                    fstring(&mut out, &entry.text);
                }
            }
        }
        if self.version >= COMPACT {
            let offset = out.len() as i64;
            out[offset_at..offset_at + 8].copy_from_slice(&offset.to_le_bytes());
            out.extend_from_slice(&(strings.len() as i32).to_le_bytes());
            for (text, references) in strings {
                fstring(&mut out, text);
                if self.version >= OPTIMIZED_CRC32 {
                    out.extend_from_slice(&references.to_le_bytes());
                }
            }
        }
        out
    }

    fn text_key(&self, out: &mut Vec<u8>, text: &str) {
        match self.version {
            OPTIMIZED_CRC32 => {
                out.extend_from_slice(&crate::localization::str_crc32(text).to_le_bytes());
            }
            OPTIMIZED_CITYHASH => out.extend_from_slice(&text_key_hash(text).to_le_bytes()),
            _ => {}
        }
        fstring(out, text);
    }

    pub fn get(&self, namespace: &str, key: &str) -> Option<&Entry> {
        self.namespaces
            .iter()
            .find(|ns| ns.name == namespace)?
            .entries
            .iter()
            .find(|entry| entry.key == key)
    }

    /// Files `text` under `namespace` and `key`, in place when the key is there and at the end of
    /// its namespace, or of a new one, when not.
    pub fn set(&mut self, namespace: &str, key: &str, source_hash: u32, text: &str) {
        let at = match self.namespaces.iter().position(|ns| ns.name == namespace) {
            Some(at) => at,
            None => {
                self.namespaces.push(Namespace {
                    name: namespace.to_string(),
                    entries: Vec::new(),
                });
                self.namespaces.len() - 1
            }
        };
        let entries = &mut self.namespaces[at].entries;
        match entries.iter_mut().find(|entry| entry.key == key) {
            Some(entry) => {
                entry.source_hash = source_hash;
                entry.text = text.to_string();
            }
            None => entries.push(Entry {
                key: key.to_string(),
                source_hash,
                text: text.to_string(),
            }),
        }
    }
}

/// `TextKeyUtil::HashString`: CityHash64 over the UTF-16 units, folded to 32 bits.
fn text_key_hash(text: &str) -> u32 {
    let units: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let hash = cityhasher::hash::<u64>(&units);
    (hash as u32).wrapping_add(((hash >> 32) as u32).wrapping_mul(23))
}

/// An FString as the engine saves one: ANSI when every character is, UTF-16 otherwise, either
/// with its terminator counted.
fn fstring(out: &mut Vec<u8>, text: &str) {
    if text.is_empty() {
        out.extend_from_slice(&0i32.to_le_bytes());
    } else if text.is_ascii() {
        out.extend_from_slice(&(text.len() as i32 + 1).to_le_bytes());
        out.extend_from_slice(text.as_bytes());
        out.push(0);
    } else {
        let units: Vec<u16> = text.encode_utf16().collect();
        out.extend_from_slice(&(-(units.len() as i32 + 1)).to_le_bytes());
        for unit in units {
            out.extend_from_slice(&unit.to_le_bytes());
        }
        out.extend_from_slice(&0u16.to_le_bytes());
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, count: usize) -> Result<&[u8], String> {
        let end = self
            .at
            .checked_add(count)
            .filter(|&end| end <= self.bytes.len())
            .ok_or("the file ends early")?;
        let out = &self.bytes[self.at..end];
        self.at = end;
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, String> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn i32(&mut self) -> Result<i32, String> {
        Ok(self.u32()? as i32)
    }

    fn i64(&mut self) -> Result<i64, String> {
        let b = self.take(8)?;
        let mut word = [0u8; 8];
        word.copy_from_slice(b);
        Ok(i64::from_le_bytes(word))
    }

    fn count(&mut self) -> Result<usize, String> {
        let count = self.i32()?;
        usize::try_from(count)
            .ok()
            .filter(|&count| count <= self.bytes.len())
            .ok_or_else(|| format!("an implausible count {count}"))
    }

    fn fstring(&mut self) -> Result<String, String> {
        let length = self.i32()?;
        if length == 0 {
            return Ok(String::new());
        }
        if length > 0 {
            let bytes = self.take(length as usize)?;
            let text = &bytes[..bytes.len() - 1];
            return Ok(text.iter().map(|&b| char::from(b)).collect());
        }
        let units = length.unsigned_abs() as usize;
        let bytes = self.take(units * 2)?;
        let wide: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .take(units - 1)
            .collect();
        Ok(String::from_utf16_lossy(&wide))
    }

    /// A namespace or key, with the hash the optimized layouts put ahead of it checked against the
    /// one this would write: a mismatch is a file this would not write back the same.
    fn text_key(&mut self, version: u8) -> Result<String, String> {
        let stored = (version >= OPTIMIZED_CRC32)
            .then(|| self.u32())
            .transpose()?;
        let text = self.fstring()?;
        let computed = match version {
            OPTIMIZED_CRC32 => Some(crate::localization::str_crc32(&text)),
            OPTIMIZED_CITYHASH => Some(text_key_hash(&text)),
            _ => None,
        };
        if stored != computed {
            return Err(format!(
                "{text} is filed under hash {stored:08X?}, not the {computed:08X?} it hashes to"
            ));
        }
        Ok(text)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn sample(version: u8) -> Locres {
        Locres {
            version,
            namespaces: vec![
                Namespace {
                    name: "601_HeroUIAsset_1011_ST".into(),
                    entries: vec![
                        Entry {
                            key: "Name".into(),
                            source_hash: 7,
                            text: "HULK".into(),
                        },
                        Entry {
                            key: "Title".into(),
                            source_hash: 9,
                            text: "HULK".into(),
                        },
                    ],
                },
                Namespace {
                    name: String::new(),
                    entries: vec![Entry {
                        key: "Wide".into(),
                        source_hash: 3,
                        text: "快速模式".into(),
                    }],
                },
            ],
        }
    }

    /// Every layout writes what it reads, a shared translation stored once in the compact ones.
    #[test]
    fn every_version_reads_back_what_it_writes() {
        for version in 0..=OPTIMIZED_CITYHASH {
            let written = sample(version).write();
            assert_eq!(Locres::read(&written).expect("read"), sample(version));
            assert_eq!(Locres::read(&written).unwrap().write(), written);
        }
        let compact = sample(OPTIMIZED_CITYHASH).write();
        let tail = &compact[compact.len() - 4 - 2 * 4..];
        assert!(
            tail.ends_with(&1i32.to_le_bytes()),
            "the wide string is referenced once"
        );
    }

    #[test]
    fn a_set_files_in_place_or_appends() {
        let mut file = sample(OPTIMIZED_CITYHASH);
        file.set("601_HeroUIAsset_1011_ST", "Name", 11, "BRUCE");
        file.set("601_HeroUIAsset_1011_ST", "Extra", 12, "Smash");
        file.set("New", "Key", 13, "Text");
        assert_eq!(
            file.get("601_HeroUIAsset_1011_ST", "Name").unwrap().text,
            "BRUCE"
        );
        assert_eq!(file.namespaces[0].entries[2].key, "Extra");
        assert_eq!(file.namespaces[2].name, "New");
    }

    /// The shipped files, each read and written back the same, which pins the hashes, the string
    /// sharing and the string encodings to what the engine wrote.
    #[test]
    fn the_game_s_translations_write_back_byte_for_byte() {
        let Ok(root) = std::env::var("RIVALS_GAME_ROOT") else {
            return;
        };
        let files = crate::pak::game_files::list(&root, "/game.locres").expect("list");
        assert!(!files.is_empty());
        for path in &files {
            let bytes = crate::pak::game_files::read(&root, path)
                .expect("read")
                .expect("present");
            let file = Locres::read(&bytes).unwrap_or_else(|e| panic!("{path}: {e}"));
            assert!(file.write() == bytes, "{path} does not write back the same");
        }
        eprintln!("{} translation files written back the same", files.len());
    }
}
