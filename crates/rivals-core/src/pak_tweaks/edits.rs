//! Turns a catalogue tweak plus a desired on/off state into the concrete INI edits that realize it.

use crate::tweaks::{TweakDefinition, TweakKind, TweakSetting, catalogue::tweak_catalogue};

use super::PakTweakEdit;

/// Value half of `key=value`, or the whole string when there is no `=`.
fn value_part(pattern: &str) -> &str {
    match pattern.split_once('=') {
        Some((_, value)) => value,
        None => pattern,
    }
}

fn key_part(pattern: &str) -> &str {
    match pattern.split_once('=') {
        Some((key, _)) => key,
        None => pattern,
    }
}

/// Edits that put `def` into the requested state.
///
/// `value` is only consulted by `Slider` tweaks; every other kind takes its values from the
/// catalogue. Returns an error rather than a silent no-op when the request cannot be expressed,
/// so a script gets a non-zero exit instead of a pak it believes was edited.
pub fn edits_for_tweak(
    def: &TweakDefinition,
    enabled: bool,
    value: Option<&str>,
) -> Result<Vec<PakTweakEdit>, String> {
    let edit = |key: &str, value: Option<String>, section: Option<String>| PakTweakEdit {
        key: key.to_string(),
        value,
        engine_section: section,
    };

    match &def.kind {
        TweakKind::RemoveLines { lines, remove_only } => {
            if !enabled && *remove_only {
                return Err(format!(
                    "'{}' only removes lines and cannot be turned back off",
                    def.id
                ));
            }
            Ok(lines
                .iter()
                .map(|line| {
                    let replacement = line.replace_with.as_deref().map(value_part);
                    let new_value = if enabled {
                        replacement.map(str::to_string)
                    } else {
                        Some(value_part(&line.pattern).to_string())
                    };
                    edit(
                        key_part(&line.pattern),
                        new_value,
                        line.engine_section.clone(),
                    )
                })
                .collect())
        }
        TweakKind::Toggle {
            key,
            on_value,
            off_value,
            engine_section,
            ..
        } => {
            let new_value = if enabled {
                Some(on_value.clone())
            } else {
                off_value.clone()
            };
            Ok(vec![edit(key, new_value, engine_section.clone())])
        }
        TweakKind::BatchToggle { entries, .. } => Ok(entries
            .iter()
            .map(|entry| {
                let new_value = if enabled {
                    Some(entry.on_value.clone())
                } else {
                    entry.off_value.clone()
                };
                edit(&entry.key, new_value, entry.engine_section.clone())
            })
            .collect()),
        TweakKind::Slider {
            key,
            min,
            max,
            default_value,
            write_default_on_disable,
            engine_section,
            ..
        } => {
            let new_value = if enabled {
                match value {
                    Some(raw) => {
                        let requested = raw
                            .trim()
                            .parse::<f64>()
                            .map_err(|_| format!("'{raw}' is not a number"))?;
                        if requested < *min || requested > *max {
                            return Err(format!(
                                "{requested} is outside the {min}..{max} range of '{}'",
                                def.id
                            ));
                        }
                        // Written back as given: the GUI sends `toFixed()` output, and renormalizing
                        // `1.0` to `1` would rewrite every slider line on the first save.
                        Some(raw.trim().to_string())
                    }
                    None => Some(format_slider(*default_value)),
                }
            } else if *write_default_on_disable {
                Some(format_slider(*default_value))
            } else {
                None
            };
            Ok(vec![edit(key, new_value, engine_section.clone())])
        }
    }
}

/// Edits realizing every requested tweak state, resolved against the pak tweak catalogue.
///
/// Both front ends go through here, so the desktop app and the CLI cannot disagree about what a
/// tweak writes. Unknown and repeated ids are rejected rather than resolved to a winner, so a
/// caller never believes it applied a tweak it did not.
pub fn edits_for_settings(settings: &[TweakSetting]) -> Result<Vec<PakTweakEdit>, String> {
    let catalogue = tweak_catalogue();
    let mut edits = Vec::new();
    for (index, setting) in settings.iter().enumerate() {
        if settings[..index].iter().any(|prior| prior.id == setting.id) {
            return Err(format!("'{}' was requested more than once", setting.id));
        }
        let def = catalogue
            .iter()
            .find(|d| d.id == setting.id)
            .ok_or_else(|| format!("no tweak with id '{}'", setting.id))?;
        edits.extend(edits_for_tweak(
            def,
            setting.enabled,
            setting.value.as_deref(),
        )?);
    }
    Ok(edits)
}

/// Whole numbers write as integers; UE parses either, but `1` matches what the catalogue and the
/// GUI already put in these files.
fn format_slider(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::tweaks::catalogue::{TweakLine, tweak_catalogue};

    fn def(id: &str, kind: TweakKind) -> TweakDefinition {
        TweakDefinition {
            id: id.to_string(),
            label: id.to_string(),
            category: "test".to_string(),
            description: String::new(),
            pak_only: false,
            kind,
        }
    }

    fn line(pattern: &str, replace_with: Option<&str>) -> TweakLine {
        TweakLine {
            pattern: pattern.to_string(),
            engine_section: None,
            replace_with: replace_with.map(str::to_string),
        }
    }

    #[test]
    fn removing_a_line_with_no_replacement_deletes_the_key() {
        let d = def(
            "x",
            TweakKind::RemoveLines {
                lines: vec![line("r.Foo=1", None)],
                remove_only: false,
            },
        );
        let on = edits_for_tweak(&d, true, None).unwrap();
        assert_eq!(on[0].key, "r.Foo");
        assert_eq!(on[0].value, None);

        let off = edits_for_tweak(&d, false, None).unwrap();
        assert_eq!(off[0].value.as_deref(), Some("1"));
    }

    #[test]
    fn a_replacement_is_written_instead_of_removing() {
        let d = def(
            "x",
            TweakKind::RemoveLines {
                lines: vec![line("r.Foo=0", Some("r.Foo=3"))],
                remove_only: false,
            },
        );
        assert_eq!(
            edits_for_tweak(&d, true, None).unwrap()[0].value.as_deref(),
            Some("3")
        );
        assert_eq!(
            edits_for_tweak(&d, false, None).unwrap()[0]
                .value
                .as_deref(),
            Some("0")
        );
    }

    #[test]
    fn a_remove_only_tweak_refuses_to_turn_off() {
        let d = def(
            "x",
            TweakKind::RemoveLines {
                lines: vec![line("r.Foo=1", None)],
                remove_only: true,
            },
        );
        assert!(edits_for_tweak(&d, true, None).is_ok());
        assert!(edits_for_tweak(&d, false, None).is_err());
    }

    #[test]
    fn a_toggle_without_an_off_value_removes_the_key() {
        let d = def(
            "x",
            TweakKind::Toggle {
                key: "r.Foo".into(),
                on_value: "1".into(),
                off_value: None,
                default_enabled: false,
                section: None,
                engine_section: None,
            },
        );
        assert_eq!(
            edits_for_tweak(&d, true, None).unwrap()[0].value.as_deref(),
            Some("1")
        );
        assert_eq!(edits_for_tweak(&d, false, None).unwrap()[0].value, None);
    }

    #[test]
    fn a_slider_rejects_values_outside_its_range() {
        let d = def(
            "x",
            TweakKind::Slider {
                key: "r.Foo".into(),
                min: 0.0,
                max: 10.0,
                step: 1.0,
                default_value: 5.0,
                write_default_on_disable: false,
                section: None,
                engine_section: None,
            },
        );
        assert_eq!(
            edits_for_tweak(&d, true, Some("7")).unwrap()[0]
                .value
                .as_deref(),
            Some("7")
        );
        assert!(edits_for_tweak(&d, true, Some("11")).is_err());
        assert!(edits_for_tweak(&d, true, Some("abc")).is_err());
        // No value given falls back to the catalogue default.
        assert_eq!(
            edits_for_tweak(&d, true, None).unwrap()[0].value.as_deref(),
            Some("5")
        );
        assert_eq!(edits_for_tweak(&d, false, None).unwrap()[0].value, None);
    }

    #[test]
    fn a_slider_value_is_written_exactly_as_given() {
        let d = def(
            "x",
            TweakKind::Slider {
                key: "r.Foo".into(),
                min: 0.0,
                max: 10.0,
                step: 0.1,
                default_value: 5.0,
                write_default_on_disable: true,
                section: None,
                engine_section: None,
            },
        );
        // The GUI sends `toFixed()` output; renormalizing it would rewrite untouched slider lines.
        for given in ["1.0", "1", "2.50", " 3.5 "] {
            assert_eq!(
                edits_for_tweak(&d, true, Some(given)).unwrap()[0]
                    .value
                    .as_deref(),
                Some(given.trim())
            );
        }
        // Only the catalogue-default fallback is formatted by us.
        assert_eq!(
            edits_for_tweak(&d, true, None).unwrap()[0].value.as_deref(),
            Some("5")
        );
    }

    fn setting(id: &str, enabled: bool, value: Option<&str>) -> TweakSetting {
        TweakSetting {
            id: id.to_string(),
            enabled,
            value: value.map(str::to_string),
        }
    }

    #[test]
    fn settings_translate_against_the_catalogue() {
        let edits = edits_for_settings(&[
            setting("cas_sharpening", true, None),
            setting("fix_dark_maps", true, None),
        ])
        .unwrap();
        // One key from the toggle plus the three the RemoveLines tweak clears.
        assert_eq!(edits.len(), 4);
        assert!(edits.iter().any(|e| e.key == "r.PostProcessing.EnableCAS"));
        assert!(edits.iter().any(|e| e.key == "r.LightCullingDistance"));
    }

    #[test]
    fn an_unknown_id_is_rejected() {
        let err = edits_for_settings(&[setting("nope", true, None)]).unwrap_err();
        assert!(err.contains("nope"), "{err}");
    }

    #[test]
    fn a_repeated_id_is_rejected_rather_than_resolved() {
        let err = edits_for_settings(&[
            setting("cas_sharpening", true, None),
            setting("cas_sharpening", false, None),
        ])
        .unwrap_err();
        assert!(err.contains("more than once"), "{err}");
    }

    #[test]
    fn a_remove_only_settings_downgrade_is_rejected() {
        let remove_only = tweak_catalogue()
            .into_iter()
            .find(|d| {
                matches!(
                    d.kind,
                    TweakKind::RemoveLines {
                        remove_only: true,
                        ..
                    }
                )
            })
            .expect("catalogue has a remove-only tweak");
        assert!(edits_for_settings(&[setting(&remove_only.id, false, None)]).is_err());
        assert!(edits_for_settings(&[setting(&remove_only.id, true, None)]).is_ok());
    }

    /// The refactor that moved edit-building out of the frontend swapped per-cvar-key change
    /// tracking for per-tweak-id tracking. Those are only equivalent while no key is claimed twice,
    /// so a catalogue entry that breaks it has to fail here rather than in the UI.
    #[test]
    fn no_cvar_is_claimed_by_two_tweaks() {
        let mut owner: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        for d in tweak_catalogue() {
            let keys = edits_for_tweak(&d, true, None)
                .unwrap_or_else(|_| edits_for_tweak(&d, false, None).unwrap_or_default());
            let mut seen = std::collections::HashSet::new();
            for edit in keys {
                let key = edit.key.to_ascii_lowercase();
                assert!(
                    seen.insert(key.clone()),
                    "'{}' writes '{}' twice",
                    d.id,
                    edit.key
                );
                if let Some(other) = owner.insert(key.clone(), d.id.clone()) {
                    panic!("'{}' and '{}' both write '{}'", other, d.id, edit.key);
                }
            }
        }
    }

    /// Both front ends split `key=value` patterns on the first `=`; a pattern without one would
    /// silently fall back to treating the whole string as a key.
    #[test]
    fn every_remove_lines_pattern_is_an_assignment() {
        for d in tweak_catalogue() {
            if let TweakKind::RemoveLines { lines, .. } = &d.kind {
                for line in lines {
                    assert!(
                        line.pattern.contains('='),
                        "'{}' has a pattern with no '=': {}",
                        d.id,
                        line.pattern
                    );
                    if let Some(replacement) = &line.replace_with {
                        assert!(
                            replacement.contains('='),
                            "'{}' has a replace_with with no '=': {replacement}",
                            d.id
                        );
                    }
                }
            }
        }
    }

    /// Every catalogue entry must produce edits in both directions (or refuse for a stated reason),
    /// so no tweak silently does nothing from the CLI.
    #[test]
    fn the_whole_catalogue_translates() {
        for d in tweak_catalogue() {
            let on = edits_for_tweak(&d, true, None)
                .unwrap_or_else(|e| panic!("{} failed to turn on: {e}", d.id));
            assert!(!on.is_empty(), "{} produced no edits", d.id);
            assert!(
                on.iter().all(|e| !e.key.trim().is_empty()),
                "{} produced an empty key",
                d.id
            );

            let remove_only = matches!(
                d.kind,
                TweakKind::RemoveLines {
                    remove_only: true,
                    ..
                }
            );
            match edits_for_tweak(&d, false, None) {
                Ok(off) => assert!(!off.is_empty(), "{} produced no off edits", d.id),
                Err(_) => assert!(remove_only, "{} refused to turn off", d.id),
            }
        }
    }
}
