//! Detects which catalogue tweaks are currently active in INI content.
//!
//! Two lookup strategies share the same per-kind detection logic via the
//! [`Lookup`] trait: [`Scoped`] enforces UE's section-strict semantics for
//! Scalability.ini; [`Unscoped`] accepts flat key=value content for pak INI
//! detection where sections have already been collapsed.

use super::catalogue::{TweakDefinition, TweakKind, TweakLine, TweakState};
use super::parser::{
    find_key_value, find_key_value_in_section, key_present_in_section, pattern_key_lower,
    pattern_present_anywhere, values_equal,
};

trait Lookup {
    /// Find the current value for `key` within the optional scalability section.
    fn find_value(
        &self,
        content: &str,
        key: &str,
        scalability_section: Option<&str>,
    ) -> Option<String>;

    /// Whether a `RemoveLines` entry is currently "blocked" (present and thus
    /// keeping the tweak in the OFF state). Implementations choose between
    /// key-only-in-section and full-pattern-anywhere semantics.
    fn line_blocks_remove(&self, content: &str, line: &TweakLine) -> bool;
}

/// Section-strict lookup. UE reads cvars only from declared sections, so
/// out-of-section dupes don't apply. Used for Scalability.ini detection.
struct Scoped;
impl Lookup for Scoped {
    fn find_value(
        &self,
        content: &str,
        key: &str,
        scalability_section: Option<&str>,
    ) -> Option<String> {
        scalability_section.and_then(|s| find_key_value_in_section(content, s, key))
    }

    fn line_blocks_remove(&self, content: &str, line: &TweakLine) -> bool {
        let Some(section) = line.scalability_section.as_deref() else {
            return false;
        };
        let key_lower = pattern_key_lower(&line.pattern);
        key_present_in_section(content, section, &key_lower)
    }
}

/// Section-agnostic lookup over flat key=value content. Used for pak detection
/// where Engine.ini and DeviceProfiles.ini have been merged into a single
/// stream and `replace_with` semantics need full-pattern matching so a replaced
/// value (e.g. `r.X=0` → `r.X=3`) doesn't count as the original still-present.
struct Unscoped;
impl Lookup for Unscoped {
    fn find_value(&self, content: &str, key: &str, _section: Option<&str>) -> Option<String> {
        find_key_value(content, key)
    }

    fn line_blocks_remove(&self, content: &str, line: &TweakLine) -> bool {
        pattern_present_anywhere(content, &line.pattern)
    }
}

/// Detect the current state of each tweak using section-scoped lookups.
/// Use for Scalability.ini where keys must be in their declared section to apply.
pub(crate) fn detect_active_tweaks(
    content: &str,
    catalogue: &[TweakDefinition],
) -> Vec<TweakState> {
    catalogue
        .iter()
        .map(|t| detect_one(content, t, &Scoped))
        .collect()
}

/// Detect tweak state from flat key=value content with no section structure.
/// Use for pak INI content (Engine.ini / DeviceProfiles.ini) merged into a single
/// key/value stream where the catalogue's `scalability_section` doesn't apply.
pub(crate) fn detect_active_tweaks_unscoped(
    content: &str,
    catalogue: &[TweakDefinition],
) -> Vec<TweakState> {
    catalogue
        .iter()
        .map(|t| detect_one(content, t, &Unscoped))
        .collect()
}

fn detect_one<L: Lookup>(content: &str, tweak: &TweakDefinition, lookup: &L) -> TweakState {
    match &tweak.kind {
        TweakKind::RemoveLines { lines, .. } => {
            let any_found = lines
                .iter()
                .any(|entry| lookup.line_blocks_remove(content, entry));
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
            let current = lookup.find_value(content, key, scalability_section.as_deref());
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
                let current =
                    lookup.find_value(content, &entry.key, entry.scalability_section.as_deref());
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
            let current = lookup.find_value(content, key, scalability_section.as_deref());
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

    #[test]
    fn toggle_case_insensitive_on_value_match() {
        // UE bool cvars ship as `True`/`true`/`TRUE`. Detector must not split
        // hairs over case.
        let def = toggle_def(
            "r.X",
            "True",
            Some("False"),
            false,
            Some("EffectsQuality@0"),
        );
        let content = "[EffectsQuality@0]\nr.X=true\n";
        let states = detect_active_tweaks(content, &[def]);
        assert!(
            states[0].active,
            "lowercase `true` must match catalogue `True`"
        );
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

    // ── Unscoped detector (pak path) ──

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
                    scalability_section: None,
                    engine_section: None,
                    replace_with: Some(replace_with.into()),
                }],
                remove_only: false,
            },
        }
    }

    #[test]
    fn unscoped_remove_lines_active_when_pattern_absent() {
        let def = remove_lines_def("r.LightTile.Enable=0", None);
        let content = "r.SomethingElse=1\n";
        let states = detect_active_tweaks_unscoped(content, &[def]);
        assert!(states[0].active);
    }

    #[test]
    fn unscoped_remove_lines_inactive_when_pattern_present() {
        let def = remove_lines_def("r.LightTile.Enable=0", None);
        let content = "r.LightTile.Enable=0\n";
        let states = detect_active_tweaks_unscoped(content, &[def]);
        assert!(!states[0].active);
    }

    #[test]
    fn unscoped_remove_lines_active_after_replace_with_applied() {
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
    fn unscoped_remove_lines_inactive_when_original_pattern_present() {
        let def = remove_lines_with_replace("r.CustomDepth=0", "r.CustomDepth=3");
        let content = "r.CustomDepth=0\n";
        let states = detect_active_tweaks_unscoped(content, &[def]);
        assert!(
            !states[0].active,
            "original value present means tweak is off"
        );
    }

    #[test]
    fn unscoped_remove_lines_handles_cvars_prefix() {
        let def = remove_lines_def("r.LightTile.Enable=0", None);
        let content = "+CVars=r.LightTile.Enable=0\n";
        let states = detect_active_tweaks_unscoped(content, &[def]);
        assert!(
            !states[0].active,
            "+CVars= prefix must be normalized for pattern matching"
        );
    }

    #[test]
    fn unscoped_toggle_active_when_on_value_present() {
        let def = toggle_def("r.X", "1", Some("0"), false, None);
        let content = "r.X=1\n";
        let states = detect_active_tweaks_unscoped(content, &[def]);
        assert!(states[0].active);
        assert_eq!(states[0].current_value.as_deref(), Some("1"));
    }

    #[test]
    fn unscoped_toggle_inactive_when_off_value_present() {
        let def = toggle_def("r.X", "1", Some("0"), false, None);
        let content = "r.X=0\n";
        let states = detect_active_tweaks_unscoped(content, &[def]);
        assert!(!states[0].active);
    }

    #[test]
    fn unscoped_toggle_default_enabled_fallback_when_absent() {
        let def = toggle_def("r.X", "1", Some("0"), true, None);
        let content = "r.OtherKey=1\n";
        let states = detect_active_tweaks_unscoped(content, &[def]);
        assert!(states[0].active);
    }

    #[test]
    fn unscoped_slider_active_when_non_default() {
        let def = slider_def("r.X", 1.0, true, None);
        let content = "r.X=5\n";
        let states = detect_active_tweaks_unscoped(content, &[def]);
        assert!(states[0].active);
        assert_eq!(states[0].current_value.as_deref(), Some("5"));
    }

    #[test]
    fn unscoped_slider_inactive_at_default_when_write_default_on_disable() {
        let def = slider_def("r.X", 1.0, true, None);
        let content = "r.X=1\n";
        let states = detect_active_tweaks_unscoped(content, &[def]);
        assert!(!states[0].active);
    }

    #[test]
    fn unscoped_slider_active_when_value_set_and_no_write_default() {
        let def = slider_def("r.X", 1.0, false, None);
        let content = "r.X=1\n";
        let states = detect_active_tweaks_unscoped(content, &[def]);
        assert!(
            states[0].active,
            "without write_default_on_disable, any presence is active"
        );
    }

    #[test]
    fn unscoped_batch_toggle_active_when_all_entries_match() {
        let entries = vec![
            BatchToggleEntry {
                key: "r.A".into(),
                on_value: "1".into(),
                off_value: None,
                scalability_section: None,
                engine_section: None,
            },
            BatchToggleEntry {
                key: "r.B".into(),
                on_value: "2".into(),
                off_value: None,
                scalability_section: None,
                engine_section: None,
            },
        ];
        let def = batch_def(entries, false);
        let content = "r.A=1\nr.B=2\n";
        let states = detect_active_tweaks_unscoped(content, &[def]);
        assert!(states[0].active);
    }

    #[test]
    fn unscoped_batch_toggle_inactive_when_one_mismatches() {
        let entries = vec![
            BatchToggleEntry {
                key: "r.A".into(),
                on_value: "1".into(),
                off_value: None,
                scalability_section: None,
                engine_section: None,
            },
            BatchToggleEntry {
                key: "r.B".into(),
                on_value: "2".into(),
                off_value: None,
                scalability_section: None,
                engine_section: None,
            },
        ];
        let def = batch_def(entries, false);
        let content = "r.A=1\nr.B=99\n";
        let states = detect_active_tweaks_unscoped(content, &[def]);
        assert!(!states[0].active);
    }
}
