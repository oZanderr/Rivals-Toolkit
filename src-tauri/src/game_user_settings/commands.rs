//! Tauri commands for GameUserSettings.ini operations and tweak detection/application.

use crate::game_status::{game_running_error, should_block_for_game};
use crate::game_user_settings;
use crate::tweaks::{TweakDefinition, TweakSetting, TweakState};

#[tauri::command]
pub(crate) fn get_game_user_settings_path() -> Result<String, String> {
    game_user_settings::get_game_user_settings_path()
}

#[tauri::command]
pub(crate) fn read_game_user_settings(path: String) -> Result<String, String> {
    game_user_settings::read_game_user_settings(&path)
}

#[tauri::command]
pub(crate) fn write_game_user_settings(path: String, content: String) -> Result<(), String> {
    if should_block_for_game() {
        return Err(game_running_error());
    }
    game_user_settings::write_game_user_settings(&path, &content)
}

#[tauri::command]
pub(crate) fn get_game_user_settings_definitions() -> Vec<TweakDefinition> {
    game_user_settings::get_tweak_definitions()
}

#[tauri::command]
pub(crate) fn detect_game_user_settings_tweaks(content: String) -> Vec<TweakState> {
    game_user_settings::detect_tweaks(&content)
}

#[tauri::command]
pub(crate) fn apply_game_user_settings_tweaks(
    content: String,
    settings: Vec<TweakSetting>,
) -> String {
    game_user_settings::apply_tweaks(&content, &settings)
}
