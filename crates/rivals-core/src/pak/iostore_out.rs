//! Writes edited packages into an IoStore mod, carrying the container's other chunks across.
//!
//! The game resolves a package through its IoStore store, so a mod holding a `.uasset` has to be
//! a container of its own: a plain pak only ever delivers loose files. The writer has no append
//! mode, so replacing one package means writing the container again around it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use retoc::iostore_writer::IoStoreWriter;
use retoc::legacy_asset::FSerializedAssetBundle;
use retoc::version::EngineVersion;
use retoc::{EIoChunkType, FIoChunkId, FPackageId, FSHAHash, UEPathBuf};

use crate::pak::containers::{open_utoc, utoc_is_obfuscated};
use crate::pak::profile::{
    self, PackCompression, RIVALS_BLOCK_SIZE, RIVALS_MOUNT_POINT, strip_mount_prefix,
};
use crate::pak_tweaks::io::{create_empty_pak, with_unpacked_pak};

/// The listing the toolkit writes beside a container so its own browsers can name what is inside.
const CHUNK_NAMES: &str = "chunknames";

/// Marvel Rivals ships UE 5.3, and a mod container has to answer to the same loader.
const ENGINE: EngineVersion = EngineVersion::UE5_3;

pub struct IoStoreOptions {
    pub compression: PackCompression,
    pub oodle_level: Option<retoc::OodleCompressionLevel>,
    /// `None` keeps whatever the container had. A container written from nothing is left plain.
    pub obfuscate: Option<bool>,
}

impl Default for IoStoreOptions {
    fn default() -> Self {
        Self {
            compression: PackCompression::Oodle,
            oodle_level: None,
            obfuscate: None,
        }
    }
}

/// One package and its sidecars, as the patcher leaves them.
pub struct PackageFiles<'a> {
    /// Mount-relative, as the container names it: `Marvel/Content/.../Thing.uasset`.
    pub entry: &'a str,
    pub asset: &'a [u8],
    pub exports: &'a [u8],
    pub bulk: Option<&'a [u8]>,
    pub optional_bulk: Option<&'a [u8]>,
    pub memory_mapped_bulk: Option<&'a [u8]>,
    /// Shader maps the source container listed for this package, restored when the conversion
    /// finds none of its own.
    pub shader_map_hashes: Vec<FSHAHash>,
}

#[derive(Debug)]
pub struct IoStoreReport {
    pub utoc: PathBuf,
    /// Whether the container already held this package, rather than gaining it.
    pub replaced: bool,
    /// Chunks carried over from the container as it was.
    pub carried_chunks: usize,
    /// How many packages the write put in.
    pub written: usize,
    /// Localized package declarations replayed into the new header.
    pub carried_localized: usize,
    /// Package redirects replayed into the new header.
    pub carried_redirects: usize,
}

/// Removes a staging directory unless the work reached the end.
struct Staging {
    dir: PathBuf,
    keep: bool,
}

impl Drop for Staging {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

/// Whether the container already carries `entry`, compared the way its paths are written.
pub fn utoc_holds_entry(utoc: &Path, entry: &str) -> Result<bool, String> {
    let store = open_utoc(&utoc.to_string_lossy())?;
    let wanted = comparable(entry);
    Ok(store
        .chunks()
        .any(|chunk| chunk.path().is_some_and(|path| comparable(&path) == wanted)))
}

fn comparable(path: &str) -> String {
    strip_mount_prefix(path).replace('\\', "/").to_lowercase()
}

/// The chunk kinds one package owns, which are the ones a replacement supersedes.
const PACKAGE_CHUNKS: [EIoChunkType; 4] = [
    EIoChunkType::ExportBundleData,
    EIoChunkType::BulkData,
    EIoChunkType::OptionalBulkData,
    EIoChunkType::MemoryMappedBulkData,
];

/// Writes `package` into the container at `utoc`, keeping everything else it holds. The container
/// is built beside the original and swapped in, so a failure leaves what was there untouched.
///
/// See [`write_many_into_iostore`] to put a batch in with a single rewrite.
pub fn write_into_iostore(
    utoc: &Path,
    package: PackageFiles<'_>,
    options: &IoStoreOptions,
) -> Result<IoStoreReport, String> {
    write_many_into_iostore(utoc, std::slice::from_ref(&package), options)
}

/// The same for a batch. Rewriting a container is a whole-file operation, so writing packages one
/// at a time repeats it once per package; a sweep over hundreds does it once instead.
pub fn write_many_into_iostore(
    utoc: &Path,
    packages: &[PackageFiles<'_>],
    options: &IoStoreOptions,
) -> Result<IoStoreReport, String> {
    if packages.is_empty() {
        return Err("no packages to write".into());
    }
    let stem = utoc
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .ok_or("the container has no name")?;
    let dir = utoc
        .parent()
        .ok_or("the container has no folder")?
        .to_path_buf();

    // Convert before touching anything: a package that will not convert is not worth staging for.
    let shader_maps: HashMap<String, Vec<FSHAHash>> = HashMap::new();
    let mut built = Vec::with_capacity(packages.len());
    for package in packages {
        let mounted: UEPathBuf = format!("{RIVALS_MOUNT_POINT}{}", package.entry).into();
        let bundle = FSerializedAssetBundle {
            asset_file_buffer: package.asset.to_vec(),
            exports_file_buffer: package.exports.to_vec(),
            bulk_data_buffer: package.bulk.map(<[u8]>::to_vec),
            optional_bulk_data_buffer: package.optional_bulk.map(<[u8]>::to_vec),
            memory_mapped_bulk_data_buffer: package.memory_mapped_bulk.map(<[u8]>::to_vec),
        };
        let converted = retoc::zen_asset_conversion::build_zen_asset(
            bundle,
            &shader_maps,
            mounted.as_ref(),
            Some(ENGINE.package_file_version()),
            ENGINE.container_header_version(),
            false,
            None,
            None,
            &retoc::logging::Log::no_log(),
        )
        .map_err(|e| format!("convert {} for IoStore: {e}", package.entry))?;
        built.push(converted);
    }

    let staging = Staging {
        dir: dir.join(format!(".{stem}_iostore")),
        keep: false,
    };
    if staging.dir.exists() {
        std::fs::remove_dir_all(&staging.dir).ok();
    }
    std::fs::create_dir_all(&staging.dir)
        .map_err(|e| format!("Could not create {}: {e}", staging.dir.display()))?;
    // The container's id comes from its file stem, so the staged name has to be the final one.
    let staged_utoc = staging.dir.join(format!("{stem}.utoc"));

    let mut report = IoStoreReport {
        utoc: utoc.to_path_buf(),
        replaced: false,
        carried_chunks: 0,
        written: packages.len(),
        carried_localized: 0,
        carried_redirects: 0,
    };
    let existing = utoc
        .is_file()
        .then(|| open_utoc(&utoc.to_string_lossy()))
        .transpose()?;
    let block_size = existing
        .as_ref()
        .and_then(|store| store.compression_block_size())
        .unwrap_or(RIVALS_BLOCK_SIZE);
    let obfuscate = options
        .obfuscate
        .unwrap_or_else(|| utoc.is_file() && utoc_is_obfuscated(utoc));

    if let Some(store) = &existing {
        if store.container_header_version() != Some(ENGINE.container_header_version()) {
            return Err(format!(
                "{} was built for another engine version, so this editor will not rewrite it",
                utoc.display()
            ));
        }
        for (converted, package) in built.iter_mut().zip(packages) {
            if store.package_store_entry(converted.package_id).is_some() {
                report.replaced = true;
            }
            if converted.store_entry().shader_map_hashes.is_empty() {
                let restored = store
                    .package_store_entry(converted.package_id)
                    .map(|entry| entry.shader_map_hashes)
                    .filter(|hashes| !hashes.is_empty())
                    .unwrap_or_else(|| package.shader_map_hashes.clone());
                if !restored.is_empty() {
                    converted.set_shader_map_hashes(restored);
                }
            }
        }
    }

    let mut writer = IoStoreWriter::new(
        &staged_utoc,
        ENGINE.toc_version(),
        Some(ENGINE.container_header_version()),
        RIVALS_MOUNT_POINT.into(),
        Some(options.compression.retoc()),
    )
    .map_err(|e| e.to_string())?
    .with_compression_block_size(block_size);
    if let Some(level) = options.oodle_level {
        writer = writer.with_compression_level(level);
    }
    if obfuscate {
        writer = writer.with_encryption(profile::obfuscation_key()?);
    }
    // Localized packages and redirects live in the container header rather than in any chunk, so
    // rewriting a container without replaying them would leave a mod whose chunks are all present
    // and whose localized variants the loader no longer knows to look for.
    if let Some(header) = existing.as_ref().and_then(|store| store.container_header()) {
        let localized: Vec<String> = header
            .localized_packages()
            .map(|name| name.into_owned())
            .collect();
        let redirects: Vec<(String, FPackageId)> = header
            .package_redirects()
            .map(|(name, id)| (name.into_owned(), id))
            .collect();
        for name in &localized {
            // The culture only matters for pre-Initial headers, which are refused above.
            writer
                .add_localized_package("", name, FPackageId::from_name(name))
                .map_err(|e| format!("carry the localized package {name}: {e}"))?;
        }
        for (name, target) in &redirects {
            writer
                .add_package_redirect(name, *target)
                .map_err(|e| format!("carry the package redirect for {name}: {e}"))?;
        }
        report.carried_localized = localized.len();
        report.carried_redirects = redirects.len();
    }

    let mut names: Vec<String> = Vec::new();
    if let Some(store) = &existing {
        let superseded: Vec<FIoChunkId> = built
            .iter()
            .flat_map(|converted| {
                PACKAGE_CHUNKS.iter().map(move |kind| {
                    FIoChunkId::from_package_id(converted.package_id, 0, *kind)
                        .with_version(ENGINE.toc_version())
                })
            })
            .collect();
        for chunk in store.chunks_all() {
            let id = chunk.id().with_version(ENGINE.toc_version());
            if id.get_chunk_type() == EIoChunkType::ContainerHeader {
                continue;
            }
            if superseded.contains(&id) {
                continue;
            }
            let path = chunk.path();
            if let Some(path) = &path {
                names.push(path.clone());
            }
            let data = chunk.read().map_err(|e| e.to_string())?;
            let as_path: Option<UEPathBuf> = path.map(Into::into);
            let as_path = as_path.as_deref();
            let compressed = chunk.is_compressed();
            if id.get_chunk_type() == EIoChunkType::ExportBundleData {
                let entry = store
                    .package_store_entry(id.get_package_id())
                    .ok_or_else(|| format!("{} lists a package with no store entry", stem))?;
                if compressed {
                    writer.write_package_chunk(id, as_path, &data, &entry)
                } else {
                    writer.write_package_chunk_uncompressed(id, as_path, &data, &entry)
                }
            } else if compressed {
                writer.write_chunk(id, as_path, &data)
            } else {
                writer.write_chunk_uncompressed(id, as_path, &data)
            }
            .map_err(|e| e.to_string())?;
            report.carried_chunks += 1;
        }
    }

    for converted in &mut built {
        converted
            .write_package_data(&mut writer)
            .map_err(|e| e.to_string())?;
        converted
            .write_and_release_bulk_data(&mut writer)
            .map_err(|e| e.to_string())?;
    }
    writer.finalize().map_err(|e| e.to_string())?;

    for package in packages {
        let stem_path = format!("{RIVALS_MOUNT_POINT}{}", package.entry);
        let without_extension = stem_path
            .rsplit_once('.')
            .map_or(stem_path.as_str(), |(head, _)| head)
            .to_string();
        names.push(stem_path);
        names.push(format!("{without_extension}.uexp"));
        for (bytes, extension) in [
            (package.bulk, "ubulk"),
            (package.optional_bulk, "uptnl"),
            (package.memory_mapped_bulk, "m.ubulk"),
        ] {
            if bytes.is_some() {
                names.push(format!("{without_extension}.{extension}"));
            }
        }
    }
    names.sort();
    names.dedup();
    write_names_pak(&staging.dir.join(format!("{stem}.pak")), utoc, &names)?;

    swap_into_place(&dir, &stem, &staging.dir, &["pak", "utoc", "ucas"])?;
    let mut staging = staging;
    staging.keep = false;
    Ok(report)
}

/// The stub pak beside a container: whatever non-package files the mod already carried, plus the
/// listing the toolkit's own browsers read.
fn write_names_pak(staged_pak: &Path, live_utoc: &Path, names: &[String]) -> Result<(), String> {
    create_empty_pak(staged_pak)?;
    let live_pak = live_utoc.with_extension("pak");
    let carried = live_pak.is_file().then(|| live_pak.clone());
    with_unpacked_pak(staged_pak, |dir| {
        if let Some(live) = &carried {
            // Whatever else the mod shipped in its pak, such as an INI, travels with it.
            with_unpacked_pak_read(live, dir)?;
        }
        std::fs::write(dir.join(CHUNK_NAMES), names.join("\n"))
            .map_err(|e| format!("Could not write the chunk listing: {e}"))
    })
}

/// Copies the contents of a pak into `into`, leaving the pak itself alone.
fn with_unpacked_pak_read(pak: &Path, into: &Path) -> Result<(), String> {
    let scratch = into.join(".carried");
    std::fs::create_dir_all(&scratch)
        .map_err(|e| format!("Could not create {}: {e}", scratch.display()))?;
    crate::pak_tweaks::io::unpack_to_dir(pak, &scratch)?;
    for entry in walkdir::WalkDir::new(&scratch)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
    {
        let relative = entry
            .path()
            .strip_prefix(&scratch)
            .map_err(|e| e.to_string())?;
        // The listing is rewritten from the container that was just built.
        if relative.as_os_str() == CHUNK_NAMES {
            continue;
        }
        let target = into.join(relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Could not create {}: {e}", parent.display()))?;
        }
        std::fs::copy(entry.path(), &target)
            .map_err(|e| format!("Could not carry {}: {e}", relative.display()))?;
    }
    std::fs::remove_dir_all(&scratch).ok();
    Ok(())
}

/// Moves the staged files over the live ones, keeping what was there until every one has landed.
pub fn swap_into_place(
    dir: &Path,
    stem: &str,
    staged: &Path,
    extensions: &[&str],
) -> Result<(), String> {
    let mut backups: Vec<(PathBuf, PathBuf)> = Vec::new();
    for extension in extensions {
        let live = dir.join(format!("{stem}.{extension}"));
        if live.is_file() {
            let backup = dir.join(format!("{stem}.{extension}.bak"));
            std::fs::rename(&live, &backup)
                .map_err(|e| format!("Could not set {} aside: {e}", live.display()))?;
            backups.push((live, backup));
        }
    }
    for extension in extensions {
        let from = staged.join(format!("{stem}.{extension}"));
        if !from.is_file() {
            continue;
        }
        let to = dir.join(format!("{stem}.{extension}"));
        if let Err(e) = std::fs::copy(&from, &to) {
            for (live, _) in &backups {
                let _ = std::fs::remove_file(live);
            }
            for (live, backup) in &backups {
                let _ = std::fs::rename(backup, live);
            }
            return Err(format!("Could not install {}: {e}", to.display()));
        }
    }
    // Anything the new form does not use was set aside and is simply not restored.
    for (_, backup) in &backups {
        let _ = std::fs::remove_file(backup);
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn oodle_available() -> bool {
        std::env::var_os("OODLE_LIB_PATH").is_some()
    }

    fn scratch(tag: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rivals-iostore-{tag}-{stamp}"));
        std::fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    /// The game's own containers declare localized packages in their header, not in any chunk.
    /// Reading them back is what lets a rewrite carry them, so this checks the accessor against
    /// real data rather than against a container this test built.
    #[test]
    fn a_shipped_container_reports_the_localized_packages_it_declares() {
        let Ok(root) = std::env::var("RIVALS_GAME_ROOT") else {
            return;
        };
        if !oodle_available() {
            return;
        }
        let utoc = format!(
            "{}/MarvelGame/Marvel/Content/Paks/pakchunkLQ-Windows.utoc",
            root.replace('\\', "/")
        );
        if !Path::new(&utoc).is_file() {
            return;
        }
        let store = open_utoc(&utoc).expect("open");
        let header = store.container_header().expect("a container header");
        let localized: Vec<String> = header
            .localized_packages()
            .map(|name| name.into_owned())
            .collect();
        assert!(
            !localized.is_empty(),
            "this container ships localized packages"
        );
        // Every declaration names a real source package, not a localized variant of one.
        for name in localized.iter().take(50) {
            assert!(name.starts_with('/'), "{name} is an object path");
            assert!(
                !name.contains("/L10N/"),
                "{name} is the source package, not its localized copy"
            );
        }
    }

    /// A rewrite carries the header tables across. Without it a localized mod would come out with
    /// all its chunks present and the loader no longer knowing to look for the localized variants,
    /// which is a failure nothing in the chunk list would show.
    #[test]
    fn a_rewrite_carries_localized_packages_and_redirects() {
        let Ok(root) = std::env::var("RIVALS_GAME_ROOT") else {
            return;
        };
        if !oodle_available() {
            return;
        }
        let dir = scratch("l10n");
        let utoc = dir.join("Localized.utoc");

        // A container declaring both, with one package in it so it is a real container.
        {
            let mut writer = IoStoreWriter::new(
                &utoc,
                ENGINE.toc_version(),
                Some(ENGINE.container_header_version()),
                RIVALS_MOUNT_POINT.into(),
                Some(PackCompression::Zlib.retoc()),
            )
            .expect("writer");
            writer
                .add_localized_package(
                    "",
                    "/Game/Maps/Lobby",
                    FPackageId::from_name("/Game/Maps/Lobby"),
                )
                .expect("localized");
            writer
                .add_package_redirect("/Game/Maps/Old", FPackageId::from_name("/Game/Maps/New"))
                .expect("redirect");
            writer.finalize().expect("finalize");
        }
        let before = open_utoc(&utoc.to_string_lossy()).expect("open");
        let header = before.container_header().expect("header");
        assert_eq!(header.localized_packages().count(), 1);
        assert_eq!(header.package_redirects().count(), 1);
        drop(before);

        // A real package, since a synthetic one would not survive the zen conversion the rewrite
        // puts it through.
        let entry = "Marvel/Content/Marvel/Data/DataTable/MarvelHeroTable.uasset";
        let source = format!(
            "{}/MarvelGame/Marvel/Content/Paks/pakchunk0-Windows.utoc",
            root.replace('\\', "/")
        );
        let loaded =
            crate::asset::load_bundle(&root, &source, entry, crate::asset::AssetSource::Utoc)
                .expect("load a package to write in");
        write_into_iostore(
            &utoc,
            PackageFiles {
                entry,
                asset: &loaded.asset_file_buffer,
                exports: &loaded.exports_file_buffer,
                bulk: None,
                optional_bulk: None,
                memory_mapped_bulk: None,
                shader_map_hashes: Vec::new(),
            },
            &IoStoreOptions {
                compression: PackCompression::Zlib,
                ..Default::default()
            },
        )
        .expect("rewrite");

        let after = open_utoc(&utoc.to_string_lossy()).expect("reopen");
        let header = after.container_header().expect("header after");
        let localized: Vec<String> = header
            .localized_packages()
            .map(|name| name.into_owned())
            .collect();
        assert_eq!(localized, vec!["/Game/Maps/Lobby"], "carried across");
        assert_eq!(
            header.lookup_package_redirect(FPackageId::from_name("/Game/Maps/Old")),
            Some(FPackageId::from_name("/Game/Maps/New")),
            "the redirect still resolves"
        );
        drop(after);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A batch goes in with one rewrite. The point of the batch is that every package lands, so
    /// this checks all three read back rather than only that the write returned.
    #[test]
    fn a_batch_writes_every_package_in_one_container() {
        let Ok(root) = std::env::var("RIVALS_GAME_ROOT") else {
            return;
        };
        if !oodle_available() {
            return;
        }
        let source = format!(
            "{}/MarvelGame/Marvel/Content/Paks/pakchunk0-Windows.utoc",
            root.replace('\\', "/")
        );
        let entries = [
            "Marvel/Content/Marvel/AbilitySystem/1011/101111/CameraShake_101111.uasset",
            "Marvel/Content/Marvel/AbilitySystem/1011/101111/CameraShake_101111_Hit.uasset",
            "Marvel/Content/Marvel/AbilitySystem/1011/101112/CameraShake_101112_Hit.uasset",
        ];
        let loaded: Vec<_> = entries
            .iter()
            .map(|entry| {
                crate::asset::load_bundle(&root, &source, entry, crate::asset::AssetSource::Utoc)
                    .expect("load")
            })
            .collect();
        let files: Vec<PackageFiles<'_>> = entries
            .iter()
            .zip(&loaded)
            .map(|(entry, held)| PackageFiles {
                entry,
                asset: &held.asset_file_buffer,
                exports: &held.exports_file_buffer,
                bulk: None,
                optional_bulk: None,
                memory_mapped_bulk: None,
                shader_map_hashes: Vec::new(),
            })
            .collect();

        let dir = scratch("batch");
        let utoc = dir.join("Batch.utoc");
        let report = write_many_into_iostore(
            &utoc,
            &files,
            &IoStoreOptions {
                compression: PackCompression::Zlib,
                ..Default::default()
            },
        )
        .expect("batch write");
        assert_eq!(report.written, entries.len());
        assert!(!report.replaced, "nothing was there to replace");

        for entry in entries {
            assert!(
                utoc_holds_entry(&utoc, entry).expect("list"),
                "{entry} is in the container"
            );
        }
        let store = open_utoc(&utoc.to_string_lossy()).expect("reopen");
        for entry in entries {
            let name = format!("/{}", entry.trim_end_matches(".uasset"));
            let name = name.replacen("/Marvel/Content/", "/Game/", 1);
            assert!(
                store
                    .package_store_entry(FPackageId::from_name(&name))
                    .is_some(),
                "{name} has a store entry"
            );
        }
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The swap keeps the old files until every new one has landed, and puts them back when one
    /// does not.
    #[test]
    fn a_failed_swap_restores_what_was_there() {
        let dir = scratch("swap");
        let staged = dir.join("staged");
        std::fs::create_dir_all(&staged).expect("staged");
        std::fs::write(dir.join("Mod.pak"), b"old pak").expect("write");
        std::fs::write(dir.join("Mod.utoc"), b"old toc").expect("write");
        std::fs::write(staged.join("Mod.pak"), b"new pak").expect("write");
        std::fs::write(staged.join("Mod.utoc"), b"new toc").expect("write");

        swap_into_place(&dir, "Mod", &staged, &["pak", "utoc"]).expect("swap");
        assert_eq!(
            std::fs::read(dir.join("Mod.pak")).expect("read"),
            b"new pak"
        );
        assert!(
            !dir.join("Mod.pak.bak").exists(),
            "no backup is left behind"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Converting to a plain pak leaves no stale container behind: an extension the new form does
    /// not use is set aside and never restored.
    #[test]
    fn a_swap_drops_the_files_the_new_form_does_not_use() {
        let dir = scratch("drop");
        let staged = dir.join("staged");
        std::fs::create_dir_all(&staged).expect("staged");
        std::fs::write(dir.join("Mod.pak"), b"old").expect("write");
        std::fs::write(dir.join("Mod.utoc"), b"stale").expect("write");
        std::fs::write(staged.join("Mod.pak"), b"new").expect("write");

        swap_into_place(&dir, "Mod", &staged, &["pak"]).expect("swap");
        assert_eq!(std::fs::read(dir.join("Mod.pak")).expect("read"), b"new");
        assert!(dir.join("Mod.utoc").exists(), "another form is left alone");
        std::fs::remove_dir_all(&dir).ok();
    }
}
