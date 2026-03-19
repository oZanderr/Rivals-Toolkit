pub(crate) mod crypto;
pub(crate) mod profile;
mod reader;
mod writer;

use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
};

type PakListMap = Mutex<HashMap<(String, u64), Vec<String>>>;

/// Pak file-list cache keyed by (absolute path, file size); invalidated on size change.
static PAK_LIST_CACHE: LazyLock<PakListMap> = LazyLock::new(|| Mutex::new(HashMap::new()));

pub(crate) fn list_pak_files(game_root: &str) -> Result<Vec<String>, String> {
    reader::list_pak_files(game_root)
}

pub(crate) fn list_pak_contents(pak_path: &str) -> Result<Vec<String>, String> {
    let file_size = std::fs::metadata(pak_path).map(|m| m.len()).unwrap_or(0);
    let cache_key = (pak_path.to_owned(), file_size);

    {
        let guard = PAK_LIST_CACHE.lock().map_err(|e| e.to_string())?;
        if let Some(files) = guard.get(&cache_key) {
            return Ok(files.clone());
        }
    }

    let files = reader::list_pak_contents(pak_path)?;
    PAK_LIST_CACHE
        .lock()
        .map_err(|e| e.to_string())?
        .insert(cache_key, files.clone());
    Ok(files)
}

pub(crate) fn unpack_pak(pak_path: &str, output_dir: &str) -> Result<Vec<String>, String> {
    reader::unpack_pak(pak_path, output_dir)
}

pub(crate) fn extract_single_file(
    pak_path: &str,
    file_name: &str,
    output_path: &str,
) -> Result<(), String> {
    reader::extract_single_file(pak_path, file_name, output_path)
}

pub(crate) fn repack_pak(input_dir: &str, output_pak: &str) -> Result<(), String> {
    writer::repack_pak(input_dir, output_pak)
}
