//! Turns the loose `--game-root` and `--pak` arguments into concrete, existing paths.

use std::path::PathBuf;

use rivals_core::paths::mods_dir;

use crate::settings::{self, AppSettings};

/// The game root to work against: the flag if given, otherwise whatever the desktop app saved.
pub fn game_root(flag: Option<&str>, app: &AppSettings) -> Result<String, String> {
    let root = match flag {
        Some(path) => path.to_string(),
        None => app.game_path.clone().ok_or_else(|| {
            let hint = settings::settings_path()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "the app's settings".to_string());
            format!("no game root: pass --game-root, or set the game path in the app ({hint})")
        })?,
    };
    if !mods_dir(&root).is_dir() {
        return Err(format!(
            "{} has no MarvelGame\\Marvel\\Content\\Paks\\~mods folder",
            root
        ));
    }
    Ok(root)
}

/// Resolve `--pak` as a path first, then as a mod name inside `~mods`, so scripts can say
/// `--pak MyMod` without knowing where the game is installed.
pub fn pak(arg: &str, root_flag: Option<&str>, app: &AppSettings) -> Result<String, String> {
    let direct = PathBuf::from(arg);
    if direct.is_file() {
        return Ok(arg.to_string());
    }
    if direct.components().count() > 1 || direct.is_absolute() {
        return Err(format!("{arg} does not exist"));
    }

    let mods = mods_dir(&game_root(root_flag, app)?);
    for candidate in [mods.join(arg), mods.join(format!("{arg}.pak"))] {
        if candidate.is_file() {
            return Ok(candidate.to_string_lossy().into_owned());
        }
    }
    Err(format!("no pak named '{arg}' in {}", mods.display()))
}
