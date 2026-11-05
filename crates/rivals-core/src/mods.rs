//! Mod-folder file discovery, and moving a mod's files as one step.

use std::path::{Path, PathBuf};

/// The files deleting the mod `full_name` in `dir` takes: the pak as named, and the `.ucas` and
/// `.utoc` beside it switched on or off, those that are there.
pub fn files_to_delete(dir: &Path, full_name: &str) -> Result<Vec<PathBuf>, String> {
    let stem = if let Some(s) = full_name.strip_suffix(".pak.disabled") {
        s
    } else if let Some(s) = full_name.strip_suffix(".pak") {
        s
    } else {
        return Err(format!("Unexpected mod filename: {full_name}"));
    };
    Ok([
        full_name.to_string(),
        format!("{stem}.ucas"),
        format!("{stem}.utoc"),
        format!("{stem}.ucas.disabled"),
        format!("{stem}.utoc.disabled"),
    ]
    .iter()
    .map(|name| dir.join(name))
    .filter(|path| path.exists())
    .collect())
}

/// Delete a mod and its companion `.ucas`/`.utoc` files, enabled or not.
pub fn delete_mod(dir: &Path, full_name: &str) -> Result<(), String> {
    for path in files_to_delete(dir, full_name)? {
        std::fs::remove_file(&path)
            .map_err(|e| format!("Failed to delete {}: {e}", path.display()))?;
    }
    Ok(())
}

/// The paths below `root` of the mod files there (`.pak`, `.ucas` and `.utoc`, switched on or
/// off), in its subfolders too when `recursive`, as the game finds them.
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

/// Writes `to` whole through a file beside it, renamed over it once complete. Windows refuses to
/// touch a file the game holds, but Linux lets one the game has open be cut short under it.
pub fn replace_file(
    to: &Path,
    write: impl FnOnce(&Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let name = to
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temporary = to.with_file_name(format!(".{name}.tmp"));
    let written = write(&temporary).and_then(|()| std::fs::rename(&temporary, to));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

/// Copies `from` over `to` the way [`replace_file`] writes.
pub fn copy_over(from: &Path, to: &Path) -> std::io::Result<()> {
    replace_file(to, |temporary| std::fs::copy(from, temporary).map(|_| ()))
}

/// Windows refuses to move a file another process holds, and here that process is almost always
/// the game. Worth saying, since the running-game check can be turned off.
pub fn held_open_hint(error: &std::io::Error) -> &'static str {
    const SHARING_VIOLATION: i32 = 32;
    if cfg!(windows) && error.raw_os_error() == Some(SHARING_VIOLATION) {
        ". Something has the file open, which is usually Marvel Rivals itself."
    } else {
        ""
    }
}

/// The patch number UE reads from a `Name_<N>_P` file: a higher number mounts above a lower one,
/// and a name without the suffix sits below every patch. Directories and extensions are ignored.
pub fn pak_priority(file_name: &str) -> Option<u32> {
    let name = file_name.rsplit(['/', '\\']).next().unwrap_or(file_name);
    let name = name.strip_suffix(".disabled").unwrap_or(name);
    let stem = Path::new(name).file_stem()?.to_str()?;
    let (rest, _) = stem
        .rsplit_once('_')
        .filter(|(_, p)| p.eq_ignore_ascii_case("p"))?;
    rest.rsplit_once('_')?.1.parse().ok()
}

/// Orders mods, named by their paths below `~mods`, so the one whose copy of a shared asset wins
/// comes first: highest patch number, then alphabetical by that whole path, ignoring case, which is
/// the order the loader breaks a tie in. A folder's name counts: `AA/C` wins over `B`, and `B`
/// over `ZZ/A` (seen in game, 2026-11-03).
pub fn winner_order(a: &str, b: &str) -> std::cmp::Ordering {
    pak_priority(b)
        .cmp(&pak_priority(a))
        .then_with(|| a.to_lowercase().cmp(&b.to_lowercase()))
}

/// [`winner_order`] for two of the mods' files below `mods`.
pub fn winner_order_below(mods: &Path, a: &Path, b: &Path) -> std::cmp::Ordering {
    winner_order(&path_below(mods, a), &path_below(mods, b))
}

/// `path` below `mods`, `/` between its parts, or whole when it is not below it. A path spelt
/// otherwise than `mods` is is compared resolved.
pub fn path_below(mods: &Path, path: &Path) -> String {
    let relative = path
        .strip_prefix(mods)
        .ok()
        .map(Path::to_path_buf)
        .or_else(|| {
            let mods = mods.canonicalize().ok()?;
            let dir = path.parent()?.canonicalize().ok()?;
            Some(dir.strip_prefix(&mods).ok()?.join(path.file_name()?))
        });
    match relative {
        Some(relative) => relative.to_string_lossy().replace('\\', "/"),
        None => path.to_string_lossy().into_owned(),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn the_patch_number_is_the_one_before_a_trailing_p() {
        assert_eq!(
            pak_priority("!ProjectGalacta_9999999_P.pak"),
            Some(9_999_999)
        );
        assert_eq!(pak_priority("sub/Foo_Bar_12_p.utoc.disabled"), Some(12));
        assert_eq!(pak_priority("Foo_P.pak"), None);
        assert_eq!(pak_priority("Foo_12.pak"), None);
    }

    /// The order the game broke ties in, between mods of one patch number in and out of folders.
    #[test]
    fn a_tie_goes_to_the_first_whole_path_below_mods() {
        let mods = Path::new("C:/Game/~mods");
        let first =
            |a: &str, b: &str| winner_order_below(mods, &mods.join(a), &mods.join(b)).is_lt();
        assert!(first("A_Rifle_9999999_P.utoc", "B_Rifle_9999999_P.utoc"));
        assert!(first("B_Body_9999999_P.utoc", "ZZ/A_Body_9999999_P.utoc"));
        assert!(first("AA/C_Face_9999999_P.utoc", "B_Face_9999999_P.utoc"));
        assert!(
            first("zz/Low_10_P.utoc", "A_Low_9_P.utoc"),
            "a higher patch number first"
        );
        assert_eq!(
            path_below(mods, &mods.join("ZZ").join("A.utoc")),
            "ZZ/A.utoc"
        );
        assert_eq!(pak_priority("Foo.pak"), None);
    }

    #[test]
    fn the_highest_patch_wins_and_names_break_a_tie() {
        let mut mods = vec!["b_1_P.pak", "Plain.pak", "a_1_P.pak", "z_50_P.pak"];
        mods.sort_by(|a, b| winner_order(a, b));
        assert_eq!(mods, ["z_50_P.pak", "a_1_P.pak", "b_1_P.pak", "Plain.pak"]);
    }

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
