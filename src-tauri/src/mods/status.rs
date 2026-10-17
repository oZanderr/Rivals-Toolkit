//! Reads and represents the installed state of mod pak files.

use serde::{Deserialize, Serialize};

use rivals_core::mod_library::{self, remove_stale_copies};

use super::BypassKind;
use super::bypass::bypass_install_kind;
use super::heroes::HeroMatch;

pub(crate) use rivals_core::mod_library::ModKind;

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
    let mut library = mod_library::list_mods(game_root, recursive);
    // A switched-off copy of a mod that is also switched on is stale: the switched-on one
    // supersedes it, so the app deletes it rather than list the mod twice.
    let conflicts_resolved = remove_stale_copies(game_root, &mut library);
    ModsStatus {
        mods_folder_exists: library.mods_folder_exists,
        mods_folder_path: library.mods_folder_path,
        sig_bypass_kind: bypass_install_kind(game_root),
        mod_entries: library
            .mods
            .into_iter()
            .map(|entry| ModEntry {
                full_name: entry.full_name,
                display_name: entry.display_name,
                enabled: entry.enabled,
                has_companions: entry.has_companions,
                size_bytes: entry.size_bytes,
                kind: entry.kind,
                heroes: Vec::new(),
            })
            .collect(),
        conflicts_resolved,
    }
}
