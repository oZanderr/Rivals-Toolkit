//! Saved tweak loadouts: persist a snapshot of TweakSettings, export/import as JSON for sharing across installs.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::settings::{SettingsState, TweakProfile};
use crate::tweaks::TweakSetting;

const EXPORT_KIND: &str = "rivals-toolkit-config-preset";
const EXPORT_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct ExportEnvelope {
    kind: String,
    version: u32,
    name: String,
    settings: Vec<TweakSetting>,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[tauri::command]
pub(crate) fn list_tweak_profiles(
    state: State<'_, SettingsState>,
) -> Result<Vec<TweakProfile>, String> {
    let guard = state.lock().map_err(|e| e.to_string())?;
    Ok(guard.tweak_profiles.clone())
}

#[tauri::command]
pub(crate) fn save_tweak_profile(
    state: State<'_, SettingsState>,
    name: String,
    settings: Vec<TweakSetting>,
) -> Result<TweakProfile, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Profile name cannot be empty".to_string());
    }

    let now = now_secs();
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    if guard.tweak_profiles.iter().any(|p| p.name == trimmed) {
        return Err(format!("Profile \"{trimmed}\" already exists"));
    }
    let profile = TweakProfile {
        name: trimmed.to_string(),
        settings,
        created_at: now,
        modified_at: now,
    };
    guard.tweak_profiles.push(profile.clone());
    guard.save()?;
    Ok(profile)
}

#[tauri::command]
pub(crate) fn overwrite_tweak_profile(
    state: State<'_, SettingsState>,
    name: String,
    settings: Vec<TweakSetting>,
) -> Result<TweakProfile, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Profile name cannot be empty".to_string());
    }
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    let profile = guard
        .tweak_profiles
        .iter_mut()
        .find(|p| p.name == trimmed)
        .ok_or_else(|| format!("Profile \"{trimmed}\" not found"))?;
    profile.settings = settings;
    profile.modified_at = now_secs();
    let result = profile.clone();
    guard.save()?;
    Ok(result)
}

#[tauri::command]
pub(crate) fn delete_tweak_profile(
    state: State<'_, SettingsState>,
    name: String,
) -> Result<(), String> {
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    let before = guard.tweak_profiles.len();
    guard.tweak_profiles.retain(|p| p.name != name);
    if guard.tweak_profiles.len() == before {
        return Err(format!("Profile \"{name}\" not found"));
    }
    guard.save()
}

#[tauri::command]
pub(crate) fn rename_tweak_profile(
    state: State<'_, SettingsState>,
    old_name: String,
    new_name: String,
) -> Result<TweakProfile, String> {
    let trimmed = new_name.trim();
    if trimmed.is_empty() {
        return Err("Profile name cannot be empty".to_string());
    }
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    if guard
        .tweak_profiles
        .iter()
        .any(|p| p.name == trimmed && p.name != old_name)
    {
        return Err(format!("Profile \"{trimmed}\" already exists"));
    }
    let profile = guard
        .tweak_profiles
        .iter_mut()
        .find(|p| p.name == old_name)
        .ok_or_else(|| format!("Profile \"{old_name}\" not found"))?;
    profile.name = trimmed.to_string();
    let result = profile.clone();
    guard.save()?;
    Ok(result)
}

#[tauri::command]
pub(crate) fn export_tweak_profile(
    state: State<'_, SettingsState>,
    name: String,
) -> Result<String, String> {
    let guard = state.lock().map_err(|e| e.to_string())?;
    let profile = guard
        .tweak_profiles
        .iter()
        .find(|p| p.name == name)
        .ok_or_else(|| format!("Profile \"{name}\" not found"))?;
    let envelope = ExportEnvelope {
        kind: EXPORT_KIND.to_string(),
        version: EXPORT_VERSION,
        name: profile.name.clone(),
        settings: profile.settings.clone(),
    };
    serde_json::to_string_pretty(&envelope).map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) fn export_tweak_profile_to_file(
    state: State<'_, SettingsState>,
    name: String,
    path: String,
) -> Result<(), String> {
    let json = export_tweak_profile(state, name)?;
    std::fs::write(&path, json).map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) fn import_tweak_profile_from_file(
    state: State<'_, SettingsState>,
    path: String,
    name_override: Option<String>,
) -> Result<TweakProfile, String> {
    let json = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    import_tweak_profile(state, json, name_override)
}

#[tauri::command]
pub(crate) fn import_tweak_profile(
    state: State<'_, SettingsState>,
    json: String,
    name_override: Option<String>,
) -> Result<TweakProfile, String> {
    let envelope: ExportEnvelope =
        serde_json::from_str(&json).map_err(|e| format!("Invalid profile JSON: {e}"))?;
    if envelope.kind != EXPORT_KIND {
        return Err(format!(
            "Unexpected file kind \"{}\". Expected \"{}\".",
            envelope.kind, EXPORT_KIND
        ));
    }
    if envelope.version > EXPORT_VERSION {
        return Err(format!(
            "Profile version {} is newer than supported version {}",
            envelope.version, EXPORT_VERSION
        ));
    }

    let target_name = name_override
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| envelope.name.trim().to_string());
    if target_name.is_empty() {
        return Err("Profile name cannot be empty".to_string());
    }

    let now = now_secs();
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    if guard.tweak_profiles.iter().any(|p| p.name == target_name) {
        return Err(format!("Profile \"{target_name}\" already exists"));
    }
    let profile = TweakProfile {
        name: target_name,
        settings: envelope.settings,
        created_at: now,
        modified_at: now,
    };
    guard.tweak_profiles.push(profile.clone());
    guard.save()?;
    Ok(profile)
}
