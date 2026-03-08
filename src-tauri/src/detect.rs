mod epic;
mod loading_bay;
mod registry;
mod steam;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use epic::find_epic_install;
use loading_bay::find_loading_bay_install;
use steam::find_steam_install;

#[derive(Serialize, Deserialize)]
pub(crate) enum InstallSource {
    Steam,
    Epic,
    LoadingBay,
}

#[derive(Serialize, Deserialize)]
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
        std::process::Command::new("cmd")
            .args(["/c", "start", "", &self.launch_url])
            .spawn()
            .map_err(|e| format!("Failed to launch game: {e}"))?;
        Ok(())
    }
}

pub(crate) fn detect_game_install() -> Option<InstallInfo> {
    find_steam_install().map(|p| InstallInfo::new(p, InstallSource::Steam, "steam://rungameid/2767030"))
        .or_else(|| find_epic_install().map(|(p, url)| InstallInfo::new(p, InstallSource::Epic, url)))
        .or_else(|| find_loading_bay_install().map(|p| InstallInfo::new(p, InstallSource::LoadingBay, "loadingbay://mygame/?gameId=31")))
}
