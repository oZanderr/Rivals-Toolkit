//! Detects asset conflicts between installed mod pak files, through the app's listing caches.

use std::path::Path;

use rivals_core::mod_library::{self, ContentLister};

use crate::pak;

pub(crate) use rivals_core::mod_library::ConflictReport;

/// Answers from the app's listing caches, which the mod browser fills as well.
pub(crate) struct CachedLister;

impl ContentLister for CachedLister {
    fn utoc(&self, utoc: &Path) -> Result<Vec<String>, String> {
        pak::list_utoc_contents(&utoc.to_string_lossy())
    }

    fn pak(&self, pak_file: &Path) -> Result<Vec<String>, String> {
        pak::list_pak_contents(&pak_file.to_string_lossy())
    }
}

/// Check all enabled mods for asset-level conflicts (same file path in multiple paks).
pub(crate) fn check_conflicts(game_root: &str, recursive: bool) -> Result<ConflictReport, String> {
    mod_library::check_conflicts(game_root, recursive, &CachedLister)
}
