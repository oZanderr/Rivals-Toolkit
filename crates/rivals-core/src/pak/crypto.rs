//! AES key construction and pak file opening with the Marvel Rivals encryption key.

use std::{fs, io::BufReader, path::Path};

use super::profile::RIVALS_PROFILE;

pub fn make_aes_key() -> Result<aes::Aes256, String> {
    RIVALS_PROFILE.make_aes_key()
}

pub fn open_pak(pak_path: &Path) -> Result<repak::PakReader, String> {
    // Patch containers (e.g. pakchunkPatch07) encrypt their index under a named key; repak picks
    // the key matching the pak footer's GUID from the chain, falling back to the default key.
    let file = fs::File::open(pak_path).map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(file);
    repak::PakBuilder::new()
        .profile(RIVALS_PROFILE.repak_profile())
        .key(make_aes_key()?)
        .keys(RIVALS_PROFILE.repak_key_chain()?)
        .reader(&mut reader)
        .map_err(|e| e.to_string())
}
