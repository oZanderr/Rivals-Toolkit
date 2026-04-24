//! Scalability.ini read/write plus thin wrappers over the shared tweak engine for the Config Tweaks tab.

mod apply;
pub(crate) mod commands;

use std::{fs, path::Path};

use crate::tweaks::{self, TweakDefinition, TweakSetting, TweakState};

const CONFIG_PATH: &str = "Marvel\\Saved\\Config\\Windows\\Scalability.ini";

pub(crate) fn get_scalability_path() -> Result<String, String> {
    dirs::data_local_dir()
        .map(|base| base.join(CONFIG_PATH))
        .map(|p| p.to_string_lossy().into_owned())
        .ok_or_else(|| "Could not determine AppData path.".to_string())
}

pub(crate) fn read_scalability(path: &str) -> Result<String, String> {
    fs::read_to_string(path).map_err(|e| e.to_string())
}

pub(crate) fn write_scalability(path: &str, content: &str) -> Result<(), String> {
    let p = Path::new(path);
    if let Ok(meta) = fs::metadata(p)
        && meta.permissions().readonly()
    {
        return Err(
            "Scalability.ini is read-only. Remove the read-only attribute and try again."
                .to_string(),
        );
    }
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::write(p, content).map_err(|e| e.to_string())
}

/// Return the full tweak catalogue.
pub(crate) fn get_tweak_definitions() -> Vec<TweakDefinition> {
    tweaks::catalogue::tweak_catalogue()
}

/// Detect which tweaks are active in Scalability.ini content.
pub(crate) fn detect_tweaks(content: &str) -> Vec<TweakState> {
    tweaks::detect_tweaks(content)
}

/// Apply tweak settings to Scalability.ini content.
pub(crate) fn apply_tweaks(content: &str, settings: &[TweakSetting]) -> String {
    let entries = tweaks::catalogue::tweak_catalogue();
    apply::apply_tweaks(content, &entries, settings)
}
