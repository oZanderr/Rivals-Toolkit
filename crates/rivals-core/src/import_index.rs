//! Indexes which packages import which objects across the game's containers, so an export removal
//! can name the packages that would break.
//!
//! Two tiers come out of one walk. The container header's store entries say which packages each
//! package imports; the zen package header's import map says which objects, as
//! `(package id, public export hash)`, which is exactly what the loader resolves and so is immune
//! to case and separators. The result is cached on disk and keyed by the containers' names, sizes
//! and modification times.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Cursor, ErrorKind};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use retoc::iostore::IoStoreTrait;
use retoc::version::EngineVersion;
use retoc::zen::FZenPackageHeader;
use retoc::zen_asset_conversion::get_public_export_hash;
use retoc::{EIoChunkType, FIoChunkId};
use serde::Serialize;

use crate::pak::containers::{MOUNT_POINT, open_base_game_paks};
use crate::paths::paks_dir;

/// The container whose siblings make up the base game; the walk covers everything the merged
/// store admits alongside it.
const BASE_CONTAINER: &str = "pakchunk0-Windows";
const MAGIC: &[u8; 8] = b"RVLIMPX1";
const ENGINE_VERSION: EngineVersion = EngineVersion::UE5_3;

/// One package importing one object of another. A zero hash records the package dependency alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Edge {
    target: u64,
    hash: u64,
    importer: u64,
}

pub struct ImportIndex {
    /// Sorted, so one binary search finds every importer of an object.
    edges: Vec<Edge>,
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

/// What the containers looked like: every top-level `.utoc` under Paks by name, size and mtime.
fn fingerprint(game_root: &str) -> Result<Vec<u8>, String> {
    let dir = paks_dir(game_root);
    let mut entries: Vec<(String, u64, u64)> = Vec::new();
    for entry in fs::read_dir(&dir).map_err(|e| format!("Could not read {}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("utoc") {
            continue;
        }
        let meta = entry.metadata().map_err(|e| e.to_string())?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
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
    let store = open_base_game_paks(&paks_dir(game_root), BASE_CONTAINER)?;
    // Deduped with the highest-priority copy first, so a patched package is indexed once, from
    // its patched header.
    let list: Vec<_> = store
        .packages_all()
        .filter_map(|pkg| {
            let chunk = FIoChunkId::from_package_id(pkg.id(), 0, EIoChunkType::ExportBundleData);
            let path = store.chunk_path(chunk)?;
            let stripped = path.strip_prefix(MOUNT_POINT).unwrap_or(&path).to_string();
            let container = pkg.container();
            Some((
                pkg.id(),
                stripped,
                container.container_file_version(),
                container.container_header_version(),
            ))
        })
        .collect();
    let total = list.len();
    let mut edges = Vec::new();
    let mut paths = BTreeMap::new();
    for (done, (id, path, toc_version, header_version)) in list.into_iter().enumerate() {
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
    let index = ImportIndex {
        edges,
        paths,
        built_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
        fingerprint,
    };
    let cache = cache_path(game_root)?;
    fs::write(&cache, index.encode())
        .map_err(|e| format!("Could not write {}: {e}", cache.display()))?;
    Ok(index)
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
        let mut packages: Vec<String> = importers
            .into_iter()
            .map(|id| {
                self.paths
                    .get(&id)
                    .cloned()
                    .unwrap_or_else(|| format!("package {id:016x}"))
            })
            .collect();
        packages.sort();
        packages.dedup();
        Importers {
            path: text.to_string(),
            packages,
        }
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
        Some(Self {
            edges,
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
        paths.insert(c, "Marvel/Content/C.uasset".to_string());
        ImportIndex {
            edges,
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
            vec!["Marvel/Content/B.uasset", "Marvel/Content/C.uasset"]
        );
        let found = index.importers_of("/game/a.DEFAULT__X_C");
        assert_eq!(found.packages, vec!["Marvel/Content/C.uasset"]);
        assert!(index.importers_of("/Game/A.Nothing").packages.is_empty());
    }

    #[test]
    fn a_bare_package_path_collects_every_importer_of_the_package() {
        let found = index().importers_of("/Game/A");
        assert_eq!(
            found.packages,
            vec!["Marvel/Content/B.uasset", "Marvel/Content/C.uasset"]
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
        assert!(ImportIndex::decode(b"not an index").is_none());
        assert!(ImportIndex::decode(&bytes[..bytes.len() - 5]).is_none());
    }
}
