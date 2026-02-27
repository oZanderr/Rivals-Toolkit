use std::fs;

use super::paths::{binaries_dir, mods_dir};

// Bypass files bundled at compile time.
// Place the real files at:
//   src-tauri/resources/bypass/dsound.dll
//   src-tauri/resources/bypass/plugins/bypass.asi
// before building. The install command validates the DLL header at runtime.
static BYPASS_DSOUND: &[u8] = include_bytes!("../../resources/bypass/dsound.dll");
static BYPASS_ASI: &[u8] =
    include_bytes!("../../resources/bypass/plugins/MarvelRivalsUTOCSignatureBypass.asi");

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

    fs::write(bin_dir.join("dsound.dll"), BYPASS_DSOUND).map_err(|e| e.to_string())?;

    let plugins_dir = bin_dir.join("plugins");
    fs::create_dir_all(&plugins_dir).map_err(|e| e.to_string())?;
    fs::write(
        plugins_dir.join("MarvelRivalsUTOCSignatureBypass.asi"),
        BYPASS_ASI,
    )
    .map_err(|e| e.to_string())?;

    fs::create_dir_all(mods_dir(game_root)).map_err(|e| e.to_string())?;

    Ok(format!("Bypass installed to {}", bin_dir.display()))
}
