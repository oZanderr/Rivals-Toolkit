use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rayon::prelude::*;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use retoc::asset_conversion::{self, FZenPackageContext};
use retoc::container_header::{EIoContainerHeaderVersion, StoreEntry};
use retoc::iostore::IoStoreTrait;
use retoc::iostore::{ChunkInfo, PackageInfo};
use retoc::iostore_writer::IoStoreWriter;
use retoc::legacy_asset::FSerializedAssetBundle;
use retoc::version::EngineVersion;
use retoc::zen_asset_conversion;
use retoc::{
    Config, EIoChunkType, EIoStoreTocVersion, FIoChunkId, FIoChunkIdRaw, FPackageId, FSFileReader,
    FSFileWriter, FSHAHash, FileReaderTrait, UEPath, UEPathBuf,
};

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
pub(crate) fn repack_iostore(
    input_dir: &str,
    output_utoc: &str,
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
        Some(retoc::compression::CompressionMethod::Oodle),
    )
    .map_err(|e| e.to_string())?;

    let mut guard = IoStoreCleanupGuard {
        path: output_utoc_path,
        disarmed: false,
    };

    let shader_maps: HashMap<String, Vec<FSHAHash>> = HashMap::new();
    let total = asset_paths.len();
    let mut packed_paths: Vec<String> = Vec::new();

    for (i, path) in asset_paths.iter().enumerate() {
        if REPACK_CANCEL.load(Ordering::Relaxed) {
            return Err("Repack cancelled".to_string());
        }

        let ue_path: &UEPath = path.as_ref();

        let bundle = FSerializedAssetBundle {
            asset_file_buffer: reader.read(ue_path).map_err(|e| e.to_string())?,
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
        };

        let mounted_path: UEPathBuf = format!("{MOUNT_POINT}{ue_path}").into();
        packed_paths.push(ue_path.to_string());

        let mut converted = zen_asset_conversion::build_zen_asset(
            bundle,
            &shader_maps,
            &mounted_path,
            Some(engine_version.package_file_version()),
            container_header_version,
            false,
            None,
            None,
            &retoc::logging::Log::no_log(),
        )
        .map_err(|e| format!("Failed to convert {ue_path}: {e}"))?;

        converted
            .write_package_data(&mut writer)
            .map_err(|e| e.to_string())?;
        converted
            .write_and_release_bulk_data(&mut writer)
            .map_err(|e| e.to_string())?;

        let _ = app.emit(
            "repack-iostore-progress",
            RepackProgress {
                phase: "repacking",
                current: i + 1,
                total,
            },
        );
    }

    writer.finalize().map_err(|e| e.to_string())?;

    let pak_path = output_utoc_path.with_extension("pak");
    write_chunknames_pak(&pak_path, &packed_paths)?;

    guard.disarmed = true;
    Ok(())
}

fn write_chunknames_pak(pak_path: &Path, packed_paths: &[String]) -> Result<(), String> {
    let content = packed_paths.join("\n");
    super::write_pak_bytes(
        &pak_path.to_string_lossy(),
        vec![("chunknames".to_string(), content.into_bytes())],
    )
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

fn make_config() -> Result<Arc<Config>, String> {
    let aes_key: retoc::AesKey = super::profile::MARVEL_AES_KEY_HEX
        .parse()
        .map_err(|e| format!("{e}"))?;
    Ok(Arc::new(Config {
        aes_keys: HashMap::from([(retoc::FGuid::default(), aes_key)]),
        ..Default::default()
    }))
}

fn open_utoc(utoc_path: &str) -> Result<Box<dyn IoStoreTrait>, String> {
    retoc::iostore::open(utoc_path, make_config()?).map_err(|e| e.to_string())
}

/// Open base game containers only (excludes mods and patches).
fn open_base_game_paks(
    paks_dir: &Path,
    target_container: &str,
) -> Result<Box<dyn IoStoreTrait>, String> {
    let target = target_container.to_string();
    retoc::iostore::open_filtered(paks_dir, make_config()?, move |name| {
        if name == target {
            return true;
        }
        if name.contains("_9999999_") {
            return false;
        }
        if name.starts_with("Patch_") {
            return false;
        }
        true
    })
    .map_err(|e| e.to_string())
}

/// List asset paths inside a .utoc container, stripped of the mount point prefix.
pub(crate) fn list_utoc_contents(utoc_path: &str) -> Result<Vec<String>, String> {
    let store = open_utoc(utoc_path)?;
    let mut paths: Vec<String> = Vec::new();

    for chunk in store.chunks() {
        if let Some(full_path) = chunk.path() {
            let stripped = full_path
                .strip_prefix(MOUNT_POINT)
                .unwrap_or(&full_path)
                .to_string();
            paths.push(stripped);
        }
    }

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

    let mut extracted: Vec<String> = chunks
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
            std::fs::write(&dest, data).map_err(|e| format!("Failed to write {stripped}: {e}"))?;
            Ok(stripped.clone())
        })
        .collect::<Result<Vec<_>, String>>()?;

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

    let mut extracted: Vec<String> = chunks
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
            std::fs::write(&dest, data).map_err(|e| format!("Failed to write {stripped}: {e}"))?;
            Ok(stripped.clone())
        })
        .collect::<Result<Vec<_>, String>>()?;

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

/// Collect asset paths from a .utoc container (standalone, no base-game merge).
fn collect_target_paths(utoc_path: &str) -> Result<HashSet<String>, String> {
    let store = open_utoc(utoc_path)?;
    let paths: HashSet<String> = store
        .chunks()
        .filter_map(|chunk| {
            let full_path = chunk.path()?;
            let stripped = full_path
                .strip_prefix(MOUNT_POINT)
                .unwrap_or(&full_path)
                .to_string();
            Some(stripped)
        })
        .collect();
    Ok(paths)
}

type PackageList = Vec<(retoc::FPackageId, String)>;

/// Resolve convertible packages in a container, filtered to target-only paths.
fn resolve_target_packages(
    target_paths: &HashSet<String>,
    game_root: &str,
    target_container: &str,
    filter: &[String],
) -> Result<(Box<dyn IoStoreTrait>, PackageList), String> {
    let paks_dir = crate::paths::paks_dir(game_root);
    let store = open_base_game_paks(&paks_dir, target_container)?;

    let target = store
        .child_containers()
        .find(|c| c.container_name() == target_container)
        .ok_or_else(|| format!("Container not found: {target_container}"))?;

    let packages: Vec<_> = target
        .packages()
        .filter_map(|pkg| {
            let chunk_id = FIoChunkId::from_package_id(pkg.id(), 0, EIoChunkType::ExportBundleData);
            let path = store.chunk_path(chunk_id)?;
            let stripped = path.strip_prefix(MOUNT_POINT).unwrap_or(&path).to_string();
            if !target_paths.contains(&stripped) {
                return None;
            }
            if !filter.is_empty() && !filter.iter().any(|f| stripped.contains(f.as_str())) {
                return None;
            }
            Some((pkg.id(), stripped))
        })
        .collect();

    Ok((store, packages))
}

/// Open the target container in isolation (no base game, no other mods).
fn open_target_only(
    paks_dir: &Path,
    target_container: &str,
) -> Result<Box<dyn IoStoreTrait>, String> {
    let target = target_container.to_string();
    retoc::iostore::open_filtered(paks_dir, make_config()?, move |name| name == target)
        .map_err(|e| e.to_string())
}

/// Count legacy-convertible packages in a .utoc container.
pub(crate) fn count_utoc_legacy_packages(
    utoc_path: &str,
    game_root: &str,
    filter: &[String],
) -> Result<usize, String> {
    let target_container = Path::new(utoc_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("Invalid utoc path")?
        .to_string();

    let paks_dir = crate::paths::paks_dir(game_root);
    let store = open_target_only(&paks_dir, &target_container)?;

    let target = store
        .child_containers()
        .find(|c| c.container_name() == target_container)
        .ok_or_else(|| format!("Container not found: {target_container}"))?;

    let count = target
        .packages()
        .filter(|pkg| {
            let chunk_id = FIoChunkId::from_package_id(pkg.id(), 0, EIoChunkType::ExportBundleData);
            let Some(path) = store.chunk_path(chunk_id) else {
                return false;
            };
            let stripped = path.strip_prefix(MOUNT_POINT).unwrap_or(&path);
            filter.is_empty() || filter.iter().any(|f| stripped.contains(f.as_str()))
        })
        .count();

    Ok(count)
}

/// IoStore wrapper that scopes bulk data reads to the target container,
/// preventing base-game bulk data from leaking into mod legacy output.
struct ModScopedStore {
    full: Box<dyn IoStoreTrait>,
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

    let target_paths = collect_target_paths(utoc_path)?;
    let target_container = Path::new(utoc_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("Invalid utoc path")?
        .to_string();
    let (full_store, packages) =
        resolve_target_packages(&target_paths, game_root, &target_container, filter)?;

    let paks_dir = crate::paths::paks_dir(game_root);
    let target_store = open_target_only(&paks_dir, &target_container)?;
    let store = ModScopedStore {
        full: full_store,
        target: target_store,
    };

    let engine_version = EngineVersion::UE5_3;
    let log = retoc::logging::Log::no_log();
    let package_context = FZenPackageContext::create(
        &store,
        Some(engine_version.package_file_version()),
        &log,
        None,
    );

    let writer = FSFileWriter::new(output_dir);

    let total = packages.len();
    let completed = std::sync::atomic::AtomicUsize::new(0);

    let results: Vec<Option<Result<String, String>>> = packages
        .par_iter()
        .map(|(pkg_id, stripped)| {
            if LEGACY_CANCEL.load(Ordering::Relaxed) {
                return None;
            }

            let result = match asset_conversion::build_legacy(
                &package_context,
                *pkg_id,
                UEPath::new(stripped),
                &writer,
            ) {
                Ok(()) => Ok(stripped.clone()),
                Err(e) => Err(format!("{stripped}: {e}")),
            };

            let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
            if done.is_multiple_of(10) || done == total {
                let _ = app.emit(
                    "legacy-extraction-progress",
                    LegacyExtractionProgress {
                        current: done,
                        total,
                    },
                );
            }

            Some(result)
        })
        .collect();

    let mut extracted: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for result in results.into_iter().flatten() {
        match result {
            Ok(path) => extracted.push(path),
            Err(err) => errors.push(err),
        }
    }

    if LEGACY_CANCEL.load(Ordering::Relaxed) {
        let _ = std::fs::remove_dir_all(output_dir);
        return Err(format!(
            "Cancelled after converting {}/{total} asset(s).",
            extracted.len()
        ));
    }

    if extracted.is_empty() {
        if let Some(first_err) = errors.first() {
            return Err(format!("Legacy conversion failed: {first_err}"));
        }
        return Err("No matching packages found in container".to_string());
    }

    extracted.sort();

    if !errors.is_empty() {
        let warnings: Vec<String> = errors.iter().take(5).map(|e| format!("  - {e}")).collect();
        let suffix = if errors.len() > 5 {
            format!("\n  ...and {} more", errors.len() - 5)
        } else {
            String::new()
        };
        extracted.push(format!(
            "__warnings__: {} asset(s) failed to convert:\n{}{}",
            errors.len(),
            warnings.join("\n"),
            suffix,
        ));
    }

    Ok(extracted)
}
