//! Tauri command wrappers for Scalability.ini operations and tweak detection/application.

use crate::scalability;
use crate::scalability::{TweakDefinition, TweakSetting, TweakState};

#[tauri::command]
pub(crate) fn get_scalability_path() -> Result<String, String> {
    scalability::get_scalability_path()
}

#[tauri::command]
pub(crate) fn read_scalability(path: String) -> Result<String, String> {
    scalability::read_scalability(&path)
}

#[tauri::command]
pub(crate) fn write_scalability(path: String, content: String) -> Result<(), String> {
    scalability::write_scalability(&path, &content)
}

#[tauri::command]
pub(crate) fn get_tweak_definitions() -> Vec<TweakDefinition> {
    scalability::get_tweak_definitions()
}

#[tauri::command]
pub(crate) fn detect_tweaks(content: String) -> Vec<TweakState> {
    scalability::detect_tweaks(&content)
}

#[tauri::command]
pub(crate) fn apply_tweaks(content: String, settings: Vec<TweakSetting>) -> String {
    scalability::apply_tweaks(&content, &settings)
}
