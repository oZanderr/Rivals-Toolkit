//! Persistent app settings stored under the user config dir, with shared accessors used by all command modules.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::detect::InstallInfo;
use crate::mods::heroes::HeroMatch;
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

/// Cached hero detection result for a mod, keyed by display name.
/// Invalidated when the mod's total size changes or the character catalogue
/// stamp moves past the value captured at scan time.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct ModHeroCacheEntry {
    pub size_bytes: u64,
    /// `Settings::last_character_data_sync` value when this entry was computed.
    /// Stale entries (older catalogue) recompute against the current catalogue.
    #[serde(default)]
    pub catalogue_stamp: u64,
    pub heroes: Vec<HeroMatch>,
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
    #[serde(default = "default_true")]
    pub(crate) show_hero_icons: bool,
    #[serde(default)]
    pub(crate) last_character_data_sync: u64,
    #[serde(default)]
    pub(crate) game_path: Option<String>,
    #[serde(default)]
    pub(crate) install_info: Option<InstallInfo>,
    #[serde(default)]
    pub(crate) mod_profiles: Vec<ModProfile>,
    #[serde(default)]
    pub(crate) tweak_profiles: Vec<TweakProfile>,
    #[serde(default)]
    pub(crate) mod_hero_cache: HashMap<String, ModHeroCacheEntry>,
    #[serde(default)]
    pub(crate) mod_hero_cache_version: u32,
}

/// Current hero detector version. Bump when matching logic changes meaningfully
/// or the cache entry shape changes so stale entries get discarded on load.
pub(crate) const MOD_HERO_CACHE_VERSION: u32 = 4;

fn default_true() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            auto_check_updates: true,
            recursive_mod_scan: true,
            auto_sync_character_data: true,
            show_hero_icons: true,
            last_character_data_sync: 0,
            game_path: None,
            install_info: None,
            mod_profiles: Vec::new(),
            tweak_profiles: Vec::new(),
            mod_hero_cache: HashMap::new(),
            mod_hero_cache_version: MOD_HERO_CACHE_VERSION,
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
                Ok(mut settings) => {
                    if settings.mod_hero_cache_version != MOD_HERO_CACHE_VERSION {
                        settings.mod_hero_cache.clear();
                        settings.mod_hero_cache_version = MOD_HERO_CACHE_VERSION;
                    }
                    settings
                }
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
    state.lock().map(|s| s.show_hero_icons).unwrap_or(true)
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
