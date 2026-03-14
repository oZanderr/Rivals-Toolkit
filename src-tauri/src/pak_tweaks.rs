mod ini;
mod io;

use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

use ini::{IniType, apply_edits_to_ini, parse_console_vars};
use io::{extract_file_to_string, inspect_pak_for_ini, repack_dir_to_pak, unpack_to_dir};

use crate::paths::mods_dir;

/// Describes which INI files exist inside a given pak mod.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PakIniInfo {
    pub pak_name: String,
    pub pak_path: String,
    pub has_device_profiles: bool,
    pub has_engine_ini: bool,
    pub device_profiles_entry: Option<String>,
    pub engine_ini_entry: Option<String>,
}

/// The current console variable values detected inside a pak mod's INI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PakTweakState {
    pub key: String,
    pub value: String,
    pub source: String,
}

/// A console variable to set or remove inside a pak mod's INI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PakTweakEdit {
    pub key: String,
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_section: Option<String>,
}

/// Inspect a single pak file and return its INI metadata, or None if it contains no tweakable INI.
pub(crate) fn inspect_single_pak(pak_path: &str) -> Result<Option<PakIniInfo>, String> {
    io::inspect_pak_for_ini(Path::new(pak_path))
}

/// Scan all pak mods in ~mods and report which ones contain INI config files.
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

/// Read current console variables from a pak mod's INI files.
///
/// When both DefaultEngine.ini and DefaultDeviceProfiles.ini exist, both are
/// read and merged: Engine.ini provides the base set of CVars, then
/// DeviceProfiles.ini layered on top (its values win for any shared keys).
/// Keys that only exist in Engine.ini are still included — they take effect
/// even when DeviceProfiles.ini is present, as long as DeviceProfiles does
/// not explicitly define the same key.
pub(crate) fn read_pak_tweaks(pak_path: &str) -> Result<Vec<PakTweakState>, String> {
    let pak_path = Path::new(pak_path);
    let info = inspect_pak_for_ini(pak_path)?
        .ok_or_else(|| "No INI config files found in this pak.".to_string())?;

    // Start with Engine.ini as the base (lower priority).
    let mut merged: Vec<PakTweakState> = if let Some(ref eng) = info.engine_ini_entry {
        let content = extract_file_to_string(pak_path, eng)?;
        parse_console_vars(&content, "DefaultEngine.ini")
    } else {
        Vec::new()
    };

    // Layer DeviceProfiles on top: its values override Engine for shared keys,
    // and any keys unique to Engine are preserved as-is.
    if let Some(ref dp) = info.device_profiles_entry {
        let content = extract_file_to_string(pak_path, dp)?;
        let dp_vars = parse_console_vars(&content, "DefaultDeviceProfiles.ini");

        for dp_var in dp_vars {
            let key_lower = dp_var.key.to_ascii_lowercase();
            if let Some(existing) = merged
                .iter_mut()
                .find(|v| v.key.to_ascii_lowercase() == key_lower)
            {
                *existing = dp_var;
            } else {
                merged.push(dp_var);
            }
        }
    }

    Ok(merged)
}

/// Detect which tweaks are currently active in a pak mod's INI files.
///
/// Reads and merges both INI files (Engine base + DeviceProfiles override),
/// then runs the same Rust-side detection logic used by ScalabilitySettings so
/// that `default_enabled`, section-awareness, and all future changes live in
/// one place rather than being duplicated in JS.
pub(crate) fn detect_pak_tweaks(
    pak_path: &str,
) -> Result<Vec<crate::scalability::TweakState>, String> {
    // Build a synthetic flat INI string from the merged key-value pairs.
    // `detect_tweaks` uses line-by-line `key=value` matching and doesn't
    // require section headers, so a flat dump is sufficient.
    let merged = read_pak_tweaks(pak_path)?;
    let synthetic: String = merged
        .iter()
        .map(|s| format!("{}={}", s.key, s.value))
        .collect::<Vec<_>>()
        .join("\n");

    Ok(crate::scalability::detect_tweaks(&synthetic))
}

/// Apply tweaks to a pak mod: extract → edit INI → repack in place.
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

    // 1. Extract entire pak to temp dir
    unpack_to_dir(pak, &temp_dir)?;

    // 2. Apply edits.
    //
    // When both files coexist:
    //   • Edits with engine_section set → Engine.ini only (those keys only work
    //     there, e.g. ApplicationScale under [/Script/Engine.UserInterfaceSettings]).
    //   • Other set/add edits → DeviceProfiles only (it overrides Engine for those keys).
    //   • Remove edits → DeviceProfiles AND Engine.ini (key could live in either).
    //
    // When only one file exists: apply all edits to that file.

    if info.has_device_profiles {
        // Split: engine-section edits go to Engine.ini; the rest go to DeviceProfiles.
        let (engine_edits, dp_edits): (Vec<PakTweakEdit>, Vec<PakTweakEdit>) = edits
            .iter()
            .cloned()
            .partition(|e| e.engine_section.is_some() && e.value.is_some());

        // Apply DeviceProfiles edits (set/add/remove for regular CVars).
        let dp_entry = info
            .device_profiles_entry
            .as_ref()
            .ok_or("DeviceProfiles entry missing despite has_device_profiles flag")?;
        let dp_rel = dp_entry
            .trim_start_matches("../../../")
            .trim_start_matches('/');
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

        // Apply engine-section edits AND all remove-edits to Engine.ini.
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
                let eng_rel = eng_entry
                    .trim_start_matches("../../../")
                    .trim_start_matches('/');
                let eng_file = temp_dir.join(eng_rel);

                if let Ok(content) = fs::read_to_string(&eng_file) {
                    let modified = apply_edits_to_ini(&content, &eng_edits, IniType::Engine);
                    let _ = fs::write(&eng_file, &modified);
                }
            }
        }
    } else {
        // Only Engine.ini present.
        let eng_entry = info
            .engine_ini_entry
            .as_ref()
            .ok_or("Engine INI entry missing despite no device profiles")?;
        let eng_rel = eng_entry
            .trim_start_matches("../../../")
            .trim_start_matches('/');
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

    // 3. Repack to a temp pak, then replace the original
    let temp_pak = pak
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(".{}_repacked.pak", stem));

    repack_dir_to_pak(&temp_dir, &temp_pak)?;

    // 4. Replace original with repacked
    fs::remove_file(pak).map_err(|e| format!("Failed to remove original pak: {}", e))?;
    fs::rename(&temp_pak, pak)
        .map_err(|e| format!("Failed to replace pak with repacked version: {}", e))?;

    // 5. Cleanup
    let _ = fs::remove_dir_all(&temp_dir);

    Ok(format!(
        "Applied {} edit(s) to {}",
        edits.len(),
        info.pak_name
    ))
}
