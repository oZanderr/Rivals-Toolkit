use std::path::PathBuf;

use serde::{Deserialize, Serialize};

fn prefs_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("rivals-toolkit").join("prefs.json"))
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Prefs {
    #[serde(default = "default_true")]
    pub auto_check_updates: bool,
}

fn default_true() -> bool {
    true
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            auto_check_updates: true,
        }
    }
}

pub(crate) fn load() -> Prefs {
    let Some(path) = prefs_path() else {
        return Prefs::default();
    };
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub(crate) fn save(prefs: &Prefs) -> Result<(), String> {
    let path = prefs_path().ok_or_else(|| "Could not resolve config directory".to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(prefs).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())
}

pub(crate) fn get_auto_check_updates() -> bool {
    load().auto_check_updates
}

pub(crate) fn set_auto_check_updates(enabled: bool) -> Result<(), String> {
    let mut prefs = load();
    prefs.auto_check_updates = enabled;
    save(&prefs)
}
