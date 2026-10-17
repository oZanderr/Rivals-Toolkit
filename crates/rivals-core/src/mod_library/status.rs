//! Every mod in `~mods`, switched on or off, with its kind, size and place in the load order.

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::mods::{pak_priority, walk_mod_files, winner_order};
use crate::paths::mods_dir;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModKind {
    /// A plain pak, which delivers loose files.
    Pak,
    /// A `.utoc` and `.ucas` the game loads packages from, with a pak beside them.
    IoStore,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModStatus {
    /// The pak's path below `~mods`, `/` between folders, with `.disabled` when switched off.
    pub full_name: String,
    /// The same without `.disabled`.
    pub display_name: String,
    pub enabled: bool,
    pub kind: ModKind,
    /// Whether the `.ucas` beside the pak is there, which is what makes it IoStore.
    pub has_companions: bool,
    /// The pak and, for IoStore, the `.utoc` and `.ucas` together.
    pub size_bytes: u64,
    /// The `_<number>_P` patch number, which mounts it above the game's patches.
    pub priority: Option<u32>,
    /// Where it stands among the switched-on mods, 1 for the one whose copy of a shared file wins.
    pub load_rank: Option<usize>,
    /// Whether its container is obfuscated.
    pub obfuscated: bool,
    /// A switched-off copy of a mod that is also switched on, left behind by an older toggle.
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModLibrary {
    pub mods_folder_exists: bool,
    pub mods_folder_path: String,
    pub mods: Vec<ModStatus>,
}

/// Total size on disk of a mod's pak and, when its `.ucas` is there, its `.utoc` and `.ucas`.
/// `full_name` is the pak's path below `mods`, `.disabled` or not.
pub fn size_on_disk(mods: &Path, full_name: &str) -> u64 {
    let size = |path: &Path| std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let pak_size = size(&mods.join(full_name));
    let (stem, suffix) = if let Some(stem) = full_name.strip_suffix(".pak.disabled") {
        (stem, ".disabled")
    } else if let Some(stem) = full_name.strip_suffix(".pak") {
        (stem, "")
    } else {
        return pak_size;
    };
    // A lone `.utoc` without its `.ucas` is no IoStore mod, so it does not count.
    let ucas = mods.join(format!("{stem}.ucas{suffix}"));
    if !ucas.exists() {
        return pak_size;
    }
    pak_size + size(&ucas) + size(&mods.join(format!("{stem}.utoc{suffix}")))
}

/// Every mod in `~mods`, below it too when `recursive`, sorted by name. Nothing is changed:
/// [`remove_stale_copies`] is what deletes a stale copy.
pub fn list_mods(game_root: &str, recursive: bool) -> ModLibrary {
    let mods = mods_dir(game_root);
    let exists = mods.is_dir();
    let mut entries: Vec<ModStatus> = if exists {
        walk_mod_files(&mods, recursive)
            .into_iter()
            .filter_map(|relative| {
                let full_name = relative.to_string_lossy().replace('\\', "/");
                let (stem, enabled) = if let Some(stem) = full_name.strip_suffix(".pak.disabled") {
                    (stem.to_string(), false)
                } else {
                    let stem = full_name.strip_suffix(".pak")?;
                    (stem.to_string(), true)
                };
                let suffix = if enabled { "" } else { ".disabled" };
                let has_companions = mods.join(format!("{stem}.ucas{suffix}")).exists();
                let utoc = mods.join(format!("{stem}.utoc{suffix}"));
                Some(ModStatus {
                    display_name: format!("{stem}.pak"),
                    size_bytes: size_on_disk(&mods, &full_name),
                    priority: pak_priority(&full_name),
                    load_rank: None,
                    obfuscated: has_companions && crate::pak::containers::utoc_is_obfuscated(&utoc),
                    stale: false,
                    kind: if has_companions {
                        ModKind::IoStore
                    } else {
                        ModKind::Pak
                    },
                    full_name,
                    enabled,
                    has_companions,
                })
            })
            .collect()
    } else {
        Vec::new()
    };

    let enabled: HashSet<String> = entries
        .iter()
        .filter(|entry| entry.enabled)
        .map(|entry| entry.display_name.to_lowercase())
        .collect();
    for entry in &mut entries {
        entry.stale = !entry.enabled && enabled.contains(&entry.display_name.to_lowercase());
    }
    let mut order: Vec<String> = entries
        .iter()
        .filter(|entry| entry.enabled)
        .map(|entry| entry.display_name.clone())
        .collect();
    order.sort_by(|a, b| winner_order(a, b));
    for entry in &mut entries {
        if entry.enabled {
            entry.load_rank = order
                .iter()
                .position(|name| *name == entry.display_name)
                .map(|at| at + 1);
        }
    }
    entries.sort_by(|a, b| {
        a.display_name
            .to_lowercase()
            .cmp(&b.display_name.to_lowercase())
            .then(b.enabled.cmp(&a.enabled))
    });
    ModLibrary {
        mods_folder_exists: exists,
        mods_folder_path: mods.to_string_lossy().into_owned(),
        mods: entries,
    }
}

/// Deletes the stale copies `library` lists, the switched-off ones of mods also switched on, and
/// takes them out of it. Returns how many went.
pub fn remove_stale_copies(game_root: &str, library: &mut ModLibrary) -> u32 {
    let mods = mods_dir(game_root);
    let mut removed = 0;
    library.mods.retain(|entry| {
        if entry.stale && crate::mods::delete_mod(&mods, &entry.full_name).is_ok() {
            removed += 1;
            return false;
        }
        true
    });
    removed
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
        let root = std::env::temp_dir().join(format!("rivals-library-{tag}-{stamp}"));
        std::fs::create_dir_all(mods_dir(root.to_str().unwrap())).expect("scratch");
        root
    }

    fn touch(path: &Path, bytes: usize) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, vec![0u8; bytes]).unwrap();
    }

    /// Kinds, sizes, load order and a stale copy, below `~mods` too when asked; nothing deleted
    /// until asked.
    #[test]
    fn the_library_lists_every_mod_with_its_state() {
        let root = scratch("list");
        let game = root.to_str().unwrap();
        let mods = mods_dir(game);
        touch(&mods.join("A_9999999_P.pak"), 10);
        touch(&mods.join("A_9999999_P.utoc"), 20);
        touch(&mods.join("A_9999999_P.ucas"), 30);
        touch(&mods.join("B_100_P.pak"), 5);
        touch(&mods.join("B_100_P.pak.disabled"), 5);
        touch(&mods.join("Off_9999999_P.pak.disabled"), 7);
        touch(&mods.join("Sub/Deep_9999999_P.pak"), 3);

        let library = list_mods(game, false);
        let names: Vec<(&str, bool)> = library
            .mods
            .iter()
            .map(|m| (m.full_name.as_str(), m.enabled))
            .collect();
        assert_eq!(
            names,
            vec![
                ("A_9999999_P.pak", true),
                ("B_100_P.pak", true),
                ("B_100_P.pak.disabled", false),
                ("Off_9999999_P.pak.disabled", false),
            ]
        );
        let a = &library.mods[0];
        assert_eq!(
            (a.kind, a.size_bytes, a.priority),
            (ModKind::IoStore, 60, Some(9999999))
        );
        assert_eq!(a.load_rank, Some(1), "the higher patch number wins");
        assert_eq!(library.mods[1].load_rank, Some(2));
        assert!(library.mods[2].stale && !library.mods[3].stale);
        assert_eq!(library.mods[3].load_rank, None);

        let deep = list_mods(game, true);
        assert!(
            deep.mods
                .iter()
                .any(|m| m.full_name == "Sub/Deep_9999999_P.pak")
        );

        let mut library = library;
        assert_eq!(remove_stale_copies(game, &mut library), 1);
        assert!(!mods.join("B_100_P.pak.disabled").exists());
        assert!(mods.join("B_100_P.pak").exists());
        assert_eq!(library.mods.len(), 3);
        std::fs::remove_dir_all(&root).ok();
    }
}
