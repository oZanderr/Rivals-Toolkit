use std::collections::HashSet;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::settings::{ModProfile, Settings};

use super::folder;
use super::status::get_mods_status;

#[derive(Serialize, Deserialize)]
pub(crate) struct ProfileDiff {
    /// Mods that will be enabled.
    pub to_enable: Vec<String>,
    /// Mods that will be disabled.
    pub to_disable: Vec<String>,
    /// Profile mods not found on disk.
    pub missing: Vec<String>,
    /// Mods already in the correct state.
    pub unchanged: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct ProfileApplyResult {
    pub successes: u32,
    pub failed: u32,
    /// Profile mods not found on disk.
    pub missing: Vec<String>,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(crate) fn list_profiles(state: &Mutex<Settings>) -> Result<Vec<ModProfile>, String> {
    let guard = state.lock().map_err(|e| e.to_string())?;
    Ok(guard.mod_profiles.clone())
}

pub(crate) fn save_profile(
    state: &Mutex<Settings>,
    name: &str,
    game_root: &str,
    recursive: bool,
) -> Result<ModProfile, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Profile name cannot be empty".to_string());
    }

    let status = get_mods_status(game_root, recursive);
    let enabled_mods: Vec<String> = status
        .mod_entries
        .iter()
        .filter(|e| e.enabled)
        .map(|e| e.display_name.clone())
        .collect();

    let now = now_secs();
    let profile = ModProfile {
        name: trimmed.to_string(),
        enabled_mods,
        created_at: now,
        modified_at: now,
    };

    let mut guard = state.lock().map_err(|e| e.to_string())?;
    if guard.mod_profiles.iter().any(|p| p.name == trimmed) {
        return Err(format!("Profile \"{trimmed}\" already exists"));
    }
    guard.mod_profiles.push(profile.clone());
    guard.save()?;
    Ok(profile)
}

pub(crate) fn delete_profile(state: &Mutex<Settings>, name: &str) -> Result<(), String> {
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    let before = guard.mod_profiles.len();
    guard.mod_profiles.retain(|p| p.name != name);
    if guard.mod_profiles.len() == before {
        return Err(format!("Profile \"{name}\" not found"));
    }
    guard.save()
}

pub(crate) fn rename_profile(
    state: &Mutex<Settings>,
    old_name: &str,
    new_name: &str,
) -> Result<(), String> {
    let trimmed = new_name.trim();
    if trimmed.is_empty() {
        return Err("Profile name cannot be empty".to_string());
    }

    let mut guard = state.lock().map_err(|e| e.to_string())?;
    if guard
        .mod_profiles
        .iter()
        .any(|p| p.name == trimmed && p.name != old_name)
    {
        return Err(format!("Profile \"{trimmed}\" already exists"));
    }
    let profile = guard
        .mod_profiles
        .iter_mut()
        .find(|p| p.name == old_name)
        .ok_or_else(|| format!("Profile \"{old_name}\" not found"))?;
    profile.name = trimmed.to_string();
    profile.modified_at = now_secs();
    guard.save()
}

pub(crate) fn overwrite_profile(
    state: &Mutex<Settings>,
    name: &str,
    game_root: &str,
    recursive: bool,
) -> Result<ModProfile, String> {
    let status = get_mods_status(game_root, recursive);
    let enabled_mods: Vec<String> = status
        .mod_entries
        .iter()
        .filter(|e| e.enabled)
        .map(|e| e.display_name.clone())
        .collect();

    let mut guard = state.lock().map_err(|e| e.to_string())?;
    let profile = guard
        .mod_profiles
        .iter_mut()
        .find(|p| p.name == name)
        .ok_or_else(|| format!("Profile \"{name}\" not found"))?;
    profile.enabled_mods = enabled_mods;
    profile.modified_at = now_secs();
    let result = profile.clone();
    guard.save()?;
    Ok(result)
}

pub(crate) fn preview_profile(
    state: &Mutex<Settings>,
    name: &str,
    game_root: &str,
    recursive: bool,
) -> Result<ProfileDiff, String> {
    let profile_mods: HashSet<String> = {
        let guard = state.lock().map_err(|e| e.to_string())?;
        let profile = guard
            .mod_profiles
            .iter()
            .find(|p| p.name == name)
            .ok_or_else(|| format!("Profile \"{name}\" not found"))?;
        profile.enabled_mods.iter().cloned().collect()
    };

    let status = get_mods_status(game_root, recursive);
    let all_on_disk: HashSet<String> = status
        .mod_entries
        .iter()
        .map(|e| e.display_name.clone())
        .collect();
    let currently_enabled: HashSet<String> = status
        .mod_entries
        .iter()
        .filter(|e| e.enabled)
        .map(|e| e.display_name.clone())
        .collect();

    let mut to_enable: Vec<String> = profile_mods
        .iter()
        .filter(|m| all_on_disk.contains(*m) && !currently_enabled.contains(*m))
        .cloned()
        .collect();
    let mut to_disable: Vec<String> = currently_enabled
        .iter()
        .filter(|m| !profile_mods.contains(*m))
        .cloned()
        .collect();
    let mut missing: Vec<String> = profile_mods
        .iter()
        .filter(|m| !all_on_disk.contains(*m))
        .cloned()
        .collect();
    let mut unchanged: Vec<String> = currently_enabled
        .iter()
        .filter(|m| profile_mods.contains(*m))
        .cloned()
        .collect();

    to_enable.sort();
    to_disable.sort();
    missing.sort();
    unchanged.sort();

    Ok(ProfileDiff {
        to_enable,
        to_disable,
        missing,
        unchanged,
    })
}

pub(crate) fn apply_profile(
    state: &Mutex<Settings>,
    name: &str,
    game_root: &str,
    recursive: bool,
) -> Result<ProfileApplyResult, String> {
    let diff = preview_profile(state, name, game_root, recursive)?;
    let status = get_mods_status(game_root, recursive);
    let mods_folder = status.mods_folder_path;

    let mut total_successes = 0u32;
    let mut total_failed = 0u32;

    // Disable mods not in profile.
    if !diff.to_disable.is_empty() {
        let disable_full_names: Vec<String> = status
            .mod_entries
            .iter()
            .filter(|e| e.enabled && diff.to_disable.contains(&e.display_name))
            .map(|e| e.full_name.clone())
            .collect();
        let res = folder::toggle_mods_enabled(&mods_folder, &disable_full_names, false);
        total_successes += res.successes;
        total_failed += res.failures.len() as u32;
    }

    // Enable mods in profile.
    if !diff.to_enable.is_empty() {
        let enable_full_names: Vec<String> = status
            .mod_entries
            .iter()
            .filter(|e| !e.enabled && diff.to_enable.contains(&e.display_name))
            .map(|e| e.full_name.clone())
            .collect();
        let res = folder::toggle_mods_enabled(&mods_folder, &enable_full_names, true);
        total_successes += res.successes;
        total_failed += res.failures.len() as u32;
    }

    Ok(ProfileApplyResult {
        successes: total_successes,
        failed: total_failed,
        missing: diff.missing,
    })
}
