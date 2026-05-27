//! Per-container vanilla extract and rebuild: dump a single game container's pak + utoc/ucas content to a legacy tree, then rebuild a modified tree back into a swap-ready container set.

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
}
