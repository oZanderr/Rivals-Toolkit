use super::{PakTweakEdit, PakTweakState};

#[derive(Clone, Copy)]
pub(super) enum IniType {
    Engine,
    DeviceProfiles,
}

/// Apply edits to an INI file's content.
pub(super) fn apply_edits_to_ini(
    content: &str,
    edits: &[PakTweakEdit],
    ini_type: IniType,
) -> String {
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

/// Parse console variables from an INI file.
/// For DefaultEngine.ini, looks for lines under [ConsoleVariables] or bare key=value.
/// For DefaultDeviceProfiles.ini, looks for +CVars= lines in [Windows DeviceProfile].
pub(super) fn parse_console_vars(content: &str, source: &str) -> Vec<PakTweakState> {
    let mut vars = Vec::new();
    let is_device_profiles = source.contains("DeviceProfiles");

    if is_device_profiles {
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

            if let Some(kv) = parse_cvar_line(trimmed) {
                vars.push(PakTweakState {
                    key: kv.0,
                    value: kv.1,
                    source: source.to_string(),
                });
            }
        }
    } else {
        // DefaultEngine.ini: scan ALL sections for key=value pairs.
        //
        // Some keys (e.g. ApplicationScale) live in object-settings sections like
        // [Script/Engine.UserInterfaceSettings] rather than [ConsoleVariables].
        // Reading every section ensures we catch them all regardless of where they sit.
        let mut in_any_section = false;
        for line in content.lines() {
            let trimmed = line.trim();

            if trimmed.starts_with('[') {
                in_any_section = true;
                continue;
            }

            if !in_any_section || trimmed.is_empty() || trimmed.starts_with(';') {
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

/// Remove every non-comment line whose CVar key matches `key_lower` (case-insensitive).
fn remove_cvar_key(lines: &mut Vec<String>, key_lower: &str) {
    lines.retain(|line| {
        let t = line.trim();
        if t.starts_with(';') {
            return true;
        }
        match parse_cvar_line(t) {
            Some((k, _)) => k.to_ascii_lowercase() != key_lower,
            None => true,
        }
    });
}

/// Format a key=value line, optionally preserving a `+CVars=` prefix.
fn format_cvar_line(key: &str, val: &str, preserve_prefix: bool) -> String {
    if preserve_prefix {
        format!("+CVars={}={}", key, val)
    } else {
        format!("{}={}", key, val)
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

/// Apply edits to DefaultDeviceProfiles.ini inside the [Windows DeviceProfile] section.
fn apply_device_profiles_edits(lines: &mut Vec<String>, edits: &[PakTweakEdit]) {
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
        // Don't modify when a section doesn't exist
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
            (Some(val), Some(_)) => {
                // Update ALL occurrences within the section, preserving +CVars= prefix.
                let end_now = section_end.unwrap_or(lines.len());
                for i in (start + 1)..end_now.min(lines.len()) {
                    let t = lines[i].trim().to_string();
                    if t.starts_with(';') || t.is_empty() {
                        continue;
                    }
                    if let Some((k, _)) = parse_cvar_line(&t) {
                        if k.to_ascii_lowercase() == key_lower {
                            let has_prefix = t.to_ascii_lowercase().starts_with("+cvars=");
                            lines[i] = format_cvar_line(&edit.key, val, has_prefix);
                        }
                    }
                }
            }
            (Some(val), None) => {
                // Add new line at end of section (before next section or EOF)
                let insert_at = find_section_insert_point(lines, start);
                lines.insert(insert_at, format_cvar_line(&edit.key, val, true));
            }
            (None, Some(_)) => {
                remove_cvar_key(lines, &key_lower);
            }
            (None, None) => {}
        }
    }
}

/// Apply edits to DefaultEngine.ini.
///
/// For each edit:
/// 1. Search the **entire file** for an existing line with that key (any section).
///    If found, update or remove it in-place — preserving its original section.
/// 2. If the key is not found and we're inserting a value, use the edit's
///    `engine_section` hint to find (or create) the right `[Section]` header,
///    then insert the line there.  Falls back to `[ConsoleVariables]`.
fn apply_engine_edits(lines: &mut Vec<String>, edits: &[PakTweakEdit]) {
    for edit in edits {
        let key_lower = edit.key.to_ascii_lowercase();

        // Find the key anywhere in the file
        let mut in_section = false;
        let mut found_idx: Option<usize> = None;
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') {
                in_section = true;
                continue;
            }
            if !in_section || trimmed.is_empty() || trimmed.starts_with(';') {
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
            // Update all occurrences
            (Some(val), Some(_)) => {
                let new_line = format_cvar_line(&edit.key, val, false);
                for line in lines.iter_mut() {
                    let t = line.trim();
                    if t.starts_with(';') {
                        continue;
                    }
                    if let Some((k, _)) = parse_cvar_line(t) {
                        if k.to_ascii_lowercase() == key_lower {
                            *line = new_line.clone();
                        }
                    }
                }
            }
            // Remove all occurrences
            (None, Some(_)) => {
                remove_cvar_key(lines, &key_lower);
            }
            // Nothing to remove
            (None, None) => {}
            // Insert into the correct section
            (Some(val), None) => {
                let target_header = edit
                    .engine_section
                    .as_deref()
                    .map(|s| format!("[{}]", s))
                    .unwrap_or_else(|| "[ConsoleVariables]".to_string());

                // Find the last occurrence of that section in the file
                let section_start = lines
                    .iter()
                    .rposition(|l| l.trim().eq_ignore_ascii_case(&target_header));

                let section_start = match section_start {
                    Some(idx) => idx,
                    None => {
                        // Section doesn't exist, create it at the end
                        if !lines.last().is_some_and(|l| l.trim().is_empty()) {
                            lines.push(String::new());
                        }
                        lines.push(target_header);
                        lines.len() - 1
                    }
                };

                let insert_at = find_section_insert_point(lines, section_start);
                lines.insert(insert_at, format_cvar_line(&edit.key, val, false));
            }
        }
    }
}
