//! Reads the desktop app's `settings.json` so the CLI defaults match what the GUI is configured to do.

use std::path::PathBuf;

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
