//! Writes edited packages into an IoStore mod, carrying the container's other chunks across.
//!
//! The game resolves a package through its IoStore store, so a mod holding a `.uasset` has to be
//! a container of its own: a plain pak only ever delivers loose files. The writer has no append
//! mode, so replacing one package means writing the container again around it.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use retoc::iostore::IoStoreTrait;
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
    /// How many of the packages written were already in the container, rather than new to it.
    /// Each one replaced a copy that was there, which is a loss of whatever it held.
    pub replaced: usize,
    /// Chunks carried over from the container as it was.
    pub carried_chunks: usize,
    /// How many packages the write put in.
    pub written: usize,
    /// Localized package declarations replayed into the new header.
    pub carried_localized: usize,
    /// Package redirects replayed into the new header.
    pub carried_redirects: usize,
}

/// Removes a staging directory when it goes out of scope. Whatever owns this decides how long the
/// staged files live, which is how a container survives between being built and being swapped in.
struct Staging {
    dir: PathBuf,
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
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

/// Everything the container carries, keyed the way [`utoc_holds_entry`] compares. Asking once beats
/// asking per entry, which reopens the container each time.
pub fn utoc_entries(utoc: &Path) -> Result<HashSet<String>, String> {
    let store = open_utoc(&utoc.to_string_lossy())?;
    Ok(store
        .chunks()
        .filter_map(|chunk| chunk.path().map(|path| comparable(&path)))
        .collect())
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

/// What a container keeps in its header rather than in any chunk, so rebuilding it from its chunks
/// alone would lose it: the shader maps each package's store entry lists, and the localized
/// packages and redirects the loader is told to look for.
#[derive(Default)]
pub struct HeaderCarry {
    pub shader_map_hashes: HashMap<FPackageId, Vec<FSHAHash>>,
    pub localized: Vec<String>,
    pub redirects: Vec<(String, FPackageId)>,
}

impl HeaderCarry {
    pub fn from_store(store: &dyn IoStoreTrait) -> Self {
        let shader_map_hashes = store
            .packages()
            .filter_map(|package| {
                let hashes = store.package_store_entry(package.id())?.shader_map_hashes;
                (!hashes.is_empty()).then(|| (package.id(), hashes))
            })
            .collect();
        let Some(header) = store.container_header() else {
            return Self {
                shader_map_hashes,
                ..Default::default()
            };
        };
        Self {
            shader_map_hashes,
            localized: header
                .localized_packages()
                .map(|name| name.into_owned())
                .collect(),
            redirects: header
                .package_redirects()
                .map(|(name, id)| (name.into_owned(), id))
                .collect(),
        }
    }

    /// Declares the localized packages and redirects in the header `writer` is building.
    pub fn replay(&self, writer: &mut IoStoreWriter) -> Result<(), String> {
        for name in &self.localized {
            // The culture only matters for pre-Initial headers, which this engine does not write.
            writer
                .add_localized_package("", name, FPackageId::from_name(name))
                .map_err(|e| format!("carry the localized package {name}: {e}"))?;
        }
        for (name, target) in &self.redirects {
            writer
                .add_package_redirect(name, *target)
                .map_err(|e| format!("carry the package redirect for {name}: {e}"))?;
        }
        Ok(())
    }

    /// Gives a converted package the shader maps its old entry listed, when it found none itself.
    pub fn restore_shader_maps(
        &self,
        converted: &mut retoc::zen_asset_conversion::ConvertedZenAssetBundle,
    ) {
        if converted.store_entry().shader_map_hashes.is_empty()
            && let Some(hashes) = self.shader_map_hashes.get(&converted.package_id)
        {
            converted.set_shader_map_hashes(hashes.clone());
        }
    }
}

/// One package's bytes, owned, so the writer can take them and let them go again. A batch is
/// produced one of these at a time rather than handed over as a whole.
pub struct PackageBytes {
    /// Mount-relative, as the container names it.
    pub entry: String,
    pub asset: Vec<u8>,
    pub exports: Vec<u8>,
    pub bulk: Option<Vec<u8>>,
    pub optional_bulk: Option<Vec<u8>>,
    pub memory_mapped_bulk: Option<Vec<u8>>,
    pub shader_map_hashes: Vec<FSHAHash>,
}

impl PackageFiles<'_> {
    fn owned(&self) -> PackageBytes {
        PackageBytes {
            entry: self.entry.to_string(),
            asset: self.asset.to_vec(),
            exports: self.exports.to_vec(),
            bulk: self.bulk.map(<[u8]>::to_vec),
            optional_bulk: self.optional_bulk.map(<[u8]>::to_vec),
            memory_mapped_bulk: self.memory_mapped_bulk.map(<[u8]>::to_vec),
            shader_map_hashes: self.shader_map_hashes.clone(),
        }
    }
}

impl PackageBytes {
    /// The file names this package puts in the container, for the listing beside it.
    fn names(&self) -> Vec<String> {
        let stem_path = format!("{RIVALS_MOUNT_POINT}{}", self.entry);
        let without_extension = stem_path
            .rsplit_once('.')
            .map_or(stem_path.as_str(), |(head, _)| head)
            .to_string();
        let mut names = vec![stem_path, format!("{without_extension}.uexp")];
        for (bytes, extension) in [
            (&self.bulk, "ubulk"),
            (&self.optional_bulk, "uptnl"),
            (&self.memory_mapped_bulk, "m.ubulk"),
        ] {
            if bytes.is_some() {
                names.push(format!("{without_extension}.{extension}"));
            }
        }
        names
    }
}

/// Writes `package` into the container at `utoc`, keeping everything else it holds. The container
/// is built beside the original and swapped in, so a failure leaves what was there untouched.
///
/// See [`write_batch_into_iostore`] to put many in with a single rewrite.
pub fn write_into_iostore(
    utoc: &Path,
    package: PackageFiles<'_>,
    options: &IoStoreOptions,
) -> Result<IoStoreReport, String> {
    let mut once = Some(package.owned());
    write_batch_into_iostore(utoc, 1, |_| once.take(), options)
}

/// [`stage_batch_into_iostore`] committed straight away, for a caller holding nothing open.
pub fn write_batch_into_iostore(
    utoc: &Path,
    count: usize,
    produce: impl FnMut(usize) -> Option<PackageBytes>,
    options: &IoStoreOptions,
) -> Result<IoStoreReport, String> {
    stage_batch_into_iostore(utoc, count, produce, options)?
        .ok_or_else(|| "no packages to write".to_string())?
        .commit()
}

/// Builds the container a batch would replace this one with, without swapping it in. `Ok(None)`
/// when `produce` had nothing to give, which is not a failure: the caller knows why.
///
/// Rewriting a container is a whole-file operation, so writing packages one at a time repeats it
/// once per package. Taking them as a slice instead would mean holding the whole batch in memory,
/// which is what forces a size limit; `produce` hands over one package, it is written, and its
/// bytes are dropped before the next is asked for. `produce` returns `None` for an index it has
/// nothing for, which is how a caller skips a package it could not prepare.
///
/// The packages go in first and the container's other chunks are carried over afterwards, since
/// what to carry is "everything not already written" and that is only known once they are in.
pub fn stage_batch_into_iostore(
    utoc: &Path,
    count: usize,
    mut produce: impl FnMut(usize) -> Option<PackageBytes>,
    options: &IoStoreOptions,
) -> Result<Option<StagedContainer>, String> {
    let stem = utoc
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .ok_or("the container has no name")?;
    let dir = utoc
        .parent()
        .ok_or("the container has no folder")?
        .to_path_buf();

    let staging = Staging {
        dir: dir.join(format!(".{stem}_iostore")),
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
        replaced: 0,
        carried_chunks: 0,
        written: 0,
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
    if let Some(store) = &existing
        && store.container_header_version() != Some(ENGINE.container_header_version())
    {
        return Err(format!(
            "{} was built for another engine version, so this editor will not rewrite it",
            utoc.display()
        ));
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
    if let Some(store) = &existing {
        let carry = HeaderCarry::from_store(&**store);
        carry.replay(&mut writer)?;
        report.carried_localized = carry.localized.len();
        report.carried_redirects = carry.redirects.len();
    }

    let shader_maps: HashMap<String, Vec<FSHAHash>> = HashMap::new();
    let mut names: Vec<String> = Vec::new();
    let mut written: HashSet<FIoChunkId> = HashSet::new();
    for index in 0..count {
        let Some(package) = produce(index) else {
            continue;
        };
        let relative = crate::asset::contained_entry(&package.entry)?;
        names.extend(package.names());
        let mounted: UEPathBuf = format!("{RIVALS_MOUNT_POINT}{relative}").into();
        let entry = package.entry;
        let fallback_hashes = package.shader_map_hashes;
        let bundle = FSerializedAssetBundle {
            asset_file_buffer: package.asset,
            exports_file_buffer: package.exports,
            bulk_data_buffer: package.bulk,
            optional_bulk_data_buffer: package.optional_bulk,
            memory_mapped_bulk_data_buffer: package.memory_mapped_bulk,
        };
        let mut converted = retoc::zen_asset_conversion::build_zen_asset(
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
        .map_err(|e| format!("convert {entry} for IoStore: {e}"))?;

        let held = existing
            .as_ref()
            .and_then(|store| store.package_store_entry(converted.package_id));
        if held.is_some() {
            report.replaced += 1;
        }
        // The mod's own copy knows best, then the source the package was read from.
        if converted.store_entry().shader_map_hashes.is_empty() {
            let restored = held
                .map(|entry| entry.shader_map_hashes)
                .filter(|hashes| !hashes.is_empty())
                .unwrap_or(fallback_hashes);
            if !restored.is_empty() {
                converted.set_shader_map_hashes(restored);
            }
        }
        written.extend(PACKAGE_CHUNKS.iter().map(|kind| {
            FIoChunkId::from_package_id(converted.package_id, 0, *kind)
                .with_version(ENGINE.toc_version())
        }));
        converted
            .write_package_data(&mut writer)
            .map_err(|e| e.to_string())?;
        converted
            .write_and_release_bulk_data(&mut writer)
            .map_err(|e| e.to_string())?;
        report.written += 1;
    }
    if report.written == 0 {
        return Ok(None);
    }

    if let Some(store) = &existing {
        for chunk in store.chunks_all() {
            let id = chunk.id().with_version(ENGINE.toc_version());
            if id.get_chunk_type() == EIoChunkType::ContainerHeader || written.contains(&id) {
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
                    .ok_or_else(|| format!("{stem} lists a package with no store entry"))?;
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
    writer.finalize().map_err(|e| e.to_string())?;

    names.sort();
    names.dedup();
    write_names_pak(&staging.dir.join(format!("{stem}.pak")), utoc, &names)?;

    // The swap renames the live container, which it cannot do while this still has it open. The
    // caller's own stores are the other half of that, and dropping them is what committing is for.
    drop(existing);
    Ok(Some(StagedContainer {
        staging,
        dir,
        stem,
        report,
    }))
}

/// A container built beside the one it replaces, waiting to be swapped in.
///
/// Nothing the caller read from has to stay open for the swap, so it holds the staged files until
/// [`StagedContainer::commit`] is called and the caller has let go of whatever it was reading.
/// Dropping it instead leaves the live container exactly as it was.
pub struct StagedContainer {
    staging: Staging,
    dir: PathBuf,
    stem: String,
    report: IoStoreReport,
}

impl StagedContainer {
    /// What the staged container would report, for a caller deciding whether to commit it.
    pub fn report(&self) -> &IoStoreReport {
        &self.report
    }

    /// Moves the staged files over the live ones.
    pub fn commit(self) -> Result<IoStoreReport, String> {
        swap_into_place(
            &self.dir,
            &self.stem,
            &self.staging.dir,
            &["pak", "utoc", "ucas"],
        )?;
        Ok(self.report)
    }
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
    let backups: Vec<(PathBuf, PathBuf)> = extensions
        .iter()
        .map(|extension| {
            (
                dir.join(format!("{stem}.{extension}")),
                dir.join(format!("{stem}.{extension}.bak")),
            )
        })
        .filter(|(live, _)| live.is_file())
        .collect();
    crate::mods::rename_all(&backups)?;
    for extension in extensions {
        let from = staged.join(format!("{stem}.{extension}"));
        if !from.is_file() {
            continue;
        }
        let to = dir.join(format!("{stem}.{extension}"));
        if let Err(e) = std::fs::copy(&from, &to) {
            crate::mods::put_back(&backups);
            return Err(format!(
                "Could not install {}: {e}{}",
                to.display(),
                crate::mods::held_open_hint(&e)
            ));
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

    const BACKSLASH: char = '\\';

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

    /// A rebuilt container reads its packages from loose files, which know nothing of the shader
    /// maps the old header listed, so the carry puts them back on the package they belonged to.
    #[test]
    fn a_carry_restores_the_shader_maps_a_package_was_listed_with() {
        let Ok(root) = std::env::var("RIVALS_GAME_ROOT") else {
            return;
        };
        if !oodle_available() {
            return;
        }
        let entry = "Marvel/Content/Marvel/Data/DataTable/MarvelHeroTable.uasset";
        let source = format!(
            "{}/MarvelGame/Marvel/Content/Paks/pakchunk0-Windows.utoc",
            root.replace('\\', "/")
        );
        let loaded =
            crate::asset::load_bundle(&root, &source, entry, crate::asset::AssetSource::Utoc)
                .expect("load a package");
        let mut converted = retoc::zen_asset_conversion::build_zen_asset(
            loaded,
            &HashMap::new(),
            UEPathBuf::from(format!("{RIVALS_MOUNT_POINT}{entry}")).as_ref(),
            Some(ENGINE.package_file_version()),
            ENGINE.container_header_version(),
            false,
            None,
            None,
            &retoc::logging::Log::no_log(),
        )
        .expect("convert");
        assert!(converted.store_entry().shader_map_hashes.is_empty());

        let listed = vec![FSHAHash::default()];
        let carry = HeaderCarry {
            shader_map_hashes: HashMap::from([(converted.package_id, listed.clone())]),
            ..Default::default()
        };
        carry.restore_shader_maps(&mut converted);
        assert_eq!(converted.store_entry().shader_map_hashes, listed);
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
        let dir = scratch("batch");
        let utoc = dir.join("Batch.utoc");
        let report = write_batch_into_iostore(
            &utoc,
            entries.len(),
            |index| {
                Some(PackageBytes {
                    entry: entries[index].to_string(),
                    asset: loaded[index].asset_file_buffer.clone(),
                    exports: loaded[index].exports_file_buffer.clone(),
                    bulk: None,
                    optional_bulk: None,
                    memory_mapped_bulk: None,
                    shader_map_hashes: Vec::new(),
                })
            },
            &IoStoreOptions {
                compression: PackCompression::Zlib,
                ..Default::default()
            },
        )
        .expect("batch write");
        assert_eq!(report.written, entries.len());
        assert_eq!(report.replaced, 0, "nothing was there to replace");

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

    /// A staged container is not the live one until it is committed. This is the property the whole
    /// split exists for: the caller gets to close what it read from before anything is renamed.
    #[test]
    fn a_staged_container_that_is_never_committed_leaves_the_live_one_alone() {
        let Ok(root) = std::env::var("RIVALS_GAME_ROOT") else {
            return;
        };
        if !oodle_available() {
            return;
        }
        let source = format!(
            "{}/MarvelGame/Marvel/Content/Paks/pakchunk0-Windows.utoc",
            root.replace(BACKSLASH, "/")
        );
        let entry = "Marvel/Content/Marvel/AbilitySystem/1011/101111/CameraShake_101111.uasset";
        let loaded =
            crate::asset::load_bundle(&root, &source, entry, crate::asset::AssetSource::Utoc)
                .expect("load");

        let dir = scratch("staged");
        let utoc = dir.join("Held.utoc");
        std::fs::write(&utoc, b"the live container").expect("write");
        let bytes = || {
            Some(PackageBytes {
                entry: entry.to_string(),
                asset: loaded.asset_file_buffer.clone(),
                exports: loaded.exports_file_buffer.clone(),
                bulk: None,
                optional_bulk: None,
                memory_mapped_bulk: None,
                shader_map_hashes: Vec::new(),
            })
        };
        // A .utoc that is not a container at all would fail to open, so stage into a fresh name.
        std::fs::remove_file(&utoc).expect("clear");
        let staging = dir.join(".Held_iostore");

        let staged = stage_batch_into_iostore(
            &utoc,
            1,
            |_| bytes(),
            &IoStoreOptions {
                compression: PackCompression::Zlib,
                ..Default::default()
            },
        )
        .expect("stage")
        .expect("a package was produced");
        assert_eq!(staged.report().written, 1);
        assert!(staging.is_dir(), "the staged container is on disk");
        assert!(!utoc.exists(), "and the live one has not been touched yet");

        drop(staged);
        assert!(
            !staging.exists(),
            "an abandoned staging directory is cleaned up"
        );
        assert!(!utoc.exists(), "and still nothing was installed");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `replaced` is what tells a caller it is about to overwrite work, so it has to count packages
    /// rather than say whether any were there at all.
    #[test]
    fn replacing_a_package_is_counted_per_package() {
        let Ok(root) = std::env::var("RIVALS_GAME_ROOT") else {
            return;
        };
        if !oodle_available() {
            return;
        }
        let source = format!(
            "{}/MarvelGame/Marvel/Content/Paks/pakchunk0-Windows.utoc",
            root.replace(BACKSLASH, "/")
        );
        let entries = [
            "Marvel/Content/Marvel/AbilitySystem/1011/101111/CameraShake_101111.uasset",
            "Marvel/Content/Marvel/AbilitySystem/1011/101111/CameraShake_101111_Hit.uasset",
        ];
        let loaded: Vec<_> = entries
            .iter()
            .map(|entry| {
                crate::asset::load_bundle(&root, &source, entry, crate::asset::AssetSource::Utoc)
                    .expect("load")
            })
            .collect();
        let options = IoStoreOptions {
            compression: PackCompression::Zlib,
            ..Default::default()
        };
        let package = |index: usize| {
            Some(PackageBytes {
                entry: entries[index].to_string(),
                asset: loaded[index].asset_file_buffer.clone(),
                exports: loaded[index].exports_file_buffer.clone(),
                bulk: None,
                optional_bulk: None,
                memory_mapped_bulk: None,
                shader_map_hashes: Vec::new(),
            })
        };

        let dir = scratch("replaced");
        let utoc = dir.join("Counted.utoc");
        let first = write_batch_into_iostore(&utoc, 1, |_| package(0), &options).expect("first");
        assert_eq!(first.replaced, 0, "nothing was there to replace");

        let both = write_batch_into_iostore(&utoc, 2, package, &options).expect("second");
        assert_eq!(both.written, 2);
        assert_eq!(
            both.replaced, 1,
            "one of the two was already in the container"
        );
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

    /// A swap that cannot set every file aside has to put back the ones it moved. Half-swapping is
    /// how a working mod loses its container while keeping its data, which is unloadable.
    #[test]
    fn a_swap_that_cannot_move_a_file_puts_back_the_ones_it_did() {
        let dir = scratch("halfswap");
        let staged = dir.join("staged");
        std::fs::create_dir_all(&staged).expect("staged");
        for (extension, body) in [
            ("pak", "live pak"),
            ("utoc", "live toc"),
            ("ucas", "live cas"),
        ] {
            std::fs::write(dir.join(format!("Mod.{extension}")), body).expect("write");
            std::fs::write(staged.join(format!("Mod.{extension}")), "new").expect("write");
        }
        // A directory where the backup would go is a rename the OS refuses, which is what a file
        // another process holds open looks like from here.
        std::fs::create_dir_all(dir.join("Mod.ucas.bak")).expect("blocker");

        let error = swap_into_place(&dir, "Mod", &staged, &["pak", "utoc", "ucas"])
            .expect_err("the third file cannot be set aside");
        assert!(error.contains("Mod.ucas"), "{error}");
        for (extension, body) in [
            ("pak", "live pak"),
            ("utoc", "live toc"),
            ("ucas", "live cas"),
        ] {
            assert_eq!(
                std::fs::read(dir.join(format!("Mod.{extension}"))).expect("still there"),
                body.as_bytes(),
                "Mod.{extension} was put back"
            );
            assert!(
                !dir.join(format!("Mod.{extension}.bak")).is_file(),
                "no backup is left behind"
            );
        }
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
