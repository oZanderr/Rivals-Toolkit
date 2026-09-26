//! Whether an object path names something the game, an installed mod or the container being saved
//! into actually has, so an import or a reference is not pointed at nothing.

use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use retoc::iostore::IoStoreTrait;
use retoc::script_objects::FPackageObjectIndex;
use retoc::version::EngineVersion;
use retoc::zen::FZenPackageHeader;
use retoc::zen_asset_conversion::get_public_export_hash;
use retoc::{EIoChunkType, FIoChunkId, FPackageId};
use serde::Serialize;

use crate::pak::containers::{open_base_game_paks, open_target_only};
use crate::paths::paks_dir;

/// What looking an object path up found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "found", rename_all = "snake_case")]
pub enum Existence {
    Found,
    /// No container this could read holds the package.
    NoPackage,
    /// The package is there, but exports nothing by that path.
    NoObject,
    /// It could not be checked, and why.
    Unchecked {
        reason: String,
    },
}

impl Existence {
    pub fn missing(&self) -> bool {
        matches!(self, Self::NoPackage | Self::NoObject)
    }

    /// Why `path` is missing, for a refusal.
    pub fn reason(&self, path: &str) -> Option<String> {
        let package = path.split('.').next().unwrap_or(path);
        match self {
            Self::NoPackage => Some(format!(
                "{path}: neither the game nor an enabled mod has {package}"
            )),
            Self::NoObject => Some(format!("{path}: {package} exports nothing by that name")),
            _ => None,
        }
    }
}

/// How a refusal over missing objects starts, so a caller can offer to go ahead anyway.
pub const MISSING: &str = "Nothing is at the path these edits point at";

/// [`objects_exist`] for one path.
pub fn object_exists(game_root: &str, container: &str, path: &str) -> Existence {
    objects_exist(game_root, container, &[path.to_string()])
        .pop()
        .unwrap_or(Existence::NoPackage)
}

/// Looks each path up in the base game, the container being read from when it is one, and the
/// enabled mods. A path without an object part names a package, which only has to be there.
pub fn objects_exist(game_root: &str, container: &str, paths: &[String]) -> Vec<Existence> {
    let is_utoc = container.to_ascii_lowercase().ends_with(".utoc");
    let target = Path::new(container)
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|_| is_utoc)
        .unwrap_or("pakchunk0-Windows")
        .to_string();
    let mut stores = Stores {
        paks: paks_dir(game_root),
        target,
        base: None,
        mods: None,
    };
    paths
        .iter()
        .map(|path| {
            let path = path.trim();
            let (package, object) = match path.split_once('.') {
                Some((package, object)) => (package, Some(object)),
                None => (path, None),
            };
            if package.starts_with("/Script/") {
                return script_exists(&stores.paks, path);
            }
            let id = FPackageId::from_name(package);
            let hash = object
                .map(|object| get_public_export_hash(&object.replace(':', "/").to_lowercase()));
            let base = match stores.base() {
                Ok(base) => base,
                Err(reason) => return Existence::Unchecked { reason },
            };
            let mut found = look_in(&*base, id, hash);
            if found != Some(Existence::Found) {
                for store in stores.mods() {
                    match look_in(&**store, id, hash) {
                        Some(Existence::Found) => {
                            found = Some(Existence::Found);
                            break;
                        }
                        Some(other) if found.is_none() => found = Some(other),
                        _ => {}
                    }
                }
            }
            match found {
                Some(found) => found,
                None if !is_utoc => Existence::Unchecked {
                    reason: format!(
                        "{package} is not in the game or an IoStore mod, and a plain pak or a \
                         loose file cannot be looked in"
                    ),
                },
                None => Existence::NoPackage,
            }
        })
        .collect()
}

/// The containers a lookup reads, each opened on first use.
struct Stores {
    paks: PathBuf,
    target: String,
    base: Option<Result<Arc<dyn IoStoreTrait>, String>>,
    mods: Option<Vec<Box<dyn IoStoreTrait>>>,
}

impl Stores {
    fn base(&mut self) -> Result<Arc<dyn IoStoreTrait>, String> {
        let (paks, target) = (&self.paks, &self.target);
        self.base
            .get_or_insert_with(|| {
                open_base_game_paks(paks, target)
                    .map_err(|e| format!("the game's containers could not be read: {e}"))
            })
            .clone()
    }

    fn mods(&mut self) -> &[Box<dyn IoStoreTrait>] {
        let paks = &self.paks;
        self.mods.get_or_insert_with(|| {
            crate::import_index::enabled_mod_containers(paks)
                .into_iter()
                .filter_map(|container| {
                    let name = container.file_stem()?.to_str()?.to_string();
                    open_target_only(paks, &container, &name).ok()
                })
                .collect()
        })
    }
}

fn script_exists(paks: &Path, path: &str) -> Existence {
    let lower = path.to_ascii_lowercase();
    let listed = || {
        crate::script_objects::current().iter().any(|known| {
            let known = known.to_ascii_lowercase();
            known == lower || (!path.contains('.') && known.starts_with(&format!("{lower}.")))
        })
    };
    match script_objects(paks) {
        Ok(known) if known.contains(&FPackageObjectIndex::create_script_import(path)) => {
            Existence::Found
        }
        Ok(_) if listed() => Existence::Found,
        Ok(_) if path.contains('.') => Existence::NoObject,
        Ok(_) => Existence::NoPackage,
        Err(reason) => Existence::Unchecked { reason },
    }
}

/// Whether `store` holds the package, and if so whether it exports the object `hash` names.
/// `None` when it does not hold the package at all.
fn look_in(store: &dyn IoStoreTrait, id: FPackageId, hash: Option<u64>) -> Option<Existence> {
    let entry = store.package_store_entry(id)?;
    let Some(hash) = hash else {
        return Some(Existence::Found);
    };
    let chunk = FIoChunkId::from_package_id(id, 0, EIoChunkType::ExportBundleData);
    let container = store
        .child_containers()
        .find(|child| child.has_chunk_id(chunk))?;
    let (toc_version, header_version) = (
        container.container_file_version()?,
        container.container_header_version()?,
    );
    let data = store.read(chunk).ok()?;
    let header = FZenPackageHeader::deserialize(
        &mut Cursor::new(&data),
        Some(entry),
        toc_version,
        header_version,
        Some(EngineVersion::UE5_3.package_file_version()),
    )
    .ok()?;
    Some(
        if header
            .export_map
            .iter()
            .any(|export| export.public_export_hash == hash)
        {
            Existence::Found
        } else {
            Existence::NoObject
        },
    )
}

/// The game's native objects, from the global container's script objects table, read once per
/// install.
fn script_objects(paks: &Path) -> Result<HashSet<FPackageObjectIndex>, String> {
    type Read = Mutex<HashMap<PathBuf, HashSet<FPackageObjectIndex>>>;
    static READ: OnceLock<Read> = OnceLock::new();
    let read = READ.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(found) = read.lock().map_err(|e| e.to_string())?.get(paks) {
        return Ok(found.clone());
    }
    let store = open_base_game_paks(paks, "pakchunk0-Windows")?;
    let objects = store
        .load_script_objects()
        .map_err(|e| format!("the game's script objects could not be read: {e}"))?;
    let known: HashSet<FPackageObjectIndex> =
        objects.script_object_lookup.keys().copied().collect();
    read.lock()
        .map_err(|e| e.to_string())?
        .insert(paks.to_path_buf(), known.clone());
    Ok(known)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real export, a native class and its package, a wrong name in a real package, a made-up
    /// package, and a real package named on its own.
    #[test]
    fn paths_are_found_in_the_game_or_said_to_be_missing() {
        let Ok(root) = std::env::var("RIVALS_GAME_ROOT") else {
            return;
        };
        let container = format!(
            "{}/MarvelGame/Marvel/Content/Paks/pakchunk0-Windows.utoc",
            root.replace('\\', "/")
        );
        let titles = "/Game/Marvel/Data/DataAsset/Career/MarvelHeroTitleData";
        let found = objects_exist(
            &root,
            &container,
            &[
                format!("{titles}.MarvelHeroTitleData"),
                "/Script/Engine.StaticMesh".into(),
                "/Script/Engine".into(),
                format!("{titles}.NoSuchObject"),
                "/Game/Made/Up/Nothing.Nothing".into(),
                titles.into(),
                "/Script/Engine.NoSuchClass".into(),
            ],
        );
        use Existence::*;
        assert_eq!(
            found,
            vec![Found, Found, Found, NoObject, NoPackage, Found, NoObject]
        );
    }
}
