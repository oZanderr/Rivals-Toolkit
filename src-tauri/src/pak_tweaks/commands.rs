//! Tauri commands for pak INI inspection, tweak detection, and editing.

use std::io::Write;
use std::path::PathBuf;

use tauri::State;

use crate::pak_tweaks;
use crate::pak_tweaks::{
    PakIniFileContent, PakIniInfo, PakIniListing, PakIniListingScan, PakIniScan,
};
use crate::settings::{SettingsState, recursive_mod_scan};
use crate::tweaks::{TweakDefinition, TweakSetting, TweakState};

/// Where INI text too large to hand over in one piece is assembled.
fn staging_dir() -> PathBuf {
    std::env::temp_dir().join("rivals-toolkit-ini-staging")
}

/// Appends one piece of an INI to a staging file and answers where it is.
///
/// A save crosses the IPC boundary as a single JSON string, and this editor opens config files
/// that run to hundreds of megabytes each; several of them at once exceeds what the webview can
/// represent as one string, which surfaced as `RangeError: Invalid string length`. Sending the
/// text in pieces keeps every message small and means the whole file never exists in the
/// webview either.
///
/// Pass `path` back from the previous call to continue a file, or `None` to start one.
#[tauri::command]
pub(crate) async fn stage_pak_ini_chunk(
    path: Option<String>,
    chunk: String,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let dir = staging_dir();
        let target = match path {
            Some(existing) => {
                // The caller names the file to append to, so it has to be one of ours.
                let candidate = PathBuf::from(&existing);
                if candidate.parent() != Some(dir.as_path()) {
                    return Err(format!("{existing} is not a staging file"));
                }
                candidate
            }
            None => {
                std::fs::create_dir_all(&dir)
                    .map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
                let stamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|e| e.to_string())?
                    .as_nanos();
                dir.join(format!("{stamp}-{}.ini", std::process::id()))
            }
        };
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&target)
            .map_err(|e| format!("Could not open {}: {e}", target.display()))?;
        file.write_all(chunk.as_bytes())
            .map_err(|e| format!("Could not write {}: {e}", target.display()))?;
        Ok(target.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Removes anything a previous session left in the staging directory. A save consumes the files
/// it uses, so whatever is left is from a run that did not finish.
pub(crate) fn clear_ini_staging() {
    let dir = staging_dir();
    if !dir.is_dir() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.path().extension().is_some_and(|e| e == "ini") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[tauri::command]
pub(crate) fn get_tweak_definitions() -> Vec<TweakDefinition> {
    crate::tweaks::catalogue::tweak_catalogue()
}

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
) -> Result<PakIniScan, String> {
    let recursive = recursive_mod_scan(&state);
    tauri::async_runtime::spawn_blocking(move || pak_tweaks::scan_mod_paks(&game_root, recursive))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn inspect_pak_path_any_ini(
    pak_path: String,
) -> Result<Option<PakIniListing>, String> {
    tauri::async_runtime::spawn_blocking(move || pak_tweaks::inspect_single_pak_any_ini(&pak_path))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn scan_mod_paks_any_ini(
    state: State<'_, SettingsState>,
    game_root: String,
) -> Result<PakIniListingScan, String> {
    let recursive = recursive_mod_scan(&state);
    tauri::async_runtime::spawn_blocking(move || {
        pak_tweaks::scan_mod_paks_any_ini(&game_root, recursive)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn detect_pak_tweaks(pak_path: String) -> Result<Vec<TweakState>, String> {
    tauri::async_runtime::spawn_blocking(move || pak_tweaks::detect_pak_tweaks(&pak_path))
        .await
        .map_err(|e| e.to_string())?
}

/// Put the named tweaks into the requested state.
///
/// The id-to-edit translation lives in `rivals_core`, so this and the CLI cannot disagree about
/// what a tweak writes.
#[tauri::command]
pub(crate) async fn apply_pak_tweak_settings(
    pak_path: String,
    settings: Vec<TweakSetting>,
) -> Result<String, String> {
    if crate::game_status::should_block_for_game() {
        return Err(crate::game_status::game_running_error());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let edits = pak_tweaks::edits_for_settings(&settings)?;
        pak_tweaks::apply_pak_tweaks(&pak_path, &edits)
    })
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
pub(crate) async fn extract_game_default_ini(
    game_root: String,
    in_pak_path: String,
) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        pak_tweaks::extract_game_default_ini(&game_root, &in_pak_path)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn create_new_mod_pak(
    game_root: String,
    name: String,
) -> Result<PakIniListing, String> {
    if crate::game_status::should_block_for_game() {
        return Err(crate::game_status::game_running_error());
    }
    tauri::async_runtime::spawn_blocking(move || pak_tweaks::create_new_mod_pak(&game_root, &name))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn save_pak_ini(
    pak_path: String,
    files: Vec<PakIniFileContent>,
    deletes: Vec<String>,
) -> Result<String, String> {
    if crate::game_status::should_block_for_game() {
        return Err(crate::game_status::game_running_error());
    }
    tauri::async_runtime::spawn_blocking(move || {
        pak_tweaks::save_pak_ini(&pak_path, files, deletes)
    })
    .await
    .map_err(|e| e.to_string())?
}
