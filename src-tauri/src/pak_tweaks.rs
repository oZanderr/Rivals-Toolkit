//! Pak INI editor: inspect, detect, and edit DefaultEngine.ini and DefaultDeviceProfiles.ini embedded in mod paks.

mod apply;
pub(crate) mod commands;
mod cvars;
mod io;
mod scan;

use serde::{Deserialize, Serialize};

pub(crate) use apply::{apply_pak_tweaks, save_pak_ini};
pub(crate) use scan::{detect_pak_tweaks, extract_pak_ini, inspect_single_pak, scan_mod_paks};

/// INI entries discovered in a pak mod.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PakIniInfo {
    pub pak_name: String,
    pub pak_path: String,
    pub has_device_profiles: bool,
    pub has_engine_ini: bool,
    pub device_profiles_entry: Option<String>,
    pub engine_ini_entry: Option<String>,
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

/// Raw INI file content for writing back to a pak.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PakIniFileContent {
    pub entry: String,
    pub content: String,
}
