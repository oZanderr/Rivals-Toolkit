//! Reads the desktop app's `settings.json` so the CLI defaults match what the GUI is configured to do.

use std::path::PathBuf;

use rivals_core::tweaks::TweakSetting;
use serde::Deserialize;

/// Only the fields the CLI honours. Unknown keys are ignored, so the app is free to add settings
/// without breaking a CLI built against an older schema.
#[derive(Debug, Default, Deserialize)]
pub struct AppSettings {
    #[serde(default)]
    pub game_path: Option<String>,
    #[serde(default = "yes")]
    pub recursive_mod_scan: bool,
    #[serde(default = "yes")]
    pub game_running_check_enabled: bool,
    #[serde(default)]
    pub usmap_path: Option<String>,
    /// A list of native object paths to name imports by, on top of the bundled one.
    #[serde(default)]
    pub extra_script_objects_path: Option<String>,
    /// The mod pak the desktop app last saved asset edits into.
    #[serde(default)]
    pub asset_mod_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_save_target: Option<rivals_core::asset_edit::SaveTarget>,
    /// Tweak presets saved from the desktop app, so `--preset` applies the same thing the GUI
    /// would.
    #[serde(default)]
    pub tweak_profiles: Vec<TweakProfile>,
}

/// One saved preset. The app stores timestamps alongside these; the CLI only needs what to write.
#[derive(Debug, Clone, Deserialize)]
pub struct TweakProfile {
    pub name: String,
    #[serde(default)]
    pub settings: Vec<TweakSetting>,
}

fn yes() -> bool {
    true
}

pub fn settings_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("rivals-toolkit").join("settings.json"))
}

/// Load the app's settings, falling back to defaults when the file is absent or unreadable. A
/// missing settings file is normal (the CLI can run on a machine that never opened the app), so it
/// is not an error here; commands that actually need a game root report that themselves.
pub fn load() -> AppSettings {
    let Some(path) = settings_path() else {
        return AppSettings {
            recursive_mod_scan: true,
            game_running_check_enabled: true,
            ..AppSettings::default()
        };
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or(AppSettings {
            recursive_mod_scan: true,
            game_running_check_enabled: true,
            ..AppSettings::default()
        })
}
