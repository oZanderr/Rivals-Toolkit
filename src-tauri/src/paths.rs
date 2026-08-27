//! Tauri wrappers over the path helpers in `rivals_core::paths`.

pub(crate) use rivals_core::paths::*;

#[tauri::command]
pub(crate) fn validate_game_path(path: String) -> Result<bool, String> {
    Ok(paks_dir(&path).is_dir())
}

#[tauri::command]
pub(crate) fn path_exists(path: String) -> bool {
    std::path::Path::new(&path).exists()
}
