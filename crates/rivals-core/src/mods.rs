//! Mod-folder file discovery.

use std::path::{Path, PathBuf};

/// Collect relative paths of mod-related files (.pak, .ucas, .utoc, and their
/// `.disabled` variants) under the given root directory. When `recursive` is
/// false, only direct children of `root` are scanned (matches UE's native
/// `~mods` load behavior).
pub fn walk_mod_files(root: &Path, recursive: bool) -> Vec<PathBuf> {
    let mut walker = walkdir::WalkDir::new(root);
    if !recursive {
        walker = walker.max_depth(1);
    }
    walker
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            let name = e.file_name().to_string_lossy();
            let base = name.strip_suffix(".disabled").unwrap_or(&name);
            matches!(
                Path::new(base).extension().and_then(|x| x.to_str()),
                Some("pak" | "ucas" | "utoc")
            )
        })
        .filter_map(|e| e.path().strip_prefix(root).ok().map(|r| r.to_path_buf()))
        .collect()
}
