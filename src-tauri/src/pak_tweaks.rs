use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::BufReader,
    path::{Path, PathBuf},
};
use walkdir::WalkDir;

use crate::pak::crypto::{make_aes_key, open_pak};

/// Describes which INI files exist inside a given pak mod.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PakIniInfo {
    /// Display name of the pak file (just the filename).
    pub pak_name: String,
    /// Full path to the pak file.
    pub pak_path: String,
    /// Whether DefaultDeviceProfiles.ini exists inside the pak.
    pub has_device_profiles: bool,
    /// Whether DefaultEngine.ini exists inside the pak.
    pub has_engine_ini: bool,
    /// The entry path for DefaultDeviceProfiles.ini inside the pak (if present).
    pub device_profiles_entry: Option<String>,
    /// The entry path for DefaultEngine.ini inside the pak (if present).
    pub engine_ini_entry: Option<String>,
}

/// The current console variable values detected inside a pak mod's INI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PakTweakState {
    pub key: String,
    pub value: String,
    /// Which file this came from.
    pub source: String,
}

/// A console variable to set or remove inside a pak mod's INI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PakTweakEdit {
    pub key: String,
    /// `None` means remove the key.
    pub value: Option<String>,
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
            Ok(None) => {} // No INI files, skip
            Err(_) => {}   // Can't read pak, skip silently
        }
    }
    results.sort_by(|a, b| a.pak_name.cmp(&b.pak_name));
    Ok(results)
}

/// Read current console variables from a pak mod's INI files.
pub(crate) fn read_pak_tweaks(pak_path: &str) -> Result<Vec<PakTweakState>, String> {
    let pak_path = Path::new(pak_path);
    let info = inspect_pak_for_ini(pak_path)?
        .ok_or_else(|| "No INI config files found in this pak.".to_string())?;

    // DeviceProfiles takes priority
    let (entry, source) = if let Some(ref dp) = info.device_profiles_entry {
        (dp.clone(), "DefaultDeviceProfiles.ini")
    } else if let Some(ref eng) = info.engine_ini_entry {
        (eng.clone(), "DefaultEngine.ini")
    } else {
        return Ok(Vec::new());
    };

    let content = extract_file_to_string(pak_path, &entry)?;
    let vars = parse_console_vars(&content, source);
    Ok(vars)
}

/// Apply tweaks to a pak mod: extract → edit INI → repack in place.
pub(crate) fn apply_pak_tweaks(
    pak_path: &str,
    edits: &[PakTweakEdit],
) -> Result<String, String> {
    let pak = Path::new(pak_path);
    if !pak.exists() {
        return Err(format!("Pak file not found: {}", pak_path));
    }

    let info = inspect_pak_for_ini(pak)?
        .ok_or_else(|| "No INI config files found in this pak.".to_string())?;

    // Determine temp directory next to the pak
    let stem = pak
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let temp_dir = pak
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(".{}_temp", stem));

    // Clean up any previous temp
    let _ = fs::remove_dir_all(&temp_dir);

    // 1. Extract entire pak to temp dir
    unpack_to_dir(pak, &temp_dir)?;

    // 2. Determine which INI file to edit (DeviceProfiles takes priority)
    let (entry, ini_type) = if info.has_device_profiles {
        (
            info.device_profiles_entry.as_ref().unwrap(),
            IniType::DeviceProfiles,
        )
    } else {
        (
            info.engine_ini_entry.as_ref().unwrap(),
            IniType::Engine,
        )
    };

    // Convert pak entry path to filesystem path inside temp dir
    let rel = entry
        .trim_start_matches("../../../")
        .trim_start_matches('/');
    let ini_file = temp_dir.join(rel);

    // 3. Read, modify, write the INI
    let content = fs::read_to_string(&ini_file).map_err(|e| {
        format!(
            "Failed to read extracted INI {}: {}",
            ini_file.display(),
            e
        )
    })?;

    let modified = apply_edits_to_ini(&content, edits, ini_type);
    fs::write(&ini_file, &modified).map_err(|e| {
        format!(
            "Failed to write modified INI {}: {}",
            ini_file.display(),
            e
        )
    })?;

    // 4. Repack to a temp pak, then replace the original
    let temp_pak = pak
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(".{}_repacked.pak", stem));

    repack_dir_to_pak(&temp_dir, &temp_pak)?;

    // 5. Replace original with repacked
    fs::remove_file(pak).map_err(|e| format!("Failed to remove original pak: {}", e))?;
    fs::rename(&temp_pak, pak)
        .map_err(|e| format!("Failed to replace pak with repacked version: {}", e))?;

    // 6. Cleanup
    let _ = fs::remove_dir_all(&temp_dir);

    Ok(format!(
        "Applied {} edit(s) to {}",
        edits.len(),
        info.pak_name
    ))
}

// ── Internal Helpers ───────────────────────────────────────────────────────

#[derive(Clone, Copy)]
enum IniType {
    Engine,
    DeviceProfiles,
}

fn mods_dir(game_root: &str) -> PathBuf {
    PathBuf::from(game_root).join("MarvelGame\\Marvel\\Content\\Paks\\~mods")
}

/// Inspect a pak to see if it contains DefaultEngine.ini or DefaultDeviceProfiles.ini.
fn inspect_pak_for_ini(pak_path: &Path) -> Result<Option<PakIniInfo>, String> {
    let pak = open_pak(pak_path)?;
    let files = pak.files();

    let mut device_profiles_entry = None;
    let mut engine_ini_entry = None;

    for f in &files {
        let lower = f.to_ascii_lowercase();
        if lower.ends_with("defaultdeviceprofiles.ini") {
            device_profiles_entry = Some(f.clone());
        } else if lower.ends_with("defaultengine.ini") {
            engine_ini_entry = Some(f.clone());
        }
    }

    if device_profiles_entry.is_none() && engine_ini_entry.is_none() {
        return Ok(None);
    }

    let pak_name = pak_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();

    Ok(Some(PakIniInfo {
        pak_name,
        pak_path: pak_path.to_string_lossy().into_owned(),
        has_device_profiles: device_profiles_entry.is_some(),
        has_engine_ini: engine_ini_entry.is_some(),
        device_profiles_entry,
        engine_ini_entry,
    }))
}

/// Extract a single file from a pak to a string.
fn extract_file_to_string(pak_path: &Path, entry: &str) -> Result<String, String> {
    let pak = open_pak(pak_path)?;
    let mut reader = BufReader::new(fs::File::open(pak_path).map_err(|e| e.to_string())?);
    let mut buf = Vec::new();
    pak.read_file(entry, &mut reader, &mut buf)
        .map_err(|e| e.to_string())?;
    String::from_utf8(buf).map_err(|e| format!("INI file is not valid UTF-8: {}", e))
}

/// Extract all files from a pak into a directory.
fn unpack_to_dir(pak_path: &Path, output_dir: &Path) -> Result<(), String> {
    fs::create_dir_all(output_dir).map_err(|e| e.to_string())?;

    let pak = open_pak(pak_path)?;
    let files = pak.files();

    for name in &files {
        let stripped = name.trim_start_matches("../../../").trim_start_matches('/');
        let dest = output_dir.join(stripped);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut reader = BufReader::new(fs::File::open(pak_path).map_err(|e| e.to_string())?);
        let mut out = fs::File::create(&dest).map_err(|e| e.to_string())?;
        pak.read_file(name, &mut reader, &mut out)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Repack a directory into a pak file using the same settings as the writer module.
fn repack_dir_to_pak(input_dir: &Path, output_pak: &Path) -> Result<(), String> {
    use std::io::BufWriter;

    if let Some(parent) = output_pak.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let out_file = fs::File::create(output_pak).map_err(|e| e.to_string())?;
    let mut pak_writer = repak::PakBuilder::new()
        .key(make_aes_key()?)
        .compression([repak::Compression::Oodle])
        .writer(
            BufWriter::new(out_file),
            repak::Version::V11,
            "../../../".to_string(),
            None,
        );

    for entry in WalkDir::new(input_dir).into_iter().flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let rel = path
            .strip_prefix(input_dir)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        pak_writer
            .write_file(&rel, true, fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    }

    pak_writer.write_index().map_err(|e| e.to_string())?;
    Ok(())
}

/// Parse console variables from an INI file.
/// For DefaultEngine.ini, looks for lines under [ConsoleVariables] or bare key=value.
/// For DefaultDeviceProfiles.ini, looks for +CVars= lines in [Windows DeviceProfile].
fn parse_console_vars(content: &str, source: &str) -> Vec<PakTweakState> {
    let mut vars = Vec::new();
    let is_device_profiles = source.contains("DeviceProfiles");

    if is_device_profiles {
        // Only parse from [Windows DeviceProfile] section
        let mut in_section = false;
        for line in content.lines() {
            let trimmed = line.trim();

            if trimmed.starts_with('[') {
                in_section = is_windows_device_profile_header(trimmed);
                continue;
            }

            if !in_section || trimmed.is_empty() || trimmed.starts_with(';') {
                continue;
            }

            // Lines look like: +CVars=r.CustomDepth=0 or key=value
            if let Some(kv) = parse_cvar_line(trimmed) {
                vars.push(PakTweakState {
                    key: kv.0,
                    value: kv.1,
                    source: source.to_string(),
                });
            }
        }
    } else {
        // DefaultEngine.ini: parse [ConsoleVariables] section
        let mut in_section = false;
        for line in content.lines() {
            let trimmed = line.trim();

            if trimmed.starts_with('[') {
                in_section = trimmed.eq_ignore_ascii_case("[ConsoleVariables]");
                continue;
            }

            if !in_section || trimmed.is_empty() || trimmed.starts_with(';') {
                continue;
            }

            if let Some(kv) = parse_cvar_line(trimmed) {
                vars.push(PakTweakState {
                    key: kv.0,
                    value: kv.1,
                    source: source.to_string(),
                });
            }
        }
    }
    vars
}

/// Parse a single CVar line, handling optional +CVars= prefix.
fn parse_cvar_line(line: &str) -> Option<(String, String)> {
    let inner = if line.to_ascii_lowercase().starts_with("+cvars=") {
        &line["+CVars=".len()..]
    } else {
        line
    };

    let (key, value) = inner.split_once('=')?;
    let key = key.trim();
    let value = value.trim();
    if key.is_empty() {
        return None;
    }
    Some((key.to_string(), value.to_string()))
}

/// Check if a section header is the Windows DeviceProfile section.
fn is_windows_device_profile_header(header: &str) -> bool {
    header.trim().eq_ignore_ascii_case("[Windows DeviceProfile]")
}

/// Apply edits to an INI file's content.
fn apply_edits_to_ini(content: &str, edits: &[PakTweakEdit], ini_type: IniType) -> String {
    let mut lines: Vec<String> = content.lines().map(String::from).collect();

    match ini_type {
        IniType::DeviceProfiles => {
            apply_device_profiles_edits(&mut lines, edits);
        }
        IniType::Engine => {
            apply_engine_edits(&mut lines, edits);
        }
    }

    let mut result = lines.join("\r\n");
    if !result.ends_with("\r\n") {
        result.push_str("\r\n");
    }
    result
}

/// Apply edits to DefaultDeviceProfiles.ini inside the [Windows DeviceProfile] section.
fn apply_device_profiles_edits(lines: &mut Vec<String>, edits: &[PakTweakEdit]) {
    // Find the [Windows DeviceProfile] section bounds
    let mut section_start: Option<usize> = None;
    let mut section_end: Option<usize> = None;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if is_windows_device_profile_header(trimmed) {
            section_start = Some(i);
        } else if trimmed.starts_with('[') && section_start.is_some() && section_end.is_none() {
            section_end = Some(i);
        }
    }

    let Some(start) = section_start else {
        // Section doesn't exist — this shouldn't happen for a valid DeviceProfiles pak,
        // but handle gracefully by not modifying.
        return;
    };
    let end = section_end.unwrap_or(lines.len());

    for edit in edits {
        let key_lower = edit.key.to_ascii_lowercase();

        // Find existing line for this key within the section
        let mut found_idx = None;
        for i in (start + 1)..end.min(lines.len()) {
            let trimmed = lines[i].trim();
            if trimmed.starts_with('[') {
                break;
            }
            if trimmed.is_empty() || trimmed.starts_with(';') {
                continue;
            }
            if let Some((k, _)) = parse_cvar_line(trimmed) {
                if k.to_ascii_lowercase() == key_lower {
                    found_idx = Some(i);
                    break;
                }
            }
        }

        match (&edit.value, found_idx) {
            (Some(val), Some(idx)) => {
                // Update existing line, preserving +CVars= prefix if present
                let old = lines[idx].trim().to_string();
                if old.to_ascii_lowercase().starts_with("+cvars=") {
                    lines[idx] = format!("+CVars={}={}", edit.key, val);
                } else {
                    lines[idx] = format!("{}={}", edit.key, val);
                }
            }
            (Some(val), None) => {
                // Add new line at end of section (before next section or EOF)
                let insert_at = find_section_insert_point(lines, start);
                lines.insert(insert_at, format!("+CVars={}={}", edit.key, val));
            }
            (None, Some(idx)) => {
                // Remove existing line
                lines.remove(idx);
            }
            (None, None) => {
                // Nothing to remove
            }
        }
    }
}

/// Apply edits to DefaultEngine.ini under [ConsoleVariables].
fn apply_engine_edits(lines: &mut Vec<String>, edits: &[PakTweakEdit]) {
    // Find [ConsoleVariables] section
    let mut section_start: Option<usize> = None;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.eq_ignore_ascii_case("[ConsoleVariables]") {
            section_start = Some(i);
            break;
        }
    }

    // If [ConsoleVariables] doesn't exist, create it at the end
    if section_start.is_none() {
        // Check if there are edits that need adding (value = Some)
        let has_additions = edits.iter().any(|e| e.value.is_some());
        if !has_additions {
            return; // Nothing to add, nothing to remove, nothing to do
        }
        // Add blank line + section header at the end
        if !lines.last().is_some_and(|l| l.trim().is_empty()) {
            lines.push(String::new());
        }
        lines.push("[ConsoleVariables]".to_string());
        section_start = Some(lines.len() - 1);
    }

    let start = section_start.unwrap();

    for edit in edits {
        let key_lower = edit.key.to_ascii_lowercase();

        // Recalculate section end since lines may have shifted
        let current_end = find_section_end(lines, start);

        // Find existing line
        let mut found_idx = None;
        for i in (start + 1)..current_end {
            let trimmed = lines[i].trim();
            if trimmed.starts_with('[') {
                break;
            }
            if trimmed.is_empty() || trimmed.starts_with(';') {
                continue;
            }
            if let Some((k, _)) = parse_cvar_line(trimmed) {
                if k.to_ascii_lowercase() == key_lower {
                    found_idx = Some(i);
                    break;
                }
            }
        }

        match (&edit.value, found_idx) {
            (Some(val), Some(idx)) => {
                lines[idx] = format!("{}={}", edit.key, val);
            }
            (Some(val), None) => {
                let insert_at = find_section_insert_point(lines, start);
                lines.insert(insert_at, format!("{}={}", edit.key, val));
            }
            (None, Some(idx)) => {
                lines.remove(idx);
            }
            (None, None) => {}
        }
    }
}

/// Find the end of a section (next `[` header or EOF).
fn find_section_end(lines: &[String], section_start: usize) -> usize {
    for i in (section_start + 1)..lines.len() {
        if lines[i].trim().starts_with('[') {
            return i;
        }
    }
    lines.len()
}

/// Find the best insert point for a new line inside a section
/// (after the last non-empty content line, before the next section or EOF).
fn find_section_insert_point(lines: &[String], section_start: usize) -> usize {
    let end = find_section_end(lines, section_start);
    // Insert before trailing blank lines at end of section
    let mut insert = end;
    while insert > section_start + 1 && lines[insert - 1].trim().is_empty() {
        insert -= 1;
    }
    insert
}
