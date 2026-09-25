//! Produces the legacy .uasset and .uexp bytes for one asset without writing anything to disk.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex};

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
            if is_package_path(&lowered) {
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
        .filter(|p| is_package_path(&p.to_string_lossy()))
        .collect())
}

/// Whether a path names a package's header file, rather than one of its sidecars or a file that
/// is no package at all.
pub fn is_package_path(path: &str) -> bool {
    let lowered = path.to_ascii_lowercase();
    lowered.ends_with(".uasset") || lowered.ends_with(".umap")
}

/// The packages a `.pak`, a folder of extracted files or a single loose file holds: a pak's by
/// their mount-relative path, anything on disk by its own path.
pub fn list_package_entries(source: &str) -> Result<Vec<String>, String> {
    let path = Path::new(source);
    if path.is_dir() {
        return Ok(list_loose_packages(path)?
            .into_iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect());
    }
    if is_package_path(source) {
        return Ok(vec![source.to_string()]);
    }
    Ok(list_pak_entries(source)?
        .into_iter()
        .filter(|entry| is_package_path(entry))
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
        let id = package_id(entry);
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
        format!("{entry} {NOT_A_PACKAGE} {container_name} or the base paks beside it")
    })?;

    PackageConverter::new(&*store).convert(package_id, &package_path)
}

/// Package paths carry one extension; `.m.ubulk` is handled by the caller building it back on.
fn strip_extension(path: &str) -> String {
    match path.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() && !stem.ends_with('/') => stem.to_string(),
        _ => path.to_string(),
    }
}

/// The mount-relative path a package ships under in the game, such as
/// `Marvel/Content/Marvel/Data/X.uasset`, which is what a mod has to name to override it.
///
/// A loose file only knows where it sits on disk, so its header's package name is looked up in the
/// base game first, which also places plugin content. A package the game does not ship falls back
/// to the project mount, then to the part of `disk_path` from the `Marvel/` folder down.
pub fn game_entry(game_root: &str, package_name: &str, disk_path: &Path) -> Result<String, String> {
    let store = open_base_game_paks(&crate::paths::paks_dir(game_root), "").ok();
    if let Some(path) = store.and_then(|store| package_path(&*store, package_name)) {
        return Ok(path);
    }
    let extension = disk_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("uasset");
    if let Some(relative) = mount_relative(package_name) {
        return Ok(format!("{relative}.{extension}"));
    }
    let normalised = disk_path.to_string_lossy().replace('\\', "/");
    ["Marvel/Content/", "Marvel/Plugins/"]
        .iter()
        .filter_map(|root| normalised.rfind(root))
        .max()
        .map(|at| normalised[at..].to_string())
        .ok_or_else(|| {
            format!(
                "{} names package {package_name}, which the game does not ship and which sits under no Marvel folder, so there is no game path to save it under",
                disk_path.display()
            )
        })
}

/// `entry` as a path inside a container, refused when it could reach outside one: a drive, a root
/// or a `..` would make joining it onto a folder land somewhere else on disk.
pub fn contained_entry(entry: &str) -> Result<String, String> {
    let normalised = entry.replace('\\', "/");
    let relative = normalised
        .strip_prefix(MOUNT_POINT)
        .unwrap_or(&normalised)
        .to_string();
    let escapes = relative.starts_with('/')
        || relative.contains(':')
        || relative.split('/').any(|part| part == "..");
    if escapes || relative.is_empty() {
        return Err(format!(
            "{entry} is not a path inside a container, so it cannot be written into a mod"
        ));
    }
    Ok(relative)
}

/// A package id is the same lowercase UTF-16 CityHash as a container id, which is the one retoc
/// exposes.
fn package_id(package_name: &str) -> retoc::FPackageId {
    retoc::FPackageId(retoc::FIoContainerId::from_name(package_name).0)
}

/// Where `store` holds a package, looked up by name, as a mount-relative path.
fn package_path(store: &dyn IoStoreTrait, package_name: &str) -> Option<String> {
    let chunk =
        FIoChunkId::from_package_id(package_id(package_name), 0, EIoChunkType::ExportBundleData);
    let path = store.chunk_path(chunk)?;
    Some(path.strip_prefix(MOUNT_POINT).unwrap_or(&path).to_string())
}

/// The shader maps a package's store entry lists, which live in the container header rather than
/// the package, so a package read out of its container leaves them behind. Looked up in the store
/// opened around `container`, or the base game's when the package came from anywhere else.
pub fn shader_map_hashes(
    game_root: &str,
    container: Option<&str>,
    package_name: &str,
) -> Vec<retoc::FSHAHash> {
    let open_as = container
        .and_then(|path| Path::new(path).file_stem())
        .and_then(|stem| stem.to_str())
        .unwrap_or_default();
    open_base_game_paks(&crate::paths::paks_dir(game_root), open_as)
        .ok()
        .and_then(|store| store.package_store_entry(package_id(package_name)))
        .map(|entry| entry.shader_map_hashes)
        .unwrap_or_default()
}

/// `/Game` is the project content directory and `/Engine` the engine's, matching how container
/// entries are named.
pub(crate) fn mount_relative(package: &str) -> Option<String> {
    for (prefix, root) in [
        ("/Game/", "Marvel/Content/"),
        ("/Engine/", "Engine/Content/"),
    ] {
        if let Some(rest) = package.strip_prefix(prefix) {
            return Some(format!("{root}{rest}"));
        }
    }
    None
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

/// The marker in the error `load_bundle` returns for an entry no container holds, so a caller can
/// tell an asset the game no longer ships from a decode failure.
pub const NOT_A_PACKAGE: &str = "is not a package in";

/// One package in a container: its id and its mount-relative path.
pub type PackageEntry = (retoc::FPackageId, String);

/// Every package in a container, alongside the open store they were read from.
pub fn list_packages(
    game_root: &str,
    utoc_path: &str,
) -> Result<(Arc<dyn IoStoreTrait>, Vec<PackageEntry>), String> {
    list_packages_via(game_root, utoc_path, utoc_path)
}

/// The same, with the store opened around `open_as` rather than around the container being listed.
///
/// A store admits its target plus the base game and no other mod, and resolves a chunk through the
/// highest-priority container holding it. Opening around a mod therefore reads that mod's copy of
/// anything it carries and the base game's copy of everything else, which is what lets a run build
/// on a mod without a second store to pay for or to keep in step.
pub fn list_packages_via(
    game_root: &str,
    open_as: &str,
    utoc_path: &str,
) -> Result<(Arc<dyn IoStoreTrait>, Vec<PackageEntry>), String> {
    let name_of = |path: &str| {
        Path::new(path)
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_string)
            .ok_or_else(|| "invalid .utoc path".to_string())
    };
    let container_name = name_of(utoc_path)?;
    let paks_dir = crate::paths::paks_dir(game_root);
    let store = open_base_game_paks(&paks_dir, &name_of(open_as)?)?;

    let packages = {
        let target = store
            .child_containers()
            .find(|c| c.container_name() == container_name)
            .ok_or_else(|| format!("container not found: {container_name}"))?;
        let target: &dyn IoStoreTrait = target;
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

/// Conversion has nothing to say, and a context borrows its log for as long as it lives.
static LOG: LazyLock<retoc::logging::Log> = LazyLock::new(retoc::logging::Log::no_log);

/// The conversion caches for one store, held across as many packages as the caller reads.
///
/// A context fills with the store's script object table and the header of every package an import
/// resolves through. Building one per package costs about forty times the package itself, so any
/// loop over packages should make one of these and keep it.
pub struct PackageConverter<'a> {
    context: FZenPackageContext<'a>,
}

impl<'a> PackageConverter<'a> {
    pub fn new(store: &'a dyn IoStoreTrait) -> Self {
        Self {
            context: FZenPackageContext::create(
                store,
                Some(ENGINE_VERSION.package_file_version()),
                &LOG,
                None,
            )
            .with_extra_script_objects(crate::script_objects::current()),
        }
    }

    /// Converts one already-resolved package back to its legacy form.
    pub fn convert(
        &self,
        package_id: retoc::FPackageId,
        path: &str,
    ) -> Result<FSerializedAssetBundle, String> {
        let writer = MemFileWriter::default();
        asset_conversion::build_legacy(&self.context, package_id, UEPath::new(path), &writer)
            .map_err(|e| format!("convert {path} to legacy: {e:#}"))?;
        writer.into_bundle(path)
    }
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

    /// A package's sidecars and a loose INI sit beside it, and none of them is a package to open.
    #[test]
    fn only_package_headers_are_listed_from_a_folder_or_a_pak() {
        let dir = std::env::temp_dir().join(format!("rivals-list-{}", std::process::id()));
        let files = [
            "Marvel/Content/A.uasset",
            "Marvel/Content/A.uexp",
            "Marvel/Content/A.ubulk",
            "Marvel/Config/Mod.ini",
        ];
        for file in files {
            let path = dir.join(file);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
            std::fs::write(path, b"x").expect("write");
        }
        let listed = list_package_entries(&dir.to_string_lossy()).expect("listed");
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert!(
            listed[0]
                .replace('\\', "/")
                .ends_with("Marvel/Content/A.uasset")
        );

        if std::env::var_os("OODLE_LIB_PATH").is_some() {
            let pak = dir.join("Test.pak");
            crate::pak_tweaks::io::create_empty_pak(&pak).expect("pak");
            crate::pak_tweaks::io::with_unpacked_pak(&pak, |unpacked| {
                for file in files {
                    let path = unpacked.join(file);
                    std::fs::create_dir_all(path.parent().expect("parent"))
                        .map_err(|e| e.to_string())?;
                    std::fs::write(path, b"x").map_err(|e| e.to_string())?;
                }
                Ok(())
            })
            .expect("fill the pak");
            assert_eq!(
                list_package_entries(&pak.to_string_lossy()).expect("listed"),
                vec!["Marvel/Content/A.uasset"]
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
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
