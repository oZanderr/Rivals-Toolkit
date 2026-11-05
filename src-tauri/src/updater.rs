//! Downloads, verifies, and self-installs new releases so the app can update itself without an installer.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter};

#[cfg(target_os = "linux")]
use linux as install;
#[cfg(windows)]
use windows as install;

pub(crate) use install::cleanup_stale_update_files;

use crate::update_check::{GITHUB_OWNER, GITHUB_REPO};

const PROGRESS_EVENT: &str = "update-download-progress";
/// Generous cap so a multi-MB release on a slow link isn't cut off mid-download.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);
/// Emit progress at most once per this many bytes to avoid flooding the UI.
const PROGRESS_STEP: u64 = 256 * 1024;

/// Set when the user cancels an in-flight download; checked in the read loop.
static DOWNLOAD_CANCEL: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Serialize)]
struct UpdateProgress {
    /// "downloading" | "verifying" | "staging" | "ready"
    phase: &'static str,
    downloaded: u64,
    /// 0 when the server didn't send Content-Length.
    total: u64,
}

fn emit(app: &AppHandle, phase: &'static str, downloaded: u64, total: u64) {
    let _ = app.emit(
        PROGRESS_EVENT,
        UpdateProgress {
            phase,
            downloaded,
            total,
        },
    );
}

fn release_asset_url(tag: &str, file: &str) -> String {
    format!("https://github.com/{GITHUB_OWNER}/{GITHUB_REPO}/releases/download/{tag}/{file}")
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(DOWNLOAD_TIMEOUT))
        .build()
        .new_agent()
}

/// Pull the lowercase hex digest out of a `sha256sum`-style line (`<hex>  <name>`).
fn parse_sha256_line(s: &str) -> Option<String> {
    let token = s.split_whitespace().next()?.to_ascii_lowercase();
    (token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit())).then_some(token)
}

fn sha256_hex(path: &Path) -> Result<String, String> {
    let mut file =
        fs::File::open(path).map_err(|e| format!("Failed to open {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn writable_error(action: &str, e: &io::Error) -> String {
    if e.kind() == io::ErrorKind::PermissionDenied {
        format!(
            "Can't {action}: the app folder isn't writable. Move Rivals Toolkit to a writable \
             location (e.g. Desktop or Downloads) and try again, or update manually."
        )
    } else {
        format!("Failed to {action}: {e}")
    }
}

/// Download this platform's release asset (streaming, cancellable), verify its SHA256 against
/// the published checksum, and stage it beside the app. Leaves nothing staged on cancel or error.
fn download_and_stage(version: &str, app: &AppHandle) -> Result<(), String> {
    DOWNLOAD_CANCEL.store(false, Ordering::Relaxed);

    let tag = format!("v{version}");
    let asset = install::asset_name(&tag);
    let asset_url = release_asset_url(&tag, &asset);
    let sha_url = release_asset_url(&tag, &format!("{asset}.sha256"));
    let target = install::target()?;

    let temp_dir = std::env::temp_dir().join(format!("rivals_update_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).map_err(|e| format!("Failed to create temp dir: {e}"))?;

    let result = (|| -> Result<(), String> {
        let net = agent();

        emit(app, "downloading", 0, 0);
        let mut resp = net
            .get(&asset_url)
            .header("User-Agent", "rivals-toolkit-updater")
            .call()
            .map_err(|e| format!("Download failed: {e}"))?;
        let total = resp
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);

        let download_path = temp_dir.join(&asset);
        let mut out = fs::File::create(&download_path)
            .map_err(|e| format!("Failed to create temp file: {e}"))?;
        let mut reader = resp.body_mut().as_reader();
        let mut buf = [0u8; 64 * 1024];
        let mut downloaded: u64 = 0;
        let mut last_emit: u64 = 0;
        loop {
            if DOWNLOAD_CANCEL.load(Ordering::Relaxed) {
                return Err("Update download cancelled".to_string());
            }
            let n = reader
                .read(&mut buf)
                .map_err(|e| format!("Download read error: {e}"))?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])
                .map_err(|e| format!("Failed writing download: {e}"))?;
            downloaded += n as u64;
            if downloaded - last_emit >= PROGRESS_STEP {
                last_emit = downloaded;
                emit(app, "downloading", downloaded, total);
            }
        }
        out.flush()
            .map_err(|e| format!("Failed flushing download: {e}"))?;
        drop(out);
        emit(app, "downloading", downloaded, total.max(downloaded));

        emit(app, "verifying", downloaded, downloaded);
        let mut sha_resp = net
            .get(&sha_url)
            .header("User-Agent", "rivals-toolkit-updater")
            .call()
            .map_err(|e| format!("Checksum download failed: {e}"))?;
        let mut sha_text = String::new();
        sha_resp
            .body_mut()
            .as_reader()
            .take(4096)
            .read_to_string(&mut sha_text)
            .map_err(|e| format!("Failed to read checksum: {e}"))?;
        let expected =
            parse_sha256_line(&sha_text).ok_or("Published checksum is missing or malformed")?;
        let actual = sha256_hex(&download_path)?;
        if actual != expected {
            return Err(
                "Downloaded update failed checksum verification; aborting for safety.".to_string(),
            );
        }

        emit(app, "staging", downloaded, downloaded);
        install::stage(&download_path, &temp_dir, &target)?;

        emit(app, "ready", downloaded, downloaded);
        Ok(())
    })();

    let _ = fs::remove_dir_all(&temp_dir);
    result
}

#[tauri::command]
pub(crate) async fn download_update(version: String, app: AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || download_and_stage(&version, &app))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) fn cancel_update_download() {
    DOWNLOAD_CANCEL.store(true, Ordering::Relaxed);
}

#[tauri::command]
pub(crate) fn apply_update_and_restart() -> Result<(), String> {
    install::apply_and_restart()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn parses_standard_sha256sum_line() {
        let hash = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(
            parse_sha256_line(&format!("{hash}  rivals-toolkit-v1.0.0-windows-x64.zip")).as_deref(),
            Some(hash)
        );
        assert_eq!(parse_sha256_line(hash).as_deref(), Some(hash));
        assert_eq!(
            parse_sha256_line(&hash.to_uppercase()).as_deref(),
            Some(hash)
        );
    }

    #[test]
    fn rejects_malformed_checksum_lines() {
        assert!(parse_sha256_line("").is_none());
        assert!(
            parse_sha256_line("nothex_nothex_nothex_nothex_nothex_nothex_nothex_nothex_nothex_")
                .is_none()
        );
        assert!(parse_sha256_line("deadbeef  short").is_none());
    }

    #[test]
    fn hashes_file_contents() {
        let dir = std::env::temp_dir().join(format!("rt_sha_test_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("data.bin");
        fs::write(&path, b"abc").unwrap();
        assert_eq!(
            sha256_hex(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_the_release_asset_the_workflow_uploads() {
        let asset = install::asset_name("v1.2.3");
        #[cfg(windows)]
        assert_eq!(asset, "rivals-toolkit-v1.2.3-windows-x64.zip");
        #[cfg(target_os = "linux")]
        assert_eq!(asset, "rivals-toolkit-v1.2.3-linux-x64.AppImage");
    }
}
