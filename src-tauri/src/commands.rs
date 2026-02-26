use std::{fs, path::PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use winreg::{enums::HKEY_LOCAL_MACHINE, RegKey};

const MARVEL_STEAM_APPID: &str = "2767030";
const MARVEL_EPIC_APP_NAME: &str = "MarvelRivals";

/// Read a REG_SZ string value from HKLM.
fn hklm_str(subkey: &str, value: &str) -> Option<String> {
    RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey(subkey)
        .ok()?
        .get_value::<String, _>(value)
        .ok()
}

/// Extract the quoted value token from an ACF/VDF line: `"key"  "value"`.
fn acf_value(line: &str) -> Option<&str> {
    let val = line.split('"').nth(3)?.trim();
    if val.is_empty() { None } else { Some(val) }
}

fn steam_library_paths() -> Vec<PathBuf> {
    let Some(root) = [
        r"SOFTWARE\Wow6432Node\Valve\Steam",
        r"SOFTWARE\Valve\Steam",
    ]
    .iter()
    .find_map(|&k| hklm_str(k, "InstallPath"))
    else {
        return Vec::new();
    };

    let root_path = PathBuf::from(root.trim());
    let steamapps = root_path.join("steamapps");
    let vdf = steamapps.join("libraryfolders.vdf");
    let mut libs = vec![steamapps];

    if let Ok(content) = fs::read_to_string(vdf) {
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with("\"path\"") {
                if let Some(val) = acf_value(line) {
                    let val = val.replace("\\\\", "\\");
                    libs.push(PathBuf::from(val).join("steamapps"));
                }
            }
        }
    }

    libs
}

fn find_steam_install() -> Option<PathBuf> {
    for lib in steam_library_paths() {
        let manifest = lib.join(format!("appmanifest_{MARVEL_STEAM_APPID}.acf"));
        let Ok(content) = fs::read_to_string(&manifest) else { continue };

        for line in content.lines() {
            let line = line.trim();
            if line.starts_with("\"installdir\"") {
                if let Some(val) = acf_value(line) {
                    return Some(lib.join("common").join(val));
                }
            }
        }
    }
    None
}

fn epic_data_path() -> Option<PathBuf> {
    let raw = hklm_str(r"SOFTWARE\Epic Games\EpicGamesLauncher", "AppDataPath")?;
    let p = PathBuf::from(raw.trim());
    p.exists().then_some(p)
}

fn find_epic_install() -> Option<PathBuf> {
    let data_dir = epic_data_path()
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData\Epic\EpicGamesLauncher\Data"));

    for entry in fs::read_dir(data_dir.join("Manifests")).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("item") {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else { continue };
        let Ok(json) = serde_json::from_str::<Value>(&content) else { continue };

        let app_name     = json["AppName"].as_str().unwrap_or_default();
        let display_name = json["DisplayName"].as_str().unwrap_or_default();

        if app_name.eq_ignore_ascii_case(MARVEL_EPIC_APP_NAME)
            || display_name.eq_ignore_ascii_case("Marvel Rivals")
        {
            if let Some(location) = json["InstallLocation"].as_str() {
                // Epic manifests may use forward slashes on Windows
                let p = PathBuf::from(location.replace('/', "\\"));
                if p.exists() {
                    return Some(p);
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
    [
        find_steam_install().map(|p| (p, InstallSource::Steam)),
        find_epic_install().map(|p| (p, InstallSource::Epic)),
    ]
    .into_iter()
    .flatten()
    .next()
    .map(|(p, source)| InstallInfo {
        path: p.to_string_lossy().into_owned(),
        source,
    })
}
