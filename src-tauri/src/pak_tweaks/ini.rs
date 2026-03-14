use super::{PakTweakEdit, PakTweakState};

#[derive(Clone, Copy)]
pub(super) enum IniType {
    Engine,
    DeviceProfiles,
}

/// Apply edits to INI content.
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

/// Parse CVar key/value lines from Engine or DeviceProfiles INI content.
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
        // Engine.ini keys can be outside [ConsoleVariables], so scan all sections.
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

/// Parse one CVar line, supporting optional `+CVars=` prefix.
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

/// Check whether a section header is `[Windows DeviceProfile]`.
fn is_windows_device_profile_header(header: &str) -> bool {
    header
        .trim()
        .eq_ignore_ascii_case("[Windows DeviceProfile]")
}

/// Remove non-comment CVar lines whose key matches `key_lower`.
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

/// Format a CVar assignment line.
fn format_cvar_line(key: &str, val: &str, preserve_prefix: bool) -> String {
    if preserve_prefix {
        format!("+CVars={}={}", key, val)
    } else {
        format!("{}={}", key, val)
    }
}

/// Find the end of a section (next header or EOF).
fn find_section_end(lines: &[String], section_start: usize) -> usize {
    for (i, line) in lines.iter().enumerate().skip(section_start + 1) {
        if line.trim().starts_with('[') {
            return i;
        }
    }
    lines.len()
}

/// Find an insert point near the end of a section, before trailing blank lines.
fn find_section_insert_point(lines: &[String], section_start: usize) -> usize {
    let end = find_section_end(lines, section_start);
    let mut insert = end;
    while insert > section_start + 1 && lines[insert - 1].trim().is_empty() {
        insert -= 1;
    }
    insert
}

/// Apply edits inside the `[Windows DeviceProfile]` section.
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
        return;
    };
    let end = section_end.unwrap_or(lines.len());

    for edit in edits {
        let key_lower = edit.key.to_ascii_lowercase();

        let mut found_idx = None;
        for (i, line) in lines
            .iter()
            .enumerate()
            .take(end.min(lines.len()))
            .skip(start + 1)
        {
            let trimmed = line.trim();
            if trimmed.starts_with('[') {
                break;
            }
            if trimmed.is_empty() || trimmed.starts_with(';') {
                continue;
            }
            if let Some((k, _)) = parse_cvar_line(trimmed)
                && k.to_ascii_lowercase() == key_lower
            {
                found_idx = Some(i);
                break;
            }
        }

        match (&edit.value, found_idx) {
            (Some(val), Some(_)) => {
                let end_now = section_end.unwrap_or(lines.len());
                for i in (start + 1)..end_now.min(lines.len()) {
                    let t = lines[i].trim().to_string();
                    if t.starts_with(';') || t.is_empty() {
                        continue;
                    }
                    if let Some((k, _)) = parse_cvar_line(&t)
                        && k.to_ascii_lowercase() == key_lower
                    {
                        let has_prefix = t.to_ascii_lowercase().starts_with("+cvars=");
                        lines[i] = format_cvar_line(&edit.key, val, has_prefix);
                    }
                }
            }
            (Some(val), None) => {
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

/// Apply edits to Engine.ini.
///
/// Existing keys are updated in place. New keys are inserted into `engine_section`
/// when provided, otherwise into `[ConsoleVariables]`.
fn apply_engine_edits(lines: &mut Vec<String>, edits: &[PakTweakEdit]) {
    for edit in edits {
        let key_lower = edit.key.to_ascii_lowercase();

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
            if let Some((k, _)) = parse_cvar_line(trimmed)
                && k.to_ascii_lowercase() == key_lower
            {
                found_idx = Some(i);
                break;
            }
        }

        match (&edit.value, found_idx) {
            (Some(val), Some(_)) => {
                let new_line = format_cvar_line(&edit.key, val, false);
                for line in lines.iter_mut() {
                    let t = line.trim();
                    if t.starts_with(';') {
                        continue;
                    }
                    if let Some((k, _)) = parse_cvar_line(t)
                        && k.to_ascii_lowercase() == key_lower
                    {
                        *line = new_line.clone();
                    }
                }
            }
            (None, Some(_)) => {
                remove_cvar_key(lines, &key_lower);
            }
            (None, None) => {}
            (Some(val), None) => {
                let target_header = edit
                    .engine_section
                    .as_deref()
                    .map(|s| format!("[{}]", s))
                    .unwrap_or_else(|| "[ConsoleVariables]".to_string());

                let section_start = lines
                    .iter()
                    .rposition(|l| l.trim().eq_ignore_ascii_case(&target_header));

                let section_start = match section_start {
                    Some(idx) => idx,
                    None => {
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
