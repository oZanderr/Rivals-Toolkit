//! Detects Marvel Rivals installation via Steam library folders.

use std::{fs, path::PathBuf};

use crate::platform::steam_roots;

const MARVEL_STEAM_APPID: &str = "2767030";

/// Extract the quoted value token from an ACF/VDF line: `"key"  "value"`
fn acf_value(line: &str) -> Option<&str> {
    let val = line.split('"').nth(3)?.trim();
    (!val.is_empty()).then_some(val)
}

/// Expand each Steam root into its `steamapps` library folders, reading
/// `libraryfolders.vdf` to pick up libraries outside the default install.
fn steam_library_paths() -> Vec<PathBuf> {
    steam_roots()
        .into_iter()
        .flat_map(|root| {
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
        })
        .collect()
}

pub(super) fn find_steam_install() -> Option<PathBuf> {
    steam_library_paths().into_iter().find_map(|lib| {
        let manifest = lib.join(format!("appmanifest_{MARVEL_STEAM_APPID}.acf"));
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
