//! Mod folder operations: enumerate, install, toggle, delete, conflict-check, and bypass utilities for `~mods`.

mod bypass;
pub(crate) mod character_sync;
pub(crate) mod commands;
pub(crate) mod conflicts;
mod folder;
pub(crate) mod hero_cache;
pub(crate) mod heroes;
pub(crate) mod profiles;
mod status;

pub(crate) use bypass::{BypassKind, BypassRole, bypass_role, install_asi_loader};
pub(crate) use conflicts::{CachedLister, ConflictReport};
pub(crate) use folder::{BulkOpResult, InstallOutcome};
pub(crate) use status::ModsStatus;

// Minimal ASI Loader (oZanderr/minimal-asi-loader) proxy DLL named dsound.dll; loads the payload
// below.
static BYPASS_ASI_LOADER: &[u8] = include_bytes!("../resources/bypass/dsound.dll");
// The original community UTOC bypass payload, redistributed unmodified (resources/bypass/NOTICE.md).
static BYPASS_PAYLOAD_ASI: &[u8] =
    include_bytes!("../resources/bypass/MarvelRivalsUTOCSignatureBypass.asi");

pub(crate) use rivals_core::mods::walk_mod_files;

/// Total on-disk size of a mod pak plus companion `.ucas`/`.utoc` when present. The size the
/// status reports, so hero-cache validation compares like for like.
pub(crate) use rivals_core::mod_library::size_on_disk as mod_size_on_disk;

pub(crate) fn get_mods_status(game_root: &str, recursive: bool) -> ModsStatus {
    status::get_mods_status(game_root, recursive)
}

pub(crate) fn install_signature_bypass(game_root: &str) -> Result<String, String> {
    bypass::install_signature_bypass(game_root)
}

pub(crate) fn remove_signature_bypass(game_root: &str) -> Result<String, String> {
    bypass::remove_signature_bypass(game_root)
}

pub(crate) fn is_signature_bypass_installed(game_root: &str) -> bool {
    bypass::is_signature_bypass_installed(game_root)
}

pub(crate) fn signature_bypass_kind(game_root: &str) -> BypassKind {
    bypass::bypass_install_kind(game_root)
}

pub(crate) fn open_mods_folder(game_root: &str) -> Result<(), String> {
    folder::open_mods_folder(game_root)
}

pub(crate) fn toggle_mod_enabled(
    mods_folder: &str,
    full_name: &str,
    enabled: bool,
) -> Result<(), String> {
    folder::toggle_mod_enabled(mods_folder, full_name, enabled)
}

pub(crate) fn export_mods_archive(
    mods_folder: &str,
    dest_path: &str,
    recursive: bool,
) -> Result<String, String> {
    folder::export_mods_archive(mods_folder, dest_path, recursive)
}

pub(crate) fn delete_mod(mods_folder: &str, full_name: &str) -> Result<(), String> {
    folder::delete_mod(mods_folder, full_name)
}

pub(crate) fn toggle_mods_enabled(
    mods_folder: &str,
    names: &[String],
    enabled: bool,
) -> BulkOpResult {
    folder::toggle_mods_enabled(mods_folder, names, enabled)
}

pub(crate) fn delete_mods(mods_folder: &str, names: &[String]) -> BulkOpResult {
    folder::delete_mods(mods_folder, names)
}

pub(crate) fn install_mods(
    mods_folder: &str,
    sources: &[String],
    io: rivals_core::pak::iostore_out::IoStoreOptions,
    game_root: Option<&str>,
) -> Vec<InstallOutcome> {
    folder::install_mods(mods_folder, sources, io, game_root)
}

pub(crate) fn rename_mod(
    mods_folder: &str,
    full_name: &str,
    new_base: &str,
) -> Result<String, String> {
    folder::rename_mod(mods_folder, full_name, new_base)
}

pub(crate) fn check_conflicts(game_root: &str, recursive: bool) -> Result<ConflictReport, String> {
    conflicts::check_conflicts(game_root, recursive)
}
