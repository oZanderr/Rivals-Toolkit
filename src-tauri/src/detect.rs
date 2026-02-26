mod epic;
mod registry;
mod steam;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use epic::find_epic_install;
use steam::find_steam_install;

#[derive(Serialize, Deserialize)]
pub(crate) enum InstallSource {
    Steam,
    Epic,
    LoadingBay,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct InstallInfo {
    pub path: String,
    pub source: InstallSource,
}

impl InstallInfo {
    fn new(path: PathBuf, source: InstallSource) -> Self {
        Self {
            path: path.to_string_lossy().into_owned(),
            source,
        }
    }
}

pub(crate) fn detect_game_install() -> Option<InstallInfo> {
    None.or_else(|| find_steam_install().map(|p| InstallInfo::new(p, InstallSource::Steam)))
        .or_else(|| find_epic_install().map(|p| InstallInfo::new(p, InstallSource::Epic)))
}
