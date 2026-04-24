//! Apply tweak settings to Scalability.ini content, returning the modified text.

use crate::tweaks::parser::{
    find_key_value, key_present_in_section, matches_key, pattern_key_lower,
};
use crate::tweaks::{TweakDefinition, TweakKind, TweakLine, TweakSetting};

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
            TweakKind::RemoveLines {
                lines: entries,
                remove_only,
            } => {
                if setting.enabled {
                    remove_matching_lines(&mut lines, entries);
                } else if !remove_only {
                    add_lines_if_absent(&mut lines, entries);
                }
            }
            TweakKind::Toggle {
                key,
                on_value,
                off_value,
                default_enabled,
                scalability_section,
                ..
            } => {
                apply_toggle_entry(
                    &mut lines,
                    content,
                    key,
                    setting.enabled,
                    *default_enabled,
                    on_value,
                    off_value.as_deref(),
                    scalability_section.as_deref(),
                );
            }
            TweakKind::BatchToggle {
                entries,
                default_enabled,
            } => {
                for entry in entries {
                    apply_toggle_entry(
                        &mut lines,
                        content,
                        &entry.key,
                        setting.enabled,
                        *default_enabled,
                        &entry.on_value,
                        entry.off_value.as_deref(),
                        entry.scalability_section.as_deref(),
                    );
                }
            }
            TweakKind::Slider {
                key,
                default_value,
                write_default_on_disable,
                scalability_section,
                ..
            } => {
                if setting.enabled {
                    let value = setting.value.as_deref().unwrap_or("0");
                    if !upsert_key_value(&mut lines, key, value)
                        && let Some(sec) = scalability_section
                    {
                        insert_into_section(&mut lines, sec, format!("{}={}", key, value));
                    }
                } else if *write_default_on_disable {
                    upsert_key_value(&mut lines, key, &default_value.to_string());
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

/// Remove all non-comment lines whose key matches any entry's pattern key, including `+CVars=` form.
/// Lines matching an entry with `replace_with` set are replaced in-place instead of removed.
/// Key-based match catches duplicates with mismatched values that exact-line matching would miss.
fn remove_matching_lines(lines: &mut Vec<String>, entries: &[TweakLine]) {
    let entry_keys: Vec<String> = entries
        .iter()
        .map(|e| pattern_key_lower(&e.pattern))
        .collect();
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim().to_string();
        if t.starts_with(';') {
            i += 1;
            continue;
        }
        let matched = entry_keys.iter().position(|k| matches_key(&t, k));
        if let Some(idx) = matched {
            if let Some(replacement) = &entries[idx].replace_with {
                lines[i] = replacement.clone();
                i += 1;
            } else {
                lines.remove(i);
            }
        } else {
            i += 1;
        }
    }
}

/// Add missing lines under their target sections.
/// Entries without a section are skipped (they only apply in pak context).
/// Absence check is section-scoped: dupes in unrelated sections (which UE ignores) don't
/// block reinsertion into the declared section.
fn add_lines_if_absent(lines: &mut Vec<String>, entries: &[TweakLine]) {
    for entry in entries {
        let Some(section) = &entry.scalability_section else {
            continue;
        };
        let key_lower = pattern_key_lower(&entry.pattern);
        let content = lines.join("\n");
        if !key_present_in_section(&content, section, &key_lower) {
            insert_into_section(lines, section, entry.pattern.clone());
        }
    }
}

/// Apply a single toggle key: set to `on_value` when enabled, `off_value` (or remove) when disabled.
/// When `scalability_section` is `None` (pak-only tweaks), only in-place updates and removals are
/// performed; new keys are never inserted since there is no valid scalability section.
#[allow(clippy::too_many_arguments)]
fn apply_toggle_entry(
    lines: &mut Vec<String>,
    content: &str,
    key: &str,
    enabled: bool,
    default_enabled: bool,
    on_value: &str,
    off_value: Option<&str>,
    scalability_section: Option<&str>,
) {
    let key_in_file = find_key_value(content, key).is_some();
    let value = if enabled { Some(on_value) } else { off_value };

    match value {
        Some(v) => {
            if key_in_file {
                upsert_key_value(lines, key, v);
            } else if enabled != default_enabled
                && let Some(sec) = scalability_section
            {
                insert_into_section(lines, sec, format!("{}={}", key, v));
            }
        }
        None => remove_key(lines, key),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tweaks::catalogue::TweakDefinition;

    fn remove_lines_def(pattern: &str, section: &str) -> TweakDefinition {
        TweakDefinition {
            id: "t".into(),
            label: "t".into(),
            category: "test".into(),
            description: String::new(),
            pak_only: false,
            kind: TweakKind::RemoveLines {
                lines: vec![TweakLine {
                    pattern: pattern.into(),
                    scalability_section: Some(section.into()),
                    engine_section: None,
                    replace_with: None,
                }],
                remove_only: false,
            },
        }
    }

    fn remove_lines_def_replace(
        pattern: &str,
        replacement: &str,
        section: Option<&str>,
    ) -> TweakDefinition {
        TweakDefinition {
            id: "t".into(),
            label: "t".into(),
            category: "test".into(),
            description: String::new(),
            pak_only: false,
            kind: TweakKind::RemoveLines {
                lines: vec![TweakLine {
                    pattern: pattern.into(),
                    scalability_section: section.map(String::from),
                    engine_section: None,
                    replace_with: Some(replacement.into()),
                }],
                remove_only: false,
            },
        }
    }

    fn slider_def(
        key: &str,
        default_value: f64,
        write_default: bool,
        section: &str,
    ) -> TweakDefinition {
        TweakDefinition {
            id: "t".into(),
            label: "t".into(),
            category: "test".into(),
            description: String::new(),
            pak_only: false,
            kind: TweakKind::Slider {
                key: key.into(),
                min: 0.0,
                max: 100.0,
                step: 1.0,
                default_value,
                write_default_on_disable: write_default,
                scalability_section: Some(section.into()),
                engine_section: None,
            },
        }
    }

    fn enabled(id: &str, value: Option<&str>) -> TweakSetting {
        TweakSetting {
            id: id.into(),
            enabled: true,
            value: value.map(String::from),
        }
    }

    fn disabled(id: &str) -> TweakSetting {
        TweakSetting {
            id: id.into(),
            enabled: false,
            value: None,
        }
    }

    #[test]
    fn remove_lines_strips_all_dupes_globally() {
        let def = remove_lines_def("r.MipMapLODBias=15", "TextureQuality@0");
        let content =
            "[TextureQuality@0]\r\nr.MipMapLODBias=15\r\n\r\n[Random]\r\nr.MipMapLODBias=16\r\n";
        let result = apply_tweaks(content, &[def], &[enabled("t", None)]);
        assert!(
            !result.contains("r.MipMapLODBias"),
            "all instances stripped:\n{}",
            result
        );
    }

    #[test]
    fn add_lines_skips_when_already_in_target_section() {
        let def = remove_lines_def("r.X=1", "S");
        let content = "[S]\r\nr.X=1\r\n";
        let result = apply_tweaks(content, &[def], &[disabled("t")]);
        assert_eq!(
            result.matches("r.X=1").count(),
            1,
            "no duplicate insertion:\n{}",
            result
        );
    }

    #[test]
    fn add_lines_inserts_when_only_dupes_outside_target_section() {
        let def = remove_lines_def("r.X=1", "S");
        let content = "[S]\r\n\r\n[Random]\r\nr.X=2\r\n";
        let result = apply_tweaks(content, &[def], &[disabled("t")]);
        assert!(
            result.contains("r.X=1"),
            "canonical pattern reinserted:\n{}",
            result
        );
        assert!(
            result.contains("r.X=2"),
            "out-of-section dupe preserved:\n{}",
            result
        );
    }

    #[test]
    fn round_trip_normalizes_polluted_target_section() {
        let def = remove_lines_def("r.MipMapLODBias=15", "TextureQuality@0");
        let content = "[TextureQuality@0]\r\nr.MipMapLODBias=15\r\nr.MipMapLODBias=16\r\n";
        let after_on = apply_tweaks(content, std::slice::from_ref(&def), &[enabled("t", None)]);
        assert!(!after_on.contains("r.MipMapLODBias"));
        let after_off = apply_tweaks(&after_on, &[def], &[disabled("t")]);
        assert_eq!(
            after_off.matches("r.MipMapLODBias=15").count(),
            1,
            "single canonical line after ON->OFF:\n{}",
            after_off
        );
        assert!(!after_off.contains("r.MipMapLODBias=16"));
    }

    #[test]
    fn slider_normalizes_all_dupes_globally() {
        let def = slider_def("r.X", 0.0, true, "S");
        let content = "[S]\r\nr.X=10\r\n\r\n[Random]\r\nr.X=99\r\n";
        let result = apply_tweaks(content, &[def], &[enabled("t", Some("5"))]);
        assert_eq!(
            result.matches("r.X=5").count(),
            2,
            "all instances normalized:\n{}",
            result
        );
        assert!(!result.contains("r.X=10"));
        assert!(!result.contains("r.X=99"));
    }

    #[test]
    fn slider_disable_with_write_default_normalizes_to_default() {
        let def = slider_def("r.X", 0.0, true, "S");
        let content = "[S]\r\nr.X=10\r\n\r\n[Random]\r\nr.X=99\r\n";
        let result = apply_tweaks(content, &[def], &[disabled("t")]);
        assert_eq!(
            result.matches("r.X=0").count(),
            2,
            "all instances reset to default:\n{}",
            result
        );
    }

    #[test]
    fn slider_disable_without_write_default_removes_all() {
        let def = slider_def("r.X", 0.0, false, "S");
        let content = "[S]\r\nr.X=10\r\n\r\n[Random]\r\nr.X=99\r\n";
        let result = apply_tweaks(content, &[def], &[disabled("t")]);
        assert!(
            !result.contains("r.X="),
            "all instances removed:\n{}",
            result
        );
    }

    #[test]
    fn replace_with_normalizes_all_key_matching_lines() {
        let def = remove_lines_def_replace("r.X=0", "r.X=3", Some("S"));
        let content = "[S]\r\nr.X=0\r\n\r\n[Random]\r\nr.X=99\r\n";
        let result = apply_tweaks(content, &[def], &[enabled("t", None)]);
        assert_eq!(
            result.matches("r.X=3").count(),
            2,
            "all key matches replaced:\n{}",
            result
        );
        assert!(!result.contains("r.X=0"));
        assert!(!result.contains("r.X=99"));
    }
}
