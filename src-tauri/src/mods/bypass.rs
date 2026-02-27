use std::fs;

use super::paths::{binaries_dir, mods_dir};

// Bypass files bundled at compile time.
static BYPASS_DSOUND: &[u8] = include_bytes!("../../resources/bypass/dsound.dll");
static BYPASS_ASI: &[u8] =
    include_bytes!("../../resources/bypass/plugins/MarvelRivalsUTOCSignatureBypass.asi");

/// Returns `true` if the file exists and its contents are byte-identical
fn file_matches(path: &std::path::Path, expected: &[u8]) -> bool {
    fs::read(path)
        .map(|data| data == expected)
        .unwrap_or(false)
}

pub(crate) fn install_signature_bypass(game_root: &str) -> Result<String, String> {
    // Validate that the bundled DLL is a real PE binary (MZ header), not a placeholder.
    if !BYPASS_DSOUND.starts_with(b"MZ") {
        return Err(
            "Bundled dsound.dll is a placeholder. \
             Replace src-tauri/resources/bypass/dsound.dll with the real file \
             from the Nexusmods bypass mod and rebuild the app."
                .to_string(),
        );
    }

    let bin_dir = binaries_dir(game_root);
    if !bin_dir.exists() {
        return Err(format!(
            "Binaries directory not found: {}\nMake sure the game root path is correct.",
            bin_dir.display()
        ));
    }

    let dsound_path = bin_dir.join("dsound.dll");
    let plugins_dir = bin_dir.join("plugins");
    let asi_path = plugins_dir.join("MarvelRivalsUTOCSignatureBypass.asi");

    let dsound_ok = file_matches(&dsound_path, BYPASS_DSOUND);
    let asi_ok = file_matches(&asi_path, BYPASS_ASI);
    let mods_ok = mods_dir(game_root).exists();

    if dsound_ok && asi_ok && mods_ok {
        return Ok("Signature bypass is already installed and up to date.".to_string());
    }

    if !dsound_ok {
        fs::write(&dsound_path, BYPASS_DSOUND).map_err(|e| e.to_string())?;
    }

    if !asi_ok {
        fs::create_dir_all(&plugins_dir).map_err(|e| e.to_string())?;
        fs::write(&asi_path, BYPASS_ASI).map_err(|e| e.to_string())?;
    }

    if !mods_ok {
        fs::create_dir_all(mods_dir(game_root)).map_err(|e| e.to_string())?;
    }

    Ok(format!("Bypass installed to {}", bin_dir.display()))
}
