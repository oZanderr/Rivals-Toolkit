//! File system helpers for listing and managing the ~mods folder.

use std::path::Path;

use rivals_core::mod_library::{self, InstallOptions};
use rivals_core::pak::iostore_out::IoStoreOptions;
use serde::Serialize;

use crate::paths::mods_dir;

pub(crate) use rivals_core::mod_library::{BulkOpResult, InstallResult};

pub(crate) fn open_mods_folder(game_root: &str) -> Result<(), String> {
    let mods = mods_dir(game_root);
    if !mods.exists() {
        return Err("Mods folder does not exist, install the bypass first!".to_string());
    }
    #[cfg(target_os = "windows")]
    std::process::Command::new("explorer")
        .arg(mods.to_string_lossy().as_ref())
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

const COMPANION_EXTS: &[&str] = &["ucas", "utoc"];

/// Enable or disable a mod and its companion `.ucas`/`.utoc` files, as one step that never writes
/// over a file already at a name it takes.
pub(crate) fn toggle_mod_enabled(
    mods_folder: &str,
    full_name: &str,
    enabled: bool,
) -> Result<(), String> {
    mod_library::toggle(Path::new(mods_folder), full_name, enabled).map(|_| ())
}

/// Toggle multiple mods in sequence, collecting per-item failures.
pub(crate) fn toggle_mods_enabled(
    mods_folder: &str,
    names: &[String],
    enabled: bool,
) -> BulkOpResult {
    mod_library::toggle_many(Path::new(mods_folder), names, enabled)
}

/// Delete multiple mods in sequence, collecting per-item failures.
pub(crate) fn delete_mods(mods_folder: &str, names: &[String]) -> BulkOpResult {
    mod_library::delete_many(Path::new(mods_folder), names)
}

/// Delete a mod and its companion `.ucas`/`.utoc` files.
pub(crate) fn delete_mod(mods_folder: &str, full_name: &str) -> Result<(), String> {
    rivals_core::mods::delete_mod(Path::new(mods_folder), full_name)
}

/// Rename a mod and its companion `.ucas`/`.utoc` files.
///
/// `full_name` is the current filename (e.g. `Foo.pak` or `Foo.pak.disabled`).
/// `new_base` is the desired stem without extension (e.g. `Bar`).
/// Returns the new `full_name` so the frontend can update its state.
pub(crate) fn rename_mod(
    mods_folder: &str,
    full_name: &str,
    new_base: &str,
) -> Result<String, String> {
    let new_base = new_base.trim();
    if new_base.is_empty() {
        return Err("Name cannot be empty.".to_string());
    }
    if new_base.contains(['/', '\\', '\0']) {
        return Err("Name contains invalid characters.".to_string());
    }

    let dir = Path::new(mods_folder);

    let (old_rel_stem, disabled) = if let Some(s) = full_name.strip_suffix(".pak.disabled") {
        (s, true)
    } else if let Some(s) = full_name.strip_suffix(".pak") {
        (s, false)
    } else {
        return Err(format!("Unexpected mod filename: {full_name}"));
    };

    // Keep the mod in its current subdirectory; only the filename changes.
    let parent_rel = Path::new(full_name)
        .parent()
        .unwrap_or_else(|| Path::new(""));
    let join_rel = |name: &str| -> String {
        if parent_rel.as_os_str().is_empty() {
            name.to_string()
        } else {
            format!("{}/{name}", parent_rel.to_string_lossy().replace('\\', "/"))
        }
    };

    let new_full_name = if disabled {
        join_rel(&format!("{new_base}.pak.disabled"))
    } else {
        join_rel(&format!("{new_base}.pak"))
    };

    if new_full_name == full_name {
        return Ok(new_full_name);
    }

    for candidate in [
        join_rel(&format!("{new_base}.pak")),
        join_rel(&format!("{new_base}.pak.disabled")),
    ] {
        if dir.join(&candidate).exists() {
            return Err(format!("A mod named \"{candidate}\" already exists."));
        }
    }

    // A container's id comes from its file name, so a container renamed on disk alone still mounts
    // under its old id: it is rebuilt under the new one. A change of case keeps the id.
    let old_base = Path::new(old_rel_stem)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !old_base.eq_ignore_ascii_case(new_base)
        && let Ok(files) = rivals_core::mod_ops::ModFiles::of(&dir.join(full_name))
        && files.is_iostore()
    {
        rivals_core::mod_ops::rename_mod(&files, new_base, &Default::default())?;
        return Ok(new_full_name);
    }

    let mut moves = vec![(dir.join(full_name), dir.join(&new_full_name))];
    for ext in COMPANION_EXTS {
        for suffix in ["", ".disabled"] {
            let old_path = dir.join(format!("{old_rel_stem}.{ext}{suffix}"));
            if old_path.exists() {
                let renamed = join_rel(&format!("{new_base}.{ext}{suffix}"));
                moves.push((old_path, dir.join(renamed)));
            }
        }
    }
    // A pak renamed without its container leaves the old container still mounted under the old
    // name, so the set moves together or not at all.
    rivals_core::mods::rename_all(&moves)?;
    Ok(new_full_name)
}

/// One mod an install found, and what installing it did.
#[derive(Debug, Serialize)]
pub(crate) struct InstallOutcome {
    /// The paths given that named what it was found in.
    pub(crate) sources: Vec<String>,
    /// Where it was: the path given, or its path in the folder or archive given.
    pub(crate) at: String,
    pub(crate) installed: Option<InstallResult>,
    pub(crate) error: Option<String>,
}

/// Installs the mods at `sources` into the mods folder, over whatever copy of each was there: a
/// pak with the container beside it, an archive, or a folder holding any mix of mods. A folder of
/// a mod's loose files, and a plain pak holding packages, are packed into a container as `io`
/// says, a shader archive in one checked against the libraries of the game at `game_root`.
pub(crate) fn install_mods(
    mods_folder: &str,
    sources: &[String],
    io: IoStoreOptions,
    game_root: Option<&str>,
) -> Vec<InstallOutcome> {
    let game_libraries = || {
        let root = game_root.ok_or(
            "Set the game folder first: a shader archive is checked against the game's own \
             libraries",
        )?;
        rivals_core::pak::shader_library::game_library_names(&rivals_core::paths::paks_dir(root))
    };
    let options = InstallOptions {
        replace: true,
        dry_run: false,
        io,
        game_libraries: &game_libraries,
    };
    let paths: Vec<std::path::PathBuf> = sources.iter().map(Into::into).collect();
    mod_library::install_each(Path::new(mods_folder), &paths, &options)
        .into_iter()
        .flat_map(|each| {
            let named: Vec<String> = each
                .named
                .iter()
                .map(|path| path.display().to_string())
                .collect();
            match each.result {
                Ok(found) => found
                    .into_iter()
                    .map(|found| {
                        let (installed, error) = match found.result {
                            Ok(installed) => (Some(installed), None),
                            Err(error) => (None, Some(error)),
                        };
                        InstallOutcome {
                            sources: named.clone(),
                            at: found.at,
                            installed,
                            error,
                        }
                    })
                    .collect(),
                Err(error) => vec![InstallOutcome {
                    at: named[0].clone(),
                    sources: named,
                    installed: None,
                    error: Some(error),
                }],
            }
        })
        .collect()
}

pub(crate) fn export_mods_archive(
    mods_folder: &str,
    dest_path: &str,
    recursive: bool,
) -> Result<String, String> {
    let count =
        mod_library::export_archive(Path::new(mods_folder), Path::new(dest_path), recursive)?;
    Ok(format!(
        "Exported {count} mod{}",
        if count == 1 { "" } else { "s" }
    ))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rivals-folder-{tag}-{stamp}"));
        std::fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    fn trio(dir: &Path, suffix: &str) {
        for ext in ["pak", "ucas", "utoc"] {
            std::fs::write(dir.join(format!("Mod.{ext}{suffix}")), ext).expect("write");
        }
    }

    /// Renaming a pak without its container leaves the container mounted under the old name. A
    /// mod whose container is switched off apart from its pak is renamed file by file, so the set
    /// still moves together or not at all.
    #[test]
    fn a_rename_that_cannot_move_a_companion_leaves_the_name_alone() {
        let dir = scratch("rename");
        std::fs::write(dir.join("Mod.pak"), "pak").expect("write");
        trio(&dir, ".disabled");
        std::fs::remove_file(dir.join("Mod.pak.disabled")).expect("only the pak is on");
        std::fs::create_dir_all(dir.join("New.ucas.disabled")).expect("blocker");

        let error =
            rename_mod(&dir.to_string_lossy(), "Mod.pak", "New").expect_err("the ucas cannot move");
        assert!(error.contains("Mod.ucas"), "{error}");
        assert!(dir.join("Mod.pak").is_file(), "Mod.pak keeps its name");
        for ext in ["ucas", "utoc"] {
            assert!(
                dir.join(format!("Mod.{ext}.disabled")).is_file(),
                "Mod.{ext} keeps its name"
            );
        }
        assert!(
            !dir.join("New.pak").exists(),
            "the pak was not left renamed"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// An IoStore mod is rebuilt under its new name, so a container that cannot be read is not
    /// renamed at all, rather than renamed on disk to mount under its old id.
    #[test]
    fn an_iostore_rename_that_cannot_rebuild_leaves_the_mod_alone() {
        let dir = scratch("rename-rebuild");
        trio(&dir, "");
        rename_mod(&dir.to_string_lossy(), "Mod.pak", "New").expect_err("not a real container");
        for ext in ["pak", "ucas", "utoc"] {
            assert!(
                dir.join(format!("Mod.{ext}")).is_file(),
                "Mod.{ext} is left"
            );
            assert!(!dir.join(format!("New.{ext}")).exists(), "no New.{ext}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A pak-only mod has no container id to rebuild, so its file is renamed as it was.
    #[test]
    fn a_pak_only_rename_moves_the_file() {
        let dir = scratch("rename-pak");
        std::fs::write(dir.join("Mod.pak.disabled"), b"pak").expect("write");
        let renamed =
            rename_mod(&dir.to_string_lossy(), "Mod.pak.disabled", "New").expect("rename");
        assert_eq!(renamed, "New.pak.disabled");
        assert!(dir.join("New.pak.disabled").is_file());
        std::fs::remove_dir_all(&dir).ok();
    }
}
