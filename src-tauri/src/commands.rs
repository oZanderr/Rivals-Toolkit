use std::{
    fs,
    path::{PathBuf},
};

use serde::{Deserialize, Serialize};

const MARVEL_STEAM_APPID: &str = "2767030";

fn steam_library_paths() -> Vec<PathBuf> {
    let mut libs = Vec::new();

    #[cfg(target_os = "windows")]
    {
        use winreg::enums::*;
        use winreg::RegKey;

        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        let reg_candidates = [
            r"SOFTWARE\Wow6432Node\Valve\Steam",
            r"SOFTWARE\Valve\Steam",
        ];

        let steam_root = reg_candidates.iter().find_map(|&k| {
            hklm.open_subkey(k)
                .ok()?
                .get_value::<String, _>("InstallPath")
                .ok()
        });

        if let Some(root) = steam_root {
            let root_path = PathBuf::from(&root);
            libs.push(root_path.join("steamapps"));

            // Parse additional library folders from libraryfolders.vdf
            let vdf = root_path.join("steamapps\\libraryfolders.vdf");
            if let Ok(content) = fs::read_to_string(vdf) {
                for line in content.lines() {
                    let line = line.trim();
                    if line.starts_with("\"path\"") {
                        if let Some(val) = line.split('"').nth(3) {
                            libs.push(PathBuf::from(val.replace("\\\\", "\\")).join("steamapps"));
                        }
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
        if let Ok(content) = fs::read_to_string(manifest) {
            for line in content.lines() {
                let line = line.trim();
                if line.starts_with("\"installdir\"") {
                    if let Some(val) = line.split('"').nth(3) {
                        let game_path = lib.join("common").join(val);
                        if game_path.exists() {
                            return Some(game_path);
                        }
                    }
                }
            }
        }
    }
    None
}

#[derive(Serialize, Deserialize)]
pub(crate) struct InstallInfo {
    pub found: bool,
    pub path: String,
    /// "Steam" | "Epic" | "None"
    pub source: String,
}

#[tauri::command]
pub(crate) fn detect_install_path() -> InstallInfo {
    if let Some(p) = find_steam_install() {
        return InstallInfo {
            found: true,
            path: p.to_string_lossy().into_owned(),
            source: "Steam".into(),
        };
    }
    InstallInfo {
        found: false,
        path: String::new(),
        source: "None".into(),
    }
}
