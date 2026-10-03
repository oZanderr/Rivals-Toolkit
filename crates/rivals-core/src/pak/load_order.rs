//! Reads the game's containers in the order it mounts them, the enabled mods above the base game,
//! so a package comes from the copy the game loads and a mod's package converts with the game
//! beside it.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use retoc::container_header::{EIoContainerHeaderVersion, StoreEntry};
use retoc::iostore::{ChunkInfo, IoStoreTrait, PackageInfo};
use retoc::{EIoChunkType, EIoStoreTocVersion, FIoChunkId, FIoChunkIdRaw, FPackageId};

use super::containers::{open_base_game_paks, open_target_only};
use crate::paths::paks_dir;

/// One mounted layer: a mod's container alone, or the whole base game.
pub struct Layer {
    /// The mod's container name, or `None` for the base game.
    pub mod_name: Option<String>,
    pub store: Arc<dyn IoStoreTrait>,
}

/// Every layer, the one the game reads first leading, answering for each chunk and package from
/// the first layer that holds it.
pub struct LoadOrder {
    pub layers: Vec<Layer>,
    /// The base game, which answers for the store as a whole: its versions and script objects.
    base: Arc<dyn IoStoreTrait>,
    /// Whether bulk payloads are read. Without them a package converts with its textures and
    /// sounds left out, which is all a reader of its values needs.
    payloads: bool,
}

/// The containers the game mounts: each enabled mod in the order the game prefers them when
/// `with_mods`, then the base game. A mod container that does not open is left out, as the game
/// leaves it.
pub fn open(game_root: &str, with_mods: bool, payloads: bool) -> Result<LoadOrder, String> {
    let paks = paks_dir(game_root);
    let mut layers = Vec::new();
    if with_mods {
        for container in crate::import_index::enabled_mod_containers(&paks) {
            let name = stem(&container);
            if let Ok(store) = open_target_only(&paks, &container, &name) {
                layers.push(Layer {
                    mod_name: Some(name),
                    store: Arc::from(store),
                });
            }
        }
    }
    let base = open_base_game_paks(&paks, BASE_CONTAINER)?;
    layers.push(Layer {
        mod_name: None,
        store: Arc::clone(&base),
    });
    Ok(LoadOrder {
        layers,
        base,
        payloads,
    })
}

/// The container whose siblings make up the base game.
const BASE_CONTAINER: &str = "pakchunk0-Windows";

fn stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string()
}

impl LoadOrder {
    fn base(&self) -> &dyn IoStoreTrait {
        self.base.as_ref()
    }

    fn holding(&self, chunk_id: FIoChunkId) -> Option<&dyn IoStoreTrait> {
        self.layers
            .iter()
            .map(|layer| layer.store.as_ref())
            .find(|store| store.has_chunk_id(chunk_id))
    }

    fn skipped(&self, chunk_id: FIoChunkId) -> bool {
        !self.payloads
            && matches!(
                chunk_id.get_chunk_type(),
                EIoChunkType::BulkData
                    | EIoChunkType::OptionalBulkData
                    | EIoChunkType::MemoryMappedBulkData
            )
    }
}

impl IoStoreTrait for LoadOrder {
    fn container_name(&self) -> &str {
        self.base().container_name()
    }
    fn container_file_version(&self) -> Option<EIoStoreTocVersion> {
        self.base().container_file_version()
    }
    fn container_header_version(&self) -> Option<EIoContainerHeaderVersion> {
        self.base().container_header_version()
    }
    fn compression_block_size(&self) -> Option<u32> {
        self.base().compression_block_size()
    }
    fn print_info(&self, depth: usize) {
        for layer in &self.layers {
            layer.store.print_info(depth);
        }
    }
    fn read(&self, chunk_id: FIoChunkId) -> retoc::anyhow::Result<Vec<u8>> {
        if self.skipped(chunk_id) {
            return Ok(Vec::new());
        }
        match self.holding(chunk_id) {
            Some(store) => store.read(chunk_id),
            None => self.base().read(chunk_id),
        }
    }
    fn read_raw(&self, chunk_id_raw: FIoChunkIdRaw) -> retoc::anyhow::Result<Vec<u8>> {
        let store = self
            .layers
            .iter()
            .map(|layer| layer.store.as_ref())
            .find(|store| store.has_chunk_id_raw(chunk_id_raw))
            .unwrap_or_else(|| self.base());
        store.read_raw(chunk_id_raw)
    }
    fn has_chunk_id(&self, chunk_id: FIoChunkId) -> bool {
        !self.skipped(chunk_id) && self.holding(chunk_id).is_some()
    }
    fn has_chunk_id_raw(&self, chunk_id_raw: FIoChunkIdRaw) -> bool {
        self.layers
            .iter()
            .any(|layer| layer.store.has_chunk_id_raw(chunk_id_raw))
    }
    fn chunks(&self) -> Box<dyn Iterator<Item = ChunkInfo<'_>> + Send + '_> {
        Box::new(self.layers.iter().flat_map(|layer| layer.store.chunks()))
    }
    fn chunks_all(&self) -> Box<dyn Iterator<Item = ChunkInfo<'_>> + Send + '_> {
        Box::new(
            self.layers
                .iter()
                .flat_map(|layer| layer.store.chunks_all()),
        )
    }
    fn packages(&self) -> Box<dyn Iterator<Item = PackageInfo<'_>> + Send + '_> {
        Box::new(self.layers.iter().flat_map(|layer| layer.store.packages()))
    }
    fn packages_all(&self) -> Box<dyn Iterator<Item = PackageInfo<'_>> + Send + '_> {
        // Each package once, from the layer the game reads it from.
        let mut seen = HashSet::new();
        Box::new(
            self.layers
                .iter()
                .flat_map(|layer| layer.store.packages_all())
                .filter(move |pkg| seen.insert(pkg.id().0)),
        )
    }
    fn child_containers(&self) -> Box<dyn Iterator<Item = &dyn IoStoreTrait> + '_> {
        Box::new(
            self.layers
                .iter()
                .flat_map(|layer| layer.store.child_containers()),
        )
    }
    fn chunk_path(&self, chunk_id: FIoChunkId) -> Option<String> {
        self.layers
            .iter()
            .find_map(|layer| layer.store.chunk_path(chunk_id))
    }
    fn package_store_entry(&self, package_id: FPackageId) -> Option<StoreEntry> {
        self.layers
            .iter()
            .find_map(|layer| layer.store.package_store_entry(package_id))
    }
    fn lookup_package_redirect(&self, source_package_id: FPackageId) -> Option<FPackageId> {
        self.layers
            .iter()
            .find_map(|layer| layer.store.lookup_package_redirect(source_package_id))
    }
    fn container_header(&self) -> Option<&retoc::container_header::FIoContainerHeader> {
        self.base().container_header()
    }
    fn compression_methods(&self) -> &[retoc::compression::CompressionMethod] {
        self.base().compression_methods()
    }
    fn load_script_objects(
        &self,
    ) -> retoc::anyhow::Result<retoc::script_objects::ZenScriptObjects> {
        self.base().load_script_objects()
    }
}
