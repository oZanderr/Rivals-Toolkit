use crate::detect::{self, InstallInfo};

#[tauri::command]
pub(crate) fn detect_install_path() -> Option<InstallInfo> {
    detect::detect_game_install()
}
