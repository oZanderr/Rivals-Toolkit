//! Rebuilds an edited legacy tree back into a swap-ready vanilla container set, copying unedited packages verbatim and re-converting edited ones.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rayon::prelude::*;
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use walkdir::WalkDir;

use retoc::container_header::{EIoContainerHeaderVersion, StoreEntry};
use retoc::iostore::IoStoreTrait;
use retoc::iostore_writer::IoStoreWriter;
use retoc::legacy_asset::FSerializedAssetBundle;
use retoc::script_objects::ZenScriptObjects;
use retoc::version::EngineVersion;
use retoc::zen_asset_conversion::{self, ConvertedZenAssetBundle};
use retoc::{EIoChunkType, EIoStoreTocVersion, FIoChunkId, FPackageId, FSHAHash, UEPathBuf};

use crate::game_status::{game_running_error, should_block_for_game};
use crate::paths::paks_dir;

const MOUNT_POINT: &str = "../../../";
const SCRIPT_OBJECTS_FILENAME: &str = "scriptobjects.bin";

static REBUILD_CANCEL: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Serialize)]
pub(crate) struct VanillaRebuildProgress {
    pub phase: &'static str,
    pub current: usize,
    pub total: usize,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct RebuildReport {
    pub container_name: String,
    pub optional_container_name: Option<String>,
    pub package_count: usize,
    pub uasset_count: usize,
    pub umap_count: usize,
    pub ubulk_routed: usize,
    pub uptnl_routed: usize,
    pub memory_mapped_routed: usize,
    pub shader_library_count: usize,
    pub pak_entry_count: usize,
    pub output_dir: String,
    pub outputs: Vec<String>,
}

pub(crate) fn cancel_vanilla_rebuild() {
    REBUILD_CANCEL.store(true, Ordering::Relaxed);
}

fn is_vanilla_container(name: &str) -> bool {
    name == "global" || name.starts_with("pakchunk") || name.starts_with("Patch_")
}

fn base_name_from_optional(name: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    let idx = lower.find("optional")?;
    let mut out = String::with_capacity(name.len() - "optional".len());
    out.push_str(&name[..idx]);
    out.push_str(&name[idx + "optional".len()..]);
    Some(out)
}

fn zen_package_stem(path: &Path) -> Option<PathBuf> {
    let file = path.file_name()?.to_str()?;
    let dir = path.parent()?;
    let stem = if let Some(rest) = file.strip_suffix(".m.ubulk") {
        rest
    } else if let Some(rest) = file.strip_suffix(".uasset") {
        rest
    } else if let Some(rest) = file.strip_suffix(".umap") {
        rest
    } else if let Some(rest) = file.strip_suffix(".uexp") {
        rest
    } else if let Some(rest) = file.strip_suffix(".ubulk") {
        rest
    } else if let Some(rest) = file.strip_suffix(".uptnl") {
        rest
    } else {
        return None;
    };
    Some(dir.join(stem))
}

struct WriterCleanupGuard<'a> {
    path: &'a Path,
    disarmed: bool,
}

impl Drop for WriterCleanupGuard<'_> {
    fn drop(&mut self) {
        if !self.disarmed {
            for ext in &["utoc", "ucas"] {
                let _ = fs::remove_file(self.path.with_extension(ext));
            }
        }
    }
}

fn rel_string(legacy_dir: &Path, file: &Path) -> Result<String, String> {
    let rel = file
        .strip_prefix(legacy_dir)
        .map_err(|e| e.to_string())?
        .to_string_lossy()
        .replace('\\', "/");
    Ok(rel)
}

enum ZenJob {
    /// Edited package: re-converted legacy -> zen.
    Convert {
        converted: ConvertedZenAssetBundle,
        rel: String,
        primary_ext: &'static str,
    },
    /// Unedited: chunks + store entry copied verbatim from the source, preserving header linkage.
    Copy {
        id: FPackageId,
        mounted: UEPathBuf,
        rel: String,
        primary_ext: &'static str,
        export_bundle: Vec<u8>,
        store_entry: StoreEntry,
        bulk: Vec<(EIoChunkType, Vec<u8>)>,
    },
}

/// Any failure (missing, malformed, version or source mismatch) yields an empty map -> full-convert.
fn empty_manifest() -> super::RebuildManifest {
    super::RebuildManifest {
        version: super::REBUILD_MANIFEST_VERSION,
        source_container: String::new(),
        entries: HashMap::new(),
        uncompressed_pak_entries: Vec::new(),
    }
}

fn load_manifest(legacy_path: &Path, expected_source: &str) -> super::RebuildManifest {
    let Ok(bytes) = fs::read(legacy_path.join(super::REBUILD_MANIFEST_FILENAME)) else {
        return empty_manifest();
    };
    let Ok(m) = serde_json::from_slice::<super::RebuildManifest>(&bytes) else {
        return empty_manifest();
    };
    if m.version != super::REBUILD_MANIFEST_VERSION {
        eprintln!(
            "vanilla rebuild: manifest version {} != expected {}; full-convert",
            m.version,
            super::REBUILD_MANIFEST_VERSION
        );
        return empty_manifest();
    }
    if m.source_container != expected_source {
        eprintln!(
            "vanilla rebuild: manifest source_container {:?} != selected {:?}; full-convert",
            m.source_container, expected_source
        );
        return empty_manifest();
    }
    m
}

/// True if every on-disk file for this package matches the extraction manifest (i.e. unedited).
fn package_unchanged(stem: &Path, legacy_path: &Path, manifest: &HashMap<String, String>) -> bool {
    let mut any = false;
    for ext in ["uasset", "umap", "uexp", "ubulk", "uptnl", "m.ubulk"] {
        let path = stem.with_extension(ext);
        if !path.is_file() {
            continue;
        }
        any = true;
        let Ok(rel) = rel_string(legacy_path, &path) else {
            return false;
        };
        let Ok(data) = fs::read(&path) else {
            return false;
        };
        let hash = blake3::hash(&data).to_hex().to_string();
        match manifest.get(&rel) {
            Some(h) if *h == hash => {}
            _ => return false,
        }
    }
    any
}

/// Build a verbatim-copy job: read the package's chunks straight from the source container.
fn build_copy_job(
    id: FPackageId,
    stem: &Path,
    legacy_path: &Path,
    store: &dyn IoStoreTrait,
    own_store: &dyn IoStoreTrait,
    base_chunks: &HashSet<FIoChunkId>,
    toc_version: EIoStoreTocVersion,
) -> Result<ZenJob, String> {
    let uasset_path = stem.with_extension("uasset");
    let (primary_path, primary_ext): (PathBuf, &'static str) = if uasset_path.is_file() {
        (uasset_path, "uasset")
    } else {
        (stem.with_extension("umap"), "umap")
    };
    let rel = rel_string(legacy_path, &primary_path)?;
    let mounted: UEPathBuf = format!("{MOUNT_POINT}{rel}").into();

    let eb_id = FIoChunkId::from_package_id(id, 0, EIoChunkType::ExportBundleData);
    let export_bundle = store
        .read(eb_id)
        .map_err(|e| format!("read source package {rel}: {e}"))?;
    let store_entry = own_store
        .package_store_entry(id)
        .ok_or_else(|| format!("no source store entry for {rel}"))?;

    let mut bulk = Vec::new();
    for ty in [
        EIoChunkType::BulkData,
        EIoChunkType::OptionalBulkData,
        EIoChunkType::MemoryMappedBulkData,
    ] {
        let cid = FIoChunkId::from_package_id(id, 0, ty);
        // Only copy bulk that physically belongs to this container. Bulk a package merely
        // references from another container (a base pakchunk or earlier patch) is left out so it
        // resolves from its home container at runtime, reproducing the original's chunk set.
        if base_chunks.contains(&cid.with_version(toc_version)) {
            let data = store
                .read(cid)
                .map_err(|e| format!("read source bulk {rel}: {e}"))?;
            bulk.push((ty, data));
        }
    }

    Ok(ZenJob::Copy {
        id,
        mounted,
        rel,
        primary_ext,
        export_bundle,
        store_entry,
        bulk,
    })
}

fn build_zen_job(
    stem: &Path,
    legacy_path: &Path,
    shader_maps: &HashMap<String, Vec<FSHAHash>>,
    script_objects: Option<Arc<ZenScriptObjects>>,
    header_version: EIoContainerHeaderVersion,
    log: &retoc::logging::Log,
) -> Result<ZenJob, String> {
    let uasset_path = stem.with_extension("uasset");
    let umap_path = stem.with_extension("umap");
    let (primary_path, primary_ext): (PathBuf, &'static str) = if uasset_path.is_file() {
        (uasset_path, "uasset")
    } else if umap_path.is_file() {
        (umap_path, "umap")
    } else {
        return Err(format!("Missing .uasset/.umap for {}", stem.display()));
    };
    let uexp_path = stem.with_extension("uexp");
    let ubulk_path = stem.with_extension("ubulk");
    let uptnl_path = stem.with_extension("uptnl");
    let m_ubulk_path = stem.with_extension("m.ubulk");

    let rel = rel_string(legacy_path, &primary_path)?;
    let mounted_path: UEPathBuf = format!("{MOUNT_POINT}{rel}").into();

    let asset_bytes =
        fs::read(&primary_path).map_err(|e| format!("read {}: {e}", primary_path.display()))?;
    let exp_bytes =
        fs::read(&uexp_path).map_err(|e| format!("read {}: {e}", uexp_path.display()))?;

    let bundle = FSerializedAssetBundle {
        asset_file_buffer: asset_bytes,
        exports_file_buffer: exp_bytes,
        bulk_data_buffer: fs::read(&ubulk_path).ok(),
        optional_bulk_data_buffer: fs::read(&uptnl_path).ok(),
        memory_mapped_bulk_data_buffer: fs::read(&m_ubulk_path).ok(),
    };

    let converted = zen_asset_conversion::build_zen_asset(
        bundle,
        shader_maps,
        &mounted_path,
        Some(EngineVersion::UE5_3.package_file_version()),
        header_version,
        false,
        script_objects,
        None,
        log,
    )
    .map_err(|e| format!("build_zen_asset {rel}: {e}"))?;

    Ok(ZenJob::Convert {
        converted,
        rel,
        primary_ext,
    })
}

pub(crate) fn rebuild_vanilla_container(
    game_root: &str,
    source_utoc: &str,
    legacy_dir: &str,
    output_dir: &str,
    vanilla_oodle_level: retoc::OodleCompressionLevel,
    app: AppHandle,
) -> Result<RebuildReport, String> {
    REBUILD_CANCEL.store(false, Ordering::Relaxed);

    if should_block_for_game() {
        return Err(game_running_error());
    }

    let legacy_path = Path::new(legacy_dir);
    let output_path = Path::new(output_dir);
    let paks_root = paks_dir(game_root);
    if !paks_root.is_dir() {
        return Err(format!("Paks directory not found: {}", paks_root.display()));
    }

    let canon_paks = paks_root.canonicalize().ok();
    let canon_output = output_path.canonicalize().ok();
    if let (Some(out), Some(p)) = (canon_output.as_ref(), canon_paks.as_ref())
        && out.starts_with(p)
    {
        return Err("Output folder must be outside the game Paks directory".into());
    }
    if !legacy_path.is_dir() {
        return Err(format!(
            "Legacy folder not found: {}",
            legacy_path.display()
        ));
    }
    fs::create_dir_all(output_path)
        .map_err(|e| format!("create {}: {e}", output_path.display()))?;

    let base_name = Path::new(source_utoc)
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| "Invalid source utoc path".to_string())?
        .to_string();

    let store = retoc::iostore::open_filtered(
        &paks_root,
        super::super::profile::make_config()?,
        |name: &str| is_vanilla_container(name),
    )
    .map_err(|e| format!("open {}: {e}", paks_root.display()))?;

    let toc_version = store
        .container_file_version()
        .ok_or_else(|| "No TOC version on store".to_string())?;
    let header_version = store
        .container_header_version()
        .ok_or_else(|| "No container header version on store".to_string())?;

    let optional_name: Option<String> = store
        .child_containers()
        .map(|c| c.container_name().to_string())
        .find(|n| {
            n.to_ascii_lowercase().contains("optional")
                && base_name_from_optional(n).as_deref() == Some(base_name.as_str())
        });

    // Store entries (shader_map_hashes especially) come from the container's own header, not the
    // merged store: a patch's merged shader resolution diverges from its own entries.
    let own_store = {
        let base = base_name.clone();
        let optional = optional_name.clone();
        retoc::iostore::open_filtered(
            &paks_root,
            super::super::profile::make_config()?,
            move |name: &str| name == base || optional.as_deref() == Some(name),
        )
        .map_err(|e| format!("open source container {}: {e}", paks_root.display()))?
    };

    let mut source_id_map: HashMap<String, FPackageId> = HashMap::new();
    let mut source_shader_hashes: HashMap<u64, Vec<FSHAHash>> = HashMap::new();
    // Chunks physically resident here (and which ship uncompressed) so the rebuild reproduces the
    // original layout. Ids built via `from_package_id` need `.with_version(toc_version)` before any
    // set lookup (FIoChunkId hashing panics on unversioned ids).
    let mut base_chunks: HashSet<FIoChunkId> = HashSet::new();
    let mut raw_chunks: HashSet<FIoChunkId> = HashSet::new();
    // chunks_all() not the deduped chunks(): a base pakchunk's own chunks also live in global/patches
    // and sort first, so chunks() would drop ~10% of them. The optional sibling counts as owned too.
    for chunk in store.chunks_all() {
        let cname = chunk.container().container_name();
        let in_base = cname == base_name;
        let in_optional = optional_name.as_deref() == Some(cname);
        if !in_base && !in_optional {
            continue;
        }
        base_chunks.insert(chunk.id());
        if !chunk.is_compressed() {
            raw_chunks.insert(chunk.id());
        }
        // source_id_map / shader hashes are keyed off base-container packages only.
        if !in_base || chunk.id().get_chunk_type() != EIoChunkType::ExportBundleData {
            continue;
        }
        let id = chunk.id().get_package_id();
        if let Some(path) = store.chunk_path(chunk.id())
            && let Some(rel) = path.strip_prefix(MOUNT_POINT)
        {
            // Lowercase the key: a few packages are cooked with a case differing from their sibling
            // dir (e.g. UI/ability vs UI/Ability), which Windows folds together on disk; a
            // case-sensitive lookup would miss and demote Copy to Convert.
            let stem = rel
                .trim_end_matches(".uasset")
                .trim_end_matches(".umap")
                .to_ascii_lowercase();
            source_id_map.insert(stem, id);
        }
        if let Some(entry) = own_store.package_store_entry(id)
            && !entry.shader_map_hashes.is_empty()
        {
            source_shader_hashes.insert(id.0, entry.shader_map_hashes);
        }
    }

    let manifest = load_manifest(legacy_path, &base_name);

    let mut all_files: Vec<PathBuf> = Vec::new();
    for entry in WalkDir::new(legacy_path).into_iter().filter_map(|e| e.ok()) {
        if entry.file_type().is_file() {
            all_files.push(entry.path().to_path_buf());
        }
    }
    let file_set: HashSet<PathBuf> = all_files.iter().cloned().collect();

    let mut zen_stems: HashSet<PathBuf> = HashSet::new();
    let script_objects_path = legacy_path.join(SCRIPT_OBJECTS_FILENAME);
    let has_script_objects = script_objects_path.is_file();

    for path in &all_files {
        let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
        if ext.eq_ignore_ascii_case("uasset") || ext.eq_ignore_ascii_case("umap") {
            let uexp = path.with_extension("uexp");
            if file_set.contains(&uexp) {
                zen_stems.insert(path.with_extension(""));
            }
        }
    }

    // Entries the source stored uncompressed are rebuilt verbatim (Oodle skipped) so they
    // reproduce byte-for-byte; everything else is compressed at the configured level.
    let uncompressed_set: HashSet<&str> = manifest
        .uncompressed_pak_entries
        .iter()
        .map(String::as_str)
        .collect();

    let mut pak_entries: Vec<(String, PathBuf, bool)> = Vec::new();
    for path in &all_files {
        if has_script_objects && path == &script_objects_path {
            continue;
        }
        if path.file_name().and_then(|s| s.to_str()) == Some(super::REBUILD_MANIFEST_FILENAME) {
            continue;
        }
        if let Some(stem) = zen_package_stem(path)
            && zen_stems.contains(&stem)
        {
            continue;
        }
        let rel = rel_string(legacy_path, path)?;
        let compress = !uncompressed_set.contains(rel.as_str());
        pak_entries.push((rel, path.clone(), compress));
    }

    // Edited packages recover their shader-map hashes from `source_shader_hashes` below, so the
    // converter does not need a package->shader-map table sourced from a legacy shader library.
    let shader_maps: HashMap<String, Vec<FSHAHash>> = HashMap::new();

    let script_objects: Option<Arc<ZenScriptObjects>> = if has_script_objects {
        let bytes =
            fs::read(&script_objects_path).map_err(|e| format!("read script objects: {e}"))?;
        Some(Arc::new(
            ZenScriptObjects::deserialize_new(&mut Cursor::new(bytes))
                .map_err(|e| format!("parse script objects: {e}"))?,
        ))
    } else {
        None
    };

    let has_optional_content = zen_stems.iter().any(|stem| {
        let uptnl = stem.with_extension("uptnl");
        let m_ubulk = stem.with_extension("m.ubulk");
        file_set.contains(&uptnl) || file_set.contains(&m_ubulk)
    });

    let block_size = store
        .compression_block_size()
        .unwrap_or(crate::pak::profile::RIVALS_BLOCK_SIZE);

    let base_utoc = output_path.join(format!("{base_name}.utoc"));
    let mut base_writer = IoStoreWriter::new(
        &base_utoc,
        toc_version,
        Some(header_version),
        MOUNT_POINT.into(),
        Some(retoc::compression::CompressionMethod::Oodle),
    )
    .map_err(|e| format!("open base writer: {e}"))?
    .with_compression_level(vanilla_oodle_level)
    .with_compression_block_size(block_size);
    let mut base_guard = WriterCleanupGuard {
        path: base_utoc.as_path(),
        disarmed: false,
    };

    let optional_utoc: Option<PathBuf> = match (&optional_name, has_optional_content) {
        (Some(n), true) => Some(output_path.join(format!("{n}.utoc"))),
        _ => None,
    };
    let mut optional_writer: Option<IoStoreWriter> = match &optional_utoc {
        Some(p) => Some(
            IoStoreWriter::new(
                p,
                toc_version,
                Some(header_version),
                MOUNT_POINT.into(),
                Some(retoc::compression::CompressionMethod::Oodle),
            )
            .map_err(|e| format!("open optional writer: {e}"))?
            .with_compression_level(vanilla_oodle_level)
            .with_compression_block_size(block_size),
        ),
        None => None,
    };
    let mut optional_guard: Option<WriterCleanupGuard> =
        optional_utoc.as_deref().map(|p| WriterCleanupGuard {
            path: p,
            disarmed: false,
        });

    let mut zen_paths: Vec<PathBuf> = zen_stems.iter().cloned().collect();
    zen_paths.sort();
    let zen_total = zen_paths.len();

    let unchanged_ids: HashMap<PathBuf, FPackageId> = zen_paths
        .par_iter()
        .filter_map(|stem| {
            let rel_stem = stem
                .strip_prefix(legacy_path)
                .ok()?
                .to_string_lossy()
                .replace('\\', "/")
                .to_ascii_lowercase();
            let id = *source_id_map.get(&rel_stem)?;
            if !package_unchanged(stem, legacy_path, &manifest.entries) {
                return None;
            }
            Some((stem.clone(), id))
        })
        .collect();

    let _ = app.emit(
        "vanilla-rebuild-progress",
        VanillaRebuildProgress {
            phase: "packages",
            current: 0,
            total: zen_total,
        },
    );

    let mut uasset_count = 0usize;
    let mut umap_count = 0usize;
    let mut ubulk_routed = 0usize;
    let mut uptnl_routed = 0usize;
    let mut memory_mapped_routed = 0usize;
    let mut completed = 0usize;

    // Bounded so slow chunk writes back-pressure the build phase instead of queuing into memory growth.
    let channel_cap = std::cmp::max(2, crate::concurrency::POOL.current_num_threads() / 2);
    let (tx, rx) = std::sync::mpsc::sync_channel::<Result<ZenJob, String>>(channel_cap);

    rayon::in_place_scope(|scope| -> Result<(), String> {
        let shader_maps_ref = &shader_maps;
        let script_objects_ref = &script_objects;
        let legacy_path_ref = legacy_path;
        let zen_paths_ref = &zen_paths;
        let unchanged_ids_ref = &unchanged_ids;
        let store_ref: &dyn IoStoreTrait = &*store;
        let own_store_ref: &dyn IoStoreTrait = &*own_store;
        let base_chunks_ref = &base_chunks;

        scope.spawn(move |_| {
            let pool = &*crate::concurrency::POOL;
            let log = retoc::logging::Log::no_log();
            let _ = pool.install(|| {
                zen_paths_ref
                    .par_iter()
                    .try_for_each(|stem| -> Result<(), ()> {
                        if REBUILD_CANCEL.load(Ordering::Relaxed) {
                            let _ = tx.send(Err("Rebuild cancelled".into()));
                            return Err(());
                        }
                        let result = match unchanged_ids_ref.get(stem) {
                            Some(&id) => build_copy_job(
                                id,
                                stem,
                                legacy_path_ref,
                                store_ref,
                                own_store_ref,
                                base_chunks_ref,
                                toc_version,
                            ),
                            None => build_zen_job(
                                stem,
                                legacy_path_ref,
                                shader_maps_ref,
                                script_objects_ref.clone(),
                                header_version,
                                &log,
                            ),
                        };
                        let send_result = match result {
                            Ok(job) => tx.send(Ok(job)),
                            Err(e) => tx.send(Err(e)),
                        };
                        if send_result.is_err() {
                            return Err(());
                        }
                        Ok(())
                    })
            });
        });

        while let Ok(item) = rx.recv() {
            match item? {
                ZenJob::Convert {
                    mut converted,
                    rel,
                    primary_ext,
                } => {
                    match primary_ext {
                        "uasset" => uasset_count += 1,
                        "umap" => umap_count += 1,
                        _ => {}
                    }
                    if converted.store_entry().shader_map_hashes.is_empty()
                        && let Some(hashes) = source_shader_hashes.get(&converted.package_id.0)
                    {
                        converted.set_shader_map_hashes(hashes.clone());
                    }
                    converted
                        .write_package_data(&mut base_writer)
                        .map_err(|e| format!("write_package_data {rel}: {e}"))?;
                    let pkg_id = converted.package_id;

                    if let Some(bytes) = converted.take_bulk_data() {
                        let id = FIoChunkId::from_package_id(pkg_id, 0, EIoChunkType::BulkData);
                        let path = converted.mounted_path().with_extension("ubulk");
                        base_writer
                            .write_chunk(id, Some(&path), &bytes)
                            .map_err(|e| format!("write ubulk {rel}: {e}"))?;
                        ubulk_routed += 1;
                    }
                    if let Some(bytes) = converted.take_optional_bulk_data() {
                        let id =
                            FIoChunkId::from_package_id(pkg_id, 0, EIoChunkType::OptionalBulkData);
                        let path = converted.mounted_path().with_extension("uptnl");
                        match &mut optional_writer {
                            Some(w) => w.write_chunk(id, Some(&path), &bytes),
                            None => base_writer.write_chunk(id, Some(&path), &bytes),
                        }
                        .map_err(|e| format!("write uptnl {rel}: {e}"))?;
                        uptnl_routed += 1;
                    }
                    if let Some(bytes) = converted.take_memory_mapped_bulk_data() {
                        let id = FIoChunkId::from_package_id(
                            pkg_id,
                            0,
                            EIoChunkType::MemoryMappedBulkData,
                        );
                        let path = converted.mounted_path().with_extension("m.ubulk");
                        match &mut optional_writer {
                            Some(w) => w.write_chunk(id, Some(&path), &bytes),
                            None => base_writer.write_chunk(id, Some(&path), &bytes),
                        }
                        .map_err(|e| format!("write m.ubulk {rel}: {e}"))?;
                        memory_mapped_routed += 1;
                    }
                    drop(converted);
                }
                ZenJob::Copy {
                    id,
                    mounted,
                    rel,
                    primary_ext,
                    export_bundle,
                    store_entry,
                    bulk,
                } => {
                    match primary_ext {
                        "uasset" => uasset_count += 1,
                        "umap" => umap_count += 1,
                        _ => {}
                    }
                    let eb_id = FIoChunkId::from_package_id(id, 0, EIoChunkType::ExportBundleData);
                    // Preserve the source's per-chunk compression decision. `with_version` is
                    // required before the set lookup (FIoChunkId hashing panics on unversioned ids).
                    if raw_chunks.contains(&eb_id.with_version(toc_version)) {
                        base_writer.write_package_chunk_uncompressed(
                            eb_id,
                            Some(&mounted),
                            &export_bundle,
                            &store_entry,
                        )
                    } else {
                        base_writer.write_package_chunk(
                            eb_id,
                            Some(&mounted),
                            &export_bundle,
                            &store_entry,
                        )
                    }
                    .map_err(|e| format!("copy package {rel}: {e}"))?;
                    for (ty, bytes) in bulk {
                        let cid = FIoChunkId::from_package_id(id, 0, ty);
                        let raw = raw_chunks.contains(&cid.with_version(toc_version));
                        match ty {
                            EIoChunkType::BulkData => {
                                let path = mounted.with_extension("ubulk");
                                if raw {
                                    base_writer.write_chunk_uncompressed(cid, Some(&path), &bytes)
                                } else {
                                    base_writer.write_chunk(cid, Some(&path), &bytes)
                                }
                                .map_err(|e| format!("copy ubulk {rel}: {e}"))?;
                                ubulk_routed += 1;
                            }
                            EIoChunkType::OptionalBulkData => {
                                let path = mounted.with_extension("uptnl");
                                match &mut optional_writer {
                                    Some(w) if raw => {
                                        w.write_chunk_uncompressed(cid, Some(&path), &bytes)
                                    }
                                    Some(w) => w.write_chunk(cid, Some(&path), &bytes),
                                    None if raw => base_writer.write_chunk_uncompressed(
                                        cid,
                                        Some(&path),
                                        &bytes,
                                    ),
                                    None => base_writer.write_chunk(cid, Some(&path), &bytes),
                                }
                                .map_err(|e| format!("copy uptnl {rel}: {e}"))?;
                                uptnl_routed += 1;
                            }
                            EIoChunkType::MemoryMappedBulkData => {
                                let path = mounted.with_extension("m.ubulk");
                                match &mut optional_writer {
                                    Some(w) if raw => {
                                        w.write_chunk_uncompressed(cid, Some(&path), &bytes)
                                    }
                                    Some(w) => w.write_chunk(cid, Some(&path), &bytes),
                                    None if raw => base_writer.write_chunk_uncompressed(
                                        cid,
                                        Some(&path),
                                        &bytes,
                                    ),
                                    None => base_writer.write_chunk(cid, Some(&path), &bytes),
                                }
                                .map_err(|e| format!("copy m.ubulk {rel}: {e}"))?;
                                memory_mapped_routed += 1;
                            }
                            _ => {}
                        }
                    }
                }
            }

            completed += 1;
            if completed.is_multiple_of(10) || completed == zen_total {
                let _ = app.emit(
                    "vanilla-rebuild-progress",
                    VanillaRebuildProgress {
                        phase: "packages",
                        current: completed,
                        total: zen_total,
                    },
                );
            }
        }

        Ok(())
    })?;

    if REBUILD_CANCEL.load(Ordering::Relaxed) {
        return Err("Rebuild cancelled".into());
    }

    // Copy this container's physically-present shader chunks verbatim rather than rebuilding the
    // ShaderCodeLibrary: its header is cumulative (the whole game's groups), so a rebuild would pull
    // every base group out of the merged store (multi-GB). Copying base_name's own chunks keeps the
    // original layout; header references to base groups still resolve against the untouched containers.
    let shader_chunk_ids: Vec<FIoChunkId> = store
        .chunks_all()
        .filter(|c| c.container().container_name() == base_name)
        .map(|c| c.id())
        .filter(|id| {
            matches!(
                id.get_chunk_type(),
                EIoChunkType::ShaderCodeLibrary | EIoChunkType::ShaderCode
            )
        })
        .collect();
    let shader_total = shader_chunk_ids.len();
    let mut shader_library_count = 0usize;
    if shader_total > 0 {
        let _ = app.emit(
            "vanilla-rebuild-progress",
            VanillaRebuildProgress {
                phase: "shaders",
                current: 0,
                total: shader_total,
            },
        );
    }
    for (i, chunk_id) in shader_chunk_ids.iter().enumerate() {
        if REBUILD_CANCEL.load(Ordering::Relaxed) {
            return Err("Rebuild cancelled".into());
        }
        let bytes = store
            .read(*chunk_id)
            .map_err(|e| format!("read shader chunk {chunk_id:?}: {e}"))?;
        let mounted: Option<UEPathBuf> = store.chunk_path(*chunk_id).map(Into::into);
        // Shader ids come straight from `chunks()` so they are already version-tagged.
        if raw_chunks.contains(chunk_id) {
            base_writer.write_chunk_uncompressed(*chunk_id, mounted.as_deref(), &bytes)
        } else {
            base_writer.write_chunk(*chunk_id, mounted.as_deref(), &bytes)
        }
        .map_err(|e| format!("write shader chunk {chunk_id:?}: {e}"))?;
        if chunk_id.get_chunk_type() == EIoChunkType::ShaderCodeLibrary {
            shader_library_count += 1;
        }
        let _ = app.emit(
            "vanilla-rebuild-progress",
            VanillaRebuildProgress {
                phase: "shaders",
                current: i + 1,
                total: shader_total,
            },
        );
    }

    let _ = app.emit(
        "vanilla-rebuild-progress",
        VanillaRebuildProgress {
            phase: "finalize",
            current: 0,
            total: 0,
        },
    );

    base_writer
        .finalize()
        .map_err(|e| format!("finalize base: {e}"))?;
    base_guard.disarmed = true;

    if let Some(w) = optional_writer.take() {
        w.finalize()
            .map_err(|e| format!("finalize optional: {e}"))?;
    }
    if let Some(g) = optional_guard.as_mut() {
        g.disarmed = true;
    }

    let base_pak = output_path.join(format!("{base_name}.pak"));
    let pak_entry_count = pak_entries.len();
    let _ = app.emit(
        "vanilla-rebuild-progress",
        VanillaRebuildProgress {
            phase: "writing pak",
            current: 0,
            total: 0,
        },
    );
    if pak_entry_count > 0 {
        super::super::writer::write_pak_streaming(
            base_pak.to_string_lossy().as_ref(),
            pak_entries,
            Some(vanilla_oodle_level),
        )?;
    } else {
        super::super::writer::write_empty_pak(
            base_pak.to_string_lossy().as_ref(),
            Some(vanilla_oodle_level),
        )?;
    }

    let mut outputs = vec![
        output_path
            .join(format!("{base_name}.utoc"))
            .to_string_lossy()
            .into_owned(),
        output_path
            .join(format!("{base_name}.ucas"))
            .to_string_lossy()
            .into_owned(),
        base_pak.to_string_lossy().into_owned(),
    ];

    let emitted_optional_name =
        if let Some(name) = optional_name.as_ref().filter(|_| has_optional_content) {
            let optional_pak = output_path.join(format!("{name}.pak"));
            super::super::writer::write_empty_pak(
                optional_pak.to_string_lossy().as_ref(),
                Some(vanilla_oodle_level),
            )?;
            outputs.push(
                output_path
                    .join(format!("{name}.utoc"))
                    .to_string_lossy()
                    .into_owned(),
            );
            outputs.push(
                output_path
                    .join(format!("{name}.ucas"))
                    .to_string_lossy()
                    .into_owned(),
            );
            outputs.push(optional_pak.to_string_lossy().into_owned());
            Some(name.clone())
        } else {
            None
        };

    Ok(RebuildReport {
        container_name: base_name,
        optional_container_name: emitted_optional_name,
        package_count: zen_total,
        uasset_count,
        umap_count,
        ubulk_routed,
        uptnl_routed,
        memory_mapped_routed,
        shader_library_count,
        pak_entry_count,
        output_dir: output_dir.to_string(),
        outputs,
    })
}
