//! Tauri commands for mod install, toggle, export, delete, and status queries.

use tauri::{AppHandle, Manager, State};

use crate::game_status;
use crate::mods;
use crate::mods::hero_cache::HeroCacheState;
use crate::mods::heroes::enrich_status_with_heroes;
use crate::mods::{BulkOpResult, ConflictReport, InstallResult, ModsStatus};
use crate::settings::{SettingsState, recursive_mod_scan};

#[tauri::command]
pub(crate) async fn get_mods_status(
    app: AppHandle,
    game_root: String,
) -> Result<ModsStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<SettingsState>();
        let cache = app.state::<HeroCacheState>();
        let recursive = recursive_mod_scan(&state);
        let mut status = mods::get_mods_status(&game_root, recursive);
        enrich_status_with_heroes(&cache, &mut status);
        status
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn check_mod_conflicts(
    state: State<'_, SettingsState>,
    game_root: String,
) -> Result<ConflictReport, String> {
    let recursive = recursive_mod_scan(&state);
    tauri::async_runtime::spawn_blocking(move || mods::check_conflicts(&game_root, recursive))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) fn install_signature_bypass(game_root: String) -> Result<String, String> {
    if crate::game_status::should_block_for_game() {
        return Err(crate::game_status::game_running_error());
    }
    mods::install_signature_bypass(&game_root)
}

#[tauri::command]
pub(crate) fn remove_signature_bypass(game_root: String) -> Result<String, String> {
    if crate::game_status::should_block_for_game() {
        return Err(crate::game_status::game_running_error());
    }
    mods::remove_signature_bypass(&game_root)
}

#[tauri::command]
pub(crate) fn is_signature_bypass_installed(game_root: String) -> bool {
    mods::is_signature_bypass_installed(&game_root)
}

#[tauri::command]
pub(crate) fn get_signature_bypass_kind(game_root: String) -> mods::BypassKind {
    mods::signature_bypass_kind(&game_root)
}

#[tauri::command]
pub(crate) fn open_mods_folder(game_root: String) -> Result<(), String> {
    mods::open_mods_folder(&game_root)
}

#[tauri::command]
pub(crate) fn toggle_mod_enabled(
    mods_folder: String,
    full_name: String,
    enabled: bool,
) -> Result<(), String> {
    if game_status::should_block_for_game() {
        return Err(game_status::game_running_error());
    }
    mods::toggle_mod_enabled(&mods_folder, &full_name, enabled)
}

#[tauri::command]
pub(crate) async fn export_mods_archive(
    state: State<'_, SettingsState>,
    mods_folder: String,
    dest_path: String,
) -> Result<String, String> {
    let recursive = recursive_mod_scan(&state);
    tauri::async_runtime::spawn_blocking(move || {
        mods::export_mods_archive(&mods_folder, &dest_path, recursive)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) fn rename_mod(
    mods_folder: String,
    full_name: String,
    new_base: String,
) -> Result<String, String> {
    if game_status::should_block_for_game() {
        return Err(game_status::game_running_error());
    }
    mods::rename_mod(&mods_folder, &full_name, &new_base)
}

#[tauri::command]
pub(crate) fn delete_mod(mods_folder: String, full_name: String) -> Result<(), String> {
    if game_status::should_block_for_game() {
        return Err(game_status::game_running_error());
    }
    mods::delete_mod(&mods_folder, &full_name)
}

#[tauri::command]
pub(crate) fn toggle_mods_enabled(
    mods_folder: String,
    full_names: Vec<String>,
    enabled: bool,
) -> Result<BulkOpResult, String> {
    if game_status::should_block_for_game() {
        return Err(game_status::game_running_error());
    }
    Ok(mods::toggle_mods_enabled(
        &mods_folder,
        &full_names,
        enabled,
    ))
}

#[tauri::command]
pub(crate) fn delete_mods(
    mods_folder: String,
    full_names: Vec<String>,
) -> Result<BulkOpResult, String> {
    if game_status::should_block_for_game() {
        return Err(game_status::game_running_error());
    }
    Ok(mods::delete_mods(&mods_folder, &full_names))
}

#[tauri::command]
pub(crate) fn install_mod(
    mods_folder: String,
    source_path: String,
) -> Result<InstallResult, String> {
    if game_status::should_block_for_game() {
        return Err(game_status::game_running_error());
    }
    mods::install_mod(&mods_folder, &source_path)
}

#[tauri::command]
pub(crate) async fn install_from_archive(
    mods_folder: String,
    archive_path: String,
) -> Result<Vec<InstallResult>, String> {
    if game_status::should_block_for_game() {
        return Err(game_status::game_running_error());
    }
    tauri::async_runtime::spawn_blocking(move || {
        mods::install_from_archive(&mods_folder, &archive_path)
    })
    .await
    .map_err(|e| e.to_string())?
}
