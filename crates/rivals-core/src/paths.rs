//! Game-relative path helpers (paks, mods, binaries, launch record) and the game's Saved folder under LocalAppData.

use std::path::PathBuf;

/// Marvel Rivals on Steam, which also names its Proton prefix.
pub const STEAM_APP_ID: &str = "2767030";

pub fn paks_dir(game_root: &str) -> PathBuf {
    PathBuf::from(game_root)
        .join("MarvelGame")
        .join("Marvel")
        .join("Content")
        .join("Paks")
}

pub fn mods_dir(game_root: &str) -> PathBuf {
    child_any_case(&paks_dir(game_root), "~mods")
}

/// `dir`'s entry named `name` in whatever case it has, or `dir/name` when there is none. The game
/// reads through Wine, which ignores case, so a `~Mods` someone made is the folder it loads.
#[cfg(not(windows))]
fn child_any_case(dir: &std::path::Path, name: &str) -> PathBuf {
    let exact = dir.join(name);
    if exact.exists() {
        return exact;
    }
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .find(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|n| n.eq_ignore_ascii_case(name))
        })
        .map_or(exact, |entry| entry.path())
}

#[cfg(windows)]
fn child_any_case(dir: &std::path::Path, name: &str) -> PathBuf {
    dir.join(name)
}

pub fn binaries_dir(game_root: &str) -> PathBuf {
    PathBuf::from(game_root)
        .join("MarvelGame")
        .join("Marvel")
        .join("Binaries")
        .join("Win64")
}

pub fn plugins_dir(game_root: &str) -> PathBuf {
    child_any_case(&binaries_dir(game_root), "plugins")
}

pub fn launch_record_path(game_root: &str) -> PathBuf {
    PathBuf::from(game_root).join("launch_record")
}

/// `%LOCALAPPDATA%\Marvel\Saved`, where the game writes its config, logs and crash reports.
#[cfg(windows)]
pub fn saved_dir(_game_root: Option<&str>) -> Option<PathBuf> {
    dirs::data_local_dir().map(|dir| dir.join("Marvel").join("Saved"))
}

/// `%LOCALAPPDATA%\Marvel\Saved` in the game's Proton prefix, which Steam keeps in the library the
/// game is installed in: the root is `<library>/steamapps/common/<folder>`.
#[cfg(not(windows))]
pub fn saved_dir(game_root: Option<&str>) -> Option<PathBuf> {
    let steamapps = std::path::Path::new(game_root?).parent()?.parent()?;
    if steamapps.file_name()? != "steamapps" {
        return None;
    }
    Some(
        steamapps
            .join("compatdata")
            .join(STEAM_APP_ID)
            .join("pfx")
            .join("drive_c")
            .join("users")
            .join("steamuser")
            .join("AppData")
            .join("Local")
            .join("Marvel")
            .join("Saved"),
    )
}

pub fn logs_dir(game_root: Option<&str>) -> Option<PathBuf> {
    saved_dir(game_root).map(|dir| dir.join("Logs"))
}

pub fn crashes_dir(game_root: Option<&str>) -> Option<PathBuf> {
    saved_dir(game_root).map(|dir| dir.join("Crashes"))
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn names(path: PathBuf) -> Vec<String> {
        path.components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn game_folders_join_one_name_at_a_time() {
        assert_eq!(
            names(paks_dir("root")),
            ["root", "MarvelGame", "Marvel", "Content", "Paks"]
        );
        assert_eq!(
            names(binaries_dir("root")),
            ["root", "MarvelGame", "Marvel", "Binaries", "Win64"]
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn saved_dir_is_in_the_proton_prefix() {
        assert_eq!(
            saved_dir(Some("/games/steamapps/common/MarvelRivals/")),
            Some(PathBuf::from(
                "/games/steamapps/compatdata/2767030/pfx/drive_c/users/steamuser/AppData/Local/Marvel/Saved"
            ))
        );
        assert_eq!(saved_dir(Some("/games/MarvelRivals")), None);
        assert_eq!(saved_dir(None), None);
    }

    #[cfg(not(windows))]
    #[test]
    fn mods_dir_is_the_folder_there_in_any_case() {
        let root = std::env::temp_dir().join(format!("rivals-paths-case-{}", std::process::id()));
        let game = root.to_string_lossy().into_owned();
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(mods_dir(&game), paks_dir(&game).join("~mods"));
        std::fs::create_dir_all(paks_dir(&game).join("~Mods")).unwrap();
        assert_eq!(mods_dir(&game), paks_dir(&game).join("~Mods"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
