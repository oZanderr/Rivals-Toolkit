use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use retoc::asset_conversion::{self, FZenPackageContext};
use retoc::iostore::IoStoreTrait;
use retoc::iostore_writer::IoStoreWriter;
use retoc::legacy_asset::FSerializedAssetBundle;
use retoc::version::EngineVersion;
use retoc::zen_asset_conversion;
use retoc::{
    Config, EIoChunkType, FIoChunkId, FSFileReader, FSFileWriter, FSHAHash, FileReaderTrait,
    UEPath, UEPathBuf,
};

const MOUNT_POINT: &str = "../../../";

/// Convert a directory of legacy assets into an IoStore container (.utoc + .ucas + .pak).
pub(crate) fn repack_iostore(input_dir: &str, output_utoc: &str) -> Result<(), String> {
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

    // Collect .uasset/.umap files that have a matching .uexp
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
    )
    .map_err(|e| e.to_string())?;

    let shader_maps: HashMap<String, Vec<FSHAHash>> = HashMap::new();
    let mut packed_paths: Vec<String> = Vec::new();

    for path in &asset_paths {
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
    }

    writer.finalize().map_err(|e| e.to_string())?;

    let pak_path = output_utoc_path.with_extension("pak");
    write_chunknames_pak(&pak_path, &packed_paths)?;

    Ok(())
}

/// Write a companion .pak with a `chunknames` entry listing the packed asset paths.
fn write_chunknames_pak(pak_path: &Path, packed_paths: &[String]) -> Result<(), String> {
    let content = packed_paths.join("\n");
    super::write_pak_bytes(
        &pak_path.to_string_lossy(),
        vec![("chunknames".to_string(), content.into_bytes())],
    )
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

/// Open the entire Paks directory as a merged IoStore backend (gives access to
/// ScriptObjects from the global container, which is needed for legacy conversion).
fn open_paks_dir(paks_dir: &Path) -> Result<Box<dyn IoStoreTrait>, String> {
    retoc::iostore::open(paks_dir, make_config()?).map_err(|e| e.to_string())
}

/// List asset paths inside a .utoc container.
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

/// Extract all raw chunks from a .utoc container, writing each to its directory-index path.
pub(crate) fn extract_utoc(utoc_path: &str, output_dir: &str) -> Result<Vec<String>, String> {
    let store = open_utoc(utoc_path)?;
    let output = Path::new(output_dir);
    std::fs::create_dir_all(output).map_err(|e| e.to_string())?;

    let mut extracted: Vec<String> = Vec::new();

    for chunk in store.chunks() {
        let Some(full_path) = chunk.path() else {
            continue;
        };
        let stripped = full_path.strip_prefix(MOUNT_POINT).unwrap_or(&full_path);

        let dest = output.join(stripped);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let data = chunk
            .read()
            .map_err(|e| format!("Failed to read {stripped}: {e}"))?;
        std::fs::write(&dest, data).map_err(|e| format!("Failed to write {stripped}: {e}"))?;

        extracted.push(stripped.to_string());
    }

    extracted.sort();
    Ok(extracted)
}

/// Extract a single file from a .utoc container by matching its path.
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

/// Extract IoStore assets to legacy format (.uasset/.uexp/.ubulk) for use in
/// tools like UAssetGUI. Opens the full Paks directory so ScriptObjects from
/// the global container are available for import resolution.
pub(crate) fn extract_utoc_legacy(
    utoc_path: &str,
    game_root: &str,
    output_dir: &str,
    filter: &[String],
) -> Result<Vec<String>, String> {
    // Determine which container name to extract from (e.g. "elsa_9999999_P")
    let target_container = Path::new(utoc_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("Invalid utoc path")?
        .to_string();

    // Open the entire Paks directory for full context (ScriptObjects, etc.)
    let paks_dir = crate::paths::paks_dir(game_root);
    let store = open_paks_dir(&paks_dir)?;

    let engine_version = EngineVersion::UE5_3;
    let log = retoc::logging::Log::no_log();
    let package_context = FZenPackageContext::create(
        &*store,
        Some(engine_version.package_file_version()),
        &log,
        None,
    );

    let writer = FSFileWriter::new(output_dir);
    let mut extracted: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    for pkg in store.packages() {
        // Only extract packages from the target container
        if pkg.container().container_name() != target_container {
            continue;
        }

        let chunk_id = FIoChunkId::from_package_id(pkg.id(), 0, EIoChunkType::ExportBundleData);
        let Some(path) = store.chunk_path(chunk_id) else {
            continue;
        };
        let stripped = path.strip_prefix(MOUNT_POINT).unwrap_or(&path);

        // Apply filter if provided
        if !filter.is_empty() && !filter.iter().any(|f| stripped.contains(f.as_str())) {
            continue;
        }

        match asset_conversion::build_legacy(
            &package_context,
            pkg.id(),
            UEPath::new(stripped),
            &writer,
        ) {
            Ok(()) => extracted.push(stripped.to_string()),
            Err(e) => errors.push(format!("{stripped}: {e}")),
        }
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
