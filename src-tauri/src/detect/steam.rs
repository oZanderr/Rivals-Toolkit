//! Detects Marvel Rivals installation via the Steam install and its library folders.

use std::{fs, path::PathBuf};

use crate::paths::STEAM_APP_ID;

/// Extract the quoted value token from an ACF/VDF line: `"key"  "value"`
fn acf_value(line: &str) -> Option<&str> {
    let val = line.split('"').nth(3)?.trim();
    (!val.is_empty()).then_some(val)
}

#[cfg(windows)]
fn steam_root() -> Option<PathBuf> {
    [r"SOFTWARE\Wow6432Node\Valve\Steam", r"SOFTWARE\Valve\Steam"]
        .iter()
        .find_map(|&k| super::registry::hklm_str(k, "InstallPath"))
        .map(|root| PathBuf::from(root.trim()))
}

/// The link a native Steam install keeps to its data folder. Flatpak and Snap installs are picked
/// by hand in Settings.
#[cfg(not(windows))]
fn steam_root() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".steam").join("steam"))
}

fn steam_library_paths() -> Vec<PathBuf> {
    let Some(root) = steam_root() else {
        return Vec::new();
    };

    let root_steamapps = root.join("steamapps");
    let vdf = root_steamapps.join("libraryfolders.vdf");

    if let Ok(content) = fs::read_to_string(&vdf) {
        let from_vdf: Vec<PathBuf> = content
            .lines()
            .filter(|l| l.trim().starts_with("\"path\""))
            .filter_map(|l| acf_value(l.trim()))
            .map(|val| PathBuf::from(val.replace("\\\\", "\\")).join("steamapps"))
            .collect();

        if !from_vdf.is_empty() {
            return from_vdf;
        }
    }

    vec![root_steamapps]
}

pub(super) fn find_steam_install() -> Option<PathBuf> {
    steam_library_paths().into_iter().find_map(|lib| {
        let manifest = lib.join(format!("appmanifest_{STEAM_APP_ID}.acf"));
        let content = fs::read_to_string(&manifest).ok()?;
        content.lines().find_map(|line| {
            let line = line.trim();
            if !line.starts_with("\"installdir\"") {
                return None;
            }
            acf_value(line).map(|val| lib.join("common").join(val))
        })
    })
}
