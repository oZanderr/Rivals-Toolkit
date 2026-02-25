use std::{
    fs,
    path::PathBuf,
};

use serde::{Deserialize, Serialize};

const MARVEL_STEAM_APPID: &str = "2767030";

fn steam_library_paths() -> Vec<PathBuf> {
    use winreg::enums::*;
    use winreg::RegKey;

    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let reg_candidates = [
        r"SOFTWARE\Wow6432Node\Valve\Steam",
        r"SOFTWARE\Valve\Steam",
    ];

    let Some(root) = reg_candidates.iter().find_map(|&k| {
        hklm.open_subkey(k)
            .ok()?
            .get_value::<String, _>("InstallPath")
            .ok()
    }) else {
        return Vec::new();
    };

    let root_path = PathBuf::from(root.trim());
    let steamapps = root_path.join("steamapps");
    let vdf = steamapps.join("libraryfolders.vdf");
    let mut libs = vec![steamapps];

    // Parse additional library folders from libraryfolders.vdf
    if let Ok(content) = fs::read_to_string(vdf) {
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with("\"path\"") {
                if let Some(val) = line.split('"').nth(3) {
                    let val = val.trim().replace("\\\\", "\\");
                    if !val.is_empty() {
                        libs.push(PathBuf::from(val).join("steamapps"));
                    }
                }
            }
        }
    }

    libs
}

fn find_steam_install() -> Option<PathBuf> {
    for lib in steam_library_paths() {
        let manifest = lib.join(format!("appmanifest_{}.acf", MARVEL_STEAM_APPID));
        let Ok(content) = fs::read_to_string(&manifest) else {
            continue;
        };
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with("\"installdir\"") {
                if let Some(val) = line.split('"').nth(3) {
                    let val = val.trim();
                    if !val.is_empty() {
                        return Some(lib.join("common").join(val));
                    }
                }
            }
        }
    }
    None
}

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

#[tauri::command]
pub(crate) fn detect_install_path() -> Option<InstallInfo> {
    find_steam_install().map(|p| InstallInfo {
        path: p.to_string_lossy().into_owned(),
        source: InstallSource::Steam,
    })
}
