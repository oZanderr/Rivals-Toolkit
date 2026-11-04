//! Tauri commands over the wordings the toolkit keeps for mods: which mods have one, and writing a mod's again over the game's current text.

use rivals_core::text_mod::{self, TextReport};

/// The mods the toolkit keeps wordings for, by their names without case.
#[tauri::command]
pub(crate) fn text_edited_mods() -> Result<Vec<String>, String> {
    text_mod::recorded_mods()
}

/// Writes an installed mod's translations again over the game's current ones.
#[tauri::command]
pub(crate) async fn rebuild_mod_text(
    game_root: String,
    mod_pak: String,
) -> Result<TextReport, String> {
    if crate::game_status::should_block_for_game() {
        return Err(crate::game_status::game_running_error());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let files = rivals_core::mod_ops::ModFiles::of(std::path::Path::new(&mod_pak))?;
        text_mod::rebuild_text(&game_root, &files.part("pak"), false, false)
    })
    .await
    .map_err(|e| e.to_string())?
}
