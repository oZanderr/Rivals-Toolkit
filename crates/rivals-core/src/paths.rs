//! Game-relative path helpers (paks, mods, binaries, launch record) plus generic existence checks.

use std::path::PathBuf;

pub fn paks_dir(game_root: &str) -> PathBuf {
    PathBuf::from(game_root).join("MarvelGame\\Marvel\\Content\\Paks")
}

pub fn mods_dir(game_root: &str) -> PathBuf {
    paks_dir(game_root).join("~mods")
}

pub fn binaries_dir(game_root: &str) -> PathBuf {
    PathBuf::from(game_root).join("MarvelGame\\Marvel\\Binaries\\Win64")
}

pub fn launch_record_path(game_root: &str) -> PathBuf {
    PathBuf::from(game_root).join("launch_record")
}

/// A turn at the game-data tests' shared `~mods`. Tests that write a probe mod there take turns
/// with tests that read every mod's packages, which would otherwise find one half written or
/// already gone. Held per thread, so a test can take it again while it holds it.
#[cfg(test)]
pub(crate) struct ModsTurn;

#[cfg(test)]
mod mods_turn {
    use std::cell::RefCell;
    use std::sync::{Mutex, MutexGuard, PoisonError};

    static TURN: Mutex<()> = Mutex::new(());

    thread_local! {
        static HELD: RefCell<(usize, Option<MutexGuard<'static, ()>>)> =
            const { RefCell::new((0, None)) };
    }

    impl super::ModsTurn {
        pub(crate) fn take() -> Self {
            HELD.with(|held| {
                let mut held = held.borrow_mut();
                if held.0 == 0 {
                    held.1 = Some(TURN.lock().unwrap_or_else(PoisonError::into_inner));
                }
                held.0 += 1;
            });
            Self
        }
    }

    impl Drop for super::ModsTurn {
        fn drop(&mut self) {
            HELD.with(|held| {
                let mut held = held.borrow_mut();
                held.0 -= 1;
                if held.0 == 0 {
                    held.1 = None;
                }
            });
        }
    }
}
