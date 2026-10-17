//! Tauri commands for loose INI files: the plugins folder's configs and any .ini opened by path.

use std::path::Path;

use rivals_core::ini_file::{self, IniText};

use crate::pak_tweaks::commands::staged_file;

#[tauri::command]
pub(crate) async fn list_plugin_inis(game_root: String) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        rivals_core::plugins::ini_files(&game_root)
            .iter()
            .map(|path| path.display().to_string())
            .collect()
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn read_ini_file(path: String) -> Result<IniText, String> {
    tauri::async_runtime::spawn_blocking(move || ini_file::read(Path::new(&path)))
        .await
        .map_err(|e| e.to_string())?
}

/// Writes `content`, or the staged file `staged_path` names for text too large to send in one
/// piece. Loose files are not held back while the game runs: a plugin config is read at launch,
/// and the frontend says so instead.
#[tauri::command]
pub(crate) async fn write_ini_file(
    path: String,
    content: String,
    staged_path: Option<String>,
    expected_modified: Option<u64>,
) -> Result<u64, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let staged = staged_path.as_deref().map(staged_file).transpose()?;
        let text = match &staged {
            Some(staged) => std::fs::read_to_string(staged)
                .map_err(|e| format!("Could not read {}: {e}", staged.display()))?,
            None => content,
        };
        let written = ini_file::write(Path::new(&path), &text, expected_modified);
        if let Some(staged) = staged {
            let _ = std::fs::remove_file(staged);
        }
        written
    })
    .await
    .map_err(|e| e.to_string())?
}
