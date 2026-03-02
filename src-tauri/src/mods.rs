mod bypass;
mod folder;
mod status;

pub(crate) use status::ModsStatus;

// Bypass files bundled at compile time.
static BYPASS_DSOUND: &[u8] = include_bytes!("../resources/bypass/dsound.dll");
static BYPASS_ASI: &[u8] =
    include_bytes!("../resources/bypass/plugins/MarvelRivalsUTOCSignatureBypass.asi");

/// Returns `true` if the file at `path` exists and its contents
/// are byte-identical to `expected`.
fn file_matches(path: &std::path::Path, expected: &[u8]) -> bool {
    std::fs::read(path)
        .map(|data| data == expected)
        .unwrap_or(false)
}

pub(crate) fn get_mods_status(game_root: &str) -> ModsStatus {
    status::get_mods_status(game_root)
}

pub(crate) fn install_signature_bypass(game_root: &str) -> Result<String, String> {
    bypass::install_signature_bypass(game_root)
}

pub(crate) fn open_mods_folder(game_root: &str) -> Result<(), String> {
    folder::open_mods_folder(game_root)
}
