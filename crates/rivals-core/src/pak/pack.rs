//! Packs a folder of loose packages into an IoStore mod: the `.utoc` and `.ucas` the game loads
//! packages from, and the `.pak` beside them carrying anything else the folder holds.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use rayon::prelude::*;
use retoc::iostore_writer::IoStoreWriter;
use retoc::legacy_asset::FSerializedAssetBundle;
use retoc::version::EngineVersion;
use retoc::zen_asset_conversion::{self, ConvertedZenAssetBundle};
use retoc::{FSFileReader, FileReaderTrait, UEPath, UEPathBuf};
use serde::Serialize;

use super::iostore_out::{HeaderCarry, IoStoreOptions, PackageBytes};
use super::profile::{self, RIVALS_BLOCK_SIZE, RIVALS_MOUNT_POINT};
use super::shader_library::{self, GameLibraries};

const ENGINE: EngineVersion = EngineVersion::UE5_3;

/// Extensions that belong to a package bundle alongside its `.uasset`/`.umap`. `.m.ubulk` comes
/// before `.ubulk`, which it also ends with.
pub const COMPANION_EXTS: [&str; 4] = [".uexp", ".m.ubulk", ".ubulk", ".uptnl"];

/// The folders the game reads a mod's files from, under the mount point every mod has: the
/// project's and the engine's. The game's own containers hold nothing else.
pub const GAME_FOLDERS: [&str; 2] = ["Marvel", "Engine"];

/// Whether `name` is one of [`GAME_FOLDERS`], in any case, as Windows finds it.
pub fn is_game_folder(name: &str) -> bool {
    GAME_FOLDERS
        .iter()
        .any(|folder| folder.eq_ignore_ascii_case(name))
}

/// Whether the folder at `dir` holds one of [`GAME_FOLDERS`]: it is a mod's loose files, as a pak
/// unpacks.
pub fn holds_game_folder(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries.filter_map(Result::ok).any(|entry| {
            entry.file_type().is_ok_and(|kind| kind.is_dir())
                && is_game_folder(&entry.file_name().to_string_lossy())
        })
    })
}

/// Which of a folder's files a pack takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Take {
    /// Every file under it.
    #[default]
    Everything,
    /// Only the files under its [`GAME_FOLDERS`], so a readme or preview beside them, which the
    /// game would never read, stays out of the mod.
    GameFolders,
}

/// A folder of loose files sorted for packing: the packages a container takes, the package files
/// that belong to no package, and everything else, which rides in the pak beside the container.
pub(crate) struct LooseFolder {
    /// Each package's `.uasset` or `.umap`, as the `Marvel/Content/...` path a container names it
    /// by: one with its `.uexp` beside it.
    pub packages: Vec<String>,
    /// The package files no package takes, each saying what it lacks.
    pub strays: Vec<String>,
    /// The other files, by their path under the folder, but for the entries the game does not
    /// support in a pak, which an older packer may have left in a mod since unpacked.
    pub others: Vec<PathBuf>,
}

impl LooseFolder {
    /// Sorts the files under `dir`. The parts of a package are matched ignoring case, as Windows
    /// finds them.
    pub(crate) fn read(dir: &Path) -> Result<Self, String> {
        Self::read_taking(dir, Take::Everything)
    }

    /// Sorts the files under `dir` that `take` takes.
    pub(crate) fn read_taking(dir: &Path, take: Take) -> Result<Self, String> {
        if !dir.is_dir() {
            return Err(format!("Input is not a directory: {}", dir.display()));
        }
        #[derive(Default)]
        struct Parts {
            assets: Vec<String>,
            uexp: Option<String>,
            bulk: Vec<String>,
        }
        let mut stems: BTreeMap<String, Parts> = BTreeMap::new();
        let mut others = Vec::new();
        let files = walkdir::WalkDir::new(dir)
            .into_iter()
            .filter_entry(|entry| {
                take == Take::Everything
                    || entry.depth() != 1
                    || (entry.file_type().is_dir()
                        && is_game_folder(&entry.file_name().to_string_lossy()))
            })
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file());
        for file in files {
            let Ok(rel) = file.path().strip_prefix(dir) else {
                continue;
            };
            let path = rel.to_string_lossy().replace('\\', "/");
            let lower = path.to_ascii_lowercase();
            let stem_of = |ext: &str| lower.strip_suffix(ext).map(str::to_string);
            if let Some(stem) = stem_of(".uasset").or_else(|| stem_of(".umap")) {
                stems.entry(stem).or_default().assets.push(path);
            } else if let Some(stem) = stem_of(".uexp") {
                stems.entry(stem).or_default().uexp = Some(path);
            } else if let Some(stem) = COMPANION_EXTS[1..].iter().find_map(|ext| stem_of(ext)) {
                stems.entry(stem).or_default().bulk.push(path);
            } else if !super::plain::is_unsupported_entry(&path) {
                others.push(rel.to_path_buf());
            }
        }

        let mut packages = Vec::new();
        let mut strays = Vec::new();
        for parts in stems.into_values() {
            let whole = parts.assets.len() == 1 && parts.uexp.is_some();
            match (parts.assets.as_slice(), &parts.uexp) {
                ([asset], Some(_)) => packages.push(asset.clone()),
                ([], Some(uexp)) => {
                    strays.push(format!("{uexp} has no .uasset or .umap beside it"))
                }
                ([], None) => {}
                (assets, None) => strays.extend(
                    assets
                        .iter()
                        .map(|asset| format!("{asset} has no .uexp beside it")),
                ),
                (assets, Some(_)) => {
                    strays.push(format!("{} share one .uexp", assets.join(" and ")))
                }
            }
            if !whole {
                strays.extend(
                    parts
                        .bulk
                        .iter()
                        .map(|bulk| format!("{bulk} belongs to no package")),
                );
            }
        }
        packages.sort();
        others.sort();
        Ok(Self {
            packages,
            strays,
            others,
        })
    }

    /// Refuses a folder holding package files no package takes: the game loads a package whole
    /// from a container, so a part without the rest is always a slip, most often a missing
    /// `.uexp`.
    pub(crate) fn refuse_strays(&self) -> Result<(), String> {
        if self.strays.is_empty() {
            return Ok(());
        }
        let listed: Vec<String> = self.strays.iter().map(|s| format!("  - {s}")).collect();
        Err(format!(
            "{} file(s) belong to no package; add what each lacks or take it out:\n{}",
            self.strays.len(),
            listed.join("\n")
        ))
    }
}

/// Relative paths under `dir` that are not part of a `.uasset`/`.umap` bundle. A container cannot
/// hold them, so they ride in the pak beside it, all but the entries the game does not support
/// there.
pub fn non_package_files(dir: &Path) -> Vec<PathBuf> {
    LooseFolder::read(dir)
        .map(|folder| folder.others)
        .unwrap_or_default()
}

/// The files of the loose package at `path`, its `.uexp` and whatever bulk data sits beside it.
fn loose_bundle(reader: &FSFileReader, path: &UEPath) -> Result<FSerializedAssetBundle, String> {
    let read = |path: &UEPath| reader.read(path).map_err(|e| e.to_string());
    let read_opt = |path: &UEPath| reader.read_opt(path).map_err(|e| e.to_string());
    Ok(FSerializedAssetBundle {
        asset_file_buffer: read(path)?,
        exports_file_buffer: read(&path.with_extension("uexp"))?,
        bulk_data_buffer: read_opt(&path.with_extension("ubulk"))?,
        optional_bulk_data_buffer: read_opt(&path.with_extension("uptnl"))?,
        memory_mapped_bulk_data_buffer: read_opt(&path.with_extension("m.ubulk"))?,
    })
}

/// One of the packages a [`LooseFolder`] lists, read for a container writer to take.
pub(crate) fn read_loose_package(dir: &Path, entry: &str) -> Result<PackageBytes, String> {
    let bundle = loose_bundle(&FSFileReader::new(dir), UEPath::new(entry))
        .map_err(|e| format!("read {entry}: {e}"))?;
    Ok(PackageBytes {
        entry: entry.to_string(),
        asset: bundle.asset_file_buffer,
        exports: bundle.exports_file_buffer,
        bulk: bundle.bulk_data_buffer,
        optional_bulk: bundle.optional_bulk_data_buffer,
        memory_mapped_bulk: bundle.memory_mapped_bulk_data_buffer,
        shader_map_hashes: Vec::new(),
    })
}

#[derive(Debug, Serialize)]
pub struct PackReport {
    pub utoc: PathBuf,
    /// Packages put in the container.
    pub packages: usize,
    /// Files that are not packages, carried in the pak beside it.
    pub loose: Vec<String>,
    /// Shader archives written into the container as libraries.
    pub shader_libraries: Vec<String>,
    /// Shader archives left out, each with a warning saying why.
    pub left_out: Vec<String>,
    pub warnings: Vec<String>,
}

/// Deletes partial output on drop unless disarmed.
struct CleanupGuard<'a> {
    utoc: &'a Path,
    disarmed: bool,
}

impl Drop for CleanupGuard<'_> {
    fn drop(&mut self) {
        if !self.disarmed {
            for extension in ["utoc", "ucas", "pak"] {
                let _ = std::fs::remove_file(self.utoc.with_extension(extension));
            }
        }
    }
}

/// Packs the loose packages under `input`, as `Marvel/Content/...` paths, into a container at
/// `output_utoc`, with the pak beside it holding everything else the folder has, of the files
/// `take` takes. A folder with no package makes a container holding only its header, the pak
/// beside it carrying the files. `carry` restores
/// what the header of a container being rebuilt held, which the loose files cannot say. Shader
/// archives become the container's libraries, and their asset infos the shader maps its packages
/// list; `game_libraries` names the game's own, which an archive is kept from replacing.
///
/// The packages convert on `threads` threads of a pool of their own while this thread writes them
/// in as they arrive, compressing on the pool it is on: converters waiting for this thread to take
/// what they made would otherwise fill the pool it needs to get there. `cancel` stops it between
/// packages, and `progress` hears how far it got.
#[allow(clippy::too_many_arguments)]
pub fn pack_folder(
    input: &Path,
    take: Take,
    output_utoc: &Path,
    options: &IoStoreOptions,
    carry: &HeaderCarry,
    game_libraries: GameLibraries,
    threads: usize,
    cancel: &AtomicBool,
    progress: &dyn Fn(usize, usize),
) -> Result<PackReport, String> {
    let folder = LooseFolder::read_taking(input, take)?;
    folder.refuse_strays()?;
    if let Some(parent) = output_utoc.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let reader = FSFileReader::new(input);
    let asset_paths: Vec<UEPathBuf> = folder.packages.iter().cloned().map(Into::into).collect();
    if asset_paths.is_empty() && folder.others.is_empty() {
        return Err(format!(
            "Nothing to pack: {} holds no file",
            input.display()
        ));
    }

    let mut writer = IoStoreWriter::new(
        output_utoc,
        ENGINE.toc_version(),
        Some(ENGINE.container_header_version()),
        RIVALS_MOUNT_POINT.into(),
        Some(options.compression.retoc()),
    )
    .map_err(|e| e.to_string())?
    .with_compression_block_size(RIVALS_BLOCK_SIZE);
    if let Some(level) = options.oodle_level {
        writer = writer.with_compression_level(level);
    }
    if options.obfuscate.unwrap_or(false) {
        writer = writer.with_encryption(profile::obfuscation_key()?);
    }

    let mut guard = CleanupGuard {
        utoc: output_utoc,
        disarmed: false,
    };
    carry.replay(&mut writer)?;

    // Written first, so the shader maps their asset infos name are there for the converters.
    let libraries = shader_library::pack(&mut writer, input, &folder.others, game_libraries)?;
    let for_pak: Vec<&PathBuf> = folder
        .others
        .iter()
        .filter(|rel| !libraries.taken.contains(*rel))
        .collect();
    // A container holding no package is still a mod, as the game's own translations container is:
    // its files ride in the pak beside it. One that would hold nothing at all is no mod.
    if asset_paths.is_empty() && libraries.written.is_empty() && for_pak.is_empty() {
        return Err(format!(
            "Nothing to pack: the folder holds no package and no file for the pak, and no shader \
             archive it holds makes a library: {}",
            libraries.warnings.join("; ")
        ));
    }
    let shader_maps = &libraries.shader_maps;
    let total = asset_paths.len();

    let converters = crate::parallel::own_pool(threads)?;
    let (tx, rx) =
        std::sync::mpsc::sync_channel::<Result<ConvertedZenAssetBundle, String>>(threads.max(2));

    std::thread::scope(|scope| -> Result<(), String> {
        let asset_paths_ref = &asset_paths;
        let shader_maps_ref = shader_maps;
        let reader_ref = &reader;
        let converters = &converters;

        scope.spawn(move || {
            let log = retoc::logging::Log::no_log();
            let convert = || {
                asset_paths_ref
                    .par_iter()
                    .try_for_each(|path| -> Result<(), ()> {
                        if cancel.load(Ordering::Relaxed) {
                            let _ = tx.send(Err("Repack cancelled".to_string()));
                            return Err(());
                        }
                        let ue_path: &UEPath = path.as_ref();
                        let bundle = match loose_bundle(reader_ref, ue_path) {
                            Ok(bundle) => bundle,
                            Err(e) => {
                                let _ = tx.send(Err(e));
                                return Err(());
                            }
                        };
                        let mounted_path: UEPathBuf =
                            format!("{RIVALS_MOUNT_POINT}{ue_path}").into();
                        let converted = zen_asset_conversion::build_zen_asset(
                            bundle,
                            shader_maps_ref,
                            &mounted_path,
                            Some(ENGINE.package_file_version()),
                            ENGINE.container_header_version(),
                            false,
                            None,
                            None,
                            &log,
                        )
                        .map_err(|e| format!("Failed to convert {ue_path}: {e}"));
                        let failed = converted.is_err();
                        if tx.send(converted).is_err() || failed {
                            return Err(());
                        }
                        Ok(())
                    })
            };
            // Entered from a thread of its own, which waits without running anything else.
            let _ = converters.install(convert);
        });

        // Owned here, so returning early lets the converters' sends fail rather than wait.
        let rx = rx;
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
                progress(completed, total);
            }
        }
        Ok(())
    })?;

    if cancel.load(Ordering::Relaxed) {
        return Err("Repack cancelled".to_string());
    }
    writer.finalize().map_err(|e| e.to_string())?;

    let mut pak_files = Vec::new();
    let mut loose = Vec::new();
    for rel in for_pak {
        let bytes =
            std::fs::read(input.join(rel)).map_err(|e| format!("read {}: {e}", rel.display()))?;
        let rel = rel.to_string_lossy().replace('\\', "/");
        loose.push(rel.clone());
        pak_files.push((rel, bytes));
    }
    super::plain::write_files(
        &output_utoc.with_extension("pak"),
        pak_files,
        &options.pak(),
    )?;

    guard.disarmed = true;
    Ok(PackReport {
        utoc: output_utoc.to_path_buf(),
        packages: total,
        loose,
        shader_libraries: libraries.written,
        left_out: libraries.left_out,
        warnings: libraries.warnings,
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::pak::containers::open_utoc;
    use crate::pak::iostore_out::{PackageBytes, write_batch_into_iostore};
    use crate::pak::profile::PackCompression;
    use crate::pak::shader_library::fixture::*;
    use retoc::{EIoChunkType, FIoChunkId};

    fn scratch(tag: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rivals-pack-{tag}-{stamp}"));
        std::fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    fn put(root: &Path, rel: &str) {
        put_bytes(root, rel, rel.as_bytes());
    }

    fn put_bytes(root: &Path, rel: &str, bytes: &[u8]) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    /// The files folders leave stay out of what a folder packs, at any depth.
    #[test]
    fn the_files_folders_leave_are_not_packed() {
        let root = scratch("system-files");
        put(&root, "Marvel/Config/DefaultGame.ini");
        put(&root, "Marvel/Config/desktop.ini");
        put(&root, "Marvel/Content/Thumbs.db");
        put(&root, "desktop.ini");
        assert_eq!(
            non_package_files(&root),
            [PathBuf::from("Marvel/Config/DefaultGame.ini")]
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// A cooked shader archive becomes a library in the container, even with no package beside
    /// it, and extracts back to the same shader maps, shaders and code. One named like a library
    /// the game ships, and one that is not cooked, are left out with a word each, and none of
    /// them rides in the pak.
    #[test]
    fn a_shader_archive_packs_into_a_library_and_extracts_back() {
        let root = scratch("shaders");
        let code: Vec<u8> = (0..200u8).collect();
        let ours = "Marvel/Content/ShaderArchive-ToolkitTest-PCD3D_SM6.ushaderbytecode";
        let game = "Marvel/Content/ShaderArchive-Global-PCD3D_SM6-PCD3D_SM6.ushaderbytecode";
        let old = "Marvel/Content/ShaderArchive-Old-PCD3D_SM6.ushaderbytecode";
        put_bytes(&root, ours, &cooked_archive(2, &code));
        put_bytes(&root, game, &cooked_archive(2, &code));
        put_bytes(&root, old, &cooked_archive(1, &code));
        let info = format!(
            r#"{{"ShaderCodeToAssets":[{{"ShaderMapHash":"{}","Assets":["/Game/Marvel/Test"]}}]}}"#,
            "07".repeat(20)
        );
        put_bytes(
            &root,
            "Marvel/Content/ShaderAssetInfo-ToolkitTest-PCD3D_SM6.assetinfo.json",
            info.as_bytes(),
        );

        let out = scratch("shaders-out");
        let packed = out.join("Shaders_P.utoc");
        let report = pack_folder(
            &root,
            Take::Everything,
            &packed,
            &IoStoreOptions {
                compression: PackCompression::Zlib,
                ..Default::default()
            },
            &HeaderCarry::default(),
            &|| Ok(["global-pcd3d_sm6-pcd3d_sm6".to_string()].into()),
            1,
            &AtomicBool::new(false),
            &|_, _| {},
        )
        .expect("pack");
        assert_eq!(report.packages, 0);
        assert_eq!(report.shader_libraries, [ours]);
        assert_eq!(report.left_out, [game, old]);
        assert!(report.warnings[0].contains("the game's own Global-PCD3D_SM6-PCD3D_SM6"));
        assert!(report.warnings[1].contains("(version 1)"));
        assert!(report.loose.is_empty(), "{:?}", report.loose);
        assert!(
            crate::pak::plain::list_files(&packed.with_extension("pak"))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            crate::pak::containers::list_utoc_paths(&packed).unwrap(),
            [ours]
        );

        let store = open_utoc(&packed.to_string_lossy()).expect("open the packed");
        let back = out.join("back");
        let extracted = shader_library::extract(&*store, &back, &|_| true).expect("extract");
        assert!(extracted.errors.is_empty(), "{:?}", extracted.errors);
        assert_eq!(extracted.written, [ours]);
        let (maps, shaders, rebuilt) = read_archive(&std::fs::read(back.join(ours)).unwrap());
        assert_eq!((maps, shaders), (vec![MAP_HASH], vec![SHADER_HASH]));
        assert_eq!(rebuilt, code);
        assert!(
            back.join("Marvel/Content/ShaderAssetInfo-ToolkitTest-PCD3D_SM6.assetinfo.json")
                .is_file()
        );
        drop(store);
        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&out).ok();
    }

    /// Each part of a package without the rest of it is named with what it lacks, and the parts
    /// of a whole package match whatever their case.
    #[test]
    fn package_files_without_their_package_are_strays() {
        let root = scratch("strays");
        for rel in [
            "Marvel/Content/Whole.uasset",
            "Marvel/Content/Whole.UEXP",
            "Marvel/Content/Whole.ubulk",
            "Marvel/Content/Maps/Level.umap",
            "Marvel/Content/Maps/Level.uexp",
            "Marvel/Content/NoExports.uasset",
            "Marvel/Content/NoExports.ubulk",
            "Marvel/Content/NoHeader.uexp",
            "Marvel/Content/Bulk.m.ubulk",
            "Marvel/Content/Optional.uptnl",
            "Marvel/Config/Mod.ini",
            "chunknames",
        ] {
            put(&root, rel);
        }
        let folder = LooseFolder::read(&root).unwrap();
        assert_eq!(
            folder.packages,
            [
                "Marvel/Content/Maps/Level.umap",
                "Marvel/Content/Whole.uasset"
            ]
        );
        assert_eq!(folder.others, [PathBuf::from("Marvel/Config/Mod.ini")]);
        assert_eq!(
            folder.strays,
            [
                "Marvel/Content/Bulk.m.ubulk belongs to no package",
                "Marvel/Content/NoExports.uasset has no .uexp beside it",
                "Marvel/Content/NoExports.ubulk belongs to no package",
                "Marvel/Content/NoHeader.uexp has no .uasset or .umap beside it",
                "Marvel/Content/Optional.uptnl belongs to no package",
            ]
        );
        let refused = folder.refuse_strays().unwrap_err();
        assert!(
            refused.starts_with("5 file(s) belong to no package"),
            "{refused}"
        );

        let packed = root.with_extension("out").join("Strays_P.utoc");
        let err = pack_folder(
            &root,
            Take::Everything,
            &packed,
            &IoStoreOptions::default(),
            &HeaderCarry::default(),
            &|| Ok(Default::default()),
            1,
            &AtomicBool::new(false),
            &|_, _| {},
        )
        .unwrap_err();
        assert_eq!(err, refused);
        assert!(!packed.exists(), "nothing is written");
        std::fs::remove_dir_all(&root).ok();
    }

    /// A container extracted to loose files and packed again holds the same packages, byte for
    /// byte, and the pak beside it carries the folder's other files.
    #[test]
    fn an_extracted_container_packs_back_to_the_same_packages() {
        let Ok(root) = std::env::var("RIVALS_GAME_ROOT") else {
            return;
        };
        if std::env::var_os("OODLE_LIB_PATH").is_none() {
            return;
        }
        let source = format!(
            "{}/MarvelGame/Marvel/Content/Paks/pakchunk0-Windows.utoc",
            root.replace('\\', "/")
        );
        let entries = [
            "Marvel/Content/Marvel/AbilitySystem/1011/101111/CameraShake_101111.uasset",
            "Marvel/Content/Marvel/AbilitySystem/1011/101111/CameraShake_101111_Hit.uasset",
            "Marvel/Content/Marvel/Data/DataTable/MarvelHeroTable.uasset",
        ];
        let dir = scratch("roundtrip");
        let original = dir.join("Original_P.utoc");
        let options = IoStoreOptions {
            compression: PackCompression::Zlib,
            ..Default::default()
        };
        write_batch_into_iostore(
            &original,
            entries.len(),
            |index| {
                let loaded = crate::asset::load_bundle(
                    &root,
                    &source,
                    entries[index],
                    crate::asset::AssetSource::Utoc,
                )
                .ok()?;
                Some(PackageBytes {
                    entry: entries[index].to_string(),
                    asset: loaded.asset_file_buffer,
                    exports: loaded.exports_file_buffer,
                    bulk: loaded.bulk_data_buffer,
                    optional_bulk: None,
                    memory_mapped_bulk: None,
                    shader_map_hashes: Vec::new(),
                })
            },
            &options,
        )
        .expect("write the original");

        let loose = dir.join("loose");
        let extracted = crate::pak::extract::extract_legacy(
            &original,
            &root,
            &loose,
            &[],
            &AtomicBool::new(false),
            &|_, _| {},
        )
        .expect("extract");
        assert!(extracted.errors.is_empty(), "{:?}", extracted.errors);
        std::fs::write(loose.join("readme.txt"), b"carried").unwrap();
        // A shader archive whose asset info says the hero table uses its shader map.
        put_bytes(
            &loose,
            "Marvel/Content/ShaderArchive-ToolkitTest-PCD3D_SM6.ushaderbytecode",
            &cooked_archive(2, &[1, 2, 3, 4]),
        );
        let info = format!(
            r#"{{"ShaderCodeToAssets":[{{"ShaderMapHash":"{}","Assets":["/Game/Marvel/Data/DataTable/MarvelHeroTable"]}}]}}"#,
            "07".repeat(20)
        );
        put_bytes(
            &loose,
            "Marvel/Content/ShaderAssetInfo-ToolkitTest-PCD3D_SM6.assetinfo.json",
            info.as_bytes(),
        );

        let packed = dir.join("Packed_P.utoc");
        let report = pack_folder(
            &loose,
            Take::Everything,
            &packed,
            &options,
            &HeaderCarry::default(),
            &|| Ok(Default::default()),
            rayon::current_num_threads(),
            &AtomicBool::new(false),
            &|_, _| {},
        )
        .expect("pack");
        assert_eq!(report.packages, entries.len());
        assert_eq!(report.loose, vec!["readme.txt".to_string()]);

        // Packed from the only thread of a pool, which the writing needs while the converters
        // wait for it to take what they made.
        let packed_alone = dir.join("PackedAlone_P.utoc");
        crate::parallel::own_pool(1)
            .unwrap()
            .install(|| {
                pack_folder(
                    &loose,
                    Take::Everything,
                    &packed_alone,
                    &options,
                    &HeaderCarry::default(),
                    &|| Ok(Default::default()),
                    1,
                    &AtomicBool::new(false),
                    &|_, _| {},
                )
            })
            .expect("pack from a pool's only thread");

        let before = open_utoc(&original.to_string_lossy()).expect("open the original");
        let ids = |store: &dyn retoc::iostore::IoStoreTrait| {
            let mut ids: Vec<_> = store.packages().map(|p| p.id()).collect();
            ids.sort_by_key(|id| id.0);
            ids
        };
        let table = retoc::FPackageId::from_name("/Game/Marvel/Data/DataTable/MarvelHeroTable");
        for packed in [&packed, &packed_alone] {
            let after = open_utoc(&packed.to_string_lossy()).expect("open the packed");
            assert_eq!(ids(&*before), ids(&*after), "the same packages");
            for id in ids(&*after) {
                let maps = after.package_store_entry(id).unwrap().shader_map_hashes;
                let expected = if id == table { 1 } else { 0 };
                assert_eq!(
                    maps.len(),
                    expected,
                    "{:?}",
                    after.chunk_path(FIoChunkId::from_package_id(
                        id,
                        0,
                        EIoChunkType::ExportBundleData
                    ))
                );
            }
            for id in ids(&*before) {
                let chunk = FIoChunkId::from_package_id(id, 0, EIoChunkType::ExportBundleData);
                assert_eq!(
                    before.read(chunk).expect("read before"),
                    after.read(chunk).expect("read after"),
                    "{:?} has the same bytes",
                    before.chunk_path(chunk)
                );
            }
        }
        drop(before);
        std::fs::remove_dir_all(&dir).ok();
    }
}
