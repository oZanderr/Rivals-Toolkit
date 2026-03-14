use std::fs;

use serde::{Deserialize, Serialize};

use crate::paths::{binaries_dir, mods_dir};

use super::{BYPASS_ASI, BYPASS_DSOUND, file_matches};

#[derive(Serialize, Deserialize)]
pub(crate) struct ModEntry {
    /// Actual filename on disk (e.g. "mymod.pak" or "mymod.pak.disabled")
    pub full_name: String,
    /// Display name without the .disabled suffix (always ends with .pak)
    pub display_name: String,
    pub enabled: bool,
    /// Whether companion .ucas/.utoc files also exist for this mod
    pub has_companions: bool,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct ModsStatus {
    pub mods_folder_exists: bool,
    pub mods_folder_path: String,
    pub sig_bypass_installed: bool,
    pub sig_bypass_up_to_date: bool,
    pub mod_entries: Vec<ModEntry>,
}

pub(crate) fn get_mods_status(game_root: &str) -> ModsStatus {
    let mods = mods_dir(game_root);
    let exists = mods.exists();

    let bin_dir = binaries_dir(game_root);
    let dsound_path = bin_dir.join("dsound.dll");
    let asi_path = bin_dir.join("plugins\\MarvelRivalsUTOCSignatureBypass.asi");

    let sig_bypass_installed = dsound_path.exists();
    let sig_bypass_up_to_date =
        file_matches(&dsound_path, BYPASS_DSOUND) && file_matches(&asi_path, BYPASS_ASI);

    let mut mod_entries: Vec<ModEntry> = if exists {
        fs::read_dir(&mods)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let full_name = e.file_name().to_string_lossy().into_owned();
                if full_name.ends_with(".pak") {
                    let stem = &full_name[..full_name.len() - 4];
                    let has_companions = mods.join(format!("{stem}.ucas")).exists();
                    Some(ModEntry {
                        display_name: full_name.clone(),
                        full_name,
                        enabled: true,
                        has_companions,
                    })
                } else if full_name.ends_with(".pak.disabled") {
                    let display_name = full_name[..full_name.len() - ".disabled".len()].to_owned();
                    let stem = &display_name[..display_name.len() - 4];
                    let has_companions = mods.join(format!("{stem}.ucas.disabled")).exists();
                    Some(ModEntry {
                        display_name,
                        full_name,
                        enabled: false,
                        has_companions,
                    })
                } else {
                    None
                }
            })
            .collect()
    } else {
        Vec::new()
    };

    mod_entries.sort_by(|a, b| {
        a.display_name
            .to_lowercase()
            .cmp(&b.display_name.to_lowercase())
    });

    ModsStatus {
        mods_folder_exists: exists,
        mods_folder_path: mods.to_string_lossy().into_owned(),
        sig_bypass_installed,
        sig_bypass_up_to_date,
        mod_entries,
    }
}
