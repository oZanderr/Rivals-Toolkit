//! INI parsing and CVar edit application for pak-embedded config files.

use super::{PakCvar, PakTweakEdit};

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
pub(super) fn parse_console_vars(content: &str, source: &str) -> Vec<PakCvar> {
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
                vars.push(PakCvar {
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
                vars.push(PakCvar {
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

/// Line ranges of every `[Windows DeviceProfile]` section, as `(header index, end)`.
///
/// Combo paks concatenate several mods' configs, so this header can appear more than once and the
/// same key can sit in each copy.
fn device_profile_sections(lines: &[String]) -> Vec<(usize, usize)> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| is_windows_device_profile_header(line.trim()))
        .map(|(i, _)| (i, find_section_end(lines, i)))
        .collect()
}

/// Every line under a `[Windows DeviceProfile]` header that sets `key_lower`, in file order.
fn device_profile_key_hits(lines: &[String], key_lower: &str) -> Vec<usize> {
    let mut hits = Vec::new();
    for (start, end) in device_profile_sections(lines) {
        for (offset, line) in lines[start + 1..end].iter().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with(';') {
                continue;
            }
            if let Some((k, _)) = parse_cvar_line(trimmed)
                && k.to_ascii_lowercase() == key_lower
            {
                hits.push(start + 1 + offset);
            }
        }
    }
    hits
}

/// Apply edits across every `[Windows DeviceProfile]` section.
///
/// Reads are last-wins over the whole file, so touching a single occurrence of a key is invisible
/// when a later duplicate survives. Every occurrence is handled, and a set collapses them to one
/// line so repeated saves cannot grow the file.
fn apply_device_profiles_edits(lines: &mut Vec<String>, edits: &[PakTweakEdit]) {
    for edit in edits {
        let key_lower = edit.key.to_ascii_lowercase();
        let hits = device_profile_key_hits(lines, &key_lower);

        match (&edit.value, hits.split_last()) {
            // Keep the occurrence a read would land on and drop the earlier duplicates.
            (Some(val), Some((&last, earlier))) => {
                let has_prefix = lines[last]
                    .trim()
                    .to_ascii_lowercase()
                    .starts_with("+cvars=");
                lines[last] = format_cvar_line(&edit.key, val, has_prefix);
                for &i in earlier.iter().rev() {
                    lines.remove(i);
                }
            }
            (None, Some(_)) => {
                for &i in hits.iter().rev() {
                    lines.remove(i);
                }
            }
            (Some(val), None) => {
                let start = match device_profile_sections(lines).last() {
                    Some(&(start, _)) => start,
                    None => {
                        if lines.last().is_some_and(|l| !l.trim().is_empty()) {
                            lines.push(String::new());
                        }
                        lines.push("[Windows DeviceProfile]".to_string());
                        lines.len() - 1
                    }
                };
                let insert_at = find_section_insert_point(lines, start);
                lines.insert(insert_at, format_cvar_line(&edit.key, val, true));
            }
            // Nothing to remove, and a pure removal never creates a section.
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
    use crate::pak_tweaks::{PakIniTarget, PakTweakEdit};
    use crate::tweaks::TweakState;
    use crate::tweaks::catalogue::{TweakDefinition, TweakKind, tweak_catalogue};
    use crate::tweaks::detect_tweaks_unscoped;
    use std::collections::HashSet;

    /// ON-state edits, built by the same `edits_for_tweak` both front ends call, so this round trip
    /// tests the shipped translation rather than a copy of it.
    fn edits_for_on(def: &TweakDefinition) -> Vec<PakTweakEdit> {
        // Sliders only detect as active away from their default, so pick the far end of the range.
        let slider_value = match &def.kind {
            TweakKind::Slider {
                min,
                max,
                default_value,
                ..
            } => {
                let away = if (default_value - min).abs() < f64::EPSILON {
                    max
                } else {
                    min
                };
                Some(format!("{away}"))
            }
            _ => None,
        };
        crate::pak_tweaks::edits_for_tweak(def, true, slider_value.as_deref())
            .unwrap_or_else(|e| panic!("'{}' failed to translate: {e}", def.id))
    }

    /// Four-file equivalent: simulate the full `apply_pak_tweaks` pipeline against
    /// BaseEngine / DefaultEngine / WindowsEngine / DeviceProfiles content. Absent
    /// files are represented by empty strings; the layer is treated as missing when
    /// `present_*` is false (mirroring how `inspect_pak_for_ini` populates entries).
    #[allow(clippy::too_many_arguments)]
    fn apply_to_quad(
        base: &str,
        default: &str,
        windows: &str,
        dp: &str,
        present_base: bool,
        present_default: bool,
        present_windows: bool,
        present_dp: bool,
        edits: &[PakTweakEdit],
        target: PakIniTarget,
    ) -> (String, String, String, String) {
        let (engine_section_edits, plain_edits): (Vec<_>, Vec<_>) = edits
            .iter()
            .cloned()
            .partition(|e| e.engine_section.is_some());

        let mut base_after = base.to_string();
        let mut default_after = default.to_string();
        let mut windows_after = windows.to_string();
        let mut dp_after = dp.to_string();

        let resolved = if matches!(target, PakIniTarget::BaseEngine) && present_base
            || matches!(target, PakIniTarget::Engine) && present_default
            || matches!(target, PakIniTarget::WindowsEngine) && present_windows
            || matches!(target, PakIniTarget::DeviceProfiles) && present_dp
        {
            target
        } else {
            // Mirror real resolve_target fallback order.
            if present_dp {
                PakIniTarget::DeviceProfiles
            } else if present_windows {
                PakIniTarget::WindowsEngine
            } else if present_default {
                PakIniTarget::Engine
            } else if present_base {
                PakIniTarget::BaseEngine
            } else {
                panic!("no files present");
            }
        };

        let engine_section_target = match resolved {
            PakIniTarget::DeviceProfiles => {
                if present_windows {
                    Some(PakIniTarget::WindowsEngine)
                } else if present_default {
                    Some(PakIniTarget::Engine)
                } else if present_base {
                    Some(PakIniTarget::BaseEngine)
                } else {
                    None
                }
            }
            t => Some(t),
        };

        let write_to = |slot: PakIniTarget,
                        content: &mut String,
                        edits: &[PakTweakEdit],
                        ini_type: IniType| {
            *content = apply_edits_to_ini(content, edits, ini_type);
            let _ = slot;
        };

        // Plain edits -> resolved target.
        match resolved {
            PakIniTarget::BaseEngine => write_to(
                PakIniTarget::BaseEngine,
                &mut base_after,
                &plain_edits,
                IniType::Engine,
            ),
            PakIniTarget::Engine => write_to(
                PakIniTarget::Engine,
                &mut default_after,
                &plain_edits,
                IniType::Engine,
            ),
            PakIniTarget::WindowsEngine => write_to(
                PakIniTarget::WindowsEngine,
                &mut windows_after,
                &plain_edits,
                IniType::Engine,
            ),
            PakIniTarget::DeviceProfiles => write_to(
                PakIniTarget::DeviceProfiles,
                &mut dp_after,
                &plain_edits,
                IniType::DeviceProfiles,
            ),
        }

        // Engine-section edits -> engine_section_target.
        if !engine_section_edits.is_empty()
            && let Some(eng_target) = engine_section_target
        {
            match eng_target {
                PakIniTarget::BaseEngine => write_to(
                    eng_target,
                    &mut base_after,
                    &engine_section_edits,
                    IniType::Engine,
                ),
                PakIniTarget::Engine => write_to(
                    eng_target,
                    &mut default_after,
                    &engine_section_edits,
                    IniType::Engine,
                ),
                PakIniTarget::WindowsEngine => write_to(
                    eng_target,
                    &mut windows_after,
                    &engine_section_edits,
                    IniType::Engine,
                ),
                PakIniTarget::DeviceProfiles => unreachable!(),
            }
        }

        // Plain-edit sibling sync across the other present files.
        let layers: [(PakIniTarget, bool, &str, IniType); 4] = [
            (
                PakIniTarget::BaseEngine,
                present_base,
                "BaseEngine.ini",
                IniType::Engine,
            ),
            (
                PakIniTarget::Engine,
                present_default,
                "DefaultEngine.ini",
                IniType::Engine,
            ),
            (
                PakIniTarget::WindowsEngine,
                present_windows,
                "WindowsEngine.ini",
                IniType::Engine,
            ),
            (
                PakIniTarget::DeviceProfiles,
                present_dp,
                "DefaultDeviceProfiles.ini",
                IniType::DeviceProfiles,
            ),
        ];

        for (slot, present, label, ini_type) in layers {
            if !present || slot == resolved {
                continue;
            }
            let content_ref = match slot {
                PakIniTarget::BaseEngine => &base_after,
                PakIniTarget::Engine => &default_after,
                PakIniTarget::WindowsEngine => &windows_after,
                PakIniTarget::DeviceProfiles => &dp_after,
            };
            let present_keys: HashSet<String> = parse_console_vars(content_ref, label)
                .into_iter()
                .map(|s| s.key.to_ascii_lowercase())
                .collect();
            let filtered: Vec<PakTweakEdit> = plain_edits
                .iter()
                .filter(|e| present_keys.contains(&e.key.to_ascii_lowercase()))
                .cloned()
                .collect();
            match slot {
                PakIniTarget::BaseEngine => {
                    base_after = apply_edits_to_ini(&base_after, &filtered, ini_type)
                }
                PakIniTarget::Engine => {
                    default_after = apply_edits_to_ini(&default_after, &filtered, ini_type)
                }
                PakIniTarget::WindowsEngine => {
                    windows_after = apply_edits_to_ini(&windows_after, &filtered, ini_type)
                }
                PakIniTarget::DeviceProfiles => {
                    dp_after = apply_edits_to_ini(&dp_after, &filtered, ini_type)
                }
            }
        }

        // Engine-section sibling sync across the other engine files.
        if !engine_section_edits.is_empty()
            && let Some(eng_target) = engine_section_target
        {
            for (slot, present, label) in [
                (PakIniTarget::BaseEngine, present_base, "BaseEngine.ini"),
                (PakIniTarget::Engine, present_default, "DefaultEngine.ini"),
                (
                    PakIniTarget::WindowsEngine,
                    present_windows,
                    "WindowsEngine.ini",
                ),
            ] {
                if !present || slot == eng_target {
                    continue;
                }
                let content_ref = match slot {
                    PakIniTarget::BaseEngine => &base_after,
                    PakIniTarget::Engine => &default_after,
                    PakIniTarget::WindowsEngine => &windows_after,
                    _ => unreachable!(),
                };
                let present_keys: HashSet<String> = parse_console_vars(content_ref, label)
                    .into_iter()
                    .map(|s| s.key.to_ascii_lowercase())
                    .collect();
                let filtered: Vec<PakTweakEdit> = engine_section_edits
                    .iter()
                    .filter(|e| present_keys.contains(&e.key.to_ascii_lowercase()))
                    .cloned()
                    .collect();
                match slot {
                    PakIniTarget::BaseEngine => {
                        base_after = apply_edits_to_ini(&base_after, &filtered, IniType::Engine)
                    }
                    PakIniTarget::Engine => {
                        default_after =
                            apply_edits_to_ini(&default_after, &filtered, IniType::Engine)
                    }
                    PakIniTarget::WindowsEngine => {
                        windows_after =
                            apply_edits_to_ini(&windows_after, &filtered, IniType::Engine)
                    }
                    _ => unreachable!(),
                }
            }
        }

        (base_after, default_after, windows_after, dp_after)
    }

    /// Legacy 2-file wrapper used by existing tests: delegates to the 4-file helper
    /// with empty/absent BaseEngine and WindowsEngine layers.
    fn apply_to_pair(
        engine: &str,
        dp: &str,
        edits: &[PakTweakEdit],
        target: PakIniTarget,
    ) -> (String, String) {
        let (_, default_after, _, dp_after) =
            apply_to_quad("", engine, "", dp, false, true, false, true, edits, target);
        (default_after, dp_after)
    }

    /// Build the synthetic key=value content `detect_pak_tweaks` feeds to the detector.
    fn merge_to_synthetic(engine: &str, dp: &str) -> String {
        merge_to_synthetic_quad("", engine, "", dp, false, true, false, true)
    }

    /// Four-file variant of merge_to_synthetic; matches `read_pak_cvars` priority chain.
    #[allow(clippy::too_many_arguments)]
    fn merge_to_synthetic_quad(
        base: &str,
        default: &str,
        windows: &str,
        dp: &str,
        present_base: bool,
        present_default: bool,
        present_windows: bool,
        present_dp: bool,
    ) -> String {
        let layers: [(&str, bool, &str); 4] = [
            (base, present_base, "BaseEngine.ini"),
            (default, present_default, "DefaultEngine.ini"),
            (windows, present_windows, "WindowsEngine.ini"),
            (dp, present_dp, "DefaultDeviceProfiles.ini"),
        ];
        let mut merged: Vec<PakCvar> = Vec::new();
        for (content, present, label) in layers {
            if !present {
                continue;
            }
            for var in parse_console_vars(content, label) {
                let key_lower = var.key.to_ascii_lowercase();
                merged.retain(|v| v.key.to_ascii_lowercase() != key_lower);
                merged.push(var);
            }
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
        let (engine_on, dp_on) = apply_to_pair(engine, dp, &edits, PakIniTarget::DeviceProfiles);

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
        let (engine_on, dp_on) = apply_to_pair(engine, dp, &edits, PakIniTarget::DeviceProfiles);

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
        let (engine_on, dp_on) = apply_to_pair(engine, dp, &edits, PakIniTarget::DeviceProfiles);

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

        // Both edit targets must round-trip to ACTIVE: the merged runtime view is the
        // same regardless of which file holds the CVar.
        for target in [PakIniTarget::DeviceProfiles, PakIniTarget::Engine] {
            for def in cat.iter() {
                let edits = edits_for_on(def);
                if edits.is_empty() {
                    continue;
                }
                let (engine_after, dp_after) = apply_to_pair(&engine, &dp, &edits, target);
                let states = detect_tweaks_unscoped(&merge_to_synthetic(&engine_after, &dp_after));
                let state = state_for(&states, &def.id);
                assert!(
                    state.active,
                    "tweak {} should detect ACTIVE after applying ON edits (target={:?}, kind={:?})",
                    def.id,
                    target,
                    std::mem::discriminant(&def.kind)
                );
            }
        }
    }

    fn cvar_edit(key: &str, value: Option<&str>) -> PakTweakEdit {
        PakTweakEdit {
            key: key.into(),
            value: value.map(str::to_string),
            engine_section: None,
        }
    }

    #[test]
    fn target_engine_syncs_existing_key_in_device_profiles() {
        let engine = "[ConsoleVariables]\nr.Foo=0\n";
        let dp = "[Windows DeviceProfile]\n+CVars=r.Foo=0\n";
        let (engine_after, dp_after) = apply_to_pair(
            engine,
            dp,
            &[cvar_edit("r.Foo", Some("1"))],
            PakIniTarget::Engine,
        );
        assert!(
            engine_after.contains("r.Foo=1"),
            "engine target:\n{engine_after}"
        );
        assert!(
            dp_after.contains("+CVars=r.Foo=1"),
            "sibling DeviceProfiles copy should update so it doesn't shadow:\n{dp_after}"
        );
    }

    #[test]
    fn target_device_profiles_syncs_existing_key_in_engine() {
        let engine = "[ConsoleVariables]\nr.Foo=0\n";
        let dp = "[Windows DeviceProfile]\n+CVars=r.Foo=0\n";
        let (engine_after, dp_after) = apply_to_pair(
            engine,
            dp,
            &[cvar_edit("r.Foo", Some("1"))],
            PakIniTarget::DeviceProfiles,
        );
        assert!(
            dp_after.contains("+CVars=r.Foo=1"),
            "dp target:\n{dp_after}"
        );
        assert!(
            engine_after.contains("r.Foo=1"),
            "sibling Engine copy should stay consistent:\n{engine_after}"
        );
    }

    #[test]
    fn removal_clears_key_from_both_files() {
        let engine = "[ConsoleVariables]\nr.Foo=0\n";
        let dp = "[Windows DeviceProfile]\n+CVars=r.Foo=0\n";
        let (engine_after, dp_after) = apply_to_pair(
            engine,
            dp,
            &[cvar_edit("r.Foo", None)],
            PakIniTarget::DeviceProfiles,
        );
        assert!(
            !engine_after.to_ascii_lowercase().contains("r.foo"),
            "engine copy should be removed for a true reset:\n{engine_after}"
        );
        assert!(
            !dp_after.to_ascii_lowercase().contains("r.foo"),
            "dp copy should be removed:\n{dp_after}"
        );
    }

    #[test]
    fn sibling_without_key_is_not_injected() {
        let engine = "[ConsoleVariables]\n";
        let dp = "[Windows DeviceProfile]\n";
        let (engine_after, dp_after) = apply_to_pair(
            engine,
            dp,
            &[cvar_edit("r.Foo", Some("1"))],
            PakIniTarget::DeviceProfiles,
        );
        assert!(
            dp_after.contains("+CVars=r.Foo=1"),
            "target dp gets the key:\n{dp_after}"
        );
        assert!(
            !engine_after.to_ascii_lowercase().contains("r.foo"),
            "engine never had the key, so it must not be injected:\n{engine_after}"
        );
    }

    #[test]
    fn device_profiles_section_created_when_missing() {
        let dp = "; device profiles file with no windows section\n";
        let out = apply_edits_to_ini(
            dp,
            &[cvar_edit("r.Foo", Some("1"))],
            IniType::DeviceProfiles,
        );
        assert!(
            out.contains("[Windows DeviceProfile]"),
            "missing section should be created:\n{out}"
        );
        assert!(
            out.contains("+CVars=r.Foo=1"),
            "cvar should be inserted:\n{out}"
        );
    }

    #[test]
    fn detection_picks_up_key_set_only_in_base_engine() {
        let base = "[ConsoleVariables]\nr.OnlyInBase=7\n";
        let synthetic = merge_to_synthetic_quad(base, "", "", "", true, false, false, false);
        assert!(
            synthetic.contains("r.OnlyInBase=7"),
            "synthetic merge should surface BaseEngine value:\n{synthetic}"
        );
    }

    #[test]
    fn windows_engine_overrides_base_engine() {
        let base = "[ConsoleVariables]\nr.Shared=1\n";
        let windows = "[ConsoleVariables]\nr.Shared=9\n";
        let synthetic = merge_to_synthetic_quad(base, "", windows, "", true, false, true, false);
        assert!(
            synthetic.contains("r.Shared=9"),
            "WindowsEngine value should win over BaseEngine:\n{synthetic}"
        );
        assert!(
            !synthetic.contains("r.Shared=1"),
            "BaseEngine value should not appear:\n{synthetic}"
        );
    }

    #[test]
    fn default_engine_overrides_base_engine() {
        let base = "[ConsoleVariables]\nr.Shared=1\n";
        let default = "[ConsoleVariables]\nr.Shared=5\n";
        let synthetic = merge_to_synthetic_quad(base, default, "", "", true, true, false, false);
        assert!(
            synthetic.contains("r.Shared=5"),
            "DefaultEngine value should win over BaseEngine:\n{synthetic}"
        );
        assert!(
            !synthetic.contains("r.Shared=1"),
            "BaseEngine value should not appear:\n{synthetic}"
        );
    }

    #[test]
    fn device_profiles_wins_over_windows_engine() {
        let windows = "[ConsoleVariables]\nr.Shared=9\n";
        let dp = "[Windows DeviceProfile]\n+CVars=r.Shared=42\n";
        let synthetic = merge_to_synthetic_quad("", "", windows, dp, false, false, true, true);
        assert!(
            synthetic.contains("r.Shared=42"),
            "DeviceProfiles value should still win at runtime:\n{synthetic}"
        );
    }

    #[test]
    fn apply_to_windows_target_syncs_existing_key_in_default_and_base() {
        let base = "[ConsoleVariables]\nr.Foo=0\n";
        let default = "[ConsoleVariables]\nr.Foo=0\n";
        let windows = "[ConsoleVariables]\nr.Foo=0\n";
        let (base_after, default_after, windows_after, _) = apply_to_quad(
            base,
            default,
            windows,
            "",
            true,
            true,
            true,
            false,
            &[cvar_edit("r.Foo", Some("1"))],
            PakIniTarget::WindowsEngine,
        );
        assert!(
            windows_after.contains("r.Foo=1"),
            "WindowsEngine target gets the new value:\n{windows_after}"
        );
        assert!(
            default_after.contains("r.Foo=1"),
            "DefaultEngine sibling updated to avoid shadow:\n{default_after}"
        );
        assert!(
            base_after.contains("r.Foo=1"),
            "BaseEngine sibling updated to avoid shadow:\n{base_after}"
        );
    }

    #[test]
    fn apply_to_base_target_does_not_inject_into_higher_priority_siblings() {
        let base = "[ConsoleVariables]\n";
        let default = "[ConsoleVariables]\n";
        let windows = "[ConsoleVariables]\n";
        let (base_after, default_after, windows_after, _) = apply_to_quad(
            base,
            default,
            windows,
            "",
            true,
            true,
            true,
            false,
            &[cvar_edit("r.Foo", Some("1"))],
            PakIniTarget::BaseEngine,
        );
        assert!(
            base_after.contains("r.Foo=1"),
            "BaseEngine target gets the new value:\n{base_after}"
        );
        assert!(
            !default_after.to_ascii_lowercase().contains("r.foo"),
            "DefaultEngine should not be injected:\n{default_after}"
        );
        assert!(
            !windows_after.to_ascii_lowercase().contains("r.foo"),
            "WindowsEngine should not be injected:\n{windows_after}"
        );
    }

    #[test]
    fn removal_clears_key_from_all_four_files() {
        let base = "[ConsoleVariables]\nr.Foo=0\n";
        let default = "[ConsoleVariables]\nr.Foo=0\n";
        let windows = "[ConsoleVariables]\nr.Foo=0\n";
        let dp = "[Windows DeviceProfile]\n+CVars=r.Foo=0\n";
        let (base_after, default_after, windows_after, dp_after) = apply_to_quad(
            base,
            default,
            windows,
            dp,
            true,
            true,
            true,
            true,
            &[cvar_edit("r.Foo", None)],
            PakIniTarget::DeviceProfiles,
        );
        for (name, content) in [
            ("base", &base_after),
            ("default", &default_after),
            ("windows", &windows_after),
            ("dp", &dp_after),
        ] {
            assert!(
                !content.to_ascii_lowercase().contains("r.foo"),
                "{name} should be cleared:\n{content}"
            );
        }
    }

    #[test]
    fn engine_section_edit_routes_to_highest_engine_when_target_is_device_profiles() {
        // ApplicationScale lives in [/Script/Engine.UserInterfaceSettings], not [ConsoleVariables].
        // When user targets DeviceProfiles, engine-section edits must still land in an
        // engine file -- the highest-priority one present.
        let default = "[/Script/Engine.UserInterfaceSettings]\n";
        let windows = "[/Script/Engine.UserInterfaceSettings]\n";
        let dp = "[Windows DeviceProfile]\n";
        let edit = PakTweakEdit {
            key: "ApplicationScale".into(),
            value: Some("1.5".into()),
            engine_section: Some("/Script/Engine.UserInterfaceSettings".into()),
        };
        let (_, default_after, windows_after, dp_after) = apply_to_quad(
            "",
            default,
            windows,
            dp,
            false,
            true,
            true,
            true,
            &[edit],
            PakIniTarget::DeviceProfiles,
        );
        assert!(
            windows_after.contains("ApplicationScale=1.5"),
            "engine-section edit should land in WindowsEngine (highest engine present):\n{windows_after}"
        );
        assert!(
            !default_after.contains("ApplicationScale="),
            "DefaultEngine should not be injected:\n{default_after}"
        );
        assert!(
            !dp_after.to_ascii_lowercase().contains("applicationscale"),
            "DeviceProfiles should never receive engine-section keys:\n{dp_after}"
        );
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod device_profile_tests {
    use super::*;

    /// A combo pak: several mods' configs concatenated, so the profile header and the keys under it
    /// both appear more than once.
    fn combo() -> String {
        [
            "[Windows DeviceProfile]",
            "+CVars=r.PostProcessing.DisableMaterials=1",
            "+CVars=r.CustomDepth=0",
            "",
            "[SomeOtherSection]",
            "Unrelated=1",
            "",
            "[Windows DeviceProfile]",
            "+CVars=r.PostProcessing.DisableMaterials=1",
            "+CVars=r.CustomDepth=0",
            "",
        ]
        .join("\r\n")
    }

    fn edit(key: &str, value: Option<&str>) -> PakTweakEdit {
        PakTweakEdit {
            key: key.to_string(),
            value: value.map(str::to_string),
            engine_section: None,
        }
    }

    fn value_of(content: &str, key: &str) -> Option<String> {
        parse_console_vars(content, "DefaultDeviceProfiles.ini")
            .into_iter()
            .rfind(|v| v.key.eq_ignore_ascii_case(key))
            .map(|v| v.value)
    }

    #[test]
    fn removal_clears_the_key_in_every_section() {
        let out = apply_edits_to_ini(
            &combo(),
            &[edit("r.PostProcessing.DisableMaterials", None)],
            IniType::DeviceProfiles,
        );
        assert_eq!(value_of(&out, "r.PostProcessing.DisableMaterials"), None);
        assert!(!out.to_ascii_lowercase().contains("disablematerials"));
        // The untouched key survives in both sections.
        assert_eq!(out.matches("r.CustomDepth=0").count(), 2);
    }

    #[test]
    fn set_collapses_duplicates_to_a_single_line() {
        let out = apply_edits_to_ini(
            &combo(),
            &[edit("r.CustomDepth", Some("3"))],
            IniType::DeviceProfiles,
        );
        assert_eq!(value_of(&out, "r.CustomDepth").as_deref(), Some("3"));
        assert_eq!(out.matches("r.CustomDepth=").count(), 1);
        assert!(!out.contains("r.CustomDepth=0"));
    }

    /// The bug that grew the user's INI: with the key never found, every save appended another line.
    #[test]
    fn repeated_sets_do_not_grow_the_file() {
        let once = apply_edits_to_ini(
            &combo(),
            &[edit("r.CustomDepth", Some("3"))],
            IniType::DeviceProfiles,
        );
        let twice = apply_edits_to_ini(
            &once,
            &[edit("r.CustomDepth", Some("3"))],
            IniType::DeviceProfiles,
        );
        assert_eq!(once, twice);
    }

    #[test]
    fn a_set_lands_where_a_read_will_find_it() {
        let out = apply_edits_to_ini(
            &combo(),
            &[edit("r.NewKey", Some("7"))],
            IniType::DeviceProfiles,
        );
        assert_eq!(value_of(&out, "r.NewKey").as_deref(), Some("7"));
    }

    #[test]
    fn a_removal_never_creates_a_section() {
        let out = apply_edits_to_ini(
            "[SomeOtherSection]\r\nUnrelated=1\r\n",
            &[edit("r.CustomDepth", None)],
            IniType::DeviceProfiles,
        );
        assert!(!out.contains("[Windows DeviceProfile]"));
    }
}
