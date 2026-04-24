//! Tauri command wrappers for pak/IoStore operations.

use tauri::{AppHandle, State};

use crate::pak;
use crate::pak::PakFileInfo;
use crate::settings::{SettingsState, recursive_mod_scan};

#[tauri::command]
pub(crate) fn list_pak_files(
    state: State<'_, SettingsState>,
    game_root: String,
) -> Result<Vec<String>, String> {
    pak::list_pak_files(&game_root, recursive_mod_scan(&state))
}

#[tauri::command]
pub(crate) fn list_pak_files_info(
    state: State<'_, SettingsState>,
    game_root: String,
) -> Result<Vec<PakFileInfo>, String> {
    pak::list_pak_files_info(&game_root, recursive_mod_scan(&state))
}

#[tauri::command]
pub(crate) async fn list_pak_contents(pak_path: String) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || pak::list_pak_contents(&pak_path))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn unpack_pak(
    pak_path: String,
    output_dir: String,
    skip: Vec<String>,
) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let skip_refs: Vec<&str> = skip.iter().map(|s| s.as_str()).collect();
        pak::unpack_pak(&pak_path, &output_dir, &skip_refs)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn extract_single_file(
    pak_path: String,
    file_name: String,
    output_path: String,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        pak::extract_single_file(&pak_path, &file_name, &output_path)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn extract_pak_files(
    pak_path: String,
    file_names: Vec<String>,
    output_dir: String,
) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        pak::extract_pak_files(&pak_path, &file_names, &output_dir)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn extract_utoc_files(
    utoc_path: String,
    file_names: Vec<String>,
    output_dir: String,
) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        pak::extract_utoc_files(&utoc_path, &file_names, &output_dir)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn repack_pak(input_dir: String, output_pak: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || pak::repack_pak(&input_dir, &output_pak))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn repack_iostore(
    input_dir: String,
    output_utoc: String,
    app: AppHandle,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || pak::repack_iostore(&input_dir, &output_utoc, app))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) fn cancel_repack_iostore() {
    pak::cancel_repack_iostore();
}

#[tauri::command]
pub(crate) async fn list_utoc_contents(utoc_path: String) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || pak::list_utoc_contents(&utoc_path))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn extract_utoc(
    utoc_path: String,
    output_dir: String,
) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || pak::extract_utoc(&utoc_path, &output_dir))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn extract_utoc_file(
    utoc_path: String,
    file_name: String,
    output_path: String,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        pak::extract_utoc_file(&utoc_path, &file_name, &output_path)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn count_utoc_legacy_packages(
    utoc_path: String,
    game_root: String,
    filter: Vec<String>,
) -> Result<usize, String> {
    tauri::async_runtime::spawn_blocking(move || {
        pak::count_utoc_legacy_packages(&utoc_path, &game_root, &filter)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn extract_utoc_legacy(
    utoc_path: String,
    game_root: String,
    output_dir: String,
    filter: Vec<String>,
    app: AppHandle,
) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        pak::extract_utoc_legacy(&utoc_path, &game_root, &output_dir, &filter, app)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) fn cancel_legacy_extraction() {
    pak::cancel_legacy_extraction();
}
