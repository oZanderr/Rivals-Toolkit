//! IoStore (utoc/ucas) read, extract, and repack operations.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use rayon::prelude::*;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use retoc::iostore_writer::IoStoreWriter;
use retoc::legacy_asset::FSerializedAssetBundle;
use retoc::version::EngineVersion;
use retoc::zen_asset_conversion;
use retoc::{FSFileReader, FSHAHash, FileReaderTrait, UEPath, UEPathBuf};

use rivals_core::pak::iostore_out::HeaderCarry;

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
}

pub(crate) fn cancel_repack_iostore() {
    REPACK_CANCEL.store(true, Ordering::Relaxed);
}

/// Convert a directory of legacy assets into an IoStore container (.utoc + .ucas + .pak).
///
/// Files that are not part of a package bundle ride in the `.pak` beside it. `carry` restores what
/// the header of a container being rebuilt held, which the loose files cannot say.
#[allow(clippy::too_many_arguments)]
pub(crate) fn repack_iostore(
    input_dir: &str,
    output_utoc: &str,
    oodle_level: Option<retoc::OodleCompressionLevel>,
    obfuscate: bool,
    compression: super::profile::PackCompression,
    carry: &HeaderCarry,
    app: AppHandle,
) -> Result<(), String> {
    REPACK_CANCEL.store(false, Ordering::Relaxed);

    let input = Path::new(input_dir);
    if !input.is_dir() {
        return Err(format!("Input is not a directory: {input_dir}"));
    }

    let output_utoc_path = Path::new(output_utoc);
    if let Some(parent) = output_utoc_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let engine_version = EngineVersion::UE5_3;
    let toc_version = engine_version.toc_version();
    let container_header_version = engine_version.container_header_version();

    let reader = FSFileReader::new(input);
    let files = reader.list_files().map_err(|e| e.to_string())?;
    let files_set: HashSet<&UEPathBuf> = HashSet::from_iter(files.iter());

    let mut asset_paths: Vec<&UEPathBuf> = Vec::new();
    for path in &files {
        let ue_path: &UEPath = path.as_ref();
        let is_asset = matches!(ue_path.extension(), Some("uasset") | Some("umap"));
        if is_asset && files_set.contains(&ue_path.with_extension("uexp")) {
            asset_paths.push(path);
        }
    }

    if asset_paths.is_empty() {
        return Err(
            "No convertible assets found. Directory must contain .uasset/.umap files with matching .uexp files."
                .to_string(),
        );
    }

    let mut writer = IoStoreWriter::new(
        output_utoc_path,
        toc_version,
        Some(container_header_version),
        MOUNT_POINT.into(),
        Some(compression.retoc()),
    )
    .map_err(|e| e.to_string())?
    .with_compression_block_size(crate::pak::profile::RIVALS_BLOCK_SIZE);
    if let Some(level) = oodle_level {
        writer = writer.with_compression_level(level);
    }
    if obfuscate {
        writer = writer.with_encryption(super::profile::obfuscation_key()?);
    }

    let mut guard = IoStoreCleanupGuard {
        path: output_utoc_path,
        disarmed: false,
    };
    carry.replay(&mut writer)?;

    let shader_maps: HashMap<String, Vec<FSHAHash>> = HashMap::new();
    let total = asset_paths.len();
    let mut packed_paths: Vec<String> = Vec::new();
    for path in &asset_paths {
        let ue_path: &UEPath = path.as_ref();
        packed_paths.push(ue_path.to_string());
        packed_paths.push(ue_path.with_extension("uexp").to_string());
        for ext in ["ubulk", "uptnl", "m.ubulk"] {
            let sibling = ue_path.with_extension(ext);
            if files_set.contains(&sibling) {
                packed_paths.push(sibling.to_string());
            }
        }
    }

    let channel_cap = std::cmp::max(2, crate::concurrency::POOL.current_num_threads());
    let (tx, rx) = std::sync::mpsc::sync_channel::<
        Result<retoc::zen_asset_conversion::ConvertedZenAssetBundle, String>,
    >(channel_cap);

    rayon::in_place_scope(|scope| -> Result<(), String> {
        let asset_paths_ref = &asset_paths;
        let shader_maps_ref = &shader_maps;

        scope.spawn(move |_| {
            let pool = &*crate::concurrency::POOL;
            let log = retoc::logging::Log::no_log();
            let _ = pool.install(|| {
                asset_paths_ref
                    .par_iter()
                    .try_for_each(|path| -> Result<(), ()> {
                        if REPACK_CANCEL.load(Ordering::Relaxed) {
                            let _ = tx.send(Err("Repack cancelled".to_string()));
                            return Err(());
                        }
                        let ue_path: &UEPath = path.as_ref();
                        let bundle_result = (|| -> Result<FSerializedAssetBundle, String> {
                            Ok(FSerializedAssetBundle {
                                asset_file_buffer: reader
                                    .read(ue_path)
                                    .map_err(|e| e.to_string())?,
                                exports_file_buffer: reader
                                    .read(&ue_path.with_extension("uexp"))
                                    .map_err(|e| e.to_string())?,
                                bulk_data_buffer: reader
                                    .read_opt(&ue_path.with_extension("ubulk"))
                                    .map_err(|e| e.to_string())?,
                                optional_bulk_data_buffer: reader
                                    .read_opt(&ue_path.with_extension("uptnl"))
                                    .map_err(|e| e.to_string())?,
                                memory_mapped_bulk_data_buffer: reader
                                    .read_opt(&ue_path.with_extension("m.ubulk"))
                                    .map_err(|e| e.to_string())?,
                            })
                        })();
                        let bundle = match bundle_result {
                            Ok(b) => b,
                            Err(e) => {
                                let _ = tx.send(Err(e));
                                return Err(());
                            }
                        };
                        let mounted_path: UEPathBuf = format!("{MOUNT_POINT}{ue_path}").into();
                        let converted = match zen_asset_conversion::build_zen_asset(
                            bundle,
                            shader_maps_ref,
                            &mounted_path,
                            Some(engine_version.package_file_version()),
                            container_header_version,
                            false,
                            None,
                            None,
                            &log,
                        ) {
                            Ok(c) => c,
                            Err(e) => {
                                let _ = tx.send(Err(format!("Failed to convert {ue_path}: {e}")));
                                return Err(());
                            }
                        };
                        if tx.send(Ok(converted)).is_err() {
                            return Err(());
                        }
                        Ok(())
                    })
            });
        });

        let mut completed = 0usize;
        while let Ok(item) = rx.recv() {
            let mut converted = item?;
            carry.restore_shader_maps(&mut converted);
            converted
                .write_package_data(&mut writer)
                .map_err(|e| e.to_string())?;
            converted
                .write_and_release_bulk_data(&mut writer)
                .map_err(|e| e.to_string())?;
            completed += 1;
            if completed.is_multiple_of(10) || completed == total {
                let _ = app.emit(
                    "repack-iostore-progress",
                    RepackProgress {
                        phase: "repacking",
                        current: completed,
                        total,
                    },
                );
            }
        }

        Ok(())
    })?;

    if REPACK_CANCEL.load(Ordering::Relaxed) {
        return Err("Repack cancelled".to_string());
    }

    writer.finalize().map_err(|e| e.to_string())?;

    let pak_path = output_utoc_path.with_extension("pak");
    let mut pak_files = vec![(
        "chunknames".to_string(),
        packed_paths.join("\n").into_bytes(),
    )];
    for rel in non_package_files(input) {
        let bytes =
            std::fs::read(input.join(&rel)).map_err(|e| format!("read {}: {e}", rel.display()))?;
        pak_files.push((rel.to_string_lossy().replace('\\', "/"), bytes));
    }
    super::write_pak_bytes(&pak_path.to_string_lossy(), pak_files)?;

    guard.disarmed = true;
    Ok(())
}

/// Extensions that belong to a package bundle alongside its `.uasset`/`.umap`. `.m.ubulk` comes
/// before `.ubulk`, which it also ends with.
pub(crate) const COMPANION_EXTS: [&str; 4] = [".uexp", ".m.ubulk", ".ubulk", ".uptnl"];

/// Relative paths under `dir` that are not part of a `.uasset`/`.umap` bundle. A container cannot
/// hold them, so they ride in the pak beside it.
pub(crate) fn non_package_files(dir: &Path) -> Vec<PathBuf> {
    let files: Vec<PathBuf> = walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.path().strip_prefix(dir).ok().map(Path::to_path_buf))
        .collect();

    let lowered: HashSet<String> = files
        .iter()
        .map(|p| p.to_string_lossy().replace('\\', "/").to_lowercase())
        .collect();

    let owned_by_package = |rel: &Path| -> bool {
        let lower = rel.to_string_lossy().replace('\\', "/").to_lowercase();
        let stem = match COMPANION_EXTS.iter().find_map(|e| lower.strip_suffix(e)) {
            Some(stem) => stem.to_string(),
            None => match lower
                .strip_suffix(".uasset")
                .or_else(|| lower.strip_suffix(".umap"))
            {
                Some(stem) => stem.to_string(),
                None => return false,
            },
        };
        lowered.contains(&format!("{stem}.uexp"))
    };

    files
        .into_iter()
        .filter(|rel| !owned_by_package(rel))
        .collect()
}

fn cleanup_iostore_files(utoc_path: &Path) {
    for ext in &["utoc", "ucas", "pak"] {
        let path = utoc_path.with_extension(ext);
        let _ = std::fs::remove_file(&path);
    }
}

/// Deletes partial IoStore output files on drop unless disarmed.
struct IoStoreCleanupGuard<'a> {
    path: &'a Path,
    disarmed: bool,
}

impl Drop for IoStoreCleanupGuard<'_> {
    fn drop(&mut self) {
        if !self.disarmed {
            cleanup_iostore_files(self.path);
        }
    }
}

use rivals_core::pak::containers::open_utoc;

pub(crate) use rivals_core::pak::containers::{
    undecryptable_container_stems, utoc_is_decryptable, utoc_is_obfuscated,
};

/// List asset paths inside a .utoc container, stripped of the mount point prefix.
/// Directory-index-only: skips chunk metadata and the .ucas sibling, so disabled
/// mods and containers with malformed post-index metadata still enumerate.
pub(crate) fn list_utoc_contents(utoc_path: &str) -> Result<Vec<String>, String> {
    let file = std::fs::File::open(utoc_path).map_err(|e| e.to_string())?;
    let mut reader = std::io::BufReader::new(file);
    let config = super::profile::make_config()?;
    let raw = retoc::read_toc_paths(&mut reader, config).map_err(|e| e.to_string())?;
    let mut paths: Vec<String> = raw
        .into_iter()
        .map(|p| p.strip_prefix(MOUNT_POINT).unwrap_or(&p).to_string())
        .collect();
    paths.sort();
    Ok(paths)
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
