//! Applies catalogue-driven edits and raw INI content saves to pak files in place.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use crate::pak::profile::strip_mount_prefix;

use super::cvars::{IniType, apply_edits_to_ini, parse_console_vars};
use super::io::{inspect_pak_for_ini, with_unpacked_pak};
use super::{PakIniFileContent, PakIniInfo, PakIniTarget, PakTweakEdit};

/// Engine-file targets in runtime priority order, lowest first.
const ENGINE_TARGETS: [PakIniTarget; 3] = [
    PakIniTarget::BaseEngine,
    PakIniTarget::Engine,
    PakIniTarget::WindowsEngine,
];

/// Apply catalogue-driven edits to a pak's INI files and repack in place.
///
/// Plain CVar edits are written to the highest-priority file present in the pak so a
/// new key takes effect without being shadowed. Engine-section settings are not console
/// variables and only belong in an engine file: they go to the highest-priority engine
/// file present. Every other file that already contains an edited key is kept in sync
/// so no higher-priority file shadows the user's edit; keys absent from a file are
/// never injected into it.
pub(crate) fn apply_pak_tweaks(pak_path: &str, edits: &[PakTweakEdit]) -> Result<String, String> {
    let pak = Path::new(pak_path);
    let info = inspect_pak_for_ini(pak)?
        .ok_or_else(|| "No INI config files found in this pak.".to_string())?;
    let pak_name = info.pak_name.clone();
    let edit_count = edits.len();

    let resolved = resolve_target(&info)?;
    // Engine-section edits need an engine file. Prefer the user's target if it's an
    // engine file; otherwise the highest-priority engine file present.
    let engine_section_target = if is_engine(resolved) {
        Some(resolved)
    } else {
        highest_engine_present(&info)
    };

    with_unpacked_pak(pak, |temp_dir| {
        let (engine_section_edits, plain_edits): (Vec<PakTweakEdit>, Vec<PakTweakEdit>) = edits
            .iter()
            .cloned()
            .partition(|e| e.engine_section.is_some());

        let target_entry = entry_for(&info, resolved)
            .ok_or("Target INI entry missing despite target resolution")?;
        apply_edits_to_file(temp_dir, target_entry, ini_type_for(resolved), &plain_edits)?;

        if !engine_section_edits.is_empty()
            && let Some(eng_target) = engine_section_target
            && let Some(eng_entry) = entry_for(&info, eng_target)
        {
            apply_edits_to_file(temp_dir, eng_entry, IniType::Engine, &engine_section_edits)?;
        }

        // Sync every other file that already contains a plain-edited key. Plain
        // edits and engine_section edits never collide on the same key, so the
        // engine_section_target file is still a valid sibling for plain edits.
        for sibling in all_targets().into_iter().filter(|t| *t != resolved) {
            if let Some(sib_entry) = entry_for(&info, sibling) {
                sync_existing_keys(
                    temp_dir,
                    sib_entry,
                    ini_type_for(sibling),
                    source_label(sibling),
                    &plain_edits,
                )?;
            }
        }

        // Engine-section edits also need sibling sync across the other engine files.
        if !engine_section_edits.is_empty()
            && let Some(eng_target) = engine_section_target
        {
            for sibling in ENGINE_TARGETS.iter().copied().filter(|t| *t != eng_target) {
                if let Some(sib_entry) = entry_for(&info, sibling) {
                    sync_existing_keys(
                        temp_dir,
                        sib_entry,
                        IniType::Engine,
                        source_label(sibling),
                        &engine_section_edits,
                    )?;
                }
            }
        }

        Ok(())
    })?;

    let label = if edit_count == 1 { "change" } else { "changes" };
    Ok(format!("Applied {edit_count} {label} to {pak_name}"))
}

/// Pick the highest-priority file present in the pak (DeviceProfiles > WindowsEngine >
/// DefaultEngine > BaseEngine) so a new edit takes effect without being shadowed.
fn resolve_target(info: &PakIniInfo) -> Result<PakIniTarget, String> {
    for candidate in [
        PakIniTarget::DeviceProfiles,
        PakIniTarget::WindowsEngine,
        PakIniTarget::Engine,
        PakIniTarget::BaseEngine,
    ] {
        if entry_for(info, candidate).is_some() {
            return Ok(candidate);
        }
    }
    Err("No INI config files found in this pak.".to_string())
}

fn all_targets() -> [PakIniTarget; 4] {
    [
        PakIniTarget::BaseEngine,
        PakIniTarget::Engine,
        PakIniTarget::WindowsEngine,
        PakIniTarget::DeviceProfiles,
    ]
}

fn is_engine(target: PakIniTarget) -> bool {
    !matches!(target, PakIniTarget::DeviceProfiles)
}

/// Highest-priority engine file actually present in the pak.
fn highest_engine_present(info: &PakIniInfo) -> Option<PakIniTarget> {
    [
        PakIniTarget::WindowsEngine,
        PakIniTarget::Engine,
        PakIniTarget::BaseEngine,
    ]
    .into_iter()
    .find(|&candidate| entry_for(info, candidate).is_some())
}

fn entry_for(info: &PakIniInfo, target: PakIniTarget) -> Option<&String> {
    match target {
        PakIniTarget::BaseEngine => info.base_engine_entry.as_ref(),
        PakIniTarget::Engine => info.engine_ini_entry.as_ref(),
        PakIniTarget::WindowsEngine => info.windows_engine_entry.as_ref(),
        PakIniTarget::DeviceProfiles => info.device_profiles_entry.as_ref(),
    }
}

fn ini_type_for(target: PakIniTarget) -> IniType {
    match target {
        PakIniTarget::BaseEngine | PakIniTarget::Engine | PakIniTarget::WindowsEngine => {
            IniType::Engine
        }
        PakIniTarget::DeviceProfiles => IniType::DeviceProfiles,
    }
}

/// `parse_console_vars` switches parsing rules based on whether the source name
/// contains "DeviceProfiles", so the label must reflect the file kind.
fn source_label(target: PakIniTarget) -> &'static str {
    match target {
        PakIniTarget::BaseEngine => "BaseEngine.ini",
        PakIniTarget::Engine => "DefaultEngine.ini",
        PakIniTarget::WindowsEngine => "WindowsEngine.ini",
        PakIniTarget::DeviceProfiles => "DefaultDeviceProfiles.ini",
    }
}

/// Replace raw INI file contents in a pak and repack in place. `files` writes are
/// applied first (creating parent dirs for brand-new entries), then `deletes` are
/// removed from the temp tree; repack picks up whatever remains.
pub(crate) fn save_pak_ini(
    pak_path: &str,
    files: Vec<PakIniFileContent>,
    deletes: Vec<String>,
) -> Result<String, String> {
    let pak = Path::new(pak_path);
    let change_count = files.len() + deletes.len();

    with_unpacked_pak(pak, |temp_dir| {
        for file in &files {
            let rel = strip_mount_prefix(&file.entry);
            let dest = temp_dir.join(rel);
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create {}: {}", parent.display(), e))?;
            }
            fs::write(&dest, &file.content)
                .map_err(|e| format!("Failed to write {}: {}", dest.display(), e))?;
        }
        for entry in &deletes {
            let rel = strip_mount_prefix(entry);
            let dest = temp_dir.join(rel);
            match fs::remove_file(&dest) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("Failed to delete {}: {}", dest.display(), e)),
            }
        }
        Ok(())
    })?;

    let pak_name = pak
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    Ok(format!("Saved {} change(s) to {}", change_count, pak_name))
}

/// Read an extracted pak INI, apply `edits`, and write it back.
fn apply_edits_to_file(
    temp_dir: &Path,
    entry: &str,
    ini_type: IniType,
    edits: &[PakTweakEdit],
) -> Result<(), String> {
    if edits.is_empty() {
        return Ok(());
    }
    let file = temp_dir.join(strip_mount_prefix(entry));
    let content = fs::read_to_string(&file)
        .map_err(|e| format!("Failed to read extracted INI {}: {}", file.display(), e))?;
    let modified = apply_edits_to_ini(&content, edits, ini_type);
    fs::write(&file, &modified)
        .map_err(|e| format!("Failed to write modified INI {}: {}", file.display(), e))?;
    Ok(())
}

/// Apply only the edits whose key already exists in the sibling file, so the two
/// INIs stay consistent without injecting keys the sibling never had.
fn sync_existing_keys(
    temp_dir: &Path,
    entry: &str,
    ini_type: IniType,
    source_label: &str,
    edits: &[PakTweakEdit],
) -> Result<(), String> {
    if edits.is_empty() {
        return Ok(());
    }
    let file = temp_dir.join(strip_mount_prefix(entry));
    let content = fs::read_to_string(&file)
        .map_err(|e| format!("Failed to read sibling INI {}: {}", file.display(), e))?;

    let present: HashSet<String> = parse_console_vars(&content, source_label)
        .into_iter()
        .map(|s| s.key.to_ascii_lowercase())
        .collect();
    let filtered: Vec<PakTweakEdit> = edits
        .iter()
        .filter(|e| present.contains(&e.key.to_ascii_lowercase()))
        .cloned()
        .collect();
    if filtered.is_empty() {
        return Ok(());
    }

    let modified = apply_edits_to_ini(&content, &filtered, ini_type);
    fs::write(&file, &modified)
        .map_err(|e| format!("Failed to write sibling INI {}: {}", file.display(), e))?;
    Ok(())
}
