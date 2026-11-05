//! The folders below `~mods`: listing them, moving mods between them, and making, renaming and
//! deleting them.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{BulkFailure, BulkOpResult};
use crate::mod_ops::ModFiles;
use crate::mods::held_open_hint;

/// Names Windows keeps for devices, which no folder can take, with an extension or without.
const RESERVED: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];

/// What deleting a folder did.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FolderDeleteReport {
    /// Mods deleted with it.
    pub deleted: u32,
    pub failures: Vec<BulkFailure>,
    /// Files below it that are no mod's, left where they are, by their paths below `~mods`.
    pub kept: Vec<String>,
}

/// The folder below `~mods` that `given` names, `/` between its parts and "" for `~mods` itself,
/// or why it cannot be one.
pub fn folder_path(given: &str) -> Result<String, String> {
    let path = given.trim().replace('\\', "/");
    if path.is_empty() {
        return Ok(path);
    }
    if path.starts_with('/') || path.contains(':') {
        return Err(format!("{path} is not a folder inside ~mods"));
    }
    for part in path.split('/') {
        folder_name(part).map_err(|why| format!("{path}: {why}"))?;
    }
    Ok(path)
}

/// Why `name` cannot be one folder's name, if it cannot.
fn folder_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("a folder needs a name".to_string());
    }
    if name == "." || name == ".." {
        return Err(format!("{name} names no folder"));
    }
    if let Some(bad) = name.chars().find(|c| {
        matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || c.is_control()
    }) {
        return Err(format!("a folder's name cannot hold {bad:?}"));
    }
    if name.starts_with(' ') || name.ends_with(' ') || name.ends_with('.') {
        return Err(format!(
            "\"{name}\" starts or ends with a space, or ends with a dot, which Windows drops"
        ));
    }
    let base = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    let numbered = base.len() == 4
        && (base.starts_with("COM") || base.starts_with("LPT"))
        && base.as_bytes()[3].is_ascii_digit();
    if RESERVED.contains(&base.as_str()) || numbered {
        return Err(format!("Windows keeps the name {name} for a device"));
    }
    if name.len() > 255 {
        return Err("a folder's name is at most 255 bytes".to_string());
    }
    // A folder named like a mod's file takes the name the mod would switch to.
    if let Some((_, extension)) = name.rsplit_once('.')
        && matches!(
            extension.to_ascii_lowercase().as_str(),
            "pak" | "utoc" | "ucas" | "disabled"
        )
    {
        return Err(format!("{name} is named like a mod's file"));
    }
    Ok(())
}

fn folder_dir(mods: &Path, path: &str) -> PathBuf {
    if path.is_empty() {
        mods.to_path_buf()
    } else {
        mods.join(path)
    }
}

/// `path` below `mods`, `/` between its parts.
fn below(mods: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(mods).ok()?;
    Some(relative.to_string_lossy().replace('\\', "/"))
}

/// Every folder below `mods`, empty ones too, `/` between their parts, sorted ignoring case.
pub fn list_folders(mods: &Path) -> Vec<String> {
    let mut folders: Vec<String> = walkdir::WalkDir::new(mods)
        .min_depth(1)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_dir())
        .filter_map(|entry| below(mods, entry.path()))
        .collect();
    folders.sort_by_key(|folder| folder.to_lowercase());
    folders
}

/// The name the mod `full_name` has once moved into the folder `to`.
pub fn moved_name(full_name: &str, to: &str) -> String {
    let leaf = full_name.rsplit('/').next().unwrap_or(full_name);
    if to.is_empty() {
        leaf.to_string()
    } else {
        format!("{to}/{leaf}")
    }
}

/// `path` with the folder `from` in it renamed `to`, when it is that folder or inside it.
pub fn renamed_under(path: &str, from: &str, to: &str) -> Option<String> {
    let rest = path.strip_prefix(from)?;
    if rest.is_empty() {
        Some(to.to_string())
    } else {
        rest.starts_with('/').then(|| format!("{to}{rest}"))
    }
}

/// Moves each mod named, all its files switched on or off together, into the folder `to` below
/// `mods`, made if it is not there. A mod of the same name already there is never written over.
pub fn move_mods(mods: &Path, names: &[String], to: &str) -> Result<BulkOpResult, String> {
    let to = folder_path(to)?;
    Ok(super::manage::each(names, |name| move_mod(mods, name, &to)))
}

fn move_mod(mods: &Path, full_name: &str, to: &str) -> Result<(), String> {
    let from = ModFiles::of(&mods.join(full_name))?;
    let dir = folder_dir(mods, to);
    if same_folder(&from.dir, &dir) {
        return Ok(());
    }
    let dest = ModFiles {
        dir: dir.clone(),
        ..from.clone()
    };
    if !dest.any_files().is_empty() {
        let shown = if to.is_empty() { "~mods" } else { to };
        return Err(format!("{shown} already holds a mod named {}", from.stem));
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("Could not make {to}: {e}"))?;
    let moves: Vec<(PathBuf, PathBuf)> = from
        .any_files()
        .into_iter()
        .filter_map(|path| {
            let name = path.file_name()?.to_os_string();
            Some((path, dir.join(name)))
        })
        .collect();
    crate::mods::rename_all(&moves)
}

fn same_folder(a: &Path, b: &Path) -> bool {
    a == b
        || matches!(
            (a.canonicalize(), b.canonicalize()),
            (Ok(a), Ok(b)) if a == b
        )
}

/// Makes the folder `path` below `mods`, and the folders above it that are not there yet. Returns
/// it as [`folder_path`] gives it.
pub fn create_folder(mods: &Path, path: &str) -> Result<String, String> {
    let path = folder_path(path)?;
    if path.is_empty() {
        return Err("a folder needs a name".to_string());
    }
    let dir = mods.join(&path);
    if dir.exists() {
        return Err(format!("{path} already exists"));
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("Could not make {path}: {e}"))?;
    Ok(path)
}

/// Renames the folder `path` below `mods` to `new_name`, where it is. Returns its new path.
pub fn rename_folder(mods: &Path, path: &str, new_name: &str) -> Result<String, String> {
    let path = folder_path(path)?;
    if path.is_empty() {
        return Err("~mods itself cannot be renamed".to_string());
    }
    let new_name = new_name.trim();
    folder_name(new_name)?;
    let from = mods.join(&path);
    if !from.is_dir() {
        return Err(format!("{path} is not a folder in ~mods"));
    }
    let renamed = match path.rsplit_once('/') {
        Some((parent, _)) => format!("{parent}/{new_name}"),
        None => new_name.to_string(),
    };
    if renamed == path {
        return Ok(renamed);
    }
    let to = mods.join(&renamed);
    // A change of case alone finds the folder itself there on Windows, and another folder on Linux.
    if to.exists() && !(cfg!(windows) && renamed.eq_ignore_ascii_case(&path)) {
        return Err(format!("{renamed} already exists"));
    }
    std::fs::rename(&from, &to)
        .map_err(|e| format!("Could not rename {path}: {e}{}", held_open_hint(&e)))?;
    Ok(renamed)
}

/// Deletes the folder `path` below `mods`. One holding mods is refused unless `with_mods`, which
/// deletes them as deleting each would. Files there that are no mod's stay, with the folders that
/// hold them.
pub fn delete_folder(
    mods: &Path,
    path: &str,
    with_mods: bool,
) -> Result<FolderDeleteReport, String> {
    let path = folder_path(path)?;
    if path.is_empty() {
        return Err("~mods itself cannot be deleted".to_string());
    }
    let dir = mods.join(&path);
    if !dir.is_dir() {
        return Err(format!("{path} is not a folder in ~mods"));
    }
    let names: Vec<String> = crate::mods::walk_mod_files(&dir, true)
        .into_iter()
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
        .filter(|name| name.ends_with(".pak") || name.ends_with(".pak.disabled"))
        .map(|name| format!("{path}/{name}"))
        .collect();
    if !names.is_empty() && !with_mods {
        let s = if names.len() == 1 { "" } else { "s" };
        return Err(format!("{path} holds {} mod{s}", names.len()));
    }
    let deleted = super::delete_many(mods, &names);
    let folders = walkdir::WalkDir::new(&dir).contents_first(true);
    for entry in folders.into_iter().filter_map(Result::ok) {
        if entry.file_type().is_dir() {
            // One still holding something stays, and what it holds is reported below.
            let _ = std::fs::remove_dir(entry.path());
        }
    }
    let kept = walkdir::WalkDir::new(&dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| below(mods, entry.path()))
        .collect();
    Ok(FolderDeleteReport {
        deleted: deleted.successes,
        failures: deleted.failures,
        kept,
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rivals-folders-{tag}-{stamp}"));
        std::fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    fn trio(dir: &Path, stem: &str, suffix: &str) {
        std::fs::create_dir_all(dir).expect("dir");
        for ext in ["pak", "ucas", "utoc"] {
            std::fs::write(dir.join(format!("{stem}.{ext}{suffix}")), ext).expect("write");
        }
    }

    #[test]
    fn a_folder_path_is_normalised_and_kept_inside_mods() {
        assert_eq!(folder_path(" Skins\\Venom ").unwrap(), "Skins/Venom");
        assert_eq!(folder_path("").unwrap(), "");
        assert_eq!(folder_path(".hidden").unwrap(), ".hidden");
        for bad in [
            "/Skins",
            "C:/Mods",
            "Skins//Venom",
            "Skins/",
            "../Paks",
            "Skins/./Venom",
            "Skin?",
            "a\u{1}b",
            "Skins /Venom",
            "Skins/ Venom",
            "Skins.",
            "con",
            "Aux.txt",
            "LPT1",
            "Venom.pak",
            "Venom.utoc.disabled",
        ] {
            assert!(folder_path(bad).is_err(), "{bad} should be refused");
        }
        assert!(folder_path(&"x".repeat(256)).is_err());
        assert!(
            folder_path("Console").is_ok(),
            "only the whole name is reserved"
        );
    }

    #[test]
    fn every_folder_is_listed_empty_ones_too_and_no_file() {
        let mods = scratch("list");
        trio(&mods.join("Skins/Venom"), "A", "");
        std::fs::create_dir_all(mods.join("empty")).unwrap();
        trio(&mods, "Top", "");
        assert_eq!(list_folders(&mods), ["empty", "Skins", "Skins/Venom"]);
        std::fs::remove_dir_all(&mods).ok();
    }

    #[test]
    fn a_mod_moves_whole_in_the_state_it_is_in() {
        let mods = scratch("move");
        trio(&mods, "Off", ".disabled");
        trio(&mods, "Mixed", "");
        std::fs::rename(mods.join("Mixed.pak"), mods.join("Mixed.pak.disabled")).unwrap();
        let names = [
            "Off.pak.disabled".to_string(),
            "Mixed.pak.disabled".to_string(),
        ];
        let moved = move_mods(&mods, &names, "Skins\\Venom").unwrap();
        assert_eq!(moved.successes, 2, "{:?}", moved.failures);
        for ext in ["pak", "ucas", "utoc"] {
            assert!(
                mods.join(format!("Skins/Venom/Off.{ext}.disabled"))
                    .is_file()
            );
        }
        assert!(mods.join("Skins/Venom/Mixed.pak.disabled").is_file());
        assert!(mods.join("Skins/Venom/Mixed.utoc").is_file());
        assert!(
            std::fs::read_dir(&mods)
                .unwrap()
                .all(|e| e.unwrap().path().is_dir())
        );

        let back = move_mods(&mods, &["Skins/Venom/Off.pak.disabled".to_string()], "").unwrap();
        assert_eq!(back.successes, 1);
        assert!(mods.join("Off.pak.disabled").is_file());
        let stay = move_mods(&mods, &["Off.pak.disabled".to_string()], "").unwrap();
        assert_eq!(stay.successes, 1, "a move to where it is does nothing");
        std::fs::remove_dir_all(&mods).ok();
    }

    #[test]
    fn a_move_onto_a_mod_of_the_same_name_moves_nothing() {
        let mods = scratch("move-taken");
        trio(&mods, "Mod", "");
        trio(&mods.join("Skins"), "Mod", ".disabled");
        let moved = move_mods(&mods, &["Mod.pak".to_string()], "Skins").unwrap();
        assert_eq!(moved.failures.len(), 1);
        assert!(
            moved.failures[0].error.contains("already holds"),
            "{:?}",
            moved.failures
        );
        assert!(mods.join("Mod.pak").is_file());
        assert!(move_mods(&mods, &["Mod.pak".to_string()], "../Out").is_err());
        std::fs::remove_dir_all(&mods).ok();
    }

    #[test]
    fn a_move_that_fails_part_way_puts_the_mod_back() {
        let mods = scratch("move-rollback");
        trio(&mods, "Mod", "");
        // A folder where the container would land: no mod's file, so the move starts, then fails.
        std::fs::create_dir_all(mods.join("Skins/Mod.utoc/inside")).unwrap();
        let moved = move_mods(&mods, &["Mod.pak".to_string()], "Skins").unwrap();
        assert_eq!(moved.failures.len(), 1);
        for ext in ["pak", "ucas", "utoc"] {
            assert!(
                mods.join(format!("Mod.{ext}")).is_file(),
                "Mod.{ext} is back"
            );
        }
        std::fs::remove_dir_all(&mods).ok();
    }

    #[test]
    fn folders_are_made_and_renamed_where_they_are() {
        let mods = scratch("make");
        assert_eq!(create_folder(&mods, "Skins/Venom").unwrap(), "Skins/Venom");
        assert!(
            create_folder(&mods, "Skins").is_err(),
            "it is there already"
        );
        assert!(create_folder(&mods, "").is_err());
        create_folder(&mods, "Skins/Loki").unwrap();

        assert_eq!(
            rename_folder(&mods, "Skins/Venom", "Red").unwrap(),
            "Skins/Red"
        );
        assert!(mods.join("Skins/Red").is_dir());
        assert!(
            rename_folder(&mods, "Skins/Red", "Loki").is_err(),
            "a sibling has it"
        );
        assert!(rename_folder(&mods, "Skins/Red", "a/b").is_err());
        assert_eq!(
            rename_folder(&mods, "Skins/Red", "RED").unwrap(),
            "Skins/RED"
        );
        assert!(rename_folder(&mods, "Missing", "X").is_err());
        std::fs::remove_dir_all(&mods).ok();
    }

    #[test]
    fn a_folder_deletes_with_its_mods_only_when_asked_and_keeps_other_files() {
        let mods = scratch("delete");
        trio(&mods.join("Skins"), "On", "");
        trio(&mods.join("Skins/Venom"), "Off", ".disabled");
        std::fs::create_dir_all(mods.join("Skins/Venom/empty")).unwrap();
        std::fs::write(mods.join("Skins/readme.txt"), "hi").unwrap();
        trio(&mods, "Top", "");

        let refused = delete_folder(&mods, "Skins", false).expect_err("it holds mods");
        assert!(refused.contains("2 mods"), "{refused}");
        let report = delete_folder(&mods, "Skins", true).unwrap();
        assert_eq!(report.deleted, 2);
        assert!(report.failures.is_empty());
        assert_eq!(report.kept, ["Skins/readme.txt"]);
        assert!(!mods.join("Skins/Venom").exists());
        assert!(mods.join("Top.pak").is_file());

        std::fs::remove_file(mods.join("Skins/readme.txt")).unwrap();
        assert!(
            delete_folder(&mods, "Skins", false)
                .unwrap()
                .kept
                .is_empty()
        );
        assert!(!mods.join("Skins").exists());
        assert!(delete_folder(&mods, "", true).is_err());
        std::fs::remove_dir_all(&mods).ok();
    }

    #[test]
    fn names_follow_a_move_and_a_folder_rename() {
        assert_eq!(moved_name("Skins/Venom.pak", ""), "Venom.pak");
        assert_eq!(
            moved_name("Venom.pak.disabled", "Skins"),
            "Skins/Venom.pak.disabled"
        );
        assert_eq!(
            renamed_under("Skins", "Skins", "Art").as_deref(),
            Some("Art")
        );
        assert_eq!(
            renamed_under("Skins/x.pak", "Skins", "Art").as_deref(),
            Some("Art/x.pak")
        );
        assert_eq!(renamed_under("Skinsy/x.pak", "Skins", "Art"), None);
        assert_eq!(renamed_under("Other.pak", "Skins", "Art"), None);
    }
}
