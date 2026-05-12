//! Scans mods for pak files containing tweakable INI entries and reads their active tweak states.

use std::path::Path;

use crate::paths::mods_dir;
use crate::tweaks::TweakState;

use super::cvars::parse_console_vars;
use super::io::{extract_file_to_string, inspect_pak_for_any_ini, inspect_pak_for_ini};
use super::{PakIniInfo, PakIniListing, PakTweakState};

/// Inspect one pak and return INI metadata when present.
pub(crate) fn inspect_single_pak(pak_path: &str) -> Result<Option<PakIniInfo>, String> {
    inspect_pak_for_ini(Path::new(pak_path))
}

/// Inspect one pak and list every `.ini` entry inside (any-INI variant).
pub(crate) fn inspect_single_pak_any_ini(pak_path: &str) -> Result<Option<PakIniListing>, String> {
    inspect_pak_for_any_ini(Path::new(pak_path))
}

/// Scan `~mods` and return paks that contain tweakable INI files.
pub(crate) fn scan_mod_paks(game_root: &str, recursive: bool) -> Result<Vec<PakIniInfo>, String> {
    let mods_dir = mods_dir(game_root);
    if !mods_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut results = Vec::new();
    for rel_path in crate::mods::walk_mod_files(&mods_dir, recursive) {
        let path = mods_dir.join(&rel_path);
        if path.extension().and_then(|x| x.to_str()) != Some("pak") {
            continue;
        }
        match inspect_pak_for_ini(&path) {
            Ok(Some(info)) => results.push(info),
            Ok(None) => {}
            Err(_) => {}
        }
    }
    results.sort_by(|a, b| a.pak_name.cmp(&b.pak_name));
    Ok(results)
}

/// Scan `~mods` and return paks that contain ANY `.ini` file (used by the Pak
/// INI Editor tab). Unlike `scan_mod_paks`, this is not limited to the curated
/// Engine/DeviceProfiles filenames.
pub(crate) fn scan_mod_paks_any_ini(
    game_root: &str,
    recursive: bool,
) -> Result<Vec<PakIniListing>, String> {
    let mods_dir = mods_dir(game_root);
    if !mods_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut results = Vec::new();
    for rel_path in crate::mods::walk_mod_files(&mods_dir, recursive) {
        let path = mods_dir.join(&rel_path);
        if path.extension().and_then(|x| x.to_str()) != Some("pak") {
            continue;
        }
        match inspect_pak_for_any_ini(&path) {
            Ok(Some(info)) => results.push(info),
            Ok(None) => {}
            Err(_) => {}
        }
    }
    results.sort_by(|a, b| a.pak_name.cmp(&b.pak_name));
    Ok(results)
}

/// Read CVar values from pak INI files.
///
/// If both Engine and DeviceProfiles are present, DeviceProfiles overrides shared keys.
pub(crate) fn read_pak_tweaks(pak_path: &str) -> Result<Vec<PakTweakState>, String> {
    let pak_path = Path::new(pak_path);
    let info = inspect_pak_for_ini(pak_path)?
        .ok_or_else(|| "No INI config files found in this pak.".to_string())?;

    let mut merged: Vec<PakTweakState> = if let Some(ref eng) = info.engine_ini_entry {
        let content = extract_file_to_string(pak_path, eng)?;
        parse_console_vars(&content, "DefaultEngine.ini")
    } else {
        Vec::new()
    };

    if let Some(ref dp) = info.device_profiles_entry {
        let content = extract_file_to_string(pak_path, dp)?;
        let dp_vars = parse_console_vars(&content, "DefaultDeviceProfiles.ini");

        for dp_var in dp_vars {
            let key_lower = dp_var.key.to_ascii_lowercase();
            merged.retain(|v| v.key.to_ascii_lowercase() != key_lower);
            merged.push(dp_var);
        }
    }

    Ok(merged)
}

/// Detect active tweaks from pak INI content using the shared tweak detector.
pub(crate) fn detect_pak_tweaks(pak_path: &str) -> Result<Vec<TweakState>, String> {
    let merged = read_pak_tweaks(pak_path)?;
    let synthetic: String = merged
        .iter()
        .map(|s| format!("{}={}", s.key, s.value))
        .collect::<Vec<_>>()
        .join("\n");

    Ok(crate::tweaks::detect_tweaks_unscoped(&synthetic))
}

/// Extract a single file from a pak as a UTF-8 string.
pub(crate) fn extract_pak_ini(pak_path: &str, entry: &str) -> Result<String, String> {
    extract_file_to_string(Path::new(pak_path), entry)
}
