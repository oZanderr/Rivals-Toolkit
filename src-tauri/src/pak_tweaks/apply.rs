//! Applies catalogue-driven edits and raw INI content saves to pak files in place.

use std::fs;
use std::path::Path;

use crate::pak::profile::strip_mount_prefix;

use super::cvars::{IniType, apply_edits_to_ini};
use super::io::{inspect_pak_for_ini, with_unpacked_pak};
use super::{PakIniFileContent, PakTweakEdit};

/// Apply catalogue-driven edits to pak INI files and repack in place.
///
/// When DeviceProfiles exists, scoped engine edits go to Engine.ini and remove
/// edits are mirrored to Engine.ini to avoid stale keys.
pub(crate) fn apply_pak_tweaks(pak_path: &str, edits: &[PakTweakEdit]) -> Result<String, String> {
    let pak = Path::new(pak_path);
    let info = inspect_pak_for_ini(pak)?
        .ok_or_else(|| "No INI config files found in this pak.".to_string())?;
    let pak_name = info.pak_name.clone();
    let edit_count = edits.len();

    with_unpacked_pak(pak, |temp_dir| {
        if info.has_device_profiles {
            apply_device_profiles_edits(temp_dir, &info, edits)?;
        } else {
            apply_engine_only_edits(temp_dir, &info, edits)?;
        }
        Ok(())
    })?;

    let label = if edit_count == 1 { "edit" } else { "edits" };
    Ok(format!("Applied {edit_count} {label} to {pak_name}"))
}

/// Replace raw INI file contents in a pak and repack in place.
pub(crate) fn save_pak_ini(
    pak_path: &str,
    files: Vec<PakIniFileContent>,
) -> Result<String, String> {
    let pak = Path::new(pak_path);
    let file_count = files.len();

    with_unpacked_pak(pak, |temp_dir| {
        for file in &files {
            let rel = strip_mount_prefix(&file.entry);
            let dest = temp_dir.join(rel);
            fs::write(&dest, &file.content)
                .map_err(|e| format!("Failed to write {}: {}", dest.display(), e))?;
        }
        Ok(())
    })?;

    let pak_name = pak
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    Ok(format!("Saved {} file(s) to {}", file_count, pak_name))
}

fn apply_device_profiles_edits(
    temp_dir: &Path,
    info: &super::PakIniInfo,
    edits: &[PakTweakEdit],
) -> Result<(), String> {
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

    // Mirror remove-edits to Engine.ini so stale keys don't shadow the fresh DP values.
    if let Some(eng_entry) = info.engine_ini_entry.as_ref() {
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
            let content = fs::read_to_string(&eng_file).map_err(|e| {
                format!(
                    "Failed to read Engine INI for mirror edits {}: {}",
                    eng_file.display(),
                    e
                )
            })?;
            let modified = apply_edits_to_ini(&content, &eng_edits, IniType::Engine);
            fs::write(&eng_file, &modified).map_err(|e| {
                format!(
                    "Failed to write mirrored Engine INI {}: {}",
                    eng_file.display(),
                    e
                )
            })?;
        }
    }

    Ok(())
}

fn apply_engine_only_edits(
    temp_dir: &Path,
    info: &super::PakIniInfo,
    edits: &[PakTweakEdit],
) -> Result<(), String> {
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

    Ok(())
}
