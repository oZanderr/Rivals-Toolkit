//! Pak file I/O primitives plus the `with_unpacked_pak` crash-safe lifecycle wrapper.

use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::pak::plain::{self, PakOptions};
use crate::pak::profile::strip_mount_prefix;

use super::{PakIniInfo, PakIniListing, PakIniTarget};

/// List every `.ini` entry inside a pak; `None` if none present.
pub(super) fn inspect_pak_for_any_ini(pak_path: &Path) -> Result<Option<PakIniListing>, String> {
    let mut entries: Vec<String> = plain::list_files(pak_path)?
        .into_iter()
        .filter(|f| f.to_ascii_lowercase().ends_with(".ini"))
        .collect();
    if entries.is_empty() {
        return Ok(None);
    }
    entries.sort();

    let pak_name = pak_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();

    Ok(Some(PakIniListing {
        pak_name,
        pak_path: pak_path.to_string_lossy().into_owned(),
        ini_entries: entries,
    }))
}

/// Which config layer a pak entry belongs to, or `None` when the tweak engine does not
/// understand the file.
pub(super) fn classify_ini_entry(entry: &str) -> Option<PakIniTarget> {
    let lower = entry.to_ascii_lowercase();
    if lower.ends_with("basedeviceprofiles.ini") {
        Some(PakIniTarget::BaseDeviceProfiles)
    } else if lower.ends_with("defaultdeviceprofiles.ini") {
        Some(PakIniTarget::DeviceProfiles)
    } else if lower.ends_with("defaultengine.ini") {
        Some(PakIniTarget::Engine)
    } else if lower.ends_with("windowsengine.ini") {
        Some(PakIniTarget::WindowsEngine)
    } else if lower.ends_with("baseengine.ini") {
        Some(PakIniTarget::BaseEngine)
    } else {
        None
    }
}

/// Order two files that share a layer, matching the order UE loads them in: the `Base*`
/// variant first, then the project copy that overrides it.
fn load_order_key(entry: &str) -> (u8, String) {
    let lower = entry.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower).to_string();
    (u8::from(!name.starts_with("base")), lower)
}

/// Inspect a pak for tweakable INI entries.
pub(super) fn inspect_pak_for_ini(pak_path: &Path) -> Result<Option<PakIniInfo>, String> {
    let mut buckets: [Vec<String>; 5] = Default::default();
    for f in plain::list_files(pak_path)? {
        if let Some(target) = classify_ini_entry(&f) {
            let slot = PakIniTarget::ALL
                .iter()
                .position(|t| *t == target)
                .unwrap_or_default();
            buckets[slot].push(f);
        }
    }
    for bucket in &mut buckets {
        bucket.sort_by_key(|entry| load_order_key(entry));
    }
    let [
        base_engine_entries,
        engine_ini_entries,
        windows_engine_entries,
        base_device_profiles_entries,
        device_profiles_entries,
    ] = buckets;

    let pak_name = pak_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();

    let info = PakIniInfo {
        pak_name,
        pak_path: pak_path.to_string_lossy().into_owned(),
        has_device_profiles: !device_profiles_entries.is_empty(),
        has_base_device_profiles: !base_device_profiles_entries.is_empty(),
        has_engine_ini: !engine_ini_entries.is_empty(),
        has_base_engine: !base_engine_entries.is_empty(),
        has_windows_engine: !windows_engine_entries.is_empty(),
        device_profiles_entries,
        base_device_profiles_entries,
        engine_ini_entries,
        base_engine_entries,
        windows_engine_entries,
    };

    if info.is_empty() {
        return Ok(None);
    }
    Ok(Some(info))
}

/// Extract one pak entry, by its path from the game root, to a UTF-8 string.
pub(super) fn extract_file_to_string(pak_path: &Path, entry: &str) -> Result<String, String> {
    let buf = plain::read_one(pak_path, entry)?
        .ok_or_else(|| format!("{entry} is not in {}", pak_path.display()))?;
    String::from_utf8(buf).map_err(|e| format!("INI file is not valid UTF-8: {}", e))
}

/// Open a pak once and find an entry whose path matches `in_pak_path` once the
/// mount prefix is normalized away on both sides. Different paks in the
/// ecosystem store the same logical path with or without the `../../../` mount
/// prefix; comparing after stripping handles either form.
pub(super) fn extract_optional_entry(
    pak_path: &Path,
    in_pak_path: &str,
) -> Result<Option<String>, String> {
    if !pak_path.exists() {
        return Ok(None);
    }
    let Some(buf) = plain::read_one(pak_path, &strip_mount_prefix(in_pak_path))? else {
        return Ok(None);
    };
    let content =
        String::from_utf8(buf).map_err(|e| format!("INI file is not valid UTF-8: {}", e))?;
    Ok(Some(content))
}

/// Extract all pak entries to a directory.
pub(crate) fn unpack_to_dir(pak_path: &Path, output_dir: &Path) -> Result<(), String> {
    plain::extract(pak_path, output_dir, &|_| true).map(|_| ())
}

/// Write a brand-new empty pak (no entries) at `output_pak`. Used by the
/// "New pak" flow so users can populate INI files via the editor instead of
/// staging a folder layout by hand first.
pub(crate) fn create_empty_pak(output_pak: &Path) -> Result<(), String> {
    plain::write_files(output_pak, Vec::new(), &PakOptions::default())
}

/// Repack a directory into a pak file.
fn repack_dir_to_pak(input_dir: &Path, output_pak: &Path) -> Result<(), String> {
    plain::repack_dir(input_dir, output_pak, &PakOptions::default())
}

struct TempDirGuard {
    path: PathBuf,
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Unpack `pak_path` into a sibling temp directory, invoke `modify` against that
/// directory, repack into a sibling temp pak, then atomically swap it in.
///
/// `modify` receives the temp directory root and returns any error to abort the
/// operation before touching the original pak. On swap failure the original is
/// restored from the `.bak` backup.
pub(crate) fn with_unpacked_pak<F>(pak_path: &Path, modify: F) -> Result<(), String>
where
    F: FnOnce(&Path) -> Result<(), String>,
{
    if !pak_path.exists() {
        return Err(format!("Pak file not found: {}", pak_path.display()));
    }

    let stem = pak_path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let parent = pak_path.parent().unwrap_or_else(|| Path::new("."));

    let temp_dir = parent.join(format!(".{}_temp", stem));
    let _ = fs::remove_dir_all(&temp_dir);
    let _guard = TempDirGuard {
        path: temp_dir.clone(),
    };

    unpack_to_dir(pak_path, &temp_dir)?;
    modify(&temp_dir)?;

    let temp_pak = parent.join(format!(".{}_repacked.pak", stem));
    repack_dir_to_pak(&temp_dir, &temp_pak)?;

    let backup = parent.join(format!(".{}.bak", stem));
    fs::rename(pak_path, &backup).map_err(|e| format!("Failed to back up original pak: {}", e))?;
    if let Err(e) = fs::rename(&temp_pak, pak_path) {
        let _ = fs::rename(&backup, pak_path);
        return Err(format!(
            "Failed to replace pak with repacked version: {}",
            e
        ));
    }
    let _ = fs::remove_file(&backup);

    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Every INI a Marvel Rivals config pak can ship has to reach a layer. `BaseDeviceProfiles.ini`
    /// used to fall through and stayed untouched while the toggle reported the fix as applied.
    #[test]
    fn every_shipped_config_file_maps_to_a_layer() {
        let cases = [
            (
                "Engine/Config/BaseDeviceProfiles.ini",
                Some(PakIniTarget::BaseDeviceProfiles),
            ),
            (
                "Marvel/Config/DefaultDeviceProfiles.ini",
                Some(PakIniTarget::DeviceProfiles),
            ),
            (
                "Engine/Config/BaseEngine.ini",
                Some(PakIniTarget::BaseEngine),
            ),
            (
                "Marvel/Config/DefaultEngine.ini",
                Some(PakIniTarget::Engine),
            ),
            (
                "Marvel/Config/Windows/WindowsEngine.ini",
                Some(PakIniTarget::WindowsEngine),
            ),
            (
                "Engine/Config/Windows/BaseWindowsEngine.ini",
                Some(PakIniTarget::WindowsEngine),
            ),
            (
                "../../../Marvel/Config/DefaultEngine.ini",
                Some(PakIniTarget::Engine),
            ),
            (
                "MARVEL/CONFIG/DEFAULTENGINE.INI",
                Some(PakIniTarget::Engine),
            ),
            ("Marvel/Config/DefaultGame.ini", None),
            ("Marvel/Config/Windows/WindowsGame.ini", None),
        ];
        for (entry, expected) in cases {
            assert_eq!(classify_ini_entry(entry), expected, "classifying {entry}");
        }
    }

    /// Two files can share a layer, and the project copy overrides the `Base*` variant, so the
    /// merged read has to see them in that order.
    #[test]
    fn base_variants_sort_before_the_project_copy() {
        let mut entries = vec![
            "Marvel/Config/Windows/WindowsEngine.ini".to_string(),
            "Engine/Config/Windows/BaseWindowsEngine.ini".to_string(),
        ];
        entries.sort_by_key(|entry| load_order_key(entry));
        assert_eq!(
            entries,
            vec![
                "Engine/Config/Windows/BaseWindowsEngine.ini".to_string(),
                "Marvel/Config/Windows/WindowsEngine.ini".to_string(),
            ]
        );
    }
}
