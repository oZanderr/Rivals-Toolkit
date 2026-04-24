//! Fetch the latest character_ids.json from GitHub so hero detection stays current with new releases.

#![allow(clippy::redundant_pub_crate)]

use std::io::Read;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::settings::SettingsState;

use super::heroes::{RawCatalogue, catalogue_data, reload_catalogue, user_catalogue_path};

const CHARACTER_SYNC_INTERVAL_SECS: u64 = 24 * 60 * 60;

const REMOTE_URL: &str = "https://raw.githubusercontent.com/oZanderr/Rivals-Toolkit/main/src-tauri/data/character_ids.json";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Hard cap so a hostile/corrupt host can't OOM the app. Legitimate file is well under.
const MAX_BODY_BYTES: usize = 5 * 1024 * 1024;

/// Serializes concurrent sync attempts (auto-sync on mount + manual button click).
static SYNC_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct CharacterDataInfo {
    pub character_count: usize,
    pub generated_at: Option<String>,
    pub origin: String,
    pub source: Option<String>,
    pub user_file_mtime: Option<u64>,
    pub user_file_present: bool,
}

#[derive(Clone, Serialize)]
pub(crate) struct SyncResult {
    pub character_count: usize,
    pub generated_at: Option<String>,
    pub fetched_at: u64,
    pub bytes: usize,
    pub source_url: String,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

fn file_mtime_secs(path: &Path) -> Option<u64> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    modified
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

fn atomic_write(path: &Path, body: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    // PID-tagged tmp avoids collision if two writers race despite the SYNC_LOCK.
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&tmp, body).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// Count characters that round-trip into the strict `RawCharacter` shape (name + skins).
fn count_valid_characters(parsed: &RawCatalogue) -> usize {
    parsed
        .characters
        .iter()
        .filter(|(id, c)| id.parse::<u32>().is_ok() && !c.name.is_empty())
        .count()
}

pub(crate) fn current_info() -> CharacterDataInfo {
    let cat = catalogue_data();
    let path = user_catalogue_path();
    let user_file_mtime = path.as_ref().and_then(|p| file_mtime_secs(p));
    let user_file_present = path.as_ref().is_some_and(|p| p.exists());
    CharacterDataInfo {
        character_count: cat.characters.len(),
        generated_at: cat.generated_at.clone(),
        origin: cat.origin.clone(),
        source: cat.source.clone(),
        user_file_mtime,
        user_file_present,
    }
}

/// Caller must update `last_character_data_sync` to the returned `fetched_at`;
/// per-entry catalogue stamp checks will then recompute stale hero results.
pub(crate) fn sync_from_remote() -> Result<SyncResult, String> {
    let _guard = SYNC_LOCK
        .try_lock()
        .map_err(|_| "character sync already in progress".to_string())?;

    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(REQUEST_TIMEOUT))
        .build()
        .new_agent();

    let mut response = agent
        .get(REMOTE_URL)
        .header("User-Agent", "rivals-toolkit-character-sync")
        .header("Accept", "application/json")
        .call()
        .map_err(|e| format!("character sync fetch failed: {e}"))?;

    // Cap before buffering the full body so a hostile/broken host can't OOM the app.
    let cap = u64::try_from(MAX_BODY_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut body = String::new();
    response
        .body_mut()
        .as_reader()
        .take(cap)
        .read_to_string(&mut body)
        .map_err(|e| format!("character sync read failed: {e}"))?;

    if body.len() > MAX_BODY_BYTES {
        return Err(format!(
            "character sync rejected: payload exceeds {MAX_BODY_BYTES} byte limit"
        ));
    }

    // Deep validation: reject if upstream JSON shape is fine but every entry is malformed
    // (would otherwise silently produce an empty catalogue on next reload).
    let parsed: RawCatalogue =
        serde_json::from_str(&body).map_err(|e| format!("character sync invalid JSON: {e}"))?;

    let valid = count_valid_characters(&parsed);
    if valid == 0 {
        return Err("character sync rejected: 0 valid characters in payload".to_string());
    }

    let path =
        user_catalogue_path().ok_or_else(|| "no config dir for character data".to_string())?;
    atomic_write(&path, &body)?;

    reload_catalogue();

    Ok(SyncResult {
        character_count: valid,
        generated_at: parsed.generated_at,
        fetched_at: now_secs(),
        bytes: body.len(),
        source_url: REMOTE_URL.to_string(),
    })
}

#[tauri::command]
pub(crate) fn get_character_data_info() -> CharacterDataInfo {
    current_info()
}

#[tauri::command]
pub(crate) async fn sync_character_data(
    state: State<'_, SettingsState>,
) -> Result<SyncResult, String> {
    let result = tauri::async_runtime::spawn_blocking(sync_from_remote)
        .await
        .map_err(|e| e.to_string())??;

    if let Ok(mut guard) = state.lock() {
        guard.last_character_data_sync = result.fetched_at;
        if let Err(e) = guard.save() {
            eprintln!("rivals-toolkit: failed to persist updated catalogue stamp: {e}");
        }
    }

    Ok(result)
}

#[tauri::command]
pub(crate) fn should_auto_sync_character_data(state: State<'_, SettingsState>) -> bool {
    let Ok(guard) = state.lock() else {
        return false;
    };
    if !guard.auto_sync_character_data {
        return false;
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    now.saturating_sub(guard.last_character_data_sync) >= CHARACTER_SYNC_INTERVAL_SECS
}

#[tauri::command]
pub(crate) fn get_auto_sync_character_data(state: State<'_, SettingsState>) -> bool {
    state
        .lock()
        .map(|s| s.auto_sync_character_data)
        .unwrap_or(true)
}

#[tauri::command]
pub(crate) fn set_auto_sync_character_data(
    state: State<'_, SettingsState>,
    enabled: bool,
) -> Result<(), String> {
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    guard.auto_sync_character_data = enabled;
    guard.save()
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn validator_accepts_well_formed_payload() {
        let body = r#"{
            "generated_at": "2026-04-17T00:00:00.000Z",
            "characters": {
                "1054": {"name": "Phoenix", "skins": {"1054001": "Default"}}
            }
        }"#;
        let parsed: RawCatalogue = serde_json::from_str(body).expect("parse");
        assert_eq!(count_valid_characters(&parsed), 1);
    }

    #[test]
    fn validator_rejects_missing_characters_key() {
        let body = r#"{"generated_at": "x"}"#;
        let parsed: Result<RawCatalogue, _> = serde_json::from_str(body);
        // Default makes characters empty rather than failing; deep check rejects.
        let parsed = parsed.expect("shape parses with default empty map");
        assert_eq!(count_valid_characters(&parsed), 0);
    }

    #[test]
    fn validator_rejects_entries_with_empty_name() {
        // Catches the bug where shape gate accepts but every character entry is malformed.
        let body = r#"{
            "characters": {
                "1054": {"name": "", "skins": {}}
            }
        }"#;
        let parsed: RawCatalogue = serde_json::from_str(body).expect("parse");
        assert_eq!(count_valid_characters(&parsed), 0);
    }

    #[test]
    fn validator_rejects_non_numeric_character_id() {
        let body = r#"{
            "characters": {
                "phoenix": {"name": "Phoenix", "skins": {}}
            }
        }"#;
        let parsed: RawCatalogue = serde_json::from_str(body).expect("parse");
        assert_eq!(count_valid_characters(&parsed), 0);
    }

    #[test]
    fn atomic_write_replaces_existing_file() {
        let dir = std::env::temp_dir().join(format!("rivals-toolkit-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("character_ids.json");
        atomic_write(&path, "first").expect("first write");
        atomic_write(&path, "second").expect("second write");
        let contents = std::fs::read_to_string(&path).expect("read");
        assert_eq!(contents, "second");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
