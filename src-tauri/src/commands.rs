use crate::{detect, pak};

use std::{
    fs,
    io::{BufReader, BufWriter},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

// Bypass files bundled at compile time.
// Place the real files at:
//   src-tauri/resources/bypass/dsound.dll
//   src-tauri/resources/bypass/plugins/bypass.asi
// before building. The install command validates the DLL header at runtime.
static BYPASS_DSOUND: &[u8] = include_bytes!("../resources/bypass/dsound.dll");
static BYPASS_ASI: &[u8] = include_bytes!("../resources/bypass/plugins/MarvelRivalsUTOCSignatureBypass.asi");

fn paks_dir(game_root: &str) -> PathBuf {
    PathBuf::from(game_root).join("MarvelGame\\Marvel\\Content\\Paks")
}

fn mods_dir(game_root: &str) -> PathBuf {
    paks_dir(game_root).join("~mods")
}

fn binaries_dir(game_root: &str) -> PathBuf {
    PathBuf::from(game_root).join("MarvelGame\\Marvel\\Binaries\\Win64")
}

#[derive(Serialize, Deserialize)]
pub(crate) struct ModsStatus {
    pub mods_folder_exists: bool,
    pub mods_folder_path: String,
    pub sig_bypass_installed: bool,
    pub mod_paks: Vec<String>,
}

#[tauri::command]
pub(crate) fn detect_install_path() -> Option<detect::InstallInfo> {
    detect::detect_game_install()
}

#[tauri::command]
pub(crate) fn list_pak_files(game_root: String) -> Result<Vec<String>, String> {
    pak::list_pak_files(&game_root)
}

#[tauri::command]
pub(crate) async fn list_pak_contents(pak_path: String) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || pak::list_pak_contents(&pak_path))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn unpack_pak(
    pak_path: String,
    output_dir: String,
) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || pak::unpack_pak(&pak_path, &output_dir))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn extract_single_file(
    pak_path: String,
    file_name: String,
    output_path: String,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        pak::extract_single_file(&pak_path, &file_name, &output_path)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn repack_pak(input_dir: String, output_pak: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || pak::repack_pak(&input_dir, &output_pak))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) fn get_mods_status(game_root: String) -> ModsStatus {
    let mods = mods_dir(&game_root);
    let exists = mods.exists();
    let sig_bypass_installed = binaries_dir(&game_root).join("dsound.dll").exists();

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

#[tauri::command]
pub(crate) fn install_signature_bypass(game_root: String) -> Result<String, String> {
    // Validate that the bundled DLL is a real PE binary (MZ header), not a placeholder.
    if !BYPASS_DSOUND.starts_with(b"MZ") {
        return Err(
            "Bundled dsound.dll is a placeholder. \
             Replace src-tauri/resources/bypass/dsound.dll with the real file \
             from the Nexusmods bypass mod and rebuild the app."
                .to_string(),
        );
    }

    let bin_dir = binaries_dir(&game_root);
    if !bin_dir.exists() {
        return Err(format!(
            "Binaries directory not found: {}\nMake sure the game root path is correct.",
            bin_dir.display()
        ));
    }

    fs::write(bin_dir.join("dsound.dll"), BYPASS_DSOUND).map_err(|e| e.to_string())?;

    let plugins_dir = bin_dir.join("plugins");
    fs::create_dir_all(&plugins_dir).map_err(|e| e.to_string())?;
    fs::write(plugins_dir.join("MarvelRivalsUTOCSignatureBypass.asi"), BYPASS_ASI).map_err(|e| e.to_string())?;

    fs::create_dir_all(mods_dir(&game_root)).map_err(|e| e.to_string())?;

    Ok(format!("Bypass installed to {}", bin_dir.display()))
}

#[tauri::command]
pub(crate) fn open_mods_folder(game_root: String) -> Result<(), String> {
    let mods = mods_dir(&game_root);
    if !mods.exists() {
        return Err("~mods folder does not exist — install the bypass first.".to_string());
    }
    #[cfg(target_os = "windows")]
    std::process::Command::new("explorer")
        .arg(mods.to_string_lossy().as_ref())
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}
