//! Pak INI editor: inspect, detect, and edit BaseEngine.ini / DefaultEngine.ini / WindowsEngine.ini / DefaultDeviceProfiles.ini embedded in mod paks.

mod apply;
pub(crate) mod commands;
mod cvars;
mod io;
mod scan;

use serde::{Deserialize, Serialize};

pub(crate) use apply::{apply_pak_tweaks, save_pak_ini};
pub(crate) use scan::{
    create_new_mod_pak, detect_pak_tweaks, extract_game_default_ini, extract_pak_ini,
    inspect_single_pak, inspect_single_pak_any_ini, scan_mod_paks, scan_mod_paks_any_ini,
};

/// INI entries discovered in a pak mod for the curated tweak workflow (Config Tweaks).
///
/// Runtime priority for shared keys (highest wins): DeviceProfiles > WindowsEngine >
/// DefaultEngine > BaseEngine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PakIniInfo {
    pub pak_name: String,
    pub pak_path: String,
    pub has_device_profiles: bool,
    pub has_engine_ini: bool,
    pub has_base_engine: bool,
    pub has_windows_engine: bool,
    pub device_profiles_entry: Option<String>,
    pub engine_ini_entry: Option<String>,
    pub base_engine_entry: Option<String>,
    pub windows_engine_entry: Option<String>,
}

/// Any-INI listing for paks shown in the Pak INI Editor tab.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PakIniListing {
    pub pak_name: String,
    pub pak_path: String,
    pub ini_entries: Vec<String>,
}

/// Parsed CVar state from pak INI files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PakTweakState {
    pub key: String,
    pub value: String,
    pub source: String,
}

/// Requested CVar edit for pak INI files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PakTweakEdit {
    pub key: String,
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_section: Option<String>,
}

/// Which embedded INI file a new CVar edit is written to. Resolved internally to the
/// highest-priority file present in the pak (DeviceProfiles > WindowsEngine > Engine >
/// BaseEngine); `Engine` is DefaultEngine.ini.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PakIniTarget {
    BaseEngine,
    Engine,
    WindowsEngine,
    DeviceProfiles,
}

/// Raw INI file content for writing back to a pak.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PakIniFileContent {
    pub entry: String,
    pub content: String,
}
