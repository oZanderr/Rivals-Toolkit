//! Replaces the AppImage the app runs from with a downloaded release, then relaunches it.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use super::writable_error;

pub(super) fn asset_name(tag: &str) -> String {
    format!("rivals-toolkit-{tag}-linux-x64.AppImage")
}

/// The file the update replaces. The AppImage runtime names it in `APPIMAGE`; the binary itself
/// runs from a read-only mount of it.
pub(super) fn target() -> Result<PathBuf, String> {
    std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .ok_or_else(|| {
            "This copy isn't running from an AppImage, so it can't update itself. Download the new \
         release instead."
                .to_string()
        })
}

fn staged_path(appimage: &Path) -> Result<PathBuf, String> {
    let name = appimage
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("Can't resolve the app's file name")?;
    Ok(appimage.with_file_name(format!(".{name}.new")))
}

/// Stage the verified AppImage beside the running one, ready to run.
pub(super) fn stage(download: &Path, _temp_dir: &Path, appimage: &Path) -> Result<(), String> {
    let staged = staged_path(appimage)?;
    // Copy rather than rename: temp is often on a different filesystem.
    let staged_ok = fs::copy(download, &staged)
        .and_then(|_| fs::set_permissions(&staged, fs::Permissions::from_mode(0o755)));
    staged_ok.map_err(|e| {
        let _ = fs::remove_file(&staged);
        writable_error("stage the update", &e)
    })
}

/// Rename the staged AppImage over the running one, which keeps reading the file it mounted, then
/// start the new one and exit.
pub(super) fn apply_and_restart() -> Result<(), String> {
    let appimage = target()?;
    let staged = staged_path(&appimage)?;
    if !staged.exists() {
        return Err("No downloaded update is staged. Download it first.".to_string());
    }
    fs::rename(&staged, &appimage).map_err(|e| writable_error("install the update", &e))?;
    std::process::Command::new(&appimage).spawn().map_err(|e| {
        format!("Update installed; please relaunch the app manually (relaunch failed: {e})")
    })?;
    std::process::exit(0);
}

/// Best-effort removal of an update a previous run staged but never applied.
pub(crate) fn cleanup_stale_update_files() {
    if let Ok(staged) = target().and_then(|appimage| staged_path(&appimage)) {
        let _ = fs::remove_file(staged);
    }
}
