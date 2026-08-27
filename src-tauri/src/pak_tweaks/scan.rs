//! Scans mods for pak files containing tweakable INI entries and reads their active tweak states.

use std::path::Path;

use crate::paths::{mods_dir, paks_dir};
use crate::tweaks::TweakState;

use super::cvars::parse_console_vars;
use super::io::{
    create_empty_pak, extract_file_to_string, extract_optional_entry, inspect_pak_for_any_ini,
    inspect_pak_for_ini,
};
use super::{
    PakIniInfo, PakIniListing, PakIniListingScan, PakIniScan, PakScanError, PakTweakState,
};

/// Inspect one pak and return INI metadata when present.
pub(crate) fn inspect_single_pak(pak_path: &str) -> Result<Option<PakIniInfo>, String> {
    inspect_pak_for_ini(Path::new(pak_path))
}

pub(crate) fn inspect_single_pak_any_ini(pak_path: &str) -> Result<Option<PakIniListing>, String> {
    inspect_pak_for_any_ini(Path::new(pak_path))
}

fn scan_error(path: &Path, error: String) -> PakScanError {
    PakScanError {
        pak_name: path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        pak_path: path.to_string_lossy().into_owned(),
        error,
    }
}

/// Every `.pak` under `~mods`, in listing order.
fn mod_pak_paths(game_root: &str, recursive: bool) -> Vec<std::path::PathBuf> {
    let mods_dir = mods_dir(game_root);
    if !mods_dir.is_dir() {
        return Vec::new();
    }
    crate::mods::walk_mod_files(&mods_dir, recursive)
        .into_iter()
        .map(|rel| mods_dir.join(rel))
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("pak"))
        .collect()
}

/// Scan `~mods` and return paks that contain tweakable INI files, plus any that could not be read.
pub(crate) fn scan_mod_paks(game_root: &str, recursive: bool) -> Result<PakIniScan, String> {
    let mut paks = Vec::new();
    let mut unreadable = Vec::new();
    for path in mod_pak_paths(game_root, recursive) {
        match inspect_pak_for_ini(&path) {
            Ok(Some(info)) => paks.push(info),
            Ok(None) => {}
            Err(e) => unreadable.push(scan_error(&path, e)),
        }
    }
    paks.sort_by(|a, b| a.pak_name.cmp(&b.pak_name));
    unreadable.sort_by(|a, b| a.pak_name.cmp(&b.pak_name));
    Ok(PakIniScan { paks, unreadable })
}

/// Scan `~mods` and return paks that contain any `.ini` file, plus any that could not be read.
pub(crate) fn scan_mod_paks_any_ini(
    game_root: &str,
    recursive: bool,
) -> Result<PakIniListingScan, String> {
    let mut paks = Vec::new();
    let mut unreadable = Vec::new();
    for path in mod_pak_paths(game_root, recursive) {
        match inspect_pak_for_any_ini(&path) {
            Ok(Some(info)) => paks.push(info),
            Ok(None) => {}
            Err(e) => unreadable.push(scan_error(&path, e)),
        }
    }
    paks.sort_by(|a, b| a.pak_name.cmp(&b.pak_name));
    unreadable.sort_by(|a, b| a.pak_name.cmp(&b.pak_name));
    Ok(PakIniListingScan { paks, unreadable })
}

/// Read CVar values from pak INI files, merged in runtime priority order (lowest first,
/// highest overrides): BaseEngine, DefaultEngine, WindowsEngine, DeviceProfiles. The map
/// is keyed by lowercased CVar name so insert is O(1); a Vec/retain merge here was O(N^2)
/// and stalled multi-second on mod paks with full engine INI overrides.
pub(crate) fn read_pak_tweaks(pak_path: &str) -> Result<Vec<PakTweakState>, String> {
    let pak_path = Path::new(pak_path);
    let info = inspect_pak_for_ini(pak_path)?
        .ok_or_else(|| "No INI config files found in this pak.".to_string())?;

    let layers: [(Option<&String>, &str); 4] = [
        (info.base_engine_entry.as_ref(), "BaseEngine.ini"),
        (info.engine_ini_entry.as_ref(), "DefaultEngine.ini"),
        (info.windows_engine_entry.as_ref(), "WindowsEngine.ini"),
        (
            info.device_profiles_entry.as_ref(),
            "DefaultDeviceProfiles.ini",
        ),
    ];

    let mut merged: std::collections::HashMap<String, PakTweakState> =
        std::collections::HashMap::new();
    for (entry, label) in layers {
        let Some(entry) = entry else { continue };
        let content = extract_file_to_string(pak_path, entry)?;
        for var in parse_console_vars(&content, label) {
            merged.insert(var.key.to_ascii_lowercase(), var);
        }
    }

    Ok(merged.into_values().collect())
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

/// Pull the game's stock copy of an INI from pakchunk0 so new mod entries can
/// start with the real defaults instead of an empty section header.
pub(crate) fn extract_game_default_ini(
    game_root: &str,
    in_pak_path: &str,
) -> Result<Option<String>, String> {
    let pakchunk0 = paks_dir(game_root).join("pakchunk0-Windows.pak");
    extract_optional_entry(&pakchunk0, in_pak_path)
}

/// Sanitize a user-typed pak name into the toolkit's mod convention
/// `<name>_9999999_P.pak`. Strips invalid Windows filename chars, idempotently
/// strips an existing `_9999999_P` suffix or `.pak` extension before re-applying.
fn normalize_pak_filename(raw: &str) -> Result<String, String> {
    const PRIORITY_SUFFIX: &str = "_9999999_P";

    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Pak name can't be empty".to_string());
    }
    // Strip extension first so suffix detection works on the bare stem.
    let no_ext = trimmed.strip_suffix(".pak").unwrap_or(trimmed);
    let no_ext = no_ext.strip_suffix(".PAK").unwrap_or(no_ext);
    let stem = no_ext.strip_suffix(PRIORITY_SUFFIX).unwrap_or(no_ext);
    let cleaned: String = stem
        .chars()
        .filter(|c| !matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'))
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        return Err("Pak name contains only invalid characters".to_string());
    }
    Ok(format!("{cleaned}{PRIORITY_SUFFIX}.pak"))
}

/// Create an empty mod pak in `~mods/` ready to be populated via the INI editor.
pub(crate) fn create_new_mod_pak(game_root: &str, name: &str) -> Result<PakIniListing, String> {
    let filename = normalize_pak_filename(name)?;
    let mods = mods_dir(game_root);
    if !mods.is_dir() {
        return Err(format!(
            "Mods folder doesn't exist at {} -- check your game path",
            mods.display()
        ));
    }
    let pak_path = mods.join(&filename);
    if pak_path.exists() {
        return Err(format!("{} already exists", filename));
    }
    create_empty_pak(&pak_path)?;
    Ok(PakIniListing {
        pak_name: filename,
        pak_path: pak_path.to_string_lossy().into_owned(),
        ini_entries: Vec::new(),
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_pak_that_cannot_be_read_is_reported_not_skipped() {
        let root = std::env::temp_dir().join(format!("rivals-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mods = mods_dir(root.to_str().unwrap());
        std::fs::create_dir_all(&mods).expect("create mods dir");
        std::fs::write(mods.join("Broken.pak"), b"not a pak at all").expect("write pak");

        let scan = scan_mod_paks(root.to_str().unwrap(), false).expect("scan");

        assert!(scan.paks.is_empty());
        assert_eq!(scan.unreadable.len(), 1);
        assert_eq!(scan.unreadable[0].pak_name, "Broken.pak");
        assert!(!scan.unreadable[0].error.is_empty());

        let listing = scan_mod_paks_any_ini(root.to_str().unwrap(), false).expect("scan");
        assert_eq!(listing.unreadable.len(), 1);

        let _ = std::fs::remove_dir_all(&root);
    }
}
