mod engine;
pub(crate) mod tweaks;

use std::{fs, path::Path};

pub(crate) use tweaks::{TweakDefinition, TweakSetting, TweakState};

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
    if let Some(parent) = Path::new(path).parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::write(path, content).map_err(|e| e.to_string())
}

/// Return the full tweak catalogue so the frontend can render controls.
pub(crate) fn get_tweak_definitions() -> Vec<TweakDefinition> {
    tweaks::tweak_catalogue()
}

/// Scan INI content and report which tweaks are currently active.
pub(crate) fn detect_tweaks(content: &str) -> Vec<TweakState> {
    let catalogue = tweaks::tweak_catalogue();
    engine::detect_active_tweaks(content, &catalogue)
}

/// Apply tweaks to INI content based on user settings.
/// Returns the modified INI text.
pub(crate) fn apply_tweaks(content: &str, settings: &[TweakSetting]) -> String {
    let catalogue = tweaks::tweak_catalogue();
    engine::apply_tweaks(content, &catalogue, settings)
}
