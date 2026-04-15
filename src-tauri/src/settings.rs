use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::detect::InstallInfo;

const FILE_NAME: &str = "settings.json";

fn settings_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("rivals-toolkit").join(FILE_NAME))
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Settings {
    #[serde(default = "default_true")]
    pub(crate) auto_check_updates: bool,
    #[serde(default = "default_true")]
    pub(crate) recursive_mod_scan: bool,
    #[serde(default)]
    pub(crate) game_path: Option<String>,
    #[serde(default)]
    pub(crate) install_info: Option<InstallInfo>,
}

fn default_true() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            auto_check_updates: true,
            recursive_mod_scan: true,
            game_path: None,
            install_info: None,
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
