//! Persistent app settings stored under the user config dir, with shared accessors used by all command modules.

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::detect::InstallInfo;
use crate::tweaks::TweakSetting;

const FILE_NAME: &str = "settings.json";

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct TweakProfile {
    pub name: String,
    pub settings: Vec<TweakSetting>,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub modified_at: u64,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct ModProfile {
    pub name: String,
    /// Display names of mods that should be enabled (e.g. "MyMod.pak").
    pub enabled_mods: Vec<String>,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub modified_at: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum CompressionLevelSetting {
    None,
    Fast,
    Normal,
    Optimal1,
    Optimal2,
    Optimal3,
}

impl CompressionLevelSetting {
    pub(crate) fn to_oodle(self) -> retoc::OodleCompressionLevel {
        use retoc::OodleCompressionLevel as L;
        match self {
            Self::None => L::None,
            Self::Fast => L::Fast,
            Self::Normal => L::Normal,
            Self::Optimal1 => L::Optimal1,
            Self::Optimal2 => L::Optimal2,
            Self::Optimal3 => L::Optimal3,
        }
    }
}

fn default_mod_compression_level() -> CompressionLevelSetting {
    CompressionLevelSetting::Normal
}

fn default_vanilla_compression_level() -> CompressionLevelSetting {
    CompressionLevelSetting::Optimal1
}

fn settings_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("rivals-toolkit").join(FILE_NAME))
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Settings {
    #[serde(default = "default_true")]
    pub(crate) auto_check_updates: bool,
    #[serde(default = "default_true")]
    pub(crate) recursive_mod_scan: bool,
    #[serde(default = "default_true")]
    pub(crate) auto_sync_character_data: bool,
    #[serde(default)]
    pub(crate) show_hero_icons: bool,
    #[serde(default)]
    pub(crate) game_path: Option<String>,
    #[serde(default)]
    pub(crate) install_info: Option<InstallInfo>,
    #[serde(default)]
    pub(crate) mod_profiles: Vec<ModProfile>,
    #[serde(default)]
    pub(crate) tweak_profiles: Vec<TweakProfile>,
    #[serde(default = "default_mod_compression_level")]
    pub(crate) mod_compression_level: CompressionLevelSetting,
    #[serde(default = "default_vanilla_compression_level")]
    pub(crate) vanilla_compression_level: CompressionLevelSetting,
}

fn default_true() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            auto_check_updates: true,
            recursive_mod_scan: true,
            auto_sync_character_data: true,
            show_hero_icons: false,
            game_path: None,
            install_info: None,
            mod_profiles: Vec::new(),
            tweak_profiles: Vec::new(),
            mod_compression_level: default_mod_compression_level(),
            vanilla_compression_level: default_vanilla_compression_level(),
        }
    }
}

impl Settings {
    pub(crate) fn load() -> Self {
        let Some(path) = settings_path() else {
            eprintln!("rivals-toolkit: no config dir available, using default settings");
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(s) => match serde_json::from_str::<Settings>(&s) {
                Ok(settings) => settings,
                Err(e) => {
                    eprintln!(
                        "rivals-toolkit: failed to parse {}: {e}. Using defaults.",
                        path.display()
                    );
                    Self::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                eprintln!(
                    "rivals-toolkit: failed to read {}: {e}. Using defaults.",
                    path.display()
                );
                Self::default()
            }
        }
    }

    pub(crate) fn save(&self) -> Result<(), String> {
        let path =
            settings_path().ok_or_else(|| "Could not resolve config directory".to_string())?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
    }
}

pub(crate) type SettingsState = Mutex<Settings>;

/// Read the recursive-mod-scan flag from settings, defaulting to true on lock failure.
pub(crate) fn recursive_mod_scan(state: &State<'_, SettingsState>) -> bool {
    state.lock().map(|s| s.recursive_mod_scan).unwrap_or(true)
}

#[tauri::command]
pub(crate) fn get_recursive_mod_scan(state: State<'_, SettingsState>) -> bool {
    recursive_mod_scan(&state)
}

#[tauri::command]
pub(crate) fn set_recursive_mod_scan(
    state: State<'_, SettingsState>,
    enabled: bool,
) -> Result<(), String> {
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    guard.recursive_mod_scan = enabled;
    guard.save()
}

#[tauri::command]
pub(crate) fn get_show_hero_icons(state: State<'_, SettingsState>) -> bool {
    state.lock().map(|s| s.show_hero_icons).unwrap_or(false)
}

#[tauri::command]
pub(crate) fn set_show_hero_icons(
    state: State<'_, SettingsState>,
    enabled: bool,
) -> Result<(), String> {
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    guard.show_hero_icons = enabled;
    guard.save()
}

#[tauri::command]
pub(crate) fn get_game_path(state: State<'_, SettingsState>) -> Option<String> {
    state.lock().ok().and_then(|s| s.game_path.clone())
}

#[tauri::command]
pub(crate) fn get_saved_install_info(state: State<'_, SettingsState>) -> Option<InstallInfo> {
    state.lock().ok().and_then(|s| s.install_info.clone())
}

#[tauri::command]
pub(crate) fn set_game_path(
    state: State<'_, SettingsState>,
    path: Option<String>,
    install_info: Option<InstallInfo>,
) -> Result<(), String> {
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    guard.game_path = path.filter(|p| !p.is_empty());
    guard.install_info = install_info;
    guard.save()
}

pub(crate) fn mod_compression_level(state: &State<'_, SettingsState>) -> CompressionLevelSetting {
    state
        .lock()
        .map(|s| s.mod_compression_level)
        .unwrap_or(CompressionLevelSetting::Normal)
}

pub(crate) fn vanilla_compression_level(
    state: &State<'_, SettingsState>,
) -> CompressionLevelSetting {
    state
        .lock()
        .map(|s| s.vanilla_compression_level)
        .unwrap_or(CompressionLevelSetting::Optimal1)
}

#[tauri::command]
pub(crate) fn get_mod_compression_level(
    state: State<'_, SettingsState>,
) -> CompressionLevelSetting {
    mod_compression_level(&state)
}

#[tauri::command]
pub(crate) fn set_mod_compression_level(
    state: State<'_, SettingsState>,
    level: CompressionLevelSetting,
) -> Result<(), String> {
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    guard.mod_compression_level = level;
    guard.save()
}

#[tauri::command]
pub(crate) fn get_vanilla_compression_level(
    state: State<'_, SettingsState>,
) -> CompressionLevelSetting {
    vanilla_compression_level(&state)
}

#[tauri::command]
pub(crate) fn set_vanilla_compression_level(
    state: State<'_, SettingsState>,
    level: CompressionLevelSetting,
) -> Result<(), String> {
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    guard.vanilla_compression_level = level;
    guard.save()
}
