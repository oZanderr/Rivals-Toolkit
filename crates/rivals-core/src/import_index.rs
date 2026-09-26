//! Indexes which packages import which objects across the game's containers and the installed
//! mods, so an export removal or rename can name the packages that would break.
//!
//! Two tiers come out of one walk. The container header's store entries say which packages each
//! package imports; the zen package header's import map says which objects, as
//! `(package id, public export hash)`, which is exactly what the loader resolves and so is immune
//! to case and separators. Each package's name map also says which packages it names by path,
//! which is every package it could point at softly. The result is cached on disk and keyed by the
//! containers' names, sizes and modification times.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{Cursor, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use retoc::iostore::IoStoreTrait;
use retoc::version::EngineVersion;
use retoc::zen::FZenPackageHeader;
use retoc::zen_asset_conversion::get_public_export_hash;
use retoc::{EIoChunkType, FIoChunkId};
use serde::Serialize;

use crate::pak::containers::{
    MOUNT_POINT, open_base_game_paks, open_target_only, undecryptable_container_stems,
};
use crate::paths::paks_dir;

/// The container whose siblings make up the base game; the walk covers everything the merged
/// store admits alongside it.
const BASE_CONTAINER: &str = "pakchunk0-Windows";
const MAGIC: &[u8; 8] = b"RVLIMPX2";
const ENGINE_VERSION: EngineVersion = EngineVersion::UE5_3;

/// One package importing one object of another. A zero hash records the package dependency alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Edge {
    target: u64,
    hash: u64,
    importer: u64,
}

/// One package naming another by path, which is what a soft reference to it needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Mention {
    target: u64,
    mentioner: u64,
}

pub struct ImportIndex {
    /// Sorted, so one binary search finds every importer of an object.
    edges: Vec<Edge>,
    /// Sorted the same way, by the package named.
    mentions: Vec<Mention>,
    /// Package id to container-relative path for every package the walk saw.
    paths: BTreeMap<u64, String>,
    pub built_at: u64,
    fingerprint: Vec<u8>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportIndexStatus {
    pub cache: String,
    pub built: bool,
    /// The containers changed since the index was built, so its answers may be off.
    pub stale: bool,
    pub packages: usize,
    pub edges: usize,
    pub built_at: Option<u64>,
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
    let mut normalised = paks_dir(game_root)
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    while normalised.ends_with('/') {
        normalised.pop();
    }
    cityhasher::hash(normalised.as_bytes())
}

/// The installed mods' containers the walk reads: every enabled `.utoc` under `~mods`, the copy
/// the game loads first leading, so a package several mods ship is read from the winner.
fn mod_containers(paks: &Path) -> Vec<PathBuf> {
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

/// What the walk reads: the base game's top-level containers and the enabled mods', by name,
/// size and mtime. A container the walk leaves out is left out here too, so it never reads as a
/// change.
fn fingerprint(game_root: &str) -> Result<Vec<u8>, String> {
    let dir = paks_dir(game_root);
    let undecryptable = undecryptable_container_stems(&dir);
    let mut containers: Vec<PathBuf> = Vec::new();
    for entry in fs::read_dir(&dir).map_err(|e| format!("Could not read {}: {e}", dir.display()))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        let name = stem(&path);
        if path.extension().and_then(|e| e.to_str()) == Some("utoc")
            && !undecryptable.contains(&name)
            && !name.contains("_9999999_")
        {
            containers.push(path);
        }
    }
    containers.extend(mod_containers(&dir));
    let mut entries: Vec<(String, u64, u64)> = Vec::new();
    for path in containers {
        let meta = fs::metadata(&path).map_err(|e| e.to_string())?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        let name = path
            .strip_prefix(&dir)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/")
            .to_ascii_lowercase();
        entries.push((name, meta.len(), mtime));
    }
    entries.sort();
    let mut out = Vec::new();
    for (name, size, mtime) in entries {
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&mtime.to_le_bytes());
    }
    Ok(out)
}

/// Walks every package once and writes the index to the cache. `progress` is told how many
/// packages are done out of how many.
pub fn build(
    game_root: &str,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<ImportIndex, String> {
    let fingerprint = fingerprint(game_root)?;
    let paks = paks_dir(game_root);
    // The mods first, each alone and the winner leading, then the game: a package is indexed from
    // the copy the game loads.
    let mut stores: Vec<(Option<String>, Arc<dyn IoStoreTrait>)> = Vec::new();
    for container in mod_containers(&paks) {
        let name = stem(&container);
        if let Ok(store) = open_target_only(&paks, &container, &name) {
            stores.push((Some(name), Arc::from(store)));
        }
    }
    stores.push((None, open_base_game_paks(&paks, BASE_CONTAINER)?));
    let mut index = walk(&stores, progress);
    index.fingerprint = fingerprint;
    let cache = cache_path(game_root)?;
    fs::write(&cache, index.encode())
        .map_err(|e| format!("Could not write {}: {e}", cache.display()))?;
    Ok(index)
}

/// Indexes every package the stores hold, each once, from the first store holding it. A mod's
/// packages are listed with the mod's name after their path.
fn walk(
    stores: &[(Option<String>, Arc<dyn IoStoreTrait>)],
    progress: &mut dyn FnMut(usize, usize),
) -> ImportIndex {
    let mut seen = HashSet::new();
    let mut list = Vec::new();
    for (mod_name, store) in stores {
        // Deduped with the highest-priority copy first, so a patched package is indexed once,
        // from its patched header.
        for pkg in store.packages_all() {
            if !seen.insert(pkg.id().0) {
                continue;
            }
            let chunk = FIoChunkId::from_package_id(pkg.id(), 0, EIoChunkType::ExportBundleData);
            let Some(path) = store.chunk_path(chunk) else {
                continue;
            };
            let stripped = path.strip_prefix(MOUNT_POINT).unwrap_or(&path).to_string();
            let shown = match mod_name {
                Some(name) => format!("{stripped} (in {name})"),
                None => stripped,
            };
            let container = pkg.container();
            list.push((
                store,
                pkg.id(),
                shown,
                container.container_file_version(),
                container.container_header_version(),
            ));
        }
    }
    let total = list.len();
    let mut edges = Vec::new();
    let mut mentions = Vec::new();
    let mut paths = BTreeMap::new();
    for (done, (store, id, path, toc_version, header_version)) in list.into_iter().enumerate() {
        paths.insert(id.0, path);
        let entry = store.package_store_entry(id);
        for imported in entry
            .iter()
            .flat_map(|entry| entry.imported_packages.iter())
        {
            edges.push(Edge {
                target: imported.0,
                hash: 0,
                importer: id.0,
            });
        }
        // The import map names each imported object by package and public export hash.
        if let (Some(toc_version), Some(header_version)) = (toc_version, header_version)
            && let Ok(data) = store.read(FIoChunkId::from_package_id(
                id,
                0,
                EIoChunkType::ExportBundleData,
            ))
            && let Ok(header) = FZenPackageHeader::deserialize(
                &mut Cursor::new(&data),
                entry,
                toc_version,
                header_version,
                Some(ENGINE_VERSION.package_file_version()),
            )
        {
            for name in header.name_map.copy_raw_names() {
                if !name.starts_with('/') || name.starts_with("/Script/") {
                    continue;
                }
                let package = name.split(['.', ':']).next().unwrap_or(&name);
                let target = package_id(package);
                if target != id.0 {
                    mentions.push(Mention {
                        target,
                        mentioner: id.0,
                    });
                }
            }
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
                    edges.push(Edge {
                        target: package.0,
                        hash: *hash,
                        importer: id.0,
                    });
                }
            }
        }
        if done % 256 == 0 || done + 1 == total {
            progress(done + 1, total);
        }
    }
    edges.sort_unstable();
    edges.dedup();
    mentions.sort_unstable();
    mentions.dedup();
    ImportIndex {
        edges,
        mentions,
        paths,
        built_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
        fingerprint: Vec::new(),
    }
}

/// The cached index, or `None` when there is none yet or the file is not one this build wrote.
pub fn load(game_root: &str) -> Result<Option<ImportIndex>, String> {
    let path = cache_path(game_root)?;
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("Could not read {}: {e}", path.display())),
    };
    Ok(ImportIndex::decode(&bytes))
}

/// What a plan says when the index it named importers from was built before the game or the mods
/// last changed.
pub const STALE_WARNING: &str = "The import index was built before your last game update or \
     mod change, so the packages it names may be out of date. Rebuild it to be sure.";

pub fn status(game_root: &str) -> Result<ImportIndexStatus, String> {
    let cache = cache_path(game_root)?.display().to_string();
    Ok(match load(game_root)? {
        Some(index) => ImportIndexStatus {
            cache,
            built: true,
            stale: index.is_stale(game_root),
            packages: index.paths.len(),
            edges: index.edges.len(),
            built_at: Some(index.built_at),
        },
        None => ImportIndexStatus {
            cache,
            built: false,
            stale: false,
            packages: 0,
            edges: 0,
            built_at: None,
        },
    })
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
    pub fn is_stale(&self, game_root: &str) -> bool {
        fingerprint(game_root).is_ok_and(|now| now != self.fingerprint)
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
        let importers: Vec<u64> = match object {
            // Zen hashes the package-relative path lowercased with `/` between the objects.
            Some(object) => {
                let hash = get_public_export_hash(&object.replace(':', "/").to_lowercase());
                self.between((target, hash), (target, hash))
            }
            None => self.between((target, 0), (target, u64::MAX)),
        };
        let mut packages: Vec<String> = importers.into_iter().map(|id| self.path_of(id)).collect();
        packages.sort();
        packages.dedup();
        Importers {
            path: text.to_string(),
            packages,
        }
    }

    /// The packages naming `package` by path, which is every package that could point at it or at
    /// an object in it softly. A candidate list: naming it is needed for a soft reference, not
    /// proof of one.
    pub fn mentions_of(&self, package: &str) -> Vec<String> {
        let target = package_id(package.split(['.', ':']).next().unwrap_or(package).trim());
        let start = self.mentions.partition_point(|m| m.target < target);
        let end = self.mentions.partition_point(|m| m.target <= target);
        let mut packages: Vec<String> = self.mentions[start..end]
            .iter()
            .map(|m| self.path_of(m.mentioner))
            .collect();
        packages.sort();
        packages.dedup();
        packages
    }

    fn path_of(&self, id: u64) -> String {
        self.paths
            .get(&id)
            .cloned()
            .unwrap_or_else(|| format!("package {id:016x}"))
    }

    /// Importer ids of every edge whose `(target, hash)` lies in the inclusive range.
    fn between(&self, from: (u64, u64), to: (u64, u64)) -> Vec<u64> {
        let key = |edge: &Edge| (edge.target, edge.hash);
        let start = self.edges.partition_point(|edge| key(edge) < from);
        let end = self.edges.partition_point(|edge| key(edge) <= to);
        self.edges[start..end]
            .iter()
            .map(|edge| edge.importer)
            .collect()
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(32 + self.edges.len() * 24);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&(self.fingerprint.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.fingerprint);
        out.extend_from_slice(&self.built_at.to_le_bytes());
        out.extend_from_slice(&(self.paths.len() as u32).to_le_bytes());
        for (id, path) in &self.paths {
            out.extend_from_slice(&id.to_le_bytes());
            out.extend_from_slice(&(path.len() as u32).to_le_bytes());
            out.extend_from_slice(path.as_bytes());
        }
        out.extend_from_slice(&(self.edges.len() as u64).to_le_bytes());
        for edge in &self.edges {
            out.extend_from_slice(&edge.target.to_le_bytes());
            out.extend_from_slice(&edge.hash.to_le_bytes());
            out.extend_from_slice(&edge.importer.to_le_bytes());
        }
        out.extend_from_slice(&(self.mentions.len() as u64).to_le_bytes());
        for mention in &self.mentions {
            out.extend_from_slice(&mention.target.to_le_bytes());
            out.extend_from_slice(&mention.mentioner.to_le_bytes());
        }
        out
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let mut reader = Reader { bytes, at: 0 };
        if reader.take(MAGIC.len())? != MAGIC {
            return None;
        }
        let fingerprint_len = reader.u32()? as usize;
        let fingerprint = reader.take(fingerprint_len)?.to_vec();
        let built_at = reader.u64()?;
        let mut paths = BTreeMap::new();
        for _ in 0..reader.u32()? {
            let id = reader.u64()?;
            let path_len = reader.u32()? as usize;
            let path = std::str::from_utf8(reader.take(path_len)?).ok()?;
            paths.insert(id, path.to_string());
        }
        let count = usize::try_from(reader.u64()?).ok()?;
        let mut edges = Vec::with_capacity(count.min(1 << 24));
        for _ in 0..count {
            edges.push(Edge {
                target: reader.u64()?,
                hash: reader.u64()?,
                importer: reader.u64()?,
            });
        }
        let count = usize::try_from(reader.u64()?).ok()?;
        let mut mentions = Vec::with_capacity(count.min(1 << 24));
        for _ in 0..count {
            mentions.push(Mention {
                target: reader.u64()?,
                mentioner: reader.u64()?,
            });
        }
        Some(Self {
            edges,
            mentions,
            paths,
            built_at,
            fingerprint,
        })
    }
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
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn index() -> ImportIndex {
        let a = package_id("/Game/A");
        let b = package_id("/Game/B");
        let c = package_id("/Game/C");
        let target = get_public_export_hash("default__x_c/sub");
        let other = get_public_export_hash("default__x_c");
        let mut edges = vec![
            Edge {
                target: a,
                hash: 0,
                importer: b,
            },
            Edge {
                target: a,
                hash: target,
                importer: b,
            },
            Edge {
                target: a,
                hash: target,
                importer: c,
            },
            Edge {
                target: a,
                hash: other,
                importer: c,
            },
        ];
        edges.sort_unstable();
        let mut paths = BTreeMap::new();
        paths.insert(b, "Marvel/Content/B.uasset".to_string());
        paths.insert(c, "Marvel/Content/C.uasset (in SomeMod)".to_string());
        let mut mentions = vec![Mention {
            target: a,
            mentioner: c,
        }];
        mentions.sort_unstable();
        ImportIndex {
            edges,
            mentions,
            paths,
            built_at: 7,
            fingerprint: b"fp".to_vec(),
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
        let index = index();
        let bytes = index.encode();
        let back = ImportIndex::decode(&bytes).expect("decode");
        assert_eq!(back.edges, index.edges);
        assert_eq!(back.paths, index.paths);
        assert_eq!(back.built_at, 7);
        assert_eq!(back.fingerprint, b"fp");
        assert_eq!(back.mentions, index.mentions);
        assert!(ImportIndex::decode(b"not an index").is_none());
        assert!(ImportIndex::decode(&bytes[..bytes.len() - 5]).is_none());
    }

    /// A package named by path in another's names is a candidate for a soft reference, by
    /// package or by any object in it.
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

    /// A mod enabled or changed under `~mods` changes the fingerprint; a container the walk leaves
    /// out does not, so it never makes the index read as stale.
    #[test]
    fn the_fingerprint_follows_the_mods_and_ignores_what_the_walk_skips() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let root =
            std::env::temp_dir().join(format!("rivals-index-fp-{}-{stamp}", std::process::id()));
        let paks = paks_dir(&root.to_string_lossy());
        let write = |path: &Path, bytes: usize| {
            fs::create_dir_all(path.parent().expect("parent")).expect("dir");
            let mut header = vec![0u8; bytes.max(0x90)];
            header[0..16].copy_from_slice(b"-==--==--==--==-");
            fs::write(path, header).expect("write");
        };
        let game_root = root.to_string_lossy().to_string();
        write(&paks.join("pakchunk0-Windows.utoc"), 0x90);
        let base = fingerprint(&game_root).expect("fingerprint");

        write(&paks.join("Stray_9999999_P.utoc"), 0x90);
        assert_eq!(fingerprint(&game_root).expect("fingerprint"), base);

        write(&paks.join("~mods").join("SomeMod_9999999_P.utoc"), 0x90);
        let with_mod = fingerprint(&game_root).expect("fingerprint");
        assert_ne!(with_mod, base);

        write(
            &paks.join("~mods").join("Off_9999999_P.utoc.disabled"),
            0x90,
        );
        assert_eq!(fingerprint(&game_root).expect("fingerprint"), with_mod);
        let _ = fs::remove_dir_all(&root);
    }

    /// The installed mods' packages are indexed under the mod's name, with the imports their own
    /// headers carry. Walks only the mods, which is quick; skipped when none is installed.
    #[test]
    fn an_installed_mod_s_packages_are_indexed_under_its_name() {
        let Ok(root) = std::env::var("RIVALS_GAME_ROOT") else {
            return;
        };
        let paks = paks_dir(&root);
        let stores: Vec<(Option<String>, Arc<dyn IoStoreTrait>)> = mod_containers(&paks)
            .into_iter()
            .filter_map(|container| {
                let name = stem(&container);
                let store = open_target_only(&paks, &container, &name).ok()?;
                Some((Some(name), Arc::from(store)))
            })
            .collect();
        let Some((Some(first), _)) = stores.first() else {
            return;
        };
        let index = walk(&stores, &mut |_, _| {});
        let from_mod: Vec<&String> = index
            .paths
            .values()
            .filter(|path| path.ends_with(&format!("(in {first})")))
            .collect();
        assert!(!from_mod.is_empty(), "{first} lists no packages");
        assert!(
            index.edges.iter().any(|edge| index
                .paths
                .get(&edge.importer)
                .is_some_and(|p| p.contains(first.as_str()))),
            "{first}'s packages import nothing"
        );
    }
}
