use std::fs;

use serde::{Deserialize, Serialize};

use super::paths::{binaries_dir, mods_dir};

#[derive(Serialize, Deserialize)]
pub(crate) struct ModsStatus {
    pub mods_folder_exists: bool,
    pub mods_folder_path: String,
    pub sig_bypass_installed: bool,
    pub mod_paks: Vec<String>,
}

pub(crate) fn get_mods_status(game_root: &str) -> ModsStatus {
    let mods = mods_dir(game_root);
    let exists = mods.exists();
    let sig_bypass_installed = binaries_dir(game_root).join("dsound.dll").exists();

    let mod_paks = if exists {
        fs::read_dir(&mods)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("pak"))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect()
    } else {
        Vec::new()
    };

    ModsStatus {
        mods_folder_exists: exists,
        mods_folder_path: mods.to_string_lossy().into_owned(),
        sig_bypass_installed,
        mod_paks,
    }
}
