use std::path::PathBuf;

pub(super) fn paks_dir(game_root: &str) -> PathBuf {
    PathBuf::from(game_root).join("MarvelGame\\Marvel\\Content\\Paks")
}

pub(super) fn mods_dir(game_root: &str) -> PathBuf {
    paks_dir(game_root).join("~mods")
}

pub(super) fn binaries_dir(game_root: &str) -> PathBuf {
    PathBuf::from(game_root).join("MarvelGame\\Marvel\\Binaries\\Win64")
}
