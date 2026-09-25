//! Converts the packages a single IoStore container ships back into legacy `.uasset`/`.uexp` files.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use rayon::prelude::*;
use retoc::asset_conversion::{self, FZenPackageContext};
use retoc::container_header::{EIoContainerHeaderVersion, StoreEntry};
use retoc::iostore::{ChunkInfo, IoStoreTrait, PackageInfo};
use retoc::version::EngineVersion;
use retoc::{
    EIoChunkType, EIoStoreTocVersion, FIoChunkId, FIoChunkIdRaw, FPackageId, FSFileWriter, UEPath,
};

use crate::pak::containers::{open_base_game_paks, open_target_only};

const MOUNT_POINT: &str = "../../../";

/// What a legacy extraction wrote and what it could not convert.
#[derive(Debug, Default)]
pub struct LegacyExtraction {
    /// Package paths written, relative to the output folder, sorted.
    pub extracted: Vec<String>,
    /// One `path: reason` line per package that failed to convert.
    pub errors: Vec<String>,
    /// How many packages the container offered after filtering.
    pub total: usize,
}

/// Every asset path a container's directory index lists, without opening its chunks.
fn target_paths(utoc_path: &Path) -> Result<HashSet<String>, String> {
    let file = std::fs::File::open(utoc_path).map_err(|e| e.to_string())?;
    let mut reader = std::io::BufReader::new(file);
    let config = super::profile::make_config()?;
    let raw = retoc::read_toc_paths(&mut reader, config).map_err(|e| e.to_string())?;
    Ok(raw
        .into_iter()
        .map(|p| p.strip_prefix(MOUNT_POINT).unwrap_or(&p).to_string())
        .collect())
}

fn container_stem(utoc_path: &Path) -> Result<String, String> {
    utoc_path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(str::to_string)
        .ok_or_else(|| format!("invalid utoc path {}", utoc_path.display()))
}

fn matches_filter(path: &str, filter: &[String]) -> bool {
    filter.is_empty() || filter.iter().any(|f| path.contains(f.as_str()))
}

type PackageList = Vec<(FPackageId, String)>;

/// The packages the target container itself ships, read through a store that also holds the
/// base game so imports into vanilla packages resolve.
fn resolve_target_packages(
    utoc_path: &Path,
    game_root: &str,
    filter: &[String],
) -> Result<(Arc<dyn IoStoreTrait>, PackageList), String> {
    let wanted = target_paths(utoc_path)?;
    let stem = container_stem(utoc_path)?;
    let store = open_base_game_paks(&crate::paths::paks_dir(game_root), &stem)?;
    let target = store
        .child_containers()
        .find(|c| c.container_name() == stem)
        .ok_or_else(|| format!("container not found: {stem}"))?;
    let packages = target
        .packages()
        .filter_map(|pkg| {
            let chunk = FIoChunkId::from_package_id(pkg.id(), 0, EIoChunkType::ExportBundleData);
            let path = store.chunk_path(chunk)?;
            let stripped = path.strip_prefix(MOUNT_POINT).unwrap_or(&path).to_string();
            (wanted.contains(&stripped) && matches_filter(&stripped, filter))
                .then_some((pkg.id(), stripped))
        })
        .collect();
    Ok((store, packages))
}

/// How many packages `extract_legacy` would convert, without reading the base game.
pub fn count_legacy_packages(
    utoc_path: &Path,
    game_root: &str,
    filter: &[String],
) -> Result<usize, String> {
    let stem = container_stem(utoc_path)?;
    let store = open_target_only(&crate::paths::paks_dir(game_root), utoc_path, &stem)?;
    let target = store
        .child_containers()
        .find(|c| c.container_name() == stem)
        .ok_or_else(|| format!("container not found: {stem}"))?;
    Ok(target
        .packages()
        .filter(|pkg| {
            let chunk = FIoChunkId::from_package_id(pkg.id(), 0, EIoChunkType::ExportBundleData);
            store.chunk_path(chunk).is_some_and(|path| {
                matches_filter(path.strip_prefix(MOUNT_POINT).unwrap_or(&path), filter)
            })
        })
        .count())
}

/// Converts every package `utoc_path` ships (narrowed by `filter` substrings) into `output_dir`.
///
/// Runs on the current rayon pool, so a caller that wants a bounded pool installs one first.
/// `progress` sees `(done, total)` every ten packages and at the end. Setting `cancel` stops the
/// conversion and deletes `output_dir`.
pub fn extract_legacy(
    utoc_path: &Path,
    game_root: &str,
    output_dir: &Path,
    filter: &[String],
    cancel: &AtomicBool,
    progress: &(dyn Fn(usize, usize) + Sync),
) -> Result<LegacyExtraction, String> {
    let (full, packages) = resolve_target_packages(utoc_path, game_root, filter)?;
    let stem = container_stem(utoc_path)?;
    let target = open_target_only(&crate::paths::paks_dir(game_root), utoc_path, &stem)?;
    let store = ModScopedStore { full, target };

    let log = retoc::logging::Log::no_log();
    let context = FZenPackageContext::create(
        &store,
        Some(EngineVersion::UE5_3.package_file_version()),
        &log,
        None,
    )
    .with_extra_script_objects(crate::script_objects::current());
    let writer = FSFileWriter::new(output_dir);

    let total = packages.len();
    let completed = AtomicUsize::new(0);
    let results: Vec<Option<Result<String, String>>> = packages
        .par_iter()
        .map(|(id, path)| {
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            let result = asset_conversion::build_legacy(&context, *id, UEPath::new(path), &writer)
                .map(|()| path.clone())
                .map_err(|e| format!("{path}: {e}"));
            let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
            if done.is_multiple_of(10) || done == total {
                progress(done, total);
            }
            Some(result)
        })
        .collect();

    let mut out = LegacyExtraction {
        total,
        ..Default::default()
    };
    for result in results.into_iter().flatten() {
        match result {
            Ok(path) => out.extracted.push(path),
            Err(err) => out.errors.push(err),
        }
    }
    if cancel.load(Ordering::Relaxed) {
        let _ = std::fs::remove_dir_all(output_dir);
        return Err(format!(
            "Cancelled after converting {}/{total} asset(s).",
            out.extracted.len()
        ));
    }
    if out.extracted.is_empty() {
        return Err(match out.errors.first() {
            Some(first) => format!("Legacy conversion failed: {first}"),
            None => "No matching packages found in container".to_string(),
        });
    }
    out.extracted.sort();
    Ok(out)
}

/// Reads bulk data only from the target container, so base-game payloads never leak into a mod's
/// legacy output, and everything else through the full store.
struct ModScopedStore {
    full: Arc<dyn IoStoreTrait>,
    target: Box<dyn IoStoreTrait>,
}

impl ModScopedStore {
    fn is_bulk_data_type(chunk_id: FIoChunkId) -> bool {
        matches!(
            chunk_id.get_chunk_type(),
            EIoChunkType::BulkData
                | EIoChunkType::OptionalBulkData
                | EIoChunkType::MemoryMappedBulkData
        )
    }
}

impl IoStoreTrait for ModScopedStore {
    fn container_name(&self) -> &str {
        self.full.container_name()
    }
    fn container_file_version(&self) -> Option<EIoStoreTocVersion> {
        self.full.container_file_version()
    }
    fn container_header_version(&self) -> Option<EIoContainerHeaderVersion> {
        self.full.container_header_version()
    }
    fn compression_block_size(&self) -> Option<u32> {
        self.full.compression_block_size()
    }
    fn print_info(&self, depth: usize) {
        self.full.print_info(depth);
    }
    fn read(&self, chunk_id: FIoChunkId) -> retoc::anyhow::Result<Vec<u8>> {
        if Self::is_bulk_data_type(chunk_id) {
            if self.target.has_chunk_id(chunk_id) {
                self.target.read(chunk_id)
            } else {
                Ok(Vec::new())
            }
        } else {
            self.full.read(chunk_id)
        }
    }
    fn read_raw(&self, chunk_id_raw: FIoChunkIdRaw) -> retoc::anyhow::Result<Vec<u8>> {
        self.full.read_raw(chunk_id_raw)
    }
    fn has_chunk_id(&self, chunk_id: FIoChunkId) -> bool {
        if Self::is_bulk_data_type(chunk_id) {
            self.target.has_chunk_id(chunk_id)
        } else {
            self.full.has_chunk_id(chunk_id)
        }
    }
    fn has_chunk_id_raw(&self, chunk_id_raw: FIoChunkIdRaw) -> bool {
        self.full.has_chunk_id_raw(chunk_id_raw)
    }
    fn chunks(&self) -> Box<dyn Iterator<Item = ChunkInfo<'_>> + Send + '_> {
        self.full.chunks()
    }
    fn chunks_all(&self) -> Box<dyn Iterator<Item = ChunkInfo<'_>> + Send + '_> {
        self.full.chunks_all()
    }
    fn packages(&self) -> Box<dyn Iterator<Item = PackageInfo<'_>> + Send + '_> {
        self.full.packages()
    }
    fn packages_all(&self) -> Box<dyn Iterator<Item = PackageInfo<'_>> + Send + '_> {
        self.full.packages_all()
    }
    fn child_containers(&self) -> Box<dyn Iterator<Item = &dyn IoStoreTrait> + '_> {
        self.full.child_containers()
    }
    fn chunk_path(&self, chunk_id: FIoChunkId) -> Option<String> {
        self.full.chunk_path(chunk_id)
    }
    fn package_store_entry(&self, package_id: FPackageId) -> Option<StoreEntry> {
        self.full.package_store_entry(package_id)
    }
    fn lookup_package_redirect(&self, source_package_id: FPackageId) -> Option<FPackageId> {
        self.full.lookup_package_redirect(source_package_id)
    }
    fn container_header(&self) -> Option<&retoc::container_header::FIoContainerHeader> {
        self.full.container_header()
    }
    fn compression_methods(&self) -> &[retoc::compression::CompressionMethod] {
        self.full.compression_methods()
    }
}
