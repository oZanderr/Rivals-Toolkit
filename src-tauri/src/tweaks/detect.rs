//! Detects which catalogue tweaks are currently active in INI content.

use super::catalogue::{TweakDefinition, TweakKind, TweakState};
use super::parser::{
    find_key_value_in_section, key_present_in_section, pattern_key_lower, values_equal,
};

/// Detect the current state of each tweak from INI content.
pub(crate) fn detect_active_tweaks(
    content: &str,
    catalogue: &[TweakDefinition],
) -> Vec<TweakState> {
    catalogue.iter().map(|t| detect_one(content, t)).collect()
}

fn detect_one(content: &str, tweak: &TweakDefinition) -> TweakState {
    match &tweak.kind {
        TweakKind::RemoveLines { lines, .. } => {
            // UE reads cvars only from declared sections, so out-of-section dupes don't apply.
            let any_found = lines.iter().any(|entry| {
                let Some(section) = &entry.scalability_section else {
                    return false;
                };
                let key_lower = pattern_key_lower(&entry.pattern);
                key_present_in_section(content, section, &key_lower)
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
            default_enabled,
            scalability_section,
            ..
        } => {
            let current = scalability_section
                .as_deref()
                .and_then(|s| find_key_value_in_section(content, s, key));
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
        TweakKind::BatchToggle {
            entries,
            default_enabled,
        } => {
            let active = entries.iter().all(|entry| {
                let current = entry
                    .scalability_section
                    .as_deref()
                    .and_then(|s| find_key_value_in_section(content, s, &entry.key));
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
            scalability_section,
            ..
        } => {
            let current = scalability_section
                .as_deref()
                .and_then(|s| find_key_value_in_section(content, s, key));
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

    fn remove_lines_def(pattern: &str, section: Option<&str>) -> TweakDefinition {
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
                    replace_with: None,
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
        section: Option<&str>,
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
                scalability_section: section.map(String::from),
                engine_section: None,
            },
        }
    }

    fn slider_def(
        key: &str,
        default_value: f64,
        write_default_on_disable: bool,
        section: Option<&str>,
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
                scalability_section: section.map(String::from),
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
    fn remove_lines_active_when_key_absent_in_target_section() {
        let def = remove_lines_def("r.MipMapLODBias=15", Some("TextureQuality@0"));
        let content = "[TextureQuality@0]\n";
        let states = detect_active_tweaks(content, &[def]);
        assert!(states[0].active);
    }

    #[test]
    fn remove_lines_inactive_when_pattern_in_target_section() {
        let def = remove_lines_def("r.MipMapLODBias=15", Some("TextureQuality@0"));
        let content = "[TextureQuality@0]\nr.MipMapLODBias=15\n";
        let states = detect_active_tweaks(content, &[def]);
        assert!(!states[0].active);
    }

    #[test]
    fn remove_lines_inactive_when_any_value_in_target_section() {
        let def = remove_lines_def("r.MipMapLODBias=15", Some("TextureQuality@0"));
        let content = "[TextureQuality@0]\nr.MipMapLODBias=16\n";
        let states = detect_active_tweaks(content, &[def]);
        assert!(
            !states[0].active,
            "any value in target section blocks tweak"
        );
    }

    #[test]
    fn remove_lines_ignores_random_section_with_different_value() {
        let def = remove_lines_def("r.MipMapLODBias=15", Some("TextureQuality@0"));
        let content = "[TextureQuality@0]\n\n[Random]\nr.MipMapLODBias=16\n";
        let states = detect_active_tweaks(content, &[def]);
        assert!(
            states[0].active,
            "random section dupe must not flip detection"
        );
    }

    #[test]
    fn remove_lines_ignores_random_section_with_matching_value() {
        let def = remove_lines_def("r.MipMapLODBias=15", Some("TextureQuality@0"));
        let content = "[TextureQuality@0]\n\n[Random]\nr.MipMapLODBias=15\n";
        let states = detect_active_tweaks(content, &[def]);
        assert!(
            states[0].active,
            "matching value outside target section is inert in UE"
        );
    }

    #[test]
    fn remove_lines_pak_only_entry_skipped_in_scalability() {
        let def = remove_lines_def("p.SimCollisionEnabled=0", None);
        let content = "[Whatever]\np.SimCollisionEnabled=0\n";
        let states = detect_active_tweaks(content, &[def]);
        assert!(
            states[0].active,
            "pak-only entry has no scalability presence"
        );
    }

    // ── Toggle ──

    #[test]
    fn toggle_active_when_on_value_in_target_section() {
        let def = toggle_def("r.X", "1", Some("0"), false, Some("EffectsQuality@0"));
        let content = "[EffectsQuality@0]\nr.X=1\n";
        let states = detect_active_tweaks(content, &[def]);
        assert!(states[0].active);
    }

    #[test]
    fn toggle_inactive_when_off_value_in_target_section() {
        let def = toggle_def("r.X", "1", Some("0"), false, Some("EffectsQuality@0"));
        let content = "[EffectsQuality@0]\nr.X=0\n";
        let states = detect_active_tweaks(content, &[def]);
        assert!(!states[0].active);
    }

    #[test]
    fn toggle_ignores_random_section_value() {
        let def = toggle_def("r.X", "1", Some("0"), false, Some("EffectsQuality@0"));
        let content = "[EffectsQuality@0]\n\n[Random]\nr.X=1\n";
        let states = detect_active_tweaks(content, &[def]);
        assert!(
            !states[0].active,
            "on_value pasted in random section must not activate detection"
        );
    }

    #[test]
    fn toggle_default_enabled_fallback_when_absent() {
        let def = toggle_def("r.X", "1", Some("0"), true, Some("EffectsQuality@0"));
        let content = "[EffectsQuality@0]\n";
        let states = detect_active_tweaks(content, &[def]);
        assert!(states[0].active);
    }

    #[test]
    fn toggle_uses_last_value_within_section() {
        let def = toggle_def("r.X", "1", Some("0"), false, Some("EffectsQuality@0"));
        let content = "[EffectsQuality@0]\nr.X=0\nr.X=1\n";
        let states = detect_active_tweaks(content, &[def]);
        assert!(states[0].active, "last-wins inside section");
    }

    // ── Slider ──

    #[test]
    fn slider_active_when_non_default_in_target_section() {
        let def = slider_def("r.X", 0.0, true, Some("EffectsQuality@0"));
        let content = "[EffectsQuality@0]\nr.X=5\n";
        let states = detect_active_tweaks(content, &[def]);
        assert!(states[0].active);
        assert_eq!(states[0].current_value.as_deref(), Some("5"));
    }

    #[test]
    fn slider_inactive_at_default_when_write_default_on_disable() {
        let def = slider_def("r.X", 0.0, true, Some("EffectsQuality@0"));
        let content = "[EffectsQuality@0]\nr.X=0\n";
        let states = detect_active_tweaks(content, &[def]);
        assert!(!states[0].active);
    }

    #[test]
    fn slider_ignores_random_section_value() {
        let def = slider_def("r.X", 0.0, true, Some("EffectsQuality@0"));
        let content = "[EffectsQuality@0]\n\n[Random]\nr.X=5\n";
        let states = detect_active_tweaks(content, &[def]);
        assert!(
            !states[0].active,
            "value in random section must not register"
        );
        assert!(states[0].current_value.is_none());
    }

    // ── BatchToggle ──

    #[test]
    fn batch_toggle_active_when_all_entries_match_on_value() {
        let entries = vec![
            BatchToggleEntry {
                key: "r.A".into(),
                on_value: "1".into(),
                off_value: None,
                scalability_section: Some("S".into()),
                engine_section: None,
            },
            BatchToggleEntry {
                key: "r.B".into(),
                on_value: "2".into(),
                off_value: None,
                scalability_section: Some("S".into()),
                engine_section: None,
            },
        ];
        let def = batch_def(entries, false);
        let content = "[S]\nr.A=1\nr.B=2\n";
        let states = detect_active_tweaks(content, &[def]);
        assert!(states[0].active);
    }

    #[test]
    fn batch_toggle_inactive_when_one_entry_mismatches() {
        let entries = vec![
            BatchToggleEntry {
                key: "r.A".into(),
                on_value: "1".into(),
                off_value: None,
                scalability_section: Some("S".into()),
                engine_section: None,
            },
            BatchToggleEntry {
                key: "r.B".into(),
                on_value: "2".into(),
                off_value: None,
                scalability_section: Some("S".into()),
                engine_section: None,
            },
        ];
        let def = batch_def(entries, false);
        let content = "[S]\nr.A=1\nr.B=99\n";
        let states = detect_active_tweaks(content, &[def]);
        assert!(!states[0].active);
    }

    #[test]
    fn batch_toggle_ignores_random_section_dupe() {
        let entries = vec![BatchToggleEntry {
            key: "r.A".into(),
            on_value: "1".into(),
            off_value: None,
            scalability_section: Some("S".into()),
            engine_section: None,
        }];
        let def = batch_def(entries, false);
        let content = "[S]\n\n[Random]\nr.A=1\n";
        let states = detect_active_tweaks(content, &[def]);
        assert!(
            !states[0].active,
            "default_enabled=false plus random dupe must stay inactive"
        );
    }
}
