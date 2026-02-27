use super::tweaks::{TweakDefinition, TweakKind, TweakSetting, TweakState};

/// Scan INI content and report the current state of each tweak.
pub(crate) fn detect_active_tweaks(
    content: &str,
    catalogue: &[TweakDefinition],
) -> Vec<TweakState> {
    catalogue.iter().map(|t| detect_one(content, t)).collect()
}

fn detect_one(content: &str, tweak: &TweakDefinition) -> TweakState {
    match &tweak.kind {
        TweakKind::RemoveLines { lines } => {
            // Active (fix ON) = NONE of the problematic lines are present.
            let any_found = lines.iter().any(|pattern| {
                content.lines().any(|line| {
                    let t = line.trim();
                    !t.starts_with(';') && matches_pattern(t, pattern)
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
        } => {
            let current = find_key_value(content, key);
            let active = current.as_deref() == Some(on_value.as_str());
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

/// Apply user settings to INI content and return the modified text.
pub(crate) fn apply_tweaks(
    content: &str,
    catalogue: &[TweakDefinition],
    settings: &[TweakSetting],
) -> String {
    let mut lines: Vec<String> = content.lines().map(String::from).collect();

    for setting in settings {
        let Some(tweak) = catalogue.iter().find(|t| t.id == setting.id) else {
            continue;
        };

        match &tweak.kind {
            TweakKind::RemoveLines {
                lines: patterns, ..
            } => {
                if setting.enabled {
                    // Fix ON → remove problematic lines
                    remove_matching_lines(&mut lines, patterns);
                } else {
                    // Fix OFF → re-add problematic lines (if absent)
                    add_lines_if_absent(&mut lines, patterns);
                }
            }
            TweakKind::Toggle {
                key,
                on_value,
                off_value,
            } => {
                let value = if setting.enabled { on_value } else { off_value };
                if !upsert_key_value(&mut lines, key, value) {
                    lines.push(format!("{}={}", key, value));
                }
            }
            TweakKind::Slider { key, .. } => {
                if setting.enabled {
                    let value = setting.value.as_deref().unwrap_or("0");
                    if !upsert_key_value(&mut lines, key, value) {
                        lines.push(format!("{}={}", key, value));
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

// ── Helpers ────────────────────────────────────────────────────────────────

/// Check if a line matches a pattern, also considering `+CVars=` prefix.
fn matches_pattern(trimmed_line: &str, pattern: &str) -> bool {
    let line_lower = trimmed_line.to_ascii_lowercase();
    let pat_lower = pattern.to_ascii_lowercase();
    line_lower == pat_lower || line_lower == format!("+cvars={}", pat_lower)
}

/// Find the first value of a key in the INI content (case-insensitive key).
/// Also checks `+CVars=key=value` lines.
fn find_key_value(content: &str, key: &str) -> Option<String> {
    let key_lower = key.to_ascii_lowercase();
    let prefix = format!("{}=", key_lower);
    let cvars_prefix = format!("+cvars={}=", key_lower);

    for line in content.lines() {
        let t = line.trim();
        if t.starts_with(';') {
            continue;
        }
        let t_lower = t.to_ascii_lowercase();

        if t_lower.starts_with(&prefix) {
            return Some(t[key.len() + 1..].to_string());
        }
        if t_lower.starts_with(&cvars_prefix) {
            return Some(t["+CVars=".len() + key.len() + 1..].to_string());
        }
    }
    None
}

/// Remove all lines that match any of the given patterns (+ CVars forms).
fn remove_matching_lines(lines: &mut Vec<String>, patterns: &[String]) {
    lines.retain(|line| {
        let t = line.trim();
        if t.starts_with(';') {
            return true;
        }
        !patterns.iter().any(|p| matches_pattern(t, p))
    });
}

/// Add lines that aren't already present in the content.
fn add_lines_if_absent(lines: &mut Vec<String>, patterns: &[String]) {
    for pattern in patterns {
        let already = lines.iter().any(|line| {
            let t = line.trim();
            !t.starts_with(';') && matches_pattern(t, pattern)
        });
        if !already {
            lines.push(pattern.clone());
        }
    }
}

/// Update all occurrences of `key=...` to `key=value` (case-insensitive key).
/// Returns `true` if at least one line was updated.
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

/// Remove all non-comment lines matching `key=...` (case-insensitive, + CVars).
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
