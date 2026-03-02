use std::path::PathBuf;

pub(crate) fn paks_dir(game_root: &str) -> PathBuf {
    PathBuf::from(game_root).join("MarvelGame\\Marvel\\Content\\Paks")
}

pub(crate) fn mods_dir(game_root: &str) -> PathBuf {
    paks_dir(game_root).join("~mods")
}

pub(crate) fn binaries_dir(game_root: &str) -> PathBuf {
    PathBuf::from(game_root).join("MarvelGame\\Marvel\\Binaries\\Win64")
}
