//! Detects which catalogue tweaks are active in flat key=value INI content.
//!
//! Used for pak INI content (Engine.ini / DeviceProfiles.ini) merged into a single
//! key/value stream where section structure has already been collapsed, so a tweak's
//! `section` plays no part in detection.

use super::catalogue::{TweakDefinition, TweakKind, TweakState};
use super::parser::{find_key_value, pattern_present_anywhere, values_equal};

/// Detect the state of each tweak from flat key=value content with no section structure.
pub fn detect_active_tweaks_unscoped(
    content: &str,
    catalogue: &[TweakDefinition],
) -> Vec<TweakState> {
    catalogue.iter().map(|t| detect_one(content, t)).collect()
}

fn detect_one(content: &str, tweak: &TweakDefinition) -> TweakState {
    match &tweak.kind {
        TweakKind::RemoveLines { lines, .. } => {
            // `replace_with` semantics need full-pattern matching so a replaced value
            // (e.g. `r.X=0` → `r.X=3`) doesn't count as the original still being present.
            let any_found = lines
                .iter()
                .any(|entry| pattern_present_anywhere(content, &entry.pattern));
            TweakState {
                id: tweak.id.clone(),
                active: !any_found,
                current_value: None,
            }
        }
        TweakKind::Toggle {
            key,
            on_value,
            default_enabled,
            ..
        } => {
            let current = find_key_value(content, key);
            // Case-insensitive: UE bool cvars can ship as `True`/`true`/`TRUE`.
            let active = match current.as_deref() {
                Some(v) => v.eq_ignore_ascii_case(on_value),
                None => *default_enabled,
            };
            TweakState {
                id: tweak.id.clone(),
                active,
                current_value: current,
            }
        }
        TweakKind::BatchToggle {
            entries,
            default_enabled,
        } => {
            let active = entries.iter().all(|entry| {
                let current = find_key_value(content, &entry.key);
                match current.as_deref() {
                    Some(v) => v == entry.on_value.as_str(),
                    None => *default_enabled,
                }
            });
            TweakState {
                id: tweak.id.clone(),
                active,
                current_value: None,
            }
        }
        TweakKind::Slider {
            key,
            default_value,
            write_default_on_disable,
            ..
        } => {
            let current = find_key_value(content, key);
            let active = match (&current, *write_default_on_disable) {
                (Some(v), true) => !values_equal(v, *default_value),
                (Some(_), false) => true,
                (None, _) => false,
            };
            TweakState {
                id: tweak.id.clone(),
                active,
                current_value: current,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tweaks::catalogue::{BatchToggleEntry, TweakLine};

    fn remove_lines_def(pattern: &str) -> TweakDefinition {
        TweakDefinition {
            id: "t".into(),
            label: "t".into(),
            category: "test".into(),
            description: String::new(),
            pak_only: false,
            kind: TweakKind::RemoveLines {
                lines: vec![TweakLine {
                    pattern: pattern.into(),
                    engine_section: None,
                    replace_with: None,
                }],
                remove_only: false,
            },
        }
    }

    fn remove_lines_with_replace(pattern: &str, replace_with: &str) -> TweakDefinition {
        TweakDefinition {
            id: "t".into(),
            label: "t".into(),
            category: "test".into(),
            description: String::new(),
            pak_only: false,
            kind: TweakKind::RemoveLines {
                lines: vec![TweakLine {
                    pattern: pattern.into(),
                    engine_section: None,
                    replace_with: Some(replace_with.into()),
                }],
                remove_only: false,
            },
        }
    }

    fn toggle_def(
        key: &str,
        on: &str,
        off: Option<&str>,
        default_enabled: bool,
    ) -> TweakDefinition {
        TweakDefinition {
            id: "t".into(),
            label: "t".into(),
            category: "test".into(),
            description: String::new(),
            pak_only: false,
            kind: TweakKind::Toggle {
                key: key.into(),
                on_value: on.into(),
                off_value: off.map(String::from),
                default_enabled,
                section: None,
                engine_section: None,
            },
        }
    }

    fn slider_def(
        key: &str,
        default_value: f64,
        write_default_on_disable: bool,
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
                write_default_on_disable,
                section: None,
                engine_section: None,
            },
        }
    }

    fn batch_def(entries: Vec<BatchToggleEntry>, default_enabled: bool) -> TweakDefinition {
        TweakDefinition {
            id: "t".into(),
            label: "t".into(),
            category: "test".into(),
            description: String::new(),
            pak_only: false,
            kind: TweakKind::BatchToggle {
                entries,
                default_enabled,
            },
        }
    }

    // ── RemoveLines ──

    #[test]
    fn remove_lines_active_when_pattern_absent() {
        let def = remove_lines_def("r.LightTile.Enable=0");
        let content = "r.SomethingElse=1\n";
        let states = detect_active_tweaks_unscoped(content, &[def]);
        assert!(states[0].active);
    }

    #[test]
    fn remove_lines_inactive_when_pattern_present() {
        let def = remove_lines_def("r.LightTile.Enable=0");
        let content = "r.LightTile.Enable=0\n";
        let states = detect_active_tweaks_unscoped(content, &[def]);
        assert!(!states[0].active);
    }

    #[test]
    fn remove_lines_active_after_replace_with_applied() {
        // Pattern "r.CustomDepth=0" replaced by "r.CustomDepth=3".
        // Pak content has the replacement value — tweak should read as ACTIVE.
        let def = remove_lines_with_replace("r.CustomDepth=0", "r.CustomDepth=3");
        let content = "r.CustomDepth=3\n";
        let states = detect_active_tweaks_unscoped(content, &[def]);
        assert!(
            states[0].active,
            "replacement value present means tweak is on, not off"
        );
    }

    #[test]
    fn remove_lines_inactive_when_original_pattern_present() {
        let def = remove_lines_with_replace("r.CustomDepth=0", "r.CustomDepth=3");
        let content = "r.CustomDepth=0\n";
        let states = detect_active_tweaks_unscoped(content, &[def]);
        assert!(
            !states[0].active,
            "original value present means tweak is off"
        );
    }

    #[test]
    fn remove_lines_handles_cvars_prefix() {
        let def = remove_lines_def("r.LightTile.Enable=0");
        let content = "+CVars=r.LightTile.Enable=0\n";
        let states = detect_active_tweaks_unscoped(content, &[def]);
        assert!(
            !states[0].active,
            "+CVars= prefix must be normalized for pattern matching"
        );
    }

    // ── Toggle ──

    #[test]
    fn toggle_active_when_on_value_present() {
        let def = toggle_def("r.X", "1", Some("0"), false);
        let content = "r.X=1\n";
        let states = detect_active_tweaks_unscoped(content, &[def]);
        assert!(states[0].active);
        assert_eq!(states[0].current_value.as_deref(), Some("1"));
    }

    #[test]
    fn toggle_inactive_when_off_value_present() {
        let def = toggle_def("r.X", "1", Some("0"), false);
        let content = "r.X=0\n";
        let states = detect_active_tweaks_unscoped(content, &[def]);
        assert!(!states[0].active);
    }

    #[test]
    fn toggle_default_enabled_fallback_when_absent() {
        let def = toggle_def("r.X", "1", Some("0"), true);
        let content = "r.OtherKey=1\n";
        let states = detect_active_tweaks_unscoped(content, &[def]);
        assert!(states[0].active);
    }

    #[test]
    fn toggle_case_insensitive_on_value_match() {
        // UE bool cvars ship as `True`/`true`/`TRUE`. Detector must not split
        // hairs over case.
        let def = toggle_def("r.X", "True", Some("False"), false);
        let content = "r.X=true\n";
        let states = detect_active_tweaks_unscoped(content, &[def]);
        assert!(
            states[0].active,
            "lowercase `true` must match catalogue `True`"
        );
    }

    // ── Slider ──

    #[test]
    fn slider_active_when_non_default() {
        let def = slider_def("r.X", 1.0, true);
        let content = "r.X=5\n";
        let states = detect_active_tweaks_unscoped(content, &[def]);
        assert!(states[0].active);
        assert_eq!(states[0].current_value.as_deref(), Some("5"));
    }

    #[test]
    fn slider_inactive_at_default_when_write_default_on_disable() {
        let def = slider_def("r.X", 1.0, true);
        let content = "r.X=1\n";
        let states = detect_active_tweaks_unscoped(content, &[def]);
        assert!(!states[0].active);
    }

    #[test]
    fn slider_active_when_value_set_and_no_write_default() {
        let def = slider_def("r.X", 1.0, false);
        let content = "r.X=1\n";
        let states = detect_active_tweaks_unscoped(content, &[def]);
        assert!(
            states[0].active,
            "without write_default_on_disable, any presence is active"
        );
    }

    // ── BatchToggle ──

    #[test]
    fn batch_toggle_active_when_all_entries_match() {
        let entries = vec![
            BatchToggleEntry {
                key: "r.A".into(),
                on_value: "1".into(),
                off_value: None,
                engine_section: None,
            },
            BatchToggleEntry {
                key: "r.B".into(),
                on_value: "2".into(),
                off_value: None,
                engine_section: None,
            },
        ];
        let def = batch_def(entries, false);
        let content = "r.A=1\nr.B=2\n";
        let states = detect_active_tweaks_unscoped(content, &[def]);
        assert!(states[0].active);
    }

    #[test]
    fn batch_toggle_inactive_when_one_mismatches() {
        let entries = vec![
            BatchToggleEntry {
                key: "r.A".into(),
                on_value: "1".into(),
                off_value: None,
                engine_section: None,
            },
            BatchToggleEntry {
                key: "r.B".into(),
                on_value: "2".into(),
                off_value: None,
                engine_section: None,
            },
        ];
        let def = batch_def(entries, false);
        let content = "r.A=1\nr.B=99\n";
        let states = detect_active_tweaks_unscoped(content, &[def]);
        assert!(!states[0].active);
    }
}
