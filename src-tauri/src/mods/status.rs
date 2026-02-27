use std::fs;

use serde::{Deserialize, Serialize};

use super::paths::{binaries_dir, mods_dir};

// Reference copies for status checks — same bytes as bypass.rs.
static BYPASS_DSOUND: &[u8] = include_bytes!("../../resources/bypass/dsound.dll");
static BYPASS_ASI: &[u8] =
    include_bytes!("../../resources/bypass/plugins/MarvelRivalsUTOCSignatureBypass.asi");

#[derive(Serialize, Deserialize)]
pub(crate) struct ModsStatus {
    pub mods_folder_exists: bool,
    pub mods_folder_path: String,
    pub sig_bypass_installed: bool,
    pub sig_bypass_up_to_date: bool,
    pub mod_paks: Vec<String>,
}

/// Returns `true` if the file at `path` exists and its contents
/// are byte-identical to `expected`.
fn file_matches(path: &std::path::Path, expected: &[u8]) -> bool {
    fs::read(path)
        .map(|data| data == expected)
        .unwrap_or(false)
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
        sig_bypass_up_to_date,
        mod_paks,
    }
}
