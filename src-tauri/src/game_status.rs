//! Tauri wrappers over the running-game probe in `rivals_core::game_status`.

pub(crate) use rivals_core::game_status::*;

#[tauri::command]
pub(crate) fn get_game_running() -> bool {
    is_game_running()
}

#[tauri::command]
pub(crate) fn get_should_block_for_game() -> bool {
    should_block_for_game()
}
