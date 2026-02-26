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
    (!val.is_empty()).then_some(val)
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

    let steamapps = PathBuf::from(root.trim()).join("steamapps");
    let vdf = steamapps.join("libraryfolders.vdf");
    let mut libs = vec![steamapps];

    if let Ok(content) = fs::read_to_string(vdf) {
        libs.extend(
            content
                .lines()
                .filter(|l| l.trim().starts_with("\"path\""))
                .filter_map(|l| acf_value(l.trim()))
                .map(|val| PathBuf::from(val.replace("\\\\", "\\")).join("steamapps")),
        );
    }

    libs
}

fn find_steam_install() -> Option<PathBuf> {
    steam_library_paths().into_iter().find_map(|lib| {
        let manifest = lib.join(format!("appmanifest_{MARVEL_STEAM_APPID}.acf"));
        let content = fs::read_to_string(&manifest).ok()?;
        content.lines().find_map(|line| {
            let line = line.trim();
            if !line.starts_with("\"installdir\"") { return None; }
            acf_value(line).map(|val| lib.join("common").join(val))
        })
    })
}

fn epic_data_path() -> Option<PathBuf> {
    let raw = hklm_str(r"SOFTWARE\Epic Games\EpicGamesLauncher", "AppDataPath")?;
    let p = PathBuf::from(raw.trim());
    p.exists().then_some(p)
}

fn find_epic_install() -> Option<PathBuf> {
    let data_dir = epic_data_path()
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData\Epic\EpicGamesLauncher\Data"));

    fs::read_dir(data_dir.join("Manifests"))
        .ok()?
        .flatten()
        .find_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("item") {
                return None;
            }
            let content = fs::read_to_string(&path).ok()?;
            let json = serde_json::from_str::<Value>(&content).ok()?;

            let app_name     = json["AppName"].as_str().unwrap_or_default();
            let display_name = json["DisplayName"].as_str().unwrap_or_default();

            if !app_name.eq_ignore_ascii_case(MARVEL_EPIC_APP_NAME)
                && !display_name.eq_ignore_ascii_case("Marvel Rivals")
            {
                return None;
            }

            // Epic manifests may use forward slashes on Windows
            let location = json["InstallLocation"].as_str()?;
            let p = PathBuf::from(location.replace('/', "\\"));
            p.exists().then_some(p)
        })
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
    let (path, source) = None
        .or_else(|| find_steam_install().map(|p| (p, InstallSource::Steam)))
        .or_else(|| find_epic_install().map(|p| (p, InstallSource::Epic)))?;

    Some(InstallInfo {
        path: path.to_string_lossy().into_owned(),
        source,
    })
}
