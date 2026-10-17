//! IoStore (utoc/ucas) read, extract, and repack operations.

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use rayon::prelude::*;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use rivals_core::pak::iostore_out::{HeaderCarry, IoStoreOptions};

use crate::concurrency;

const MOUNT_POINT: &str = "../../../";

static LEGACY_CANCEL: AtomicBool = AtomicBool::new(false);
static REPACK_CANCEL: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Serialize)]
pub(crate) struct LegacyExtractionProgress {
    pub current: usize,
    pub total: usize,
}

#[derive(Clone, Serialize)]
pub(crate) struct RepackProgress {
    pub phase: &'static str,
    pub current: usize,
    pub total: usize,
}

pub(crate) fn cancel_legacy_extraction() {
    LEGACY_CANCEL.store(true, Ordering::Relaxed);
    super::in_place::cancel();
}

pub(crate) fn cancel_repack_iostore() {
    REPACK_CANCEL.store(true, Ordering::Relaxed);
    super::in_place::cancel();
}

/// Convert a directory of legacy assets into an IoStore container (.utoc + .ucas + .pak).
///
/// Files that are not part of a package bundle ride in the `.pak` beside it. `carry` restores what
/// the header of a container being rebuilt held, which the loose files cannot say. A shader archive
/// is checked against the libraries of the game at `game_root`, and refused without one.
#[allow(clippy::too_many_arguments)]
pub(crate) fn repack_iostore(
    input_dir: &str,
    output_utoc: &str,
    oodle_level: Option<retoc::OodleCompressionLevel>,
    obfuscate: bool,
    compression: super::profile::PackCompression,
    carry: &HeaderCarry,
    game_root: Option<&str>,
    app: AppHandle,
) -> Result<(), String> {
    REPACK_CANCEL.store(false, Ordering::Relaxed);
    let progress = |current, total| {
        let _ = app.emit(
            "repack-iostore-progress",
            RepackProgress {
                phase: "repacking",
                current,
                total,
            },
        );
    };
    rivals_core::pak::pack::pack_folder(
        Path::new(input_dir),
        rivals_core::pak::pack::Take::Everything,
        Path::new(output_utoc),
        &IoStoreOptions {
            compression,
            oodle_level,
            obfuscate: Some(obfuscate),
            reencode: false,
        },
        carry,
        &|| {
            let root = game_root.ok_or(
                "Set the game folder first: a shader archive is checked against the game's own \
                 libraries",
            )?;
            rivals_core::pak::shader_library::game_library_names(&rivals_core::paths::paks_dir(
                root,
            ))
        },
        concurrency::POOL.current_num_threads(),
        &REPACK_CANCEL,
        &progress,
    )?;
    Ok(())
}

use rivals_core::pak::containers::open_utoc;

pub(crate) use rivals_core::pak::containers::{
    undecryptable_container_stems, utoc_is_decryptable, utoc_is_obfuscated,
};

/// List asset paths inside a .utoc container, stripped of the mount point prefix.
pub(crate) fn list_utoc_contents(utoc_path: &str) -> Result<Vec<String>, String> {
    rivals_core::pak::containers::list_utoc_paths(Path::new(utoc_path))
}

/// Extract all raw chunks from a .utoc container to disk.
pub(crate) fn extract_utoc(utoc_path: &str, output_dir: &str) -> Result<Vec<String>, String> {
    let store = open_utoc(utoc_path)?;
    let output = Path::new(output_dir);
    std::fs::create_dir_all(output).map_err(|e| e.to_string())?;

    let chunks: Vec<_> = store
        .chunks()
        .filter_map(|chunk| {
            let full_path = chunk.path()?;
            let stripped = full_path
                .strip_prefix(MOUNT_POINT)
                .unwrap_or(&full_path)
                .to_string();
            Some((chunk, stripped))
        })
        .collect();

    let pool = &*concurrency::POOL;
    let mut extracted: Vec<String> = pool.install(|| {
        chunks
            .par_iter()
            .map(|(chunk, stripped)| {
                let dest = output.join(stripped);
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("Failed to create dir for {stripped}: {e}"))?;
                }
                let data = chunk
                    .read()
                    .map_err(|e| format!("Failed to read {stripped}: {e}"))?;
                std::fs::write(&dest, data)
                    .map_err(|e| format!("Failed to write {stripped}: {e}"))?;
                Ok(stripped.clone())
            })
            .collect::<Result<Vec<_>, String>>()
    })?;

    extracted.sort();
    Ok(extracted)
}

/// Extract specific files from a .utoc container to disk.
pub(crate) fn extract_utoc_files(
    utoc_path: &str,
    file_names: &[String],
    output_dir: &str,
) -> Result<Vec<String>, String> {
    let store = open_utoc(utoc_path)?;
    let output = Path::new(output_dir);
    std::fs::create_dir_all(output).map_err(|e| e.to_string())?;

    let wanted: HashSet<&str> = file_names.iter().map(|s| s.as_str()).collect();

    let chunks: Vec<_> = store
        .chunks()
        .filter_map(|chunk| {
            let full_path = chunk.path()?;
            let stripped = full_path
                .strip_prefix(MOUNT_POINT)
                .unwrap_or(&full_path)
                .to_string();
            if wanted.contains(stripped.as_str()) {
                Some((chunk, stripped))
            } else {
                None
            }
        })
        .collect();

    let pool = &*concurrency::POOL;
    let mut extracted: Vec<String> = pool.install(|| {
        chunks
            .par_iter()
            .map(|(chunk, stripped)| {
                let dest = output.join(stripped);
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("Failed to create dir for {stripped}: {e}"))?;
                }
                let data = chunk
                    .read()
                    .map_err(|e| format!("Failed to read {stripped}: {e}"))?;
                std::fs::write(&dest, data)
                    .map_err(|e| format!("Failed to write {stripped}: {e}"))?;
                Ok(stripped.clone())
            })
            .collect::<Result<Vec<_>, String>>()
    })?;

    extracted.sort();
    Ok(extracted)
}

/// Extract a single file from a .utoc container.
pub(crate) fn extract_utoc_file(
    utoc_path: &str,
    file_name: &str,
    output_path: &str,
) -> Result<(), String> {
    let store = open_utoc(utoc_path)?;

    for chunk in store.chunks() {
        let Some(full_path) = chunk.path() else {
            continue;
        };
        let stripped = full_path.strip_prefix(MOUNT_POINT).unwrap_or(&full_path);

        if stripped == file_name {
            if let Some(parent) = Path::new(output_path).parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let data = chunk.read().map_err(|e| e.to_string())?;
            std::fs::write(output_path, data).map_err(|e| e.to_string())?;
            return Ok(());
        }
    }

    Err(format!("File not found in container: {file_name}"))
}

/// Count legacy-convertible packages in a .utoc container.
pub(crate) fn count_utoc_legacy_packages(
    utoc_path: &str,
    game_root: &str,
    filter: &[String],
) -> Result<usize, String> {
    rivals_core::pak::extract::count_legacy_packages(Path::new(utoc_path), game_root, filter)
}

/// Extract IoStore assets to legacy format (.uasset/.uexp/.ubulk).
pub(crate) fn extract_utoc_legacy(
    utoc_path: &str,
    game_root: &str,
    output_dir: &str,
    filter: &[String],
    app: AppHandle,
) -> Result<Vec<String>, String> {
    LEGACY_CANCEL.store(false, Ordering::Relaxed);
    let progress = |current, total| {
        let _ = app.emit(
            "legacy-extraction-progress",
            LegacyExtractionProgress { current, total },
        );
    };
    let result = concurrency::POOL.install(|| {
        rivals_core::pak::extract::extract_legacy(
            Path::new(utoc_path),
            game_root,
            Path::new(output_dir),
            filter,
            &LEGACY_CANCEL,
            &progress,
        )
    })?;

    let mut extracted = result.extracted;
    let errors = result.errors;
    if !errors.is_empty() {
        let warnings: Vec<String> = errors.iter().take(5).map(|e| format!("  - {e}")).collect();
        let suffix = if errors.len() > 5 {
            format!(
                "
  ...and {} more",
                errors.len() - 5
            )
        } else {
            String::new()
        };
        extracted.push(format!(
            "__warnings__: {} asset(s) failed to convert:
{}{}",
            errors.len(),
            warnings.join(
                "
"
            ),
            suffix,
        ));
    }

    Ok(extracted)
}
