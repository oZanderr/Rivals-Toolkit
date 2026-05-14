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
        #[cfg(windows)]
        let mut command = {
            let mut c = std::process::Command::new("cmd");
            c.args(["/c", "start", "", &self.launch_url]);
            c
        };
        #[cfg(not(windows))]
        let mut command = {
            let mut c = std::process::Command::new("xdg-open");
            c.arg(&self.launch_url);
            c
        };
        command
            .spawn()
            .map_err(|e| format!("Failed to launch game: {e}"))?;
        Ok(())
    }
}

pub(crate) fn detect_game_install() -> Option<InstallInfo> {
    let steam = find_steam_install()
        .map(|p| InstallInfo::new(p, InstallSource::Steam, "steam://rungameid/2767030"));

    #[cfg(windows)]
    {
        steam
            .or_else(|| {
                find_epic_install().map(|(p, url)| InstallInfo::new(p, InstallSource::Epic, url))
            })
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
    #[cfg(not(windows))]
    {
        steam
    }
}

#[tauri::command]
pub(crate) fn detect_install_path() -> Option<InstallInfo> {
    detect_game_install()
}

#[tauri::command]
pub(crate) fn launch_game(install_info: InstallInfo) -> Result<(), String> {
    install_info.launch_game()
}
