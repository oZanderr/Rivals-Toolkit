//! Pak INI editor: inspect, detect, and edit BaseEngine.ini / DefaultEngine.ini / WindowsEngine.ini / DefaultDeviceProfiles.ini embedded in mod paks.

mod apply;
mod cvars;
mod edits;
mod io;
mod scan;

use serde::{Deserialize, Serialize};

pub use apply::{apply_pak_tweaks, save_pak_ini};
pub use edits::{edits_for_settings, edits_for_tweak};
pub use scan::{
    create_new_mod_pak, detect_pak_tweaks, extract_game_default_ini, extract_pak_ini,
    inspect_single_pak, inspect_single_pak_any_ini, read_pak_cvars, scan_mod_paks,
    scan_mod_paks_any_ini,
};

/// INI entries discovered in a pak mod for the curated tweak workflow (Config Tweaks).
///
/// Runtime priority for shared keys (highest wins): DeviceProfiles > WindowsEngine >
/// DefaultEngine > BaseEngine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PakIniInfo {
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

/// A pak the scan could not read. Reported rather than skipped, so a mod that fails to open is
/// visibly broken instead of silently missing from the list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PakScanError {
    pub pak_name: String,
    pub pak_path: String,
    pub error: String,
}

/// Curated-tweak scan results plus whatever could not be read.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PakIniScan {
    pub paks: Vec<PakIniInfo>,
    pub unreadable: Vec<PakScanError>,
}

/// Any-INI scan results plus whatever could not be read.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PakIniListingScan {
    pub paks: Vec<PakIniListing>,
    pub unreadable: Vec<PakScanError>,
}

/// Any-INI listing for paks shown in the Pak INI Editor tab.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PakIniListing {
    pub pak_name: String,
    pub pak_path: String,
    pub ini_entries: Vec<String>,
}

/// One CVar assignment read out of a pak's INI files, tagged with the file it came from.
///
/// Not to be confused with `tweaks::TweakState`, which is a catalogue tweak's on/off state. This is
/// the raw key/value layer underneath that.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PakCvar {
    pub key: String,
    pub value: String,
    pub source: String,
}

/// Requested CVar edit for pak INI files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PakTweakEdit {
    pub key: String,
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_section: Option<String>,
}

/// Which embedded INI file a new CVar edit is written to. Resolved internally to the
/// highest-priority file present in the pak (DeviceProfiles > WindowsEngine > Engine >
/// BaseEngine); `Engine` is DefaultEngine.ini.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PakIniTarget {
    BaseEngine,
    Engine,
    WindowsEngine,
    DeviceProfiles,
}

/// Raw INI file content for writing back to a pak.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PakIniFileContent {
    pub entry: String,
    pub content: String,
}
