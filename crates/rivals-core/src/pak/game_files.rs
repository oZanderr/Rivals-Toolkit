//! Files the base game ships inside its `.pak` files rather than as packages, such as the
//! translation tables. A patch pak overrides the base paks it follows.

use std::collections::HashMap;
use std::fs;
use std::io::BufReader;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use super::crypto::open_pak;
use super::profile::strip_mount_prefix;
use crate::paths::paks_dir;

/// One opened pak and its entries, each as it names itself and as a path from the game root.
struct OpenPak {
    path: PathBuf,
    reader: repak::PakReader,
    entries: HashMap<String, String>,
}

/// What the paks were when they were opened, so a game update opens them again.
type Signature = Vec<(PathBuf, u64, Option<SystemTime>)>;

/// The paks opened for each `Paks` folder, with what they were when opened.
type Opened = HashMap<PathBuf, (Signature, Arc<Vec<OpenPak>>)>;

/// The base game's paks, patch paks last, opened once for as long as none of them changes.
fn base_paks(game_root: &str) -> Result<Arc<Vec<OpenPak>>, String> {
    static OPENED: OnceLock<Mutex<Opened>> = OnceLock::new();
    let dir = paks_dir(game_root);
    let mut found: Vec<(PathBuf, u64, Option<SystemTime>)> = fs::read_dir(&dir)
        .map_err(|e| format!("read {}: {e}", dir.display()))?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "pak"))
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            meta.is_file()
                .then(|| (entry.path(), meta.len(), meta.modified().ok()))
        })
        .collect();
    let is_patch = |path: &PathBuf| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.to_ascii_lowercase().starts_with("patch_"))
    };
    found.sort_by(|a, b| is_patch(&a.0).cmp(&is_patch(&b.0)).then(a.0.cmp(&b.0)));

    let opened = OPENED.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some((signature, paks)) = opened.lock().map_err(|e| e.to_string())?.get(&dir)
        && *signature == found
    {
        return Ok(paks.clone());
    }
    let mut paks = Vec::with_capacity(found.len());
    for (path, ..) in &found {
        // A pak this key cannot open holds nothing this can read either.
        let Ok(reader) = open_pak(path) else {
            continue;
        };
        let entries = reader
            .files()
            .into_iter()
            .map(|name| (strip_mount_prefix(&name).to_ascii_lowercase(), name))
            .collect();
        paks.push(OpenPak {
            path: path.clone(),
            reader,
            entries,
        });
    }
    let paks = Arc::new(paks);
    opened
        .lock()
        .map_err(|e| e.to_string())?
        .insert(dir, (found, paks.clone()));
    Ok(paks)
}

/// The bytes of `path`, a path from the game root such as `Marvel/Content/...`, from the last
/// base pak holding it. `None` when no base pak does.
pub fn read(game_root: &str, path: &str) -> Result<Option<Vec<u8>>, String> {
    let wanted = strip_mount_prefix(path).to_ascii_lowercase();
    let paks = base_paks(game_root)?;
    let Some(pak) = paks
        .iter()
        .rev()
        .find(|pak| pak.entries.contains_key(&wanted))
    else {
        return Ok(None);
    };
    let name = &pak.entries[&wanted];
    let mut file = BufReader::new(
        fs::File::open(&pak.path).map_err(|e| format!("open {}: {e}", pak.path.display()))?,
    );
    pak.reader
        .get(name, &mut file)
        .map(Some)
        .map_err(|e| format!("read {name} from {}: {e}", pak.path.display()))
}

/// Every path the base paks hold that ends with `suffix`, compared without case, as paths from
/// the game root in lower case, each once.
pub fn list(game_root: &str, suffix: &str) -> Result<Vec<String>, String> {
    let suffix = suffix.to_ascii_lowercase();
    let mut paths: Vec<String> = base_paks(game_root)?
        .iter()
        .flat_map(|pak| pak.entries.keys())
        .filter(|path| path.ends_with(&suffix))
        .cloned()
        .collect();
    paths.sort();
    paths.dedup();
    Ok(paths)
}
