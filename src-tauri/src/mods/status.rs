//! Reads and represents the installed state of mod pak files.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::paths::mods_dir;

use super::BypassKind;
use super::bypass::bypass_install_kind;
use super::heroes::HeroMatch;
use super::mod_size_on_disk;
use super::walk_mod_files;

#[derive(Serialize, Deserialize)]
pub(crate) enum ModKind {
    Pak,
    IoStore,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct ModEntry {
    /// Filename on disk, including optional `.disabled` suffix.
    pub full_name: String,
    /// Display name without `.disabled`.
    pub display_name: String,
    pub enabled: bool,
    /// Whether companion `.ucas`/`.utoc` files exist.
    pub has_companions: bool,
    /// Total size in bytes (pak + companions if present).
    pub size_bytes: u64,
    pub kind: ModKind,
    #[serde(default)]
    pub heroes: Vec<HeroMatch>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct ModsStatus {
    pub mods_folder_exists: bool,
    pub mods_folder_path: String,
    pub sig_bypass_kind: BypassKind,
    pub mod_entries: Vec<ModEntry>,
    /// Number of disabled duplicates auto-removed because an enabled version of the same mod existed.
    pub conflicts_resolved: u32,
}

pub(crate) fn get_mods_status(game_root: &str, recursive: bool) -> ModsStatus {
    let mods = mods_dir(game_root);
    let exists = mods.exists();

    let sig_bypass_kind = bypass_install_kind(game_root);

    let mut mod_entries: Vec<ModEntry> = if exists {
        walk_mod_files(&mods, recursive)
            .into_iter()
            .filter_map(|rel_path| {
                let full_name = rel_path.to_string_lossy().into_owned().replace('\\', "/");
                let parent = rel_path.parent().unwrap_or(Path::new(""));
                if full_name.ends_with(".pak") {
                    let file_stem = &rel_path.file_name()?.to_string_lossy();
                    let stem = &file_stem[..file_stem.len() - 4];
                    let dir = mods.join(parent);
                    let has_companions = dir.join(format!("{stem}.ucas")).exists();
                    let size_bytes = mod_size_on_disk(&mods, &full_name);
                    Some(ModEntry {
                        display_name: full_name.clone(),
                        full_name,
                        enabled: true,
                        has_companions,
                        size_bytes,
                        kind: if has_companions {
                            ModKind::IoStore
                        } else {
                            ModKind::Pak
                        },
                        heroes: Vec::new(),
                    })
                } else if full_name.ends_with(".pak.disabled") {
                    let display_name = full_name[..full_name.len() - ".disabled".len()].to_owned();
                    let file_stem = &rel_path.file_name()?.to_string_lossy();
                    let stem = &file_stem[..file_stem.len() - ".pak.disabled".len()];
                    let dir = mods.join(parent);
                    let has_companions = dir.join(format!("{stem}.ucas.disabled")).exists();
                    let size_bytes = mod_size_on_disk(&mods, &full_name);
                    Some(ModEntry {
                        display_name,
                        full_name,
                        enabled: false,
                        has_companions,
                        size_bytes,
                        kind: if has_companions {
                            ModKind::IoStore
                        } else {
                            ModKind::Pak
                        },
                        heroes: Vec::new(),
                    })
                } else {
                    None
                }
            })
            .collect()
    } else {
        Vec::new()
    };

    // Auto-resolve: if both an enabled and a disabled version of the same mod exist,
    // delete the disabled copy (it's stale — the enabled one supersedes it).
    let mut conflicts_resolved = 0u32;
    if exists {
        let enabled_names: std::collections::HashSet<String> = mod_entries
            .iter()
            .filter(|e| e.enabled)
            .map(|e| e.display_name.clone())
            .collect();
        let conflicting: std::collections::HashSet<String> = mod_entries
            .iter()
            .filter(|e| !e.enabled && enabled_names.contains(&e.display_name))
            .map(|e| e.full_name.clone())
            .collect();
        for full_name in &conflicting {
            if super::folder::delete_mod(&mods.to_string_lossy(), full_name).is_ok() {
                conflicts_resolved += 1;
            }
        }
        mod_entries.retain(|e| !conflicting.contains(&e.full_name));
    }

    mod_entries.sort_by(|a, b| {
        a.display_name
            .to_lowercase()
            .cmp(&b.display_name.to_lowercase())
    });

    ModsStatus {
        mods_folder_exists: exists,
        mods_folder_path: mods.to_string_lossy().into_owned(),
        sig_bypass_kind,
        mod_entries,
        conflicts_resolved,
    }
}
