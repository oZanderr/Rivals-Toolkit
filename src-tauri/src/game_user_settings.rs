//! GameUserSettings.ini (user's saved-game settings) read/write and curated tweak engine wrapper.

pub(crate) mod commands;

use std::{fs, path::Path};

use crate::tweaks::{self, TweakDefinition, TweakSetting, TweakState, parser};

const CONFIG_PATH: &str = "Marvel\\Saved\\Config\\Windows\\GameUserSettings.ini";

pub(crate) fn get_game_user_settings_path() -> Result<String, String> {
    dirs::data_local_dir()
        .map(|base| base.join(CONFIG_PATH))
        .map(|p| p.to_string_lossy().into_owned())
        .ok_or_else(|| "Could not determine AppData path.".to_string())
}

pub(crate) fn read_game_user_settings(path: &str) -> Result<String, String> {
    fs::read_to_string(path).map_err(|e| e.to_string())
}

pub(crate) fn write_game_user_settings(path: &str, content: &str) -> Result<(), String> {
    let p = Path::new(path);
    if let Ok(meta) = fs::metadata(p)
        && meta.permissions().readonly()
    {
        return Err(
            "GameUserSettings.ini is read-only. Remove the read-only attribute and try again."
                .to_string(),
        );
    }
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::write(p, content).map_err(|e| e.to_string())
}

pub(crate) fn get_tweak_definitions() -> Vec<TweakDefinition> {
    tweaks::catalogue::game_user_settings_catalogue()
}

/// Custom detect: presence in section = active (GameUserSettings always writes
/// every key, so matching the catalogue default would falsely flag entries as
/// inactive). Toggles still compare value to on_value.
pub(crate) fn detect_tweaks(content: &str) -> Vec<TweakState> {
    let entries = tweaks::catalogue::game_user_settings_catalogue();
    entries
        .iter()
        .map(|t| match &t.kind {
            tweaks::TweakKind::Toggle {
                key,
                on_value,
                default_enabled,
                scalability_section,
                ..
            } => {
                let current = scalability_section
                    .as_deref()
                    .and_then(|s| parser::find_key_value_in_section(content, s, key));
                let active = match current.as_deref() {
                    Some(v) => v.eq_ignore_ascii_case(on_value),
                    None => *default_enabled,
                };
                TweakState {
                    id: t.id.clone(),
                    active,
                    current_value: current,
                }
            }
            tweaks::TweakKind::Slider {
                key,
                scalability_section,
                ..
            } => {
                let current = scalability_section
                    .as_deref()
                    .and_then(|s| parser::find_key_value_in_section(content, s, key));
                TweakState {
                    id: t.id.clone(),
                    active: current.is_some(),
                    current_value: current,
                }
            }
            _ => TweakState {
                id: t.id.clone(),
                active: false,
                current_value: None,
            },
        })
        .collect()
}

/// Apply tweak settings to GameUserSettings.ini content and return modified text.
/// GameUserSettings always has section headers, so we share the section-aware
/// parser helpers but inline the apply loop to avoid Scalability's default
/// `[ScalabilitySettings]` header injection.
pub(crate) fn apply_tweaks(content: &str, settings: &[TweakSetting]) -> String {
    let catalogue = tweaks::catalogue::game_user_settings_catalogue();
    let mut text = content.to_string();
    for setting in settings {
        let Some(tweak) = catalogue.iter().find(|t| t.id == setting.id) else {
            continue;
        };
        match &tweak.kind {
            tweaks::TweakKind::Toggle {
                key,
                on_value,
                off_value,
                scalability_section,
                ..
            } => {
                let Some(section) = scalability_section.as_deref() else {
                    continue;
                };
                let target_value = if setting.enabled {
                    on_value.clone()
                } else if let Some(off) = off_value {
                    off.clone()
                } else {
                    text = remove_key_in_section(&text, section, key);
                    continue;
                };
                text = upsert_key_in_section(&text, section, key, &target_value);
            }
            tweaks::TweakKind::Slider {
                key,
                default_value,
                write_default_on_disable,
                scalability_section,
                ..
            } => {
                let Some(section) = scalability_section.as_deref() else {
                    continue;
                };
                if setting.enabled {
                    let value = setting
                        .value
                        .clone()
                        .unwrap_or_else(|| default_value.to_string());
                    text = upsert_key_in_section(&text, section, key, &value);
                } else if *write_default_on_disable {
                    text = upsert_key_in_section(&text, section, key, &default_value.to_string());
                } else {
                    text = remove_key_in_section(&text, section, key);
                }
            }
            _ => {}
        }
    }
    text
}

fn upsert_key_in_section(content: &str, section: &str, key: &str, value: &str) -> String {
    let target_header = format!("[{section}]").to_ascii_lowercase();
    let mut lines: Vec<String> = content.lines().map(String::from).collect();
    let mut in_section = false;
    let mut section_start: Option<usize> = None;
    let mut section_end: Option<usize> = None;
    let mut replaced = false;
    let key_lower = key.to_ascii_lowercase();
    for (i, line) in lines.iter_mut().enumerate() {
        let t = line.trim();
        if t.starts_with('[') && t.ends_with(']') {
            if in_section {
                section_end = Some(i);
                break;
            }
            if t.to_ascii_lowercase() == target_header {
                in_section = true;
                section_start = Some(i);
            }
            continue;
        }
        if in_section && parser::matches_key(t, &key_lower) {
            *line = format!("{key}={value}");
            replaced = true;
        }
    }
    if !replaced {
        match (section_start, section_end) {
            (Some(_), Some(end)) => lines.insert(end, format!("{key}={value}")),
            (Some(_), None) => lines.push(format!("{key}={value}")),
            (None, _) => {
                if !lines.is_empty() && !lines.last().map(|l| l.is_empty()).unwrap_or(false) {
                    lines.push(String::new());
                }
                lines.push(format!("[{section}]"));
                lines.push(format!("{key}={value}"));
            }
        }
    }
    lines.join("\n")
}

fn remove_key_in_section(content: &str, section: &str, key: &str) -> String {
    let target_header = format!("[{section}]").to_ascii_lowercase();
    let mut in_section = false;
    let key_lower = key.to_ascii_lowercase();
    content
        .lines()
        .filter(|line| {
            let t = line.trim();
            if t.starts_with('[') && t.ends_with(']') {
                in_section = t.to_ascii_lowercase() == target_header;
                return true;
            }
            !(in_section && parser::matches_key(t, &key_lower))
        })
        .collect::<Vec<_>>()
        .join("\n")
}
