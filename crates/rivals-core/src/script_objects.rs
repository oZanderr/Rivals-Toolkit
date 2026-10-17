//! Supplies the native object paths retoc needs to name imports the game's script objects table leaves out.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Paths for native objects the game registers at runtime but never cooked into global.utoc.
const BUNDLED: &str = include_str!("../resources/script_objects.txt");

/// A list the user pointed the process at, read fresh each time a conversion context is built.
static USER_LIST: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Points every conversion in this process at a user list, on top of the bundled one.
pub fn set_user_list(path: Option<PathBuf>) {
    if let Ok(mut slot) = USER_LIST.lock() {
        *slot = path;
    }
}

/// An explicit path, else the configured one, else nothing: the bundled list alone is the normal
/// case. A path that is set but not there is an error rather than a silent fallback.
pub fn resolve(
    explicit: Option<&str>,
    configured: Option<&str>,
) -> Result<Option<PathBuf>, String> {
    let Some(candidate) = explicit.or(configured) else {
        return Ok(None);
    };
    let path = PathBuf::from(candidate);
    if path.is_file() {
        Ok(Some(path))
    } else {
        Err("the script objects list that is set is not there any more.".to_string())
    }
}

pub fn load(path: &Path) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    parse_list(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// One object path per line, `/Script/Pkg`, `/Script/Pkg.Object` or `/Script/Pkg.Object:Sub`.
/// Blank lines and `#` comments are skipped; anything else names its line in the error.
pub fn parse_list(text: &str) -> Result<Vec<String>, String> {
    let mut paths = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        check_path(line).map_err(|e| format!("line {}: {e}", index + 1))?;
        paths.push(line.to_string());
    }
    Ok(paths)
}

fn check_path(line: &str) -> Result<(), String> {
    let Some(rest) = line.strip_prefix("/Script/") else {
        return Err(format!("{line} does not start with /Script/"));
    };
    if line.contains('.') {
        rivals_uasset::parse_object_path(line).map(|_| ())
    } else if rest.is_empty() || rest.contains(['/', ':']) || rest.contains(char::is_whitespace) {
        Err(format!(
            "{line} is not a package path; expected /Script/Module"
        ))
    } else {
        Ok(())
    }
}

/// The bundled entries plus the user list, for a conversion context. A user list that fails to
/// read here was validated when it was set, so it is dropped rather than failing the read.
pub fn current() -> Vec<String> {
    let mut paths = parse_list(BUNDLED).unwrap_or_default();
    let user = USER_LIST.lock().ok().and_then(|slot| slot.clone());
    if let Some(path) = user {
        paths.extend(load(&path).unwrap_or_default());
    }
    paths
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "rivals-script-objects-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create dir");
        root
    }

    #[test]
    fn the_bundled_list_names_the_patch_utility() {
        let paths = parse_list(BUNDLED).expect("bundled list parses");
        assert_eq!(paths.len(), 3 + 10 + 60);
        for expected in [
            "/Script/NePatchUtility",
            "/Script/NePatchUtility.NePatchUtility",
            "/Script/NePatchUtility.Default__NePatchUtility",
            "/Script/NePatchUtility.NePatchUtility:MountPak",
            "/Script/NePatchUtility.NePatchUtility:GetMountedPakNames",
        ] {
            assert!(paths.iter().any(|p| p == expected), "{expected}");
        }
        assert!(current().len() >= paths.len());
    }

    #[test]
    fn comments_and_blanks_are_skipped_and_bad_lines_are_named() {
        let text = "# heading\n\n  /Script/Foo  \n/Script/Foo.Bar:Baz\n";
        assert_eq!(
            parse_list(text).expect("parses"),
            vec!["/Script/Foo".to_string(), "/Script/Foo.Bar:Baz".to_string()]
        );

        let err = parse_list("/Script/Foo\n/Game/Foo.Bar\n").expect_err("should fail");
        assert!(err.starts_with("line 2:"), "{err}");
        let err = parse_list("/Script/Foo.\n").expect_err("should fail");
        assert!(err.starts_with("line 1:"), "{err}");
        let err = parse_list("/Script/Foo/Bar\n").expect_err("should fail");
        assert!(err.contains("package path"), "{err}");
    }

    #[test]
    fn resolve_prefers_explicit_and_refuses_a_missing_path_without_naming_it() {
        let root = scratch("resolve");
        let explicit = root.join("explicit.txt");
        let configured = root.join("configured.txt");
        std::fs::write(&explicit, "/Script/A\n").expect("write");
        std::fs::write(&configured, "/Script/B\n").expect("write");
        let explicit_text = explicit.display().to_string();
        let configured_text = configured.display().to_string();

        let picked = resolve(Some(&explicit_text), Some(&configured_text)).expect("resolves");
        assert_eq!(picked, Some(explicit));
        let picked = resolve(None, Some(&configured_text)).expect("resolves");
        assert_eq!(picked, Some(configured));
        assert_eq!(resolve(None, None).expect("nothing set is fine"), None);

        let err = resolve(None, Some("Z:/nope/list.txt")).expect_err("should fail");
        assert!(err.contains("not there any more"), "{err}");
        assert!(!err.contains("Z:/nope"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
