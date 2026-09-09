//! Finds and caches the .usmap schema file that unversioned property parsing depends on.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use rivals_uasset::Mappings;

/// Parsing a full game mappings file takes long enough that doing it per asset click would show.
/// Keyed by path and modification time so replacing the file after a game patch is picked up.
type Cache = Mutex<HashMap<(PathBuf, u64), Arc<Mappings>>>;
static CACHE: LazyLock<Cache> = LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Clone)]
pub struct MappingsStatus {
    pub path: Option<PathBuf>,
    pub struct_count: usize,
    pub enum_count: usize,
}

pub fn load(path: &Path) -> Result<Arc<Mappings>, String> {
    let key = (path.to_path_buf(), modified_stamp(path));
    if let Ok(cache) = CACHE.lock()
        && let Some(hit) = cache.get(&key)
    {
        return Ok(hit.clone());
    }

    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mappings = Arc::new(Mappings::load(&bytes)?);
    if let Ok(mut cache) = CACHE.lock() {
        // A stale entry per replaced file is not worth tracking; the map stays tiny either way.
        cache.insert(key, mappings.clone());
    }
    Ok(mappings)
}

pub fn status(path: Option<&Path>) -> MappingsStatus {
    let Some(path) = path else {
        return MappingsStatus {
            path: None,
            struct_count: 0,
            enum_count: 0,
        };
    };
    match load(path) {
        Ok(mappings) => MappingsStatus {
            path: Some(path.to_path_buf()),
            struct_count: mappings.struct_count(),
            enum_count: mappings.enum_count(),
        },
        Err(_) => MappingsStatus {
            path: Some(path.to_path_buf()),
            struct_count: 0,
            enum_count: 0,
        },
    }
}

/// Resolution order: an explicit path, then the configured one, then the conventional spots.
/// Returns the newest `.usmap` in a folder when handed a folder rather than a file.
pub fn resolve(explicit: Option<&str>, configured: Option<&str>) -> Result<PathBuf, String> {
    let mut tried = Vec::new();
    for candidate in [explicit, configured].into_iter().flatten() {
        let path = Path::new(candidate);
        if let Some(found) = pick(path) {
            return Ok(found);
        }
        tried.push(candidate.to_string());
    }

    for fallback in conventional_locations() {
        if let Some(found) = pick(&fallback) {
            return Ok(found);
        }
        tried.push(fallback.display().to_string());
    }

    Err(format!(
        "no .usmap mappings file found. Reading asset properties needs one because Marvel Rivals ships unversioned properties. Looked in: {}",
        tried.join(", ")
    ))
}

fn pick(path: &Path) -> Option<PathBuf> {
    if path.is_file() {
        return Some(path.to_path_buf());
    }
    if !path.is_dir() {
        return None;
    }
    let mut newest: Option<(u64, PathBuf)> = None;
    for entry in std::fs::read_dir(path).ok()?.flatten() {
        let candidate = entry.path();
        if candidate.extension().and_then(|e| e.to_str()) != Some("usmap") {
            continue;
        }
        let stamp = modified_stamp(&candidate);
        if newest.as_ref().is_none_or(|(best, _)| stamp > *best) {
            newest = Some((stamp, candidate));
        }
    }
    newest.map(|(_, path)| path)
}

fn conventional_locations() -> Vec<PathBuf> {
    let mut locations = Vec::new();
    if let Some(config) = dirs::config_dir() {
        let app = config.join("rivals-toolkit");
        locations.push(app.join("Mappings.usmap"));
        locations.push(app.join("mappings"));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        locations.push(dir.join("Mappings.usmap"));
        locations.push(dir.join("mappings"));
    }
    locations
}

fn modified_stamp(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("rivals-mappings-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create dir");
        root
    }

    #[test]
    fn a_folder_resolves_to_the_newest_usmap_inside_it() {
        let root = scratch("folder");
        std::fs::write(root.join("old.usmap"), b"x").expect("write");
        std::fs::write(root.join("notes.txt"), b"x").expect("write");
        let picked = pick(&root).expect("should pick a usmap");
        assert_eq!(picked.extension().and_then(|e| e.to_str()), Some("usmap"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_folder_with_no_mappings_inside_resolves_to_nothing() {
        let root = scratch("empty");
        std::fs::write(root.join("notes.txt"), b"x").expect("write");
        assert!(pick(&root).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_explicit_path_wins_over_the_configured_one() {
        let root = scratch("explicit");
        let explicit = root.join("explicit.usmap");
        let configured = root.join("configured.usmap");
        std::fs::write(&explicit, b"x").expect("write");
        std::fs::write(&configured, b"x").expect("write");
        let resolved = resolve(
            Some(&explicit.display().to_string()),
            Some(&configured.display().to_string()),
        )
        .expect("resolve");
        assert_eq!(resolved, explicit);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn failing_to_resolve_explains_why_a_mappings_file_is_needed() {
        let err = resolve(Some("Z:/nope/missing.usmap"), None).expect_err("should fail");
        assert!(err.contains("unversioned properties"), "{err}");
    }
}
