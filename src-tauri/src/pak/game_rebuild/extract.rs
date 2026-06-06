//! Dumps a single vanilla container's contents to a legacy tree ready for round-trip rebuild.

use std::collections::HashMap;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use rayon::prelude::*;
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use walkdir::WalkDir;

use retoc::asset_conversion::{self, FZenPackageContext};
use retoc::iostore::IoStoreTrait;
use retoc::version::EngineVersion;
use retoc::{EIoChunkType, FPackageId, FSFileWriter, UEPath};

use crate::concurrency;
use crate::paths::paks_dir;

const MOUNT_POINT: &str = "../../../";
const SCRIPT_OBJECTS_FILENAME: &str = "scriptobjects.bin";

static EXTRACT_CANCEL: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Serialize)]
pub(crate) struct VanillaExtractProgress {
    pub phase: &'static str,
    pub current: usize,
    pub total: usize,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ExtractReport {
    pub container_name: String,
    pub optional_container_name: Option<String>,
    pub package_count: usize,
    pub shader_library_count: usize,
    pub pak_entry_count: usize,
    pub uasset_count: usize,
    pub umap_count: usize,
    pub uexp_count: usize,
    pub ubulk_count: usize,
    pub uptnl_count: usize,
    pub memory_mapped_count: usize,
    pub script_objects_count: usize,
    pub total_files: usize,
    pub output_dir: String,
}

pub(crate) fn cancel_vanilla_extract() {
    EXTRACT_CANCEL.store(true, Ordering::Relaxed);
}

fn is_vanilla_container(name: &str) -> bool {
    name == "global" || name.starts_with("pakchunk") || name.starts_with("Patch_")
}

/// `pakchunk0optional-Windows` -> `Some("pakchunk0-Windows")`. None if no `optional` token.
fn base_name_from_optional(name: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    let idx = lower.find("optional")?;
    let mut out = String::with_capacity(name.len() - "optional".len());
    out.push_str(&name[..idx]);
    out.push_str(&name[idx + "optional".len()..]);
    Some(out)
}

fn has_entries(path: &Path) -> Result<bool, String> {
    Ok(fs::read_dir(path)
        .map_err(|e| format!("read_dir {}: {e}", path.display()))?
        .next()
        .is_some())
}

/// Removes a directory created during this run if the operation aborts; pre-existing data untouched.
struct OwnedOutputGuard<'a> {
    path: &'a Path,
    we_created_it: bool,
    disarmed: bool,
}

impl Drop for OwnedOutputGuard<'_> {
    fn drop(&mut self) {
        if !self.disarmed && self.we_created_it {
            let _ = fs::remove_dir_all(self.path);
        }
    }
}

pub(crate) fn extract_vanilla_container(
    game_root: &str,
    source_utoc: &str,
    output_dir: &str,
    app: AppHandle,
) -> Result<ExtractReport, String> {
    EXTRACT_CANCEL.store(false, Ordering::Relaxed);

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

    let we_created_output = !output_path.exists();
    if output_path.exists() {
        if has_entries(output_path)? {
            return Err("Output folder is not empty".into());
        }
    } else {
        fs::create_dir_all(output_path)
            .map_err(|e| format!("create {}: {e}", output_path.display()))?;
    }
    let mut output_guard = OwnedOutputGuard {
        path: output_path,
        we_created_it: we_created_output,
        disarmed: false,
    };

    let base_name = Path::new(source_utoc)
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| "Invalid source utoc path".to_string())?
        .to_string();

    let source_pak = Path::new(source_utoc).with_extension("pak");

    let store = retoc::iostore::open_filtered(
        &paks_root,
        super::super::profile::make_config()?,
        |name: &str| is_vanilla_container(name),
    )
    .map_err(|e| format!("open {}: {e}", paks_root.display()))?;

    let optional_name: Option<String> = store
        .child_containers()
        .map(|c| c.container_name().to_string())
        .find(|n| {
            n.to_ascii_lowercase().contains("optional")
                && base_name_from_optional(n).as_deref() == Some(base_name.as_str())
        });

    let engine_version = EngineVersion::UE5_3;
    let log = retoc::logging::Log::no_log();
    let package_context = FZenPackageContext::create(
        &*store,
        Some(engine_version.package_file_version()),
        &log,
        None,
    );

    let mut pak_entry_count = 0usize;
    let mut uncompressed_pak_entries: Vec<String> = Vec::new();
    if source_pak.is_file() {
        let source_pak_str = source_pak.to_string_lossy();
        let unpacked =
            super::super::reader::unpack_pak(source_pak_str.as_ref(), output_dir, &["chunknames"])?;
        pak_entry_count = unpacked.len();
        uncompressed_pak_entries =
            super::super::reader::pak_uncompressed_entries(source_pak_str.as_ref())?;
    }

    if EXTRACT_CANCEL.load(Ordering::Relaxed) {
        return Err("Extraction cancelled".into());
    }

    // Enumerate by physically-present ExportBundleData chunks, not header package_ids: a patch's
    // header declares the whole cumulative game, so store.packages() would pull in everything it
    // references. chunks_all() not chunks(): the deduped view sorts shared chunks away and would
    // skip ~10% of a base pakchunk's own packages.
    let mut packages_to_extract: Vec<(FPackageId, String)> = Vec::new();
    for chunk in store.chunks_all() {
        if chunk.container().container_name() != base_name {
            continue;
        }
        if chunk.id().get_chunk_type() != EIoChunkType::ExportBundleData {
            continue;
        }
        let Some(full_path) = chunk.path() else {
            continue;
        };
        let asset_path = full_path
            .strip_prefix(MOUNT_POINT)
            .unwrap_or(&full_path)
            .to_string();
        packages_to_extract.push((chunk.id().get_package_id(), asset_path));
    }

    let total_packages = packages_to_extract.len();
    let _ = app.emit(
        "vanilla-extract-progress",
        VanillaExtractProgress {
            phase: "packages",
            current: 0,
            total: total_packages,
        },
    );

    let writer = FSFileWriter::new(output_path);
    let completed = AtomicUsize::new(0);

    let pool = &*concurrency::POOL;
    let package_result: Result<(), String> = pool.install(|| {
        packages_to_extract
            .par_iter()
            .try_for_each(|(pkg_id, asset_path)| -> Result<(), String> {
                if EXTRACT_CANCEL.load(Ordering::Relaxed) {
                    return Err("Extraction cancelled".into());
                }
                asset_conversion::build_legacy(
                    &package_context,
                    *pkg_id,
                    UEPath::new(asset_path),
                    &writer,
                )
                .map_err(|e| format!("{asset_path}: {e}"))?;
                let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                if done.is_multiple_of(10) || done == total_packages {
                    let _ = app.emit(
                        "vanilla-extract-progress",
                        VanillaExtractProgress {
                            phase: "packages",
                            current: done,
                            total: total_packages,
                        },
                    );
                }
                Ok(())
            })
    });
    package_result?;
    if EXTRACT_CANCEL.load(Ordering::Relaxed) {
        return Err("Extraction cancelled".into());
    }

    // Shader libraries are not materialized here: the cumulative ShaderCodeLibrary header would pull
    // the whole multi-GB library out of the merged store. Rebuild copies the physical shader chunks
    // verbatim instead, so extract only needs the count for reporting.
    let shader_total = store
        .chunks_all()
        .filter(|c| c.id().get_chunk_type() == EIoChunkType::ShaderCodeLibrary)
        .filter(|c| c.container().container_name() == base_name)
        .count();

    let script_objects = store
        .load_script_objects()
        .map_err(|e| format!("load script objects: {e}"))?;
    let mut script_buf: Vec<u8> = Vec::new();
    script_objects
        .serialize_new(&mut Cursor::new(&mut script_buf))
        .map_err(|e| format!("serialize script objects: {e}"))?;
    write_with_dirs(&output_path.join(SCRIPT_OBJECTS_FILENAME), &script_buf)?;

    write_rebuild_manifest(output_path, &base_name, uncompressed_pak_entries)?;

    output_guard.disarmed = true;

    let counts = count_emitted_files(output_path);

    Ok(ExtractReport {
        container_name: base_name,
        optional_container_name: optional_name,
        package_count: total_packages,
        shader_library_count: shader_total,
        pak_entry_count,
        uasset_count: counts.uasset,
        umap_count: counts.umap,
        uexp_count: counts.uexp,
        ubulk_count: counts.ubulk,
        uptnl_count: counts.uptnl,
        memory_mapped_count: counts.memory_mapped,
        script_objects_count: counts.script_objects,
        total_files: counts.total,
        output_dir: output_dir.to_string(),
    })
}

#[derive(Default)]
struct EmittedCounts {
    total: usize,
    uasset: usize,
    umap: usize,
    uexp: usize,
    ubulk: usize,
    uptnl: usize,
    memory_mapped: usize,
    script_objects: usize,
}

fn count_emitted_files(root: &Path) -> EmittedCounts {
    let mut c = EmittedCounts::default();
    for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        c.total += 1;
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if name.ends_with(".m.ubulk") {
            c.memory_mapped += 1;
        } else if name.ends_with(".uasset") {
            c.uasset += 1;
        } else if name.ends_with(".umap") {
            c.umap += 1;
        } else if name.ends_with(".uexp") {
            c.uexp += 1;
        } else if name.ends_with(".ubulk") {
            c.ubulk += 1;
        } else if name.ends_with(".uptnl") {
            c.uptnl += 1;
        } else if name == SCRIPT_OBJECTS_FILENAME {
            c.script_objects += 1;
        }
    }
    c
}

fn write_with_dirs(path: &Path, data: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    fs::write(path, data).map_err(|e| format!("write {}: {e}", path.display()))
}

/// Hash every zen-package file so rebuild can tell which packages are unedited.
fn write_rebuild_manifest(
    root: &Path,
    source_container: &str,
    uncompressed_pak_entries: Vec<String>,
) -> Result<(), String> {
    let files: Vec<PathBuf> = WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_ascii_lowercase();
            let is_zen = name.ends_with(".uasset")
                || name.ends_with(".umap")
                || name.ends_with(".uexp")
                || name.ends_with(".ubulk")
                || name.ends_with(".uptnl");
            is_zen.then(|| e.path().to_path_buf())
        })
        .collect();

    let entries: HashMap<String, String> = files
        .par_iter()
        .filter_map(|p| {
            let rel = p
                .strip_prefix(root)
                .ok()?
                .to_string_lossy()
                .replace('\\', "/");
            let data = fs::read(p).ok()?;
            Some((rel, blake3::hash(&data).to_hex().to_string()))
        })
        .collect();

    let manifest = super::RebuildManifest {
        version: super::REBUILD_MANIFEST_VERSION,
        source_container: source_container.to_string(),
        entries,
        uncompressed_pak_entries,
    };
    let json = serde_json::to_vec(&manifest).map_err(|e| format!("serialize manifest: {e}"))?;

    // Write to a sibling .tmp then rename so a process crash mid-write cannot leave a torn manifest.
    let final_path = root.join(super::REBUILD_MANIFEST_FILENAME);
    let tmp_path = final_path.with_extension("json.tmp");
    fs::write(&tmp_path, json).map_err(|e| format!("write manifest tmp: {e}"))?;
    fs::rename(&tmp_path, &final_path).map_err(|e| format!("rename manifest: {e}"))
}
