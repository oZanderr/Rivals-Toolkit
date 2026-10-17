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
use crate::pak::plain::{self, PakOptions};
use crate::pak::profile::{
    self, PackCompression, RIVALS_BLOCK_SIZE, RIVALS_MOUNT_POINT, strip_mount_prefix,
};

/// Marvel Rivals ships UE 5.3, and a mod container has to answer to the same loader.
const ENGINE: EngineVersion = EngineVersion::UE5_3;

#[derive(Debug, Clone, Copy)]
pub struct IoStoreOptions {
    pub compression: PackCompression,
    pub oodle_level: Option<retoc::OodleCompressionLevel>,
    /// `None` keeps whatever the container had. A container written from nothing is left plain.
    pub obfuscate: Option<bool>,
    /// Compress the chunks a rewrite carries over again, at `compression` and `oodle_level`. Off,
    /// they keep the blocks they were stored in, and only a block in another method is compressed
    /// again.
    pub reencode: bool,
}

impl IoStoreOptions {
    /// The same compression for the pak beside the container.
    pub fn pak(&self) -> PakOptions {
        PakOptions {
            compression: self.compression,
            oodle_level: self.oodle_level,
        }
    }
}

impl Default for IoStoreOptions {
    fn default() -> Self {
        Self {
            compression: PackCompression::Oodle,
            oodle_level: None,
            obfuscate: None,
            reencode: false,
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
    /// Packages left out of the rewrite on purpose.
    pub removed: usize,
    /// Blocks of the carried chunks written as they were stored.
    pub copied_blocks: usize,
    /// Blocks of the carried chunks compressed again.
    pub recompressed_blocks: usize,
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

/// An entry as [`utoc_entries`] keys it.
pub fn entry_key(entry: &str) -> String {
    comparable(entry)
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

    /// The same, without anything that names one of `dropped`.
    pub fn without(&self, dropped: &HashSet<FPackageId>) -> Self {
        let kept = |name: &str| !dropped.contains(&FPackageId::from_name(name));
        Self {
            shader_map_hashes: self
                .shader_map_hashes
                .iter()
                .filter(|(id, _)| !dropped.contains(id))
                .map(|(id, hashes)| (*id, hashes.clone()))
                .collect(),
            localized: self
                .localized
                .iter()
                .filter(|name| kept(name))
                .cloned()
                .collect(),
            redirects: self
                .redirects
                .iter()
                .filter(|(name, target)| kept(name) && !dropped.contains(target))
                .cloned()
                .collect(),
        }
    }

    /// The same, with only what names one of `kept`: what packages brought in from another
    /// container take with them.
    pub fn only(&self, kept: &HashSet<FPackageId>) -> Self {
        let named = |name: &str| kept.contains(&FPackageId::from_name(name));
        Self {
            shader_map_hashes: self
                .shader_map_hashes
                .iter()
                .filter(|(id, _)| kept.contains(id))
                .map(|(id, hashes)| (*id, hashes.clone()))
                .collect(),
            localized: self
                .localized
                .iter()
                .filter(|name| named(name))
                .cloned()
                .collect(),
            redirects: self
                .redirects
                .iter()
                .filter(|(name, target)| named(name) || kept.contains(target))
                .cloned()
                .collect(),
        }
    }

    /// Takes in `other`'s records too, its own winning where both name a package.
    pub fn merge(&mut self, other: Self) {
        self.shader_map_hashes.extend(other.shader_map_hashes);
        for name in other.localized {
            if !self
                .localized
                .iter()
                .any(|held| held.eq_ignore_ascii_case(&name))
            {
                self.localized.push(name);
            }
        }
        for (name, target) in other.redirects {
            self.redirects
                .retain(|(held, _)| !held.eq_ignore_ascii_case(&name));
            self.redirects.push((name, target));
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

    /// Gives a converted package the shader maps its old entry listed, beside any a shader
    /// archive's asset info gave it.
    pub fn restore_shader_maps(
        &self,
        converted: &mut retoc::zen_asset_conversion::ConvertedZenAssetBundle,
    ) {
        let Some(carried) = self.shader_map_hashes.get(&converted.package_id) else {
            return;
        };
        let mut hashes = converted.store_entry().shader_map_hashes.clone();
        let before = hashes.len();
        for hash in carried {
            if !hashes.contains(hash) {
                hashes.push(*hash);
            }
        }
        if hashes.len() != before {
            converted.set_shader_map_hashes(hashes);
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
    produce: impl FnMut(usize) -> Option<PackageBytes>,
    options: &IoStoreOptions,
) -> Result<Option<StagedContainer>, String> {
    stage_container(utoc, count, produce, &HashSet::new(), options)
}

/// Packages another container holds, written into one as they are stored there: their chunks and
/// store entries byte for byte, so nothing is converted and nothing is read from the game.
pub struct PackagesFrom<'a> {
    pub store: &'a dyn IoStoreTrait,
    pub packages: HashSet<FPackageId>,
}

/// Builds the container at `utoc` with `from`'s packages written in over its own copies of them,
/// keeping everything else it holds, without swapping it in.
pub fn stage_bringing_into_iostore(
    utoc: &Path,
    from: &PackagesFrom<'_>,
    options: &IoStoreOptions,
) -> Result<Option<StagedContainer>, String> {
    stage_container_from(
        utoc,
        utoc,
        0,
        |_| None,
        &HashSet::new(),
        Some(from),
        options,
    )
}

/// Writes `package` and `others` into the container and drops the packages in `dropped`, in one
/// rewrite.
pub fn write_replacing_in_iostore(
    utoc: &Path,
    package: PackageFiles<'_>,
    others: Vec<PackageBytes>,
    dropped: &HashSet<FPackageId>,
    options: &IoStoreOptions,
) -> Result<IoStoreReport, String> {
    let mut packages: Vec<Option<PackageBytes>> = std::iter::once(package.owned())
        .chain(others)
        .map(Some)
        .collect();
    let count = packages.len();
    let produce = |at: usize| packages.get_mut(at).and_then(Option::take);
    stage_container(utoc, count, produce, dropped, options)?
        .ok_or_else(|| "no packages to write".to_string())?
        .commit()
}

/// Rewrites the container without the packages in `dropped`, keeping everything else it holds.
pub fn remove_from_iostore(
    utoc: &Path,
    dropped: &HashSet<FPackageId>,
    options: &IoStoreOptions,
) -> Result<IoStoreReport, String> {
    stage_container(utoc, 0, |_| None, dropped, options)?
        .ok_or_else(|| "nothing to remove".to_string())?
        .commit()
}

/// Copies the container at `source` to `dest`, rebuilt under the new name so it mounts as a
/// container of its own, without the packages in `dropped`. Whatever else the source's pak ships
/// travels with it.
pub fn copy_container(
    source: &Path,
    dest: &Path,
    dropped: &HashSet<FPackageId>,
    options: &IoStoreOptions,
) -> Result<IoStoreReport, String> {
    stage_container_from(source, dest, 0, |_| None, dropped, None, options)?
        .ok_or_else(|| "nothing to copy".to_string())?
        .commit()
}

fn stage_container(
    utoc: &Path,
    count: usize,
    produce: impl FnMut(usize) -> Option<PackageBytes>,
    dropped: &HashSet<FPackageId>,
    options: &IoStoreOptions,
) -> Result<Option<StagedContainer>, String> {
    stage_container_from(utoc, utoc, count, produce, dropped, None, options)
}

/// The container read from `source` and built under `dest`'s name, which is where its id comes
/// from: the same path to rewrite one in place, another to copy it.
fn stage_container_from(
    source: &Path,
    dest: &Path,
    count: usize,
    mut produce: impl FnMut(usize) -> Option<PackageBytes>,
    dropped: &HashSet<FPackageId>,
    brought: Option<&PackagesFrom<'_>>,
    options: &IoStoreOptions,
) -> Result<Option<StagedContainer>, String> {
    let copying = source != dest;
    let utoc = source;
    // A disabled container is named `X.utoc.disabled`: its id is `X`, not `X.utoc`.
    let bare = |path: &Path| {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let name = name.strip_suffix(".disabled").unwrap_or(&name).to_string();
        Path::new(&name)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
    };
    let stem = bare(dest).ok_or("the container has no name")?;
    let switched_off = dest
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".disabled"));
    let dir = dest
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
        utoc: if copying {
            dir.join(format!("{stem}.utoc"))
        } else {
            utoc.to_path_buf()
        },
        replaced: 0,
        carried_chunks: 0,
        written: 0,
        carried_localized: 0,
        carried_redirects: 0,
        removed: 0,
        copied_blocks: 0,
        recompressed_blocks: 0,
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
    let mut carry = existing
        .as_ref()
        .map(|store| HeaderCarry::from_store(&**store).without(dropped))
        .unwrap_or_default();
    if let Some(brought) = brought {
        carry.merge(HeaderCarry::from_store(brought.store).only(&brought.packages));
    }
    carry.replay(&mut writer)?;
    report.carried_localized = carry.localized.len();
    report.carried_redirects = carry.redirects.len();

    let shader_maps: HashMap<String, Vec<FSHAHash>> = HashMap::new();
    let mut written: HashSet<FIoChunkId> = HashSet::new();
    let mut carried: Vec<Carried> = Vec::new();
    for index in 0..count {
        let Some(package) = produce(index) else {
            continue;
        };
        let relative = crate::asset::contained_entry(&package.entry)?;
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
    if let Some(brought) = brought {
        for chunk in brought.store.chunks_all() {
            let id = chunk.id().with_version(ENGINE.toc_version());
            if !PACKAGE_CHUNKS.contains(&id.get_chunk_type())
                || !brought.packages.contains(&id.get_package_id())
                || !written.insert(id)
            {
                continue;
            }
            if id.get_chunk_type() == EIoChunkType::ExportBundleData {
                let held = existing
                    .as_ref()
                    .is_some_and(|store| store.package_store_entry(id.get_package_id()).is_some());
                if held {
                    report.replaced += 1;
                }
                report.written += 1;
            }
            carried.push(write_stored_chunk(
                &mut writer,
                brought.store,
                &chunk,
                id,
                &stem,
                options.reencode,
            )?);
        }
    }
    if report.written == 0 && dropped.is_empty() && !copying {
        return Ok(None);
    }

    if let Some(store) = &existing {
        for chunk in store.chunks_all() {
            let id = chunk.id().with_version(ENGINE.toc_version());
            if id.get_chunk_type() == EIoChunkType::ContainerHeader || written.contains(&id) {
                continue;
            }
            if PACKAGE_CHUNKS.contains(&id.get_chunk_type())
                && dropped.contains(&id.get_package_id())
            {
                if id.get_chunk_type() == EIoChunkType::ExportBundleData {
                    report.removed += 1;
                }
                continue;
            }
            carried.push(write_stored_chunk(
                &mut writer,
                &**store,
                &chunk,
                id,
                &stem,
                options.reencode,
            )?);
            report.carried_chunks += 1;
        }
    }
    writer.finalize().map_err(|e| e.to_string())?;
    for chunk in &carried {
        report.copied_blocks += chunk.copied;
        report.recompressed_blocks += chunk.recompressed;
    }
    check_written(&staged_utoc, &carried, &written, &stem)?;

    write_companion_pak(
        &staging.dir.join(format!("{stem}.pak")),
        utoc,
        &options.pak(),
    )?;

    // The swap renames the live container, which it cannot do while this still has it open. The
    // caller's own stores are the other half of that, and dropping them is what committing is for.
    drop(existing);
    Ok(Some(StagedContainer {
        staging,
        dir,
        stem,
        switched_off,
        report,
    }))
}

/// What a container's stored form allows a rewrite: whether its chunks can be carried as they are
/// stored, and whether it compresses them at all.
pub struct StoredForm {
    /// Why its chunks cannot be carried as stored, if they cannot.
    pub why_not: Option<String>,
    /// Whether the container names any compression method, which one written plain does not.
    pub compressed: bool,
}

/// The stored form of the container at `utoc`: one built for another engine version holds ids and
/// a header this editor would write differently, so its chunks are not copied as they are.
pub fn stored_form(utoc: &Path) -> Result<StoredForm, String> {
    let store = open_utoc(&utoc.to_string_lossy())?;
    let why_not = if store.container_file_version() != Some(ENGINE.toc_version()) {
        Some("its TOC is not the game's version".to_string())
    } else if store.container_header_version() != Some(ENGINE.container_header_version()) {
        Some("its header is not the game's version".to_string())
    } else {
        None
    };
    Ok(StoredForm {
        why_not,
        compressed: !store.compression_methods().is_empty(),
    })
}

/// A chunk a rewrite carried over: the hash its data has to read back as, and how its blocks went
/// in.
struct Carried {
    id: FIoChunkId,
    hash: [u8; 32],
    copied: usize,
    recompressed: usize,
}

/// Writes a chunk `store` holds into `writer`, a package's with its store entry: the blocks as they
/// are stored, unless `reencode` asks for them compressed again or they are not laid out the way a
/// copy needs, when its data is read and written anew.
fn write_stored_chunk(
    writer: &mut IoStoreWriter,
    store: &dyn IoStoreTrait,
    chunk: &retoc::iostore::ChunkInfo<'_>,
    id: FIoChunkId,
    stem: &str,
    reencode: bool,
) -> Result<Carried, String> {
    let path = chunk.path();
    let as_path: Option<UEPathBuf> = path.clone().map(Into::into);
    let as_path = as_path.as_deref();
    let entry = if id.get_chunk_type() == EIoChunkType::ExportBundleData {
        Some(
            store
                .package_store_entry(id.get_package_id())
                .ok_or_else(|| format!("{stem} lists a package with no store entry"))?,
        )
    } else {
        None
    };
    let stored = if reencode {
        None
    } else {
        chunk.read_stored().map_err(|e| e.to_string())?
    };
    if let Some(stored) = stored {
        let copy = writer
            .write_stored(id, as_path, &stored, entry.as_ref())
            .map_err(|e| e.to_string())?;
        return Ok(Carried {
            id,
            hash: stored.hash,
            copied: copy.copied,
            recompressed: copy.recompressed,
        });
    }
    let data = chunk.read().map_err(|e| e.to_string())?;
    let compressed = chunk.is_compressed();
    match (&entry, compressed) {
        (Some(entry), true) => writer.write_package_chunk(id, as_path, &data, entry),
        (Some(entry), false) => writer.write_package_chunk_uncompressed(id, as_path, &data, entry),
        (None, true) => writer.write_chunk(id, as_path, &data),
        (None, false) => writer.write_chunk_uncompressed(id, as_path, &data),
    }
    .map_err(|e| e.to_string())?;
    let blocks = store
        .compression_block_size()
        .filter(|size| *size > 0)
        .map_or(1, |size| data.len().div_ceil(size as usize));
    Ok(Carried {
        id,
        hash: *blake3::hash(&data).as_bytes(),
        copied: 0,
        recompressed: blocks,
    })
}

/// Holds a rewrite to what went into it before it can replace anything: every chunk carried over
/// reads back as the data it was, however its blocks went in, and the header lists exactly the
/// packages written and carried. A copied block landing in the wrong place would fail here rather
/// than in the game.
fn check_written(
    staged_utoc: &Path,
    carried: &[Carried],
    written: &HashSet<FIoChunkId>,
    stem: &str,
) -> Result<(), String> {
    // Opened and closed here: a handle left open would keep the staging folder from being removed.
    let store = open_utoc(&staged_utoc.to_string_lossy())?;
    for chunk in carried {
        let data = store
            .read(chunk.id)
            .map_err(|e| format!("{stem}: a carried chunk does not read back: {e}"))?;
        if blake3::hash(&data).as_bytes() != &chunk.hash {
            return Err(format!(
                "{stem}: a chunk carried into the rewrite does not read back as it was ({:?}), so the \
                 mod stays as it is",
                chunk.id
            ));
        }
    }
    let expected: HashSet<FPackageId> = written
        .iter()
        .chain(carried.iter().map(|chunk| &chunk.id))
        .filter(|id| id.get_chunk_type() == EIoChunkType::ExportBundleData)
        .map(|id| id.get_package_id())
        .collect();
    let listed: HashSet<FPackageId> = store.packages().map(|package| package.id()).collect();
    if listed != expected {
        return Err(format!(
            "{stem}: the rewritten header lists {} package(s) where {} went in, so the mod stays as \
             it is",
            listed.len(),
            expected.len()
        ));
    }
    Ok(())
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
    /// The live container is `X.utoc.disabled`, so the files are swapped in under those names.
    switched_off: bool,
    report: IoStoreReport,
}

impl StagedContainer {
    /// What the staged container would report, for a caller deciding whether to commit it.
    pub fn report(&self) -> &IoStoreReport {
        &self.report
    }

    /// Moves the staged files over the live ones, keeping the mod switched on or off.
    pub fn commit(self) -> Result<IoStoreReport, String> {
        let suffix = if self.switched_off { ".disabled" } else { "" };
        let mut extensions = Vec::new();
        for extension in ["pak", "utoc", "ucas"] {
            let named = format!("{extension}{suffix}");
            let staged = self.staging.dir.join(format!("{}.{extension}", self.stem));
            if self.switched_off && staged.is_file() {
                let to = self.staging.dir.join(format!("{}.{named}", self.stem));
                std::fs::rename(&staged, &to)
                    .map_err(|e| format!("Could not name {}: {e}", staged.display()))?;
            }
            extensions.push(named);
        }
        let extensions: Vec<&str> = extensions.iter().map(String::as_str).collect();
        swap_into_place(&self.dir, &self.stem, &self.staging.dir, &extensions)?;
        Ok(self.report)
    }
}

/// The pak beside a container: `X.pak` for `X.utoc`, `X.pak.disabled` for `X.utoc.disabled`.
fn pak_beside(utoc: &Path) -> PathBuf {
    let name = utoc
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match name.strip_suffix(".utoc.disabled") {
        Some(stem) => utoc.with_file_name(format!("{stem}.pak.disabled")),
        None => utoc.with_extension("pak"),
    }
}

/// The pak beside a container: whatever non-package files the mod already carried, such as an
/// INI, written as `options` say, and empty when it carried none, as the game's own are. What the
/// game does not support in such a pak is left behind.
fn write_companion_pak(
    staged_pak: &Path,
    live_utoc: &Path,
    options: &PakOptions,
) -> Result<(), String> {
    let live_pak = pak_beside(live_utoc);
    let files = if live_pak.is_file() {
        plain::read_files(&live_pak, &|name| !plain::is_unsupported_entry(name))?
    } else {
        Vec::new()
    };
    plain::write_files(staged_pak, files, options)
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

    /// The pak beside a container is written in the compression chosen for the container, carrying
    /// what the mod's pak held but the entries the game does not support, and adding none.
    #[test]
    fn the_pak_beside_a_container_takes_its_compression() {
        let dir = scratch("companion-pak");
        let zlib = IoStoreOptions {
            compression: PackCompression::Zlib,
            ..Default::default()
        };
        plain::write_files(
            &dir.join("Mod_P.pak"),
            vec![
                ("Marvel/Config/a.ini".into(), b"a".to_vec()),
                ("chunknames".into(), b"stale".to_vec()),
                ("patched_files".into(), b"stale".to_vec()),
            ],
            &zlib.pak(),
        )
        .expect("the mod's pak");
        let staged = dir.join("staged").join("Mod_P.pak");
        write_companion_pak(&staged, &dir.join("Mod_P.utoc"), &zlib.pak()).expect("the pak beside");
        let reader = crate::pak::crypto::open_pak(&staged).expect("open");
        assert_eq!(
            format!("{:?}", reader.used_compression()),
            format!("{:?}", vec![repak::Compression::Zlib])
        );
        let files = plain::read_files(&staged, &|_| true).expect("read");
        assert_eq!(
            files,
            vec![("Marvel/Config/a.ini".to_string(), b"a".to_vec())]
        );
        // A mod whose pak carried nothing else gets an empty one.
        let bare = dir.join("bare");
        write_companion_pak(
            &bare.join("Bare_P.pak"),
            &bare.join("Bare_P.utoc"),
            &zlib.pak(),
        )
        .expect("an empty pak");
        assert!(
            plain::list_files(&bare.join("Bare_P.pak"))
                .expect("read")
                .is_empty()
        );
        std::fs::remove_dir_all(&dir).ok();
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

    /// A package taken out of a container takes its header declarations with it, and leaves the
    /// others' alone.
    #[test]
    fn a_carry_without_a_package_drops_what_names_it() {
        let gone = FPackageId::from_name("/Game/Gone");
        let carry = HeaderCarry {
            shader_map_hashes: HashMap::from([
                (gone, vec![FSHAHash::default()]),
                (
                    FPackageId::from_name("/Game/Kept"),
                    vec![FSHAHash::default()],
                ),
            ]),
            localized: vec!["/Game/Gone".into(), "/Game/Kept".into()],
            redirects: vec![
                ("/Game/Old".into(), gone),
                ("/Game/Gone".into(), FPackageId::from_name("/Game/Kept")),
                ("/Game/Older".into(), FPackageId::from_name("/Game/Kept")),
            ],
        };
        let kept = carry.without(&HashSet::from([gone]));
        assert_eq!(kept.localized, vec!["/Game/Kept".to_string()]);
        assert_eq!(
            kept.redirects,
            vec![(
                "/Game/Older".to_string(),
                FPackageId::from_name("/Game/Kept")
            )]
        );
        assert_eq!(kept.shader_map_hashes.len(), 1);
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

    /// A rewrite carries the chunks it does not change as they were stored: one package written
    /// again leaves every other chunk's blocks byte for byte, a texture's bulk data included, and
    /// they are compressed again only when asked. The packages read back the same either way.
    #[test]
    fn a_rewrite_carries_the_chunks_it_does_not_change_as_they_were_stored() {
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
            "Marvel/Content/Marvel/AbilitySystem/Cue/T_SprayPaint_Default.uasset",
        ];
        let loaded: Vec<_> = entries
            .iter()
            .map(|entry| {
                crate::asset::load_bundle(&root, &source, entry, crate::asset::AssetSource::Utoc)
                    .expect("load")
            })
            .collect();
        let package = |index: usize| PackageBytes {
            entry: entries[index].to_string(),
            asset: loaded[index].asset_file_buffer.clone(),
            exports: loaded[index].exports_file_buffer.clone(),
            bulk: loaded[index].bulk_data_buffer.clone(),
            optional_bulk: None,
            memory_mapped_bulk: None,
            shader_map_hashes: Vec::new(),
        };
        let dir = scratch("carried-as-stored");
        let utoc = dir.join("Carried.utoc");
        let zlib = IoStoreOptions {
            compression: PackCompression::Zlib,
            ..Default::default()
        };
        write_batch_into_iostore(&utoc, entries.len(), |index| Some(package(index)), &zlib)
            .expect("batch write");
        // Every chunk but the first package's, as stored, and as data.
        let first = FPackageId::from_name(
            &format!("/{}", entries[0].trim_end_matches(".uasset")).replacen(
                "/Marvel/Content/",
                "/Game/",
                1,
            ),
        );
        let others = |utoc: &Path| {
            let store = open_utoc(&utoc.to_string_lossy()).expect("open");
            let mut held: Vec<_> = store
                .chunks_all()
                .filter(|chunk| {
                    chunk.id().get_chunk_type() != EIoChunkType::ContainerHeader
                        && chunk.id().get_package_id() != first
                })
                .map(|chunk| {
                    let stored = chunk.read_stored().expect("read").expect("tiled");
                    (
                        format!("{:?}", chunk.id()),
                        stored.blocks,
                        chunk.read().expect("read"),
                    )
                })
                .collect();
            held.sort_by(|a, b| a.0.cmp(&b.0));
            held
        };
        let before = others(&utoc);
        assert!(
            before.len() >= 3,
            "two packages and the texture's bulk data: {}",
            before.len()
        );

        let report =
            write_batch_into_iostore(&utoc, 1, |_| Some(package(0)), &zlib).expect("rewrite");
        assert_eq!(report.replaced, 1);
        assert!(report.copied_blocks >= before.len(), "{report:?}");
        assert_eq!(report.recompressed_blocks, 0, "{report:?}");
        assert!(
            others(&utoc) == before,
            "the other chunks keep their blocks"
        );

        // Asked to, the rewrite compresses them again, in Oodle now, and they hold the same data.
        let report = write_batch_into_iostore(
            &utoc,
            1,
            |_| Some(package(0)),
            &IoStoreOptions {
                reencode: true,
                ..Default::default()
            },
        )
        .expect("rewrite again");
        assert_eq!(report.copied_blocks, 0, "{report:?}");
        assert!(report.recompressed_blocks >= before.len(), "{report:?}");
        let after = others(&utoc);
        assert_eq!(after.len(), before.len());
        for ((id, blocks, data), (_, old_blocks, old_data)) in after.iter().zip(&before) {
            assert!(data == old_data, "{id} holds what it did");
            assert!(blocks != old_blocks, "{id} was compressed again");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A copy is a container of its own: its id comes from its new name, and it holds what the
    /// source did less what the copy leaves out.
    #[test]
    fn a_copy_takes_the_id_of_its_new_name_and_leaves_out_what_it_drops() {
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
        let dir = scratch("copy");
        let utoc = dir.join("Original_P.utoc");
        write_batch_into_iostore(
            &utoc,
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
        .expect("write the original");

        let dropped =
            FPackageId::from_name("/Game/Marvel/AbilitySystem/1011/101111/CameraShake_101111_Hit");
        let copy = dir.join("Copy_P.utoc");
        let report = copy_container(
            &utoc,
            &copy,
            &HashSet::from([dropped]),
            &IoStoreOptions {
                compression: PackCompression::Zlib,
                ..Default::default()
            },
        )
        .expect("copy");
        assert_eq!(report.utoc, copy);
        assert_eq!(report.removed, 1);
        assert!(dir.join("Copy_P.pak").is_file(), "the copy has its pak");

        let store = open_utoc(&copy.to_string_lossy()).expect("open the copy");
        assert_eq!(
            store.container_header().expect("header").container_id,
            retoc::FIoContainerId::from_name("Copy_P"),
            "the copy mounts under its own name"
        );
        assert!(utoc_holds_entry(&copy, entries[0]).expect("list"));
        assert!(!utoc_holds_entry(&copy, entries[1]).expect("list"));
        assert!(store.package_store_entry(dropped).is_none());
        drop(store);
        assert!(
            utoc_holds_entry(&utoc, entries[1]).expect("list"),
            "the original keeps everything"
        );
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
