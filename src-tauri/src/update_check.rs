//! GitHub release polling with a short cache, used by the auto-update prompt on app launch.

use std::io::Read;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::settings::SettingsState;

pub(crate) const GITHUB_OWNER: &str = "oZanderr";
pub(crate) const GITHUB_REPO: &str = "Rivals-Toolkit";
const CACHE_MAX_AGE: Duration = Duration::from_secs(30 * 60); // 30 minutes
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Hard cap so a hostile/broken host can't OOM the app. Real GitHub release JSON is well under.
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Serializes concurrent update checks (auto-check on launch + manual button click).
static FETCH_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct UpdateInfo {
    pub update_available: bool,
    pub latest_version: String,
    pub current_version: String,
    pub release_url: String,
    pub release_notes: Option<String>,
}

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    html_url: String,
    body: Option<String>,
    prerelease: bool,
}

#[derive(Serialize, Deserialize)]
struct CachedCheck {
    timestamp: u64,
    info: UpdateInfo,
}

fn cache_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("rivals-toolkit").join("update_cache.json"))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn read_cache() -> Option<UpdateInfo> {
    let path = cache_path()?;
    let data = std::fs::read_to_string(path).ok()?;
    let cached: CachedCheck = serde_json::from_str(&data).ok()?;
    if now_secs().saturating_sub(cached.timestamp) < CACHE_MAX_AGE.as_secs() {
        Some(cached.info)
    } else {
        None
    }
}

fn write_cache(info: &UpdateInfo) {
    let Some(path) = cache_path() else {
        eprintln!("rivals-toolkit: no cache dir for update cache");
        return;
    };
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        eprintln!("rivals-toolkit: failed to create update cache dir: {e}");
        return;
    }
    let cached = CachedCheck {
        timestamp: now_secs(),
        info: info.clone(),
    };
    let json = match serde_json::to_string_pretty(&cached) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("rivals-toolkit: failed to serialize update cache: {e}");
            return;
        }
    };
    let tmp = path.with_extension("json.tmp");
    if let Err(e) = std::fs::write(&tmp, json) {
        eprintln!("rivals-toolkit: failed to write update cache tmp: {e}");
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        eprintln!("rivals-toolkit: failed to commit update cache: {e}");
    }
}

fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
    let s = s.strip_prefix('v').unwrap_or(s);
    let mut parts = s.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    Some((major, minor, patch))
}

fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

fn fetch_update_info(current_version: &str, force: bool) -> Result<UpdateInfo, String> {
    // Return cached result if fresh enough (unless force=true)
    if !force && let Some(mut cached) = read_cache() {
        // Re-evaluate against current version in case of app upgrade
        cached.current_version = current_version.to_string();
        cached.update_available = is_newer(&cached.latest_version, current_version);
        return Ok(cached);
    }

    // Serialize with concurrent callers; second caller will see fresh cache after first finishes.
    let _guard = FETCH_LOCK
        .try_lock()
        .map_err(|_| "Update check already in progress".to_string())?;

    // Double-check cache after acquiring lock to avoid duplicate fetch.
    if !force && let Some(mut cached) = read_cache() {
        cached.current_version = current_version.to_string();
        cached.update_available = is_newer(&cached.latest_version, current_version);
        return Ok(cached);
    }

    let url = format!("https://api.github.com/repos/{GITHUB_OWNER}/{GITHUB_REPO}/releases/latest");

    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(REQUEST_TIMEOUT))
        .build()
        .new_agent();

    let mut http_response = agent
        .get(&url)
        .header("User-Agent", "rivals-toolkit-update-check")
        .header("Accept", "application/vnd.github.v3+json")
        .call()
        .map_err(|e| format!("Update check failed: {e}"))?;

    let cap = u64::try_from(MAX_BODY_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut body = String::new();
    http_response
        .body_mut()
        .as_reader()
        .take(cap)
        .read_to_string(&mut body)
        .map_err(|e| format!("Failed to read response: {e}"))?;

    if body.len() > MAX_BODY_BYTES {
        return Err(format!(
            "Update check rejected: payload exceeds {MAX_BODY_BYTES} byte limit"
        ));
    }

    let response: GitHubRelease =
        serde_json::from_str(&body).map_err(|e| format!("Failed to parse response: {e}"))?;

    if response.prerelease {
        let info = UpdateInfo {
            update_available: false,
            latest_version: current_version.to_string(),
            current_version: current_version.to_string(),
            release_url: String::new(),
            release_notes: None,
        };
        return Ok(info);
    }

    let latest = response
        .tag_name
        .strip_prefix('v')
        .unwrap_or(&response.tag_name)
        .to_string();

    let info = UpdateInfo {
        update_available: is_newer(&latest, current_version),
        latest_version: latest,
        current_version: current_version.to_string(),
        release_url: response.html_url,
        release_notes: response.body,
    };

    write_cache(&info);
    Ok(info)
}

#[tauri::command]
pub(crate) async fn check_for_update(
    current_version: String,
    force: Option<bool>,
) -> Result<UpdateInfo, String> {
    let force = force.unwrap_or(false);
    tauri::async_runtime::spawn_blocking(move || fetch_update_info(&current_version, force))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) fn get_auto_check_updates(state: State<'_, SettingsState>) -> bool {
    state.lock().map(|s| s.auto_check_updates).unwrap_or(true)
}

#[tauri::command]
pub(crate) fn set_auto_check_updates(
    state: State<'_, SettingsState>,
    enabled: bool,
) -> Result<(), String> {
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    guard.auto_check_updates = enabled;
    guard.save()
}
