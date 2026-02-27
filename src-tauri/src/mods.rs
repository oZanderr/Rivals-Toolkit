mod bypass;
mod folder;
mod paths;
mod status;

pub(crate) use status::ModsStatus;

pub(crate) fn get_mods_status(game_root: &str) -> ModsStatus {
    status::get_mods_status(game_root)
}

pub(crate) fn install_signature_bypass(game_root: &str) -> Result<String, String> {
    bypass::install_signature_bypass(game_root)
}

pub(crate) fn open_mods_folder(game_root: &str) -> Result<(), String> {
    folder::open_mods_folder(game_root)
}
