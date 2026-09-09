//! Mod-folder file discovery, and moving a mod's files as one step.

use std::path::{Path, PathBuf};

/// Collect relative paths of mod-related files (.pak, .ucas, .utoc, and their
/// `.disabled` variants) under the given root directory. When `recursive` is
/// false, only direct children of `root` are scanned (matches UE's native
/// `~mods` load behavior).
pub fn walk_mod_files(root: &Path, recursive: bool) -> Vec<PathBuf> {
    let mut walker = walkdir::WalkDir::new(root);
    if !recursive {
        walker = walker.max_depth(1);
    }
    walker
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            let name = e.file_name().to_string_lossy();
            let base = name.strip_suffix(".disabled").unwrap_or(&name);
            matches!(
                Path::new(base).extension().and_then(|x| x.to_str()),
                Some("pak" | "ucas" | "utoc")
            )
        })
        .filter_map(|e| e.path().strip_prefix(root).ok().map(|r| r.to_path_buf()))
        .collect()
}

/// Renames a set of files as one step. A rename that fails puts back the ones that already moved.
///
/// A mod is a `.pak` and, for IoStore, a `.utoc` and `.ucas` beside it. Moving them one at a time
/// and giving up on the first failure leaves the mod split: half renamed, or half disabled, which
/// the game reads as a broken mount rather than as the mod being off. A file the game still holds
/// open is the usual reason one will not move.
pub fn rename_all(moves: &[(PathBuf, PathBuf)]) -> Result<(), String> {
    for (done, (from, to)) in moves.iter().enumerate() {
        if let Err(error) = std::fs::rename(from, to) {
            put_back(&moves[..done]);
            return Err(format!(
                "Could not move {}: {error}{}",
                from.display(),
                held_open_hint(&error)
            ));
        }
    }
    Ok(())
}

/// Undoes moves that landed, for an operation that could not finish. Best effort by design: there
/// is nothing useful to do with a failure to undo, and reporting the original cause matters more.
pub fn put_back(moved: &[(PathBuf, PathBuf)]) {
    for (from, to) in moved.iter().rev() {
        // Whatever sits at the source now is a partial write from the step that failed.
        let _ = std::fs::remove_file(from);
        let _ = std::fs::rename(to, from);
    }
}

/// Windows refuses to move a file another process holds, and here that process is almost always
/// the game. Worth saying, since the running-game check can be turned off.
pub fn held_open_hint(error: &std::io::Error) -> &'static str {
    const SHARING_VIOLATION: i32 = 32;
    if error.raw_os_error() == Some(SHARING_VIOLATION) {
        ". Something has the file open, which is usually Marvel Rivals itself."
    } else {
        ""
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rivals-moves-{tag}-{stamp}"));
        std::fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    #[test]
    fn every_file_moves_or_none_of_them_do() {
        let dir = scratch("rollback");
        for extension in ["pak", "utoc", "ucas"] {
            std::fs::write(dir.join(format!("Mod.{extension}")), extension).expect("write");
        }
        // A directory where the last file is headed is a rename the OS refuses, which is what a
        // file another process holds open looks like from here.
        std::fs::create_dir_all(dir.join("Mod.ucas.disabled")).expect("blocker");

        let moves: Vec<(PathBuf, PathBuf)> = ["pak", "utoc", "ucas"]
            .iter()
            .map(|extension| {
                (
                    dir.join(format!("Mod.{extension}")),
                    dir.join(format!("Mod.{extension}.disabled")),
                )
            })
            .collect();
        let error = rename_all(&moves).expect_err("the last file cannot move");
        assert!(error.contains("Mod.ucas"), "{error}");
        for extension in ["pak", "utoc", "ucas"] {
            assert_eq!(
                std::fs::read(dir.join(format!("Mod.{extension}"))).expect("put back"),
                extension.as_bytes(),
                "Mod.{extension} is where it started"
            );
        }
        assert!(!dir.join("Mod.pak.disabled").exists(), "nothing half moved");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_set_that_all_moves_leaves_nothing_behind() {
        let dir = scratch("moved");
        std::fs::write(dir.join("Mod.pak"), "pak").expect("write");
        std::fs::write(dir.join("Mod.utoc"), "toc").expect("write");
        let moves = vec![
            (dir.join("Mod.pak"), dir.join("Mod.pak.disabled")),
            (dir.join("Mod.utoc"), dir.join("Mod.utoc.disabled")),
        ];
        rename_all(&moves).expect("all move");
        assert!(dir.join("Mod.pak.disabled").is_file());
        assert!(dir.join("Mod.utoc.disabled").is_file());
        assert!(!dir.join("Mod.pak").exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
