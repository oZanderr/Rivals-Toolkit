//! Detects whether the Marvel Rivals process is running to block mutating operations on the files it has open.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};

// Off in this crate's own tests, so whether the game happens to be open never decides one.
static CHECK_ENABLED: AtomicBool = AtomicBool::new(!cfg!(test));

pub fn set_check_enabled(enabled: bool) {
    CHECK_ENABLED.store(enabled, Ordering::Relaxed);
}

/// Guard call sites use this; UI surfaces keep using `is_game_running()` so the
/// indicator stays truthful when the user toggles the check off. Under Proton it is all that
/// stops a write to a file the game has open: Wine locks files only against its own processes.
pub fn should_block_for_game() -> bool {
    if !CHECK_ENABLED.load(Ordering::Relaxed) {
        return false;
    }
    is_game_running()
}

/// The shipping executable, which holds the game's containers open; the launcher does not. Linux
/// names a process by its `/proc` comm, cut to 15 bytes, which Wine sets from the exe's name.
#[cfg(not(target_os = "linux"))]
const GAME_PROCESS: &str = "Marvel-Win64-Shipping.exe";
#[cfg(target_os = "linux")]
const GAME_PROCESS: &str = "Marvel-Win64-Sh";

/// Reused across polls to keep the process-name cache warm and avoid reallocating.
static SYSTEM: LazyLock<Mutex<System>> = LazyLock::new(|| {
    Mutex::new(System::new_with_specifics(
        RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing()),
    ))
});

/// Probe the OS process list and return true when the Marvel Rivals shipping
/// executable is running. Returns false on lock contention so callers fail open
/// (the backend command guards are the source of truth either way).
pub fn is_game_running() -> bool {
    let Ok(mut sys) = SYSTEM.lock() else {
        return false;
    };
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    sys.processes().values().any(|p| {
        // Linux lists threads alongside processes.
        p.thread_kind().is_none() && p.name().eq_ignore_ascii_case(GAME_PROCESS)
    })
}

/// Short English error for mutating ops attempted while the game is running.
pub fn game_running_error() -> String {
    "Marvel Rivals is running! Close the game before modifying mods.".to_string()
}
