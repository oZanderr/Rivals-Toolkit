//! Pak INI editor: inspect, detect, and edit BaseEngine.ini / DefaultEngine.ini / WindowsEngine.ini / BaseDeviceProfiles.ini / DefaultDeviceProfiles.ini embedded in mod paks.

mod apply;
mod cvars;
mod edits;
pub(crate) mod io;
pub(crate) mod scan;

use serde::{Deserialize, Serialize};

pub use apply::{apply_pak_tweaks, save_pak_ini};
pub use edits::{edits_for_settings, edits_for_tweak, needs_engine_ini};
pub use scan::{
    create_new_mod_pak, detect_pak_tweaks, extract_game_default_ini, extract_pak_ini,
    inspect_single_pak, inspect_single_pak_any_ini, read_pak_cvars, scan_mod_paks,
    scan_mod_paks_any_ini,
};

/// INI entries discovered in a pak mod for the curated tweak workflow (Config Tweaks).
///
/// Runtime priority for shared keys (highest wins): DefaultDeviceProfiles >
/// BaseDeviceProfiles > WindowsEngine > DefaultEngine > BaseEngine.
///
/// Each layer holds every matching file, not just one: a pak can ship both
/// `Engine/Config/Windows/BaseWindowsEngine.ini` and `Marvel/Config/Windows/WindowsEngine.ini`,
/// and a key left behind in either one still applies at runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PakIniInfo {
    pub pak_name: String,
    pub pak_path: String,
    pub has_device_profiles: bool,
    pub has_base_device_profiles: bool,
    pub has_engine_ini: bool,
    pub has_base_engine: bool,
    pub has_windows_engine: bool,
    pub device_profiles_entries: Vec<String>,
    pub base_device_profiles_entries: Vec<String>,
    pub engine_ini_entries: Vec<String>,
    pub base_engine_entries: Vec<String>,
    pub windows_engine_entries: Vec<String>,
}

impl PakIniInfo {
    /// Every INI file in the pak, lowest runtime priority first.
    pub(crate) fn layers(&self) -> Vec<(PakIniTarget, &str)> {
        PakIniTarget::ALL
            .iter()
            .flat_map(|&target| {
                self.entries_for(target)
                    .iter()
                    .map(move |entry| (target, entry.as_str()))
            })
            .collect()
    }

    pub(crate) fn entries_for(&self, target: PakIniTarget) -> &[String] {
        match target {
            PakIniTarget::BaseEngine => &self.base_engine_entries,
            PakIniTarget::Engine => &self.engine_ini_entries,
            PakIniTarget::WindowsEngine => &self.windows_engine_entries,
            PakIniTarget::BaseDeviceProfiles => &self.base_device_profiles_entries,
            PakIniTarget::DeviceProfiles => &self.device_profiles_entries,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        PakIniTarget::ALL
            .iter()
            .all(|&target| self.entries_for(target).is_empty())
    }
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

/// A config layer a pak INI file belongs to, ordered by runtime priority.
///
/// `Engine` is DefaultEngine.ini; `DeviceProfiles` is DefaultDeviceProfiles.ini. The order
/// decides which value the game reads when two files disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PakIniTarget {
    BaseEngine,
    Engine,
    WindowsEngine,
    BaseDeviceProfiles,
    DeviceProfiles,
}

impl PakIniTarget {
    /// Lowest runtime priority first, matching UE's config load order.
    pub(crate) const ALL: [PakIniTarget; 5] = [
        PakIniTarget::BaseEngine,
        PakIniTarget::Engine,
        PakIniTarget::WindowsEngine,
        PakIniTarget::BaseDeviceProfiles,
        PakIniTarget::DeviceProfiles,
    ];

    pub(crate) fn is_engine(self) -> bool {
        matches!(
            self,
            PakIniTarget::BaseEngine | PakIniTarget::Engine | PakIniTarget::WindowsEngine
        )
    }

    /// `parse_console_vars` switches parsing rules on whether the source name contains
    /// "DeviceProfiles", so the label must reflect the file kind.
    pub(crate) fn source_label(self) -> &'static str {
        match self {
            PakIniTarget::BaseEngine => "BaseEngine.ini",
            PakIniTarget::Engine => "DefaultEngine.ini",
            PakIniTarget::WindowsEngine => "WindowsEngine.ini",
            PakIniTarget::BaseDeviceProfiles => "BaseDeviceProfiles.ini",
            PakIniTarget::DeviceProfiles => "DefaultDeviceProfiles.ini",
        }
    }
}

/// Raw INI file content for writing back to a pak.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PakIniFileContent {
    pub entry: String,
    #[serde(default)]
    pub content: String,
    /// Where the text is instead, when it was too large to hand over in one piece. A save
    /// crosses the app's IPC boundary as a single JSON string, so a few hundred megabytes of
    /// config across several files cannot travel inline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staged_path: Option<String>,
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn info(windows_engine_entries: Vec<String>) -> PakIniInfo {
        PakIniInfo {
            pak_name: "config_9999999_P.pak".into(),
            pak_path: "config_9999999_P.pak".into(),
            has_device_profiles: true,
            has_base_device_profiles: true,
            has_engine_ini: true,
            has_base_engine: true,
            has_windows_engine: !windows_engine_entries.is_empty(),
            device_profiles_entries: vec!["Marvel/Config/DefaultDeviceProfiles.ini".into()],
            base_device_profiles_entries: vec!["Engine/Config/BaseDeviceProfiles.ini".into()],
            engine_ini_entries: vec!["Marvel/Config/DefaultEngine.ini".into()],
            base_engine_entries: vec!["Engine/Config/BaseEngine.ini".into()],
            windows_engine_entries,
        }
    }

    /// The order is the contract: the merged read applies it last-wins, so a layer in the wrong
    /// slot reports a value the game never uses.
    #[test]
    fn layers_run_from_lowest_runtime_priority_to_highest() {
        let info = info(vec![
            "Engine/Config/Windows/BaseWindowsEngine.ini".into(),
            "Marvel/Config/Windows/WindowsEngine.ini".into(),
        ]);
        assert_eq!(
            info.layers(),
            vec![
                (PakIniTarget::BaseEngine, "Engine/Config/BaseEngine.ini"),
                (PakIniTarget::Engine, "Marvel/Config/DefaultEngine.ini"),
                (
                    PakIniTarget::WindowsEngine,
                    "Engine/Config/Windows/BaseWindowsEngine.ini"
                ),
                (
                    PakIniTarget::WindowsEngine,
                    "Marvel/Config/Windows/WindowsEngine.ini"
                ),
                (
                    PakIniTarget::BaseDeviceProfiles,
                    "Engine/Config/BaseDeviceProfiles.ini"
                ),
                (
                    PakIniTarget::DeviceProfiles,
                    "Marvel/Config/DefaultDeviceProfiles.ini"
                ),
            ]
        );
    }

    #[test]
    fn a_missing_layer_drops_out_of_the_walk() {
        let info = info(Vec::new());
        assert_eq!(info.layers().len(), 4);
        assert!(!info.is_empty());
    }
}
