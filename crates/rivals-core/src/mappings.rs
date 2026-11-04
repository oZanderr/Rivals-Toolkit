//! Finds and caches the .usmap schema file that unversioned property parsing depends on.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::SystemTime;

use rivals_uasset::Mappings;

/// What tells a file from the one that replaced it: when it was written, as finely as the file
/// system records it, and how long it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    modified: Option<SystemTime>,
    len: u64,
}

impl Stamp {
    fn of(path: &Path) -> Result<Self, String> {
        let meta = std::fs::metadata(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        Ok(Self {
            modified: meta.modified().ok(),
            len: meta.len(),
        })
    }
}

type Parsed = Arc<OnceLock<Result<Arc<Mappings>, String>>>;

/// The file being read with and its parse, shared by every thread asking for it.
struct Slot {
    path: PathBuf,
    stamp: Stamp,
    parsed: Parsed,
}

/// The mappings file in use, parsed once: parsing a whole game's takes long enough that doing it
/// per asset click would show. Only one file is held, so the one a game patch replaced or the one
/// the app stopped using is let go rather than kept beside it.
struct Current {
    slot: Mutex<Option<Slot>>,
}

impl Current {
    const fn new() -> Self {
        Self {
            slot: Mutex::new(None),
        }
    }

    fn load(&self, path: &Path) -> Result<Arc<Mappings>, String> {
        let stamp = Stamp::of(path)?;
        let parsed = {
            let mut slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
            match slot.as_ref() {
                Some(held) if held.path == path && held.stamp == stamp => held.parsed.clone(),
                _ => {
                    let parsed = Parsed::default();
                    *slot = Some(Slot {
                        path: path.to_path_buf(),
                        stamp,
                        parsed: parsed.clone(),
                    });
                    parsed
                }
            }
        };
        // Parsed outside the lock: a thread asking for this file waits for the one parse of it,
        // and nothing else waits. The parse uses no thread pool, so it cannot be handed work that
        // asks for the file again.
        let result = parsed
            .get_or_init(|| {
                let bytes =
                    std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
                Mappings::load(&bytes).map(Arc::new)
            })
            .clone();
        if result.is_err() {
            // A file that did not read is read again next time, in case it was still being written.
            let mut slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
            if slot
                .as_ref()
                .is_some_and(|held| Arc::ptr_eq(&held.parsed, &parsed))
            {
                *slot = None;
            }
        }
        result
    }
}

static CURRENT: Current = Current::new();

#[derive(Clone)]
pub struct MappingsStatus {
    pub path: Option<PathBuf>,
    pub struct_count: usize,
    pub enum_count: usize,
    /// Why the file did not load, when it did not.
    pub error: Option<String>,
}

/// The mappings file at `path`, parsed once for as long as it is the one asked for and unchanged.
pub fn load(path: &Path) -> Result<Arc<Mappings>, String> {
    CURRENT.load(path)
}

/// Check bytes really are a mappings file, answering what they hold. Nothing is cached: this is
/// for validating a download before it replaces the file in use.
pub fn parse_check(bytes: &[u8]) -> Result<(usize, usize), String> {
    let mappings = Mappings::load(bytes)?;
    Ok((mappings.struct_count(), mappings.enum_count()))
}

pub fn status(path: Option<&Path>) -> MappingsStatus {
    let Some(path) = path else {
        return MappingsStatus {
            path: None,
            struct_count: 0,
            enum_count: 0,
            error: None,
        };
    };
    match load(path) {
        Ok(mappings) => MappingsStatus {
            path: Some(path.to_path_buf()),
            struct_count: mappings.struct_count(),
            enum_count: mappings.enum_count(),
            error: None,
        },
        Err(error) => MappingsStatus {
            path: Some(path.to_path_buf()),
            struct_count: 0,
            enum_count: 0,
            error: Some(error),
        },
    }
}

/// An explicit path, else the configured one. Returns the newest `.usmap` in a folder when
/// handed a folder rather than a file.
///
/// Nowhere else is searched. Guessing at conventional spots meant the file in use was whichever
/// one happened to be found first, which is the wrong one to read a patched game with, and the
/// failure had to recite every path it had tried to explain itself. For the same reason an
/// explicit path that is not there is an error, not a reason to read with the configured one.
pub fn resolve(explicit: Option<&str>, configured: Option<&str>) -> Result<PathBuf, String> {
    let given = explicit.or(configured);
    if let Some(found) = given.and_then(|path| pick(Path::new(path))) {
        return Ok(found);
    }

    Err(format!(
        "{} Reading asset properties needs one because Marvel Rivals ships unversioned properties.",
        if given.is_some() {
            "the .usmap mappings file that is set is not there any more."
        } else {
            "no .usmap mappings file is set."
        }
    ))
}

/// The game build a mappings file was dumped from, as its name carries it: the number after the
/// engine version in `5.3.2-3870120+++depot_marvel+S10.0_release-Marvel.usmap`.
pub fn mappings_build(path: &Path) -> Option<u64> {
    let name = path.file_name()?.to_str()?;
    let (_, after) = name.split_once('-')?;
    let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// The game's build: the changelist `MarvelGame/version.json` records, or the newest build a
/// patch container names, such as `Patch_-Windows_1.1.3892207_P.utoc`. A season update folds its
/// patches into the base containers and leaves none, so the version file is what says it then.
pub fn game_build(game_root: &str) -> Option<u64> {
    let recorded = std::fs::read(std::path::Path::new(game_root).join("MarvelGame/version.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|version| version.get("changelist")?.as_u64());
    let patched = std::fs::read_dir(crate::paths::paks_dir(game_root))
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let version = name.strip_prefix("Patch_")?.strip_suffix("_P.utoc")?;
            version.rsplit('.').next()?.parse::<u64>().ok()
        })
        .max();
    recorded.max(patched)
}

/// What to say when the mappings were dumped from an older build than the game's: a class changed
/// since then reads wrong, or not at all, until the mappings match.
pub fn drift_warning(game_root: &str, mappings: &Path) -> Option<String> {
    let dumped = mappings_build(mappings)?;
    let game = game_build(game_root)?;
    (dumped < game).then(|| {
        format!(
            "the mappings file is for build {dumped}, and the game has been patched to build \
             {game}: a class changed since reads wrong or not at all until the mappings are for \
             build {game}"
        )
    })
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

    #[test]
    fn mappings_older_than_the_newest_patch_are_reported() {
        let root = scratch("drift");
        let paks = crate::paths::paks_dir(&root.to_string_lossy());
        std::fs::create_dir_all(&paks).expect("paks");
        for name in [
            "Patch_-Windows_1.1.3870120_P.utoc",
            "Patch_-Windows_1.1.3892207_P.utoc",
            "pakchunk0-Windows.utoc",
        ] {
            std::fs::write(paks.join(name), b"").expect("write");
        }
        let game = root.to_string_lossy().into_owned();
        assert_eq!(game_build(&game), Some(3892207));
        let old = Path::new("5.3.2-3870120+++depot_marvel+S10.0_release-Marvel.usmap");
        assert_eq!(mappings_build(old), Some(3870120));
        let warning = drift_warning(&game, old).expect("a warning");
        assert!(
            warning.contains("3870120") && warning.contains("3892207"),
            "{warning}"
        );
        let current = Path::new("5.3.2-3892207+++depot_marvel+S10.0_release-Marvel.usmap");
        assert!(drift_warning(&game, current).is_none());
        assert!(drift_warning(&game, Path::new("Mappings.usmap")).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A season update leaves no patch container, and the version file still gives the build.
    #[test]
    fn the_game_s_build_comes_from_its_version_file_without_a_patch() {
        let root = scratch("version");
        let paks = crate::paths::paks_dir(&root.to_string_lossy());
        std::fs::create_dir_all(&paks).expect("paks");
        std::fs::write(paks.join("pakchunk0-Windows.utoc"), b"").expect("write");
        let game = root.to_string_lossy().into_owned();
        assert_eq!(game_build(&game), None);
        std::fs::write(
            root.join("MarvelGame/version.json"),
            br#"{"version": "1.1.3922109", "changelist": 3922109, "NonUFSFiles": []}"#,
        )
        .expect("write");
        assert_eq!(game_build(&game), Some(3922109));
        let old = Path::new("5.3.2-3870120+++depot_marvel+S10.0_release-Marvel.usmap");
        let warning = drift_warning(&game, old).expect("a warning");
        assert!(warning.contains("3922109"), "{warning}");
        let current = Path::new("5.3.2-3922109+++depot_marvel+S10.5_release-Marvel.usmap");
        assert!(drift_warning(&game, current).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    fn scratch(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("rivals-mappings-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create dir");
        root
    }

    /// A mappings file of empty structs with these names, written at `path`.
    fn write_usmap(path: &Path, structs: &[&str]) {
        let usmap = usmap::Usmap {
            enums: Vec::new(),
            structs: structs
                .iter()
                .map(|name| usmap::Struct {
                    name: name.to_string(),
                    super_struct: None,
                    properties: Vec::new(),
                })
                .collect(),
            cext: None,
            ppth: None,
            eatr: None,
            envp: None,
        };
        let mut bytes = Vec::new();
        usmap.write(&mut bytes).expect("write the usmap");
        std::fs::write(path, bytes).expect("write");
    }

    /// The same file is parsed once however often it is asked for. Rewritten, it is parsed again,
    /// and the copy it replaced is let go.
    #[test]
    fn a_file_is_parsed_once_and_again_only_once_rewritten() {
        let root = scratch("cache");
        let path = root.join("game.usmap");
        write_usmap(&path, &["A"]);
        let current = Current::new();
        let first = current.load(&path).expect("load");
        assert!(Arc::ptr_eq(&first, &current.load(&path).expect("again")));

        write_usmap(&path, &["A", "B"]);
        let second = current.load(&path).expect("rewritten");
        assert_eq!((first.struct_count(), second.struct_count()), (1, 2));
        assert!(Arc::ptr_eq(&second, &current.load(&path).expect("again")));
        assert_eq!(Arc::strong_count(&first), 1, "the replaced copy is let go");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A file rewritten to the same size within the same second still reads as rewritten, by
    /// the time the file system gives it.
    #[test]
    fn a_rewrite_of_the_same_size_is_told_apart_by_its_time() {
        let root = scratch("same-size");
        let path = root.join("game.usmap");
        write_usmap(&path, &["A"]);
        let written = std::fs::metadata(&path).unwrap().modified().unwrap();
        let current = Current::new();
        let first = current.load(&path).expect("load");
        write_usmap(&path, &["B"]);
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(written + std::time::Duration::from_millis(1))
            .unwrap();
        drop(file);
        let second = current.load(&path).expect("rewritten");
        assert!(!Arc::ptr_eq(&first, &second));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Switching to another file lets the one used before go.
    #[test]
    fn only_the_file_in_use_is_held() {
        let root = scratch("switch");
        let (old, new) = (root.join("old.usmap"), root.join("new.usmap"));
        write_usmap(&old, &["A"]);
        write_usmap(&new, &["A", "B"]);
        let current = Current::new();
        let before = current.load(&old).expect("old");
        let after = current.load(&new).expect("new");
        assert_eq!(Arc::strong_count(&before), 1);
        assert!(Arc::ptr_eq(&after, &current.load(&new).expect("again")));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Threads asking for a file nothing has parsed yet wait for one parse of it and share it.
    #[test]
    fn threads_asking_at_once_share_one_parse() {
        let root = scratch("threads");
        let path = root.join("game.usmap");
        let names: Vec<String> = (0..20_000).map(|at| format!("S{at}")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        write_usmap(&path, &names);
        let current = Current::new();
        let start = std::sync::Barrier::new(8);
        let loaded: Vec<Arc<Mappings>> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        start.wait();
                        current.load(&path).expect("load")
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert!(loaded.iter().all(|each| Arc::ptr_eq(each, &loaded[0])));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A file that does not parse says why, and is read again once it does.
    #[test]
    fn a_file_that_does_not_parse_is_not_kept() {
        let root = scratch("broken");
        let path = root.join("game.usmap");
        std::fs::write(&path, b"not a mappings file").unwrap();
        let current = Current::new();
        assert!(current.load(&path).is_err());
        assert!(current.slot.lock().unwrap().is_none());
        write_usmap(&path, &["A"]);
        assert_eq!(current.load(&path).expect("fixed").struct_count(), 1);
        let status = status(Some(&root.join("missing.usmap")));
        assert!(status.error.is_some_and(|e| e.contains("missing.usmap")));
        let _ = std::fs::remove_dir_all(&root);
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
        let missing = root.join("missing.usmap");
        let refused = resolve(
            Some(&missing.display().to_string()),
            Some(&configured.display().to_string()),
        )
        .expect_err("a missing explicit path is not swapped for the configured one");
        assert!(refused.contains("not there any more"), "{refused}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn failing_to_resolve_explains_why_a_mappings_file_is_needed() {
        let err = resolve(Some("Z:/nope/missing.usmap"), None).expect_err("should fail");
        assert!(err.contains("unversioned properties"), "{err}");
    }

    /// Naming a path that no longer exists reads differently from never having set one, and
    /// neither says anything about where else a file might have been.
    #[test]
    fn failing_to_resolve_names_no_paths_of_its_own() {
        let missing = resolve(Some("Z:/nope/missing.usmap"), None).expect_err("should fail");
        assert!(missing.contains("not there any more"), "{missing}");
        assert!(!missing.contains("Z:/nope"), "{missing}");

        let unset = resolve(None, None).expect_err("should fail");
        assert!(unset.contains("no .usmap mappings file is set"), "{unset}");
    }

    /// A file sitting in one of the spots the resolver used to guess at is not picked up.
    #[test]
    fn nothing_is_resolved_without_being_asked_for() {
        let root = scratch("unsearched");
        std::fs::write(root.join("Mappings.usmap"), b"x").expect("write");
        assert!(resolve(None, None).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
