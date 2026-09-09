//! Produces the legacy .uasset and .uexp bytes for one asset without writing anything to disk.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use retoc::asset_conversion::{self, FZenPackageContext};
use retoc::iostore::IoStoreTrait;
use retoc::legacy_asset::FSerializedAssetBundle;
use retoc::version::EngineVersion;
use retoc::{EIoChunkType, FIoChunkId, FileWriterTrait, UEPath};

use crate::pak::containers::{MOUNT_POINT, open_base_game_paks};
use crate::pak::crypto::open_pak;

const ENGINE_VERSION: EngineVersion = EngineVersion::UE5_3;

/// Captures what `build_legacy` would have written, keyed by the path it asked for.
#[derive(Default)]
struct MemFileWriter {
    files: Mutex<HashMap<String, Vec<u8>>>,
}

impl FileWriterTrait for MemFileWriter {
    fn write_file(&self, path: String, _allow_compress: bool, data: Vec<u8>) -> anyhow::Result<()> {
        match self.files.lock() {
            Ok(mut files) => {
                files.insert(path, data);
                Ok(())
            }
            Err(_) => anyhow::bail!("in-memory writer lock was poisoned"),
        }
    }
}

impl MemFileWriter {
    /// `build_legacy` names its outputs itself, so classify what came back by extension rather
    /// than guessing at the keys.
    fn into_bundle(self, what: &str) -> Result<FSerializedAssetBundle, String> {
        let files = self
            .files
            .into_inner()
            .map_err(|_| "in-memory writer lock was poisoned".to_string())?;
        let mut bundle = FSerializedAssetBundle {
            asset_file_buffer: Vec::new(),
            exports_file_buffer: Vec::new(),
            bulk_data_buffer: None,
            optional_bulk_data_buffer: None,
            memory_mapped_bulk_data_buffer: None,
        };
        let mut seen_header = false;
        for (path, data) in files {
            let lowered = path.to_ascii_lowercase();
            if lowered.ends_with(".uasset") || lowered.ends_with(".umap") {
                bundle.asset_file_buffer = data;
                seen_header = true;
            } else if lowered.ends_with(".uexp") {
                bundle.exports_file_buffer = data;
            } else if lowered.ends_with(".m.ubulk") {
                bundle.memory_mapped_bulk_data_buffer = Some(data);
            } else if lowered.ends_with(".ubulk") {
                bundle.bulk_data_buffer = Some(data);
            } else if lowered.ends_with(".uptnl") {
                bundle.optional_bulk_data_buffer = Some(data);
            }
        }
        if !seen_header {
            return Err(format!("legacy conversion produced no header for {what}"));
        }
        Ok(bundle)
    }
}

/// Where an asset lives, mirroring the `pak | utoc` split the asset browser already tracks.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AssetSource {
    Pak,
    Utoc,
    /// A `.uasset` sitting on disk beside its siblings, as produced by legacy extraction.
    Loose,
}

/// Reads the legacy form of one asset. For `.pak` sources that is the stored bytes; for IoStore
/// sources the zen package is converted in memory first.
pub fn load_bundle(
    game_root: &str,
    container: &str,
    entry: &str,
    source: AssetSource,
) -> Result<FSerializedAssetBundle, String> {
    match source {
        AssetSource::Pak => load_from_pak(container, entry),
        AssetSource::Utoc => load_from_utoc(game_root, container, entry),
        AssetSource::Loose => load_from_disk(Path::new(entry)),
    }
}

/// Reads an already-extracted package straight off disk. `path` points at the `.uasset` or
/// `.umap`; the siblings are found by swapping the extension.
pub fn load_from_disk(path: &Path) -> Result<FSerializedAssetBundle, String> {
    let asset_file_buffer =
        std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let sibling = |extension: &str| {
        let mut name = path.to_path_buf();
        name.set_extension(extension);
        std::fs::read(name).ok()
    };
    Ok(FSerializedAssetBundle {
        asset_file_buffer,
        exports_file_buffer: sibling("uexp").unwrap_or_default(),
        bulk_data_buffer: sibling("ubulk"),
        optional_bulk_data_buffer: sibling("uptnl"),
        memory_mapped_bulk_data_buffer: sibling("m.ubulk"),
    })
}

/// Every package file under a directory, for walking an extracted corpus.
pub fn list_loose_packages(root: &Path) -> Result<Vec<std::path::PathBuf>, String> {
    if !root.is_dir() {
        return Err(format!("{} is not a directory", root.display()));
    }
    Ok(walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .map(walkdir::DirEntry::into_path)
        .filter(|p| {
            matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("uasset") | Some("umap")
            )
        })
        .collect())
}

fn load_from_pak(pak_path: &str, entry: &str) -> Result<FSerializedAssetBundle, String> {
    let file = std::fs::File::open(pak_path).map_err(|e| format!("open {pak_path}: {e}"))?;
    let mut reader = std::io::BufReader::new(file);
    let pak = open_pak(Path::new(pak_path))?;

    let stem = strip_extension(entry);
    let mut read = |name: &str| -> Option<Vec<u8>> {
        let mut out = Vec::new();
        pak.read_file(name, &mut reader, &mut out).ok()?;
        Some(out)
    };

    let asset_file_buffer = read(entry)
        .or_else(|| read(&format!("{MOUNT_POINT}{entry}")))
        .ok_or_else(|| format!("{entry} is not in {pak_path}"))?;
    let exports_file_buffer = read(&format!("{stem}.uexp"))
        .or_else(|| read(&format!("{MOUNT_POINT}{stem}.uexp")))
        .unwrap_or_default();

    Ok(FSerializedAssetBundle {
        asset_file_buffer,
        exports_file_buffer,
        bulk_data_buffer: read(&format!("{stem}.ubulk")),
        optional_bulk_data_buffer: read(&format!("{stem}.uptnl")),
        memory_mapped_bulk_data_buffer: read(&format!("{stem}.m.ubulk")),
    })
}

fn load_from_utoc(
    game_root: &str,
    utoc_path: &str,
    entry: &str,
) -> Result<FSerializedAssetBundle, String> {
    let container_name = Path::new(utoc_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("invalid .utoc path")?;
    let paks_dir = crate::paths::paks_dir(game_root);
    let store = open_base_game_paks(&paks_dir, container_name)?;

    let target = store
        .child_containers()
        .find(|c| c.container_name() == container_name)
        .ok_or_else(|| format!("container not found: {container_name}"))?;

    let wanted = strip_extension(entry);
    let locate = |container: &dyn IoStoreTrait| {
        container.packages().find_map(|pkg| {
            let chunk = FIoChunkId::from_package_id(pkg.id(), 0, EIoChunkType::ExportBundleData);
            let path = store.chunk_path(chunk)?;
            let stripped = path.strip_prefix(MOUNT_POINT).unwrap_or(&path).to_string();
            (strip_extension(&stripped) == wanted).then_some((pkg.id(), stripped))
        })
    };
    // A mount path such as `/MarvelGAS/Marvel/X` names the package directly: its id is a hash of
    // the name, and the store knows which container holds it, whatever the mount point.
    let by_name = || {
        // A package id is the same lowercase UTF-16 CityHash as a container id, which is the one
        // retoc exposes.
        let id = retoc::FPackageId(retoc::FIoContainerId::from_name(entry).0);
        let chunk = FIoChunkId::from_package_id(id, 0, EIoChunkType::ExportBundleData);
        let path = store.chunk_path(chunk)?;
        Some((
            id,
            path.strip_prefix(MOUNT_POINT).unwrap_or(&path).to_string(),
        ))
    };
    // A package referenced from this container may live in another base pak, such as a Blueprint
    // class in the environment chunk that a map in the main chunk instances.
    let (package_id, package_path) = if entry.starts_with('/') {
        by_name()
    } else {
        locate(target).or_else(|| {
            store
                .child_containers()
                .filter(|c| c.container_name() != container_name)
                .find_map(locate)
        })
    }
    .ok_or_else(|| {
        format!("{entry} is not a package in {container_name} or the base paks beside it")
    })?;

    let log = retoc::logging::Log::no_log();
    let context = FZenPackageContext::create(
        &*store,
        Some(ENGINE_VERSION.package_file_version()),
        &log,
        None,
    );

    let writer = MemFileWriter::default();
    asset_conversion::build_legacy(&context, package_id, UEPath::new(&package_path), &writer)
        .map_err(|e| format!("convert {entry} to legacy: {e:#}"))?;
    writer.into_bundle(entry)
}

/// Package paths carry one extension; `.m.ubulk` is handled by the caller building it back on.
fn strip_extension(path: &str) -> String {
    match path.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() && !stem.ends_with('/') => stem.to_string(),
        _ => path.to_string(),
    }
}

/// Every file stored in a .pak, with the UE mount prefix stripped for display.
pub fn list_pak_entries(pak_path: &str) -> Result<Vec<String>, String> {
    let pak = open_pak(Path::new(pak_path))?;
    Ok(pak
        .files()
        .into_iter()
        .map(|path| path.strip_prefix(MOUNT_POINT).unwrap_or(&path).to_string())
        .collect())
}

/// One package in a container: its id and its mount-relative path.
pub type PackageEntry = (retoc::FPackageId, String);

/// Every package in a container, alongside the open store they were read from.
pub fn list_packages(
    game_root: &str,
    utoc_path: &str,
) -> Result<(Arc<dyn IoStoreTrait>, Vec<PackageEntry>), String> {
    let container_name = Path::new(utoc_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("invalid .utoc path")?;
    let paks_dir = crate::paths::paks_dir(game_root);
    let store = open_base_game_paks(&paks_dir, container_name)?;

    let packages = {
        let target = store
            .child_containers()
            .find(|c| c.container_name() == container_name)
            .ok_or_else(|| format!("container not found: {container_name}"))?;
        target
            .packages()
            .filter_map(|pkg| {
                let chunk =
                    FIoChunkId::from_package_id(pkg.id(), 0, EIoChunkType::ExportBundleData);
                let path = store.chunk_path(chunk)?;
                let stripped = path.strip_prefix(MOUNT_POINT).unwrap_or(&path).to_string();
                Some((pkg.id(), stripped))
            })
            .collect()
    };
    Ok((store, packages))
}

/// Converts one already-resolved package, for callers that hold a store open across many assets.
pub fn bundle_from_package(
    store: &dyn IoStoreTrait,
    package_id: retoc::FPackageId,
    path: &str,
) -> Result<FSerializedAssetBundle, String> {
    let log = retoc::logging::Log::no_log();
    let context = FZenPackageContext::create(
        store,
        Some(ENGINE_VERSION.package_file_version()),
        &log,
        None,
    );
    let writer = MemFileWriter::default();
    asset_conversion::build_legacy(&context, package_id, UEPath::new(path), &writer)
        .map_err(|e| format!("convert {path} to legacy: {e:#}"))?;
    writer.into_bundle(path)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn stripping_an_extension_leaves_directory_separators_alone() {
        assert_eq!(
            strip_extension("Marvel/Content/DT_Hero.uasset"),
            "Marvel/Content/DT_Hero"
        );
        assert_eq!(
            strip_extension("Marvel/Content/NoExtension"),
            "Marvel/Content/NoExtension"
        );
        assert_eq!(strip_extension("Some.Folder/File.umap"), "Some.Folder/File");
    }

    #[test]
    fn the_in_memory_writer_sorts_outputs_into_the_bundle_by_extension() {
        let writer = MemFileWriter::default();
        for (path, byte) in [("A/B.uasset", 1u8), ("A/B.uexp", 2), ("A/B.ubulk", 3)] {
            writer
                .write_file(path.into(), false, vec![byte])
                .expect("write");
        }
        let bundle = writer.into_bundle("A/B.uasset").expect("bundle");
        assert_eq!(bundle.asset_file_buffer, vec![1]);
        assert_eq!(bundle.exports_file_buffer, vec![2]);
        assert_eq!(bundle.bulk_data_buffer, Some(vec![3]));
    }

    #[test]
    fn a_conversion_that_produced_no_header_is_an_error_not_an_empty_bundle() {
        let writer = MemFileWriter::default();
        writer
            .write_file("A/B.uexp".into(), false, vec![2])
            .expect("write");
        assert!(writer.into_bundle("A/B.uasset").is_err());
    }
}

/// Set `RIVALS_GAME_ROOT` to a real install to run these. They are skipped otherwise because the
/// repo ships no game data, and they cover the pak read path that has no synthetic equivalent.
#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod game_data_tests {
    use super::*;
    use crate::pak::profile::RIVALS_PROFILE;

    const TABLE: &str =
        "Marvel/Content/Marvel/Data/DataTable/UI/Friends/DT_FriendsRecommendTag.uasset";

    fn game_root() -> Option<String> {
        std::env::var("RIVALS_GAME_ROOT").ok()
    }

    /// Written uncompressed so the test does not depend on the Oodle DLL being present.
    fn write_pak(path: &Path, files: &[(String, Vec<u8>)]) -> Result<(), String> {
        let out = std::fs::File::create(path).map_err(|e| e.to_string())?;
        let mut writer = repak::PakBuilder::new()
            .profile(RIVALS_PROFILE.repak_profile())
            .key(crate::pak::profile::RIVALS_PROFILE.make_aes_key()?)
            .compression(RIVALS_PROFILE.compression())
            .writer(
                std::io::BufWriter::new(out),
                RIVALS_PROFILE.pak_version(),
                RIVALS_PROFILE.mount_point().to_string(),
                None,
            );
        for (name, data) in files {
            writer
                .write_file(name, false, data.clone())
                .map_err(|e| e.to_string())?;
        }
        writer.write_index().map_err(|e| e.to_string())?;
        Ok(())
    }

    /// The desktop app can inspect an asset inside a legacy mod pak, a path the IoStore tests
    /// never touch. Round-tripping a real package through a pak proves the sibling `.uexp` is
    /// found and that the mount prefix is handled.
    #[test]
    fn an_asset_written_into_a_pak_reads_back_byte_for_byte() {
        let Some(root) = game_root() else {
            return;
        };
        let container = crate::paths::paks_dir(&root)
            .join("pakchunk0-Windows.utoc")
            .display()
            .to_string();
        let Ok(original) = load_bundle(&root, &container, TABLE, AssetSource::Utoc) else {
            return;
        };

        let dir = std::env::temp_dir().join(format!("rivals-pakroundtrip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create dir");
        let pak = dir.join("Test.pak");

        let stem = strip_extension(TABLE);
        write_pak(
            &pak,
            &[
                (TABLE.to_string(), original.asset_file_buffer.clone()),
                (format!("{stem}.uexp"), original.exports_file_buffer.clone()),
            ],
        )
        .expect("write pak");

        let reloaded = load_bundle(&root, &pak.display().to_string(), TABLE, AssetSource::Pak)
            .expect("read the asset back out of the pak");

        assert_eq!(reloaded.asset_file_buffer, original.asset_file_buffer);
        assert_eq!(
            reloaded.exports_file_buffer, original.exports_file_buffer,
            "the sibling .uexp has to come back too, or every export is empty"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn asking_a_pak_for_an_entry_it_does_not_hold_is_an_error_not_an_empty_bundle() {
        let Some(root) = game_root() else {
            return;
        };
        let dir = std::env::temp_dir().join(format!("rivals-pakmissing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create dir");
        let pak = dir.join("Empty.pak");
        write_pak(
            &pak,
            &[("Some/Other/File.uasset".to_string(), vec![1, 2, 3])],
        )
        .expect("write pak");

        let result = load_bundle(&root, &pak.display().to_string(), TABLE, AssetSource::Pak);

        assert!(result.is_err(), "a missing entry must not read as empty");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
