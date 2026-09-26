//! What the game shows for a text: its translation in a language, from the `.locres` files the
//! game ships, and for a string table reference the entry that table holds.
//!
//! A translation is filed under a namespace and a key with a hash of the source string it was
//! made from. The game shows it only while that hash still matches the text's source; an edited
//! source makes it stale and the source is shown instead, which was checked in-game.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use rivals_uasset::{Mappings, ParsedPackage, PropertyEntry, PropertyValue};

use crate::asset::{self, AssetSource};
use crate::pak::game_files;

/// The GUID a versioned `.locres` file starts with; one without it is the legacy layout.
const LOCRES_MAGIC: [u8; 16] = [
    0x0E, 0x14, 0x74, 0x75, 0x67, 0x4A, 0x03, 0xFC, 0x4A, 0x15, 0x90, 0x9D, 0xC3, 0x37, 0x7F, 0x1B,
];

/// `ELocResVersion`: compact files move the strings into one shared array, and the optimized ones
/// store a hash before every namespace and key.
const COMPACT: u8 = 1;
const OPTIMIZED: u8 = 2;

/// One language's translations, keyed by namespace and key.
#[derive(Default)]
pub struct Localizer {
    pub culture: String,
    entries: HashMap<String, (u32, u32)>,
    strings: Vec<String>,
}

fn entry_key(namespace: &str, key: &str) -> String {
    format!("{namespace}\u{1}{key}")
}

impl Localizer {
    /// The translation filed under `namespace` and `key`, when there is one and it was made from
    /// `source`. `None` means the game shows the source.
    pub fn lookup(&self, namespace: &str, key: &str, source: Option<&str>) -> Option<&str> {
        let &(hash, index) = self.entries.get(&entry_key(namespace, key))?;
        if source.is_some_and(|source| str_crc32(source) != hash) {
            return None;
        }
        self.strings.get(index as usize).map(String::as_str)
    }

    /// Reads one `.locres` file into this language, over whatever an earlier file filed under the
    /// same namespace and key.
    pub fn add_locres(&mut self, bytes: &[u8]) -> Result<(), String> {
        let mut at = Reader { bytes, at: 0 };
        let version = if bytes.len() >= 16 && bytes[..16] == LOCRES_MAGIC {
            at.at = 16;
            at.u8()?
        } else {
            0
        };
        let base = self.strings.len() as u32;
        if version >= COMPACT {
            let offset = at.i64()?;
            if offset >= 0 {
                let mut strings = Reader {
                    bytes,
                    at: usize::try_from(offset).map_err(|_| "a string array past the file")?,
                };
                for _ in 0..strings.count()? {
                    self.strings.push(strings.fstring()?);
                    if version >= OPTIMIZED {
                        strings.i32()?; // how many entries share it
                    }
                }
            }
        }
        if version >= OPTIMIZED {
            at.u32()?; // how many entries follow
        }
        for _ in 0..at.u32()? {
            let namespace = at.text_key(version)?;
            for _ in 0..at.u32()? {
                let key = at.text_key(version)?;
                let hash = at.u32()?;
                let index = if version >= COMPACT {
                    base + u32::try_from(at.i32()?).map_err(|_| "a negative string index")?
                } else {
                    self.strings.push(at.fstring()?);
                    self.strings.len() as u32 - 1
                };
                self.entries
                    .insert(entry_key(&namespace, &key), (hash, index));
            }
        }
        Ok(())
    }

    /// One language's translations from every `.locres` the base game ships for it, the game's own
    /// last so its entries win. Cached per game install and language.
    pub fn for_culture(game_root: &str, culture: &str) -> Result<Arc<Localizer>, String> {
        static LOADED: OnceLock<Mutex<HashMap<String, Arc<Localizer>>>> = OnceLock::new();
        let cache_key = format!("{game_root}\u{1}{culture}");
        let loaded = LOADED.get_or_init(|| Mutex::new(HashMap::new()));
        if let Some(found) = loaded.lock().map_err(|e| e.to_string())?.get(&cache_key) {
            return Ok(found.clone());
        }
        let wanted = format!("/{}/", culture.to_ascii_lowercase());
        let mut files: Vec<String> = game_files::list(game_root, ".locres")?
            .into_iter()
            .filter(|path| path.contains(&wanted))
            .collect();
        // The game's own translations last, and the copy under its content folder after a stray one.
        files.sort_by_key(|path| {
            (
                path.contains("/localization/game/"),
                path.starts_with("marvel/"),
                path.clone(),
            )
        });
        if files.is_empty() {
            return Err(format!("the game ships no translations for {culture}"));
        }
        let mut localizer = Localizer {
            culture: culture.to_string(),
            ..Default::default()
        };
        for path in &files {
            if let Some(bytes) = game_files::read(game_root, path)? {
                localizer
                    .add_locres(&bytes)
                    .map_err(|e| format!("{path}: {e}"))?;
            }
        }
        let localizer = Arc::new(localizer);
        loaded
            .lock()
            .map_err(|e| e.to_string())?
            .insert(cache_key, localizer.clone());
        Ok(localizer)
    }
}

/// The languages the base game ships translations for.
pub fn cultures(game_root: &str) -> Result<Vec<String>, String> {
    let mut found: Vec<String> = game_files::list(game_root, ".locres")?
        .iter()
        .filter_map(|path| path.rsplit('/').nth(1).map(str::to_string))
        .collect();
    found.sort();
    found.dedup();
    Ok(found)
}

/// UE's `FCrc::StrCrc32`, which translations store for the source string they were made from:
/// CRC-32 over each UTF-16 unit widened to four bytes.
pub fn str_crc32(text: &str) -> u32 {
    const TABLE: [u32; 256] = {
        let mut table = [0u32; 256];
        let mut i = 0;
        while i < 256 {
            let mut crc = i as u32;
            let mut bit = 0;
            while bit < 8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
                bit += 1;
            }
            table[i] = crc;
            i += 1;
        }
        table
    };
    let mut crc = !0u32;
    for unit in text.encode_utf16() {
        for byte in u32::from(unit).to_le_bytes() {
            crc = (crc >> 8) ^ TABLE[((crc ^ u32::from(byte)) & 0xFF) as usize];
        }
    }
    !crc
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

    /// An FString: a signed length counting the terminator, one byte a character when positive
    /// and UTF-16 when negative.
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
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .take(units - 1)
            .collect();
        Ok(String::from_utf16_lossy(&wide))
    }

    /// A namespace or key: its hash first in the optimized layouts, which a lookup by name has no
    /// use for.
    fn text_key(&mut self, version: u8) -> Result<String, String> {
        if version >= OPTIMIZED {
            self.u32()?;
        }
        self.fstring()
    }
}

/// A string table's namespace and the source string of each key, as its package reads.
pub struct TableEntries {
    pub namespace: String,
    pub sources: HashMap<String, String>,
}

/// Where the string tables a package points at are read from: the store its own container opens,
/// so a mod's own tables are the ones its texts show.
pub struct TableSource<'a> {
    pub game_root: &'a str,
    pub container: &'a str,
    pub mappings: Option<&'a Mappings>,
}

impl TableSource<'_> {
    /// The table a `TableId` such as `/Game/.../129_Team_ST.129_Team_ST` names, read once per
    /// container. `None` when it cannot be read as one.
    pub fn table(&self, table_id: &str) -> Option<Arc<TableEntries>> {
        type Tables = HashMap<String, Option<Arc<TableEntries>>>;
        static READ: OnceLock<Mutex<Tables>> = OnceLock::new();
        let container = if self.container.to_ascii_lowercase().ends_with(".utoc") {
            self.container.to_string()
        } else {
            "pakchunk0-Windows.utoc".to_string()
        };
        let cache_key = format!("{}\u{1}{container}\u{1}{table_id}", self.game_root);
        let read = READ.get_or_init(|| Mutex::new(HashMap::new()));
        if let Some(found) = read.lock().ok()?.get(&cache_key) {
            return found.clone();
        }
        let found = self.read_table(&container, table_id);
        read.lock().ok()?.insert(cache_key, found.clone());
        found
    }

    fn read_table(&self, container: &str, table_id: &str) -> Option<Arc<TableEntries>> {
        let package = table_id.split('.').next()?;
        let loaded =
            asset::load_bundle(self.game_root, container, package, AssetSource::Utoc).ok()?;
        let parsed = rivals_uasset::parse_package(
            &rivals_uasset::AssetBundle {
                asset: &loaded.asset_file_buffer,
                exports: &loaded.exports_file_buffer,
            },
            self.mappings,
        )
        .ok()?;
        let table = parsed
            .exports
            .into_iter()
            .find_map(|export| export.string_table)?;
        Some(Arc::new(TableEntries {
            namespace: table.namespace,
            sources: table
                .entries
                .into_iter()
                .map(|entry| (entry.key, entry.source))
                .collect(),
        }))
    }
}

/// Fills in what the game shows for every text in the package, where that differs from what the
/// text stores: the translation of a localized text, and a string table entry's translation or
/// source. Only for showing; nothing that edits a package reads it.
pub fn localize(parsed: &mut ParsedPackage, localizer: &Localizer, tables: &TableSource<'_>) {
    for export in &mut parsed.exports {
        localize_entries(&mut export.properties, localizer, tables);
        localize_entries(&mut export.defaults, localizer, tables);
        if let Some(table) = &mut export.data_table {
            for row in &mut table.rows {
                localize_entries(&mut row.fields, localizer, tables);
            }
        }
    }
}

fn localize_entries(
    entries: &mut [PropertyEntry],
    localizer: &Localizer,
    tables: &TableSource<'_>,
) {
    for entry in entries {
        localize_value(&mut entry.value, localizer, tables);
    }
}

fn localize_value(value: &mut PropertyValue, localizer: &Localizer, tables: &TableSource<'_>) {
    match value {
        PropertyValue::Text {
            value,
            parts,
            namespace,
            key,
            display,
        } => {
            localize_entries(parts, localizer, tables);
            let shown = match (namespace.as_deref(), key.as_deref()) {
                (Some(namespace), Some(key)) => localizer
                    .lookup(namespace, key, value.as_deref())
                    .map(str::to_string),
                _ => table_text(parts, localizer, tables),
            };
            *display = shown.filter(|shown| Some(shown) != value.as_ref());
        }
        PropertyValue::Struct { fields, .. }
        | PropertyValue::Unset { fields, .. }
        | PropertyValue::Default { fields, .. } => localize_entries(fields, localizer, tables),
        PropertyValue::Array { items } | PropertyValue::Set { items } => {
            for item in items {
                localize_value(item, localizer, tables);
            }
        }
        PropertyValue::Map { entries } => {
            for pair in entries {
                localize_value(&mut pair.key, localizer, tables);
                localize_value(&mut pair.value, localizer, tables);
            }
        }
        _ => {}
    }
}

/// A string table reference's text: the entry's translation, or its source when there is none.
fn table_text(
    parts: &[PropertyEntry],
    localizer: &Localizer,
    tables: &TableSource<'_>,
) -> Option<String> {
    let part = |name: &str| {
        parts
            .iter()
            .find(|part| part.name == name)
            .map(|part| &part.value)
    };
    let (Some(PropertyValue::Name { value: table_id }), Some(PropertyValue::Str { value: key })) =
        (part("TableId"), part("Key"))
    else {
        return None;
    };
    let table = tables.table(table_id)?;
    let source = table.sources.get(key)?;
    Some(
        localizer
            .lookup(&table.namespace, key, Some(source))
            .unwrap_or(source)
            .to_string(),
    )
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn fstring(out: &mut Vec<u8>, text: &str) {
        if text.is_ascii() {
            out.extend_from_slice(&(text.len() as i32 + 1).to_le_bytes());
            out.extend_from_slice(text.as_bytes());
            out.push(0);
        } else {
            let wide: Vec<u16> = text.encode_utf16().collect();
            out.extend_from_slice(&(-(wide.len() as i32 + 1)).to_le_bytes());
            for unit in wide.iter().chain(&[0]) {
                out.extend_from_slice(&unit.to_le_bytes());
            }
        }
    }

    /// A `.locres` in the optimized layout with one namespace holding `entries` of key, source
    /// and translation.
    fn locres(namespace: &str, entries: &[(&str, &str, &str)]) -> Vec<u8> {
        let mut out = LOCRES_MAGIC.to_vec();
        out.push(3);
        let offset_at = out.len();
        out.extend_from_slice(&0i64.to_le_bytes());
        out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        fstring(&mut out, namespace);
        out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        for (index, (key, source, _)) in entries.iter().enumerate() {
            out.extend_from_slice(&0u32.to_le_bytes());
            fstring(&mut out, key);
            out.extend_from_slice(&str_crc32(source).to_le_bytes());
            out.extend_from_slice(&(index as i32).to_le_bytes());
        }
        let strings_at = out.len() as i64;
        out[offset_at..offset_at + 8].copy_from_slice(&strings_at.to_le_bytes());
        out.extend_from_slice(&(entries.len() as i32).to_le_bytes());
        for (_, _, translated) in entries {
            fstring(&mut out, translated);
            out.extend_from_slice(&1i32.to_le_bytes());
        }
        out
    }

    /// A translation is found by namespace and key, while its source still matches; once the
    /// source is edited it is stale and the game shows the source.
    #[test]
    fn a_translation_holds_only_while_its_source_matches() {
        let mut localizer = Localizer::default();
        localizer
            .add_locres(&locres("Menu", &[("Quick", "快速模式", "Quick Match")]))
            .expect("reads");
        assert_eq!(
            localizer.lookup("Menu", "Quick", Some("快速模式")),
            Some("Quick Match")
        );
        assert_eq!(
            localizer.lookup("Menu", "Quick", Some("TOOLKIT TEST")),
            None
        );
        assert_eq!(localizer.lookup("Menu", "Quick", None), Some("Quick Match"));
        assert_eq!(localizer.lookup("Menu", "Other", None), None);
    }

    /// A later file wins over an earlier one for the same namespace and key.
    #[test]
    fn a_later_file_overrides_an_earlier_one() {
        let mut localizer = Localizer::default();
        localizer
            .add_locres(&locres("Menu", &[("Quick", "a", "Old")]))
            .expect("reads");
        localizer
            .add_locres(&locres("Menu", &[("Quick", "a", "New")]))
            .expect("reads");
        assert_eq!(localizer.lookup("Menu", "Quick", Some("a")), Some("New"));
    }

    /// `StrCrc32` widens every character to four bytes, so it is plain CRC-32 over UTF-32 for
    /// these; the empty string hashes to zero.
    #[test]
    fn str_crc32_matches_crc32_over_widened_characters() {
        assert_eq!(str_crc32(""), 0);
        // CRC-32 of "a\0\0\0".
        assert_eq!(str_crc32("a"), 0xA2DE_4F7A);
    }

    /// The game's install and mappings, when the environment names them; these tests read the
    /// real translation files and tables, and are skipped otherwise.
    fn game() -> Option<(String, Arc<Mappings>)> {
        let root = std::env::var("RIVALS_GAME_ROOT").ok()?;
        let usmap = std::env::var("RIVALS_USMAP").ok()?;
        let mappings = crate::mappings::load(std::path::Path::new(&usmap)).expect("mappings");
        Some((root, mappings))
    }

    const MODES: &str = "/Game/Marvel/Data/StringTable/111_ModeSelection_ST.111_ModeSelection_ST";

    /// The mode name checked in-game resolves to what an English player sees, which also pins
    /// `StrCrc32` to the hash the game's own file stores for its Chinese source.
    #[test]
    fn a_string_table_entry_reads_as_the_game_shows_it() {
        let Some((root, mappings)) = game() else {
            return;
        };
        let english = Localizer::for_culture(&root, "en").expect("English");
        let tables = TableSource {
            game_root: &root,
            container: "pakchunk0-Windows.utoc",
            mappings: Some(&mappings),
        };
        let table = tables.table(MODES).expect("the mode table");
        let source = table.sources.get("Text_QuickMode").expect("the entry");
        assert_eq!(
            english.lookup(&table.namespace, "Text_QuickMode", Some(source)),
            Some("QUICK MATCH")
        );
        assert!(cultures(&root).expect("cultures").iter().any(|c| c == "ja"));
    }

    /// A table's texts come back with what the game shows, while `value` keeps what the package
    /// stores, so an edit or a diff made from the dump is unchanged.
    #[test]
    fn a_package_s_texts_are_filled_in_and_keep_what_they_store() {
        let Some((root, mappings)) = game() else {
            return;
        };
        let container = "pakchunk0-Windows.utoc";
        let entry =
            "Marvel/Content/Marvel/Data/DataTable/UI/CustomGame/MarvelCustomGameModeTable.uasset";
        let loaded = asset::load_bundle(&root, container, entry, AssetSource::Utoc).expect("load");
        let mut parsed = rivals_uasset::parse_package(
            &rivals_uasset::AssetBundle {
                asset: &loaded.asset_file_buffer,
                exports: &loaded.exports_file_buffer,
            },
            Some(&mappings),
        )
        .expect("parse");
        let english = Localizer::for_culture(&root, "en").expect("English");
        localize(
            &mut parsed,
            &english,
            &TableSource {
                game_root: &root,
                container,
                mappings: Some(&mappings),
            },
        );
        let texts: Vec<(Option<String>, Option<String>)> = parsed.exports[0]
            .data_table
            .as_ref()
            .expect("a table")
            .rows
            .iter()
            .flat_map(|row| &row.fields)
            .filter_map(|field| match &field.value {
                PropertyValue::Text { value, display, .. } => {
                    Some((value.clone(), display.clone()))
                }
                _ => None,
            })
            .collect();
        assert!(!texts.is_empty());
        assert!(
            texts.iter().all(|(_, display)| display.is_some()),
            "{texts:?}"
        );
        assert!(texts.iter().any(|(value, display)| {
            value.as_deref() == Some(&format!("{MODES}:Text_QuickMode"))
                && display.as_deref() == Some("QUICK MATCH")
        }));
    }
}
