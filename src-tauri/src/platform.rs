//! OS abstraction for game-data directory resolution, Steam root discovery, and process-name matching.

use std::path::PathBuf;

/// Steam install roots to probe for `steamapps/libraryfolders.vdf`.
#[cfg(windows)]
pub(crate) fn steam_roots() -> Vec<PathBuf> {
    use winreg::{RegKey, enums::HKEY_LOCAL_MACHINE};
    [r"SOFTWARE\Wow6432Node\Valve\Steam", r"SOFTWARE\Valve\Steam"]
        .iter()
        .find_map(|&key| {
            RegKey::predef(HKEY_LOCAL_MACHINE)
                .open_subkey(key)
                .ok()?
                .get_value::<String, _>("InstallPath")
                .ok()
        })
        .map(|p| vec![PathBuf::from(p.trim())])
        .unwrap_or_default()
}

/// Steam install roots to probe for `steamapps/libraryfolders.vdf`.
#[cfg(not(windows))]
pub(crate) fn steam_roots() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    [
        ".steam/steam",
        ".steam/root",
        ".local/share/Steam",
        ".var/app/com.valvesoftware.Steam/.local/share/Steam",
        ".steam/debian-installation",
    ]
    .iter()
    .map(|rel| home.join(rel))
    .filter(|p| p.is_dir())
    .collect()
}

/// `%LOCALAPPDATA%`-equivalent where the game reads/writes its config.
/// Windows: `dirs::data_local_dir()`. Linux: the Proton prefix's `AppData/Local`.
// Wired into config-path resolution in Milestone B5.
#[allow(dead_code)]
#[cfg(windows)]
pub(crate) fn game_data_dir(_game_root: &str) -> Result<PathBuf, String> {
    dirs::data_local_dir().ok_or_else(|| "Could not determine AppData path.".to_string())
}

/// `%LOCALAPPDATA%`-equivalent where the game reads/writes its config.
/// Windows: `dirs::data_local_dir()`. Linux: the Proton prefix's `AppData/Local`.
// Wired into config-path resolution in Milestone B5; the Proton-prefix lookup lands in B3.
#[allow(dead_code)]
#[cfg(not(windows))]
pub(crate) fn game_data_dir(_game_root: &str) -> Result<PathBuf, String> {
    Err("Proton prefix detection is not yet implemented".to_string())
}

/// True when a process's reported name or exe basename identifies `target`.
/// Tolerates Linux's 15-char `TASK_COMM_LEN` truncation of `/proc/<pid>/comm`.
pub(crate) fn process_matches(
    proc_name: &str,
    proc_exe_basename: Option<&str>,
    target: &str,
) -> bool {
    if proc_exe_basename.is_some_and(|base| base.eq_ignore_ascii_case(target)) {
        return true;
    }
    if proc_name.eq_ignore_ascii_case(target) {
        return true;
    }
    // `/proc/<pid>/comm` caps at TASK_COMM_LEN (16 bytes, 15 usable), so a long
    // exe name shows up truncated in the process list. Match the truncated prefix.
    const COMM_MAX: usize = 15;
    target.len() > COMM_MAX
        && target.is_char_boundary(COMM_MAX)
        && proc_name.eq_ignore_ascii_case(&target[..COMM_MAX])
}
