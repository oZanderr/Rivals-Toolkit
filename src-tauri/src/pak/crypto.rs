use std::{fs, io::BufReader, path::Path};

use aes::cipher::KeyInit;

/// NetEase AES-256 key for Marvel Rivals pak files
const MARVEL_AES_KEY: &str = "0C263D8C22DCB085894899C3A3796383E9BF9DE0CBFB08C9BF2DEF2E84F29D74";

/// Decodes and byte-swaps the AES key for repak-rivals
/// The NetEase pak format reverses each 4-byte word of the raw key
pub(crate) fn make_aes_key() -> Result<aes::Aes256, String> {
    let mut bytes = hex::decode(MARVEL_AES_KEY).map_err(|e| e.to_string())?;
    bytes.chunks_mut(4).for_each(|c| c.reverse());
    aes::Aes256::new_from_slice(&bytes).map_err(|e| e.to_string())
}

pub(crate) fn open_pak(pak_path: &Path) -> Result<repak::PakReader, String> {
    let key = make_aes_key()?;
    let file = fs::File::open(pak_path).map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(file);
    repak::PakBuilder::new()
        .key(key)
        .reader(&mut reader)
        .map_err(|e| e.to_string())
}
