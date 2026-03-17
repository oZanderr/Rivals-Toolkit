use super::tweaks::{TweakDefinition, TweakKind, TweakSetting, TweakState};

/// Detect the current state of each tweak from INI content.
pub(crate) fn detect_active_tweaks(
    content: &str,
    catalogue: &[TweakDefinition],
) -> Vec<TweakState> {
    catalogue.iter().map(|t| detect_one(content, t)).collect()
}

fn detect_one(content: &str, tweak: &TweakDefinition) -> TweakState {
    match &tweak.kind {
        TweakKind::RemoveLines { lines } => {
            let any_found = lines.iter().any(|entry| {
                content.lines().any(|line| {
                    let t = line.trim();
                    !t.starts_with(';') && matches_pattern(t, &entry.pattern)
                })
            });
            TweakState {
                id: tweak.id.clone(),
                active: !any_found,
                current_value: None,
            }
        }
        TweakKind::Toggle {
            key,
            on_value,
            off_value: _,
            default_enabled,
            section: _,
        } => {
            let current = find_key_value(content, key);
            let active = match current.as_deref() {
                Some(v) => v == on_value.as_str(),
                None => *default_enabled,
            };
            TweakState {
                id: tweak.id.clone(),
                active,
                current_value: current,
            }
        }
        TweakKind::Slider { key, .. } => {
            let current = find_key_value(content, key);
            TweakState {
                id: tweak.id.clone(),
                active: current.is_some(),
                current_value: current,
            }
        }
    }
}

/// Apply tweak settings to INI content and return the modified text.
pub(crate) fn apply_tweaks(
    content: &str,
    catalogue: &[TweakDefinition],
    settings: &[TweakSetting],
) -> String {
    let mut lines: Vec<String> = if content.trim().is_empty() {
        vec!["[ScalabilitySettings]".to_string()]
    } else {
        content.lines().map(String::from).collect()
    };

    for setting in settings {
        let Some(tweak) = catalogue.iter().find(|t| t.id == setting.id) else {
            continue;
        };

        match &tweak.kind {
            TweakKind::RemoveLines { lines: entries } => {
                if setting.enabled {
                    remove_matching_lines(&mut lines, entries);
                } else {
                    add_lines_if_absent(&mut lines, entries);
                }
            }
            TweakKind::Toggle {
                key,
                on_value,
                off_value,
                default_enabled,
                section,
            } => {
                let key_in_file = find_key_value(content, key).is_some();
                if setting.enabled {
                    if key_in_file {
                        upsert_key_value(&mut lines, key, on_value);
                    } else if setting.enabled != *default_enabled {
                        insert_into_section(&mut lines, section, format!("{}={}", key, on_value));
                    }
                } else {
                    match off_value {
                        Some(v) => {
                            if key_in_file {
                                upsert_key_value(&mut lines, key, v);
                            } else if setting.enabled != *default_enabled {
                                insert_into_section(&mut lines, section, format!("{}={}", key, v));
                            }
                        }
                        None => remove_key(&mut lines, key),
                    }
                }
            }
            TweakKind::Slider { key, section, .. } => {
                if setting.enabled {
                    let value = setting.value.as_deref().unwrap_or("0");
                    if !upsert_key_value(&mut lines, key, value) {
                        insert_into_section(&mut lines, section, format!("{}={}", key, value));
                    }
                } else {
                    remove_key(&mut lines, key);
                }
            }
        }
    }

    let mut result = lines.join("\r\n");
    if !result.ends_with("\r\n") {
        result.push_str("\r\n");
    }
    result
}

/// Check whether a line matches a pattern, including `+CVars=` form.
fn matches_pattern(trimmed_line: &str, pattern: &str) -> bool {
    let line_lower = trimmed_line.to_ascii_lowercase();
    let pat_lower = pattern.to_ascii_lowercase();
    line_lower == pat_lower || line_lower == format!("+cvars={}", pat_lower)
}

/// Find the last value of `key`, including `+CVars=key=value` lines.
fn find_key_value(content: &str, key: &str) -> Option<String> {
    let key_lower = key.to_ascii_lowercase();
    let prefix = format!("{}=", key_lower);
    let cvars_prefix = format!("+cvars={}=", key_lower);
    let mut found: Option<String> = None;

    for line in content.lines() {
        let t = line.trim();
        if t.starts_with(';') {
            continue;
        }
        let t_lower = t.to_ascii_lowercase();

        if t_lower.starts_with(&prefix) {
            found = Some(t[key.len() + 1..].to_string());
        }
        if t_lower.starts_with(&cvars_prefix) {
            found = Some(t["+CVars=".len() + key.len() + 1..].to_string());
        }
    }
    found
}

/// Remove all non-comment lines matching any given pattern.
fn remove_matching_lines(lines: &mut Vec<String>, entries: &[super::tweaks::ScalabilityLine]) {
    lines.retain(|line| {
        let t = line.trim();
        if t.starts_with(';') {
            return true;
        }
        !entries.iter().any(|e| matches_pattern(t, &e.pattern))
    });
}

/// Add missing lines under their target sections.
/// `pak_only` entries are ignored for scalability files.
fn add_lines_if_absent(lines: &mut Vec<String>, entries: &[super::tweaks::ScalabilityLine]) {
    for entry in entries {
        if entry.pak_only {
            continue;
        }
        let already = lines.iter().any(|line| {
            let t = line.trim();
            !t.starts_with(';') && matches_pattern(t, &entry.pattern)
        });
        if !already {
            insert_into_section(lines, &entry.section, entry.pattern.clone());
        }
    }
}

/// Insert `new_line` into `[section]`, creating the section when missing.
fn insert_into_section(lines: &mut Vec<String>, section: &str, new_line: String) {
    let header = format!("[{}]", section);
    let header_lower = header.to_ascii_lowercase();

    let section_start = lines
        .iter()
        .position(|l| l.trim().to_ascii_lowercase() == header_lower);

    let insert_at = if let Some(start) = section_start {
        let end = lines[start + 1..]
            .iter()
            .position(|l| {
                let t = l.trim();
                t.starts_with('[') && t.ends_with(']')
            })
            .map(|rel| start + 1 + rel)
            .unwrap_or(lines.len());
        let mut pos = end;
        while pos > start + 1 && lines[pos - 1].trim().is_empty() {
            pos -= 1;
        }
        pos
    } else {
        if lines.last().map(|l| !l.trim().is_empty()).unwrap_or(false) {
            lines.push(String::new());
        }
        lines.push(header);
        lines.len()
    };

    lines.insert(insert_at, new_line);
}

/// Update all occurrences of `key` and return whether any line changed.
fn upsert_key_value(lines: &mut [String], key: &str, value: &str) -> bool {
    let key_lower = key.to_ascii_lowercase();
    let prefix = format!("{}=", key_lower);
    let cvars_prefix = format!("+cvars={}=", key_lower);

    let mut found = false;
    for line in lines.iter_mut() {
        let t = line.trim();
        if t.starts_with(';') {
            continue;
        }
        let t_lower = t.to_ascii_lowercase();

        if t_lower.starts_with(&prefix) {
            *line = format!("{}={}", key, value);
            found = true;
        } else if t_lower.starts_with(&cvars_prefix) {
            *line = format!("+CVars={}={}", key, value);
            found = true;
        }
    }
    found
}

/// Remove all non-comment lines matching `key`, including `+CVars=` form.
fn remove_key(lines: &mut Vec<String>, key: &str) {
    let key_lower = key.to_ascii_lowercase();
    let prefix = format!("{}=", key_lower);
    let cvars_prefix = format!("+cvars={}=", key_lower);

    lines.retain(|line| {
        let t = line.trim();
        if t.starts_with(';') {
            return true;
        }
        let t_lower = t.to_ascii_lowercase();
        !t_lower.starts_with(&prefix) && !t_lower.starts_with(&cvars_prefix)
    });
}
