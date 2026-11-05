//! Installs a downloaded Windows release zip over the running exe and its Oodle DLL, then relaunches the app.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::writable_error;

const DLL_NAME: &str = "oo2core_9_win64.dll";

pub(super) fn asset_name(tag: &str) -> String {
    format!("rivals-toolkit-{tag}-windows-x64.zip")
}

/// The file the update replaces.
pub(super) fn target() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| format!("Can't locate the app: {e}"))
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

/// Extract the exe and dll from the verified zip and stage them next to the running exe as
/// `*.new`.
pub(super) fn stage(zip_path: &Path, temp_dir: &Path, cur_exe: &Path) -> Result<(), String> {
    let install_dir = cur_exe.parent().ok_or("Can't resolve the app's folder")?;
    let (new_exe, new_dll) = extract_target_files(zip_path, &temp_dir.join("extracted"))?;

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
    Ok(())
}

/// Swap the staged `*.new` files over the running exe/dll and relaunch. Windows
/// forbids overwriting a running exe or loaded dll but allows renaming them, so
/// we rename the live files aside, move the staged ones into place, then exec the
/// new exe and exit. Leftover `*.old` files are cleared on next startup.
pub(super) fn apply_and_restart() -> Result<(), String> {
    let cur_exe = target()?;
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::io::Write;

    use super::*;

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
