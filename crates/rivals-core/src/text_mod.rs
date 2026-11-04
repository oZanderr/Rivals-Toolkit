//! The translations a mod changes: the wording it gives a text in a language, kept by the toolkit
//! so the mod's copy of the game's translation file can be written again over a patched one.
//!
//! The game reads one translation file a language, so a mod's copy replaces the game's whole file.
//! A copy made before a patch lacks whatever the patch added and keeps what it changed, which is
//! why the wordings are recorded on their own and the file rebuilt from the game's current one.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::locres::Locres;
use crate::pak::{game_files, plain};

/// The wordings a mod gives, by language, then namespace, then key.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextEdits {
    pub cultures: BTreeMap<String, BTreeMap<String, BTreeMap<String, String>>>,
}

impl TextEdits {
    /// What the toolkit has recorded for the mod `stem`, nothing when it has recorded nothing.
    pub fn load(stem: &str) -> Result<Self, String> {
        let path = store_path(stem)?;
        match fs::read(&path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|e| format!("read {}: {e}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("read {}: {e}", path.display())),
        }
    }

    fn save(&self, stem: &str) -> Result<(), String> {
        let path = store_path(stem)?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        }
        if self.cultures.is_empty() {
            return match fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(format!("remove {}: {e}", path.display())),
            };
        }
        let json = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        fs::write(&path, json).map_err(|e| format!("write {}: {e}", path.display()))
    }
}

fn store_dir() -> Result<PathBuf, String> {
    let dir = dirs::config_dir().ok_or("no configuration folder to keep text edits in")?;
    Ok(dir.join("rivals-toolkit").join("text-edits"))
}

/// Where a mod's wordings are kept, by its name without case.
fn store_path(stem: &str) -> Result<PathBuf, String> {
    Ok(store_dir()?.join(format!("{}.json", stem.to_ascii_lowercase())))
}

/// The mods the toolkit keeps wordings for, by their names without case.
pub fn recorded_mods() -> Result<Vec<String>, String> {
    let dir = store_dir()?;
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("read {}: {e}", dir.display())),
    };
    let mut stems: Vec<String> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| Some(path.file_stem()?.to_string_lossy().into_owned()))
        .collect();
    stems.sort();
    Ok(stems)
}

/// Drops the wordings kept for a mod deleted for good, named by its pak, switched on or off. One
/// that will not go is left: it does no harm beside a mod that is gone.
pub fn forget_mod(full_name: &str) {
    let name = full_name.rsplit(['/', '\\']).next().unwrap_or(full_name);
    let name = name.strip_suffix(".disabled").unwrap_or(name);
    let stem = name.strip_suffix(".pak").unwrap_or(name);
    if let Ok(path) = store_path(stem) {
        let _ = fs::remove_file(path);
    }
}

/// Keeps a renamed mod's wordings under its new name.
pub fn rename_record(from: &str, to: &str) -> Result<(), String> {
    let (old, new) = (store_path(from)?, store_path(to)?);
    if old == new || !old.is_file() {
        return Ok(());
    }
    fs::rename(&old, &new).map_err(|e| format!("move {}: {e}", old.display()))
}

/// A text as a command names it: `NAMESPACE/KEY`, or a string table entry as the table's id and
/// the key, `/Game/.../Table.Table:KEY`, which is filed under the namespace the table gives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextName {
    Filed { namespace: String, key: String },
    Table { table: String, key: String },
}

impl TextName {
    pub fn parse(given: &str) -> Result<Self, String> {
        let given = given.trim();
        let wrong = || {
            format!("{given} names no text; name one as NAMESPACE/KEY or /Game/.../Table.Table:KEY")
        };
        if given.starts_with('/')
            && let Some((table, key)) = given.split_once(':')
        {
            if table.len() < 2 || key.is_empty() {
                return Err(wrong());
            }
            return Ok(Self::Table {
                table: table.to_string(),
                key: key.to_string(),
            });
        }
        let (namespace, key) = given.split_once('/').ok_or_else(wrong)?;
        if key.is_empty() {
            return Err(wrong());
        }
        Ok(Self::Filed {
            namespace: namespace.to_string(),
            key: key.to_string(),
        })
    }

    /// The namespace and key the text is filed under, reading a table's namespace from the game.
    pub fn resolve(
        &self,
        game_root: &str,
        mappings: Option<&rivals_uasset::Mappings>,
    ) -> Result<(String, String), String> {
        match self {
            Self::Filed { namespace, key } => Ok((namespace.clone(), key.clone())),
            Self::Table { table, key } => {
                let found = crate::localization::TableSource {
                    game_root,
                    container: "",
                    mappings,
                }
                .table(table)
                .ok_or_else(|| format!("{table} is no string table the game holds"))?;
                if !found.sources.contains_key(key) {
                    return Err(format!("{table} has no key {key}"));
                }
                Ok((found.namespace.clone(), key.clone()))
            }
        }
    }
}

/// A language's translation file as the game ships it: its path from the game root, spelt as the
/// game spells it, and what it holds.
pub fn game_translations(game_root: &str, culture: &str) -> Result<(String, Locres), String> {
    let wanted = format!("Marvel/Content/Localization/Game/{culture}/Game.locres");
    let path = game_files::cased(game_root, &wanted)?
        .ok_or_else(|| crate::localization::no_translations(game_root, culture))?;
    let bytes = game_files::read(game_root, &path)?
        .ok_or_else(|| crate::localization::no_translations(game_root, culture))?;
    Ok((
        path,
        Locres::read(&bytes).map_err(|e| format!("{wanted}: {e}"))?,
    ))
}

/// The language a translation file's path is for, as the game spells it.
fn culture_of(path: &str) -> String {
    path.rsplit('/').nth(1).unwrap_or_default().to_string()
}

/// The translation file for `culture` in the pak beside a mod, with its path there, when the pak
/// holds one.
pub fn mod_translations(pak: &Path, culture: &str) -> Result<Option<(String, Locres)>, String> {
    if !pak.is_file() {
        return Ok(None);
    }
    let Some(path) = plain::list_files(pak)?.into_iter().find(|path| {
        path.to_ascii_lowercase().ends_with("/game.locres")
            && culture_of(path).eq_ignore_ascii_case(culture)
    }) else {
        return Ok(None);
    };
    let Some(bytes) = plain::read_one(pak, &path)? else {
        return Ok(None);
    };
    let file = Locres::read(&bytes).map_err(|e| format!("{path}: {e}"))?;
    Ok(Some((path, file)))
}

/// One change: a text's wording in a language, or `None` to give it back to the game's.
#[derive(Debug, Clone)]
pub struct TextChange {
    pub culture: String,
    pub namespace: String,
    pub key: String,
    pub text: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TextReport {
    pub pak: String,
    /// Each language the mod's translations were written for, or taken out of it in.
    pub files: Vec<TextFile>,
    pub written: bool,
}

#[derive(Debug, Serialize)]
pub struct TextFile {
    pub culture: String,
    /// The file's path in the mod's pak, from the game root.
    pub path: String,
    /// How many texts the mod words its own way in this language.
    pub edits: usize,
    /// The mod changes nothing in this language any more, so its file is taken out.
    pub removed: bool,
    /// Wordings taken from a translation file the mod already shipped, which the toolkit now keeps.
    pub adopted: usize,
    /// Texts in that file the game's translations no longer have, left out of the new one.
    pub dropped: usize,
}

/// Gives the mod whose pak is `pak` the `changes`. Each language they touch gets the game's current
/// translation file with every wording the mod gives filed over it, written into the pak, or taken
/// out of it once the mod changes nothing in that language. A pak that is not there is made.
///
/// A translation file the mod already ships that the toolkit did not write has its wordings kept:
/// each one differing from the game's is taken in. Texts in it the game no longer has would be
/// lost, so that is refused without `overwrite`.
pub fn apply_text(
    game_root: &str,
    pak: &Path,
    changes: &[TextChange],
    overwrite: bool,
    dry_run: bool,
) -> Result<TextReport, String> {
    let stem = stem_of(pak)?;
    let mut edits = TextEdits::load(&stem)?;
    let mut touched: Vec<String> = Vec::new();
    let mut adoptions: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for change in changes {
        let (path, file) = game_translations(game_root, &change.culture)?;
        let culture = culture_of(&path);
        if let Some(adopted) = adopt_shipped(&mut edits, &stem, pak, &culture, &file, overwrite)? {
            adoptions.insert(culture.clone(), adopted);
        }
        let held = edits.cultures.entry(culture.clone()).or_default();
        match &change.text {
            Some(text) => {
                if file.get(&change.namespace, &change.key).is_none() {
                    return Err(format!(
                        "{}/{} has no {culture} translation in the game to change",
                        change.namespace, change.key
                    ));
                }
                held.entry(change.namespace.clone())
                    .or_default()
                    .insert(change.key.clone(), text.clone());
            }
            None => {
                if let Some(keys) = held.get_mut(&change.namespace) {
                    keys.remove(&change.key);
                    if keys.is_empty() {
                        held.remove(&change.namespace);
                    }
                }
            }
        }
        if !touched.contains(&culture) {
            touched.push(culture);
        }
    }
    let report = write_cultures(game_root, pak, &stem, edits, &touched, dry_run)?;
    Ok(with_adoptions(report, &adoptions))
}

/// Writes every language the mod changes again over the game's current translations, which a
/// patch that changed them needs. A translation file the mod ships that the toolkit did not write
/// is taken in first, as [`apply_text`] takes it.
pub fn rebuild_text(
    game_root: &str,
    pak: &Path,
    overwrite: bool,
    dry_run: bool,
) -> Result<TextReport, String> {
    let stem = stem_of(pak)?;
    let mut edits = TextEdits::load(&stem)?;
    let mut adoptions: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let shipped: Vec<String> = if pak.is_file() {
        plain::list_files(pak)?
            .into_iter()
            .filter(|path| path.to_ascii_lowercase().ends_with("/game.locres"))
            .collect()
    } else {
        Vec::new()
    };
    for path in shipped {
        // A language the game does not ship is one nothing here can rebuild, so it stays as it is.
        let Ok((game_path, file)) = game_translations(game_root, &culture_of(&path)) else {
            continue;
        };
        let culture = culture_of(&game_path);
        if let Some(adopted) = adopt_shipped(&mut edits, &stem, pak, &culture, &file, overwrite)? {
            adoptions.insert(culture, adopted);
        }
    }
    if edits.cultures.is_empty() {
        return Err(format!(
            "{stem} ships no translations and the toolkit keeps no text edits for it"
        ));
    }
    let cultures: Vec<String> = edits.cultures.keys().cloned().collect();
    let report = write_cultures(game_root, pak, &stem, edits, &cultures, dry_run)?;
    Ok(with_adoptions(report, &adoptions))
}

/// The mod's name from its pak, switched on or off.
fn stem_of(pak: &Path) -> Result<String, String> {
    let name = pak
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or("the mod has no name")?;
    let name = name.strip_suffix(".disabled").unwrap_or(&name);
    Ok(name.strip_suffix(".pak").unwrap_or(name).to_string())
}

fn with_adoptions(
    mut report: TextReport,
    adoptions: &BTreeMap<String, (usize, usize)>,
) -> TextReport {
    for file in &mut report.files {
        if let Some(&(adopted, dropped)) = adoptions.get(&file.culture) {
            (file.adopted, file.dropped) = (adopted, dropped);
        }
    }
    report
}

/// Takes in the wordings of the translation file for `culture` the mod ships when the toolkit
/// keeps none for that language yet, giving how many it took and how many texts the game no
/// longer has, which are refused without `overwrite`.
fn adopt_shipped(
    edits: &mut TextEdits,
    stem: &str,
    pak: &Path,
    culture: &str,
    game: &Locres,
    overwrite: bool,
) -> Result<Option<(usize, usize)>, String> {
    if edits.cultures.contains_key(culture) {
        return Ok(None);
    }
    let Some((shipped, held)) = mod_translations(pak, culture)? else {
        return Ok(None);
    };
    let (taken, dropped) = adopt(&held, game);
    if dropped > 0 && !overwrite {
        return Err(format!(
            "{stem} ships {shipped} with {dropped} text(s) the game's {culture} translations no \
             longer have, which writing it again leaves out; pass --overwrite to write it anyway"
        ));
    }
    let count = taken.len();
    let into = edits.cultures.entry(culture.to_string()).or_default();
    for (namespace, key, text) in taken {
        into.entry(namespace).or_default().insert(key, text);
    }
    Ok(Some((count, dropped)))
}

/// The wordings in a mod's own translation file that differ from the game's, and how many of its
/// texts the game no longer has.
fn adopt(held: &Locres, game: &Locres) -> (Vec<(String, String, String)>, usize) {
    let mut taken = Vec::new();
    let mut dropped = 0;
    for namespace in &held.namespaces {
        for entry in &namespace.entries {
            match game.get(&namespace.name, &entry.key) {
                Some(theirs) if theirs.text == entry.text => {}
                Some(_) => taken.push((
                    namespace.name.clone(),
                    entry.key.clone(),
                    entry.text.clone(),
                )),
                None => dropped += 1,
            }
        }
    }
    (taken, dropped)
}

fn write_cultures(
    game_root: &str,
    pak: &Path,
    stem: &str,
    mut edits: TextEdits,
    cultures: &[String],
    dry_run: bool,
) -> Result<TextReport, String> {
    let mut files = Vec::new();
    let mut writes: Vec<(String, Option<Vec<u8>>)> = Vec::new();
    for culture in cultures {
        let (path, mut file) = game_translations(game_root, culture)?;
        let held = edits.cultures.get(culture).cloned().unwrap_or_default();
        let mut count = 0;
        for (namespace, keys) in &held {
            for (key, text) in keys {
                // The game's own entry says which source the wording is for; one it no longer has
                // is left out, since nothing would show it.
                let Some(hash) = file.get(namespace, key).map(|entry| entry.source_hash) else {
                    continue;
                };
                file.set(namespace, key, hash, text);
                count += 1;
            }
        }
        let removed = count == 0;
        if removed {
            edits.cultures.remove(culture);
        }
        writes.push((path.clone(), (!removed).then(|| file.write())));
        files.push(TextFile {
            culture: culture.clone(),
            path,
            edits: count,
            removed,
            adopted: 0,
            dropped: 0,
        });
    }
    if !dry_run {
        write_into_pak(pak, &writes)?;
        edits.save(stem)?;
    }
    Ok(TextReport {
        pak: pak.display().to_string(),
        files,
        written: !dry_run,
    })
}

/// Puts each file into the pak at its path, or takes it out for `None`. A pak not there is made.
fn write_into_pak(pak: &Path, writes: &[(String, Option<Vec<u8>>)]) -> Result<(), String> {
    if !pak.is_file() {
        let files: Vec<(&String, &Vec<u8>)> = writes
            .iter()
            .filter_map(|(path, bytes)| bytes.as_ref().map(|bytes| (path, bytes)))
            .collect();
        if files.is_empty() {
            return Ok(());
        }
        return new_mod(pak, &files);
    }
    crate::pak_tweaks::io::with_unpacked_pak(pak, |dir| {
        for (path, bytes) in writes {
            let at = dir.join(path);
            match bytes {
                Some(bytes) => {
                    if let Some(parent) = at.parent() {
                        fs::create_dir_all(parent)
                            .map_err(|e| format!("create {}: {e}", parent.display()))?;
                    }
                    fs::write(&at, bytes).map_err(|e| format!("write {}: {e}", at.display()))?;
                }
                None => match fs::remove_file(&at) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(format!("remove {}: {e}", at.display())),
                },
            }
        }
        Ok(())
    })
}

/// Makes the mod whose pak is `pak` as a container holding no package, its pak carrying `files`.
/// The game logs out at login over a plain pak in `~mods`, while this form loads.
fn new_mod(pak: &Path, files: &[(&String, &Vec<u8>)]) -> Result<(), String> {
    let staged = std::env::temp_dir().join(format!(
        "rivals_text_{}_{}",
        std::process::id(),
        stem_of(pak)?
    ));
    let _ = fs::remove_dir_all(&staged);
    let packed = (|| {
        for (path, bytes) in files {
            let at = staged.join(path);
            if let Some(parent) = at.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("create {}: {e}", parent.display()))?;
            }
            fs::write(&at, bytes).map_err(|e| format!("write {}: {e}", at.display()))?;
        }
        crate::pak::pack::pack_folder(
            &staged,
            crate::pak::pack::Take::Everything,
            &pak.with_extension("utoc"),
            &crate::pak::iostore_out::IoStoreOptions::default(),
            &Default::default(),
            &|| Ok(Default::default()),
            1,
            &std::sync::atomic::AtomicBool::new(false),
            &|_, _| {},
        )
    })();
    let _ = fs::remove_dir_all(&staged);
    packed.map(|_| ())
}

/// What the translation files in the pak beside a mod no longer agree with the game on: the texts
/// the game has that the mod's copy lacks, whose sources the game has changed since, or whose
/// wording the game has changed under the mod's unchanged copy. Each is a line naming the language.
pub fn stale_text(game_root: &str, pak: &Path) -> Result<Vec<String>, String> {
    let mut lines = Vec::new();
    for path in plain::list_files(pak)?
        .into_iter()
        .filter(|path| path.to_ascii_lowercase().ends_with("/game.locres"))
    {
        let culture = culture_of(&path);
        let Some(bytes) = plain::read_one(pak, &path)? else {
            continue;
        };
        let held = Locres::read(&bytes).map_err(|e| format!("{path}: {e}"))?;
        let Ok((_, game)) = game_translations(game_root, &culture) else {
            continue;
        };
        let missing = game
            .namespaces
            .iter()
            .flat_map(|ns| ns.entries.iter().map(move |entry| (&ns.name, &entry.key)))
            .filter(|(namespace, key)| held.get(namespace, key).is_none())
            .count();
        let resourced = held
            .namespaces
            .iter()
            .flat_map(|ns| ns.entries.iter().map(move |entry| (&ns.name, entry)))
            .filter(|(namespace, entry)| {
                game.get(namespace, &entry.key)
                    .is_some_and(|now| now.source_hash != entry.source_hash)
            })
            .count();
        if missing > 0 {
            lines.push(format!(
                "{culture}: lacks {missing} text(s) the game's translations have, which show untranslated while it is installed"
            ));
        }
        if resourced > 0 {
            lines.push(format!(
                "{culture}: {resourced} text(s) translate a source the game has changed since"
            ));
        }
    }
    Ok(lines)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn deleting_a_mod_forgets_its_wordings_in_whatever_folder_it_is() {
        let stem = format!("ZZForget_{}_9999999_P", std::process::id());
        let record = store_path(&stem).expect("store");
        let mods = std::env::temp_dir().join(format!("rivals_forget_{}", std::process::id()));
        for folder in ["", "Skins/"] {
            fs::create_dir_all(record.parent().expect("store folder")).expect("store folder");
            fs::write(&record, b"{}").expect("record");
            fs::create_dir_all(mods.join(folder)).expect("mods");
            let pak = format!("{folder}{stem}.pak.disabled");
            fs::write(mods.join(&pak), b"pak").expect("pak");

            crate::mod_library::delete_one(&mods, &pak).expect("delete");
            assert!(!mods.join(&pak).exists());
            assert!(
                !record.exists(),
                "the wordings go with the mod in {folder:?}"
            );
        }
        let _ = fs::remove_dir_all(&mods);
    }

    #[test]
    fn texts_are_named_by_namespace_or_by_table() {
        assert_eq!(
            TextName::parse("101_Replay_ST/TXT_Replay_Sign_New").unwrap(),
            TextName::Filed {
                namespace: "101_Replay_ST".into(),
                key: "TXT_Replay_Sign_New".into()
            }
        );
        assert_eq!(
            TextName::parse("/Game/Marvel/ST/101_Replay_ST.101_Replay_ST:A:B").unwrap(),
            TextName::Table {
                table: "/Game/Marvel/ST/101_Replay_ST.101_Replay_ST".into(),
                key: "A:B".into()
            }
        );
        assert_eq!(
            TextName::parse("/KEY").unwrap(),
            TextName::Filed {
                namespace: String::new(),
                key: "KEY".into()
            }
        );
        assert!(TextName::parse("NoSeparator").is_err());
        assert!(TextName::parse("NS/").is_err());
    }

    #[test]
    fn a_mods_own_wordings_are_kept_and_lost_texts_counted() {
        let file = |entries: &[(&str, &str)]| Locres {
            version: 3,
            namespaces: vec![crate::locres::Namespace {
                name: "NS".into(),
                entries: entries
                    .iter()
                    .map(|(key, text)| crate::locres::Entry {
                        key: (*key).into(),
                        source_hash: 0,
                        text: (*text).into(),
                    })
                    .collect(),
            }],
        };
        let game = file(&[("A", "a"), ("B", "b")]);
        let held = file(&[("A", "a"), ("B", "mine"), ("Gone", "x")]);
        let (taken, dropped) = adopt(&held, &game);
        assert_eq!(taken, vec![("NS".into(), "B".into(), "mine".into())]);
        assert_eq!(dropped, 1);
    }

    #[test]
    fn a_translation_path_names_its_language() {
        assert_eq!(
            culture_of("Marvel/Content/Localization/Game/zh-Hans/Game.locres"),
            "zh-Hans"
        );
    }

    /// A wording set, rebuilt and given back, against the shipped English translations, written
    /// into a new mod, made as a container, in a scratch folder.
    #[test]
    fn a_wording_goes_into_a_mod_and_comes_back_out() {
        let Ok(root) = std::env::var("RIVALS_GAME_ROOT") else {
            return;
        };
        let (_, game) = game_translations(&root, "EN").expect("english");
        let namespace = &game.namespaces[0];
        let entry = &namespace.entries[0];
        let dir = std::env::temp_dir().join(format!("rivals-text-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let pak = dir.join("ZZTextTest_9999999_P.pak");
        let change = |text: Option<&str>| TextChange {
            culture: "en".into(),
            namespace: namespace.name.clone(),
            key: entry.key.clone(),
            text: text.map(str::to_string),
        };

        let report =
            apply_text(&root, &pak, &[change(Some("Changed"))], false, false).expect("set");
        for part in ["utoc", "ucas"] {
            assert!(
                pak.with_extension(part).is_file(),
                "a new mod is a container"
            );
        }
        assert_eq!(report.files[0].culture, "en");
        assert_eq!(report.files[0].edits, 1);
        let path = &report.files[0].path;
        let held = Locres::read(&plain::read_one(&pak, path).unwrap().unwrap()).unwrap();
        let mine = held.get(&namespace.name, &entry.key).unwrap();
        assert_eq!(mine.text, "Changed");
        assert_eq!(mine.source_hash, entry.source_hash);
        assert!(stale_text(&root, &pak).unwrap().is_empty());

        let report = apply_text(&root, &pak, &[change(None)], false, false).expect("unset");
        assert!(report.files[0].removed);
        assert!(plain::read_one(&pak, path).unwrap().is_none());
        assert!(
            TextEdits::load("ZZTextTest_9999999_P")
                .unwrap()
                .cultures
                .is_empty()
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
