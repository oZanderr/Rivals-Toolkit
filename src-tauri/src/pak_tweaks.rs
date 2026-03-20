mod ini;
mod io;

use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

use ini::{IniType, apply_edits_to_ini, parse_console_vars};
use io::{extract_file_to_string, inspect_pak_for_ini, repack_dir_to_pak, unpack_to_dir};

use crate::pak::profile::strip_mount_prefix;
use crate::paths::mods_dir;

/// INI entries discovered in a pak mod.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PakIniInfo {
    pub pak_name: String,
    pub pak_path: String,
    pub has_device_profiles: bool,
    pub has_engine_ini: bool,
    pub device_profiles_entry: Option<String>,
    pub engine_ini_entry: Option<String>,
}

/// Parsed CVar state from pak INI files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PakTweakState {
    pub key: String,
    pub value: String,
    pub source: String,
}

/// Requested CVar edit for pak INI files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PakTweakEdit {
    pub key: String,
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_section: Option<String>,
}

/// Inspect one pak and return INI metadata when present.
pub(crate) fn inspect_single_pak(pak_path: &str) -> Result<Option<PakIniInfo>, String> {
    io::inspect_pak_for_ini(Path::new(pak_path))
}

/// Scan `~mods` and return paks that contain tweakable INI files.
pub(crate) fn scan_mod_paks(game_root: &str) -> Result<Vec<PakIniInfo>, String> {
    let mods_dir = mods_dir(game_root);
    if !mods_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut results = Vec::new();
    for entry in fs::read_dir(&mods_dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
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
pub(crate) fn detect_pak_tweaks(
    pak_path: &str,
) -> Result<Vec<crate::scalability::TweakState>, String> {
    let merged = read_pak_tweaks(pak_path)?;
    let synthetic: String = merged
        .iter()
        .map(|s| format!("{}={}", s.key, s.value))
        .collect::<Vec<_>>()
        .join("\n");

    Ok(crate::scalability::detect_tweaks(&synthetic))
}

/// Apply edits to pak INI files and repack in place.
pub(crate) fn apply_pak_tweaks(pak_path: &str, edits: &[PakTweakEdit]) -> Result<String, String> {
    let pak = Path::new(pak_path);
    if !pak.exists() {
        return Err(format!("Pak file not found: {}", pak_path));
    }

    let info = inspect_pak_for_ini(pak)?
        .ok_or_else(|| "No INI config files found in this pak.".to_string())?;

    let stem = pak
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let temp_dir = pak
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(".{}_temp", stem));

    let _ = fs::remove_dir_all(&temp_dir);

    unpack_to_dir(pak, &temp_dir)?;

    // When DeviceProfiles exists, scoped engine edits go to Engine.ini.
    // Remove edits are also applied to Engine.ini to avoid stale keys.

    if info.has_device_profiles {
        let (engine_edits, dp_edits): (Vec<PakTweakEdit>, Vec<PakTweakEdit>) = edits
            .iter()
            .cloned()
            .partition(|e| e.engine_section.is_some() && e.value.is_some());

        let dp_entry = info
            .device_profiles_entry
            .as_ref()
            .ok_or("DeviceProfiles entry missing despite has_device_profiles flag")?;
        let dp_rel = strip_mount_prefix(dp_entry);
        let dp_file = temp_dir.join(dp_rel);

        let content = fs::read_to_string(&dp_file).map_err(|e| {
            format!(
                "Failed to read extracted DeviceProfiles INI {}: {}",
                dp_file.display(),
                e
            )
        })?;
        let modified = apply_edits_to_ini(&content, &dp_edits, IniType::DeviceProfiles);
        fs::write(&dp_file, &modified).map_err(|e| {
            format!(
                "Failed to write modified DeviceProfiles INI {}: {}",
                dp_file.display(),
                e
            )
        })?;

        if let Some(ref eng_entry) = info.engine_ini_entry {
            let remove_edits: Vec<PakTweakEdit> = edits
                .iter()
                .filter(|e| e.value.is_none())
                .cloned()
                .collect();

            let mut eng_edits = engine_edits;
            for r in remove_edits {
                if !eng_edits.iter().any(|e| e.key.eq_ignore_ascii_case(&r.key)) {
                    eng_edits.push(r);
                }
            }

            if !eng_edits.is_empty() {
                let eng_rel = strip_mount_prefix(eng_entry);
                let eng_file = temp_dir.join(eng_rel);

                if let Ok(content) = fs::read_to_string(&eng_file) {
                    let modified = apply_edits_to_ini(&content, &eng_edits, IniType::Engine);
                    let _ = fs::write(&eng_file, &modified);
                }
            }
        }
    } else {
        let eng_entry = info
            .engine_ini_entry
            .as_ref()
            .ok_or("Engine INI entry missing despite no device profiles")?;
        let eng_rel = strip_mount_prefix(eng_entry);
        let eng_file = temp_dir.join(eng_rel);

        let content = fs::read_to_string(&eng_file).map_err(|e| {
            format!(
                "Failed to read extracted Engine INI {}: {}",
                eng_file.display(),
                e
            )
        })?;
        let modified = apply_edits_to_ini(&content, edits, IniType::Engine);
        fs::write(&eng_file, &modified).map_err(|e| {
            format!(
                "Failed to write modified Engine INI {}: {}",
                eng_file.display(),
                e
            )
        })?;
    }

    let temp_pak = pak
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(".{}_repacked.pak", stem));

    repack_dir_to_pak(&temp_dir, &temp_pak)?;

    fs::remove_file(pak).map_err(|e| format!("Failed to remove original pak: {}", e))?;
    fs::rename(&temp_pak, pak)
        .map_err(|e| format!("Failed to replace pak with repacked version: {}", e))?;

    let _ = fs::remove_dir_all(&temp_dir);

    Ok(format!(
        "Applied {} edit(s) to {}",
        edits.len(),
        info.pak_name
    ))
}
