//! Indexes which packages import which objects across the game's containers and the installed
//! mods, so an export removal or rename can name the packages that would break.
//!
//! Two tiers come out of one walk. The container header's store entries say which packages each
//! package imports; the zen package header's import map says which objects, as
//! `(package id, public export hash)`, which is exactly what the loader resolves and so is immune
//! to case and separators. Each package's name map also says which packages it names by path,
//! which is every package it could point at softly.
//!
//! The base game and each enabled mod are indexed as layers of their own, each keyed by its
//! containers' names, sizes and modification times, so saving or switching a mod reads that mod
//! again and a game update reads the game, never both. A query takes a package's edges from the
//! copy the game loads: the first mod holding it in the game's order, or else the game's own. The
//! layers are cached in one file per install, written whole and swapped in; a write deletes the
//! cache files nothing can use any more.
//!
//! The same walk records which packages hold functions, which is all a script search needs to
//! know before it reads one.

use std::collections::HashSet;
use std::fs;
use std::io::{BufWriter, Cursor, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rayon::prelude::*;
use retoc::iostore::IoStoreTrait;
use retoc::script_objects::FPackageObjectIndex;
use retoc::version::EngineVersion;
use retoc::zen::FZenPackageHeader;
use retoc::zen_asset_conversion::get_public_export_hash;
use retoc::{EIoChunkType, FIoChunkId, FPackageId};
use serde::Serialize;

use crate::pak::containers::{
    MOUNT_POINT, base_store_files, open_base_game_paks, open_target_only,
    undecryptable_container_stems,
};
use crate::paths::paks_dir;

/// The container whose siblings make up the base game; the walk covers everything the merged
/// store admits alongside it.
const BASE_CONTAINER: &str = "pakchunk0-Windows";
const MAGIC: &[u8; 8] = b"RVLIMPX4";
const ENGINE_VERSION: EngineVersion = EngineVersion::UE5_3;
/// How many packages are read in parallel between two progress reports.
const BATCH: usize = 4096;
/// How old a half-written cache file has to be before it is taken for one an interrupted build
/// left, rather than one another build is still writing.
const LEFTOVER_AGE: Duration = Duration::from_secs(60 * 60);
/// The class a function export names, as a script import.
static FUNCTION_CLASS: LazyLock<FPackageObjectIndex> =
    LazyLock::new(|| FPackageObjectIndex::create_script_import("/Script/CoreUObject.Function"));

/// One package importing one object of another, the importer by its place in its layer. A zero
/// hash records a package dependency that no object edge already gives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Edge {
    target: u64,
    hash: u64,
    importer: u32,
}

/// One package naming another by path, which is what a soft reference to it needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Mention {
    target: u64,
    mentioner: u32,
}

/// The packages one source holds, indexed: the base game, or one enabled mod.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Layer {
    /// The mod's name, `None` for the base game.
    name: Option<String>,
    /// The containers it was read from, by name, size and modification time.
    fingerprint: Vec<u8>,
    /// Sorted package ids; a package's place here is how edges and mentions name it.
    ids: Vec<u64>,
    /// Each package's container-relative path, end to end, and where each ends.
    text: String,
    ends: Vec<u32>,
    /// Sorted, so one binary search finds every importer of an object.
    edges: Vec<Edge>,
    /// Sorted the same way, by the package named.
    mentions: Vec<Mention>,
    /// The places of the packages one of whose exports is a function, in order.
    functions: Vec<u32>,
    /// The places of the packages whose header did not read, in order.
    unread: Vec<u32>,
    /// What the walk could not read, said wherever the index is used.
    notes: Vec<String>,
}

pub struct ImportIndex {
    /// The enabled mods' layers in the order the game prefers their copies, then the game's.
    layers: Vec<Layer>,
}

/// The packages importing one object, by container-relative path.
#[derive(Debug, Clone, Serialize)]
pub struct Importers {
    pub path: String,
    pub packages: Vec<String>,
}

pub fn cache_path(game_root: &str) -> Result<PathBuf, String> {
    let dir = dirs::cache_dir()
        .ok_or("no cache directory on this system")?
        .join("rivals-toolkit");
    fs::create_dir_all(&dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    Ok(dir.join(format!("import-index-{:016x}.bin", cache_tag(game_root))))
}

/// The same install spelt with either separator, any case or a trailing slash is one index.
fn cache_tag(game_root: &str) -> u64 {
    cityhasher::hash(install_of(game_root).as_bytes())
}

/// The install an index belongs to, as its file records it.
fn install_of(game_root: &str) -> String {
    let mut normalised = paks_dir(game_root)
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    while normalised.ends_with('/') {
        normalised.pop();
    }
    normalised
}

/// The installed mods' containers the walk reads: every enabled `.utoc` under `~mods`, the copy
/// the game loads first leading, so a package several mods ship is read from the winner.
pub fn enabled_mod_containers(paks: &Path) -> Vec<PathBuf> {
    let undecryptable = undecryptable_container_stems(paks);
    let mut found: Vec<PathBuf> = walkdir::WalkDir::new(paks.join("~mods"))
        .into_iter()
        .filter_map(Result::ok)
        .map(walkdir::DirEntry::into_path)
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("utoc"))
        .filter(|path| !undecryptable.contains(&stem(path)))
        .collect();
    found.sort_by(|a, b| crate::mods::winner_order(&stem(a), &stem(b)));
    found
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string()
}

/// What the layers are read from now: the containers the base-game store admits, and each
/// enabled mod's, by name, size and modification time.
struct Sources {
    base: Vec<u8>,
    /// The enabled mods in the order the game prefers their copies: name, `.utoc`, fingerprint.
    mods: Vec<(String, PathBuf, Vec<u8>)>,
}

impl Sources {
    fn of(game_root: &str) -> Self {
        let paks = paks_dir(game_root);
        let mut base = Vec::new();
        for (path, size, modified) in base_store_files(&paks, BASE_CONTAINER) {
            stamp(&mut base, &paks, &path, size, modified);
        }
        let mods = enabled_mod_containers(&paks)
            .into_iter()
            .map(|utoc| {
                let mut fingerprint = Vec::new();
                // The data beside the table of contents too, so a mod rewritten within the same
                // tick of a coarse clock still reads as changed.
                for file in [utoc.clone(), utoc.with_extension("ucas")] {
                    let meta = fs::metadata(&file).ok();
                    let size = meta.as_ref().map_or(0, fs::Metadata::len);
                    let modified = meta.and_then(|meta| meta.modified().ok());
                    stamp(&mut fingerprint, &paks, &file, size, modified);
                }
                (stem(&utoc), utoc, fingerprint)
            })
            .collect();
        Self { base, mods }
    }
}

fn stamp(out: &mut Vec<u8>, paks: &Path, path: &Path, size: u64, modified: Option<SystemTime>) {
    let name = path
        .strip_prefix(paks)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let nanos = modified
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |since| {
            u64::try_from(since.as_nanos()).unwrap_or(u64::MAX)
        });
    out.extend_from_slice(name.as_bytes());
    out.push(0);
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&nanos.to_le_bytes());
}

/// One builder at a time, so callers asking at once share one walk rather than each reading the
/// whole game.
static BUILDING: Mutex<()> = Mutex::new(());

/// The index as the game and its mods are now: the cached layers that still match, and the ones
/// that do not read again. The walk is isolated, so this thread runs none of the waiting callers'
/// work while it holds the turn they wait for.
pub fn current(game_root: &str) -> Result<ImportIndex, String> {
    current_with(game_root, &|_, _| {})
}

/// [`current`] where an index has been built before, `None` where none has, so a caller can name
/// importers whenever the user has asked for an index without making the first walk itself.
pub fn current_if_built(game_root: &str) -> Result<Option<ImportIndex>, String> {
    if !cache_path(game_root)?.is_file() {
        return Ok(None);
    }
    current(game_root).map(Some)
}

/// [`current`], telling `progress` how many packages it reads again out of how many.
pub fn current_with(
    game_root: &str,
    progress: &(dyn Fn(usize, usize) + Sync),
) -> Result<ImportIndex, String> {
    crate::parallel::isolated(|| {
        update(game_root, false, &mut |done, total| progress(done, total))
    })?
}

/// [`current`], telling `progress` how many packages it has read out of how many, on the caller's
/// own thread.
pub fn refresh(
    game_root: &str,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<ImportIndex, String> {
    update(game_root, false, progress)
}

/// Reads every layer again, cached or not.
pub fn build(
    game_root: &str,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<ImportIndex, String> {
    update(game_root, true, progress)
}

/// A layer as the update has it: kept from the cache, or to be read from its containers.
enum Slot {
    Kept(Layer),
    Walk {
        name: Option<String>,
        utoc: Option<PathBuf>,
        fingerprint: Vec<u8>,
    },
}

/// Which of `held` still match `sources`, in the order the index takes them, and whether the
/// cache holds anything else, which the update drops.
fn plan_layers(mut held: Vec<Layer>, sources: &Sources) -> (Vec<Slot>, bool) {
    let mut take = |name: Option<&str>, fingerprint: &[u8]| {
        let at = held
            .iter()
            .position(|layer| layer.name.as_deref() == name && layer.fingerprint == fingerprint)?;
        Some(held.swap_remove(at))
    };
    let mut slots: Vec<Slot> = sources
        .mods
        .iter()
        .map(
            |(name, utoc, fingerprint)| match take(Some(name), fingerprint) {
                Some(layer) => Slot::Kept(layer),
                None => Slot::Walk {
                    name: Some(name.clone()),
                    utoc: Some(utoc.clone()),
                    fingerprint: fingerprint.clone(),
                },
            },
        )
        .collect();
    slots.push(match take(None, &sources.base) {
        Some(layer) => Slot::Kept(layer),
        None => Slot::Walk {
            name: None,
            utoc: None,
            fingerprint: sources.base.clone(),
        },
    });
    (slots, !held.is_empty())
}

fn update(
    game_root: &str,
    rebuild: bool,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<ImportIndex, String> {
    let _turn = BUILDING.lock().unwrap_or_else(PoisonError::into_inner);
    let sources = Sources::of(game_root);
    let held = if rebuild {
        Vec::new()
    } else {
        read_cache(game_root)?.unwrap_or_default()
    };
    let (slots, dropped) = plan_layers(held, &sources);
    let paks = paks_dir(game_root);
    let mut walks = Vec::new();
    for (at, slot) in slots.iter().enumerate() {
        let Slot::Walk { name, utoc, .. } = slot else {
            continue;
        };
        let store: Result<Arc<dyn IoStoreTrait>, String> = match (name, utoc) {
            (Some(name), Some(utoc)) => open_target_only(&paks, utoc, name).map(Arc::from),
            _ => open_base_game_paks(&paks, BASE_CONTAINER),
        };
        walks.push((at, store));
    }
    let listed: Vec<(usize, Opened<'_>)> = walks
        .iter()
        .map(|(at, store)| {
            let listed = match store {
                Ok(store) => Ok((store, list_packages(store.as_ref()))),
                Err(why) => Err(why.clone()),
            };
            (*at, listed)
        })
        .collect();
    let total = listed
        .iter()
        .map(|(_, listed)| listed.as_ref().map_or(0, |(_, packages)| packages.len()))
        .sum();
    let mut done = 0;
    let mut walked = Vec::new();
    for (at, listed) in listed {
        let Slot::Walk {
            name, fingerprint, ..
        } = &slots[at]
        else {
            continue;
        };
        let layer = match listed {
            Ok((store, packages)) => {
                let layer = index_layer(
                    store.as_ref(),
                    packages,
                    name.clone(),
                    fingerprint.clone(),
                    &mut |read| progress(done + read, total),
                )?;
                done += layer.ids.len();
                layer
            }
            // The game's own containers have to open; a mod that does not is said, not fatal.
            Err(why) if name.is_none() => return Err(why),
            Err(why) => Layer {
                notes: vec![format!(
                    "{} could not be read, so nothing it imports is listed: {why}",
                    name.as_deref().unwrap_or_default()
                )],
                ..Layer::empty(name.clone(), fingerprint.clone())
            },
        };
        walked.push((at, layer));
    }
    let changed = dropped || !walked.is_empty();
    let mut layers: Vec<Option<Layer>> = slots
        .into_iter()
        .map(|slot| match slot {
            Slot::Kept(layer) => Some(layer),
            Slot::Walk { .. } => None,
        })
        .collect();
    for (at, layer) in walked {
        layers[at] = Some(layer);
    }
    let layers: Vec<Layer> = layers.into_iter().flatten().collect();
    if changed {
        let cache = cache_path(game_root)?;
        write_cache(&cache, &install_of(game_root), &layers)?;
        if let Some(dir) = cache.parent() {
            prune(dir, &cache, LEFTOVER_AGE);
        }
    }
    Ok(ImportIndex { layers })
}

/// A layer's store with the packages it lists, or why it would not open.
type Opened<'a> = Result<(&'a Arc<dyn IoStoreTrait>, Vec<Listed<'a>>), String>;

/// One package to read: its id, its container-relative path, the versions the header listing it
/// and its table of contents were written in, and the container holding its chunk.
type Listed<'a> = (
    FPackageId,
    String,
    Option<retoc::EIoStoreTocVersion>,
    Option<retoc::container_header::EIoContainerHeaderVersion>,
    &'a dyn IoStoreTrait,
);

/// Every package `store` holds, each once from its highest-priority copy, so a patched package
/// is read from its patched header. Sorted by id, which is the order a layer keeps them in.
///
/// A patch container's header lists nearly every package while holding the chunks of only those
/// it patched, so each chunk is found once, here, in the container holding it, rather than
/// searched for through every container on each read.
fn list_packages(store: &dyn IoStoreTrait) -> Vec<Listed<'_>> {
    let mut seen = HashSet::new();
    let found: Vec<_> = store
        .packages_all()
        .filter(|pkg| seen.insert(pkg.id().0))
        .map(|pkg| {
            let container = pkg.container();
            (
                pkg.id(),
                container.container_file_version(),
                container.container_header_version(),
            )
        })
        .collect();
    let mut holders: Vec<&dyn IoStoreTrait> = store.child_containers().collect();
    if holders.is_empty() {
        holders.push(store);
    }
    let mut listed: Vec<Listed<'_>> = found
        .into_par_iter()
        .filter_map(|(id, toc_version, header_version)| {
            let chunk = FIoChunkId::from_package_id(id, 0, EIoChunkType::ExportBundleData);
            let holder = *holders.iter().find(|holder| holder.has_chunk_id(chunk))?;
            let path = holder.chunk_path(chunk)?;
            let path = path.strip_prefix(MOUNT_POINT).unwrap_or(&path).to_string();
            Some((id, path, toc_version, header_version, holder))
        })
        .collect();
    listed.sort_unstable_by_key(|(id, ..)| id.0);
    listed
}

/// What one package's header says.
struct Gathered {
    /// `(package, public export hash)` per imported object, and a zero hash for a package it
    /// depends on without importing an object of.
    objects: Vec<(u64, u64)>,
    /// The packages it names by path.
    named: Vec<u64>,
    read: bool,
    /// One of its exports is a function.
    functions: bool,
}

fn read_package(store: &dyn IoStoreTrait, package: &Listed<'_>) -> Gathered {
    let (id, _, toc_version, header_version, holder) = package;
    let entry = store.package_store_entry(*id);
    let depends: Vec<u64> = entry
        .iter()
        .flat_map(|entry| entry.imported_packages.iter().map(|p| p.0))
        .collect();
    let mut objects = Vec::new();
    let mut named = Vec::new();
    let mut read = false;
    let mut functions = false;
    if let (Some(toc_version), Some(header_version)) = (toc_version, header_version)
        && let Some(header) = read_header(*holder, *id, entry, *toc_version, *header_version)
    {
        read = true;
        functions = header
            .export_map
            .iter()
            .any(|export| export.class_index == *FUNCTION_CLASS);
        for name in header.name_map.raw_names() {
            if !name.starts_with('/') || name.starts_with("/Script/") {
                continue;
            }
            let target = package_id(name.split(['.', ':']).next().unwrap_or(name));
            if target != id.0 {
                named.push(target);
            }
        }
        // The import map names each imported object by package and public export hash.
        for import in &header.import_map {
            let Some(reference) = import.package_import() else {
                continue;
            };
            let package = header
                .imported_packages
                .get(reference.imported_package_index as usize);
            let hash = header
                .imported_public_export_hashes
                .get(reference.imported_public_export_hash_index as usize);
            if let (Some(package), Some(hash)) = (package, hash) {
                objects.push((package.0, *hash));
            }
        }
    }
    named.sort_unstable();
    named.dedup();
    Gathered {
        objects: with_dependencies(objects, &depends),
        named,
        read,
        functions,
    }
}

/// A package's zen header, read from as much of the package as the header takes rather than its
/// exports, which dwarf it. A header that will not read from its prefix is read from the whole
/// package before it counts as unread.
fn read_header(
    store: &dyn IoStoreTrait,
    id: FPackageId,
    entry: Option<retoc::container_header::StoreEntry>,
    toc_version: retoc::EIoStoreTocVersion,
    header_version: retoc::container_header::EIoContainerHeaderVersion,
) -> Option<FZenPackageHeader> {
    let chunk = FIoChunkId::from_package_id(id, 0, EIoChunkType::ExportBundleData);
    let parse = |data: &[u8], entry| {
        FZenPackageHeader::deserialize(
            &mut Cursor::new(data),
            entry,
            toc_version,
            header_version,
            Some(ENGINE_VERSION.package_file_version()),
        )
        .ok()
    };
    let data = retoc::zen::read_header_bytes(store, chunk, header_version).ok()?;
    parse(&data, entry.clone()).or_else(|| parse(&store.read(chunk).ok()?, entry))
}

/// `objects` with a zero-hash edge for each package in `depends` none of them is in: a package's
/// importers are found through its objects' edges, so a dependency those already give is not kept
/// twice.
fn with_dependencies(mut objects: Vec<(u64, u64)>, depends: &[u64]) -> Vec<(u64, u64)> {
    objects.sort_unstable();
    objects.dedup();
    for package in depends {
        let from = objects.partition_point(|(target, _)| target < package);
        if objects
            .get(from)
            .is_none_or(|(target, _)| target != package)
        {
            objects.push((*package, 0));
        }
    }
    objects.sort_unstable();
    objects.dedup();
    objects
}

/// Reads `packages` from `store` in parallel, in batches, and indexes them as one layer.
/// `progress` is told how many have been read.
fn index_layer(
    store: &dyn IoStoreTrait,
    packages: Vec<Listed<'_>>,
    name: Option<String>,
    fingerprint: Vec<u8>,
    progress: &mut dyn FnMut(usize),
) -> Result<Layer, String> {
    let mut layer = Layer::empty(name, fingerprint);
    layer.ids = packages.iter().map(|(id, ..)| id.0).collect();
    layer.ends.reserve(packages.len());
    for (_, path, ..) in &packages {
        layer.text.push_str(path);
        layer.ends.push(
            u32::try_from(layer.text.len())
                .map_err(|_| "the package paths outgrow the index's 4 GB of text".to_string())?,
        );
    }
    let mut unread = 0usize;
    for (batch, chunk) in packages.chunks(BATCH).enumerate() {
        let read: Vec<Gathered> = chunk
            .par_iter()
            .map(|package| read_package(store, package))
            .collect();
        for (offset, gathered) in read.into_iter().enumerate() {
            let importer = u32::try_from(batch * BATCH + offset)
                .map_err(|_| "more packages than the index can number".to_string())?;
            if gathered.functions {
                layer.functions.push(importer);
            }
            if !gathered.read {
                unread += 1;
                layer.unread.push(importer);
            }
            layer
                .edges
                .extend(gathered.objects.into_iter().map(|(target, hash)| Edge {
                    target,
                    hash,
                    importer,
                }));
            layer
                .mentions
                .extend(gathered.named.into_iter().map(|target| Mention {
                    target,
                    mentioner: importer,
                }));
        }
        progress(batch * BATCH + chunk.len());
    }
    layer.edges.sort_unstable();
    layer.mentions.sort_unstable();
    if unread > 0 {
        layer.notes.push(format!(
            "{unread} of {} package(s){} had a header that did not read, so the objects they \
             import are not listed",
            layer.ids.len(),
            layer
                .name
                .as_deref()
                .map_or_else(String::new, |name| format!(" in {name}"))
        ));
    }
    Ok(layer)
}

impl Layer {
    fn empty(name: Option<String>, fingerprint: Vec<u8>) -> Self {
        Self {
            name,
            fingerprint,
            ids: Vec::new(),
            text: String::new(),
            ends: Vec::new(),
            edges: Vec::new(),
            mentions: Vec::new(),
            functions: Vec::new(),
            unread: Vec::new(),
            notes: Vec::new(),
        }
    }

    fn holds(&self, id: u64) -> bool {
        self.ids.binary_search(&id).is_ok()
    }

    /// A package by its place here, as the index lists it: a mod's with the mod's name after it.
    fn shown(&self, at: u32) -> String {
        let at = at as usize;
        let start = at
            .checked_sub(1)
            .map_or(0, |before| self.ends[before] as usize);
        let path = self
            .text
            .get(start..self.ends[at] as usize)
            .unwrap_or_default();
        match &self.name {
            Some(name) => format!("{path} (in {name})"),
            None => path.to_string(),
        }
    }

    /// The importers of every edge whose `(target, hash)` lies in the inclusive range.
    fn between(&self, from: (u64, u64), to: (u64, u64)) -> impl Iterator<Item = u32> + '_ {
        let key = |edge: &Edge| (edge.target, edge.hash);
        let start = self.edges.partition_point(|edge| key(edge) < from);
        let end = self.edges.partition_point(|edge| key(edge) <= to);
        self.edges[start..end.max(start)]
            .iter()
            .map(|edge| edge.importer)
    }

    fn naming(&self, target: u64) -> impl Iterator<Item = u32> + '_ {
        let start = self.mentions.partition_point(|m| m.target < target);
        let end = self.mentions.partition_point(|m| m.target <= target);
        self.mentions[start..end.max(start)]
            .iter()
            .map(|m| m.mentioner)
    }
}

/// The cached index as it is, without reading anything again, or `None` when there is none yet or
/// the file is not one this build wrote. A layer for a mod since switched off is left out, and one
/// gone stale is kept.
pub fn load(game_root: &str) -> Result<Option<ImportIndex>, String> {
    let Some(held) = read_cache(game_root)? else {
        return Ok(None);
    };
    Ok(arrange(held, &Sources::of(game_root)).map(|layers| ImportIndex { layers }))
}

/// The cached layers, or `None` when there are none or the file is not one this build wrote.
fn read_cache(game_root: &str) -> Result<Option<Vec<Layer>>, String> {
    let path = cache_path(game_root)?;
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("Could not read {}: {e}", path.display())),
    };
    Ok(decode(&bytes).map(|(_, layers)| layers))
}

/// The enabled mods' layers in the order the game prefers them, then the game's; `None` without
/// a layer for the game.
fn arrange(mut held: Vec<Layer>, sources: &Sources) -> Option<Vec<Layer>> {
    let base = held.swap_remove(held.iter().position(|layer| layer.name.is_none())?);
    let mut layers = Vec::new();
    for (name, _, fingerprint) in &sources.mods {
        let at = held
            .iter()
            .position(|layer| {
                layer.name.as_deref() == Some(name) && &layer.fingerprint == fingerprint
            })
            .or_else(|| {
                held.iter()
                    .position(|layer| layer.name.as_deref() == Some(name))
            });
        if let Some(at) = at {
            layers.push(held.swap_remove(at));
        }
    }
    layers.push(base);
    Some(layers)
}

/// `FPackageId::FromName`: CityHash64 of the lowercased UTF-16 package name.
fn package_id(package: &str) -> u64 {
    cityhasher::hash(
        package
            .to_ascii_lowercase()
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<u8>>(),
    )
}

impl ImportIndex {
    /// What the walk could not read, so the packages it lists may be short.
    pub fn notes(&self) -> Vec<String> {
        self.layers
            .iter()
            .flat_map(|layer| layer.notes.iter().cloned())
            .collect()
    }

    /// Each package `found` gives, from the copy the game loads: a layer's package counts unless
    /// a mod the game prefers holds a copy of its own.
    fn loaded<'a, I: Iterator<Item = u32> + 'a>(
        &'a self,
        found: impl Fn(&'a Layer) -> I,
    ) -> Vec<String> {
        let mut packages = Vec::new();
        for (rank, layer) in self.layers.iter().enumerate() {
            for at in found(layer) {
                let id = layer.ids[at as usize];
                if !self.layers[..rank].iter().any(|above| above.holds(id)) {
                    packages.push(layer.shown(at));
                }
            }
        }
        packages.sort();
        packages.dedup();
        packages
    }

    /// The packages importing `path`: an object in UE's dotted form (`/Pkg/Path.Object:Sub`), or a
    /// bare package path, in which case every package importing anything from it counts.
    pub fn importers_of(&self, path: &str) -> Importers {
        let text = path.trim();
        let (package, object) = match text.split_once('.') {
            Some((package, object)) => (package, Some(object)),
            None => (text, None),
        };
        let target = package_id(package);
        let (from, to) = match object {
            // Zen hashes the package-relative path lowercased with `/` between the objects.
            Some(object) => {
                let hash = get_public_export_hash(&object.replace(':', "/").to_lowercase());
                ((target, hash), (target, hash))
            }
            None => ((target, 0), (target, u64::MAX)),
        };
        Importers {
            path: text.to_string(),
            packages: self.loaded(|layer| layer.between(from, to)),
        }
    }

    /// The packages naming `package` by path, which is every package that could point at it or at
    /// an object in it softly. A candidate list: naming it is needed for a soft reference, not
    /// proof of one.
    pub fn mentions_of(&self, package: &str) -> Vec<String> {
        let target = package_id(package.split(['.', ':']).next().unwrap_or(package).trim());
        self.loaded(|layer| layer.naming(target))
    }

    /// Whether a script search has to read package `id` from the layer named `layer`, a mod's
    /// name or `None` for the game's: one of its exports is a function, its header did not read,
    /// or the index does not hold it.
    pub fn may_hold_functions(&self, layer: Option<&str>, id: u64) -> bool {
        let Some(held) = self
            .layers
            .iter()
            .find(|held| held.name.as_deref() == layer)
        else {
            return true;
        };
        let Some(at) = held
            .ids
            .binary_search(&id)
            .ok()
            .and_then(|at| u32::try_from(at).ok())
        else {
            return true;
        };
        held.functions.binary_search(&at).is_ok() || held.unread.binary_search(&at).is_ok()
    }
}

/// Writes the layers to `path` through a file of its own beside it, swapped in once whole, so a
/// reader never meets half an index and an interrupted write leaves the old one.
fn write_cache(path: &Path, install: &str, layers: &[Layer]) -> Result<(), String> {
    let temp = path.with_extension(format!("{}.tmp", std::process::id()));
    let written = fs::File::create(&temp).and_then(|file| {
        let mut out = BufWriter::new(file);
        encode(&mut out, install, layers)?;
        out.into_inner()
            .map_err(std::io::IntoInnerError::into_error)?;
        Ok(())
    });
    if let Err(e) = written.and_then(|()| fs::rename(&temp, path)) {
        let _ = fs::remove_file(&temp);
        return Err(format!("Could not write {}: {e}", path.display()));
    }
    Ok(())
}

/// Deletes the cache files in `dir` nothing can use: one an older build wrote, one whose install
/// is gone, and one an interrupted write left, once it is older than `leftover_age`. Another
/// install's index is kept.
fn prune(dir: &Path, keep: &Path, leftover_age: Duration) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.starts_with("import-index-") || path == keep {
            continue;
        }
        let unusable = if name.ends_with(".tmp") {
            entry
                .metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age >= leftover_age)
        } else if name.ends_with(".bin") {
            !recorded_install(&path).is_some_and(|install| Path::new(&install).is_dir())
        } else {
            false
        };
        if unusable {
            let _ = fs::remove_file(&path);
        }
    }
}

/// The install a cache file records, when this build wrote it.
fn recorded_install(path: &Path) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let mut head = [0u8; 12];
    file.read_exact(&mut head).ok()?;
    if head[..8] != *MAGIC {
        return None;
    }
    let len = u32::from_le_bytes(head[8..12].try_into().ok()?) as usize;
    let mut install = vec![0u8; len.min(4096)];
    file.read_exact(&mut install).ok()?;
    String::from_utf8(install).ok()
}

fn encode(out: &mut impl Write, install: &str, layers: &[Layer]) -> std::io::Result<()> {
    let len = |n: usize| {
        u32::try_from(n).map_err(|_| std::io::Error::other("too large for the index format"))
    };
    let bytes = |out: &mut dyn Write, data: &[u8]| -> std::io::Result<()> {
        out.write_all(&len(data.len())?.to_le_bytes())?;
        out.write_all(data)
    };
    out.write_all(MAGIC)?;
    bytes(out, install.as_bytes())?;
    out.write_all(&len(layers.len())?.to_le_bytes())?;
    for layer in layers {
        bytes(out, layer.name.as_deref().unwrap_or_default().as_bytes())?;
        out.write_all(&[u8::from(layer.name.is_some())])?;
        bytes(out, &layer.fingerprint)?;
        out.write_all(&len(layer.notes.len())?.to_le_bytes())?;
        for note in &layer.notes {
            bytes(out, note.as_bytes())?;
        }
        out.write_all(&len(layer.ids.len())?.to_le_bytes())?;
        for id in &layer.ids {
            out.write_all(&id.to_le_bytes())?;
        }
        for end in &layer.ends {
            out.write_all(&end.to_le_bytes())?;
        }
        bytes(out, layer.text.as_bytes())?;
        out.write_all(&(layer.edges.len() as u64).to_le_bytes())?;
        for edge in &layer.edges {
            out.write_all(&edge.target.to_le_bytes())?;
            out.write_all(&edge.hash.to_le_bytes())?;
            out.write_all(&edge.importer.to_le_bytes())?;
        }
        out.write_all(&(layer.mentions.len() as u64).to_le_bytes())?;
        for mention in &layer.mentions {
            out.write_all(&mention.target.to_le_bytes())?;
            out.write_all(&mention.mentioner.to_le_bytes())?;
        }
        for places in [&layer.functions, &layer.unread] {
            out.write_all(&len(places.len())?.to_le_bytes())?;
            for at in places {
                out.write_all(&at.to_le_bytes())?;
            }
        }
    }
    Ok(())
}

/// The install and layers `bytes` hold, or `None` for a file this build did not write. A layer
/// whose edges name a package it does not hold is refused, so a query never indexes past one.
fn decode(bytes: &[u8]) -> Option<(String, Vec<Layer>)> {
    let mut reader = Reader { bytes, at: 0 };
    if reader.take(MAGIC.len())? != MAGIC {
        return None;
    }
    let install = reader.string()?;
    let mut layers = Vec::new();
    for _ in 0..reader.u32()? {
        let name = reader.string()?;
        let name = (reader.take(1)?[0] == 1).then_some(name);
        let fingerprint = reader.bytes()?.to_vec();
        let mut notes = Vec::new();
        for _ in 0..reader.u32()? {
            notes.push(reader.string()?);
        }
        let count = reader.u32()? as usize;
        let mut ids = Vec::with_capacity(count.min(1 << 24));
        for _ in 0..count {
            ids.push(reader.u64()?);
        }
        let mut ends = Vec::with_capacity(count.min(1 << 24));
        for _ in 0..count {
            ends.push(reader.u32()?);
        }
        let text = reader.string()?;
        if ends.windows(2).any(|pair| pair[0] > pair[1])
            || ends.last().is_some_and(|end| *end as usize > text.len())
        {
            return None;
        }
        let edges_count = usize::try_from(reader.u64()?).ok()?;
        let mut edges = Vec::with_capacity(edges_count.min(1 << 24));
        for _ in 0..edges_count {
            edges.push(Edge {
                target: reader.u64()?,
                hash: reader.u64()?,
                importer: reader.u32()?,
            });
        }
        let mentions_count = usize::try_from(reader.u64()?).ok()?;
        let mut mentions = Vec::with_capacity(mentions_count.min(1 << 24));
        for _ in 0..mentions_count {
            mentions.push(Mention {
                target: reader.u64()?,
                mentioner: reader.u32()?,
            });
        }
        let mut places = || -> Option<Vec<u32>> {
            let held = reader.u32()? as usize;
            let mut out = Vec::with_capacity(held.min(1 << 24));
            for _ in 0..held {
                out.push(reader.u32()?);
            }
            Some(out)
        };
        let functions = places()?;
        let unread = places()?;
        let past = |at: u32| at as usize >= count;
        let ordered = |list: &[u32]| {
            list.windows(2).all(|pair| pair[0] < pair[1]) && list.last().is_none_or(|at| !past(*at))
        };
        if edges.iter().any(|edge| past(edge.importer))
            || mentions.iter().any(|mention| past(mention.mentioner))
            || !ordered(&functions)
            || !ordered(&unread)
        {
            return None;
        }
        layers.push(Layer {
            name,
            fingerprint,
            ids,
            text,
            ends,
            edges,
            mentions,
            functions,
            unread,
            notes,
        });
    }
    (reader.at == bytes.len()).then_some((install, layers))
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let slice = self.bytes.get(self.at..self.at.checked_add(count)?)?;
        self.at += count;
        Some(slice)
    }

    fn u32(&mut self) -> Option<u32> {
        self.take(4)?.try_into().ok().map(u32::from_le_bytes)
    }

    fn u64(&mut self) -> Option<u64> {
        self.take(8)?.try_into().ok().map(u64::from_le_bytes)
    }

    fn bytes(&mut self) -> Option<&'a [u8]> {
        let len = self.u32()? as usize;
        self.take(len)
    }

    fn string(&mut self) -> Option<String> {
        std::str::from_utf8(self.bytes()?).ok().map(str::to_string)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    /// A package as a test layer holds it: its path, its imports as `(package, hash)`, and the
    /// packages it names.
    type Spec<'a> = (&'a str, &'a [(&'a str, u64)], &'a [&'a str]);

    /// A layer of `packages`.
    fn layer(name: Option<&str>, packages: &[Spec<'_>]) -> Layer {
        let mut sorted: Vec<(u64, &Spec<'_>)> = packages
            .iter()
            .map(|spec| (package_id(&game_name(spec.0)), spec))
            .collect();
        sorted.sort_by_key(|(id, _)| *id);
        let mut layer = Layer::empty(name.map(str::to_string), b"fp".to_vec());
        for (at, (id, (path, imports, names))) in sorted.iter().enumerate() {
            layer.ids.push(*id);
            layer.text.push_str(path);
            layer.ends.push(layer.text.len() as u32);
            for (package, hash) in *imports {
                layer.edges.push(Edge {
                    target: package_id(package),
                    hash: *hash,
                    importer: at as u32,
                });
            }
            for named in *names {
                layer.mentions.push(Mention {
                    target: package_id(named),
                    mentioner: at as u32,
                });
            }
        }
        layer.edges.sort_unstable();
        layer.mentions.sort_unstable();
        layer
    }

    /// `Marvel/Content/B.uasset` as the package `/Game/B` its id is made from.
    fn game_name(path: &str) -> String {
        let stem = path.trim_end_matches(".uasset");
        format!("/Game/{}", stem.trim_start_matches("Marvel/Content/"))
    }

    fn sub() -> u64 {
        get_public_export_hash("default__x_c/sub")
    }

    fn index() -> ImportIndex {
        let other = get_public_export_hash("default__x_c");
        let base = layer(
            None,
            &[
                (
                    "Marvel/Content/B.uasset",
                    &[("/Game/A", 0), ("/Game/A", sub())],
                    &[],
                ),
                ("Marvel/Content/D.uasset", &[], &[]),
            ],
        );
        let in_mod = layer(
            Some("SomeMod"),
            &[(
                "Marvel/Content/C.uasset",
                &[("/Game/A", sub()), ("/Game/A", other)],
                &["/Game/A"],
            )],
        );
        ImportIndex {
            layers: vec![in_mod, base],
        }
    }

    /// The lookup hashes the object the way zen does: lowercased, `:` turned into `/`, and the
    /// package part left to the package id.
    #[test]
    fn an_object_is_found_by_its_dotted_path_whatever_the_case() {
        let index = index();
        let found = index.importers_of("/Game/A.Default__X_C:Sub");
        assert_eq!(
            found.packages,
            vec![
                "Marvel/Content/B.uasset",
                "Marvel/Content/C.uasset (in SomeMod)"
            ]
        );
        let found = index.importers_of("/game/a.DEFAULT__X_C");
        assert_eq!(found.packages, vec!["Marvel/Content/C.uasset (in SomeMod)"]);
        assert!(index.importers_of("/Game/A.Nothing").packages.is_empty());
    }

    #[test]
    fn a_bare_package_path_collects_every_importer_of_the_package() {
        let found = index().importers_of("/Game/A");
        assert_eq!(
            found.packages,
            vec![
                "Marvel/Content/B.uasset",
                "Marvel/Content/C.uasset (in SomeMod)"
            ]
        );
        assert!(index().importers_of("/Game/B").packages.is_empty());
    }

    /// A package a mod ships is read from the mod's copy: the game's copy's imports stop
    /// counting, and of two mods holding one package, the one the game prefers wins.
    #[test]
    fn the_copy_the_game_loads_is_the_one_that_counts() {
        let base = layer(
            None,
            &[
                ("Marvel/Content/B.uasset", &[("/Game/A", sub())], &[]),
                (
                    "Marvel/Content/C.uasset",
                    &[("/Game/A", sub())],
                    &["/Game/A"],
                ),
            ],
        );
        let winner = layer(Some("Winner"), &[("Marvel/Content/B.uasset", &[], &[])]);
        let loser = layer(
            Some("Loser"),
            &[
                ("Marvel/Content/B.uasset", &[("/Game/A", sub())], &[]),
                ("Marvel/Content/E.uasset", &[("/Game/A", sub())], &[]),
            ],
        );
        let index = ImportIndex {
            layers: vec![winner, loser, base],
        };
        assert_eq!(
            index.importers_of("/Game/A.Default__X_C:Sub").packages,
            vec![
                "Marvel/Content/C.uasset",
                "Marvel/Content/E.uasset (in Loser)"
            ]
        );
        assert_eq!(
            index.mentions_of("/Game/A"),
            vec!["Marvel/Content/C.uasset"]
        );
    }

    /// A dependency an object edge already gives is not stored again; one with no object edge
    /// still is, so a bare package query finds it.
    #[test]
    fn a_dependency_is_kept_only_where_no_object_edge_gives_it() {
        assert_eq!(
            with_dependencies(vec![(5, 9), (5, 9), (3, 1)], &[5, 7]),
            vec![(3, 1), (5, 9), (7, 0)]
        );
    }

    #[test]
    fn the_cache_is_named_the_same_however_the_install_path_is_spelt() {
        assert_eq!(
            cache_tag("C:/Games/Marvel Rivals"),
            cache_tag(r"c:\games\MARVEL RIVALS\")
        );
        assert_ne!(cache_tag("C:/Games/A"), cache_tag("C:/Games/B"));
    }

    #[test]
    fn the_cache_encoding_round_trips_and_rejects_other_files() {
        let mut layers = index().layers;
        layers[0].notes.push("a note".into());
        let mut bytes = Vec::new();
        encode(&mut bytes, "c:/game/paks", &layers).unwrap();
        let (install, back) = decode(&bytes).expect("decode");
        assert_eq!(install, "c:/game/paks");
        assert_eq!(back, layers);
        assert!(decode(b"not an index").is_none());
        assert!(decode(&bytes[..bytes.len() - 5]).is_none());
        let mut longer = bytes.clone();
        longer.push(0);
        assert!(decode(&longer).is_none(), "trailing bytes are not an index");
    }

    /// A package named in another's names is a candidate for a soft reference, by package or by
    /// any object in it.
    #[test]
    fn a_package_named_in_another_is_a_soft_reference_candidate() {
        let index = index();
        assert_eq!(
            index.mentions_of("/Game/A"),
            vec!["Marvel/Content/C.uasset (in SomeMod)"]
        );
        assert_eq!(
            index.mentions_of("/game/a.Default__X_C:Sub"),
            vec!["Marvel/Content/C.uasset (in SomeMod)"]
        );
        assert!(index.mentions_of("/Game/B").is_empty());
    }

    /// An update keeps the layers whose containers are as they were, reads the rest, and drops
    /// the layer of a mod no longer enabled.
    #[test]
    fn an_update_reads_only_what_changed() {
        let mut base = Layer::empty(None, b"game".to_vec());
        base.ids.push(1);
        let same = Layer::empty(Some("Same".into()), b"same".to_vec());
        let edited = Layer::empty(Some("Edited".into()), b"before".to_vec());
        let gone = Layer::empty(Some("Gone".into()), b"gone".to_vec());
        let sources = Sources {
            base: b"game".to_vec(),
            mods: vec![
                (
                    "Edited".into(),
                    PathBuf::from("Edited.utoc"),
                    b"after".to_vec(),
                ),
                ("Same".into(), PathBuf::from("Same.utoc"), b"same".to_vec()),
                ("New".into(), PathBuf::from("New.utoc"), b"new".to_vec()),
            ],
        };
        let (slots, dropped) = plan_layers(vec![gone, base, same, edited], &sources);
        assert!(dropped, "the switched-off mod's layer goes");
        let plan: Vec<(Option<String>, bool)> = slots
            .iter()
            .map(|slot| match slot {
                Slot::Kept(layer) => (layer.name.clone(), true),
                Slot::Walk { name, .. } => (name.clone(), false),
            })
            .collect();
        assert_eq!(
            plan,
            vec![
                (Some("Edited".into()), false),
                (Some("Same".into()), true),
                (Some("New".into()), false),
                (None, true),
            ]
        );
    }

    /// A load leaves out a mod since switched off and puts the rest in the game's order.
    #[test]
    fn a_load_follows_the_mods_as_they_are_now() {
        let base = Layer::empty(None, b"game".to_vec());
        let first = Layer::empty(Some("First".into()), b"1".to_vec());
        let second = Layer::empty(Some("Second".into()), b"2".to_vec());
        let off = Layer::empty(Some("Off".into()), b"3".to_vec());
        let sources = Sources {
            base: b"game".to_vec(),
            mods: vec![
                ("Second".into(), PathBuf::new(), b"2".to_vec()),
                ("First".into(), PathBuf::new(), b"changed".to_vec()),
            ],
        };
        let layers = arrange(vec![first, base, off, second], &sources).expect("a game layer");
        let names: Vec<Option<&str>> = layers.iter().map(|l| l.name.as_deref()).collect();
        assert_eq!(names, vec![Some("Second"), Some("First"), None]);
        assert!(arrange(Vec::new(), &sources).is_none());
    }

    fn scratch(what: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        std::env::temp_dir().join(format!(
            "rivals-index-{what}-{}-{stamp}",
            std::process::id()
        ))
    }

    /// A mod enabled or rewritten under `~mods`, or a game container anywhere the store reads,
    /// changes what the index reads from; a container the walk leaves out does not.
    #[test]
    fn the_sources_follow_the_mods_and_the_game_and_ignore_what_the_walk_skips() {
        let root = scratch("fp");
        let paks = paks_dir(&root.to_string_lossy());
        let write = |path: &Path, bytes: usize| {
            fs::create_dir_all(path.parent().expect("parent")).expect("dir");
            let mut header = vec![0u8; bytes.max(0x90)];
            header[0..16].copy_from_slice(b"-==--==--==--==-");
            fs::write(path, header).expect("write");
        };
        let game_root = root.to_string_lossy().to_string();
        write(&paks.join("pakchunk0-Windows.utoc"), 0x90);
        let base = Sources::of(&game_root);
        assert!(base.mods.is_empty());

        write(&paks.join("Stray_9999999_P.utoc"), 0x90);
        assert_eq!(Sources::of(&game_root).base, base.base);

        write(&paks.join("Patches").join("Patch_-Windows_P.utoc"), 0x90);
        let patched = Sources::of(&game_root).base;
        assert_ne!(
            patched, base.base,
            "a container in a folder the store reads counts"
        );

        write(&paks.join("~mods").join("SomeMod_9999999_P.utoc"), 0x90);
        let with_mod = Sources::of(&game_root);
        assert_eq!(with_mod.base, patched, "a mod is no game container");
        assert_eq!(with_mod.mods.len(), 1);
        write(&paks.join("~mods").join("SomeMod_9999999_P.ucas"), 0x200);
        assert_ne!(Sources::of(&game_root).mods[0].2, with_mod.mods[0].2);

        write(
            &paks.join("~mods").join("Off_9999999_P.utoc.disabled"),
            0x90,
        );
        assert_eq!(Sources::of(&game_root).mods.len(), 1);
        let _ = fs::remove_dir_all(&root);
    }

    /// A write deletes the cache files nothing can use and keeps another install's.
    #[test]
    fn a_write_prunes_what_nothing_can_use() {
        let dir = scratch("prune");
        let install = dir.join("Paks");
        fs::create_dir_all(&install).unwrap();
        let file = |name: &str| dir.join(name);
        let index_for = |path: &Path| {
            let mut bytes = Vec::new();
            encode(&mut bytes, &path.to_string_lossy(), &[]).unwrap();
            bytes
        };
        fs::write(file("import-index-1.bin"), index_for(&install)).unwrap();
        fs::write(file("import-index-2.bin"), index_for(&dir.join("Gone"))).unwrap();
        fs::write(file("import-index-3.bin"), b"RVLIMPX2 an older build's").unwrap();
        fs::write(file("import-index-4.bin.77.tmp"), b"half written").unwrap();
        fs::write(file("hero_cache.json"), b"{}").unwrap();
        fs::write(file("import-index-0.bin"), b"the one just written").unwrap();
        prune(&dir, &file("import-index-0.bin"), Duration::ZERO);
        let left: HashSet<String> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        let expected: HashSet<String> = [
            "Paks",
            "import-index-0.bin",
            "import-index-1.bin",
            "hero_cache.json",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        assert_eq!(left, expected);
        let _ = fs::remove_dir_all(&dir);
    }

    /// The listing holds every package the merged store finds a path for, read from the
    /// container holding its chunk, which a patch container's header lists without holding.
    #[test]
    fn every_package_the_game_holds_is_listed_from_the_container_holding_it() {
        let Ok(root) = std::env::var("RIVALS_GAME_ROOT") else {
            return;
        };
        let store = open_base_game_paks(&paks_dir(&root), BASE_CONTAINER).expect("open");
        let listed = list_packages(store.as_ref());
        let mut seen = HashSet::new();
        let pathed = store
            .packages_all()
            .filter(|pkg| seen.insert(pkg.id().0))
            .filter(|pkg| {
                let chunk =
                    FIoChunkId::from_package_id(pkg.id(), 0, EIoChunkType::ExportBundleData);
                store.chunk_path(chunk).is_some()
            })
            .count();
        assert_eq!(listed.len(), pathed);
        for (id, path, _, _, holder) in listed.iter().step_by(4999) {
            let chunk = FIoChunkId::from_package_id(*id, 0, EIoChunkType::ExportBundleData);
            assert!(holder.has_chunk_id(chunk), "{path}");
            let whole = store.chunk_path(chunk).expect("a path");
            assert_eq!(whole.strip_prefix(MOUNT_POINT).unwrap_or(&whole), path);
        }
    }

    /// A header read from the package's first blocks is the header the whole package gives,
    /// for the game's largest packages, whose headers outgrow one block, and a spread of the
    /// rest.
    #[test]
    fn a_header_read_from_its_prefix_is_the_whole_package_s() {
        let Ok(root) = std::env::var("RIVALS_GAME_ROOT") else {
            return;
        };
        let store = open_base_game_paks(&paks_dir(&root), BASE_CONTAINER).expect("open");
        let packages = list_packages(store.as_ref());
        let mut sample: Vec<&Listed<'_>> = packages.iter().step_by(997).collect();
        let mut by_size: Vec<(u64, u64)> = store
            .chunks()
            .filter(|chunk| chunk.id().get_chunk_type() == EIoChunkType::ExportBundleData)
            .map(|chunk| (chunk.size(), chunk.id().get_package_id().0))
            .collect();
        by_size.sort_unstable_by_key(|(size, _)| std::cmp::Reverse(*size));
        for (_, id) in by_size.iter().take(50) {
            if let Ok(at) = packages.binary_search_by_key(id, |(id, ..)| id.0) {
                sample.push(&packages[at]);
            }
        }
        let mut long_headers = 0;
        for (id, path, toc, header, holder) in sample {
            let (Some(toc), Some(header)) = (*toc, *header) else {
                continue;
            };
            let entry = store.package_store_entry(*id);
            let chunk = FIoChunkId::from_package_id(*id, 0, EIoChunkType::ExportBundleData);
            let whole = store.read(chunk).expect("read");
            let full = FZenPackageHeader::deserialize(
                &mut Cursor::new(&whole),
                entry.clone(),
                toc,
                header,
                Some(ENGINE_VERSION.package_file_version()),
            )
            .expect("parse the whole package");
            let from_prefix = read_header(*holder, *id, entry, toc, header).expect("prefix header");
            long_headers +=
                usize::from(u64::from(full.summary.header_size) > retoc::zen::HEADER_GUESS);
            assert_eq!(
                from_prefix.name_map.raw_names(),
                full.name_map.raw_names(),
                "{path}"
            );
            assert_eq!(
                from_prefix.imported_packages, full.imported_packages,
                "{path}"
            );
            assert_eq!(
                from_prefix.imported_public_export_hashes, full.imported_public_export_hashes,
                "{path}"
            );
            assert_eq!(
                from_prefix.import_map.len(),
                full.import_map.len(),
                "{path}"
            );
        }
        eprintln!("{long_headers} sampled header(s) longer than the first read");
    }

    /// The Settings screens' widgets, a good share of them Blueprints: a package is recorded as
    /// holding functions exactly when one of its exports parses as a function.
    #[test]
    fn a_package_holds_functions_exactly_when_an_export_parses_as_one() {
        let (Ok(root), Ok(usmap)) = (
            std::env::var("RIVALS_GAME_ROOT"),
            std::env::var("RIVALS_USMAP"),
        ) else {
            return;
        };
        let mappings = crate::mappings::load(Path::new(&usmap)).expect("mappings");
        let store = open_base_game_paks(&paks_dir(&root), BASE_CONTAINER).expect("open");
        let packages: Vec<Listed<'_>> = list_packages(store.as_ref())
            .into_iter()
            .filter(|(_, path, ..)| path.starts_with("Marvel/Content/Marvel/UI/Blueprints/Setting"))
            .collect();
        let named: Vec<(FPackageId, String)> = packages
            .iter()
            .map(|(id, path, ..)| (*id, path.clone()))
            .collect();
        assert!(named.len() > 50, "{}", named.len());
        let layer =
            index_layer(store.as_ref(), packages, None, Vec::new(), &mut |_| {}).expect("index");
        assert!(layer.unread.is_empty(), "{:?}", layer.unread);
        assert!(!layer.functions.is_empty());
        let converter = crate::asset::PackageConverter::new(store.as_ref());
        for (at, (id, path)) in named.iter().enumerate() {
            let bundle = converter.convert(*id, path).expect("converts");
            let parsed = rivals_uasset::parse_package(
                &rivals_uasset::AssetBundle {
                    asset: &bundle.asset_file_buffer,
                    exports: &bundle.exports_file_buffer,
                },
                Some(&mappings),
            )
            .expect("parses");
            let functions = parsed
                .exports
                .iter()
                .any(|export| export.class_name == "Function");
            let at = u32::try_from(at).unwrap();
            assert_eq!(
                layer.functions.binary_search(&at).is_ok(),
                functions,
                "{path}"
            );
        }
    }

    /// The index says a package holds functions wherever reading its whole header says so, over a
    /// spread of the game's packages.
    #[test]
    fn the_index_holds_functions_where_the_whole_header_says_so() {
        let Ok(root) = std::env::var("RIVALS_GAME_ROOT") else {
            return;
        };
        let store = open_base_game_paks(&paks_dir(&root), BASE_CONTAINER).expect("open");
        let packages: Vec<Listed<'_>> = list_packages(store.as_ref())
            .into_iter()
            .step_by(37)
            .collect();
        let expected: Vec<(String, bool)> = packages
            .par_iter()
            .map(|(id, path, toc, header, _)| {
                let holds = toc.zip(*header).and_then(|versions| {
                    crate::game_search::header_names_class(
                        store.as_ref(),
                        *id,
                        versions,
                        &[*FUNCTION_CLASS],
                    )
                });
                (path.clone(), holds.unwrap_or(true))
            })
            .collect();
        let layer =
            index_layer(store.as_ref(), packages, None, Vec::new(), &mut |_| {}).expect("index");
        let index = ImportIndex {
            layers: vec![layer],
        };
        let mut holding = 0;
        for (at, (path, holds)) in expected.iter().enumerate() {
            let id = index.layers[0].ids[at];
            assert_eq!(index.may_hold_functions(None, id), *holds, "{path}");
            holding += usize::from(*holds);
        }
        assert!(holding > 0);
        eprintln!(
            "{holding} of {} sampled packages hold functions",
            expected.len()
        );
    }

    /// The installed mods' packages are indexed under the mod's name, with the imports their own
    /// headers carry. Walks only the mods, which is quick; skipped when none is installed. It holds
    /// the mods folder for its turn, or it could read a probe mod another test is still writing.
    #[test]
    fn an_installed_mod_s_packages_are_indexed_under_its_name() {
        let Ok(root) = std::env::var("RIVALS_GAME_ROOT") else {
            return;
        };
        let _turn = crate::paths::ModsTurn::take();
        let paks = paks_dir(&root);
        let Some(container) = enabled_mod_containers(&paks).into_iter().next() else {
            return;
        };
        let name = stem(&container);
        let store = open_target_only(&paks, &container, &name).expect("open the mod");
        let packages = list_packages(store.as_ref());
        let layer = index_layer(
            store.as_ref(),
            packages,
            Some(name.clone()),
            Vec::new(),
            &mut |_| {},
        )
        .expect("index");
        assert!(!layer.ids.is_empty(), "{name} lists no packages");
        assert!(layer.shown(0).ends_with(&format!("(in {name})")));
        assert!(!layer.edges.is_empty(), "{name}'s packages import nothing");
        assert!(layer.notes.is_empty(), "{:?}", layer.notes);
    }
}
