pub(crate) mod crypto;
mod iostore;
pub(crate) mod profile;
mod reader;
mod writer;

use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
};

type ListCache = Mutex<HashMap<(String, u64), Vec<String>>>;

/// Pak file-list cache keyed by (absolute path, file size); invalidated on size change.
static PAK_LIST_CACHE: LazyLock<ListCache> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Utoc file-list cache keyed by (absolute path, file size); invalidated on size change.
static UTOC_LIST_CACHE: LazyLock<ListCache> = LazyLock::new(|| Mutex::new(HashMap::new()));

pub(crate) use reader::PakFileInfo;

pub(crate) fn list_pak_files(game_root: &str, recursive: bool) -> Result<Vec<String>, String> {
    reader::list_pak_files(game_root, recursive)
}

pub(crate) fn list_pak_files_info(
    game_root: &str,
    recursive: bool,
) -> Result<Vec<PakFileInfo>, String> {
    reader::list_pak_files_info(game_root, recursive)
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

pub(crate) fn unpack_pak(
    pak_path: &str,
    output_dir: &str,
    skip: &[&str],
) -> Result<Vec<String>, String> {
    reader::unpack_pak(pak_path, output_dir, skip)
}

pub(crate) fn extract_single_file(
    pak_path: &str,
    file_name: &str,
    output_path: &str,
) -> Result<(), String> {
    reader::extract_single_file(pak_path, file_name, output_path)
}

pub(crate) fn extract_pak_files(
    pak_path: &str,
    file_names: &[String],
    output_dir: &str,
) -> Result<Vec<String>, String> {
    reader::extract_pak_files(pak_path, file_names, output_dir)
}

pub(crate) fn extract_utoc_files(
    utoc_path: &str,
    file_names: &[String],
    output_dir: &str,
) -> Result<Vec<String>, String> {
    iostore::extract_utoc_files(utoc_path, file_names, output_dir)
}

pub(crate) fn repack_pak(input_dir: &str, output_pak: &str) -> Result<(), String> {
    writer::repack_pak(input_dir, output_pak)
}

pub(crate) fn write_pak_bytes(
    output_pak: &str,
    files: Vec<(String, Vec<u8>)>,
) -> Result<(), String> {
    writer::write_pak_bytes(output_pak, files)
}

pub(crate) fn repack_iostore(
    input_dir: &str,
    output_utoc: &str,
    app: tauri::AppHandle,
) -> Result<(), String> {
    iostore::repack_iostore(input_dir, output_utoc, app)
}

pub(crate) fn cancel_repack_iostore() {
    iostore::cancel_repack_iostore();
}

pub(crate) fn list_utoc_contents(utoc_path: &str) -> Result<Vec<String>, String> {
    let file_size = std::fs::metadata(utoc_path).map(|m| m.len()).unwrap_or(0);
    let cache_key = (utoc_path.to_owned(), file_size);

    {
        let guard = UTOC_LIST_CACHE.lock().map_err(|e| e.to_string())?;
        if let Some(files) = guard.get(&cache_key) {
            return Ok(files.clone());
        }
    }

    let files = iostore::list_utoc_contents(utoc_path)?;
    UTOC_LIST_CACHE
        .lock()
        .map_err(|e| e.to_string())?
        .insert(cache_key, files.clone());
    Ok(files)
}

pub(crate) fn extract_utoc(utoc_path: &str, output_dir: &str) -> Result<Vec<String>, String> {
    iostore::extract_utoc(utoc_path, output_dir)
}

pub(crate) fn extract_utoc_file(
    utoc_path: &str,
    file_name: &str,
    output_path: &str,
) -> Result<(), String> {
    iostore::extract_utoc_file(utoc_path, file_name, output_path)
}

pub(crate) fn count_utoc_legacy_packages(
    utoc_path: &str,
    game_root: &str,
    filter: &[String],
) -> Result<usize, String> {
    iostore::count_utoc_legacy_packages(utoc_path, game_root, filter)
}

pub(crate) fn extract_utoc_legacy(
    utoc_path: &str,
    game_root: &str,
    output_dir: &str,
    filter: &[String],
    app: tauri::AppHandle,
) -> Result<Vec<String>, String> {
    iostore::extract_utoc_legacy(utoc_path, game_root, output_dir, filter, app)
}

pub(crate) fn cancel_legacy_extraction() {
    iostore::cancel_legacy_extraction();
}
