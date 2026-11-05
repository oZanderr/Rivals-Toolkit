//! Marvel Rivals install detection across Steam, Epic Games, and Loading Bay launchers.

#[cfg(windows)]
mod epic;
#[cfg(windows)]
mod loading_bay;
#[cfg(windows)]
mod registry;
mod steam;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[cfg(windows)]
use epic::find_epic_install;
#[cfg(windows)]
use loading_bay::find_loading_bay_install;
use steam::find_steam_install;

use crate::paths::STEAM_APP_ID;

#[derive(Clone, Serialize, Deserialize)]
pub(crate) enum InstallSource {
    Steam,
    Epic,
    LoadingBay,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct InstallInfo {
    pub(crate) path: String,
    pub(crate) source: InstallSource,
    pub(crate) launch_url: String,
}

impl InstallInfo {
    fn new(path: PathBuf, source: InstallSource, launch_url: impl Into<String>) -> Self {
        Self {
            path: path.to_string_lossy().into_owned(),
            source,
            launch_url: launch_url.into(),
        }
    }

    pub(crate) fn launch_game(&self) -> Result<(), String> {
        tauri_plugin_opener::open_url(&self.launch_url, None::<&str>)
            .map_err(|e| format!("Failed to launch game: {e}"))
    }
}

pub(crate) fn detect_game_install() -> Option<InstallInfo> {
    find_steam_install()
        .map(|p| {
            InstallInfo::new(
                p,
                InstallSource::Steam,
                format!("steam://rungameid/{STEAM_APP_ID}"),
            )
        })
        .or_else(detect_other_launchers)
}

#[cfg(windows)]
fn detect_other_launchers() -> Option<InstallInfo> {
    find_epic_install()
        .map(|(p, url)| InstallInfo::new(p, InstallSource::Epic, url))
        .or_else(|| {
            find_loading_bay_install().map(|p| {
                InstallInfo::new(
                    p,
                    InstallSource::LoadingBay,
                    "loadingbay://mygame/?gameId=31",
                )
            })
        })
}

/// Epic and Loading Bay are found through the registry, so elsewhere only Steam is detected.
#[cfg(not(windows))]
fn detect_other_launchers() -> Option<InstallInfo> {
    None
}

#[tauri::command]
pub(crate) fn detect_install_path() -> Option<InstallInfo> {
    detect_game_install()
}

#[tauri::command]
pub(crate) fn launch_game(install_info: InstallInfo) -> Result<(), String> {
    install_info.launch_game()
}
