//! Game-relative path helpers (paks, mods, binaries, launch record) plus generic existence checks.

use std::path::PathBuf;

pub(crate) fn paks_dir(game_root: &str) -> PathBuf {
    PathBuf::from(game_root).join("MarvelGame/Marvel/Content/Paks")
}

pub(crate) fn mods_dir(game_root: &str) -> PathBuf {
    paks_dir(game_root).join("~mods")
}

pub(crate) fn binaries_dir(game_root: &str) -> PathBuf {
    PathBuf::from(game_root).join("MarvelGame/Marvel/Binaries/Win64")
}

pub(crate) fn launch_record_path(game_root: &str) -> PathBuf {
    PathBuf::from(game_root).join("launch_record")
}

#[tauri::command]
pub(crate) fn validate_game_path(path: String) -> Result<bool, String> {
    Ok(paks_dir(&path).is_dir())
}

#[tauri::command]
pub(crate) fn path_exists(path: String) -> bool {
    std::path::Path::new(&path).exists()
}
