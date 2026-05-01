//! INI parsing and CVar edit application for pak-embedded config files.

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

/// Remove matching CVar lines only within the section starting at `section_start` (header index).
/// Stops at the next section header or end of file.
fn remove_cvar_key_in_section(lines: &mut Vec<String>, section_start: usize, key_lower: &str) {
    let mut i = section_start + 1;
    while i < lines.len() {
        let t = lines[i].trim();
        if t.starts_with('[') {
            break;
        }
        if t.starts_with(';') || t.is_empty() {
            i += 1;
            continue;
        }
        match parse_cvar_line(t) {
            Some((k, _)) if k.to_ascii_lowercase() == key_lower => {
                lines.remove(i);
            }
            _ => {
                i += 1;
            }
        }
    }
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
                remove_cvar_key_in_section(lines, start, &key_lower);
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

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod roundtrip_tests {
    //! Round-trip catalogue verification: simulate the full apply detect pipeline
    //! at the INI layer (skipping pak encrypt/repack which is repak's responsibility).
    //! For each catalogue tweak, toggle ON via the same logic the frontend uses,
    //! apply the edits like apply_pak_tweaks does, then run detect_tweaks_unscoped
    //! against the merged result. Catches regressions in detection vs apply drift.
    use super::*;
    use crate::pak_tweaks::PakTweakEdit;
    use crate::tweaks::TweakState;
    use crate::tweaks::catalogue::{TweakDefinition, TweakKind, tweak_catalogue};
    use crate::tweaks::detect_tweaks_unscoped;

    /// Mirrors the frontend `toggleQuickTweak` logic for the ON case.
    fn edits_for_on(def: &TweakDefinition) -> Vec<PakTweakEdit> {
        match &def.kind {
            TweakKind::RemoveLines { lines, .. } => lines
                .iter()
                .map(|line| {
                    let key = line
                        .pattern
                        .split_once('=')
                        .map(|(k, _)| k)
                        .unwrap_or(&line.pattern)
                        .to_string();
                    let replace_val: Option<String> = line.replace_with.as_ref().map(|rw| {
                        rw.split_once('=')
                            .map(|(_, v)| v.to_string())
                            .unwrap_or_else(|| rw.clone())
                    });
                    PakTweakEdit {
                        key,
                        value: replace_val, // None for plain remove, Some for replace_with
                        engine_section: line.engine_section.clone(),
                    }
                })
                .collect(),
            TweakKind::Toggle {
                key,
                on_value,
                engine_section,
                ..
            } => vec![PakTweakEdit {
                key: key.clone(),
                value: Some(on_value.clone()),
                engine_section: engine_section.clone(),
            }],
            TweakKind::Slider {
                key,
                default_value,
                engine_section,
                ..
            } => {
                // Pick a non-default value so detection registers as active.
                let v = if (*default_value - 0.0).abs() < f64::EPSILON {
                    "1".to_string()
                } else {
                    "0".to_string()
                };
                vec![PakTweakEdit {
                    key: key.clone(),
                    value: Some(v),
                    engine_section: engine_section.clone(),
                }]
            }
            TweakKind::BatchToggle { entries, .. } => entries
                .iter()
                .map(|e| PakTweakEdit {
                    key: e.key.clone(),
                    value: Some(e.on_value.clone()),
                    engine_section: e.engine_section.clone(),
                })
                .collect(),
        }
    }

    /// Apply edits to engine+dp content following the same partition logic the
    /// backend uses in `apply_device_profiles_edits`.
    fn apply_to_pair(engine: &str, dp: &str, edits: &[PakTweakEdit]) -> (String, String) {
        let (engine_edits, dp_edits): (Vec<_>, Vec<_>) = edits
            .iter()
            .cloned()
            .partition(|e| e.engine_section.is_some() && e.value.is_some());

        let dp_after = apply_edits_to_ini(dp, &dp_edits, IniType::DeviceProfiles);

        // Mirror remove-edits to engine.ini so stale keys don't shadow.
        let remove_edits: Vec<PakTweakEdit> = edits
            .iter()
            .filter(|e| e.value.is_none())
            .cloned()
            .collect();
        let mut engine_full = engine_edits;
        for r in remove_edits {
            if !engine_full
                .iter()
                .any(|e| e.key.eq_ignore_ascii_case(&r.key))
            {
                engine_full.push(r);
            }
        }
        let engine_after = if !engine_full.is_empty() {
            apply_edits_to_ini(engine, &engine_full, IniType::Engine)
        } else {
            engine.to_string()
        };

        (engine_after, dp_after)
    }

    /// Build the synthetic key=value content `detect_pak_tweaks` feeds to the detector.
    fn merge_to_synthetic(engine: &str, dp: &str) -> String {
        let mut merged = parse_console_vars(engine, "DefaultEngine.ini");
        let dp_vars = parse_console_vars(dp, "DefaultDeviceProfiles.ini");
        for dp_var in dp_vars {
            let key_lower = dp_var.key.to_ascii_lowercase();
            merged.retain(|v| v.key.to_ascii_lowercase() != key_lower);
            merged.push(dp_var);
        }
        merged
            .iter()
            .map(|s| format!("{}={}", s.key, s.value))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn state_for<'a>(states: &'a [TweakState], id: &str) -> &'a TweakState {
        states
            .iter()
            .find(|s| s.id == id)
            .unwrap_or_else(|| panic!("state for {id} missing"))
    }

    #[test]
    fn fix_abilities_with_replace_with_round_trip() {
        // The exact regression that prompted this audit: r.CustomDepth=0 → r.CustomDepth=3.
        let cat = tweak_catalogue();
        let def = cat
            .iter()
            .find(|t| t.id == "fix_abilities")
            .expect("fix_abilities catalogue entry");

        // Pak baseline: engine.ini contains the OFF-state lines (typical mod state).
        let engine = "[ConsoleVariables]\nr.PostProcessing.DisableMaterials=1\nr.CustomDepth=0\nr.LightTile.Enable=0\n";
        let dp = "[Windows DeviceProfile]\n";

        // Detect baseline: tweak should read OFF (original patterns present).
        let detect_off = detect_tweaks_unscoped(&merge_to_synthetic(engine, dp));
        assert!(
            !state_for(&detect_off, "fix_abilities").active,
            "OFF baseline should detect inactive"
        );

        // Apply ON.
        let edits = edits_for_on(def);
        let (engine_on, dp_on) = apply_to_pair(engine, dp, &edits);

        // After ON: PostProcessing.DisableMaterials and LightTile.Enable removed,
        // CustomDepth replaced with 3.
        let detect_on = detect_tweaks_unscoped(&merge_to_synthetic(&engine_on, &dp_on));
        assert!(
            state_for(&detect_on, "fix_abilities").active,
            "ON state must be detected after apply (regression: was reading inactive due to key-only check)"
        );
    }

    #[test]
    fn batch_toggle_network_revert_round_trip() {
        let cat = tweak_catalogue();
        let def = cat
            .iter()
            .find(|t| t.id == "network_revert_update_65")
            .expect("network_revert_update_65 catalogue entry");

        let engine = "";
        let dp = "[Windows DeviceProfile]\n";

        let edits = edits_for_on(def);
        let (engine_on, dp_on) = apply_to_pair(engine, dp, &edits);

        let detect_on = detect_tweaks_unscoped(&merge_to_synthetic(&engine_on, &dp_on));
        assert!(
            state_for(&detect_on, "network_revert_update_65").active,
            "BatchToggle ON state must be detected"
        );
    }

    #[test]
    fn slider_write_default_on_disable_round_trip() {
        let cat = tweak_catalogue();
        let def = cat
            .iter()
            .find(|t| {
                matches!(
                    &t.kind,
                    TweakKind::Slider {
                        write_default_on_disable: true,
                        ..
                    }
                )
            })
            .expect("at least one slider with write_default_on_disable=true");

        let engine = "[ConsoleVariables]\n";
        let dp = "[Windows DeviceProfile]\n";

        let edits = edits_for_on(def);
        let (engine_on, dp_on) = apply_to_pair(engine, dp, &edits);

        let detect_on = detect_tweaks_unscoped(&merge_to_synthetic(&engine_on, &dp_on));
        assert!(
            state_for(&detect_on, &def.id).active,
            "slider non-default value must register as active"
        );
    }

    /// Walk every catalogue tweak and verify the apply→detect round-trip.
    /// This is the safety net the regression slipped past.
    #[test]
    fn full_catalogue_apply_detect_round_trip() {
        let cat = tweak_catalogue();

        // Baseline pak content with original RemoveLines patterns present so
        // that those tweaks start in the OFF state.
        let mut engine = String::from("[ConsoleVariables]\n");
        for def in cat.iter() {
            if let TweakKind::RemoveLines { lines, .. } = &def.kind {
                for line in lines {
                    engine.push_str(&line.pattern);
                    engine.push('\n');
                }
            }
        }
        let dp = String::from("[Windows DeviceProfile]\n");

        for def in cat.iter() {
            // Skip pak-only=false tweaks that target Scalability sections only;
            // they still flow through pak path as the frontend allows.
            // (Detection happens against the full catalogue regardless.)
            let edits = edits_for_on(def);
            if edits.is_empty() {
                continue;
            }
            let (engine_after, dp_after) = apply_to_pair(&engine, &dp, &edits);
            let states = detect_tweaks_unscoped(&merge_to_synthetic(&engine_after, &dp_after));
            let state = state_for(&states, &def.id);
            assert!(
                state.active,
                "tweak {} should detect ACTIVE after applying ON edits (kind={:?})",
                def.id,
                std::mem::discriminant(&def.kind)
            );
        }
    }
}
