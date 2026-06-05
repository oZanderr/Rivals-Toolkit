//! Downloads, verifies, and self-installs new portable releases so the app can update itself without an installer.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter};

use crate::update_check::{GITHUB_OWNER, GITHUB_REPO};

const DLL_NAME: &str = "oo2core_9_win64.dll";
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

fn asset_base(tag: &str) -> String {
    format!("rivals-toolkit-{tag}-windows-x64")
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

/// Pull just `rivals-toolkit.exe` (required) and `oo2core_9_win64.dll` (optional)
/// out of the release zip, matching by basename so it works whether the files
/// sit at the zip root or inside a wrapper folder. Returns their extracted paths.
fn extract_target_files(
    zip_path: &Path,
    dest_dir: &Path,
) -> Result<(PathBuf, Option<PathBuf>), String> {
    let file =
        fs::File::open(zip_path).map_err(|e| format!("Failed to open downloaded zip: {e}"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("Invalid update zip: {e}"))?;
    fs::create_dir_all(dest_dir).map_err(|e| format!("Failed to create extract dir: {e}"))?;

    let mut exe_out = None;
    let mut dll_out = None;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        let Some(name) = entry
            .enclosed_name()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        else {
            continue;
        };
        let lower = name.to_ascii_lowercase();
        let dest = if lower == "rivals-toolkit.exe" {
            dest_dir.join("rivals-toolkit.exe")
        } else if lower == DLL_NAME {
            dest_dir.join(DLL_NAME)
        } else {
            continue;
        };
        let mut out =
            fs::File::create(&dest).map_err(|e| format!("Failed to write {name}: {e}"))?;
        io::copy(&mut entry, &mut out).map_err(|e| format!("Failed to extract {name}: {e}"))?;
        if lower.ends_with(".exe") {
            exe_out = Some(dest);
        } else {
            dll_out = Some(dest);
        }
    }

    let exe = exe_out.ok_or("Update archive did not contain rivals-toolkit.exe")?;
    Ok((exe, dll_out))
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

/// Download the zip (streaming, cancellable), verify its SHA256 against the
/// published checksum, extract the exe + dll, and stage them next to the running
/// exe as `*.new`. Leaves nothing staged on cancel or error.
fn download_and_stage(version: &str, app: &AppHandle) -> Result<(), String> {
    DOWNLOAD_CANCEL.store(false, Ordering::Relaxed);

    let tag = format!("v{version}");
    let base = asset_base(&tag);
    let zip_name = format!("{base}.zip");
    let zip_url = release_asset_url(&tag, &zip_name);
    let sha_url = release_asset_url(&tag, &format!("{zip_name}.sha256"));

    let cur_exe = std::env::current_exe().map_err(|e| format!("Can't locate the app: {e}"))?;
    let install_dir = cur_exe
        .parent()
        .ok_or("Can't resolve the app's folder")?
        .to_path_buf();

    let temp_dir = std::env::temp_dir().join(format!("rivals_update_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).map_err(|e| format!("Failed to create temp dir: {e}"))?;

    let result = (|| -> Result<(), String> {
        let net = agent();

        emit(app, "downloading", 0, 0);
        let mut resp = net
            .get(&zip_url)
            .header("User-Agent", "rivals-toolkit-updater")
            .call()
            .map_err(|e| format!("Download failed: {e}"))?;
        let total = resp
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);

        let zip_path = temp_dir.join(&zip_name);
        let mut out =
            fs::File::create(&zip_path).map_err(|e| format!("Failed to create temp file: {e}"))?;
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
        let actual = sha256_hex(&zip_path)?;
        if actual != expected {
            return Err(
                "Downloaded update failed checksum verification; aborting for safety.".to_string(),
            );
        }

        let extract_dir = temp_dir.join("extracted");
        let (new_exe, new_dll) = extract_target_files(&zip_path, &extract_dir)?;

        emit(app, "staging", downloaded, downloaded);
        let exe_name = cur_exe
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("Can't resolve the app's file name")?;
        let staged_exe = install_dir.join(format!("{exe_name}.new"));
        let staged_dll = install_dir.join(format!("{DLL_NAME}.new"));

        // Copy rather than rename: temp is often on a different volume.
        fs::copy(&new_exe, &staged_exe).map_err(|e| writable_error("stage the update", &e))?;
        if let Some(new_dll) = new_dll
            && let Err(e) = fs::copy(&new_dll, &staged_dll)
        {
            let _ = fs::remove_file(&staged_exe);
            return Err(writable_error("stage the update", &e));
        }

        emit(app, "ready", downloaded, downloaded);
        Ok(())
    })();

    let _ = fs::remove_dir_all(&temp_dir);
    result
}

/// Swap the staged `*.new` files over the running exe/dll and relaunch. Windows
/// forbids overwriting a running exe or loaded dll but allows renaming them, so
/// we rename the live files aside, move the staged ones into place, then exec the
/// new exe and exit. Leftover `*.old` files are cleared on next startup.
fn apply_and_restart() -> Result<(), String> {
    let cur_exe = std::env::current_exe().map_err(|e| format!("Can't locate the app: {e}"))?;
    let install_dir = cur_exe.parent().ok_or("Can't resolve the app's folder")?;
    let exe_name = cur_exe
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("Can't resolve the app's file name")?;

    let staged_exe = install_dir.join(format!("{exe_name}.new"));
    if !staged_exe.exists() {
        return Err("No downloaded update is staged. Download it first.".to_string());
    }

    let old_exe = install_dir.join(format!("{exe_name}.old"));
    let _ = fs::remove_file(&old_exe);
    fs::rename(&cur_exe, &old_exe).map_err(|e| writable_error("install the update", &e))?;
    if let Err(e) = fs::rename(&staged_exe, &cur_exe) {
        let _ = fs::rename(&old_exe, &cur_exe); // roll back so the app still launches
        return Err(format!("Failed to install the update: {e}"));
    }

    let staged_dll = install_dir.join(format!("{DLL_NAME}.new"));
    if staged_dll.exists() {
        let dll = install_dir.join(DLL_NAME);
        let old_dll = install_dir.join(format!("{DLL_NAME}.old"));
        let _ = fs::remove_file(&old_dll);
        if dll.exists() {
            let _ = fs::rename(&dll, &old_dll);
        }
        if let Err(e) = fs::rename(&staged_dll, &dll) {
            return Err(format!("Failed to install the update's Oodle library: {e}"));
        }
    }

    schedule_relaunch(&cur_exe)?;
    std::process::exit(0);
}

/// Relaunch the (swapped) exe only after this process has fully exited. Starting
/// it immediately races the old process's WebView2 user-data teardown and the new
/// window never appears, so a hidden PowerShell helper waits on our PID and then
/// launches the new exe. Only `CREATE_NO_WINDOW` is set: adding `DETACHED_PROCESS`
/// stops the helper from completing the relaunch.
fn schedule_relaunch(exe: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let pid = std::process::id();
    let exe_str = exe.to_string_lossy().replace('\'', "''"); // escape for the PS single-quoted string
    let script = format!(
        "Wait-Process -Id {pid} -ErrorAction SilentlyContinue; \
         Start-Process -FilePath '{exe_str}'"
    );
    std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            &script,
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| {
            format!("Update installed; please relaunch the app manually (relaunch failed: {e})")
        })?;
    Ok(())
}

/// Best-effort removal of leftover `*.old` files from a prior self-update.
pub(crate) fn cleanup_stale_update_files() {
    let Ok(cur_exe) = std::env::current_exe() else {
        return;
    };
    let Some(dir) = cur_exe.parent() else {
        return;
    };
    if let Some(name) = cur_exe.file_name().and_then(|n| n.to_str()) {
        let _ = fs::remove_file(dir.join(format!("{name}.old")));
    }
    let _ = fs::remove_file(dir.join(format!("{DLL_NAME}.old")));
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
    apply_and_restart()
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

    fn build_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let file = fs::File::create(path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let opts: zip::write::FileOptions<()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, data) in entries {
            writer.start_file(*name, opts).unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap();
    }

    #[test]
    fn locates_target_files_at_zip_root() {
        let dir = std::env::temp_dir().join(format!("rt_zip_root_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let zip_path = dir.join("root.zip");
        build_zip(
            &zip_path,
            &[
                ("rivals-toolkit.exe", b"EXE"),
                ("oo2core_9_win64.dll", b"DLL"),
                ("README.txt", b"ignored"),
            ],
        );
        let (exe, dll) = extract_target_files(&zip_path, &dir.join("out")).unwrap();
        assert_eq!(fs::read(&exe).unwrap(), b"EXE");
        assert_eq!(fs::read(dll.unwrap()).unwrap(), b"DLL");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn locates_target_files_in_wrapper_folder() {
        let dir = std::env::temp_dir().join(format!("rt_zip_wrap_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let zip_path = dir.join("wrap.zip");
        build_zip(
            &zip_path,
            &[
                (
                    "rivals-toolkit-v1.0.0-windows-x64/rivals-toolkit.exe",
                    b"EXE",
                ),
                (
                    "rivals-toolkit-v1.0.0-windows-x64/oo2core_9_win64.dll",
                    b"DLL",
                ),
            ],
        );
        let (exe, dll) = extract_target_files(&zip_path, &dir.join("out")).unwrap();
        assert_eq!(fs::read(&exe).unwrap(), b"EXE");
        assert_eq!(fs::read(dll.unwrap()).unwrap(), b"DLL");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn errors_when_exe_missing_from_zip() {
        let dir = std::env::temp_dir().join(format!("rt_zip_noexe_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let zip_path = dir.join("noexe.zip");
        build_zip(&zip_path, &[("oo2core_9_win64.dll", b"DLL")]);
        assert!(extract_target_files(&zip_path, &dir.join("out")).is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
