use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const GITHUB_OWNER: &str = "oZanderr";
const GITHUB_REPO: &str = "Rivals-Toolkit";
const CACHE_MAX_AGE: Duration = Duration::from_secs(30 * 60); // 30 minutes
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

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
    let Some(path) = cache_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let cached = CachedCheck {
        timestamp: now_secs(),
        info: info.clone(),
    };
    let _ = serde_json::to_string(&cached).map(|json| std::fs::write(path, json));
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

pub(crate) fn check_for_update(current_version: &str, force: bool) -> Result<UpdateInfo, String> {
    // Return cached result if fresh enough (unless force=true)
    if !force && let Some(mut cached) = read_cache() {
        // Re-evaluate against current version in case of app upgrade
        cached.current_version = current_version.to_string();
        cached.update_available = is_newer(&cached.latest_version, current_version);
        return Ok(cached);
    }

    let url = format!("https://api.github.com/repos/{GITHUB_OWNER}/{GITHUB_REPO}/releases/latest");

    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(REQUEST_TIMEOUT))
        .build()
        .new_agent();

    let response: GitHubRelease = agent
        .get(&url)
        .header("User-Agent", "rivals-toolkit-update-check")
        .header("Accept", "application/vnd.github.v3+json")
        .call()
        .map_err(|e| format!("Update check failed: {e}"))?
        .body_mut()
        .read_json()
        .map_err(|e| format!("Failed to parse response: {e}"))?;

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
