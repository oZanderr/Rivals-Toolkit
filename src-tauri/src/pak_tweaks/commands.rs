//! Tauri commands for pak INI inspection, tweak detection, and editing.

use tauri::State;

use crate::pak_tweaks;
use crate::pak_tweaks::{PakIniFileContent, PakIniInfo, PakTweakEdit};
use crate::settings::{SettingsState, recursive_mod_scan};
use crate::tweaks::TweakState;

#[tauri::command]
pub(crate) async fn inspect_pak_path(pak_path: String) -> Result<Option<PakIniInfo>, String> {
    tauri::async_runtime::spawn_blocking(move || pak_tweaks::inspect_single_pak(&pak_path))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn scan_mod_paks_for_ini(
    state: State<'_, SettingsState>,
    game_root: String,
) -> Result<Vec<PakIniInfo>, String> {
    let recursive = recursive_mod_scan(&state);
    tauri::async_runtime::spawn_blocking(move || pak_tweaks::scan_mod_paks(&game_root, recursive))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn detect_pak_tweaks(pak_path: String) -> Result<Vec<TweakState>, String> {
    tauri::async_runtime::spawn_blocking(move || pak_tweaks::detect_pak_tweaks(&pak_path))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn apply_pak_tweak_edits(
    pak_path: String,
    edits: Vec<PakTweakEdit>,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || pak_tweaks::apply_pak_tweaks(&pak_path, &edits))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn extract_pak_ini(pak_path: String, entry: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || pak_tweaks::extract_pak_ini(&pak_path, &entry))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn save_pak_ini(
    pak_path: String,
    files: Vec<PakIniFileContent>,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || pak_tweaks::save_pak_ini(&pak_path, files))
        .await
        .map_err(|e| e.to_string())?
}
