//! Vanilla container extract and rebuild: shared types for the round-trip between a game container and an editable legacy tree.

pub(crate) mod extract;
pub(crate) mod rebuild;

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

pub(crate) const REBUILD_MANIFEST_FILENAME: &str = "rebuild_manifest.json";
pub(crate) const REBUILD_MANIFEST_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
pub(crate) struct RebuildManifest {
    pub(crate) version: u32,
    /// Source utoc file stem (e.g. `pakchunk0-Windows`); rebuild rejects mismatches.
    pub(crate) source_container: String,
    pub(crate) entries: HashMap<String, String>,
    /// Mount-stripped pak-entry paths the source stored uncompressed; rebuilt verbatim (no Oodle)
    /// so raw-shipped entries (e.g. `Marvel/AssetRegistry.bin`, `.locres`) reproduce exactly.
    #[serde(default)]
    pub(crate) uncompressed_pak_entries: Vec<String>,
}
