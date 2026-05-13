//! Detects whether the Marvel Rivals process is running to block mutating operations on locked pak files.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};

static CHECK_ENABLED: AtomicBool = AtomicBool::new(true);

pub(crate) fn set_check_enabled(enabled: bool) {
    CHECK_ENABLED.store(enabled, Ordering::Relaxed);
}

/// Guard call sites use this; UI surfaces keep using `is_game_running()` so the
/// indicator stays truthful when the user toggles the check off.
pub(crate) fn should_block_for_game() -> bool {
    if !CHECK_ENABLED.load(Ordering::Relaxed) {
        return false;
    }
    is_game_running()
}

/// Shipping executable name — locks pak/ucas/utoc file handles while the game
/// runs. The launcher exe does not, so ignore it.
const GAME_PROCESS: &str = "Marvel-Win64-Shipping.exe";

/// Reused across polls to keep the process-name cache warm and avoid reallocating.
static SYSTEM: LazyLock<Mutex<System>> = LazyLock::new(|| {
    Mutex::new(System::new_with_specifics(
        RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing()),
    ))
});

/// Probe the OS process list and return true when the Marvel Rivals shipping
/// executable is running. Returns false on lock contention so callers fail open
/// (the backend command guards are the source of truth either way).
pub(crate) fn is_game_running() -> bool {
    let Ok(mut sys) = SYSTEM.lock() else {
        return false;
    };
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    sys.processes().values().any(|p| {
        p.name()
            .to_string_lossy()
            .eq_ignore_ascii_case(GAME_PROCESS)
    })
}

/// Short English error for mutating ops attempted while the game is running.
pub(crate) fn game_running_error() -> String {
    "Marvel Rivals is running! Close the game before modifying mods.".to_string()
}

#[tauri::command]
pub(crate) fn get_game_running() -> bool {
    is_game_running()
}

#[tauri::command]
pub(crate) fn get_should_block_for_game() -> bool {
    should_block_for_game()
}
