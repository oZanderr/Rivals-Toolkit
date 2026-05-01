//! Shared INI query helpers used by both detect and apply paths. Read-only.

/// Compare a string value from an INI file against a default `f64`.
/// Handles mismatches like `"1"` vs `1.0` by parsing both as numbers.
pub(crate) fn values_equal(file_value: &str, default: f64) -> bool {
    if let Ok(v) = file_value.trim().parse::<f64>() {
        (v - default).abs() < f64::EPSILON
    } else {
        false
    }
}

/// Extract the lowercased key from a tweak pattern like `r.X=0` or `+CVars=r.X=0`.
pub(crate) fn pattern_key_lower(pattern: &str) -> String {
    let inner = if pattern.to_ascii_lowercase().starts_with("+cvars=") {
        &pattern["+CVars=".len()..]
    } else {
        pattern
    };
    let key = inner.split_once('=').map(|(k, _)| k).unwrap_or(inner);
    key.trim().to_ascii_lowercase()
}

/// Check whether a non-comment line is an assignment to `key_lower`, including `+CVars=` form.
pub(crate) fn matches_key(trimmed_line: &str, key_lower: &str) -> bool {
    let line_lower = trimmed_line.to_ascii_lowercase();
    let prefix = format!("{}=", key_lower);
    let cvars_prefix = format!("+cvars={}=", key_lower);
    line_lower.starts_with(&prefix) || line_lower.starts_with(&cvars_prefix)
}

/// Find the last value of `key`, including `+CVars=key=value` lines.
pub(crate) fn find_key_value(content: &str, key: &str) -> Option<String> {
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

/// Find the last value of `key` within `[section]` only. Last-wins inside the section.
pub(crate) fn find_key_value_in_section(content: &str, section: &str, key: &str) -> Option<String> {
    let key_lower = key.to_ascii_lowercase();
    let prefix = format!("{}=", key_lower);
    let cvars_prefix = format!("+cvars={}=", key_lower);
    let target_lower = format!("[{}]", section).to_ascii_lowercase();
    let mut in_section = false;
    let mut found: Option<String> = None;

    for line in content.lines() {
        let t = line.trim();
        if t.starts_with('[') && t.ends_with(']') {
            in_section = t.to_ascii_lowercase() == target_lower;
            continue;
        }
        if !in_section || t.starts_with(';') {
            continue;
        }
        let t_lower = t.to_ascii_lowercase();
        if t_lower.starts_with(&prefix) {
            found = Some(t[key.len() + 1..].to_string());
        } else if t_lower.starts_with(&cvars_prefix) {
            found = Some(t["+CVars=".len() + key.len() + 1..].to_string());
        }
    }
    found
}

/// Whether any non-comment line in `content` matches the full `pattern`, ignoring
/// section structure. Pattern can be `key=value` (full match required) or just `key`
/// (key-only match). Handles `+CVars=` prefix on both pattern and lines.
pub(crate) fn pattern_present_anywhere(content: &str, pattern: &str) -> bool {
    let inner_pattern = if pattern.to_ascii_lowercase().starts_with("+cvars=") {
        &pattern["+CVars=".len()..]
    } else {
        pattern
    };
    let (pattern_key, pattern_val): (String, Option<String>) = match inner_pattern.split_once('=') {
        Some((k, v)) => (k.trim().to_ascii_lowercase(), Some(v.trim().to_string())),
        None => (inner_pattern.trim().to_ascii_lowercase(), None),
    };

    for line in content.lines() {
        let t = line.trim();
        if t.starts_with(';') || t.is_empty() {
            continue;
        }
        let inner_line = if t.to_ascii_lowercase().starts_with("+cvars=") {
            &t["+CVars=".len()..]
        } else {
            t
        };
        let (line_key, line_val) = match inner_line.split_once('=') {
            Some((k, v)) => (k.trim().to_ascii_lowercase(), Some(v.trim().to_string())),
            None => (inner_line.trim().to_ascii_lowercase(), None),
        };
        if line_key != pattern_key {
            continue;
        }
        match (&pattern_val, &line_val) {
            (Some(pv), Some(lv)) if pv == lv => return true,
            (None, _) => return true,
            _ => {}
        }
    }
    false
}

/// Whether any non-comment assignment to `key_lower` exists within `[section]`.
pub(crate) fn key_present_in_section(content: &str, section: &str, key_lower: &str) -> bool {
    let target_lower = format!("[{}]", section).to_ascii_lowercase();
    let mut in_section = false;
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with('[') && t.ends_with(']') {
            in_section = t.to_ascii_lowercase() == target_lower;
            continue;
        }
        if !in_section || t.starts_with(';') {
            continue;
        }
        if matches_key(t, key_lower) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_key_lower_strips_cvars_prefix_and_value() {
        assert_eq!(pattern_key_lower("r.X=15"), "r.x");
        assert_eq!(pattern_key_lower("+CVars=r.X=15"), "r.x");
        assert_eq!(pattern_key_lower("+cvars=r.Y=0"), "r.y");
    }

    #[test]
    fn matches_key_handles_both_forms_case_insensitive() {
        assert!(matches_key("r.X=99", "r.x"));
        assert!(matches_key("+CVars=R.X=1", "r.x"));
        assert!(!matches_key("r.XY=1", "r.x"));
        assert!(!matches_key("=1", "r.x"));
    }

    #[test]
    fn find_key_value_in_section_returns_last_in_section() {
        let content = "[Target]\nr.X=1\nr.X=2\n[Other]\nr.X=99\n[Target]\nr.X=3\n";
        assert_eq!(
            find_key_value_in_section(content, "Target", "r.X").as_deref(),
            Some("3")
        );
    }

    #[test]
    fn find_key_value_in_section_ignores_other_sections() {
        let content = "[Target]\n\n[Other]\nr.X=99\n";
        assert!(find_key_value_in_section(content, "Target", "r.X").is_none());
    }

    #[test]
    fn key_present_in_section_ignores_comments_and_other_sections() {
        let content = "[Target]\n;r.X=1\n[Other]\nr.X=2\n";
        assert!(!key_present_in_section(content, "Target", "r.x"));
        assert!(key_present_in_section(content, "Other", "r.x"));
    }

    #[test]
    fn key_present_in_section_matches_cvars_form() {
        let content = "[Target]\n+CVars=r.X=1\n";
        assert!(key_present_in_section(content, "Target", "r.x"));
    }
}
