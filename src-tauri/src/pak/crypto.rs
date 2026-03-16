use std::{fs, io::BufReader, path::Path};

use super::profile::RIVALS_PROFILE;

pub(crate) fn make_aes_key() -> Result<aes::Aes256, String> {
    RIVALS_PROFILE.make_aes_key()
}

pub(crate) fn open_pak(pak_path: &Path) -> Result<repak::PakReader, String> {
    let key = make_aes_key()?;
    let file = fs::File::open(pak_path).map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(file);
    repak::PakBuilder::new()
        .profile(RIVALS_PROFILE.repak_profile())
        .key(key)
        .reader(&mut reader)
        .map_err(|e| e.to_string())
}
