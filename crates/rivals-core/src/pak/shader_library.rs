//! Shader archives in a mod. The editor cooks a material's new shaders into a legacy
//! `ShaderArchive-<Library>.ushaderbytecode`, with a `ShaderAssetInfo-<Library>.assetinfo.json`
//! naming the packages that use each of its shader maps. In a container the archive becomes the
//! library's chunks, and the packages' store entries list the shader maps instead.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use retoc::iostore::IoStoreTrait;
use retoc::iostore_writer::IoStoreWriter;
use retoc::shader_library;
use retoc::{EIoChunkType, FSHAHash, UEPath};

use super::containers::{MOUNT_POINT, base_game_utocs, list_utoc_paths};

const ARCHIVE_EXT: &str = ".ushaderbytecode";
const ASSET_INFO_PREFIX: &str = "shaderassetinfo-";
const ASSET_INFO_EXT: &str = ".assetinfo.json";

/// The version of the archives the editor cooks, the only one a library is built from.
const COOKED_ARCHIVE_VERSION: i32 = 2;

/// Where a pack learns the libraries the game ships, by [`library_name`] in lower case. It is
/// asked only when the folder holds an archive, since finding them reads every game container.
pub type GameLibraries<'a> = &'a dyn Fn() -> Result<HashSet<String>, String>;

/// The name the library of the archive at `path` goes by: `Global-PCD3D_SM6-PCD3D_SM6` for
/// `ShaderArchive-Global-PCD3D_SM6-PCD3D_SM6.ushaderbytecode`. The library's chunk is named after
/// it, so two archives of one name are one library to the game, the later shadowing the other.
pub fn library_name(path: &str) -> Option<String> {
    let file = path.rsplit(['/', '\\']).next()?;
    if !file.to_ascii_lowercase().ends_with(ARCHIVE_EXT) {
        return None;
    }
    let stem = &file[..file.len() - ARCHIVE_EXT.len()];
    let (_, name) = stem.split_once('-')?;
    (!name.is_empty()).then(|| name.to_string())
}

/// Whether `path` names a shader archive's asset info.
pub fn is_asset_info(path: &str) -> bool {
    let file = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let lower = file.to_ascii_lowercase();
    lower.starts_with(ASSET_INFO_PREFIX) && lower.ends_with(ASSET_INFO_EXT)
}

/// The library a shader archive or its asset info belongs to, by [`library_name`]: the one name
/// the two files share.
pub fn library_of(path: &str) -> Option<String> {
    library_name(path).or_else(|| {
        let file = path.rsplit(['/', '\\']).next().unwrap_or(path);
        is_asset_info(file)
            .then(|| file[ASSET_INFO_PREFIX.len()..file.len() - ASSET_INFO_EXT.len()].to_string())
    })
}

/// The asset info that goes with the archive at `path`, beside it.
fn asset_info_path(path: &str) -> Option<String> {
    shader_library::get_shader_asset_info_filename_from_library_filename(path).ok()
}

/// The libraries the game at `paks_dir` ships, by [`library_name`] in lower case, read from its
/// containers' directory indices.
pub fn game_library_names(paks_dir: &Path) -> Result<HashSet<String>, String> {
    let mut names = HashSet::new();
    for utoc in base_game_utocs(paks_dir) {
        for path in list_utoc_paths(&utoc)? {
            if let Some(name) = library_name(&path) {
                names.insert(name.to_ascii_lowercase());
            }
        }
    }
    Ok(names)
}

/// What packing a folder's shader archives did.
#[derive(Default)]
pub(crate) struct PackedLibraries {
    /// Each archive written as a library, by its path under the folder.
    pub written: Vec<String>,
    /// Each archive left out, by its path under the folder.
    pub left_out: Vec<String>,
    /// The archives and their asset infos, which go in no pak.
    pub taken: HashSet<PathBuf>,
    /// Package name to the shader maps its store entry lists.
    pub shader_maps: HashMap<String, Vec<FSHAHash>>,
    pub warnings: Vec<String>,
}

/// Writes the shader archives among `files`, paths under `dir`, as libraries into `writer`, and
/// gathers the shader maps their asset infos give each package. An archive named like a library
/// the game ships would shadow that whole library, so it is left out, and so is one that is not
/// what the editor cooks.
pub(crate) fn pack(
    writer: &mut IoStoreWriter,
    dir: &Path,
    files: &[PathBuf],
    game_libraries: GameLibraries,
) -> Result<PackedLibraries, String> {
    let mut packed = PackedLibraries::default();
    let shown = |rel: &Path| rel.to_string_lossy().replace('\\', "/");
    let by_lower: HashMap<String, &PathBuf> = files
        .iter()
        .map(|rel| (shown(rel).to_ascii_lowercase(), rel))
        .collect();
    let archives: Vec<(&PathBuf, String)> = files
        .iter()
        .filter_map(|rel| Some((rel, library_name(&shown(rel))?)))
        .collect();
    if archives.is_empty() {
        return Ok(packed);
    }
    let shipped = game_libraries()?;
    let log = retoc::logging::Log::no_log();
    for (rel, name) in archives {
        let path = shown(rel);
        packed.taken.insert(rel.clone());
        let info = asset_info_path(&path)
            .and_then(|info| by_lower.get(&info.to_ascii_lowercase()).copied());
        if let Some(info) = info {
            packed.taken.insert(info.clone());
        }
        if shipped.contains(&name.to_ascii_lowercase()) {
            packed.warnings.push(format!(
                "{path} is named like the game's own {name} shader library, which it would \
                 replace; left out"
            ));
            packed.left_out.push(path);
            continue;
        }
        let bytes = std::fs::read(dir.join(rel)).map_err(|e| format!("read {path}: {e}"))?;
        let version = bytes
            .get(..4)
            .map(|word| i32::from_le_bytes([word[0], word[1], word[2], word[3]]));
        if version != Some(COOKED_ARCHIVE_VERSION) {
            packed.warnings.push(format!(
                "{path} is not a shader archive as the editor cooks one (version {}), so no \
                 library is made from it; left out",
                version.map_or_else(|| "unknown".to_string(), |v| v.to_string())
            ));
            packed.left_out.push(path);
            continue;
        }
        match info {
            Some(info) => {
                let json = std::fs::read(dir.join(info))
                    .map_err(|e| format!("read {}: {e}", shown(info)))?;
                shader_library::read_shader_asset_info(&json, &mut packed.shader_maps)
                    .map_err(|e| format!("{}: {e}", shown(info)))?;
            }
            None => packed.warnings.push(format!(
                "{path} has no {} beside it, so no package is told which of its shader maps it \
                 uses",
                asset_info_path(&path).unwrap_or_else(|| "asset info".to_string())
            )),
        }
        let mounted = format!("{MOUNT_POINT}{path}");
        shader_library::write_io_store_library(writer, &bytes, UEPath::new(&mounted), &log)
            .map_err(|e| format!("{path}: {e}"))?;
        packed.written.push(path);
    }
    Ok(packed)
}

/// What extracting a container's libraries wrote.
#[derive(Default)]
pub(crate) struct ExtractedLibraries {
    /// Each archive written, by its path under the output folder.
    pub written: Vec<String>,
    /// One `path: reason` line per library that could not be rebuilt.
    pub errors: Vec<String>,
}

/// Writes each library `store` holds back out under `out` as the archive and asset info the editor
/// cooks, at the path the container lists it by: the packages' shader maps are read from their
/// store entries, so `store` should hold the mod alone. `keep` picks libraries by that path.
pub(crate) fn extract(
    store: &dyn IoStoreTrait,
    out: &Path,
    keep: &dyn Fn(&str) -> bool,
) -> Result<ExtractedLibraries, String> {
    let log = retoc::logging::Log::no_log();
    let mut extracted = ExtractedLibraries::default();
    let libraries: Vec<_> = store
        .chunks()
        .filter(|chunk| chunk.id().get_chunk_type() == EIoChunkType::ShaderCodeLibrary)
        .filter_map(|chunk| Some((chunk.id(), chunk.path()?)))
        .collect();
    for (id, path) in libraries {
        let path = path.strip_prefix(MOUNT_POINT).unwrap_or(&path).to_string();
        if !keep(&path) {
            continue;
        }
        let (archive, info) =
            match shader_library::rebuild_shader_library_from_io_store(store, id, &log, true) {
                Ok(rebuilt) => rebuilt,
                Err(e) => {
                    extracted.errors.push(format!("{path}: {e}"));
                    continue;
                }
            };
        let write = |rel: &str, bytes: &[u8]| -> Result<(), String> {
            let dest = out.join(rel);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("create {}: {e}", parent.display()))?;
            }
            std::fs::write(&dest, bytes).map_err(|e| format!("write {}: {e}", dest.display()))
        };
        write(&path, &archive)?;
        if let Some(info_path) = asset_info_path(&path) {
            write(&info_path, &info)?;
        }
        extracted.written.push(path);
    }
    extracted.written.sort();
    Ok(extracted)
}

/// Shader archives for tests, built by hand as the editor cooks them.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
pub(crate) mod fixture {
    pub(crate) const MAP_HASH: [u8; 20] = [7; 20];
    pub(crate) const SHADER_HASH: [u8; 20] = [9; 20];

    /// A shader archive as the editor cooks one: one shader map holding one shader, stored
    /// uncompressed, and preloaded whole.
    pub(crate) fn cooked_archive(version: i32, code: &[u8]) -> Vec<u8> {
        let mut out = version.to_le_bytes().to_vec();
        let mut array = |count: u32, items: &[&[u8]]| {
            out.extend(count.to_le_bytes());
            for item in items {
                out.extend(*item);
            }
        };
        let size = (code.len() as u32).to_le_bytes();
        array(1, &[&MAP_HASH]);
        array(1, &[&SHADER_HASH]);
        array(
            1,
            &[
                &0u32.to_le_bytes(),
                &1u32.to_le_bytes(),
                &0u32.to_le_bytes(),
                &1u32.to_le_bytes(),
            ],
        );
        array(1, &[&0u64.to_le_bytes(), &size, &size, &[3]]);
        array(
            1,
            &[&0i64.to_le_bytes(), &(code.len() as i64).to_le_bytes()],
        );
        array(1, &[&0u32.to_le_bytes()]);
        out.extend(code);
        out
    }

    /// The shader map hashes, shader hashes and code of a cooked archive.
    pub(crate) fn read_archive(bytes: &[u8]) -> (Vec<[u8; 20]>, Vec<[u8; 20]>, Vec<u8>) {
        let mut at = 4;
        let count = |at: &mut usize| {
            let n = u32::from_le_bytes(bytes[*at..*at + 4].try_into().unwrap()) as usize;
            *at += 4;
            n
        };
        let hashes = |at: &mut usize, n: usize| -> Vec<[u8; 20]> {
            let list = (0..n)
                .map(|i| bytes[*at + i * 20..*at + i * 20 + 20].try_into().unwrap())
                .collect();
            *at += n * 20;
            list
        };
        let n = count(&mut at);
        let maps = hashes(&mut at, n);
        let n = count(&mut at);
        let shaders = hashes(&mut at, n);
        for width in [16, 17, 16, 4] {
            let n = count(&mut at);
            at += n * width;
        }
        (maps, shaders, bytes[at..].to_vec())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_library_is_named_after_its_archive() {
        assert_eq!(
            library_name("Marvel/Content/ShaderArchive-Global-PCD3D_SM6-PCD3D_SM6.ushaderbytecode")
                .as_deref(),
            Some("Global-PCD3D_SM6-PCD3D_SM6")
        );
        assert_eq!(
            library_name("ShaderArchive-MyMod-PCD3D_SM6.USHADERBYTECODE").as_deref(),
            Some("MyMod-PCD3D_SM6")
        );
        assert_eq!(library_name("Marvel/Content/NoName.ushaderbytecode"), None);
        assert_eq!(library_name("Marvel/Content/ShaderArchive-X.uasset"), None);
        assert!(is_asset_info(
            "Marvel/Content/ShaderAssetInfo-MyMod-PCD3D_SM6.assetinfo.json"
        ));
        assert!(!is_asset_info("Marvel/Content/Other.json"));
        assert_eq!(
            asset_info_path("Marvel/Content/ShaderArchive-MyMod-PCD3D_SM6.ushaderbytecode")
                .as_deref(),
            Some("Marvel/Content/ShaderAssetInfo-MyMod-PCD3D_SM6.assetinfo.json")
        );
    }
}
