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
