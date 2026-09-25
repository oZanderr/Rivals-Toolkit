//! Applies value edits to an asset and ships the result as a mod that overrides it.
//!
//! The game resolves a package through its IoStore store, so a mod carrying a `.uasset` has to be
//! an IoStore container of its own. A plain pak only delivers loose files such as INIs and sound
//! banks, whatever priority its name claims. Nothing is written until the patched package has been
//! read back and checked.

use std::fs;
use std::path::{Path, PathBuf};

use retoc::legacy_asset::FSerializedAssetBundle;
use rivals_uasset::{
    AssetBundle, Mappings, PackageEdits, ParseOptions, PatchedBundle, RemovalPlan,
};

pub mod diff;
pub mod json;
pub mod sweep;

use crate::asset::{self, AssetSource};
use crate::pak::profile::strip_mount_prefix;
use crate::pak_tweaks::io::{create_empty_pak, with_unpacked_pak};
use crate::pak_tweaks::scan::normalize_pak_filename;
use crate::paths::mods_dir;
use crate::schema_synth::{self, PackageSource};

pub struct AssetEditRequest<'a> {
    pub game_root: &'a str,
    pub container: &'a str,
    pub entry: &'a str,
    pub kind: AssetSource,
    /// Mod pak to write into, created if it does not exist yet.
    pub mod_name: &'a str,
    pub changes: PackageEdits,
}

/// Patches the values and proves the result still reads the same way, without writing anything.
/// Split out from [`save_edits`] so the half that can corrupt an asset is testable on its own.
pub fn preview_edits(
    request: &AssetEditRequest<'_>,
    mappings: Option<&Mappings>,
) -> Result<(PatchedBundle, FSerializedAssetBundle), String> {
    if request.changes.is_empty() {
        return Err("No changes to save".into());
    }
    let (loaded, parsed) = read_package(request, mappings)?;
    preview_read_edits(request, mappings, loaded, &parsed)
}

/// [`preview_edits`] for a caller that has already read and parsed the package, so a run over many
/// assets reads each one once rather than once per stage.
pub fn preview_read_edits(
    request: &AssetEditRequest<'_>,
    mappings: Option<&Mappings>,
    loaded: FSerializedAssetBundle,
    parsed: &rivals_uasset::ParsedPackage,
) -> Result<(PatchedBundle, FSerializedAssetBundle), String> {
    let changes = &request.changes;
    if changes.is_empty() {
        return Err("No changes to save".into());
    }
    let patched = rivals_uasset::patch_package_with(
        &bundle_of(&loaded),
        rivals_uasset::Sidecars {
            bulk: loaded.bulk_data_buffer.as_deref(),
            optional_bulk: loaded.optional_bulk_data_buffer.as_deref(),
        },
        parsed,
        changes,
        mappings,
    )?;

    // Read the patched bytes back before anything reaches disk: the decoder is the only honest
    // check that an edit changed exactly what it claimed to.
    let reread = AssetBundle {
        asset: &patched.asset,
        exports: &patched.exports,
    };
    let after =
        schema_synth::parse_package_opts(&reread, mappings, &source_of(request), editor_options())?;
    rivals_uasset::verify_patch(parsed, &after, changes, &patched.applied)?;
    Ok((patched, loaded))
}

/// What removing the requested exports would do, from a fresh read. Nothing is written.
pub fn plan_export_removal(
    request: &AssetEditRequest<'_>,
    mappings: Option<&Mappings>,
) -> Result<RemovalPlan, String> {
    let (_, parsed) = read_package(request, mappings)?;
    let mut plan = rivals_uasset::plan_removal(&parsed, &request.changes.remove_exports)?;
    // The generic "may be imported" warning gives way to names when an index has been built.
    if !plan.public.is_empty()
        && let Ok(Some(index)) = crate::import_index::load(request.game_root)
    {
        plan.resolve_importers(|path| index.importers_of(path).packages);
    }
    Ok(plan)
}

/// One package to copy exports out of, named the way an asset is named anywhere else here.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CopyFrom {
    /// A path, or a container file name to look up under `Paks` and then `Paks/~mods`.
    pub container: String,
    pub entry: String,
}

impl CopyFrom {
    /// The key a [`rivals_uasset::CopyExport`] names this source by.
    pub fn key(&self) -> String {
        format!("{}::{}", self.container, self.entry)
    }
}

/// A copy request: which packages to read, and what to take out of each.
pub struct CopyRequest<'a> {
    pub game_root: &'a str,
    pub container: &'a str,
    pub entry: &'a str,
    pub kind: AssetSource,
    pub mod_name: &'a str,
    /// The packages the copies come out of, keyed as [`CopyFrom::key`] gives them.
    pub sources: Vec<CopyFrom>,
    pub copies: Vec<rivals_uasset::CopyExport>,
}

/// A source package held open for the length of a copy, since the copy borrows its bytes.
struct LoadedSource {
    key: String,
    loaded: FSerializedAssetBundle,
    parsed: rivals_uasset::ParsedPackage,
    header: retoc::legacy_asset::FLegacyPackageHeader,
}

/// Reads every source the request names. A source that will not load is an error rather than a
/// blocker: nothing about the copy can be judged without it.
fn load_sources(
    request: &CopyRequest<'_>,
    mappings: Option<&Mappings>,
) -> Result<Vec<LoadedSource>, String> {
    let mut out = Vec::with_capacity(request.sources.len());
    for source in &request.sources {
        let container = crate::asset_edit::json::resolve_container(
            &source.container,
            std::path::Path::new("."),
            request.game_root,
        )?;
        let kind = if std::path::Path::new(&container)
            .extension()
            .and_then(|e| e.to_str())
            == Some("utoc")
        {
            AssetSource::Utoc
        } else {
            AssetSource::Pak
        };
        let loaded = asset::load_bundle(request.game_root, &container, &source.entry, kind)?;
        let bundle = AssetBundle {
            asset: &loaded.asset_file_buffer,
            exports: &loaded.exports_file_buffer,
        };
        let header = rivals_uasset::read_header(&bundle)?;
        let parsed = schema_synth::parse_package_opts(
            &bundle,
            mappings,
            &PackageSource {
                game_root: request.game_root,
                container: &container,
                entry: &source.entry,
                kind,
            },
            editor_options(),
        )?;
        out.push(LoadedSource {
            key: source.key(),
            loaded,
            parsed,
            header,
        });
    }
    Ok(out)
}

/// Patches the copies in and proves the result reads back, without writing anything.
pub fn preview_copy(
    request: &CopyRequest<'_>,
    mappings: Option<&Mappings>,
) -> Result<(PatchedBundle, FSerializedAssetBundle), String> {
    if request.copies.is_empty() {
        return Err("No exports to copy".into());
    }
    let held = load_sources(request, mappings)?;
    let sources: std::collections::BTreeMap<String, rivals_uasset::CopySource<'_>> = held
        .iter()
        .map(|source| {
            (
                source.key.clone(),
                rivals_uasset::CopySource {
                    parsed: &source.parsed,
                    header: &source.header,
                    exports: &source.loaded.exports_file_buffer,
                },
            )
        })
        .collect();

    let into = AssetEditRequest {
        game_root: request.game_root,
        container: request.container,
        entry: request.entry,
        kind: request.kind,
        mod_name: request.mod_name,
        changes: PackageEdits::default(),
    };
    let (loaded, parsed) = read_package(&into, mappings)?;
    let patched =
        rivals_uasset::patch_package_copy(&bundle_of(&loaded), &parsed, &sources, &request.copies)?;
    let reread = AssetBundle {
        asset: &patched.asset,
        exports: &patched.exports,
    };
    let after =
        schema_synth::parse_package_opts(&reread, mappings, &source_of(&into), editor_options())?;
    rivals_uasset::verify_copy(&parsed, &after, &sources, &request.copies)?;
    Ok((patched, loaded))
}

/// What the copy would bring across and what stands in the way. Nothing is written.
pub fn plan_copy(
    request: &CopyRequest<'_>,
    mappings: Option<&Mappings>,
) -> Result<rivals_uasset::CopyPlan, String> {
    let held = load_sources(request, mappings)?;
    let sources: std::collections::BTreeMap<String, rivals_uasset::CopySource<'_>> = held
        .iter()
        .map(|source| {
            (
                source.key.clone(),
                rivals_uasset::CopySource {
                    parsed: &source.parsed,
                    header: &source.header,
                    exports: &source.loaded.exports_file_buffer,
                },
            )
        })
        .collect();
    let into = AssetEditRequest {
        game_root: request.game_root,
        container: request.container,
        entry: request.entry,
        kind: request.kind,
        mod_name: request.mod_name,
        changes: PackageEdits::default(),
    };
    let (_, parsed) = read_package(&into, mappings)?;
    rivals_uasset::plan_copy(&parsed, &sources, &request.copies)
}

/// What dropping the requested imports would do: which rows go, which references are cleared, and
/// what blocks it. Nothing is written.
pub fn plan_import_removal(
    request: &AssetEditRequest<'_>,
    mappings: Option<&Mappings>,
) -> Result<rivals_uasset::ImportRemovalPlan, String> {
    let dropped: Vec<u32> = request
        .changes
        .imports
        .iter()
        .filter_map(|edit| match edit {
            rivals_uasset::ImportEdit::Remove { import } => Some(*import),
            _ => None,
        })
        .collect();
    let (loaded, parsed) = read_package(request, mappings)?;
    let header = rivals_uasset::read_header(&bundle_of(&loaded))?;
    rivals_uasset::plan_import_removal(&parsed, &header, &dropped)
}

/// What the requested export table edits would do: the paths that move, the hashes other packages
/// import by, and what blocks them. Nothing is written.
pub fn plan_export_edits(
    request: &AssetEditRequest<'_>,
    mappings: Option<&Mappings>,
) -> Result<rivals_uasset::ExportEditPlan, String> {
    let (_, parsed) = read_package(request, mappings)?;
    let mut plan = rivals_uasset::plan_export_edits_with(
        &parsed,
        &request.changes.exports,
        mappings,
        &request.changes.reset_exports,
    )?;
    if !plan.public.is_empty()
        && let Ok(Some(index)) = crate::import_index::load(request.game_root)
    {
        plan.resolve_importers(|path| index.importers_of(path).packages);
    }
    Ok(plan)
}

/// What the requested dependency edits would do: what blocks them, what they leave dangling, and
/// the cycle they would make if they would make one. Nothing is written.
pub fn plan_dependency_edits(
    request: &AssetEditRequest<'_>,
    mappings: Option<&Mappings>,
) -> Result<rivals_uasset::DependencyPlan, String> {
    let (loaded, parsed) = read_package(request, mappings)?;
    let header = rivals_uasset::read_header(&bundle_of(&loaded))?;
    rivals_uasset::plan_dependency_edits(&parsed, &header, &request.changes.dependencies)
}

/// The asset as the editor sees it: every declared slot listed, stored or not, since edits address
/// slots an export may not store.
fn read_package(
    request: &AssetEditRequest<'_>,
    mappings: Option<&Mappings>,
) -> Result<(FSerializedAssetBundle, rivals_uasset::ParsedPackage), String> {
    let loaded = asset::load_bundle(
        request.game_root,
        request.container,
        request.entry,
        request.kind,
    )?;
    let parsed = parse_loaded(request, mappings, &loaded)?;
    Ok((loaded, parsed))
}

/// The editor's parse of a bundle the caller already holds.
pub fn parse_loaded(
    request: &AssetEditRequest<'_>,
    mappings: Option<&Mappings>,
    loaded: &FSerializedAssetBundle,
) -> Result<rivals_uasset::ParsedPackage, String> {
    schema_synth::parse_package_opts(
        &bundle_of(loaded),
        mappings,
        &source_of(request),
        editor_options(),
    )
}

fn bundle_of(loaded: &FSerializedAssetBundle) -> AssetBundle<'_> {
    AssetBundle {
        asset: &loaded.asset_file_buffer,
        exports: &loaded.exports_file_buffer,
    }
}

fn source_of<'a>(request: &AssetEditRequest<'a>) -> PackageSource<'a> {
    PackageSource {
        game_root: request.game_root,
        container: request.container,
        entry: request.entry,
        kind: request.kind,
    }
}

fn editor_options() -> ParseOptions {
    ParseOptions {
        declared_slots: true,
        ..Default::default()
    }
}

/// The bytes an export carries after its properties, for saving to a file.
pub fn read_payload(
    request: &AssetEditRequest<'_>,
    mappings: Option<&Mappings>,
    export: u32,
) -> Result<Vec<u8>, String> {
    let (loaded, parsed) = read_package(request, mappings)?;
    let found = parsed
        .exports
        .iter()
        .find(|e| e.index == export)
        .ok_or_else(|| format!("no export {export}"))?;
    if let Some(reason) = rivals_uasset::payload_lock(found, &parsed.resources) {
        return Err(format!("{}: {reason}", found.object_name));
    }
    let (consumed, payload_bytes) = match &found.status {
        rivals_uasset::ExportStatus::Payload {
            consumed,
            payload_bytes,
            ..
        } => (*consumed, *payload_bytes),
        _ => return Err("the export has no payload".into()),
    };
    let bundle = bundle_of(&loaded);
    let header = rivals_uasset::read_header(&bundle)?;
    let (bytes, _) = rivals_uasset::export_bytes(&bundle, &header, export)?;
    let start = usize::try_from(consumed).map_err(|_| "payload offset does not fit")?;
    let end = start + usize::try_from(payload_bytes).map_err(|_| "payload size does not fit")?;
    bytes
        .get(start..end)
        .map(<[u8]>::to_vec)
        .ok_or_else(|| "the payload lies outside the export".to_string())
}

/// The bytes of one bulk data resource, inline or in a sidecar, for saving to a file.
pub fn read_bulk(request: &AssetEditRequest<'_>, resource: u32) -> Result<Vec<u8>, String> {
    let loaded = asset::load_bundle(
        request.game_root,
        request.container,
        request.entry,
        request.kind,
    )?;
    let bundle = bundle_of(&loaded);
    let header = rivals_uasset::read_header(&bundle)?;
    let index = resource as usize;
    let entry = header
        .data_resources
        .get(index)
        .ok_or_else(|| format!("no bulk data resource {resource}"))?;
    let (file, offset): (&[u8], i64) = match rivals_uasset::placement(entry.legacy_bulk_data_flags)
    {
        "inline" => {
            let payload = rivals_uasset::locate_inline_payload(&header, bundle.exports, index)
                .ok_or("no export holds this payload where the table points")?;
            (bundle.exports, payload.start)
        }
        "ubulk" => (
            loaded
                .bulk_data_buffer
                .as_deref()
                .ok_or("the .ubulk was not read with the package")?,
            entry.serial_offset,
        ),
        "uptnl" => (
            loaded
                .optional_bulk_data_buffer
                .as_deref()
                .ok_or("the .uptnl was not read with the package")?,
            entry.serial_offset,
        ),
        _ => (
            loaded
                .memory_mapped_bulk_data_buffer
                .as_deref()
                .ok_or("the .m.ubulk was not read with the package")?,
            entry.serial_offset,
        ),
    };
    let start = usize::try_from(offset).map_err(|_| "payload offset does not fit")?;
    let end =
        start + usize::try_from(entry.serial_size).map_err(|_| "payload size does not fit")?;
    file.get(start..end)
        .map(<[u8]>::to_vec)
        .ok_or_else(|| "the payload lies outside its file".to_string())
}

/// What a save did. Nothing is written for `HoldsCopy`: the mod already carries an edited copy of
/// this asset, and writing would replace it with the original plus the new edits, so the caller
/// has to ask for that explicitly.
#[derive(Debug)]
pub enum SaveOutcome {
    Written {
        message: String,
        pak: PathBuf,
        /// Other installed mods that carry the same asset, and which copy the game loads.
        warnings: Vec<String>,
    },
    HoldsCopy {
        pak: String,
    },
}

/// Which form a save takes. The game resolves packages through its IoStore store, so a mod holding
/// a `.uasset` has to be a container; a plain pak reaches only loose files such as INIs, whatever
/// priority its name claims.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SaveTarget {
    /// A plain pak. Useful only for tooling that converts it onward itself.
    Pak,
    #[default]
    IoStore,
}

#[derive(Default)]
pub struct SaveOptions {
    /// Start again from the source, dropping whatever the mod's copy already carries.
    pub replace: bool,
    /// Build on the mod's copy when it already carries the asset: the edits were made against it.
    pub layer: bool,
    pub target: SaveTarget,
    pub iostore: crate::pak::iostore_out::IoStoreOptions,
}

/// Patches, verifies, then writes the result into a mod that overrides the original.
/// Copies exports in from other packages and writes the result, the same way a value edit is
/// written. Split from [`save_edits`] because the sources have to be held open across the patch.
pub fn save_copy(
    request: &CopyRequest<'_>,
    mappings: Option<&Mappings>,
    options: &SaveOptions,
) -> Result<SaveOutcome, String> {
    let into = AssetEditRequest {
        game_root: request.game_root,
        container: request.container,
        entry: request.entry,
        kind: request.kind,
        mod_name: request.mod_name,
        changes: PackageEdits::default(),
    };
    let entry = save_entry(&into)?;
    let held = destination_check(&into, &entry, options)?;
    if let Some(outcome) = holds_copy(&into, held, options)? {
        return Ok(outcome);
    }
    let (patched, loaded) = match layered_source(&into, held, options)? {
        Some((container, kind)) => preview_copy(
            &CopyRequest {
                container: &container,
                entry: &entry,
                kind,
                sources: request.sources.clone(),
                copies: request.copies.clone(),
                ..*request
            },
            mappings,
        )?,
        None => preview_copy(request, mappings)?,
    };
    write_patched(&into, &entry, &patched, &loaded, options)
}

pub fn save_edits(
    request: &AssetEditRequest<'_>,
    mappings: Option<&Mappings>,
    options: &SaveOptions,
) -> Result<SaveOutcome, String> {
    let entry = save_entry(request)?;
    let held = destination_check(request, &entry, options)?;
    if let Some(outcome) = holds_copy(request, held, options)? {
        return Ok(outcome);
    }
    let (patched, loaded) = match layered_source(request, held, options)? {
        Some((container, kind)) => preview_edits(
            &AssetEditRequest {
                container: &container,
                entry: &entry,
                kind,
                changes: request.changes.clone(),
                ..*request
            },
            mappings,
        )?,
        None => preview_edits(request, mappings)?,
    };
    write_patched(request, &entry, &patched, &loaded, options)
}

/// Saves several packages into one mod with a single container rewrite, rather than one per
/// package. Each request is patched and verified on its own and reports its own outcome, so one
/// that fails does not stop the rest. Anything but an IoStore save into one mod is written one
/// request at a time.
pub fn save_batch(
    requests: &[AssetEditRequest<'_>],
    mappings: Option<&Mappings>,
    options: &SaveOptions,
) -> Vec<Result<SaveOutcome, String>> {
    let one_mod = requests
        .windows(2)
        .all(|pair| pair[0].mod_name == pair[1].mod_name && pair[0].game_root == pair[1].game_root);
    if options.target != SaveTarget::IoStore || !one_mod || requests.len() < 2 {
        return requests
            .iter()
            .map(|request| save_edits(request, mappings, options))
            .collect();
    }
    let mut outcomes: Vec<Option<Result<SaveOutcome, String>>> =
        requests.iter().map(|_| None).collect();
    let mut staged: Vec<(usize, String, usize, crate::pak::iostore_out::PackageBytes)> = Vec::new();
    for (index, request) in requests.iter().enumerate() {
        match stage_one(request, mappings, options) {
            Ok(Staged::Held(outcome)) => outcomes[index] = Some(Ok(outcome)),
            Ok(Staged::Ready {
                entry,
                changes,
                bytes,
            }) => staged.push((index, entry, changes, *bytes)),
            Err(reason) => outcomes[index] = Some(Err(reason)),
        }
    }
    if !staged.is_empty() {
        let written = mod_pak_path(requests[0].game_root, requests[0].mod_name).and_then(|pak| {
            let utoc = pak.with_extension("utoc");
            let mut bytes: Vec<Option<crate::pak::iostore_out::PackageBytes>> = staged
                .iter_mut()
                .map(|(_, _, _, package)| Some(std::mem::replace(package, empty_package())))
                .collect();
            crate::pak::containers::drop_cached_store();
            let report = crate::pak::iostore_out::write_batch_into_iostore(
                &utoc,
                bytes.len(),
                |at| bytes.get_mut(at).and_then(Option::take),
                &options.iostore,
            )?;
            Ok((utoc, report))
        });
        for (index, entry, changes, _) in &staged {
            let request = &requests[*index];
            outcomes[*index] = Some(match &written {
                Ok((utoc, report)) => Ok(SaveOutcome::Written {
                    message: format!(
                        "Saved {changes} change(s) to {}{}",
                        utoc.file_name().unwrap_or_default().to_string_lossy(),
                        placed_as(request, entry)
                    ),
                    warnings: other_overrides(request.game_root, entry, utoc),
                    pak: report.utoc.clone(),
                }),
                Err(reason) => Err(reason.clone()),
            });
        }
    }
    outcomes
        .into_iter()
        .map(|outcome| outcome.unwrap_or_else(|| Err("nothing was done".to_string())))
        .collect()
}

/// One request of a batch, patched and verified but not yet written.
enum Staged {
    Held(SaveOutcome),
    Ready {
        entry: String,
        changes: usize,
        bytes: Box<crate::pak::iostore_out::PackageBytes>,
    },
}

fn stage_one(
    request: &AssetEditRequest<'_>,
    mappings: Option<&Mappings>,
    options: &SaveOptions,
) -> Result<Staged, String> {
    let entry = save_entry(request)?;
    let held = destination_check(request, &entry, options)?;
    if let Some(outcome) = holds_copy(request, held, options)? {
        return Ok(Staged::Held(outcome));
    }
    let (patched, loaded) = match layered_source(request, held, options)? {
        Some((container, kind)) => preview_edits(
            &AssetEditRequest {
                container: &container,
                entry: &entry,
                kind,
                changes: request.changes.clone(),
                ..*request
            },
            mappings,
        )?,
        None => preview_edits(request, mappings)?,
    };
    let shader_map_hashes = source_shader_maps(request, &patched.asset);
    Ok(Staged::Ready {
        changes: patched.applied.len(),
        bytes: Box::new(crate::pak::iostore_out::PackageBytes {
            entry: entry.clone(),
            asset: patched.asset,
            exports: patched.exports,
            bulk: patched.bulk.or(loaded.bulk_data_buffer),
            optional_bulk: patched.optional_bulk.or(loaded.optional_bulk_data_buffer),
            memory_mapped_bulk: loaded.memory_mapped_bulk_data_buffer,
            shader_map_hashes,
        }),
        entry,
    })
}

fn empty_package() -> crate::pak::iostore_out::PackageBytes {
    crate::pak::iostore_out::PackageBytes {
        entry: String::new(),
        asset: Vec::new(),
        exports: Vec::new(),
        bulk: None,
        optional_bulk: None,
        memory_mapped_bulk: None,
        shader_map_hashes: Vec::new(),
    }
}

/// The mod's own copy of the asset a request names, when the mod already carries one: the
/// container to inspect so that edits are made on top of it.
pub fn mod_copy_of(
    request: &AssetEditRequest<'_>,
    target: SaveTarget,
) -> Result<Option<PathBuf>, String> {
    let entry = save_entry(request)?;
    let options = SaveOptions {
        target,
        ..Default::default()
    };
    if !destination_check(request, &entry, &options)? {
        return Ok(None);
    }
    let pak = mod_pak_path(request.game_root, request.mod_name)?;
    Ok(Some(match target {
        SaveTarget::IoStore => pak.with_extension("utoc"),
        SaveTarget::Pak => pak,
    }))
}

/// Stops a save that would replace the mod's own copy with the original plus the new edits,
/// unless the caller asked to replace it or to build on it.
fn holds_copy(
    request: &AssetEditRequest<'_>,
    held: bool,
    options: &SaveOptions,
) -> Result<Option<SaveOutcome>, String> {
    if !held || options.replace || options.layer {
        return Ok(None);
    }
    let pak = mod_pak_path(request.game_root, request.mod_name)?;
    Ok(Some(SaveOutcome::HoldsCopy {
        pak: pak
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
    }))
}

/// Where a layered save reads from: the mod's own copy, when it holds one. The mod's container is
/// opened like any other, so its copy wins over the game's.
fn layered_source(
    request: &AssetEditRequest<'_>,
    held: bool,
    options: &SaveOptions,
) -> Result<Option<(String, AssetSource)>, String> {
    if !held || !options.layer || options.replace {
        return Ok(None);
    }
    let pak = mod_pak_path(request.game_root, request.mod_name)?;
    Ok(Some(match options.target {
        SaveTarget::IoStore => (
            pak.with_extension("utoc").to_string_lossy().into_owned(),
            AssetSource::Utoc,
        ),
        SaveTarget::Pak => (pak.to_string_lossy().into_owned(), AssetSource::Pak),
    }))
}

/// The path a save writes under, which is the one the game resolves the package by. A loose file
/// is named by where it sits on disk and a package can be named by its package name, and neither
/// is a path inside a container.
pub fn save_entry(request: &AssetEditRequest<'_>) -> Result<String, String> {
    let entry = match request.kind {
        AssetSource::Loose => {
            let path = Path::new(request.entry);
            let asset = fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
            let header = rivals_uasset::read_header(&AssetBundle {
                asset: &asset,
                exports: &[],
            })?;
            asset::game_entry(request.game_root, &header.summary.package_name, path)?
        }
        _ if request.entry.starts_with('/') => {
            asset::game_entry(request.game_root, request.entry, Path::new(request.entry))?
        }
        _ => request.entry.to_string(),
    };
    asset::contained_entry(&entry)
}

/// Whether the destination can take the write at all, and whether it already holds a copy. Checked
/// before the patch so a declined replace does not cost a parse and a verify.
fn destination_check(
    request: &AssetEditRequest<'_>,
    entry: &str,
    options: &SaveOptions,
) -> Result<bool, String> {
    let pak = mod_pak_path(request.game_root, request.mod_name)?;
    let utoc = pak.with_extension("utoc");
    let name = pak
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    match options.target {
        SaveTarget::Pak if utoc.is_file() => {
            return Err(format!(
                "{name} is an IoStore mod, so a plain pak beside it would be a third file the game reads separately. Choose another name."
            ));
        }
        SaveTarget::IoStore if pak.is_file() && !utoc.is_file() => {
            return Err(format!(
                "{name} is a plain pak mod. Repack it in place as IoStore first, or choose another name."
            ));
        }
        _ => {}
    }
    Ok(match options.target {
        SaveTarget::Pak => pak.is_file() && holds_entry(&pak, entry)?,
        SaveTarget::IoStore => {
            utoc.is_file() && crate::pak::iostore_out::utoc_holds_entry(&utoc, entry)?
        }
    })
}

/// Which other installed IoStore mods carry `entry`, each said with whether its copy or the one
/// being written is what the game loads. A plain pak cannot deliver a package, so only containers
/// count.
pub fn other_overrides(game_root: &str, entry: &str, own: &Path) -> Vec<String> {
    let own_name = own
        .with_extension("utoc")
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut holders: Vec<String> = walkdir::WalkDir::new(mods_dir(game_root))
        .into_iter()
        .filter_map(Result::ok)
        .map(walkdir::DirEntry::into_path)
        .filter(|path| path.extension().is_some_and(|ext| ext == "utoc"))
        .filter_map(|path| {
            let name = path.file_name()?.to_string_lossy().into_owned();
            let other = !name.eq_ignore_ascii_case(&own_name);
            (other && crate::pak::iostore_out::utoc_holds_entry(&path, entry).unwrap_or(false))
                .then_some(name)
        })
        .collect();
    holders.sort_by(|a, b| crate::mods::winner_order(a, b));
    holders
        .into_iter()
        .map(|other| {
            let winner = if crate::mods::winner_order(&other, &own_name).is_lt() {
                &other
            } else {
                &own_name
            };
            format!("{other} also overrides this asset, and the game loads the copy in {winner}")
        })
        .collect()
}

/// The patched bundle onto disk, in whichever form the target asks for.
fn write_patched(
    request: &AssetEditRequest<'_>,
    entry: &str,
    patched: &PatchedBundle,
    loaded: &FSerializedAssetBundle,
    options: &SaveOptions,
) -> Result<SaveOutcome, String> {
    let pak = mod_pak_path(request.game_root, request.mod_name)?;
    let utoc = pak.with_extension("utoc");
    let name = pak
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    if options.target == SaveTarget::IoStore {
        return save_into_iostore(request, entry, &utoc, patched, loaded, options);
    }
    write_into_pak(
        &pak,
        entry,
        &patched.asset,
        &patched.exports,
        [
            (
                "ubulk",
                patched
                    .bulk
                    .as_deref()
                    .or(loaded.bulk_data_buffer.as_deref()),
            ),
            (
                "uptnl",
                patched
                    .optional_bulk
                    .as_deref()
                    .or(loaded.optional_bulk_data_buffer.as_deref()),
            ),
            ("m.ubulk", loaded.memory_mapped_bulk_data_buffer.as_deref()),
        ],
    )?;
    Ok(SaveOutcome::Written {
        message: format!(
            "Saved {} change(s) to {name}{}",
            patched.applied.len(),
            placed_as(request, entry)
        ),
        warnings: other_overrides(request.game_root, entry, &pak),
        pak,
    })
}

/// The IoStore form of a save: the container is written again around the edited package.
fn save_into_iostore(
    request: &AssetEditRequest<'_>,
    entry: &str,
    utoc: &Path,
    patched: &PatchedBundle,
    loaded: &FSerializedAssetBundle,
    options: &SaveOptions,
) -> Result<SaveOutcome, String> {
    let shader_map_hashes = source_shader_maps(request, &patched.asset);
    // A read earlier in this session may still hold the container open, and it is about to be
    // replaced underneath.
    crate::pak::containers::drop_cached_store();
    let report = crate::pak::iostore_out::write_into_iostore(
        utoc,
        crate::pak::iostore_out::PackageFiles {
            entry,
            asset: &patched.asset,
            exports: &patched.exports,
            bulk: patched
                .bulk
                .as_deref()
                .or(loaded.bulk_data_buffer.as_deref()),
            optional_bulk: patched
                .optional_bulk
                .as_deref()
                .or(loaded.optional_bulk_data_buffer.as_deref()),
            memory_mapped_bulk: loaded.memory_mapped_bulk_data_buffer.as_deref(),
            shader_map_hashes,
        },
        &options.iostore,
    )?;
    let name = utoc
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let carried = if report.carried_chunks > 0 {
        format!(", {} chunk(s) carried over", report.carried_chunks)
    } else {
        String::new()
    };
    Ok(SaveOutcome::Written {
        message: format!(
            "Saved {} change(s) to {name}{}{carried}",
            patched.applied.len(),
            placed_as(request, entry)
        ),
        warnings: other_overrides(request.game_root, entry, utoc),
        pak: report.utoc,
    })
}

/// The shader maps the package's source lists for it, which a material needs carried into the
/// mod's header or it loses them.
fn source_shader_maps(request: &AssetEditRequest<'_>, asset: &[u8]) -> Vec<retoc::FSHAHash> {
    let Ok(header) = rivals_uasset::read_header(&AssetBundle {
        asset,
        exports: &[],
    }) else {
        return Vec::new();
    };
    let container = (request.kind == AssetSource::Utoc).then_some(request.container);
    asset::shader_map_hashes(request.game_root, container, &header.summary.package_name)
}

/// Names the game path a save went to when the caller named the package some other way.
fn placed_as(request: &AssetEditRequest<'_>, entry: &str) -> String {
    if comparable(request.entry) == comparable(entry) {
        String::new()
    } else {
        format!(" as {entry}")
    }
}

/// Whether the pak already carries this entry, compared the way `write_into_pak` names it.
fn holds_entry(pak: &Path, entry: &str) -> Result<bool, String> {
    let wanted = comparable(entry);
    Ok(asset::list_pak_entries(&pak.to_string_lossy())?
        .iter()
        .any(|held| comparable(held) == wanted))
}

fn comparable(path: &str) -> String {
    strip_mount_prefix(path)
        .replace('\\', "/")
        .to_ascii_lowercase()
}

fn mod_pak_path(game_root: &str, mod_name: &str) -> Result<PathBuf, String> {
    let mods = mods_dir(game_root);
    if !mods.is_dir() {
        fs::create_dir_all(&mods)
            .map_err(|e| format!("Could not create {}: {e}", mods.display()))?;
    }
    Ok(mods.join(normalize_pak_filename(mod_name)?))
}

/// The sidecars travel with the package: a `.uexp` that lost its `.ubulk` will not load.
fn write_into_pak(
    pak: &Path,
    entry: &str,
    asset_bytes: &[u8],
    exports_bytes: &[u8],
    sidecars: [(&str, Option<&[u8]>); 3],
) -> Result<(), String> {
    let entry = &asset::contained_entry(entry)?;
    if !pak.is_file() {
        create_empty_pak(pak)?;
    }
    let stem = entry
        .rsplit_once('.')
        .map_or(entry.as_str(), |(stem, _)| stem);
    let mut files: Vec<(String, &[u8])> = vec![
        (entry.to_string(), asset_bytes),
        (format!("{stem}.uexp"), exports_bytes),
    ];
    for (extension, bytes) in sidecars {
        if let Some(bytes) = bytes {
            files.push((format!("{stem}.{extension}"), bytes));
        }
    }

    with_unpacked_pak(pak, |temp_dir| {
        for (name, bytes) in &files {
            let dest = temp_dir.join(strip_mount_prefix(name));
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("Could not create {}: {e}", parent.display()))?;
            }
            fs::write(&dest, bytes)
                .map_err(|e| format!("Could not write {}: {e}", dest.display()))?;
        }
        Ok(())
    })
}

/// Writing a pak compresses with Oodle, so the tests that do set `OODLE_LIB_PATH` and skip
/// without it, like the game-data tests below. The refusal tests need no pak and always run.
#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn oodle_available() -> bool {
        std::env::var_os("OODLE_LIB_PATH").is_some()
    }

    /// A scratch game root under the system temp folder, removed when dropped.
    struct ScratchRoot(PathBuf);

    impl ScratchRoot {
        fn new(tag: &str) -> Self {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos();
            Self(std::env::temp_dir().join(format!(
                "rivals-asset-edit-{tag}-{}-{stamp}",
                std::process::id()
            )))
        }

        fn game_root(&self) -> String {
            self.0.to_string_lossy().into_owned()
        }
    }

    impl Drop for ScratchRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const ENTRY: &str = "Marvel/Content/Test/Thing.uasset";

    fn request<'a>(root: &'a str, mod_name: &'a str) -> AssetEditRequest<'a> {
        AssetEditRequest {
            game_root: root,
            container: "",
            entry: ENTRY,
            kind: AssetSource::Utoc,
            mod_name,
            changes: PackageEdits::default(),
        }
    }

    /// A mod pak that already carries the entry, as `write_into_pak` would have left it.
    fn pak_holding_the_entry(root: &str, mod_name: &str) -> PathBuf {
        let pak = mod_pak_path(root, mod_name).expect("pak path");
        create_empty_pak(&pak).expect("empty pak");
        with_unpacked_pak(&pak, |dir| {
            let file = dir.join(ENTRY);
            fs::create_dir_all(file.parent().expect("parent")).map_err(|e| e.to_string())?;
            fs::write(file, b"not really an asset").map_err(|e| e.to_string())
        })
        .expect("add the entry");
        pak
    }

    /// The pak names its entries its own way, so a different case, separator or mount prefix from
    /// the caller must still be recognised as the same asset.
    #[test]
    fn a_pak_is_recognised_as_holding_an_entry_however_the_path_is_spelt() {
        if !oodle_available() {
            return;
        }
        let scratch = ScratchRoot::new("holds");
        let pak = pak_holding_the_entry(&scratch.game_root(), "Holds");

        assert!(holds_entry(&pak, ENTRY).expect("list"));
        assert!(holds_entry(&pak, "../../../marvel/content/test/thing.uasset").expect("list"));
        assert!(holds_entry(&pak, "Marvel\\Content\\Test\\Thing.uasset").expect("list"));
        assert!(!holds_entry(&pak, "Marvel/Content/Test/Other.uasset").expect("list"));
    }

    /// Saving into a pak that already holds the asset must not reach the patch, let alone the
    /// disk, unless the caller said to replace.
    #[test]
    fn a_mod_already_holding_the_asset_is_reported_rather_than_overwritten() {
        if !oodle_available() {
            return;
        }
        let scratch = ScratchRoot::new("copy");
        let root = scratch.game_root();
        let pak = pak_holding_the_entry(&root, "Copy");
        let before = fs::read(&pak).expect("pak bytes");

        let outcome = save_edits(
            &request(&root, "Copy"),
            None,
            &SaveOptions {
                target: SaveTarget::Pak,
                ..Default::default()
            },
        )
        .expect("outcome");
        assert!(
            matches!(outcome, SaveOutcome::HoldsCopy { ref pak } if pak == "Copy_9999999_P.pak"),
            "{outcome:?}"
        );
        assert_eq!(
            fs::read(&pak).expect("pak bytes"),
            before,
            "nothing may be written"
        );
    }

    /// A plain pak beside an IoStore mod would be a third file the game reads separately, so the
    /// name is refused before anything is read.
    #[test]
    fn a_plain_pak_is_refused_beside_an_iostore_mod() {
        let scratch = ScratchRoot::new("zen");
        let root = scratch.game_root();
        let pak = mod_pak_path(&root, "Zen").expect("pak path");
        fs::write(pak.with_extension("utoc"), b"").expect("utoc");

        let error = save_edits(
            &request(&root, "Zen"),
            None,
            &SaveOptions {
                target: SaveTarget::Pak,
                ..Default::default()
            },
        )
        .expect_err("refused");
        assert!(error.contains("IoStore"), "{error}");
    }

    /// A header naming `package`, written where an extraction would have left it on disk.
    fn loose_package(dir: &Path, package: &str) -> PathBuf {
        use retoc::legacy_asset::{
            FLegacyPackageFileSummary, FLegacyPackageHeader, FPackageNameMap,
        };
        let mut summary = FLegacyPackageFileSummary {
            package_name: package.to_string(),
            ..Default::default()
        };
        summary.versioning_info.package_file_version =
            retoc::version::EngineVersion::UE5_3.package_file_version();
        let header = FLegacyPackageHeader {
            summary,
            name_map: FPackageNameMap::create_from_names(vec!["None".to_string()]),
            ..Default::default()
        };
        let mut bytes = std::io::Cursor::new(Vec::new());
        header
            .serialize(&mut bytes, None, &retoc::logging::Log::no_log())
            .expect("serialize a header");
        let path = dir.join("Extracted/Somewhere/Thing.uasset");
        fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        fs::write(&path, bytes.into_inner()).expect("write");
        path
    }

    /// A loose file is named by where it sits on disk, which is no path inside a container: a save
    /// has to go under the path its package name gives, or it would override nothing.
    #[test]
    fn a_loose_package_is_saved_under_its_game_path() {
        let scratch = ScratchRoot::new("loose");
        let root = scratch.game_root();
        let path = loose_package(&scratch.0, "/Game/Test/Thing");
        let disk = path.to_string_lossy();
        let request = AssetEditRequest {
            entry: &disk,
            kind: AssetSource::Loose,
            ..request(&root, "Loose")
        };
        assert_eq!(
            save_entry(&request).expect("an entry"),
            "Marvel/Content/Test/Thing.uasset"
        );
    }

    /// Joining an absolute path onto the pak's scratch folder would replace the folder, so the
    /// write would land on the disk path itself.
    #[test]
    fn a_pak_write_refuses_a_path_outside_the_container() {
        let scratch = ScratchRoot::new("escape");
        let root = scratch.game_root();
        let pak = mod_pak_path(&root, "Escape").expect("pak path");
        for entry in [
            "C:/Users/Someone/Thing.uasset",
            "/Marvel/Content/Thing.uasset",
            "../../../../Thing.uasset",
        ] {
            let error =
                write_into_pak(&pak, entry, b"", b"", [("ubulk", None); 3]).expect_err("refused");
            assert!(error.contains("not a path inside a container"), "{error}");
        }
        assert!(!pak.exists(), "nothing may be created");
    }

    /// The game reads packages only from a container, so a plain pak mod cannot simply gain one
    /// beside it: the mod has to be converted first.
    #[test]
    fn an_iostore_save_is_refused_into_a_plain_pak_mod() {
        let scratch = ScratchRoot::new("plain");
        let root = scratch.game_root();
        let pak = mod_pak_path(&root, "Plain").expect("pak path");
        fs::write(&pak, b"not really a pak").expect("pak");

        let error = save_edits(&request(&root, "Plain"), None, &SaveOptions::default())
            .expect_err("refused");
        assert!(error.contains("Repack it in place"), "{error}");
    }
}

/// Set `RIVALS_GAME_ROOT`, `RIVALS_USMAP` and `OODLE_LIB_PATH` to run these. Skipped otherwise.
#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod game_data_tests {
    use super::*;
    use crate::mappings;
    use rivals_uasset::{
        BulkEdit, DuplicateExport, EditOp, ExportEdit, ExportStatus, ImportEdit, KeyEdit, KeyOp,
        PayloadEdit, PropertyEntry, PropertyValue, RowEdit, RowOp, ScriptConstEdit, StringEdit,
        StringOp, ValueEdit,
    };

    /// Every cell in this table is stored, and a third of them are strings.
    const STRINGS: &str =
        "Marvel/Content/Marvel/Data/DataTable/UI/HeroSkin/UIHeroKillTipsTable.uasset";
    /// Nearly a quarter of this table holds its default, which is what a store or clear needs.
    const DEFAULTS: &str = "Marvel/Content/Marvel/Data/DataTable/MarvelHeroTable.uasset";
    /// Holds a stored, non-empty map column (`HeroSculptDataRemap`), which the editor must refuse.
    const MAPS: &str =
        "Marvel/Content/Marvel/Data/DataTable/AI/HeroBehaviorTree/AIHeroBehaviorTreeTable.uasset";
    /// A DataAsset whose Object properties point at material imports, so imports can be retargeted
    /// and references pointed at assets the package never named.
    const TITLES: &str = "Marvel/Content/Marvel/Data/DataAsset/Career/MarvelHeroTitleData.uasset";
    /// A material in the same family as the one the asset imports. Only the mechanics are under
    /// test here, so it need not exist in this build.
    const OTHER_MATERIAL: &str =
        "/Game/Marvel/UI/Materials/Common/MI_ColorFont_Dark.MI_ColorFont_Dark";
    /// A Blueprint class default object rather than a table: its values sit in the export's own
    /// property tree, three structs deep.
    const SHAKE: &str = "Marvel/Content/Marvel/AbilitySystem/1011/101111/CameraShake_101111.uasset";
    /// A map holding instances of two different Blueprint classes that share the name
    /// `SM_NewYorkM01Building020A_C`, one under `Blueprints/` and one under `Blueprints/Props/`.
    const SAME_NAME_MAP: &str = "Marvel/Content/Marvel/Maps/MCU/Ultron/UltronPVE_Background.umap";
    /// An animation Blueprint whose default object carries the class's sparse data struct, a
    /// Blueprint struct defined two exports further on.
    const ANIM_BLUEPRINT: &str = "Marvel/Plugins/MarvelGAS/Content/Marvel/Characters/1015/1015001/Cues/101595/101595_AnimBP.uasset";
    /// Input mappings whose copy in the base chunk predates the mappings file by three bytes per
    /// mapping; the copy the latest patch carries matches it.
    const INPUT_CONTEXT: &str = "Marvel/Content/Marvel/Data/Input/AbilityInputData/InputContext/IMC_1023000_HeroAbilityInput.uasset";
    /// Niagara components whose redirect map removes two inherited keys before holding nothing.
    const REDIRECT_CUE: &str = "Marvel/Plugins/MarvelGAS/Content/Marvel/Characters/1023/1023302/Cues/102392/Cue_Summoner_Loop_10239201_302_BP.uasset";
    /// A level sequence float section: its channel's extrapolation modes are enums a native
    /// reader names, which the mappings alone would leave as numbers.
    const CHANNEL_ENUM: &str =
        "Marvel/Content/Marvel/Blueprints/LevelGameplay/Activity/192/LS_Balloon_01.uasset";
    /// A texture with four streaming mips in its `.ubulk` and seven inline ones.
    const TEXTURE: &str = "Marvel/Content/Marvel/AbilitySystem/Cue/T_SprayPaint_Default.uasset";
    /// A static mesh whose body setup, navigation collision and render data each hold one inline
    /// payload.
    const STATIC_MESH: &str =
        "Marvel/Content/Marvel/AbilitySystem/1018/101861/SM_DS_Portal_02.uasset";
    /// A widget whose compiled sequence data holds an entity tree with one child node.
    const ENTITY_TREE: &str =
        "Marvel/Plugins/MarvelGAS/Content/Marvel/UI/1033/Blueprints/WBP_AbilityHUD_103394.uasset";
    /// An animation whose target frame rate is a per-platform property.
    const PER_PLATFORM_RATE: &str = "Marvel/Plugins/MarvelGAS/Content/Marvel/Characters/1022/1022001/Animations/102239_CloseCombat_Four.uasset";
    /// A skeletal mesh whose LOD info opens with a per-platform screen size.
    const SKELETAL_MESH: &str = "Marvel/Content/Marvel/NPC/Knull/Meshes/SK_Knull_4018001.uasset";
    /// A text render component whose text is a formatted number.
    const NUMBER_TEXT: &str = "Marvel/Content/Marvel/Maps/ConsoleTest/ConsoleTest_Destruction.umap";
    /// A runtime font: composite font data with cooked font faces.
    const FONT: &str = "Engine/Content/EngineFonts/DroidSansMono.uasset";
    /// A map whose components instance a native class this build no longer ships.
    const UNRESOLVED_CLASS: &str =
        "Marvel/Content/Marvel/Maps/NuevaYork/NuevaYorkM01_HighQuality.umap";
    /// A string table that closes with three metadata records.
    const METADATA_TABLE: &str = "Marvel/Content/Marvel/Data/StringTable/102_Subtitle_ST.uasset";
    /// A boss sequence whose compiled hierarchy holds one sub-sequence in its tree.
    const SUB_SEQUENCE_TREE: &str =
        "Marvel/Content/Marvel/Blueprints/LevelGameplay/M2208/M2208BossAppear2.uasset";
    /// A string table whose lobby entries carry the `Encrypt` marker after their source.
    const TAGGED_TABLE: &str = "Marvel/Content/Marvel/Data/StringTable/106_Lobby_ST.uasset";
    /// A table whose effect specs carry `DiffProperties`: arrays of the game's own
    /// `SerializablePropertySoftPath`, which the mappings describe as two reflected slots.
    const SOFT_PATH_TABLE: &str =
        "Marvel/Content/Marvel/Data/DataTable/GameMode/2201/1023/1023_2201_EffectTable.uasset";
    /// A Blueprint holding components of two Blueprint classes that live in second packages,
    /// `WC_M2201BallGameTerminalBP_C` and `LevelScopeCheckComponentBP_C`. Neither decodes unless
    /// the class is read out of its own package first.
    const PARENT_CHAIN: &str = "Marvel/Content/Marvel/Blueprints/LevelGameplay/Activity/10151/M2201BallGameTerminalBP.uasset";
    /// A widget instancing a widget Blueprint class from a plugin mount point.
    const PLUGIN_WIDGET: &str = "Engine/Plugins/MovieScene/MovieRenderPipeline/Content/Blueprints/UI_MovieRenderPipelineScreenOverlay.uasset";
    /// A Niagara system whose GPU compute script carries seventeen data interface parameter infos,
    /// one of them with two generated functions.
    const GPU_SCRIPT: &str = "Marvel/Plugins/MarvelGAS/Content/Marvel/VFX/Particles/Characters/1011/1011001/10116101/NS_10116101_Trajectory_01.uasset";
    /// A level sequence whose float section's channel holds four keys.
    const FLOAT_SECTION: &str =
        "Marvel/Content/Marvel/Blueprints/LevelGameplay/Activity/192/LS_Balloon_01.uasset";
    /// A level sequence of transform sections, nine double channels each.
    const TRANSFORM_SECTION: &str = "Marvel/Content/Marvel/Blueprints/LevelGameplay/Activity/192/LS_SummerFestival_Random_01.uasset";
    /// A default object whose class a patch revised. Only the patched copy matches the mappings
    /// file; the pre-patch copy in the base chunk stops a hundred bytes short of its end.
    const PATCHED_CUE: &str = "Marvel/Plugins/MarvelGAS/Content/Marvel/Characters/1037/1037001/Cues/103703/Cue_Summoner_Loop_10370301_BP.uasset";
    /// A lobby map whose Blueprint building actors the mappings file describes wrongly: the class
    /// was revised after the dump, so only its own package can decode its instances.
    const STALE_CLASS_MAP: &str = "Marvel/Content/Marvel/Maps/Lobby/2014/Lobby_2014001408.umap";
    /// A widget class implementing `UserObjectListEntry`, in the UI container: its interface
    /// records follow the generated-by reference and the class closes with a metadata map naming
    /// them.
    const WIDGET_CLASS: &str =
        "Marvel/Content/Marvel/UI/Blueprints/Common/CommonPanel/WBP_Common_Item_V2_Light.uasset";
    /// A widget in another package instancing that class.
    const WIDGET_INSTANCE: &str =
        "Marvel/Content/Marvel/UI/Blueprints/Mall/WBP_Mall_Details_MultiBundleItem.uasset";
    /// A widget whose Python parent shares its name with another module's class: the mappings
    /// file holds both, and only the other one reads this widget.
    const TWIN_PARENT_WIDGET: &str = "Marvel/Content/Marvel/UI/Blueprints/League/ScheduleV2/Common/WBP_LeagueSchedule_Dual.uasset";
    const TWIN_PARENT_INSTANCE: &str = "Marvel/Content/Marvel/UI/Blueprints/League/ScheduleV2/MatchKnockout/WBP_LeagueSchedule_MatchKnockout_Dual.uasset";
    /// A lobby mesh whose clothing asset stores tether batches behind an empty reflected block.
    const CLOTH_MESH: &str =
        "Marvel/Content/Marvel/Characters/1046/1046303/Meshes/SK_1046_1046303_Lobby.uasset";
    /// A skeletal mesh with sampling regions built for Niagara.
    const SAMPLED_MESH: &str =
        "Marvel/Content/Marvel/Characters/1021/1021001/Meshes/SK_1021_1021001.uasset";
    /// A material instance with a Nanite override, cooked as a flag and a hard reference.
    const NANITE_MATERIAL: &str = "Marvel/Content/Marvel/VFX/Materials/Characters/1034/Materials/1034503/MI_1034503_Line_84_003.uasset";
    /// A pose asset: an animation asset that closes with its skeleton guid and nothing after.
    const POSE_ASSET: &str = "Marvel/Content/Marvel/Characters/1063/1063500/AnimationDataAssets/RBF/PA_Thigh_L_UERBFSolver.uasset";
    /// A sound cue whose wave players name their waves after two strip bytes.
    const SOUND_CUE: &str = "Marvel/Content/Marvel/Environment/FluidFlux/Surface/Templates/River/Audio/CS_RiverSource.uasset";
    const RIG: &str = "Marvel/Content/Marvel/Characters/Common/Rig/Paragon_Proto_Retarget.uasset";
    /// Exports whose remainder is cooked data the reader names rather than decodes.
    const NAMED_PAYLOADS: [(&str, &str); 6] = [
        (
            "Marvel/Content/Marvel/Characters/GameMode/2206/4040/Skelot_4040001.uasset",
            "Skelot animation data",
        ),
        (
            "Marvel/Content/Marvel/Wwise/Assets/WwiseBus/spatial/rev_room_wood_small.uasset",
            "Wwise audio data",
        ),
        (
            "Marvel/Content/Marvel/Environment/5007/50070010/GC_BluePrint/Component/GC_50070010Component036I/GC_50070010Component036I.uasset",
            "geometry collection data",
        ),
        (
            "Marvel/Content/Marvel/Environment/5008/Vehicle/TreeCar/BakedAnimData/TreeCarBakedData_5008_B.uasset",
            "vehicle animation baked data",
        ),
        (
            "Marvel/Content/Marvel/Environment/Asgard/AsgardE01/Scenes/Backgrounds/SM_AsgardE01Background010A.uasset",
            "Datasmith scene data",
        ),
        (
            "Marvel/Content/Marvel/Environment/Asgard/Vehicles/GoatChariot/BakedAnimData/TurnBakedData.uasset",
            "baked control rig data",
        ),
    ];
    /// An animation Blueprint whose generated mutable data struct holds an array where the
    /// mappings file's same-named twin, dumped from another Blueprint, holds a float.
    const GROUND_MOTION_ANIM_BP: &str =
        "Marvel/Content/Marvel/Characters/1047/1047001/1047001_1047GroundMotion_AnimBP.uasset";
    /// An animation Blueprint whose default object holds runtime float curves, each carrying a
    /// key handle map that cooked data leaves empty.
    const CURVE_ANIM_BP: &str =
        "Marvel/Content/Marvel/Characters/1011/1011001/1011001_AnimBP.uasset";
    /// Rows holding instanced structs, each guarded by a byte length that has to follow the width
    /// of whatever is edited inside.
    const INSTANCED: &str =
        "Marvel/Content/Marvel/Data/DataTable/GameMode/2206/AIAutoAbilityTable_Zombie.uasset";

    /// Fixtures other modules of this crate open. Their own pins are plain literals in test bodies
    /// rather than constants this module can name, so the strings are repeated here and
    /// `the_fixture_list_holds_every_pinned_path` is what keeps the two copies honest.
    const PAK_ROUND_TRIP: &str =
        "Marvel/Content/Marvel/Data/DataTable/UI/Friends/DT_FriendsRecommendTag.uasset";
    const SYNTH_ROW_TABLE: &str =
        "Marvel/Content/Marvel/Data/DataTable/GameMode/2206/2206_UIHeroInfoTable.uasset";
    const BATCH_SHAKE_HIT: &str =
        "Marvel/Content/Marvel/AbilitySystem/1011/101111/CameraShake_101111_Hit.uasset";
    const BATCH_SHAKE_OTHER: &str =
        "Marvel/Content/Marvel/AbilitySystem/1011/101112/CameraShake_101112_Hit.uasset";

    /// Every asset these tests pin, so one check can tell which pins a game patch broke. A path
    /// that is only ever used as an edit value, never opened, does not belong here.
    const ALL_FIXTURES: &[(&str, &str)] = &[
        ("PAK_ROUND_TRIP", PAK_ROUND_TRIP),
        ("SYNTH_ROW_TABLE", SYNTH_ROW_TABLE),
        ("BATCH_SHAKE_HIT", BATCH_SHAKE_HIT),
        ("BATCH_SHAKE_OTHER", BATCH_SHAKE_OTHER),
        ("STRINGS", STRINGS),
        ("DEFAULTS", DEFAULTS),
        ("MAPS", MAPS),
        ("TITLES", TITLES),
        ("SHAKE", SHAKE),
        ("SAME_NAME_MAP", SAME_NAME_MAP),
        ("ANIM_BLUEPRINT", ANIM_BLUEPRINT),
        ("INPUT_CONTEXT", INPUT_CONTEXT),
        ("REDIRECT_CUE", REDIRECT_CUE),
        ("CHANNEL_ENUM", CHANNEL_ENUM),
        ("TEXTURE", TEXTURE),
        ("STATIC_MESH", STATIC_MESH),
        ("ENTITY_TREE", ENTITY_TREE),
        ("PER_PLATFORM_RATE", PER_PLATFORM_RATE),
        ("SKELETAL_MESH", SKELETAL_MESH),
        ("NUMBER_TEXT", NUMBER_TEXT),
        ("FONT", FONT),
        ("UNRESOLVED_CLASS", UNRESOLVED_CLASS),
        ("METADATA_TABLE", METADATA_TABLE),
        ("SUB_SEQUENCE_TREE", SUB_SEQUENCE_TREE),
        ("TAGGED_TABLE", TAGGED_TABLE),
        ("SOFT_PATH_TABLE", SOFT_PATH_TABLE),
        ("PARENT_CHAIN", PARENT_CHAIN),
        ("PLUGIN_WIDGET", PLUGIN_WIDGET),
        ("GPU_SCRIPT", GPU_SCRIPT),
        ("FLOAT_SECTION", FLOAT_SECTION),
        ("TRANSFORM_SECTION", TRANSFORM_SECTION),
        ("PATCHED_CUE", PATCHED_CUE),
        ("STALE_CLASS_MAP", STALE_CLASS_MAP),
        ("WIDGET_CLASS", WIDGET_CLASS),
        ("WIDGET_INSTANCE", WIDGET_INSTANCE),
        ("TWIN_PARENT_WIDGET", TWIN_PARENT_WIDGET),
        ("TWIN_PARENT_INSTANCE", TWIN_PARENT_INSTANCE),
        ("CLOTH_MESH", CLOTH_MESH),
        ("SAMPLED_MESH", SAMPLED_MESH),
        ("NANITE_MATERIAL", NANITE_MATERIAL),
        ("POSE_ASSET", POSE_ASSET),
        ("SOUND_CUE", SOUND_CUE),
        ("RIG", RIG),
        ("GROUND_MOTION_ANIM_BP", GROUND_MOTION_ANIM_BP),
        ("CURVE_ANIM_BP", CURVE_ANIM_BP),
        ("INSTANCED", INSTANCED),
        ("LEVEL", LEVEL),
        ("HOOP_BLUEPRINT", HOOP_BLUEPRINT),
        ("STRING_TABLE", STRING_TABLE),
        ("STRUCT", STRUCT),
        ("NIAGARA", NIAGARA),
    ];

    /// Paths in the scanned files that no test opens, so the audit must not demand them: an edit
    /// value, and the invented paths the unit tests above the gated ones use.
    const NOT_FIXTURES: &[&str] = &[
        OTHER_MATERIAL,
        "Marvel/Content/Test/Thing.uasset",
        "Marvel/Content/Test/Other.uasset",
        "Marvel/Content/DT_Hero.uasset",
        "Marvel/Content/Marvel/Data/DataTable/GameMode/2206/Row.uasset",
        "Marvel/Content/A.uasset",
    ];

    /// The files that pin a real game asset. A source scan cannot tell a fixture from an example,
    /// so only files whose paths are actually opened belong here: `mods/heroes.rs` names a dozen
    /// real assets to classify them as strings and must stay out.
    const FIXTURE_SOURCES: &[(&str, &str)] = &[
        ("asset_edit.rs", include_str!("asset_edit.rs")),
        ("asset.rs", include_str!("asset.rs")),
        ("asset_edit/sweep.rs", include_str!("asset_edit/sweep.rs")),
        ("pak/iostore_out.rs", include_str!("pak/iostore_out.rs")),
        ("schema_synth.rs", include_str!("schema_synth.rs")),
    ];

    /// Every path the shipped game holds, mount-relative, the way a fixture constant spells it.
    /// Enumeration only: no package bytes are read and no mappings are needed, so checking every
    /// pin costs about as much as opening one of them.
    fn shipped_package_paths(root: &str) -> std::collections::HashSet<String> {
        use crate::pak::containers::{MOUNT_POINT, open_base_game_paks};
        use retoc::{EIoChunkType, FIoChunkId};

        let store = open_base_game_paks(&crate::paths::paks_dir(root), "pakchunk0-Windows")
            .expect("open the base paks");
        store
            .packages_all()
            .filter_map(|pkg| {
                let chunk =
                    FIoChunkId::from_package_id(pkg.id(), 0, EIoChunkType::ExportBundleData);
                let path = store.chunk_path(chunk)?;
                Some(path.strip_prefix(MOUNT_POINT).unwrap_or(&path).to_string())
            })
            .collect()
    }

    /// The gate above only earns its keep if its message is right, and a game patch is a poor time
    /// to find out otherwise. This needs no game install, so it runs on CI where the rest cannot.
    #[test]
    fn a_missing_fixture_reads_as_path_rot_rather_than_a_decoder_bug() {
        let gone = open_failure(
            PARENT_CHAIN,
            &format!("{PARENT_CHAIN} {} pakchunk0-Windows", asset::NOT_A_PACKAGE),
        );
        assert!(gone.contains("FIXTURE GONE"), "{gone}");
        assert!(gone.contains(PARENT_CHAIN), "{gone}");

        let broken = open_failure(STRINGS, "cursor ran past the end of the export");
        assert!(!broken.contains("FIXTURE GONE"), "{broken}");
        assert!(broken.contains("cursor ran past"), "{broken}");
    }

    /// A pinned asset the game no longer ships breaks its test with a failure that says nothing
    /// about which other pins went with it. This names them all at once, so a patch costs one
    /// re-pinning pass rather than a series of reruns.
    #[test]
    fn every_pinned_fixture_is_still_in_the_shipped_game() {
        let Ok(root) = std::env::var("RIVALS_GAME_ROOT") else {
            return;
        };
        let shipped = shipped_package_paths(&root);
        // The one line that tells a run from a skip: both print `... ok` otherwise.
        eprintln!(
            "checked {} pinned fixtures against {} shipped packages",
            ALL_FIXTURES.len() + NAMED_PAYLOADS.len(),
            shipped.len()
        );
        // NAMED_PAYLOADS spells its pairs the other way round, path first.
        let pinned = ALL_FIXTURES
            .iter()
            .map(|(name, path)| (*name, *path))
            .chain(NAMED_PAYLOADS.iter().map(|(path, kind)| (*kind, *path)));
        let mut dead: Vec<String> = pinned
            .filter(|(_, path)| !shipped.contains(*path))
            .map(|(name, path)| format!("  {name}: {path}"))
            .collect();
        dead.sort();
        assert!(
            dead.is_empty(),
            "{} pinned fixture(s) are no longer in the shipped game:\n{}\n\nRe-pin each to an \
             asset of the same shape (its doc comment says which), and update the export names \
             the tests that use it assert.",
            dead.len(),
            dead.join("\n")
        );
    }

    /// The audit is only as good as its list, and nothing makes a new pin join it. This reads the
    /// pinning files back and fails if a path escaped, which is cheaper than discovering it a
    /// season later.
    #[test]
    fn the_fixture_list_holds_every_pinned_path() {
        let known: std::collections::HashSet<&str> = ALL_FIXTURES
            .iter()
            .map(|(_, path)| *path)
            .chain(NAMED_PAYLOADS.iter().map(|(path, _)| *path))
            .chain(NOT_FIXTURES.iter().copied())
            .collect();
        let mut missing: Vec<String> = Vec::new();
        for (name, source) in FIXTURE_SOURCES {
            for path in source.split('"').filter(|part| {
                (part.starts_with("Marvel/") || part.starts_with("Engine/"))
                    && (part.ends_with(".uasset") || part.ends_with(".umap"))
            }) {
                if !known.contains(path) {
                    missing.push(format!("  {name}: {path}"));
                }
            }
        }
        missing.sort();
        missing.dedup();
        assert!(
            missing.is_empty(),
            "these pinned paths are in no fixture list, so the audit would not notice them going \
             missing. Add each to ALL_FIXTURES, or to NOT_FIXTURES if it is never opened:\n{}",
            missing.join("\n")
        );
    }

    /// Tells fixture rot apart from a decoder failure. A pinned asset that a game patch removed is
    /// the common case after an update and says nothing about the reader, so it must not read like
    /// a bug in one.
    fn open_failure(entry: &str, error: &str) -> String {
        if !error.contains(asset::NOT_A_PACKAGE) {
            return format!("load the fixture {entry}: {error}");
        }
        format!(
            "FIXTURE GONE: {entry} is not in the shipped game.\n\n\
             This is fixture rot from a game patch, not a decoder failure. The constant needs \
             re-pinning to an asset of the same shape (its doc comment says which shape), and any \
             export names this test asserts will need updating with it. Run \
             `every_pinned_fixture_is_still_in_the_shipped_game` for the full list of dead pins."
        )
    }

    struct Fixture {
        root: String,
        container: String,
        entry: &'static str,
        schema: std::sync::Arc<Mappings>,
        loaded: FSerializedAssetBundle,
    }

    impl Fixture {
        fn open(entry: &'static str) -> Option<Self> {
            let root = std::env::var("RIVALS_GAME_ROOT").ok()?;
            let usmap = std::env::var("RIVALS_USMAP").ok()?;
            let container = format!(
                "{}/MarvelGame/Marvel/Content/Paks/pakchunk0-Windows.utoc",
                root.replace('\\', "/")
            );
            let schema = mappings::load(std::path::Path::new(&usmap)).expect("mappings");
            let loaded = asset::load_bundle(&root, &container, entry, AssetSource::Utoc)
                .unwrap_or_else(|e| panic!("{}", open_failure(entry, &e)));
            Some(Self {
                root,
                container,
                entry,
                schema,
                loaded,
            })
        }

        fn bundle(&self) -> AssetBundle<'_> {
            AssetBundle {
                asset: &self.loaded.asset_file_buffer,
                exports: &self.loaded.exports_file_buffer,
            }
        }

        fn source(&self) -> PackageSource<'_> {
            PackageSource {
                game_root: &self.root,
                container: &self.container,
                entry: self.entry,
                kind: AssetSource::Utoc,
            }
        }

        fn parse(&self) -> rivals_uasset::ParsedPackage {
            Self::parse_bundle(&self.bundle(), &self.schema, &self.source())
        }

        /// The editor's own parse: every declared slot listed, stored or not.
        fn parse_bundle(
            bundle: &AssetBundle<'_>,
            schema: &Mappings,
            source: &PackageSource<'_>,
        ) -> rivals_uasset::ParsedPackage {
            schema_synth::parse_package_opts(
                bundle,
                Some(schema),
                source,
                ParseOptions {
                    declared_slots: true,
                    ..Default::default()
                },
            )
            .expect("parse")
        }

        fn request(&self, edits: Vec<ValueEdit>) -> AssetEditRequest<'_> {
            self.request_all(edits, Vec::new())
        }

        fn request_all(
            &self,
            edits: Vec<ValueEdit>,
            imports: Vec<ImportEdit>,
        ) -> AssetEditRequest<'_> {
            AssetEditRequest {
                game_root: &self.root,
                container: &self.container,
                entry: self.entry,
                kind: AssetSource::Utoc,
                mod_name: "unused, preview writes nothing",
                changes: PackageEdits {
                    values: edits,
                    imports,
                    ..Default::default()
                },
            }
        }

        fn request_changes(&self, changes: PackageEdits) -> AssetEditRequest<'_> {
            AssetEditRequest {
                game_root: &self.root,
                container: &self.container,
                entry: self.entry,
                kind: AssetSource::Utoc,
                mod_name: "unused, preview writes nothing",
                changes,
            }
        }

        /// Applies the edits and re-reads the result, which is what proves an edit is coherent.
        fn apply(&self, edits: Vec<ValueEdit>) -> (PatchedBundle, rivals_uasset::ParsedPackage) {
            self.apply_all(edits, Vec::new())
        }

        fn apply_all(
            &self,
            edits: Vec<ValueEdit>,
            imports: Vec<ImportEdit>,
        ) -> (PatchedBundle, rivals_uasset::ParsedPackage) {
            self.apply_changes(PackageEdits {
                values: edits,
                imports,
                ..Default::default()
            })
        }

        fn apply_changes(
            &self,
            changes: PackageEdits,
        ) -> (PatchedBundle, rivals_uasset::ParsedPackage) {
            let request = self.request_changes(changes);
            let (patched, _) = preview_edits(&request, Some(&self.schema)).expect("preview");
            let reread = AssetBundle {
                asset: &patched.asset,
                exports: &patched.exports,
            };
            let after = Self::parse_bundle(&reread, &self.schema, &self.source());
            (patched, after)
        }
    }

    /// A replacement for this cell that is the same width and differs in exactly one byte: a bool
    /// flips, an integer flips its low bit, an ASCII string swaps its first character. Which kind
    /// supplies it does not matter, the width is what is under test.
    fn one_byte_change(field: &PropertyEntry) -> Option<String> {
        match &field.value {
            PropertyValue::Bool { value } => Some((!value).to_string()),
            PropertyValue::Int { value } => Some((value ^ 1).to_string()),
            PropertyValue::Str { value } => {
                let first = value.chars().next()?;
                first.is_ascii_graphic().then(|| {
                    let swapped = if first == 'A' { 'B' } else { 'A' };
                    format!("{swapped}{}", &value[first.len_utf8()..])
                })
            }
            _ => None,
        }
    }

    /// The first row cell the predicate accepts. A miss usually means a game patch reshaped the
    /// table, so the message says what the table does hold rather than only that nothing matched.
    fn field_where(
        parsed: &rivals_uasset::ParsedPackage,
        what: &str,
        want: impl Fn(&PropertyEntry) -> bool,
    ) -> PropertyEntry {
        let table = parsed.exports[0]
            .data_table
            .as_ref()
            .unwrap_or_else(|| panic!("{} holds no data table", parsed.info.package_name));
        table
            .rows
            .iter()
            .flat_map(|row| &row.fields)
            .find(|field| want(field))
            .unwrap_or_else(|| {
                let mut kinds: std::collections::BTreeMap<String, usize> = Default::default();
                for field in table.rows.iter().flat_map(|row| &row.fields).filter(|f| stored(f)) {
                    *kinds.entry(rivals_uasset::kind_of(&field.value)).or_default() += 1;
                }
                let held: Vec<String> = kinds.iter().map(|(k, n)| format!("{k} x{n}")).collect();
                panic!(
                    "no stored cell that {what} in {}; its stored cells are {}.\nIf a game patch reshaped the table, widen the test or re-pin it.",
                    parsed.info.package_name,
                    held.join(", ")
                )
            })
            .clone()
    }

    /// The first field with this name anywhere in the package: every export's property tree and,
    /// for a DataTable, the cells of every row. `field_where` only reaches a row's top level, and
    /// these sit inside instanced struct payloads several structs down.
    fn find_named(parsed: &rivals_uasset::ParsedPackage, name: &str) -> Option<PropertyEntry> {
        fn inside(value: &PropertyValue, name: &str) -> Option<PropertyEntry> {
            match value {
                PropertyValue::Struct { fields, .. } => walk(fields, name),
                PropertyValue::Array { items } | PropertyValue::Set { items } => {
                    items.iter().find_map(|item| inside(item, name))
                }
                PropertyValue::Map { entries } => {
                    entries.iter().find_map(|pair| inside(&pair.value, name))
                }
                _ => None,
            }
        }
        fn walk(entries: &[PropertyEntry], name: &str) -> Option<PropertyEntry> {
            entries.iter().find_map(|entry| {
                if entry.name == name {
                    return Some(entry.clone());
                }
                match &entry.value {
                    PropertyValue::Text { parts, .. } => walk(parts, name),
                    other => inside(other, name),
                }
            })
        }
        parsed.exports.iter().find_map(|export| {
            walk(&export.properties, name).or_else(|| {
                export
                    .data_table
                    .as_ref()?
                    .rows
                    .iter()
                    .find_map(|row| walk(&row.fields, name))
            })
        })
    }

    fn segments_of(field: &PropertyEntry) -> Vec<String> {
        let PropertyValue::Array { items } = &field.value else {
            panic!("expected the segments as an array, got {:?}", field.value);
        };
        items.iter().map(PropertyValue::summary).collect()
    }

    /// The game writes `FSerializablePropertySoftPath` with a serializer of its own, so reading it
    /// against the mappings fails inside the instanced payload that holds it. The payload's length
    /// prefix hides that: the export still reads as exact.
    #[test]
    fn a_property_soft_path_reads_its_segments_and_leaves_nothing_undecoded() {
        let Some(fixture) = Fixture::open(SOFT_PATH_TABLE) else {
            return;
        };
        let parsed = fixture.parse();
        let hidden: Vec<String> = parsed
            .exports
            .iter()
            .flat_map(|export| {
                export
                    .undecoded
                    .iter()
                    .map(|p| format!("{}#{}: {}", export.index, p.struct_name, p.reason))
            })
            .collect();
        assert!(hidden.is_empty(), "payloads left undecoded: {hidden:?}");

        let segments = find_named(&parsed, "PropertySoftPath").expect("a property soft path");
        assert_eq!(segments_of(&segments), ["ScopeQuote", "Spawn_AgentId"]);
    }

    /// The segment list is counted by a single byte, so growing it moves that byte rather than a
    /// word, and the enclosing payload's length prefix follows. Writing a four byte count here
    /// would run over the first segment's own length and the re-read would not come back.
    #[test]
    fn a_property_soft_path_grows_and_shrinks_by_a_segment() {
        let Some(fixture) = Fixture::open(SOFT_PATH_TABLE) else {
            return;
        };
        let before = fixture.parse();
        let segments = find_named(&before, "PropertySoftPath").expect("a property soft path");

        let (_, after) = fixture.apply(vec![edit_of(
            &segments,
            EditOp::Insert {
                index: 2,
                key: None,
            },
        )]);
        let grown = find_named(&after, "PropertySoftPath").expect("the segments after the insert");
        assert_eq!(
            segments_of(&grown),
            ["ScopeQuote", "Spawn_AgentId", "Spawn_AgentId"],
            "a new element copies the one before it"
        );

        let (_, after) = fixture.apply(vec![edit_of(&segments, EditOp::Remove { index: 0 })]);
        let shrunk = find_named(&after, "PropertySoftPath").expect("the segments after the drop");
        assert_eq!(segments_of(&shrunk), ["Spawn_AgentId"]);
    }

    fn edit_of(field: &PropertyEntry, op: EditOp) -> ValueEdit {
        ValueEdit {
            offset: field.span.expect("a span").0,
            expect_name: field.name.clone(),
            expect_element: field.element,
            expect_kind: rivals_uasset::kind_of(&field.value),
            op,
        }
    }

    /// The longest stored string in the table, so a shrink test has something to shrink.
    fn longest_string(parsed: &rivals_uasset::ParsedPackage) -> PropertyEntry {
        table_of(parsed)
            .rows
            .iter()
            .flat_map(|row| &row.fields)
            .filter(|f| stored(f))
            .filter_map(|f| match &f.value {
                PropertyValue::Str { value } => Some((value.len(), f)),
                _ => None,
            })
            .max_by_key(|(length, _)| *length)
            .expect("a stored string")
            .1
            .clone()
    }

    fn stored(field: &PropertyEntry) -> bool {
        field.span.is_some_and(|(start, end)| end > start)
    }

    fn table_of(parsed: &rivals_uasset::ParsedPackage) -> &rivals_uasset::DataTable {
        parsed.exports[0].data_table.as_ref().expect("a data table")
    }

    /// The narrowest case: one byte changes and nothing moves, so the header must be untouched.
    #[test]
    fn a_same_width_edit_changes_exactly_one_byte_and_moves_nothing() {
        let Some(fixture) = Fixture::open(STRINGS) else {
            return;
        };
        let before = fixture.parse();
        let field = field_where(&before, "can be rewritten one byte wide", |f| {
            stored(f) && one_byte_change(f).is_some()
        });
        let text = one_byte_change(&field).expect("the predicate accepted this cell");

        let (patched, after) = fixture.apply(vec![edit_of(&field, EditOp::Set { text })]);

        let changed: Vec<usize> = fixture
            .loaded
            .exports_file_buffer
            .iter()
            .zip(&patched.exports)
            .enumerate()
            .filter(|(_, (old, new))| old != new)
            .map(|(at, _)| at)
            .collect();
        assert_eq!(
            changed.len(),
            1,
            "expected one changed byte, got {changed:?}"
        );
        assert_eq!(
            fixture.loaded.asset_file_buffer, patched.asset,
            "a same-width edit must not touch the header"
        );
        assert!(matches!(after.exports[0].status, ExportStatus::Complete));
    }

    /// Lengthening a string is the whole point: the export grows, the export table has to follow,
    /// and the value has to read back as typed.
    #[test]
    fn a_longer_string_grows_the_export_and_still_reads_back() {
        let Some(fixture) = Fixture::open(STRINGS) else {
            return;
        };
        let before = fixture.parse();
        let field = field_where(&before, "is a stored non-empty string", |f| {
            matches!(&f.value, PropertyValue::Str { value } if !value.is_empty()) && stored(f)
        });
        let PropertyValue::Str { value: ref was } = field.value else {
            unreachable!()
        };
        let longer = format!("{was} and then some more text");

        let (patched, after) = fixture.apply(vec![edit_of(
            &field,
            EditOp::Set {
                text: longer.clone(),
            },
        )]);

        assert_eq!(
            patched.exports.len(),
            fixture.loaded.exports_file_buffer.len() + longer.len() - was.len(),
            "the export data grows by exactly the extra characters"
        );
        // The summary's bulk data start is derived from the export table as the header is written,
        // so it must have followed the export that grew.
        let header = rivals_uasset::read_header(&AssetBundle {
            asset: &patched.asset,
            exports: &patched.exports,
        })
        .expect("header");
        let end = header
            .exports
            .iter()
            .map(|e| e.serial_offset + e.serial_size)
            .max()
            .expect("exports");
        assert_eq!(header.summary.bulk_data_start_offset, end);
        assert!(matches!(after.exports[0].status, ExportStatus::Complete));
        assert_eq!(
            table_of(&after)
                .rows
                .iter()
                .flat_map(|row| &row.fields)
                .filter(|f| matches!(&f.value, PropertyValue::Str { value } if *value == longer))
                .count(),
            1
        );
        assert_eq!(
            table_of(&after).rows.len(),
            table_of(&before).rows.len(),
            "no row may be lost"
        );
    }

    #[test]
    fn a_shorter_string_shrinks_the_export_and_still_reads_back() {
        let Some(fixture) = Fixture::open(STRINGS) else {
            return;
        };
        let before = fixture.parse();
        let field = longest_string(&before);
        let PropertyValue::Str { value: ref was } = field.value else {
            unreachable!()
        };
        assert!(was.len() > 1, "nothing here is long enough to shorten");

        let (patched, after) =
            fixture.apply(vec![edit_of(&field, EditOp::Set { text: "x".into() })]);

        assert_eq!(
            patched.exports.len(),
            fixture.loaded.exports_file_buffer.len() + 1 - was.len()
        );
        assert!(matches!(after.exports[0].status, ExportStatus::Complete));
    }

    /// Emptying a string is the narrowest shrink there is: four bytes and no payload.
    #[test]
    fn a_string_can_be_emptied() {
        let Some(fixture) = Fixture::open(STRINGS) else {
            return;
        };
        let before = fixture.parse();
        let field = field_where(&before, "is a stored non-empty string", |f| {
            matches!(&f.value, PropertyValue::Str { value } if !value.is_empty()) && stored(f)
        });
        let (_, after) = fixture.apply(vec![edit_of(
            &field,
            EditOp::Set {
                text: String::new(),
            },
        )]);
        assert!(matches!(after.exports[0].status, ExportStatus::Complete));
    }

    /// A value holding its default occupies no bytes. Storing one has to clear its mask bit and
    /// insert the value, and the header must keep its length because the bit was already there.
    #[test]
    fn a_defaulted_value_can_be_given_a_value_of_its_own() {
        let Some(fixture) = Fixture::open(DEFAULTS) else {
            return;
        };
        let before = fixture.parse();
        let field = field_where(&before, "is an int left to its default", |f| {
            matches!(f.value, PropertyValue::Int { .. })
                && !stored(f)
                && f.slot.is_some_and(|slot| slot.declared == "Int")
        });

        let (patched, after) = fixture.apply(vec![edit_of(
            &field,
            EditOp::Set {
                text: "12345".into(),
            },
        )]);

        assert_eq!(
            patched.exports.len(),
            fixture.loaded.exports_file_buffer.len() + 4,
            "storing a four-byte integer costs four bytes and no more"
        );
        assert!(matches!(after.exports[0].status, ExportStatus::Complete));
        assert!(
            table_of(&after)
                .rows
                .iter()
                .flat_map(|row| &row.fields)
                .any(|f| f.name == field.name
                    && matches!(f.value, PropertyValue::Int { value: 12345 })),
            "the value has to come back as the one that was asked for"
        );
    }

    /// The reverse: a stored value goes back to its default and its bytes disappear.
    #[test]
    fn a_stored_value_can_be_sent_back_to_its_default() {
        let Some(fixture) = Fixture::open(DEFAULTS) else {
            return;
        };
        let before = fixture.parse();
        let field = field_where(&before, "is a four byte int", |f| {
            matches!(f.value, PropertyValue::Int { .. })
                && f.span.is_some_and(|(start, end)| end - start == 4)
        });

        let (patched, after) = fixture.apply(vec![edit_of(&field, EditOp::Clear)]);

        assert!(
            patched.exports.len() <= fixture.loaded.exports_file_buffer.len() - 4,
            "the four bytes have to go, and the mask may cost one more"
        );
        assert!(matches!(after.exports[0].status, ExportStatus::Complete));
    }

    /// Several edits at once is where an offset fixup goes wrong, because each one moves the ones
    /// after it. All of them have to land.
    #[test]
    fn several_string_edits_in_one_save_all_land() {
        let Some(fixture) = Fixture::open(STRINGS) else {
            return;
        };
        let before = fixture.parse();
        let table = table_of(&before);
        let fields: Vec<PropertyEntry> = table
            .rows
            .iter()
            .flat_map(|row| &row.fields)
            .filter(|f| matches!(&f.value, PropertyValue::Str { value } if !value.is_empty()))
            .take(5)
            .cloned()
            .collect();
        assert_eq!(
            fields.len(),
            5,
            "the table should have five strings to edit"
        );

        let wanted: Vec<String> = (0..5).map(|n| format!("edited value number {n}")).collect();
        let edits = fields
            .iter()
            .zip(&wanted)
            .map(|(field, text)| edit_of(field, EditOp::Set { text: text.clone() }))
            .collect();

        let (_, after) = fixture.apply(edits);
        assert!(matches!(after.exports[0].status, ExportStatus::Complete));
        for text in &wanted {
            assert!(
                table_of(&after)
                    .rows
                    .iter()
                    .flat_map(|row| &row.fields)
                    .any(|f| matches!(&f.value, PropertyValue::Str { value } if value == text)),
                "{text} did not survive the save"
            );
        }
    }

    /// Pointing a soft object at something the package has never named appends to the name map,
    /// which is the first section after the summary. The header grows, so every export offset in
    /// the table moves, and the only proof that went right is reading the whole thing back.
    #[test]
    fn a_new_asset_reference_grows_the_name_map_and_moves_every_export() {
        let Some(fixture) = Fixture::open(STRINGS) else {
            return;
        };
        let before = fixture.parse();
        let field = field_where(&before, "is a stored soft object path", |f| {
            matches!(&f.value, PropertyValue::SoftObject { path } if !path.is_empty()) && stored(f)
        });
        let wanted = "/Game/Marvel/UI/Invented/WBP_NotReal.WBP_NotReal_C";

        let (patched, after) = fixture.apply(vec![edit_of(
            &field,
            EditOp::Set {
                text: wanted.into(),
            },
        )]);

        assert!(
            patched.asset.len() > fixture.loaded.asset_file_buffer.len(),
            "a name the package did not have has to lengthen the header"
        );
        assert!(matches!(after.exports[0].status, ExportStatus::Complete));
        assert_eq!(
            table_of(&after).rows.len(),
            table_of(&before).rows.len(),
            "every row still has to decode"
        );
        assert_eq!(
            table_of(&after)
                .rows
                .iter()
                .flat_map(|row| &row.fields)
                .filter(
                    |f| matches!(&f.value, PropertyValue::SoftObject { path } if path == wanted)
                )
                .count(),
            1
        );
    }

    /// Pointing at something the package already names must not grow anything, which is the case
    /// worth separating because it proves the append is conditional rather than unconditional.
    #[test]
    fn reusing_a_name_the_package_already_has_leaves_the_header_alone() {
        let Some(fixture) = Fixture::open(STRINGS) else {
            return;
        };
        let before = fixture.parse();
        let table = table_of(&before);
        let paths: Vec<String> = table
            .rows
            .iter()
            .flat_map(|row| &row.fields)
            .filter_map(|f| match &f.value {
                PropertyValue::SoftObject { path } if !path.is_empty() => Some(path.clone()),
                _ => None,
            })
            .collect();
        let other = paths
            .iter()
            .find(|path| **path != paths[0])
            .cloned()
            .expect("two different asset references");
        let field = field_where(
            &before,
            "is the soft object path this test wrote",
            |f| matches!(&f.value, PropertyValue::SoftObject { path } if *path == paths[0]),
        );

        let (patched, after) = fixture.apply(vec![edit_of(
            &field,
            EditOp::Set {
                text: other.clone(),
            },
        )]);

        assert_eq!(
            patched.asset.len(),
            fixture.loaded.asset_file_buffer.len(),
            "no new name means no new header bytes"
        );
        assert_eq!(
            patched.exports.len(),
            fixture.loaded.exports_file_buffer.len(),
            "two references of the same shape are the same width"
        );
        assert!(matches!(after.exports[0].status, ExportStatus::Complete));
    }

    /// An FName is eight bytes whatever it spells, so this is a fixed-width edit that still has to
    /// lengthen the header when the name it points at is new.
    #[test]
    fn renaming_to_a_name_the_package_has_never_seen_still_reads_back() {
        let Some(fixture) = Fixture::open(DEFAULTS) else {
            return;
        };
        let before = fixture.parse();
        let field = field_where(&before, "is a stored name", |f| {
            matches!(&f.value, PropertyValue::Name { .. }) && stored(f)
        });
        let wanted = "ANameNoPackageHasEverHeldBefore";

        let (patched, after) = fixture.apply(vec![edit_of(
            &field,
            EditOp::Set {
                text: wanted.into(),
            },
        )]);

        assert_eq!(
            patched.exports.len(),
            fixture.loaded.exports_file_buffer.len(),
            "an FName is eight bytes whichever name it points at"
        );
        assert!(
            patched.asset.len() > fixture.loaded.asset_file_buffer.len(),
            "the name itself has to be added to the map"
        );
        assert!(matches!(after.exports[0].status, ExportStatus::Complete));
        assert!(
            table_of(&after)
                .rows
                .iter()
                .flat_map(|row| &row.fields)
                .any(|f| matches!(&f.value, PropertyValue::Name { value } if value == wanted))
        );
    }

    /// An array with something in it. Containers are where the reader records element spans, and
    /// nothing else in the table exercises them.
    fn array_field(parsed: &rivals_uasset::ParsedPackage) -> PropertyEntry {
        table_of(parsed)
            .rows
            .iter()
            .flat_map(|row| &row.fields)
            .filter(|f| stored(f))
            .find(|f| matches!(&f.value, PropertyValue::Array { items } if items.len() >= 2))
            .expect("an array with at least two elements")
            .clone()
    }

    fn count_of(value: &PropertyValue) -> usize {
        match value {
            PropertyValue::Array { items } | PropertyValue::Set { items } => items.len(),
            PropertyValue::Map { entries } => entries.len(),
            _ => panic!("not a container"),
        }
    }

    /// Elements held by every column of this name across the whole table. Rows share column names,
    /// so picking one row's array back out after the edit is guesswork; the total is not.
    fn total_elements(parsed: &rivals_uasset::ParsedPackage, column: &str) -> usize {
        table_of(parsed)
            .rows
            .iter()
            .flat_map(|row| &row.fields)
            .filter(|f| f.name == column)
            .filter(|f| {
                matches!(
                    f.value,
                    PropertyValue::Array { .. }
                        | PropertyValue::Set { .. }
                        | PropertyValue::Map { .. }
                )
            })
            .map(|f| count_of(&f.value))
            .sum()
    }

    /// Adding to an array copies the element already at that position, so the bytes are always
    /// valid whatever the element type is, and the count in front of them has to follow.
    #[test]
    fn adding_an_array_element_lengthens_the_array_and_its_count() {
        let Some(fixture) = Fixture::open(DEFAULTS) else {
            return;
        };
        let before = fixture.parse();
        let field = array_field(&before);
        let was = total_elements(&before, &field.name);

        let (_, after) = fixture.apply(vec![edit_of(
            &field,
            EditOp::Insert {
                index: 1,
                key: None,
            },
        )]);

        assert!(matches!(after.exports[0].status, ExportStatus::Complete));
        assert_eq!(total_elements(&after, &field.name), was + 1);
    }

    #[test]
    fn dropping_an_array_element_shortens_the_array_and_its_count() {
        let Some(fixture) = Fixture::open(DEFAULTS) else {
            return;
        };
        let before = fixture.parse();
        let field = array_field(&before);
        let was = total_elements(&before, &field.name);

        let (patched, after) = fixture.apply(vec![edit_of(&field, EditOp::Remove { index: 0 })]);

        assert!(matches!(after.exports[0].status, ExportStatus::Complete));
        assert!(
            patched.exports.len() < fixture.loaded.exports_file_buffer.len(),
            "dropping an element has to make the export smaller"
        );
        assert_eq!(total_elements(&after, &field.name), was - 1);
    }

    /// Adding then dropping the same position has to leave the file exactly as it was. Anything
    /// that leaks bytes or leaves a count wrong shows up here and nowhere else.
    #[test]
    fn adding_an_element_and_dropping_it_again_restores_the_original_bytes() {
        let Some(fixture) = Fixture::open(DEFAULTS) else {
            return;
        };
        let before = fixture.parse();
        let field = array_field(&before);

        let (grown, _) = fixture.apply(vec![edit_of(
            &field,
            EditOp::Insert {
                index: 1,
                key: None,
            },
        )]);

        // The second edit works on the grown package, so it has to be re-read rather than reused.
        let bundle = AssetBundle {
            asset: &grown.asset,
            exports: &grown.exports,
        };
        let parsed = schema_synth::parse_package(&bundle, Some(&fixture.schema), &fixture.source())
            .expect("parse the grown table");
        let again = parsed.exports[0]
            .data_table
            .as_ref()
            .expect("a data table")
            .rows
            .iter()
            .flat_map(|row| &row.fields)
            .find(|f| f.name == field.name && count_of(&f.value) == count_of(&field.value) + 1)
            .expect("the array that grew")
            .clone();

        let shrunk = rivals_uasset::patch_values(
            &bundle,
            &parsed,
            &[edit_of(&again, EditOp::Remove { index: 1 })],
            None,
        )
        .expect("remove");

        assert_eq!(
            shrunk.exports, fixture.loaded.exports_file_buffer,
            "adding and dropping the same element must be a round trip"
        );
        assert_eq!(shrunk.asset, fixture.loaded.asset_file_buffer);
    }

    /// A container element has no property entry of its own, so it is addressed through the
    /// container plus an index. Editing one has to leave its neighbours alone.
    #[test]
    fn an_array_element_can_be_edited_through_its_container() {
        let Some(fixture) = Fixture::open(DEFAULTS) else {
            return;
        };
        let before = fixture.parse();
        let field = table_of(&before)
            .rows
            .iter()
            .flat_map(|row| &row.fields)
            .filter(|f| stored(f))
            .find(|f| match &f.value {
                PropertyValue::Array { items } => {
                    items.len() >= 2 && matches!(items[0], PropertyValue::Int { .. })
                }
                _ => false,
            })
            .cloned();
        let Some(field) = field else {
            return;
        };

        let (_, after) = fixture.apply(vec![edit_of(
            &field,
            EditOp::SetElement {
                index: 0,
                text: "4242".into(),
            },
        )]);

        assert!(matches!(after.exports[0].status, ExportStatus::Complete));
        assert!(
            table_of(&after)
                .rows
                .iter()
                .flat_map(|row| &row.fields)
                .any(|f| matches!(&f.value, PropertyValue::Array { items }
                    if matches!(items.first(), Some(PropertyValue::Int { value: 4242 })))),
            "the element has to come back as the number that was asked for"
        );
    }

    /// The material import the asset's `Title_Deluxe_Mat_Dark` property points at.
    fn material_import(parsed: &rivals_uasset::ParsedPackage) -> (PropertyEntry, i32) {
        let field = nested(&parsed.exports[0].properties, &["Title_Deluxe_Mat_Dark"]).clone();
        let PropertyValue::Object { index, .. } = field.value else {
            panic!(
                "expected an object reference, got {}",
                field.value.summary()
            );
        };
        assert!(index < 0, "the material is an import");
        (field, index)
    }

    /// Retargeting an import is a header-only edit: the export bytes do not change, yet every
    /// property that pointed at the import now reads the new path, and the import keeps its class.
    #[test]
    fn an_import_can_be_pointed_at_another_asset() {
        let Some(fixture) = Fixture::open(TITLES) else {
            return;
        };
        let before = fixture.parse();
        let (field, index) = material_import(&before);
        let was = before
            .imports
            .iter()
            .find(|i| i.index == index)
            .expect("the import")
            .clone();
        assert_eq!(was.class_name, "MaterialInstanceConstant");

        let (patched, after) = fixture.apply_all(
            Vec::new(),
            vec![ImportEdit::Retarget {
                import: (-index - 1) as u32,
                path: OTHER_MATERIAL.into(),
                class: None,
            }],
        );

        assert_eq!(
            patched.exports, fixture.loaded.exports_file_buffer,
            "an import edit touches the header alone"
        );
        assert!(patched.asset.len() > fixture.loaded.asset_file_buffer.len());
        let now = after
            .imports
            .iter()
            .find(|i| i.index == index)
            .expect("the import is still there");
        assert_eq!(now.path, OTHER_MATERIAL);
        assert_eq!(now.class_name, "MaterialInstanceConstant");
        assert_eq!(
            after.imports.len(),
            before.imports.len() + 1,
            "the new package needs an import of its own; the old one stays"
        );
        let value = nested(&after.exports[0].properties, &[field.name.as_str()]);
        assert!(
            matches!(&value.value, PropertyValue::Object { path: Some(path), .. } if path.ends_with("MI_ColorFont_Dark")),
            "{}",
            value.value.summary()
        );
        assert!(matches!(after.exports[0].status, ExportStatus::Complete));
    }

    /// Typing a path the package never named into an object cell adds the imports for it, typed
    /// like the object the property pointed at before.
    #[test]
    fn an_object_reference_can_reach_an_asset_the_package_never_named() {
        let Some(fixture) = Fixture::open(TITLES) else {
            return;
        };
        let before = fixture.parse();
        let (field, _) = material_import(&before);

        let (_, after) = fixture.apply(vec![edit_of(
            &field,
            EditOp::Set {
                text: OTHER_MATERIAL.into(),
            },
        )]);

        assert_eq!(
            after.imports.len(),
            before.imports.len() + 2,
            "one import for the package, one for the material"
        );
        let added = after
            .imports
            .iter()
            .find(|i| i.path == OTHER_MATERIAL)
            .expect("the material import");
        assert_eq!(added.class_name, "MaterialInstanceConstant");
        let value = nested(&after.exports[0].properties, &[field.name.as_str()]);
        assert!(
            matches!(&value.value, PropertyValue::Object { index, .. } if *index == added.index),
            "{}",
            value.value.summary()
        );
        assert!(matches!(after.exports[0].status, ExportStatus::Complete));
    }

    const LEVEL: &str = "Marvel/Content/Marvel/Maps/Battle/SimpleSelectHeroLevel.umap";
    /// A blueprint class in its own package, whose default object stores values under it.
    const HOOP_BLUEPRINT: &str = "Marvel/Content/Marvel/AbilitySystem/Common/GameAbility/MarvelEmoteBasketBallHoop_BP.uasset";
    const STRING_TABLE: &str = "Marvel/Content/Marvel/Data/StringTable/104_Currency_ST.uasset";
    const STRUCT: &str = "Marvel/Content/Marvel/Data/Struct/CardData.uasset";

    /// Every actor in a level carries this game's label trailer and every component its list of
    /// construction-script changes; with both read, a level leaves nothing unexplained.
    #[test]
    fn level_actors_and_components_read_exactly() {
        let Some(fixture) = Fixture::open(LEVEL) else {
            return;
        };
        let parsed = fixture.parse();
        let unexplained: Vec<String> = parsed
            .exports
            .iter()
            .filter(|export| {
                matches!(
                    export.status,
                    ExportStatus::Partial { .. } | ExportStatus::Failed { .. }
                )
            })
            .map(|export| format!("{} ({})", export.object_name, export.class_name))
            .collect();
        assert!(unexplained.is_empty(), "{unexplained:?}");
        let sky = parsed
            .exports
            .iter()
            .find(|export| export.object_name == "SkySphereMesh")
            .expect("the sky sphere component");
        assert!(matches!(sky.status, ExportStatus::Complete));
        assert!(
            parsed
                .exports
                .iter()
                .any(|export| export.class_name == "StaticMeshActor"
                    && matches!(export.status, ExportStatus::Complete))
        );
        // The component's modified-property records name the class each property belongs to.
        let inside_sky = parsed
            .references
            .iter()
            .filter(|reference| {
                reference.at >= sky.serial_offset as u64
                    && reference.at < (sky.serial_offset + sky.serial_size) as u64
            })
            .count();
        assert!(
            inside_sky >= 1,
            "the UCS record's class index is a reference"
        );
    }

    /// A mod that gets deleted whatever the test does, so a failure does not leave a container in
    /// the real `~mods` for the game to load.
    struct ScratchMod {
        root: String,
        name: &'static str,
    }

    impl ScratchMod {
        fn container(&self) -> std::path::PathBuf {
            mod_pak_path(&self.root, self.name)
                .expect("pak path")
                .with_extension("utoc")
        }
    }

    impl Drop for ScratchMod {
        fn drop(&mut self) {
            let Ok(pak) = mod_pak_path(&self.root, self.name) else {
                return;
            };
            for extension in ["pak", "utoc", "ucas"] {
                let _ = fs::remove_file(pak.with_extension(extension));
            }
        }
    }

    /// A material's shader maps are listed in its container's header, not in the package, so the
    /// first save into a fresh mod has to bring them from the game or the material loses them.
    #[test]
    fn a_material_keeps_its_shader_maps_in_a_fresh_mod() {
        let Some(fixture) = Fixture::open(NANITE_MATERIAL) else {
            return;
        };
        let scratch = ScratchMod {
            root: fixture.root.clone(),
            name: "RivalsToolkitShaderMapProbe",
        };
        drop(ScratchMod {
            root: fixture.root.clone(),
            name: "RivalsToolkitShaderMapProbe",
        });
        let header = rivals_uasset::read_header(&fixture.bundle()).expect("header");
        let package = header.summary.package_name.clone();
        let base = asset::shader_map_hashes(&fixture.root, Some(&fixture.container), &package);
        assert!(!base.is_empty(), "{package} lists shader maps in the game");

        let before = fixture.parse();
        let value = find_named(&before, "ParameterValue").expect("a scalar parameter");
        let PropertyValue::Float { value: was } = value.value else {
            unreachable!()
        };
        save_edits(
            &AssetEditRequest {
                game_root: &fixture.root,
                container: &fixture.container,
                entry: fixture.entry,
                kind: AssetSource::Utoc,
                mod_name: scratch.name,
                changes: PackageEdits {
                    values: vec![ValueEdit {
                        offset: value.span.expect("a span").0,
                        expect_name: value.name.clone(),
                        expect_element: value.element,
                        expect_kind: "float".into(),
                        op: EditOp::Set {
                            text: (was + 0.5).to_string(),
                        },
                    }],
                    ..Default::default()
                },
            },
            Some(&fixture.schema),
            &SaveOptions::default(),
        )
        .expect("save");

        let written = crate::pak::containers::open_utoc(&scratch.container().to_string_lossy())
            .expect("open the mod");
        let id = retoc::FPackageId(retoc::FIoContainerId::from_name(&package).0);
        let entry = written.package_store_entry(id).expect("the mod lists it");
        assert_eq!(entry.shader_map_hashes, base);
    }

    /// The first stored int in each of the first `count` rows of the hero table, for saves that
    /// change one cell each.
    fn row_ints(parsed: &rivals_uasset::ParsedPackage, count: usize) -> Vec<PropertyEntry> {
        parsed.exports[0].data_table.as_ref().expect("table").rows[..count]
            .iter()
            .map(|row| {
                row.fields
                    .iter()
                    .find(|field| matches!(field.value, PropertyValue::Int { .. }))
                    .expect("an int cell")
                    .clone()
            })
            .collect()
    }

    fn bump(cell: &PropertyEntry, by: i64) -> ValueEdit {
        let PropertyValue::Int { value } = cell.value else {
            unreachable!()
        };
        ValueEdit {
            offset: cell.span.expect("a span").0,
            expect_name: cell.name.clone(),
            expect_element: cell.element,
            expect_kind: "int".into(),
            op: EditOp::Set {
                text: (value + by).to_string(),
            },
        }
    }

    /// Reads the mod's copy of `entry` back out of its container.
    fn read_back(fixture: &Fixture, utoc: &Path) -> rivals_uasset::ParsedPackage {
        let container = utoc.to_string_lossy().into_owned();
        let loaded =
            asset::load_bundle(&fixture.root, &container, fixture.entry, AssetSource::Utoc)
                .expect("read the saved copy back");
        Fixture::parse_bundle(
            &AssetBundle {
                asset: &loaded.asset_file_buffer,
                exports: &loaded.exports_file_buffer,
            },
            &fixture.schema,
            &PackageSource {
                game_root: &fixture.root,
                container: &container,
                entry: fixture.entry,
                kind: AssetSource::Utoc,
            },
        )
    }

    /// A second save stops at the copy the first left, unless it builds on it: then both edits are
    /// in the mod, rather than the second one alone on top of the original.
    #[test]
    fn a_layered_save_keeps_what_the_mod_already_carries() {
        let Some(fixture) = Fixture::open(DEFAULTS) else {
            return;
        };
        let scratch = ScratchMod {
            root: fixture.root.clone(),
            name: "RivalsToolkitLayerProbe",
        };
        drop(ScratchMod {
            root: fixture.root.clone(),
            name: "RivalsToolkitLayerProbe",
        });
        let before = fixture.parse();
        let cells = row_ints(&before, 2);
        let request = |edit: ValueEdit| AssetEditRequest {
            game_root: &fixture.root,
            container: &fixture.container,
            entry: fixture.entry,
            kind: AssetSource::Utoc,
            mod_name: scratch.name,
            changes: PackageEdits {
                values: vec![edit],
                ..Default::default()
            },
        };
        save_edits(
            &request(bump(&cells[0], 3)),
            Some(&fixture.schema),
            &SaveOptions::default(),
        )
        .expect("first save");
        let second = request(bump(&cells[1], 5));
        let stopped = save_edits(&second, Some(&fixture.schema), &SaveOptions::default())
            .expect("second save");
        assert!(
            matches!(stopped, SaveOutcome::HoldsCopy { .. }),
            "{stopped:?}"
        );
        save_edits(
            &second,
            Some(&fixture.schema),
            &SaveOptions {
                layer: true,
                ..Default::default()
            },
        )
        .expect("layered save");

        let now = row_ints(&read_back(&fixture, &scratch.container()), 2);
        for (was, (is, by)) in cells.iter().zip(now.iter().zip([3, 5])) {
            let (PropertyValue::Int { value: was }, PropertyValue::Int { value: is }) =
                (&was.value, &is.value)
            else {
                unreachable!()
            };
            assert_eq!(*is, was + by, "{}", was);
        }
    }

    /// Several packages saved into one mod go in with a single container rewrite, and a second mod
    /// carrying one of them is named, with which copy the game loads.
    #[test]
    fn a_batch_writes_every_package_and_names_other_mods_holding_them() {
        let Some(fixture) = Fixture::open(DEFAULTS) else {
            return;
        };
        let Some(strings) = Fixture::open(STRINGS) else {
            return;
        };
        let first = ScratchMod {
            root: fixture.root.clone(),
            name: "RivalsToolkitBatchProbeA",
        };
        let second = ScratchMod {
            root: fixture.root.clone(),
            name: "RivalsToolkitBatchProbeB",
        };
        drop(ScratchMod {
            root: fixture.root.clone(),
            name: "RivalsToolkitBatchProbeA",
        });
        drop(ScratchMod {
            root: fixture.root.clone(),
            name: "RivalsToolkitBatchProbeB",
        });
        let cell = row_ints(&fixture.parse(), 1).remove(0);
        let text = field_where(&strings.parse(), "is a stored string", |f| {
            stored(f) && matches!(f.value, PropertyValue::Str { .. })
        });
        let requests = |mod_name: &'static str| {
            vec![
                AssetEditRequest {
                    game_root: &fixture.root,
                    container: &fixture.container,
                    entry: fixture.entry,
                    kind: AssetSource::Utoc,
                    mod_name,
                    changes: PackageEdits {
                        values: vec![bump(&cell, 1)],
                        ..Default::default()
                    },
                },
                AssetEditRequest {
                    game_root: &strings.root,
                    container: &strings.container,
                    entry: strings.entry,
                    kind: AssetSource::Utoc,
                    mod_name,
                    changes: PackageEdits {
                        values: vec![edit_of(
                            &text,
                            EditOp::Set {
                                text: "Probe".into(),
                            },
                        )],
                        ..Default::default()
                    },
                },
            ]
        };
        for outcome in save_batch(
            &requests(first.name),
            Some(&fixture.schema),
            &SaveOptions::default(),
        ) {
            assert!(
                matches!(outcome, Ok(SaveOutcome::Written { .. })),
                "{outcome:?}"
            );
        }
        let held = crate::pak::iostore_out::utoc_entries(&first.container()).expect("list");
        for entry in [fixture.entry, strings.entry] {
            assert!(
                held.contains(&entry.to_lowercase()),
                "{entry} is in the mod"
            );
        }

        let outcomes = save_batch(
            &requests(second.name),
            Some(&fixture.schema),
            &SaveOptions::default(),
        );
        let Some(Ok(SaveOutcome::Written { warnings, .. })) = outcomes.first() else {
            panic!("{outcomes:?}");
        };
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("RivalsToolkitBatchProbeA")),
            "{warnings:?}"
        );
    }

    /// An extracted package opened from disk saves under the path the game ships it at, so the mod
    /// overrides it, and the file it was opened from is left as it was.
    #[test]
    fn a_loose_save_overrides_the_game_path_and_leaves_the_file_alone() {
        let Some(fixture) = Fixture::open(DEFAULTS) else {
            return;
        };
        let scratch = ScratchMod {
            root: fixture.root.clone(),
            name: "RivalsToolkitLooseProbe",
        };
        drop(ScratchMod {
            root: fixture.root.clone(),
            name: "RivalsToolkitLooseProbe",
        });
        let dir = std::env::temp_dir().join(format!("rivals-loose-{}", std::process::id()));
        let disk = dir.join("Extracted/MarvelHeroTable.uasset");
        fs::create_dir_all(disk.parent().expect("parent")).expect("dirs");
        fs::write(&disk, &fixture.loaded.asset_file_buffer).expect("uasset");
        fs::write(
            disk.with_extension("uexp"),
            &fixture.loaded.exports_file_buffer,
        )
        .expect("uexp");
        let on_disk = fs::read(&disk).expect("read back");

        let before = fixture.parse();
        let cell = before.exports[0].data_table.as_ref().expect("table").rows[0]
            .fields
            .iter()
            .find(|field| matches!(field.value, PropertyValue::Int { .. }))
            .expect("an int cell")
            .clone();
        let PropertyValue::Int { value: was } = cell.value else {
            unreachable!()
        };
        let disk_path = disk.to_string_lossy().into_owned();
        let outcome = save_edits(
            &AssetEditRequest {
                game_root: &fixture.root,
                container: "",
                entry: &disk_path,
                kind: AssetSource::Loose,
                mod_name: scratch.name,
                changes: PackageEdits {
                    values: vec![ValueEdit {
                        offset: cell.span.expect("a span").0,
                        expect_name: cell.name.clone(),
                        expect_element: cell.element,
                        expect_kind: "int".into(),
                        op: EditOp::Set {
                            text: (was + 1).to_string(),
                        },
                    }],
                    ..Default::default()
                },
            },
            Some(&fixture.schema),
            &SaveOptions::default(),
        );
        let untouched = fs::read(&disk).expect("read back") == on_disk;
        let _ = fs::remove_dir_all(&dir);
        let outcome = outcome.expect("save");
        assert!(
            matches!(&outcome, SaveOutcome::Written { message, .. } if message.contains(DEFAULTS)),
            "{outcome:?}"
        );
        assert!(
            untouched,
            "the file the package was opened from is left alone"
        );
        assert!(
            crate::pak::iostore_out::utoc_holds_entry(&scratch.container(), DEFAULTS)
                .expect("list"),
            "the mod carries the game path"
        );
    }

    /// Plugin content has no fixed mount to derive a path from, so it is placed by looking its
    /// package up in the game.
    #[test]
    fn a_plugin_package_is_placed_by_the_game() {
        let Ok(root) = std::env::var("RIVALS_GAME_ROOT") else {
            return;
        };
        let package = format!(
            "/MarvelGAS/{}",
            ANIM_BLUEPRINT
                .strip_prefix("Marvel/Plugins/MarvelGAS/Content/")
                .and_then(|rest| rest.strip_suffix(".uasset"))
                .expect("a plugin path")
        );
        assert_eq!(
            asset::game_entry(&root, &package, Path::new("Anywhere.uasset")).expect("placed"),
            ANIM_BLUEPRINT
        );
    }

    /// The save actually lands: an edit written into an IoStore mod reads back out of the `.utoc`
    /// on disk, with the change in it. Everything before this proves the bytes are right in
    /// memory; only reading the container back proves the container is.
    #[test]
    fn an_iostore_save_reads_back_out_of_the_container() {
        let Some(fixture) = Fixture::open(DEFAULTS) else {
            return;
        };
        let scratch = ScratchMod {
            root: fixture.root.clone(),
            name: "RivalsToolkitIoStoreProbe",
        };
        // A stale container from an interrupted run would be reported rather than replaced.
        drop(ScratchMod {
            root: fixture.root.clone(),
            name: "RivalsToolkitIoStoreProbe",
        });

        let before = fixture.parse();
        let cell = before.exports[0].data_table.as_ref().expect("table").rows[0]
            .fields
            .iter()
            .find(|field| matches!(field.value, PropertyValue::Int { .. }))
            .expect("an int cell")
            .clone();
        let (offset, _) = cell.span.expect("a span");
        let was = match cell.value {
            PropertyValue::Int { value } => value,
            _ => unreachable!(),
        };

        let outcome = save_edits(
            &AssetEditRequest {
                game_root: &fixture.root,
                container: &fixture.container,
                entry: fixture.entry,
                kind: AssetSource::Utoc,
                mod_name: scratch.name,
                changes: PackageEdits {
                    values: vec![ValueEdit {
                        offset,
                        expect_name: cell.name.clone(),
                        expect_element: cell.element,
                        expect_kind: "int".into(),
                        op: EditOp::Set {
                            text: (was + 11).to_string(),
                        },
                    }],
                    ..Default::default()
                },
            },
            Some(&fixture.schema),
            &SaveOptions::default(),
        )
        .expect("save");
        assert!(
            matches!(outcome, SaveOutcome::Written { .. }),
            "{outcome:?}"
        );

        let utoc = scratch.container();
        assert!(utoc.is_file(), "{} was written", utoc.display());
        assert!(
            crate::pak::iostore_out::utoc_holds_entry(&utoc, fixture.entry).expect("list"),
            "the container carries the entry"
        );

        let container = utoc.to_string_lossy().into_owned();
        let loaded =
            asset::load_bundle(&fixture.root, &container, fixture.entry, AssetSource::Utoc)
                .expect("read the saved copy back");
        let after = Fixture::parse_bundle(
            &AssetBundle {
                asset: &loaded.asset_file_buffer,
                exports: &loaded.exports_file_buffer,
            },
            &fixture.schema,
            &PackageSource {
                game_root: &fixture.root,
                container: &container,
                entry: fixture.entry,
                kind: AssetSource::Utoc,
            },
        );
        let now = after.exports[0].data_table.as_ref().expect("table").rows[0]
            .fields
            .iter()
            .find(|field| field.name == cell.name)
            .expect("the cell")
            .value
            .clone();
        assert!(
            matches!(now, PropertyValue::Int { value } if value == was + 11),
            "{}",
            now.summary()
        );
        assert_eq!(
            after.exports.len(),
            before.exports.len(),
            "the package is whole"
        );
    }

    /// The whole dump-edit-diff-apply loop on a real table: dump the parse, change one cell and
    /// drop one row in the JSON, diff it back into an edit list, and apply that list. What comes
    /// out has to be the change that was typed and nothing else.
    #[test]
    fn an_edited_dump_diffs_back_into_the_edits_that_make_it() {
        let Some(fixture) = Fixture::open(DEFAULTS) else {
            return;
        };
        let before = fixture.parse();
        let mut dump = serde_json::to_value(&before).expect("dump");

        let dropped;
        {
            let rows = dump["exports"][0]["data_table"]["rows"]
                .as_array_mut()
                .expect("rows");
            let cell = rows[0]["fields"]
                .as_array_mut()
                .expect("fields")
                .iter_mut()
                .find(|field| field["value"]["kind"] == "int")
                .expect("an int cell");
            let was = cell["value"]["value"].as_i64().expect("an int");
            cell["value"]["value"] = serde_json::json!(was + 7);
            dropped = rows.pop().expect("a row to drop")["name"]
                .as_str()
                .expect("a row name")
                .to_string();
        }

        let outcome = crate::asset_edit::diff::diff_dump(&before, &dump).expect("diff");
        assert!(outcome.notes.is_empty(), "{:?}", outcome.notes);
        assert_eq!(outcome.edits.values.len(), 1, "one cell changed");
        assert_eq!(outcome.edits.rows.len(), 1, "one row went");

        let changes = outcome
            .edits
            .resolve(std::path::Path::new("."))
            .expect("resolve");
        let (_, after) = fixture.apply_changes(changes);
        let table = after.exports[0].data_table.as_ref().expect("table");
        assert_eq!(
            table.rows.len(),
            before.exports[0]
                .data_table
                .as_ref()
                .expect("table")
                .rows
                .len()
                - 1
        );
        assert!(
            !table.rows.iter().any(|row| row.name == dropped),
            "{dropped} is gone"
        );

        // Diffing the result against its own dump finds nothing, which is what says the loop is
        // closed rather than merely applying without error.
        let again = serde_json::to_value(&after).expect("dump");
        let settled = crate::asset_edit::diff::diff_dump(&after, &again).expect("diff");
        assert!(settled.edits.is_empty(), "{:?}", settled.edits);
        assert!(settled.notes.is_empty(), "{:?}", settled.notes);
    }

    /// A string table round trips the same way, through its own entry ops rather than value edits.
    #[test]
    fn an_edited_string_table_diffs_back_into_string_edits() {
        let Some(fixture) = Fixture::open(STRING_TABLE) else {
            return;
        };
        let before = fixture.parse();
        let mut dump = serde_json::to_value(&before).expect("dump");
        dump["exports"][0]["string_table"]["entries"][1]["source"] =
            serde_json::json!("Rivals Toolkit");

        let outcome = crate::asset_edit::diff::diff_dump(&before, &dump).expect("diff");
        assert!(outcome.notes.is_empty(), "{:?}", outcome.notes);
        assert_eq!(outcome.edits.strings.len(), 1);

        let changes = outcome
            .edits
            .resolve(std::path::Path::new("."))
            .expect("resolve");
        let (_, after) = fixture.apply_changes(changes);
        let table = after.exports[0].string_table.as_ref().expect("table");
        assert_eq!(table.entries[1].source, "Rivals Toolkit");
        assert_eq!(
            table.entries[1].key,
            before.exports[0]
                .string_table
                .as_ref()
                .expect("table")
                .entries[1]
                .key
        );
    }

    /// A dependency edit changes the order the loader builds the package in and nothing else: the
    /// runs read back as asked, every other export keeps the runs it had, and not one value moves.
    /// The whole table is rewritten by any such edit, so the exports around the edited one are the
    /// real test.
    #[test]
    fn a_dependency_run_is_replaced_and_the_rest_of_the_table_holds() {
        let Some(fixture) = Fixture::open(LEVEL) else {
            return;
        };
        let before = fixture.parse();
        let runs = before.dependencies.as_ref().expect("the runs").clone();
        // An export whose create-before-serialize run has something to drop.
        let target = runs
            .iter()
            .position(|held| held.create_before_serialize.len() > 1)
            .expect("an export with a run to shorten") as u32;
        let mut wanted = runs[target as usize].clone();
        wanted.create_before_serialize.pop();

        let (_, after) = fixture.apply_changes(PackageEdits {
            dependencies: vec![rivals_uasset::DependencyEdit {
                export: target,
                runs: wanted.clone(),
            }],
            ..Default::default()
        });

        let now = after.dependencies.as_ref().expect("the runs");
        assert_eq!(now.len(), runs.len());
        assert_eq!(now[target as usize], wanted);
        for (at, (was, is)) in runs.iter().zip(now).enumerate() {
            if at as u32 == target {
                continue;
            }
            assert_eq!(was, is, "export {at}'s runs moved");
        }
        assert_eq!(after.exports.len(), before.exports.len());
        for (was, is) in before.exports.iter().zip(&after.exports) {
            assert_eq!(was.path, is.path);
            assert_eq!(was.properties.len(), is.properties.len());
        }
    }

    /// The runs make a graph the loader walks, and a cycle in it is a package that loads nowhere.
    /// It is refused before anything is patched rather than left for the converter to hit.
    #[test]
    fn a_dependency_cycle_is_refused() {
        let Some(fixture) = Fixture::open(LEVEL) else {
            return;
        };
        let parsed = fixture.parse();
        let bundle = fixture.bundle();
        let header = rivals_uasset::read_header(&bundle).expect("header");
        // Two exports each waiting on the other's creation cannot both be built.
        let plan = rivals_uasset::plan_dependency_edits(
            &parsed,
            &header,
            &[
                rivals_uasset::DependencyEdit {
                    export: 0,
                    runs: rivals_uasset::Runs {
                        create_before_create: vec![2],
                        ..Default::default()
                    },
                },
                rivals_uasset::DependencyEdit {
                    export: 1,
                    runs: rivals_uasset::Runs {
                        create_before_create: vec![1],
                        ..Default::default()
                    },
                },
            ],
        )
        .expect("plan");
        assert!(plan.cycle.is_some(), "{plan:?}");
        assert!(
            plan.blockers.iter().any(|b| b.contains("cycle")),
            "{:?}",
            plan.blockers
        );
    }

    /// An index that names nothing in this package, or the export itself, is refused: the loader
    /// would wait on an object that never arrives.
    #[test]
    fn a_run_naming_nothing_or_itself_is_refused() {
        let Some(fixture) = Fixture::open(LEVEL) else {
            return;
        };
        let parsed = fixture.parse();
        let bundle = fixture.bundle();
        let header = rivals_uasset::read_header(&bundle).expect("header");
        let past_the_end = parsed.exports.len() as i32 + 5;
        let plan = rivals_uasset::plan_dependency_edits(
            &parsed,
            &header,
            &[rivals_uasset::DependencyEdit {
                export: 3,
                runs: rivals_uasset::Runs {
                    create_before_serialize: vec![past_the_end, 4, 0],
                    ..Default::default()
                },
            }],
        )
        .expect("plan");
        let joined = plan.blockers.join(" | ");
        assert!(joined.contains("does not have"), "{joined}");
        assert!(joined.contains("wait for itself"), "{joined}");
        assert!(joined.contains("null"), "{joined}");
    }

    /// A cross-package copy is three rewrites at once: names against the destination's map,
    /// indices inside the closure at where they land, and indices outside it through fresh
    /// imports. The copy has to read back with every property the original had and every
    /// reference following it rather than the source.
    #[test]
    fn an_export_and_its_subobject_copy_into_another_package() {
        let Some(fixture) = Fixture::open(LEVEL) else {
            return;
        };
        let source = CopyFrom {
            container: fixture.container.clone(),
            entry: STALE_CLASS_MAP.to_string(),
        };
        let schema = mappings::load(std::path::Path::new(
            &std::env::var("RIVALS_USMAP").expect("usmap"),
        ))
        .expect("mappings");
        let from = asset::load_bundle(
            &fixture.root,
            &fixture.container,
            STALE_CLASS_MAP,
            AssetSource::Utoc,
        )
        .expect("load the source");
        let from_parsed = Fixture::parse_bundle(
            &AssetBundle {
                asset: &from.asset_file_buffer,
                exports: &from.exports_file_buffer,
            },
            &schema,
            &PackageSource {
                game_root: &fixture.root,
                container: &fixture.container,
                entry: STALE_CLASS_MAP,
                kind: AssetSource::Utoc,
            },
        );
        // An actor that owns a component, both reading to their end.
        let Some(actor) = from_parsed
            .exports
            .iter()
            .find(|export| {
                export.class_name == "PointLight"
                    && matches!(export.status, ExportStatus::Complete)
                    && from_parsed.exports.iter().any(|held| {
                        held.outer_index == export.index as i32 + 1
                            && matches!(held.status, ExportStatus::Complete)
                    })
            })
            .map(|export| export.index)
        else {
            return;
        };

        let before = fixture.parse();
        let level = before
            .exports
            .iter()
            .find(|export| export.class_name == "Level")
            .expect("a level")
            .index;
        let request = CopyRequest {
            game_root: &fixture.root,
            container: &fixture.container,
            entry: fixture.entry,
            kind: AssetSource::Utoc,
            mod_name: "unused, preview writes nothing",
            sources: vec![source.clone()],
            copies: vec![rivals_uasset::CopyExport {
                from: source.key(),
                export: actor,
                into_outer: Some(level),
                name: "ProbeLightActor".to_string(),
                into_level: None,
            }],
        };
        let (patched, _) = preview_copy(&request, Some(&schema)).expect("copy");
        let after = Fixture::parse_bundle(
            &AssetBundle {
                asset: &patched.asset,
                exports: &patched.exports,
            },
            &schema,
            &fixture.source(),
        );

        assert_eq!(
            after.exports.len(),
            before.exports.len() + 2,
            "actor and component"
        );
        let copy = &after.exports[before.exports.len()];
        let child = &after.exports[before.exports.len() + 1];
        let original = &from_parsed.exports[actor as usize];
        assert_eq!(copy.object_name, "ProbeLightActor");
        assert_eq!(copy.class_name, original.class_name);
        assert!(
            matches!(copy.status, ExportStatus::Complete),
            "{:?}",
            copy.status
        );
        assert_eq!(
            copy.outer_index,
            level as i32 + 1,
            "it lands under the level"
        );
        assert_eq!(
            child.outer_index,
            copy.index as i32 + 1,
            "the component follows its owner rather than the source's"
        );
        assert!(
            child.path.ends_with("ProbeLightActor:LightComponent0"),
            "{}",
            child.path
        );

        // With no outer named, the copy sits at the package root rather than under export 0.
        let to_root = CopyRequest {
            copies: vec![rivals_uasset::CopyExport {
                into_outer: None,
                ..request.copies[0].clone()
            }],
            sources: vec![source.clone()],
            ..request
        };
        let (rooted, _) = preview_copy(&to_root, Some(&schema)).expect("copy to the root");
        let rooted = Fixture::parse_bundle(
            &AssetBundle {
                asset: &rooted.asset,
                exports: &rooted.exports,
            },
            &schema,
            &fixture.source(),
        );
        let at_root = &rooted.exports[before.exports.len()];
        assert_eq!(at_root.outer_index, 0, "it lands at the root");
        assert_eq!(
            at_root.path,
            format!("{}.ProbeLightActor", before.info.package_name)
        );

        // Every property reads the same, and the references inside point at the copies.
        let names = |export: &rivals_uasset::ParsedExport| -> Vec<String> {
            export
                .properties
                .iter()
                .map(|entry| entry.label())
                .collect()
        };
        assert_eq!(names(original), names(copy));
        for entry in &copy.properties {
            if let PropertyValue::Object {
                path: Some(path), ..
            } = &entry.value
            {
                assert!(
                    !path.contains("Lobby_2014001408"),
                    "{} still points into the source: {path}",
                    entry.label()
                );
            }
        }

        // Nothing the destination already held moved.
        for (was, is) in before.exports.iter().zip(&after.exports) {
            assert_eq!(was.path, is.path);
            assert_eq!(was.class_name, is.class_name);
        }
    }

    /// A copy that would land on a name the destination already uses is refused, since the two
    /// objects would share a path and the loader would find whichever came first.
    #[test]
    fn a_copy_onto_a_taken_name_is_refused() {
        let Some(fixture) = Fixture::open(LEVEL) else {
            return;
        };
        let schema = mappings::load(std::path::Path::new(
            &std::env::var("RIVALS_USMAP").expect("usmap"),
        ))
        .expect("mappings");
        let source = CopyFrom {
            container: fixture.container.clone(),
            entry: STALE_CLASS_MAP.to_string(),
        };
        let before = fixture.parse();
        let level = before
            .exports
            .iter()
            .find(|export| export.class_name == "Level")
            .expect("a level")
            .index;
        let taken = before
            .exports
            .iter()
            .find(|export| export.outer_index == level as i32 + 1)
            .expect("something under the level")
            .object_name
            .clone();

        let request = CopyRequest {
            game_root: &fixture.root,
            container: &fixture.container,
            entry: fixture.entry,
            kind: AssetSource::Utoc,
            mod_name: "unused",
            sources: vec![source.clone()],
            copies: vec![rivals_uasset::CopyExport {
                from: source.key(),
                export: 0,
                into_outer: Some(level),
                name: taken.clone(),
                into_level: None,
            }],
        };
        let plan = plan_copy(&request, Some(&schema)).expect("plan");
        assert!(
            plan.blockers
                .iter()
                .any(|b| b.contains(&format!("called {taken}"))),
            "{:?}",
            plan.blockers
        );
    }

    /// A level writes its actor list, the URL it was cooked from, its model and components, and
    /// its script actor after its properties. With those read, only the precomputed lighting data
    /// is left unaccounted for, and every actor it names resolves to an export it owns.
    #[test]
    fn a_level_reads_its_actors_url_and_components() {
        let Some(fixture) = Fixture::open(LEVEL) else {
            return;
        };
        let parsed = fixture.parse();
        let level = parsed
            .exports
            .iter()
            .find(|export| export.class_name == "Level")
            .expect("a level");
        assert!(
            matches!(
                level.status,
                ExportStatus::Payload {
                    kind: "level data",
                    payload_bytes: 105,
                    ..
                }
            ),
            "{:?}",
            level.status
        );

        // The class schema declares slots of the same names, and the tail's own entries come
        // after them, so the tail's is the last match rather than the first.
        let field = |name: &str| {
            level
                .properties
                .iter()
                .rev()
                .find(|entry| entry.name == name)
                .unwrap_or_else(|| panic!("{name} was not read"))
        };
        let PropertyValue::Array { items } = &field("Actors").value else {
            panic!("the actor list is not an array");
        };
        assert_eq!(items.len(), 15);
        let mut named = 0;
        for item in items {
            let PropertyValue::Object { index, .. } = item else {
                panic!("an actor is not a reference");
            };
            if *index == 0 {
                continue;
            }
            named += 1;
            let actor = &parsed.exports[(*index - 1) as usize];
            assert_eq!(
                actor.outer_index,
                level.index as i32 + 1,
                "{} is an actor of this level",
                actor.path
            );
        }
        assert_eq!(named, 13, "two of the entries are null");

        let PropertyValue::Struct { fields, .. } = &field("URL").value else {
            panic!("the URL is not a struct");
        };
        let url = |name: &str| {
            fields
                .iter()
                .find(|entry| entry.name == name)
                .map(|entry| entry.value.summary())
                .unwrap_or_default()
        };
        assert_eq!(url("Protocol"), "unreal");
        assert_eq!(url("Map"), "/Game/MarvelDemo/Maps/Feature/ClientEntry");
        assert_eq!(url("Port"), "7777");

        let PropertyValue::Array { items } = &field("ModelComponents").value else {
            panic!("the component list is not an array");
        };
        assert_eq!(items.len(), 6);
        assert!(field("Model").value.summary().contains("Model_0"));
        assert!(
            field("LevelScriptActor")
                .value
                .summary()
                .contains("SimpleSelectHeroLevel_C_1")
        );
        assert_eq!(field("NavListStart").value.summary(), "None");
        assert_eq!(field("NavListEnd").value.summary(), "None");
    }

    /// An actor the level does not name is loaded and never spawned, so a copy asked into the
    /// level is appended to its actor list, the list's count moves with it, and the level still
    /// reads as a level with its precomputed data where it was.
    #[test]
    fn a_duplicated_actor_is_listed_in_its_level() {
        let Some(fixture) = Fixture::open(LEVEL) else {
            return;
        };
        let before = fixture.parse();
        let level = before
            .exports
            .iter()
            .find(|export| export.class_name == "Level")
            .expect("a level")
            .index;
        let actor = before
            .exports
            .iter()
            .find(|export| {
                export.class_name == "StaticMeshActor"
                    && export.outer_index == level as i32 + 1
                    && matches!(export.status, ExportStatus::Complete)
            })
            .expect("a static mesh actor in the level")
            .index;

        let (_, after) = fixture.apply_changes(PackageEdits {
            duplicate_exports: vec![DuplicateExport {
                export: actor,
                name: "ProbeMeshActor".to_string(),
                into_level: Some(level),
            }],
            ..Default::default()
        });

        let listed = |parsed: &rivals_uasset::ParsedPackage| -> Vec<String> {
            let entry = parsed.exports[level as usize]
                .properties
                .iter()
                .find(|entry| entry.name == "Actors")
                .expect("the actor list");
            match &entry.value {
                PropertyValue::Array { items } => items.iter().map(|item| item.summary()).collect(),
                other => panic!("the actor list reads as {}", other.summary()),
            }
        };
        let was = listed(&before);
        let is = listed(&after);
        assert_eq!(is.len(), was.len() + 1);
        assert_eq!(&is[..was.len()], &was[..], "nothing already listed moved");
        assert!(
            is[was.len()].ends_with(":ProbeMeshActor"),
            "{}",
            is[was.len()]
        );

        // The list grew by four bytes and the precomputed data after it is untouched, which is
        // what says the splice landed inside the tail rather than over it.
        let (
            ExportStatus::Payload {
                consumed: was_consumed,
                payload_bytes: was_payload,
                ..
            },
            ExportStatus::Payload {
                consumed: is_consumed,
                payload_bytes: is_payload,
                ..
            },
        ) = (
            &before.exports[level as usize].status,
            &after.exports[level as usize].status,
        )
        else {
            panic!("the level stopped reading as level data");
        };
        assert_eq!(*is_consumed, was_consumed + 4);
        assert_eq!(is_payload, was_payload);

        // The component came along, and the copy owns it rather than the original.
        let copy = after
            .exports
            .iter()
            .find(|export| export.object_name == "ProbeMeshActor")
            .expect("the copy");
        assert!(
            after
                .exports
                .iter()
                .any(|export| export.outer_index == copy.index as i32 + 1),
            "the copy owns its component"
        );
    }

    /// Only a level has an actor list, and it lists only actors it owns. Both refusals happen
    /// before anything is patched.
    #[test]
    fn a_copy_is_refused_into_something_that_is_not_its_level() {
        let Some(fixture) = Fixture::open(LEVEL) else {
            return;
        };
        let parsed = fixture.parse();
        let level = parsed
            .exports
            .iter()
            .find(|export| export.class_name == "Level")
            .expect("a level")
            .index;
        let world = parsed
            .exports
            .iter()
            .find(|export| export.class_name == "World")
            .expect("a world")
            .index;
        let actor = parsed
            .exports
            .iter()
            .find(|export| {
                export.class_name == "StaticMeshActor" && export.outer_index == level as i32 + 1
            })
            .expect("an actor")
            .index;
        // A component sits under its actor, not under the level, so it is exactly the kind of
        // export the level must refuse to list.
        let outside = parsed
            .exports
            .iter()
            .find(|export| {
                export.outer_index == actor as i32 + 1
                    && matches!(export.status, ExportStatus::Complete)
            })
            .expect("a component of the actor")
            .index;

        let plan = |export: u32, into: u32| {
            rivals_uasset::plan_duplication(
                &parsed,
                &[DuplicateExport {
                    export,
                    name: "ProbeCopy".to_string(),
                    into_level: Some(into),
                }],
            )
        };
        let err = plan(actor, world).expect_err("the world has no actor list");
        assert!(err.contains("not a Level"), "{err}");
        let err = plan(outside, level).expect_err("an outsider is not this level's actor");
        assert!(err.contains("does not sit in"), "{err}");
    }

    /// A rename moves the export and every path that names it, and nothing else in the package
    /// notices. The one property pointing at the renamed component reads the new path, which is
    /// the difference verification has to excuse rather than refuse.
    #[test]
    fn an_export_rename_moves_it_and_the_values_that_name_it() {
        let Some(fixture) = Fixture::open(LEVEL) else {
            return;
        };
        let before = fixture.parse();
        let target = before
            .exports
            .iter()
            .position(|export| export.object_name == "BodySetup_0")
            .expect("a body setup to rename") as u32;
        let was = before.exports[target as usize].path.clone();
        let (_, after) = fixture.apply_changes(PackageEdits {
            exports: vec![ExportEdit::Rename {
                export: target,
                name: "ProbeRenamed".to_string(),
            }],
            ..Default::default()
        });

        assert_eq!(after.exports.len(), before.exports.len());
        let now = &after.exports[target as usize];
        assert_eq!(now.object_name, "ProbeRenamed");
        let moved = was.replace("BodySetup_0", "ProbeRenamed");
        assert_eq!(now.path, moved);
        for (old, new) in before.exports.iter().zip(&after.exports) {
            if old.index == target {
                continue;
            }
            assert_eq!(old.path, new.path, "only the renamed export moves");
            assert_eq!(old.class_name, new.class_name);
        }
        let names_it = |parsed: &rivals_uasset::ParsedPackage, path: &str| {
            parsed
                .exports
                .iter()
                .flat_map(|export| export.properties.iter())
                .filter(|entry| {
                    matches!(&entry.value, PropertyValue::Object { path: Some(at), .. } if at == path)
                })
                .count()
        };
        assert_eq!(
            names_it(&before, &was),
            names_it(&after, &moved),
            "the values naming it follow it"
        );
        assert_eq!(
            names_it(&after, &was),
            0,
            "nothing still reads the old path"
        );
    }

    /// A retype swaps the class and empties the property block, so the object reads back as the
    /// new class with nothing stored. The bytes past the block are the class chain's, and this
    /// pair writes the same ones, which is what makes the retype safe at all.
    #[test]
    fn an_export_is_retyped_and_reads_back_empty_under_its_new_class() {
        let Some(fixture) = Fixture::open(LEVEL) else {
            return;
        };
        let before = fixture.parse();
        let target = before
            .exports
            .iter()
            .find(|export| export.class_name == "SkyLight")
            .expect("a sky light")
            .index;
        let class = before
            .imports
            .iter()
            .find(|import| import.object_name == "DirectionalLight")
            .expect("the directional light class")
            .index;

        let (_, after) = fixture.apply_changes(PackageEdits {
            exports: vec![ExportEdit::SetClass {
                export: target,
                class,
            }],
            reset_exports: vec![target],
            ..Default::default()
        });

        let now = &after.exports[target as usize];
        assert_eq!(now.class_name, "DirectionalLight");
        assert_eq!(now.class_index, class);
        assert!(
            matches!(now.status, ExportStatus::Complete),
            "{:?}",
            now.status
        );
        assert!(
            !now.properties
                .iter()
                .any(|entry| !matches!(entry.value, PropertyValue::Unset { .. })),
            "nothing is stored after a retype"
        );
        assert_eq!(after.exports.len(), before.exports.len());
        for (was, is) in before.exports.iter().zip(&after.exports) {
            if was.index == target {
                continue;
            }
            assert_eq!(was.class_name, is.class_name, "{} kept its class", was.path);
        }
    }

    /// A retype between classes whose chains write different things is refused: the bytes already
    /// past the property block stay there, and the new class would read them as its own.
    #[test]
    fn a_retype_across_different_tails_is_refused() {
        let Some(fixture) = Fixture::open(LEVEL) else {
            return;
        };
        let parsed = fixture.parse();
        let actor = parsed
            .exports
            .iter()
            .find(|export| export.class_name == "SkyLight")
            .expect("a sky light")
            .index;
        // A component writes a construction-script record where an actor writes a label.
        let component = parsed
            .imports
            .iter()
            .find(|import| import.object_name == "BrushComponent")
            .expect("a component class")
            .index;
        let plan = rivals_uasset::plan_export_edits_with(
            &parsed,
            &[ExportEdit::SetClass {
                export: actor,
                class: component,
            }],
            Some(&fixture.schema),
            &[actor],
        )
        .expect("plan");
        assert!(
            plan.blockers
                .iter()
                .any(|b| b.contains("would not read back")),
            "{:?}",
            plan.blockers
        );
    }

    /// A retype that does not empty the export is refused: the stored values were written under a
    /// schema that no longer applies, and reading them back under the new one is nonsense.
    #[test]
    fn a_retype_without_a_reset_is_refused() {
        let Some(fixture) = Fixture::open(LEVEL) else {
            return;
        };
        let parsed = fixture.parse();
        let target = parsed
            .exports
            .iter()
            .find(|export| export.class_name == "SkyLight")
            .expect("a sky light")
            .index;
        let class = parsed
            .imports
            .iter()
            .find(|import| import.object_name == "DirectionalLight")
            .expect("the class")
            .index;
        let plan = rivals_uasset::plan_export_edits_with(
            &parsed,
            &[ExportEdit::SetClass {
                export: target,
                class,
            }],
            Some(&fixture.schema),
            &[],
        )
        .expect("plan");
        assert!(
            plan.blockers.iter().any(|b| b.contains("reset it")),
            "{:?}",
            plan.blockers
        );
    }

    /// Reparenting rewrites the index in two places: the export table row and the layout the
    /// reader walks. Both have to read back, or the class and its definition disagree.
    ///
    /// The target is a function rather than a class because a class is refused: its default object
    /// stores values laid out for the old chain. A function is the case the rule still allows,
    /// since nothing in the package is an instance of one.
    #[test]
    fn a_type_nothing_instantiates_is_reparented_in_the_table_and_in_its_layout() {
        let Some(fixture) = Fixture::open(LEVEL) else {
            return;
        };
        let before = fixture.parse();
        let stores_values = |class: u32| {
            before.exports.iter().any(|held| {
                held.class_index == class as i32 + 1
                    && held
                        .properties
                        .iter()
                        .any(|entry| !matches!(entry.value, PropertyValue::Unset { .. }))
            })
        };
        let functions: Vec<u32> = before
            .exports
            .iter()
            .filter(|export| {
                export.class_name == "Function"
                    && export.super_struct_at.is_some()
                    && !stores_values(export.index)
            })
            .map(|export| export.index)
            .collect();
        let (Some(&target), Some(&parent)) = (functions.first(), functions.last()) else {
            return;
        };
        if target == parent {
            return;
        }
        let wanted = parent as i32 + 1;

        let (_, after) = fixture.apply_changes(PackageEdits {
            exports: vec![ExportEdit::SetSuper {
                export: target,
                super_index: wanted,
            }],
            ..Default::default()
        });

        let now = &after.exports[target as usize];
        assert_eq!(now.super_index, wanted, "the table row moved");
        assert_eq!(
            now.super_struct_at, before.exports[target as usize].super_struct_at,
            "the layout is the same shape, so the index was spliced rather than the block rewritten"
        );
        assert_eq!(
            status_name_of(now),
            status_name_of(&before.exports[target as usize]),
            "it reads the same way it did"
        );
        assert_eq!(after.exports.len(), before.exports.len());
    }

    fn status_name_of(export: &rivals_uasset::ParsedExport) -> &'static str {
        match export.status {
            ExportStatus::Complete => "complete",
            ExportStatus::Payload { .. } => "payload",
            ExportStatus::Partial { .. } => "partial",
            ExportStatus::Failed { .. } => "failed",
        }
    }

    /// Reparenting a class whose instances store values is refused, because their bytes are laid
    /// out for the flattened chain they were written under and the new chain reads them at
    /// different widths. A class always has one such instance: its own default object.
    ///
    /// This one crashed the game on 2026-09-08 with `Bad export index 459025/8` while serializing
    /// the CDO, and the offline verification did not catch it: re-reading resolves the class
    /// through the mappings file by name, so it kept using the original chain and saw no change.
    #[test]
    fn reparenting_a_class_whose_default_object_stores_values_is_refused() {
        let Some(fixture) = Fixture::open(HOOP_BLUEPRINT) else {
            return;
        };
        let parsed = fixture.parse();
        let class = parsed
            .exports
            .iter()
            .position(|export| export.class_name == "BlueprintGeneratedClass")
            .expect("a blueprint class") as u32;
        let cdo = parsed
            .exports
            .iter()
            .find(|export| export.class_index == class as i32 + 1)
            .expect("the class default object");
        assert!(
            cdo.properties
                .iter()
                .any(|entry| !matches!(entry.value, PropertyValue::Unset { .. })),
            "the default object stores values, which is what makes this unsafe"
        );
        let parent = parsed
            .imports
            .iter()
            .find(|import| {
                import.class_name == "BlueprintGeneratedClass"
                    && import.path.contains("EffectActor")
            })
            .expect("another blueprint class to parent it to")
            .index;

        let plan = rivals_uasset::plan_export_edits(
            &parsed,
            &[ExportEdit::SetSuper {
                export: class,
                super_index: parent,
            }],
            Some(&fixture.schema),
        )
        .expect("plan");
        assert!(
            plan.blockers
                .iter()
                .any(|b| b.contains("flattens to a different property list")),
            "{:?}",
            plan.blockers
        );

        // Emptying the default object first is not a way through either. That was tried in game
        // and crashed on the component wiring that was no longer there, so the refusal stands whatever
        // else the same save does.
        let plan = rivals_uasset::plan_export_edits_with(
            &parsed,
            &[ExportEdit::SetSuper {
                export: class,
                super_index: parent,
            }],
            Some(&fixture.schema),
            &[cdo.index],
        )
        .expect("plan");
        assert!(
            plan.blockers
                .iter()
                .any(|b| b.contains("flattens to a different property list")),
            "resetting the instance does not unlock it: {:?}",
            plan.blockers
        );
    }

    /// Only a class, struct or enum has a parent, so anything else is refused before it is tried.
    #[test]
    fn reparenting_something_that_is_not_a_type_is_refused() {
        let Some(fixture) = Fixture::open(LEVEL) else {
            return;
        };
        let parsed = fixture.parse();
        let actor = parsed
            .exports
            .iter()
            .find(|export| export.class_name == "SkyLight")
            .expect("a sky light")
            .index;
        let plan = rivals_uasset::plan_export_edits(
            &parsed,
            &[ExportEdit::SetSuper {
                export: actor,
                super_index: -1,
            }],
            Some(&fixture.schema),
        )
        .expect("plan");
        assert!(
            plan.blockers
                .iter()
                .any(|b| b.contains("no parent to change")),
            "{:?}",
            plan.blockers
        );
    }

    /// An import nothing names comes out of the table, every import above it moves down a place,
    /// and each one still resolves to the object it did before.
    #[test]
    fn an_unused_import_is_removed_and_the_rest_keep_their_targets() {
        let Some(fixture) = Fixture::open(LEVEL) else {
            return;
        };
        let before = fixture.parse();
        let bundle = fixture.bundle();
        let header = rivals_uasset::read_header(&bundle).expect("header");
        let Some(spare) = rivals_uasset::unused_imports(&before, &header)
            .into_iter()
            .find(|import| import.blocked.is_none())
        else {
            return;
        };
        let dropped = (-spare.index - 1) as u32;
        let kept: Vec<String> = before
            .imports
            .iter()
            .filter(|import| import.index != spare.index)
            .map(|import| import.path.clone())
            .collect();

        let (_, after) = fixture.apply_changes(PackageEdits {
            imports: vec![ImportEdit::Remove { import: dropped }],
            ..Default::default()
        });

        assert_eq!(after.imports.len(), kept.len());
        for (was, is) in kept.iter().zip(&after.imports) {
            assert_eq!(was, &is.path, "import {} kept its target", is.index);
        }
        assert!(
            !after.imports.iter().any(|import| import.path == spare.path),
            "{} is gone",
            spare.path
        );
        assert_eq!(after.exports.len(), before.exports.len());
        for (was, is) in before.exports.iter().zip(&after.exports) {
            assert_eq!(was.path, is.path);
            assert_eq!(was.class_name, is.class_name);
        }
    }

    #[test]
    fn a_string_table_reads_its_entries_and_where_they_sit() {
        let Some(fixture) = Fixture::open(STRING_TABLE) else {
            return;
        };
        let parsed = fixture.parse();
        let export = &parsed.exports[0];
        assert!(
            matches!(export.status, ExportStatus::Complete),
            "{:?}",
            export.status
        );
        let table = export.string_table.as_ref().expect("string table");
        assert_eq!(table.namespace, "104_Currency_ST");
        assert!(!table.entries.is_empty());
        assert_eq!(table.entries[0].key, "MarvelCurrencyTable_1_Description");
        assert!(!table.entries[0].source.is_empty());
        let layout = parsed
            .string_tables
            .iter()
            .find(|layout| layout.export == 0)
            .expect("layout");
        // The count the table ships is scenery; that the layout accounts for every entry, end to
        // end and up to the trailer, is the invariant.
        assert_eq!(layout.entries.len(), table.entries.len());
        assert!(
            layout
                .entries
                .windows(2)
                .all(|pair| pair[0].end == pair[1].key.0)
        );
        assert_eq!(
            layout.entries.last().expect("an entry").end,
            layout.trailer_at
        );
    }

    /// A Blueprint struct's export reads to its end, default instance included, and hands back the
    /// field records that other packages need to decode rows of it.
    #[test]
    fn a_blueprint_struct_reads_exactly_and_keeps_its_definition() {
        let Some(fixture) = Fixture::open(STRUCT) else {
            return;
        };
        let parsed = fixture.parse();
        let export = &parsed.exports[0];
        assert!(
            matches!(export.status, ExportStatus::Complete),
            "{:?}",
            export.status
        );
        let definition = export.struct_definition.as_ref().expect("definition");
        assert_eq!(definition.name, "CardData");
        assert_eq!(definition.properties.len(), 5);
        assert!(
            definition
                .properties
                .iter()
                .any(|property| property.name.starts_with("CardID_64"))
        );
    }

    const NIAGARA: &str =
        "Marvel/Plugins/MarvelGAS/Content/Marvel/UI/1027/Particle/NS_ActivityHUDCom_102771.uasset";

    /// Niagara variables serialize themselves around a schema-driven type block; with that read,
    /// every script, renderer and the system itself decode instead of failing on the first store.
    #[test]
    fn niagara_scripts_and_renderers_read_exactly() {
        let Some(fixture) = Fixture::open(NIAGARA) else {
            return;
        };
        let parsed = fixture.parse();
        let failed: Vec<&str> = parsed
            .exports
            .iter()
            .filter(|export| {
                matches!(
                    export.status,
                    ExportStatus::Failed { .. } | ExportStatus::Partial { .. }
                )
            })
            .map(|export| export.object_name.as_str())
            .collect();
        assert!(failed.is_empty(), "{failed:?}");
        let scripts = parsed
            .exports
            .iter()
            .filter(|export| export.class_name == "NiagaraScript")
            .count();
        assert!(scripts >= 4, "the emitter and system scripts are there");
        assert!(
            parsed
                .exports
                .iter()
                .all(|export| export.class_name != "NiagaraScript"
                    || matches!(export.status, ExportStatus::Complete)),
            "scripts decode to their end once the parameter stores read"
        );
        let renderer = parsed
            .exports
            .iter()
            .find(|export| export.class_name == "NiagaraSpriteRendererProperties")
            .expect("the sprite renderer");
        assert!(matches!(renderer.status, ExportStatus::Complete));
        // Two of the binding's declared fields are stored; the root variable is a composite whose
        // raw value bytes read as a container.
        let root = nested(
            &renderer.properties,
            &["RendererEnabledBinding", "RootVariable", "VarData"],
        );
        assert!(
            matches!(&root.value, PropertyValue::Array { .. }),
            "{}",
            root.value.summary()
        );
        let binding = nested(&renderer.properties, &["RendererEnabledBinding"]);
        let PropertyValue::Struct { fields, .. } = &binding.value else {
            panic!("expected a struct, got {}", binding.value.summary());
        };
        assert_eq!(fields.iter().filter(|field| !unset(field)).count(), 2);
    }

    /// The pattern export the class default object points at through `RootShakePattern`.
    fn pattern_export(parsed: &rivals_uasset::ParsedPackage) -> u32 {
        parsed
            .exports
            .iter()
            .position(|export| export.object_name == "RootShakePattern")
            .expect("the pattern export") as u32
    }

    /// Removing the last export: the CDO's reference to it reads None, the class export's bytes
    /// stay where they were, and the plan names the class layout it cannot follow.
    #[test]
    fn a_tail_export_can_be_removed_and_the_reference_to_it_cleared() {
        let Some(fixture) = Fixture::open(SHAKE) else {
            return;
        };
        let before = fixture.parse();
        let pattern = pattern_export(&before);
        assert_eq!(pattern as usize, before.exports.len() - 1);
        let cdo = cdo(&before);
        let reference = nested(&before.exports[cdo].properties, &["RootShakePattern"]);
        assert!(
            matches!(reference.value, PropertyValue::Object { index, .. } if index == pattern as i32 + 1),
            "{}",
            reference.value.summary()
        );

        let plan = rivals_uasset::plan_removal(&before, &[pattern]).expect("plan");
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(plan.indices(), vec![pattern]);
        assert_eq!(plan.renumbered, 0);
        assert_eq!(plan.cleared.len(), 1, "{:?}", plan.cleared);
        assert_eq!(plan.cleared[0].property, "RootShakePattern");
        // The class export's layout is walked, so nothing undecoded is left pointing at the
        // pattern; the one warning is that other packages may import it.
        assert_eq!(plan.warnings.len(), 1, "{:?}", plan.warnings);
        assert!(
            plan.warnings[0].contains("imported by other packages"),
            "{}",
            plan.warnings[0]
        );

        let (patched, after) = fixture.apply_changes(PackageEdits {
            remove_exports: vec![pattern],
            ..Default::default()
        });
        assert_eq!(after.exports.len(), before.exports.len() - 1);
        let now = nested(&after.exports[cdo].properties, &["RootShakePattern"]);
        assert!(
            matches!(now.value, PropertyValue::Object { index: 0, .. }),
            "{}",
            now.value.summary()
        );
        let removed = &before.exports[pattern as usize];
        assert_eq!(
            patched.exports.len() as i64,
            fixture.loaded.exports_file_buffer.len() as i64 - removed.serial_size
        );
        // Every other export sits before the removed one, so its bytes are exactly where they were.
        let base = rivals_uasset::header_size(&fixture.bundle()).expect("header size") as usize;
        let class = &before.exports[0];
        let range = class.serial_offset as usize - base
            ..(class.serial_offset + class.serial_size) as usize - base;
        assert_eq!(
            patched.exports[range.clone()],
            fixture.loaded.exports_file_buffer[range]
        );
        assert!(matches!(after.exports[0].status, ExportStatus::Complete));
    }

    /// The pattern is a subobject of the default object, so removing the object takes it along.
    #[test]
    fn removing_an_outer_takes_its_subobjects_with_it() {
        let Some(fixture) = Fixture::open(SHAKE) else {
            return;
        };
        let before = fixture.parse();
        let pattern = pattern_export(&before);
        let cdo = cdo(&before) as u32;
        assert_eq!(before.exports[pattern as usize].outer_index, cdo as i32 + 1);
        let plan = rivals_uasset::plan_removal(&before, &[cdo]).expect("plan");
        assert_eq!(plan.indices(), vec![cdo, pattern]);
        assert!(plan.removed[0].requested);
        assert!(!plan.removed[1].requested);
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
    }

    /// A reset leaves every declared slot inherited and touches nothing outside the export.
    #[test]
    fn resetting_the_default_object_drops_every_stored_value() {
        let Some(fixture) = Fixture::open(SHAKE) else {
            return;
        };
        let before = fixture.parse();
        let cdo = cdo(&before);
        let stored = before.exports[cdo]
            .properties
            .iter()
            .filter(|entry| !unset(entry))
            .count();
        assert!(stored > 0);
        let (patched, after) = fixture.apply_changes(PackageEdits {
            reset_exports: vec![cdo as u32],
            ..Default::default()
        });
        assert_eq!(after.exports.len(), before.exports.len());
        assert_eq!(
            after.exports[cdo].properties.len(),
            before.exports[cdo].properties.len(),
            "every declared slot is still listed"
        );
        assert!(after.exports[cdo].properties.iter().all(unset));
        assert!(matches!(after.exports[cdo].status, ExportStatus::Complete));
        assert!(patched.exports.len() < fixture.loaded.exports_file_buffer.len());
        assert_eq!(patched.applied.len(), 1);
        assert!(
            patched.applied[0]
                .before
                .starts_with(&format!("{stored} stored")),
            "{}",
            patched.applied[0].before
        );
    }

    /// The class default object's export, which is where a Blueprint keeps its tuning values.
    fn cdo(parsed: &rivals_uasset::ParsedPackage) -> usize {
        parsed
            .exports
            .iter()
            .position(|e| e.object_name.starts_with("Default__"))
            .expect("a class default object")
    }

    fn unset(field: &PropertyEntry) -> bool {
        matches!(field.value, PropertyValue::Unset { .. })
    }

    /// A slot the header skips has no bytes and no header item. Giving it a value inserts both,
    /// and everything the export already stored must still read the same.
    #[test]
    fn an_inherited_value_can_be_given_one_of_its_own() {
        let Some(fixture) = Fixture::open(SHAKE) else {
            return;
        };
        let before = fixture.parse();
        let at = cdo(&before);
        let field = nested(&before.exports[at].properties, &["OscillationBlendInTime"]).clone();
        assert!(unset(&field), "{:?}", field.value);

        let (patched, after) = fixture.apply(vec![edit_of(
            &field,
            EditOp::Set {
                text: "0.75".into(),
            },
        )]);

        assert_eq!(
            patched.exports.len(),
            fixture.loaded.exports_file_buffer.len() + 4 + 2,
            "four bytes of float and one more header fragment"
        );
        assert!(matches!(after.exports[at].status, ExportStatus::Complete));
        let value = nested(&after.exports[at].properties, &["OscillationBlendInTime"]);
        assert!(
            matches!(value.value, PropertyValue::Float { value } if (value - 0.75).abs() < 1e-6),
            "{}",
            value.value.summary()
        );
        assert!(stored(value));
    }

    /// Zero is a flag, not a value: the slot joins the header with its mask bit set and no bytes.
    #[test]
    fn an_inherited_value_can_be_flagged_as_zero() {
        let Some(fixture) = Fixture::open(SHAKE) else {
            return;
        };
        let before = fixture.parse();
        let at = cdo(&before);
        let field = nested(&before.exports[at].properties, &["bAttenuationByDistance"]).clone();
        assert!(unset(&field));

        let (_, after) = fixture.apply(vec![edit_of(&field, EditOp::Clear)]);

        let value = nested(&after.exports[at].properties, &["bAttenuationByDistance"]);
        assert!(matches!(value.value, PropertyValue::Bool { value: false }));
        assert!(!stored(value), "a zero flag occupies no bytes");
        assert!(matches!(after.exports[at].status, ExportStatus::Complete));
    }

    /// Storing a struct writes its all-skipped header; only then do its fields exist to be set,
    /// which takes a second round against the patched package.
    #[test]
    fn an_inherited_struct_is_stored_empty_and_then_edited_inside() {
        let Some(fixture) = Fixture::open(SHAKE) else {
            return;
        };
        let before = fixture.parse();
        let at = cdo(&before);
        let field = nested(&before.exports[at].properties, &["FOVOscillation"]).clone();
        assert!(unset(&field));

        let (patched, after) = fixture.apply(vec![edit_of(&field, EditOp::Store)]);
        let stored_struct = nested(&after.exports[at].properties, &["FOVOscillation"]);
        let PropertyValue::Struct { fields, .. } = &stored_struct.value else {
            panic!("expected a struct, got {}", stored_struct.value.summary());
        };
        assert!(
            !fields.is_empty() && fields.iter().all(unset),
            "a struct stored from nothing declares its fields and stores none"
        );

        let amplitude = nested(
            &after.exports[at].properties,
            &["FOVOscillation", "Amplitude"],
        )
        .clone();
        let reread = AssetBundle {
            asset: &patched.asset,
            exports: &patched.exports,
        };
        let again = rivals_uasset::patch_values(
            &reread,
            &after,
            &[edit_of(&amplitude, EditOp::Set { text: "2.5".into() })],
            None,
        )
        .expect("second round");
        let final_bundle = AssetBundle {
            asset: &again.asset,
            exports: &again.exports,
        };
        let last = Fixture::parse_bundle(&final_bundle, &fixture.schema, &fixture.source());
        let value = nested(
            &last.exports[at].properties,
            &["FOVOscillation", "Amplitude"],
        );
        assert!(
            matches!(value.value, PropertyValue::Float { value } if (value - 2.5).abs() < 1e-6),
            "{}",
            value.value.summary()
        );
        assert!(matches!(last.exports[at].status, ExportStatus::Complete));
    }

    /// The inverse of storing: the value's bytes and header item go, and the object inherits again.
    #[test]
    fn a_stored_value_can_be_returned_to_its_inherited_default() {
        let Some(fixture) = Fixture::open(SHAKE) else {
            return;
        };
        let before = fixture.parse();
        let at = cdo(&before);
        let field = nested(&before.exports[at].properties, &["AnimPlayRate"]).clone();
        assert!(stored(&field));

        let (patched, after) = fixture.apply(vec![edit_of(&field, EditOp::Unset)]);

        assert_eq!(
            patched.exports.len(),
            fixture.loaded.exports_file_buffer.len() - 4,
            "the float goes and the header stays the same size"
        );
        assert!(unset(nested(
            &after.exports[at].properties,
            &["AnimPlayRate"]
        )));
        assert!(matches!(after.exports[at].status, ExportStatus::Complete));
    }

    /// An unset slot shares its offset with the stored value that follows it. Storing one and
    /// editing the other in one save has to put the new bytes first and still find the second value
    /// where it ended up.
    #[test]
    fn storing_a_slot_and_editing_its_neighbour_in_one_save_keeps_stream_order() {
        let Some(fixture) = Fixture::open(SHAKE) else {
            return;
        };
        let before = fixture.parse();
        let at = cdo(&before);
        let blend_in = nested(&before.exports[at].properties, &["OscillationBlendInTime"]).clone();
        let blend_out =
            nested(&before.exports[at].properties, &["OscillationBlendOutTime"]).clone();
        assert_eq!(
            blend_in.span.map(|s| s.0),
            blend_out.span.map(|s| s.0),
            "the two share an offset"
        );

        let (_, after) = fixture.apply(vec![
            edit_of(&blend_out, EditOp::Set { text: "0.9".into() }),
            edit_of(
                &blend_in,
                EditOp::Set {
                    text: "0.75".into(),
                },
            ),
        ]);

        let read = |name: &str| match nested(&after.exports[at].properties, &[name]).value {
            PropertyValue::Float { value } => value,
            ref other => panic!("{name} is {}", other.summary()),
        };
        assert!((read("OscillationBlendInTime") - 0.75).abs() < 1e-6);
        assert!((read("OscillationBlendOutTime") - 0.9).abs() < 1e-6);
        assert!(matches!(after.exports[at].status, ExportStatus::Complete));
    }

    /// Walks struct fields by name, which is how a caller outside a table addresses a value.
    fn nested<'a>(entries: &'a [PropertyEntry], path: &[&str]) -> &'a PropertyEntry {
        let (first, rest) = path.split_first().expect("a non-empty path");
        let entry = entries
            .iter()
            .find(|e| e.name == *first)
            .unwrap_or_else(|| panic!("no field called {first}"));
        if rest.is_empty() {
            return entry;
        }
        match &entry.value {
            PropertyValue::Struct { fields, .. } => nested(fields, rest),
            other => panic!(
                "{first} is a {}, not a struct",
                rivals_uasset::kind_of(other)
            ),
        }
    }

    /// The reader records each map pair as one span and, separately, where its key ends, so a map
    /// value is written over its own bytes alone and the key beside it reads as before.
    #[test]
    fn a_map_value_is_edited_without_touching_its_key() {
        let Some(fixture) = Fixture::open(MAPS) else {
            return;
        };
        let before = fixture.parse();
        let field = before
            .exports
            .iter()
            .filter_map(|export| export.data_table.as_ref())
            .flat_map(|table| &table.rows)
            .flat_map(|row| &row.fields)
            .find(|f| {
                stored(f)
                    && matches!(&f.value, PropertyValue::Map { entries }
                        if entries.first().is_some_and(|pair| matches!(pair.value, PropertyValue::Int { .. })))
            })
            .expect("a stored map of ints")
            .clone();
        let PropertyValue::Map { entries } = &field.value else {
            panic!("{:?}", field.value);
        };

        let (patched, after) = fixture.apply(vec![edit_of(
            &field,
            EditOp::SetElement {
                index: 0,
                text: "7".into(),
            },
        )]);
        assert!(
            matches!(after.exports[0].status, ExportStatus::Complete),
            "{:?}",
            after.exports[0].status
        );
        let done = &patched.applied[0];
        let now = after
            .exports
            .iter()
            .filter_map(|export| export.data_table.as_ref())
            .flat_map(|table| &table.rows)
            .flat_map(|row| &row.fields)
            .find(|f| f.name == field.name && f.span.is_some_and(|(s, _)| s == done.offset_after))
            .expect("the edited map");
        let PropertyValue::Map {
            entries: now_entries,
        } = &now.value
        else {
            panic!("{:?}", now.value);
        };
        assert_eq!(now_entries.len(), entries.len());
        assert_eq!(now_entries[0].key.summary(), entries[0].key.summary());
        assert!(matches!(
            now_entries[0].value,
            PropertyValue::Int { value: 7 }
        ));
        for (was, is) in entries.iter().zip(now_entries).skip(1) {
            assert_eq!(was.key.summary(), is.key.summary());
            assert_eq!(was.value.summary(), is.value.summary());
        }
    }

    /// Nothing about the lookup or the splice is specific to DataTable rows, and a class default
    /// object is where Blueprint tuning values live, so an edit deep in one has to land the same
    /// way a cell does.
    #[test]
    fn a_field_nested_inside_a_class_default_object_can_be_edited() {
        let Some(fixture) = Fixture::open(SHAKE) else {
            return;
        };
        let before = fixture.parse();
        let path = ["RotOscillation", "Pitch", "Amplitude"];
        let cdo = |parsed: &rivals_uasset::ParsedPackage| {
            parsed
                .exports
                .iter()
                .position(|e| e.object_name.starts_with("Default__"))
                .expect("a class default object")
        };
        let at = cdo(&before);
        let field = nested(&before.exports[at].properties, &path).clone();
        assert!(stored(&field));

        let (patched, after) = fixture.apply(vec![edit_of(
            &field,
            EditOp::Set {
                text: "0.25".into(),
            },
        )]);

        assert_eq!(
            patched.exports.len(),
            fixture.loaded.exports_file_buffer.len(),
            "a float edit is the same width, so nothing moves"
        );
        assert_eq!(cdo(&after), at);
        for (old, new) in before.exports.iter().zip(&after.exports) {
            assert_eq!(
                std::mem::discriminant(&old.status),
                std::mem::discriminant(&new.status),
                "export {} changed status",
                old.index
            );
        }
        let value = nested(&after.exports[at].properties, &path);
        assert!(
            matches!(value.value, PropertyValue::Float { value } if (value - 0.25).abs() < 1e-6),
            "{}",
            value.value.summary()
        );
        // The sibling struct holds a field of the same name at a different offset; the name
        // alone must not have been enough to reach it.
        let roll = nested(
            &after.exports[at].properties,
            &["RotOscillation", "Roll", "Amplitude"],
        );
        let was = nested(
            &before.exports[at].properties,
            &["RotOscillation", "Roll", "Amplitude"],
        );
        assert_eq!(roll.value.summary(), was.value.summary());
    }

    /// The offset the caller holds can go stale, and writing into whatever now lives there would
    /// corrupt an unrelated property, so a kind mismatch has to be refused.
    #[test]
    fn an_offset_whose_kind_does_not_match_is_refused() {
        let Some(fixture) = Fixture::open(STRINGS) else {
            return;
        };
        let before = fixture.parse();
        let field = field_where(&before, "is a stored string", |f| {
            matches!(&f.value, PropertyValue::Str { .. }) && stored(f)
        });
        let mut edit = edit_of(&field, EditOp::Set { text: "x".into() });
        edit.expect_kind = "float".into();
        let error = match preview_edits(&fixture.request(vec![edit]), Some(&fixture.schema)) {
            Ok(_) => panic!("a stale request should be refused, not written"),
            Err(error) => error,
        };
        assert!(error.contains("not a float"), "{error}");
    }

    fn row_edit(export: u32, op: RowOp) -> RowEdit {
        RowEdit { export, op }
    }

    /// Rows go in and out by name. Everything the table kept has to read exactly as before, the
    /// copy has to equal its source and the new row has to store nothing.
    #[test]
    fn rows_are_added_copied_renamed_and_removed_by_name() {
        let Some(fixture) = Fixture::open(DEFAULTS) else {
            return;
        };
        let before = fixture.parse();
        let was = table_of(&before);
        let first = was.rows[0].name.clone();
        let second = was.rows[1].name.clone();
        let last = was.rows.last().expect("rows").name.clone();
        let export = before.exports[0].index;
        let (patched, after) = fixture.apply_changes(PackageEdits {
            rows: vec![
                row_edit(
                    export,
                    RowOp::Add {
                        name: "ZZ_New".into(),
                        at: Some(1),
                    },
                ),
                row_edit(
                    export,
                    RowOp::Duplicate {
                        source: first.clone(),
                        name: "ZZ_Copy".into(),
                        at: None,
                    },
                ),
                row_edit(
                    export,
                    RowOp::Remove {
                        name: second.to_uppercase(),
                    },
                ),
                row_edit(
                    export,
                    RowOp::Rename {
                        name: last.clone(),
                        to: "ZZ_Renamed".into(),
                    },
                ),
            ],
            ..Default::default()
        });
        assert!(
            matches!(after.exports[0].status, ExportStatus::Complete),
            "{:?}",
            after.exports[0].status
        );
        let now = table_of(&after);
        assert_eq!(now.rows.len(), was.rows.len() + 1);
        assert_eq!(now.declared_rows as usize, now.rows.len());
        assert_eq!(now.rows[0].name, first);
        assert_eq!(now.rows[1].name, "ZZ_New");
        assert!(!now.rows[1].fields.is_empty(), "declared slots are listed");
        assert!(
            now.rows[1]
                .fields
                .iter()
                .all(|field| matches!(field.value, PropertyValue::Unset { .. }))
        );
        assert_eq!(now.rows[2].name, was.rows[2].name);
        assert_eq!(now.rows[now.rows.len() - 2].name, "ZZ_Renamed");
        let copy = now.rows.last().expect("rows");
        assert_eq!(copy.name, "ZZ_Copy");
        assert_eq!(copy.fields.len(), was.rows[0].fields.len());
        for (source, copied) in was.rows[0].fields.iter().zip(&copy.fields) {
            assert_eq!(source.name, copied.name);
            assert_eq!(source.value.summary(), copied.value.summary());
        }
        assert_eq!(patched.applied.len(), 4);
        assert_eq!(patched.applied[0].name, "row ZZ_New");
        assert_eq!(patched.applied[2].after, "(removed)");
    }

    #[test]
    fn a_cell_in_a_row_being_removed_is_refused_in_the_same_save() {
        let Some(fixture) = Fixture::open(DEFAULTS) else {
            return;
        };
        let before = fixture.parse();
        let row = &table_of(&before).rows[3];
        let field = row
            .fields
            .iter()
            .find(|field| {
                stored(field)
                    && matches!(
                        field.value,
                        PropertyValue::Int { .. } | PropertyValue::Float { .. }
                    )
            })
            .expect("a stored number");
        let export = before.exports[0].index;
        let request = fixture.request_changes(PackageEdits {
            values: vec![edit_of(field, EditOp::Set { text: "1".into() })],
            rows: vec![row_edit(
                export,
                RowOp::Remove {
                    name: row.name.clone(),
                },
            )],
            ..Default::default()
        });
        let error = preview_edits(&request, Some(&fixture.schema))
            .err()
            .expect("refused");
        assert!(error.contains("is being removed"), "{error}");
    }

    /// The first entry anywhere under `fields`, struct fields and array elements included.
    fn find_field<'a>(
        fields: &'a [PropertyEntry],
        want: &dyn Fn(&PropertyEntry) -> bool,
    ) -> Option<&'a PropertyEntry> {
        for field in fields {
            if want(field) {
                return Some(field);
            }
            let nested: Vec<&[PropertyEntry]> = match &field.value {
                PropertyValue::Struct { fields, .. } => vec![fields.as_slice()],
                PropertyValue::Array { items } => items
                    .iter()
                    .filter_map(|item| match item {
                        PropertyValue::Struct { fields, .. } => Some(fields.as_slice()),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            for fields in nested {
                if let Some(found) = find_field(fields, want) {
                    return Some(found);
                }
            }
        }
        None
    }

    /// A value inside an instanced struct payload is given bytes of its own, so the byte length
    /// written in front of the payload has to grow by everything that went in: the value and
    /// whatever its block header needed. A stale length would send the re-read past the wrong end.
    /// The value is an int holding zero without bytes, whether skipped or zero-masked.
    #[test]
    fn storing_a_value_inside_an_instanced_struct_moves_its_length_prefix() {
        let Some(fixture) = Fixture::open(INSTANCED) else {
            return;
        };
        let before = fixture.parse();
        assert!(
            !before.instanced.is_empty(),
            "the sample holds instanced payloads"
        );
        let layout_of = |start: u64| {
            before
                .instanced
                .iter()
                .find(|layout| start > layout.payload_start && start < layout.payload_end)
                .copied()
        };
        let field = table_of(&before)
            .rows
            .iter()
            .find_map(|row| {
                // Zero-masked or skipped: either way the int has no bytes yet.
                find_field(&row.fields, &|field| {
                    matches!(
                        field.value,
                        PropertyValue::Int { .. }
                            | PropertyValue::Unset {
                                declared: "Int",
                                ..
                            }
                    ) && field
                        .span
                        .is_some_and(|(start, end)| start == end && layout_of(start).is_some())
                })
            })
            .expect("an int without bytes inside a payload")
            .clone();
        let start = field.span.expect("a span").0;
        let layout = layout_of(start).expect("its payload");

        let (patched, after) =
            fixture.apply(vec![edit_of(&field, EditOp::Set { text: "42".into() })]);
        assert!(
            matches!(after.exports[0].status, ExportStatus::Complete),
            "{:?}",
            after.exports[0].status
        );
        let done = &patched.applied[0];
        let now = table_of(&after)
            .rows
            .iter()
            .find_map(|row| {
                find_field(&row.fields, &|f| {
                    f.name == field.name && f.span.is_some_and(|(s, _)| s == done.offset_after)
                })
            })
            .expect("the stored int");
        assert!(
            matches!(now.value, PropertyValue::Int { value: 42 }),
            "{:?}",
            now.value
        );
        let grown = after
            .instanced
            .iter()
            .find(|candidate| candidate.size_at == layout.size_at)
            .expect("the same payload");
        let widened = (grown.payload_end - grown.payload_start) as i64
            - (layout.payload_end - layout.payload_start) as i64;
        assert!(widened >= 4, "{widened}");
        assert_eq!(
            widened,
            after.exports[0].serial_size - before.exports[0].serial_size
        );
    }

    /// An empty array gets its first element from nothing: the type's default, which for a struct
    /// is a header that stores no slot and for a name is `None`.
    #[test]
    fn an_empty_array_grows_a_defaulted_element() {
        let Some(fixture) = Fixture::open(DEFAULTS) else {
            return;
        };
        let before = fixture.parse();
        let layout_of = |f: &PropertyEntry| {
            before
                .containers
                .iter()
                .find(|layout| f.span.is_some_and(|(start, _)| start == layout.at))
        };
        let candidates: Vec<&PropertyEntry> = table_of(&before)
            .rows
            .iter()
            .flat_map(|row| &row.fields)
            .filter(|f| {
                stored(f)
                    && matches!(&f.value, PropertyValue::Array { items } if items.is_empty())
                    && layout_of(f).is_some_and(|layout| {
                        layout.default_element.is_some() || layout.default_name.is_some()
                    })
            })
            .collect();
        // A struct element proves the most: its header has to come out as a valid empty block.
        let field = candidates
            .iter()
            .find(|f| layout_of(f).is_some_and(|layout| layout.element_kind == "Struct"))
            .or(candidates.first())
            .copied()
            .expect("an empty array with a writable default")
            .clone();
        let kind = layout_of(&field).expect("its layout").element_kind;

        let (patched, after) = fixture.apply(vec![edit_of(
            &field,
            EditOp::Insert {
                index: 0,
                key: None,
            },
        )]);
        assert!(
            matches!(after.exports[0].status, ExportStatus::Complete),
            "{:?}",
            after.exports[0].status
        );
        let done = &patched.applied[0];
        let now = table_of(&after)
            .rows
            .iter()
            .flat_map(|row| &row.fields)
            .find(|f| f.name == field.name && f.span.is_some_and(|(s, _)| s == done.offset_after))
            .expect("the grown array");
        let PropertyValue::Array { items } = &now.value else {
            panic!("{:?}", now.value);
        };
        assert_eq!(items.len(), 1);
        match (&items[0], kind) {
            (PropertyValue::Struct { fields, .. }, "Struct") => {
                assert!(!fields.is_empty(), "declared slots are listed");
                assert!(
                    fields
                        .iter()
                        .all(|f| matches!(f.value, PropertyValue::Unset { .. })),
                    "{fields:?}"
                );
            }
            (PropertyValue::Name { value }, "Name") => assert_eq!(value, "None"),
            (PropertyValue::Enum { value, .. }, "Enum") => assert_eq!(*value, 0),
            (other, kind) => panic!("a {kind} element read back as {other:?}"),
        }
    }

    /// Strings are FStrings, so a source string can grow, shrink or switch to UTF-16; the count
    /// follows adds and removals and the closing word stays where the table ends.
    #[test]
    fn string_table_entries_are_changed_added_and_removed() {
        let Some(fixture) = Fixture::open(STRING_TABLE) else {
            return;
        };
        let before = fixture.parse();
        let (export, was) = before
            .exports
            .iter()
            .find_map(|e| {
                e.string_table
                    .as_ref()
                    .map(|table| (e.index, table.clone()))
            })
            .expect("a string table");
        assert!(was.entries.len() >= 3, "{} entries", was.entries.len());
        let string_edit = |op: StringOp| StringEdit { export, op };
        let (patched, after) = fixture.apply_changes(PackageEdits {
            strings: vec![
                string_edit(StringOp::SetSource {
                    index: 0,
                    key: was.entries[0].key.clone(),
                    to: "Edited \u{91D1}\u{5E01} much longer than before".into(),
                }),
                string_edit(StringOp::SetKey {
                    index: 2,
                    key: was.entries[2].key.clone(),
                    to: "ZZ_Key".into(),
                }),
                string_edit(StringOp::Remove {
                    index: 1,
                    key: was.entries[1].key.clone(),
                }),
                string_edit(StringOp::Add {
                    key: "ZZ_Added".into(),
                    source: "plain".into(),
                }),
            ],
            ..Default::default()
        });
        let export_after = after
            .exports
            .iter()
            .find(|e| e.index == export)
            .expect("the export");
        assert!(
            matches!(export_after.status, ExportStatus::Complete),
            "{:?}",
            export_after.status
        );
        let now = export_after.string_table.as_ref().expect("the table");
        assert_eq!(now.namespace, was.namespace);
        assert_eq!(now.entries.len(), was.entries.len());
        assert_eq!(now.entries[0].key, was.entries[0].key);
        assert_eq!(
            now.entries[0].source,
            "Edited \u{91D1}\u{5E01} much longer than before"
        );
        assert_eq!(now.entries[1].key, "ZZ_Key");
        assert_eq!(now.entries[1].source, was.entries[2].source);
        let last = now.entries.last().expect("entries");
        assert_eq!(
            (last.key.as_str(), last.source.as_str()),
            ("ZZ_Added", "plain")
        );
        assert_eq!(patched.applied.len(), 4);
        assert_eq!(patched.applied[2].after, "(removed)");
    }

    /// The mappings file holds two `PyWidget_LeagueSchedule_Dual` classes from two script
    /// modules, and the fuller one is the default. The widget descends from the other, which the
    /// twin search finds from the export sizes: its default object and an instance in another
    /// package then read to their end, and the choice is on record.
    #[test]
    fn a_class_under_a_twin_named_parent_reads_under_the_twin_that_fits() {
        let Some(fixture) = Fixture::open(TWIN_PARENT_WIDGET) else {
            return;
        };
        let parsed = fixture.parse();
        assert!(
            parsed
                .twins
                .iter()
                .any(|choice| choice.name == "PyWidget_LeagueSchedule_Dual" && choice.of == 2),
            "{:?}",
            parsed.twins
        );
        let default_object = export_named(&parsed, "Default__WBP_LeagueSchedule_Dual_C");
        assert!(
            matches!(default_object.status, ExportStatus::Complete),
            "{:?}",
            default_object.status
        );
        let Some(instance) = Fixture::open(TWIN_PARENT_INSTANCE) else {
            return;
        };
        let parsed = instance.parse();
        let item = export_of_class(&parsed, "WBP_LeagueSchedule_Dual_C");
        assert!(
            matches!(item.status, ExportStatus::Complete),
            "{:?}",
            item.status
        );
    }

    /// Every animation Blueprint generates a struct named `AnimBlueprintGeneratedMutableData`, so
    /// the mappings file can hold only one of them. The package's own definition, an export, has
    /// to win, and it is a struct with a parent, which used to make it look like a class named by
    /// path and unfindable by name.
    #[test]
    fn a_generated_anim_struct_is_read_from_its_own_package_not_the_mappings_twin() {
        let Some(fixture) = Fixture::open(GROUND_MOTION_ANIM_BP) else {
            return;
        };
        let parsed = fixture.parse();
        let default_object = &parsed.exports[cdo(&parsed)];
        assert!(
            !matches!(
                default_object.status,
                ExportStatus::Failed { .. } | ExportStatus::Partial { .. }
            ),
            "{:?}",
            default_object.status
        );
        let mutables = find_field(&default_object.properties, &|f| {
            f.name == "__AnimBlueprintMutables"
        })
        .expect("the mutable data");
        let PropertyValue::Struct { fields, .. } = &mutables.value else {
            panic!("{:?}", mutables.value);
        };
        assert!(
            fields
                .iter()
                .any(|f| f.name == "__ArrayProperty_7"
                    && matches!(f.value, PropertyValue::Array { .. })),
            "the package's own layout, with its float array, was used"
        );
    }

    /// The class tails that were left unexplained across the containers: a pose asset and a rig
    /// read to their end, a cue and its wave players too, an animation sequence's payload now
    /// starts after its skeleton guid, and the rest are named as the payloads they are.
    #[test]
    fn class_tails_read_or_name_every_formerly_unexplained_export() {
        let complete = |entry: &'static str, class: &str, field: &str| {
            let Some(fixture) = Fixture::open(entry) else {
                return false;
            };
            let parsed = fixture.parse();
            let export = export_of_class(&parsed, class);
            assert!(
                matches!(export.status, ExportStatus::Complete),
                "{entry}#{class}: {:?} {:?}",
                export.status,
                export.note
            );
            if !field.is_empty() {
                find_field(&export.properties, &|f| f.name == field)
                    .unwrap_or_else(|| panic!("{entry}#{class} has {field}"));
            }
            true
        };
        if !complete(POSE_ASSET, "PoseAsset", "SkeletonGuid") {
            return;
        }
        complete(SOUND_CUE, "SoundCue", "");
        complete(SOUND_CUE, "SoundNodeWavePlayer", "SoundWave");
        complete(RIG, "Rig", "");
        for (entry, kind) in NAMED_PAYLOADS {
            let Some(fixture) = Fixture::open(entry) else {
                return;
            };
            let parsed = fixture.parse();
            let export = main_export(&parsed, entry);
            assert!(
                matches!(&export.status, ExportStatus::Payload { kind: found, .. } if *found == kind),
                "{entry}: {:?}",
                export.status
            );
        }
        let Some(fixture) = Fixture::open(PER_PLATFORM_RATE) else {
            return;
        };
        let parsed = fixture.parse();
        let sequence = parsed
            .exports
            .iter()
            .find(|e| e.class_name == "AnimSequence")
            .expect("an animation sequence");
        assert!(
            matches!(
                sequence.status,
                ExportStatus::Payload {
                    kind: "compressed animation data",
                    ..
                }
            ),
            "{:?}",
            sequence.status
        );
        let guid = find_field(&sequence.properties, &|f| f.name == "SkeletonGuid")
            .expect("the skeleton guid ahead of the payload");
        let ExportStatus::Payload { consumed, .. } = sequence.status else {
            unreachable!()
        };
        assert_eq!(
            guid.span.map(|(_, end)| end),
            Some(sequence.serial_offset as u64 + consumed),
            "the payload starts where the guid ends"
        );
    }

    /// The four native layouts that failed across the containers, each read to its end: cloth
    /// tethers behind their empty reflected block, a sampling region with its bones before the
    /// sampler, a Nanite override with its cooked reference on record for renumbering, and a
    /// curve whose key handle map is empty.
    #[test]
    fn cloth_tethers_sampling_regions_nanite_overrides_and_curves_read_to_their_end() {
        let cases: [(&str, u32, &str); 4] = [
            (CLOTH_MESH, 3, "Tethers"),
            (SAMPLED_MESH, 71, "BoneIndices"),
            (NANITE_MATERIAL, 0, "CookedOverrideMaterial"),
            (CURVE_ANIM_BP, 0, ""),
        ];
        for (entry, index, field) in cases {
            let Some(fixture) = Fixture::open(entry) else {
                return;
            };
            let parsed = fixture.parse();
            let export = export_at(&parsed, index);
            assert!(
                !matches!(
                    export.status,
                    ExportStatus::Failed { .. } | ExportStatus::Partial { .. }
                ),
                "{entry}#{index}: {:?}",
                export.status
            );
            if field.is_empty() {
                continue;
            }
            let found = find_field(&export.properties, &|f| f.name == field)
                .unwrap_or_else(|| panic!("{entry}#{index} has {field}"));
            if let PropertyValue::Object { index: target, .. } = found.value {
                assert!(
                    target != 0 && parsed.references.iter().any(|r| r.index == target),
                    "the override material is on record for renumbering"
                );
            }
        }
    }

    /// Read with the interfaces after the generated-by reference, the class lands exactly and its
    /// definition is recovered, so its default object and an instance in another package read
    /// through the synthesised schema instead of failing for want of one.
    #[test]
    fn a_widget_class_implementing_an_interface_reads_with_its_default_object_and_instances() {
        let Some(fixture) = Fixture::open(WIDGET_CLASS) else {
            return;
        };
        let parsed = fixture.parse();
        for export in [
            export_of_class(&parsed, "WidgetBlueprintGeneratedClass"),
            export_named(&parsed, "Default__WBP_Common_Item_V2_Light_C"),
        ] {
            assert!(
                matches!(export.status, ExportStatus::Complete),
                "export {}: {:?} {:?}",
                export.index,
                export.status,
                export.note
            );
        }
        let Some(instance) = Fixture::open(WIDGET_INSTANCE) else {
            return;
        };
        let parsed = instance.parse();
        let item = export_of_class(&parsed, "WBP_Common_Item_V2_Light_C");
        assert!(
            matches!(item.status, ExportStatus::Complete),
            "{:?}",
            item.status
        );
    }

    /// A Blueprint class the mappings file describes wrongly shifts every inherited slot, so the
    /// instance misreads until the class is taken from its own package. Then the actor's root
    /// component and its created components read as themselves and the export lands exactly.
    #[test]
    fn a_blueprint_class_revised_after_the_mappings_dump_is_read_from_its_package() {
        let Some(fixture) = Fixture::open(STALE_CLASS_MAP) else {
            return;
        };
        let parsed = fixture.parse();
        let actor = export_of_class(&parsed, "SM_TokyoH01Building006A_C");
        assert_complete(actor);
        let stored_property = |name: &str| {
            actor
                .properties
                .iter()
                .find(|p| p.name == name && stored(p))
                .unwrap_or_else(|| panic!("{name} stored"))
        };
        // The path, not the export number: the number moves whenever the level is rebuilt.
        assert!(matches!(
            &stored_property("RootComponent").value,
            PropertyValue::Object { path: Some(path), .. }
                if path.ends_with(&format!("{}:SM_TokyoH01Building006A", actor.object_name))
        ));
        assert!(matches!(
            &stored_property("BlueprintCreatedComponents").value,
            PropertyValue::Array { items } if items.len() == 3
        ));
        assert!(
            actor
                .properties
                .iter()
                .all(|p| p.name != "bTearOff" || !stored(p))
        );
    }

    /// Two Blueprint classes with one name: the mappings file can only hold one layout under it,
    /// so the other class's instances are read with their own package's definition, found by path.
    #[test]
    fn a_blueprint_class_sharing_its_name_is_read_by_its_path() {
        let Some(fixture) = Fixture::open(SAME_NAME_MAP) else {
            return;
        };
        let parsed = fixture.parse();
        let twins: Vec<&rivals_uasset::ParsedExport> = parsed
            .exports
            .iter()
            .filter(|e| e.class_name == "SM_NewYorkM01Building020A_C")
            .collect();
        assert!(!twins.is_empty(), "the twinned class is instanced");
        for actor in &twins {
            assert_complete(actor);
        }
        // Only an instance that overrides them stores these, so one carrying both is what proves
        // the class was read through its own path rather than its twin's.
        let stored_named = |actor: &rivals_uasset::ParsedExport, name: &str| {
            actor.properties.iter().any(|p| p.name == name && stored(p))
        };
        assert!(
            twins
                .iter()
                .any(|actor| stored_named(actor, "RootComponent")
                    && stored_named(actor, "BlueprintCreatedComponents")),
            "no instance of the twinned class stores its components"
        );
    }

    /// The default object's sparse class data is a struct reference and a block of a struct this
    /// package defines. The block itself stops at a struct the mappings file describes short, so
    /// it is named as a payload; the struct reference still has to be on record for renumbering.
    #[test]
    fn an_anim_blueprint_default_object_reads_its_sparse_class_data() {
        let Some(fixture) = Fixture::open(ANIM_BLUEPRINT) else {
            return;
        };
        let parsed = fixture.parse();
        let default_object = &parsed.exports[0];
        assert_eq!(default_object.class_name, "101595_AnimBP_C");
        assert!(
            matches!(
                default_object.status,
                ExportStatus::Complete
                    | ExportStatus::Payload {
                        kind: "sparse class data",
                        ..
                    }
            ),
            "{:?}",
            default_object.status
        );
        let constant_data = parsed
            .exports
            .iter()
            .find(|e| e.object_name == "AnimBlueprintGeneratedConstantData")
            .expect("the sparse data struct");
        let referenced = parsed
            .references
            .iter()
            .any(|r| r.index == constant_data.index as i32 + 1);
        assert!(
            referenced,
            "the struct reference is recorded for renumbering"
        );
    }

    /// An enum edits by enumerator name whether the schema declares it (the texture's compression
    /// setting) or a native reader produced it (a channel's extrapolation mode), and reads back
    /// under that name.
    #[test]
    fn an_enum_edits_by_name_in_a_schema_slot_and_in_a_native_struct() {
        for (entry, index, property, to) in [
            (TEXTURE, 0u32, "CompressionSettings", "TC_Default"),
            (CHANNEL_ENUM, 256, "PreInfinityExtrap", "RCCE_Linear"),
        ] {
            let Some(fixture) = Fixture::open(entry) else {
                return;
            };
            let parsed = fixture.parse();
            let export = export_at(&parsed, index);
            let field = find_field(&export.properties, &|f| {
                f.name == property && matches!(f.value, PropertyValue::Enum { .. })
            })
            .expect("an enum field");
            let PropertyValue::Enum {
                enum_type, name, ..
            } = &field.value
            else {
                unreachable!()
            };
            assert!(enum_type.is_some(), "{property} knows its enum type");
            assert_ne!(name.as_deref(), Some(to));

            let (_, reread) = fixture.apply(vec![edit_of(field, EditOp::Set { text: to.into() })]);
            let after = find_field(&export_at(&reread, index).properties, &|f| {
                f.name == property && f.span.map(|(s, _)| s) == field.span.map(|(s, _)| s)
            })
            .expect("the field after");
            assert!(
                matches!(&after.value, PropertyValue::Enum { name: Some(n), .. } if n == to),
                "{property}: {:?}",
                after.value
            );
        }
    }

    /// A map that already holds pairs grows by a pair under the key typed for it, written in the
    /// key's own kind, and the new pair reads back under that key.
    #[test]
    fn a_keyed_map_grows_by_a_typed_key() {
        let Some(fixture) = Fixture::open(MAPS) else {
            return;
        };
        let before = fixture.parse();
        let field = table_of(&before)
            .rows
            .iter()
            .flat_map(|row| &row.fields)
            .find(|f| {
                stored(f)
                    && matches!(&f.value, PropertyValue::Map { entries }
                        if entries.first().is_some_and(|pair| matches!(pair.key, PropertyValue::Name { .. } | PropertyValue::Int { .. } | PropertyValue::Str { .. })))
            })
            .expect("a stored map keyed by a name, a number or a string")
            .clone();
        let PropertyValue::Map { entries } = &field.value else {
            unreachable!()
        };
        let key = match &entries[0].key {
            PropertyValue::Int { .. } => "424242".to_string(),
            _ => "RivalsToolkitKey".to_string(),
        };

        let (patched, after) = fixture.apply(vec![edit_of(
            &field,
            EditOp::Insert {
                index: entries.len() as u32,
                key: Some(key.clone()),
            },
        )]);
        assert!(
            matches!(after.exports[0].status, ExportStatus::Complete),
            "{:?}",
            after.exports[0].status
        );
        let done = &patched.applied[0];
        let now = table_of(&after)
            .rows
            .iter()
            .flat_map(|row| &row.fields)
            .find(|f| f.name == field.name && f.span.is_some_and(|(s, _)| s == done.offset_after))
            .expect("the grown map");
        let PropertyValue::Map { entries: grown } = &now.value else {
            panic!("{:?}", now.value);
        };
        assert_eq!(grown.len(), entries.len() + 1);
        assert_eq!(grown[grown.len() - 1].key.summary(), key);

        let repeat = rivals_uasset::patch_values(
            &fixture.bundle(),
            &before,
            &[edit_of(
                &field,
                EditOp::Insert {
                    index: 0,
                    key: Some(entries[0].key.summary()),
                },
            )],
            None,
        );
        let err = repeat.err().expect("a repeated key is refused");
        assert!(err.contains("already holds the key"), "{err}");
    }

    /// A natively serialized struct stores from nothing as the struct the engine would construct:
    /// a Niagara variable named None with an empty type block, and a float channel with no keys
    /// and the default tick resolution.
    #[test]
    fn a_native_struct_stores_from_nothing_as_its_constructed_default() {
        for (entry, property, struct_name, inner, want) in [
            (
                GPU_SCRIPT,
                "RootVariable",
                "NiagaraVariable",
                "Name",
                "None",
            ),
            (
                TRANSFORM_SECTION,
                "ManualWeight",
                "MovieSceneFloatChannel",
                "Numerator",
                "24000",
            ),
        ] {
            let Some(fixture) = Fixture::open(entry) else {
                return;
            };
            let parsed = fixture.parse();
            let (at, field) = parsed
                .exports
                .iter()
                .enumerate()
                .find_map(|(at, export)| {
                    find_field(&export.properties, &|f| f.name == property && unset(f))
                        .map(|field| (at, field.clone()))
                })
                .unwrap_or_else(|| panic!("an unset {property} in {entry}"));
            let slot_of = |f: &PropertyEntry| f.slot.map(|s| (s.header_at, s.schema_index));

            let (_, after) = fixture.apply(vec![edit_of(&field, EditOp::Store)]);
            assert_not_failed(&after.exports[at]);
            let stored_struct = find_field(&after.exports[at].properties, &|f| {
                f.name == property && slot_of(f) == slot_of(&field)
            })
            .expect("the stored struct");
            let PropertyValue::Struct { name, fields } = &stored_struct.value else {
                panic!("{property}: {}", stored_struct.value.summary());
            };
            assert_eq!(name, struct_name);
            let value = find_field(fields, &|f| f.name == inner).expect(inner);
            assert_eq!(value.value.summary(), want, "{property}.{inner}");
        }
    }

    /// A texture's smallest mips and a mesh's LOD0 collision sit inline in the export data, and
    /// the bulk table addresses them from their export's start. Storing a value ahead of them moves
    /// them, and the table has to follow; before this the offsets were left behind.
    #[test]
    fn inline_bulk_offsets_follow_a_widening_edit() {
        for (entry, property) in [(TEXTURE, "LODBias"), (STATIC_MESH, "LightMapResolution")] {
            let Some(fixture) = Fixture::open(entry) else {
                return;
            };
            let parsed = fixture.parse();
            let before = rivals_uasset::read_header(&fixture.bundle()).expect("header");
            let placed_before =
                rivals_uasset::inline_payloads(&before, &fixture.loaded.exports_file_buffer)
                    .expect("inline payloads placed");
            assert!(!placed_before.is_empty(), "{entry} has inline bulk data");
            let (export, slot) = parsed
                .exports
                .iter()
                .find_map(|export| {
                    export
                        .properties
                        .iter()
                        .find(|p| {
                            p.name == property && matches!(p.value, PropertyValue::Unset { .. })
                        })
                        .map(|slot| (export, slot))
                })
                .expect("an unset slot to store");

            let (patched, _) = fixture.apply(vec![edit_of(slot, EditOp::Set { text: "1".into() })]);
            let after = rivals_uasset::read_header(&AssetBundle {
                asset: &patched.asset,
                exports: &patched.exports,
            })
            .expect("header");
            let grew = after.exports[export.index as usize].serial_size
                - before.exports[export.index as usize].serial_size;
            assert!(grew > 0, "{entry}: storing {property} widened the export");
            for payload in &placed_before {
                let was = before.data_resources[payload.resource].serial_offset;
                let is = after.data_resources[payload.resource].serial_offset;
                if payload.owner == export.index as usize {
                    assert_eq!(
                        is,
                        was + grew,
                        "{entry}: resource {} moved with its export",
                        payload.resource
                    );
                } else {
                    assert_eq!(is, was, "{entry}: resource {} stayed", payload.resource);
                }
            }
        }
    }

    /// Only for an export a test has already identified by content, so it can be looked up again
    /// in a re-parsed package. Addressing one by a written-down index instead rots: the table
    /// reshuffles on a game patch and the lookup silently returns a different export.
    fn export_at(
        parsed: &rivals_uasset::ParsedPackage,
        index: u32,
    ) -> &rivals_uasset::ParsedExport {
        parsed
            .exports
            .iter()
            .find(|e| e.index == index)
            .unwrap_or_else(|| panic!("export {index}"))
    }

    /// The package's main object, which the cooker names after the package file. That name
    /// survives a patch where the export's place in the table does not.
    fn main_export<'a>(
        parsed: &'a rivals_uasset::ParsedPackage,
        entry: &str,
    ) -> &'a rivals_uasset::ParsedExport {
        let stem = entry.rsplit('/').next().unwrap_or(entry);
        let stem = stem.split('.').next().unwrap_or(stem);
        export_named(parsed, stem)
    }

    fn export_of_class<'a>(
        parsed: &'a rivals_uasset::ParsedPackage,
        class: &str,
    ) -> &'a rivals_uasset::ParsedExport {
        export_where(parsed, &format!("of class {class:?}"), |e| {
            e.class_name == class
        })
    }

    fn export_named<'a>(
        parsed: &'a rivals_uasset::ParsedPackage,
        name: &str,
    ) -> &'a rivals_uasset::ParsedExport {
        export_where(parsed, &format!("named {name:?}"), |e| {
            e.object_name == name
        })
    }

    /// The first export the predicate accepts. A miss lists what the package does hold, since the
    /// usual cause is a game patch reshaping the asset and the next step is picking a new key.
    fn export_where<'a>(
        parsed: &'a rivals_uasset::ParsedPackage,
        what: &str,
        want: impl Fn(&rivals_uasset::ParsedExport) -> bool,
    ) -> &'a rivals_uasset::ParsedExport {
        parsed.exports.iter().find(|e| want(e)).unwrap_or_else(|| {
            let held: Vec<String> = parsed
                .exports
                .iter()
                .take(40)
                .map(|e| format!("[{}] {} {}", e.index, e.class_name, e.object_name))
                .collect();
            panic!(
                "no export {what} in this package.\nIt holds: {}{}\nIf a game patch reshaped the asset, re-pin the test to what it holds now.",
                held.join(", "),
                if parsed.exports.len() > 40 {
                    format!(" and {} more", parsed.exports.len() - 40)
                } else {
                    String::new()
                }
            )
        })
    }

    fn assert_not_failed(export: &rivals_uasset::ParsedExport) {
        assert!(
            !matches!(export.status, ExportStatus::Failed { .. }),
            "export {}: {:?}",
            export.index,
            export.status
        );
    }

    fn assert_complete(export: &rivals_uasset::ParsedExport) {
        assert!(
            matches!(export.status, ExportStatus::Complete),
            "export {}: {:?}",
            export.index,
            export.status
        );
    }

    /// The evaluation tree serializes itself with no header anywhere; the reader lays it out as
    /// the root node, two entry tables and their items.
    #[test]
    fn compiled_sequence_data_reads_its_entity_tree() {
        let Some(fixture) = Fixture::open(ENTITY_TREE) else {
            return;
        };
        let parsed = fixture.parse();
        let compiled = export_of_class(&parsed, "MovieSceneCompiledData");
        assert_complete(compiled);
        let child = find_field(&compiled.properties, &|f| {
            f.name == "ChildNodes" && f.element == Some(0)
        })
        .expect("the one child node");
        let PropertyValue::Struct { fields, .. } = &child.value else {
            panic!("{:?}", child.value);
        };
        assert!(
            matches!(fields[5].value, PropertyValue::Int { value: 0 }),
            "data entry 0"
        );
    }

    /// The cooked flag of a per-platform property is a full word, so read one byte wide every
    /// following value sat three bytes early.
    #[test]
    fn per_platform_properties_read_their_cooked_flag_as_a_word() {
        let Some(fixture) = Fixture::open(PER_PLATFORM_RATE) else {
            return;
        };
        let parsed = fixture.parse();
        let animation = export_of_class(&parsed, "AnimSequence");
        assert_not_failed(animation);
        let rate = find_field(&animation.properties, &|f| {
            f.name == "PlatformTargetFrameRate"
        })
        .expect("PlatformTargetFrameRate");
        let numerator =
            find_field(std::slice::from_ref(rate), &|f| f.name == "Numerator").expect("Numerator");
        assert!(matches!(numerator.value, PropertyValue::Int { value: 60 }));

        let Some(fixture) = Fixture::open(SKELETAL_MESH) else {
            return;
        };
        let parsed = fixture.parse();
        let mesh = export_of_class(&parsed, "SkeletalMesh");
        assert_not_failed(mesh);
        let hysteresis =
            find_field(&mesh.properties, &|f| f.name == "LODHysteresis").expect("LODHysteresis");
        assert!(
            matches!(hysteresis.value, PropertyValue::Float { value } if (0.0..1.0).contains(&value)),
            "{:?}",
            hysteresis.value
        );
    }

    #[test]
    fn a_number_formatted_text_reads_its_source_value_and_takes_a_new_one() {
        let Some(fixture) = Fixture::open(NUMBER_TEXT) else {
            return;
        };
        let parsed = fixture.parse();
        let component = export_named(&parsed, "ThresholdValueText");
        assert_complete(component);
        let at = component.index;
        let text = find_field(&component.properties, &|f| f.name == "Text").expect("Text");
        assert!(
            matches!(&text.value, PropertyValue::Text { value: Some(v), .. } if v == "2"),
            "{:?}",
            text.value
        );

        let (_, reread) = fixture.apply(vec![edit_of(text, EditOp::Set { text: "3".into() })]);
        let component = export_at(&reread, at);
        assert_complete(component);
        let after = find_field(&component.properties, &|f| f.name == "Text").expect("Text");
        assert!(
            matches!(&after.value, PropertyValue::Text { value: Some(v), .. } if v == "3"),
            "{:?}",
            after.value
        );
        assert_eq!(after.span, text.span, "a number keeps its width");
    }

    #[test]
    fn a_runtime_font_reads_its_cooked_font_faces() {
        let Some(fixture) = Fixture::open(FONT) else {
            return;
        };
        let parsed = fixture.parse();
        let font = export_of_class(&parsed, "Font");
        assert_not_failed(font);
        let asset =
            find_field(&font.properties, &|f| f.name == "FontFaceAsset").expect("FontFaceAsset");
        assert!(matches!(asset.value, PropertyValue::Object { index, .. } if index < 0));
    }

    /// retoc names an import it cannot resolve `UnknownExport`; an instance of such a class has no
    /// layout to read against and is named for what it is rather than counted as a decode failure.
    #[test]
    fn an_instance_of_a_class_this_build_lacks_is_an_opaque_payload() {
        let Some(fixture) = Fixture::open(UNRESOLVED_CLASS) else {
            return;
        };
        let parsed = fixture.parse();
        // The class is a script import the game's table cannot name, so retoc carries its raw
        // index through the legacy form rather than a placeholder.
        let component = export_where(&parsed, "instancing a class this build lacks", |e| {
            rivals_uasset::is_unresolved_import_name(&e.class_name)
        });
        assert!(
            component.class_name.starts_with("__zenrawscripthash_"),
            "{}",
            component.class_name
        );
        assert!(
            matches!(&component.status, ExportStatus::Payload { kind, consumed: 0, .. }
                if *kind == "instance of a class this build does not have"),
            "{:?}",
            component.status
        );
    }

    /// An export and its subobjects are copied to the end of the table under a new name, the
    /// references between them pointing at the copies, and the result still converts to zen: the
    /// dependency graph stays acyclic and the copies get public hashes of their own.
    #[test]
    fn an_export_is_duplicated_with_its_subobjects_and_converts_to_zen() {
        let Some(fixture) = Fixture::open(REDIRECT_CUE) else {
            return;
        };
        let before = fixture.parse();
        let request = |export: &rivals_uasset::ParsedExport| DuplicateExport {
            export: export.index,
            name: format!("{}_Copy", export.object_name),
            into_level: None,
        };
        let candidates: Vec<(&rivals_uasset::ParsedExport, rivals_uasset::DuplicatePlan)> = before
            .exports
            .iter()
            .filter_map(|export| {
                rivals_uasset::plan_duplication(&before, &[request(export)])
                    .ok()
                    .map(|mut plans| (export, plans.remove(0)))
            })
            .collect();
        let (root, plan) = candidates
            .iter()
            .find(|(_, plan)| plan.members.len() > 1)
            .or(candidates.first())
            .expect("an export that can be duplicated");
        let (patched, after) = fixture.apply_changes(PackageEdits {
            duplicate_exports: vec![request(root)],
            ..Default::default()
        });
        assert_eq!(
            after.exports.len(),
            before.exports.len() + plan.members.len()
        );
        let root_at = plan
            .members
            .iter()
            .position(|&m| m == root.index)
            .expect("the root is in its set");
        let copy = &after.exports[plan.copies[root_at] as usize];
        assert_eq!(copy.object_name, format!("{}_Copy", root.object_name));
        assert_eq!(copy.class_name, root.class_name);
        assert_eq!(copy.outer_index, root.outer_index);
        for (&member, &index) in plan.members.iter().zip(&plan.copies) {
            if member == root.index {
                continue;
            }
            let source = &before.exports[member as usize];
            let made = &after.exports[index as usize];
            assert_eq!(made.object_name, source.object_name);
            let outer_copy = plan
                .members
                .iter()
                .position(|&m| m as i32 + 1 == source.outer_index)
                .map(|at| plan.copies[at] as i32 + 1)
                .expect("the outer is in the set");
            assert_eq!(made.outer_index, outer_copy);
        }
        assert_eq!(
            patched.exports.len(),
            fixture.loaded.exports_file_buffer.len()
                + plan
                    .members
                    .iter()
                    .map(|&m| before.exports[m as usize].serial_size as usize)
                    .sum::<usize>()
        );

        let bundle = FSerializedAssetBundle {
            asset_file_buffer: patched.asset.clone(),
            exports_file_buffer: patched.exports.clone(),
            bulk_data_buffer: fixture.loaded.bulk_data_buffer.clone(),
            optional_bulk_data_buffer: fixture.loaded.optional_bulk_data_buffer.clone(),
            memory_mapped_bulk_data_buffer: fixture.loaded.memory_mapped_bulk_data_buffer.clone(),
        };
        let engine = retoc::version::EngineVersion::UE5_3;
        let shader_maps: std::collections::HashMap<String, Vec<retoc::FSHAHash>> =
            std::collections::HashMap::new();
        let path: retoc::UEPathBuf = format!("../../../{}", fixture.entry).into();
        retoc::zen_asset_conversion::build_zen_asset(
            bundle,
            &shader_maps,
            &path,
            Some(engine.package_file_version()),
            engine.container_header_version(),
            false,
            None,
            None,
            &retoc::logging::Log::no_log(),
        )
        .expect("the duplicated package converts to zen");
    }

    /// A payload in the `.ubulk` is swapped for one four bytes longer: the entries after it move,
    /// the file grows by four, and the table reads the new size back.
    #[test]
    fn a_separate_bulk_payload_is_replaced_and_the_entries_after_it_follow() {
        let Some(fixture) = Fixture::open(TEXTURE) else {
            return;
        };
        let before = fixture.parse();
        let ubulk: Vec<&rivals_uasset::ResourceInfo> = before
            .resources
            .iter()
            .filter(|r| r.placement == "ubulk" && r.locked.is_none())
            .collect();
        assert!(ubulk.len() >= 2, "{} .ubulk entries", ubulk.len());
        let target = ubulk[0];
        let later = ubulk[1];
        let file = fixture.loaded.bulk_data_buffer.as_ref().expect(".ubulk");
        let mut bytes = file
            [target.serial_offset as usize..(target.serial_offset + target.serial_size) as usize]
            .to_vec();
        bytes.extend_from_slice(&[0xAB; 4]);

        let (patched, after) = fixture.apply_changes(PackageEdits {
            bulk: vec![BulkEdit {
                resource: target.index,
                bytes: bytes.clone(),
            }],
            ..Default::default()
        });
        let rewritten = patched.bulk.as_ref().expect("a rewritten .ubulk");
        assert_eq!(rewritten.len(), file.len() + 4);
        let now = &after.resources[target.index as usize];
        assert_eq!(now.serial_size, target.serial_size + 4);
        assert_eq!(now.raw_size, target.serial_size + 4);
        assert_eq!(
            after.resources[later.index as usize].serial_offset,
            later.serial_offset + 4
        );
        assert_eq!(
            &rewritten[now.serial_offset as usize..(now.serial_offset + now.serial_size) as usize],
            &bytes[..]
        );
        assert_eq!(
            patched.exports.len(),
            fixture.loaded.exports_file_buffer.len()
        );
    }

    /// An inline payload is swapped for one four bytes longer: the export grows by four, the
    /// entries after it inside that export move, and the index word before each stays in place.
    #[test]
    fn an_inline_bulk_payload_is_replaced_and_its_export_grows() {
        let Some(fixture) = Fixture::open(TEXTURE) else {
            return;
        };
        let before = fixture.parse();
        let inline: Vec<&rivals_uasset::ResourceInfo> = before
            .resources
            .iter()
            .filter(|r| r.placement == "inline" && r.locked.is_none())
            .collect();
        assert!(inline.len() >= 2, "{} inline entries", inline.len());
        let target = inline[0];
        let owner = target.owner.expect("an owner");
        let bundle = fixture.bundle();
        let header = rivals_uasset::read_header(&bundle).expect("header");
        let placed =
            rivals_uasset::locate_inline_payload(&header, bundle.exports, target.index as usize)
                .expect("placed");
        let mut bytes =
            bundle.exports[placed.start as usize..(placed.start + placed.size) as usize].to_vec();
        bytes.extend_from_slice(&[0xCD; 4]);
        let export_size = export_at(&before, owner).serial_size;

        let (patched, after) = fixture.apply_changes(PackageEdits {
            bulk: vec![BulkEdit {
                resource: target.index,
                bytes: bytes.clone(),
            }],
            ..Default::default()
        });
        assert!(patched.bulk.is_none(), "the sidecar is untouched");
        assert_eq!(
            patched.exports.len(),
            fixture.loaded.exports_file_buffer.len() + 4
        );
        let now = &after.resources[target.index as usize];
        assert_eq!(now.serial_size, target.serial_size + 4);
        assert_eq!(now.owner, Some(owner));
        assert_eq!(export_at(&after, owner).serial_size, export_size + 4);
        for (was, is) in before.resources.iter().zip(&after.resources) {
            if was.owner == Some(owner) && was.serial_offset > target.serial_offset {
                assert_eq!(
                    is.serial_offset,
                    was.serial_offset + 4,
                    "resource {}",
                    was.index
                );
            } else if was.index != target.index {
                assert_eq!(
                    is.serial_offset, was.serial_offset,
                    "resource {}",
                    was.index
                );
            }
        }
    }

    /// Every Blueprint script in a level package disassembles to exactly the loaded size the
    /// export declares. That size is the oracle: an operand read at the wrong width would land
    /// anywhere else, however plausible the instructions looked.
    #[test]
    fn level_blueprint_scripts_disassemble_whole() {
        let Some(fixture) = Fixture::open(LEVEL) else {
            return;
        };
        let parsed = fixture.parse();
        let with_script: Vec<_> = parsed
            .exports
            .iter()
            .filter(|e| e.script.is_some())
            .collect();
        assert!(
            !with_script.is_empty(),
            "the level Blueprint carries functions"
        );
        for export in &with_script {
            let script = export.script.as_ref().expect("a script");
            assert!(
                script.complete(),
                "{} stopped: {:?}",
                export.object_name,
                script.stopped
            );
            assert_eq!(
                script.decoded_size, script.buffer_size,
                "{} accounts for every loaded byte",
                export.object_name
            );
        }
        let event = with_script
            .iter()
            .find(|e| e.object_name == "ReceiveBeginPlay")
            .expect("the level's begin play event");
        let script = event.script.as_ref().expect("a script");
        assert_eq!((script.storage_size, script.buffer_size), (14, 18));
        assert_eq!(script.statements.len(), 3);
    }

    /// A constant inside a function is changed where it sits: the script keeps its length, its
    /// size words and its statements, and reads the new value back.
    #[test]
    fn a_script_constant_is_changed_in_place() {
        let Some(fixture) = Fixture::open(LEVEL) else {
            return;
        };
        let before = fixture.parse();
        let event = before
            .exports
            .iter()
            .find(|e| e.object_name == "ReceiveBeginPlay")
            .expect("the level's begin play event");
        let script = event.script.as_ref().expect("a script");
        let first = rivals_uasset::literals(&script.statements[0].expr);
        assert!(
            matches!(
                first.as_slice(),
                [rivals_uasset::Expr::IntConst { value: 411, .. }]
            ),
            "{first:?}"
        );

        let (patched, after) = fixture.apply_changes(PackageEdits {
            scripts: vec![ScriptConstEdit {
                export: event.index,
                statement: 0,
                constant: 0,
                value: "1000".to_string(),
            }],
            ..Default::default()
        });
        let now = export_at(&after, event.index);
        let script = now.script.as_ref().expect("still disassembles");
        assert!(script.complete(), "{:?}", script.stopped);
        assert_eq!((script.storage_size, script.buffer_size), (14, 18));
        assert_eq!(script.statements.len(), 3);
        assert_eq!(now.serial_size, event.serial_size);
        let first = rivals_uasset::literals(&script.statements[0].expr);
        assert!(
            matches!(
                first.as_slice(),
                [rivals_uasset::Expr::IntConst { value: 1000, .. }]
            ),
            "{first:?}"
        );
        assert_eq!(patched.applied.len(), 1);
        assert_eq!(
            (
                patched.applied[0].before.as_str(),
                patched.applied[0].after.as_str()
            ),
            ("411", "1000")
        );

        let request = fixture.request_changes(PackageEdits {
            scripts: vec![ScriptConstEdit {
                export: event.index,
                statement: 15,
                constant: 0,
                value: "1".to_string(),
            }],
            ..Default::default()
        });
        let err = match preview_edits(&request, Some(&fixture.schema)) {
            Ok(_) => panic!("Return holds no literal, so the preview should refuse"),
            Err(err) => err,
        };
        assert!(err.contains("holds no literal constant"), "{err}");
    }

    /// A script that disassembles can be replaced at another length, because its loaded size can
    /// be worked out and the two words in front of it rewritten to match.
    #[test]
    fn a_script_is_replaced_at_another_length_and_its_size_words_follow() {
        let Some(fixture) = Fixture::open(LEVEL) else {
            return;
        };
        let before = fixture.parse();
        let event = before
            .exports
            .iter()
            .find(|e| e.object_name == "ReceiveBeginPlay")
            .expect("the level's begin play event");
        let ExportStatus::Payload {
            consumed,
            payload_bytes,
            ..
        } = &event.status
        else {
            unreachable!("a function with bytecode is a payload")
        };
        let bundle = fixture.bundle();
        let header = rivals_uasset::read_header(&bundle).expect("header");
        let (slice, _) = rivals_uasset::export_bytes(&bundle, &header, event.index).expect("bytes");
        let mut bytes = slice[*consumed as usize..(*consumed + *payload_bytes) as usize].to_vec();
        // One more Nothing in front of the end marker: one byte longer, stored and loaded alike.
        let last = bytes.len() - 1;
        bytes.insert(last, 0x0B);

        let (patched, after) = fixture.apply_changes(PackageEdits {
            payloads: vec![PayloadEdit {
                export: event.index,
                bytes: bytes.clone(),
            }],
            ..Default::default()
        });
        let now = export_at(&after, event.index);
        let script = now.script.as_ref().expect("the replacement disassembles");
        assert!(script.complete(), "{:?}", script.stopped);
        assert_eq!((script.storage_size, script.buffer_size), (15, 19));
        assert_eq!(script.statements.len(), 4);
        assert_eq!(now.serial_size, event.serial_size + 1);
        assert_eq!(
            patched.applied.len(),
            2,
            "the script and the size words in front of it"
        );
    }

    /// A payload the reader measures but does not decode is swapped for itself plus four bytes:
    /// the export grows, the properties before it read as before, and the status keeps its kind.
    #[test]
    fn an_export_payload_is_replaced_by_itself_plus_four_bytes() {
        let Some(fixture) = Fixture::open(PER_PLATFORM_RATE) else {
            return;
        };
        let before = fixture.parse();
        let export = before
            .exports
            .iter()
            .find(|e| rivals_uasset::payload_lock(e, &before.resources).is_none())
            .expect("an export with a replaceable payload");
        let ExportStatus::Payload {
            consumed,
            payload_bytes,
            kind,
        } = &export.status
        else {
            unreachable!()
        };
        let bundle = fixture.bundle();
        let header = rivals_uasset::read_header(&bundle).expect("header");
        let (slice, _) =
            rivals_uasset::export_bytes(&bundle, &header, export.index).expect("bytes");
        let mut bytes = slice[*consumed as usize..(*consumed + *payload_bytes) as usize].to_vec();
        bytes.extend_from_slice(&[0xEF; 4]);

        let (patched, after) = fixture.apply_changes(PackageEdits {
            payloads: vec![PayloadEdit {
                export: export.index,
                bytes: bytes.clone(),
            }],
            ..Default::default()
        });
        assert_eq!(
            patched.exports.len(),
            fixture.loaded.exports_file_buffer.len() + 4
        );
        let now = export_at(&after, export.index);
        assert!(
            matches!(&now.status, ExportStatus::Payload { consumed: c, payload_bytes: p, kind: k }
                if c == consumed && *p == payload_bytes + 4 && k == kind),
            "{:?}",
            now.status
        );
        assert_eq!(now.serial_size, export.serial_size + 4);
        assert_eq!(patched.applied.len(), 1);
        assert!(patched.applied[0].name.ends_with("payload"));
    }

    /// The event graph's entry points and latent resume offsets point into it and are not
    /// rewritten, so unlike any other function it may not change size.
    #[test]
    fn a_resized_event_graph_is_refused() {
        let Some(fixture) = Fixture::open(LEVEL) else {
            return;
        };
        let before = fixture.parse();
        let graph = before
            .exports
            .iter()
            .find(|e| e.object_name.starts_with("ExecuteUbergraph_") && e.script.is_some())
            .expect("the level's event graph");
        let ExportStatus::Payload {
            consumed,
            payload_bytes,
            ..
        } = &graph.status
        else {
            unreachable!("a function with bytecode is a payload")
        };
        let bundle = fixture.bundle();
        let header = rivals_uasset::read_header(&bundle).expect("header");
        let (slice, _) = rivals_uasset::export_bytes(&bundle, &header, graph.index).expect("bytes");
        let mut bytes = slice[*consumed as usize..(*consumed + *payload_bytes) as usize].to_vec();
        let last = bytes.len() - 1;
        bytes.insert(last, 0x0B);
        let err = rivals_uasset::patch_package(
            &bundle,
            &before,
            &PackageEdits {
                payloads: vec![PayloadEdit {
                    export: graph.index,
                    bytes,
                }],
                ..Default::default()
            },
            None,
        )
        .err()
        .expect("a longer event graph is refused");
        assert!(err.contains("event graph"), "{err}");
    }

    /// A function's bytecode is measured to its start and end, so it can be swapped for bytes of
    /// the same length: here itself with one byte changed. A replacement that does not disassemble
    /// has no loaded size to write, so it is held to the length it replaces.
    #[test]
    fn a_functions_bytecode_is_replaced_at_its_own_length() {
        let Some(fixture) = Fixture::open(WIDGET_CLASS) else {
            return;
        };
        let before = fixture.parse();
        let function = before
            .exports
            .iter()
            .find(|e| {
                matches!(e.status, ExportStatus::Payload { kind: "bytecode", payload_bytes, .. } if payload_bytes > 4)
            })
            .expect("a function with bytecode");
        let ExportStatus::Payload {
            consumed,
            payload_bytes,
            ..
        } = function.status
        else {
            unreachable!()
        };
        assert!(
            rivals_uasset::payload_lock(function, &before.resources).is_none(),
            "measured bytecode is replaceable"
        );
        let bundle = fixture.bundle();
        let header = rivals_uasset::read_header(&bundle).expect("header");
        let (slice, _) =
            rivals_uasset::export_bytes(&bundle, &header, function.index).expect("bytes");
        let mut script = slice[consumed as usize..(consumed + payload_bytes) as usize].to_vec();
        let last = script.len() - 1;
        script[last] ^= 0x01;
        let (patched, after) = fixture.apply_changes(PackageEdits {
            payloads: vec![PayloadEdit {
                export: function.index,
                bytes: script.clone(),
            }],
            ..Default::default()
        });
        assert_eq!(
            patched.exports.len(),
            fixture.loaded.exports_file_buffer.len()
        );
        let now = export_at(&after, function.index);
        assert!(
            matches!(now.status, ExportStatus::Payload { consumed: c, payload_bytes: p, kind: "bytecode" }
                if c == consumed && p == payload_bytes),
            "{:?}",
            now.status
        );
        let patched_bundle = AssetBundle {
            asset: &patched.asset,
            exports: &patched.exports,
        };
        let patched_header = rivals_uasset::read_header(&patched_bundle).expect("patched header");
        let (slice, _) =
            rivals_uasset::export_bytes(&patched_bundle, &patched_header, function.index)
                .expect("patched bytes");
        assert_eq!(
            &slice[consumed as usize..(consumed + payload_bytes) as usize],
            &script[..]
        );

        script.push(0);
        let err = rivals_uasset::patch_package(
            &bundle,
            &before,
            &PackageEdits {
                payloads: vec![PayloadEdit {
                    export: function.index,
                    bytes: script,
                }],
                ..Default::default()
            },
            None,
        )
        .err()
        .expect("a script that does not decode is held to its length");
        assert!(err.contains("own length"), "{err}");
    }

    /// A key moved past its neighbour comes out where its new frame sorts with its value intact,
    /// a frame retyped as a value stays where its neighbours leave room, and a frame that would
    /// pass a neighbour is refused before anything is written.
    #[test]
    fn a_channel_key_moves_with_its_value_and_a_retimed_key_keeps_its_place() {
        let Some(fixture) = Fixture::open(FLOAT_SECTION) else {
            return;
        };
        let before = fixture.parse();
        let export = export_of_class(&before, "MovieSceneFloatSection");
        let at = export.index;
        let channel = find_field(&export.properties, &|f| {
            f.span.is_some()
                && matches!(&f.value, PropertyValue::Struct { name, .. } if name == "MovieSceneFloatChannel")
        })
        .expect("a float channel")
        .clone();
        let keys = |entry: &PropertyEntry| -> Vec<(i64, f64, PropertyEntry)> {
            let PropertyValue::Struct { fields, .. } = &entry.value else {
                panic!("a struct");
            };
            fields
                .iter()
                .filter(|f| f.name == "Keys")
                .filter_map(|key| match &key.value {
                    PropertyValue::Struct { fields, .. } => {
                        match (&fields[0].value, &fields[1].value) {
                            (
                                PropertyValue::Int { value: frame },
                                PropertyValue::Float { value },
                            ) => Some((*frame, *value, fields[0].clone())),
                            _ => None,
                        }
                    }
                    _ => None,
                })
                .collect()
        };
        let was = keys(&channel);
        assert!(was.len() >= 3, "{} keys", was.len());
        let past = ((was[1].0 + was[2].0) / 2) as i32;
        assert!(i64::from(past) > was[1].0 && i64::from(past) < was[2].0);
        let offset = channel.span.expect("span").0;
        let (_, after) = fixture.apply_changes(PackageEdits {
            keys: vec![KeyEdit {
                offset,
                expect_name: channel.name.clone(),
                expect_element: channel.element,
                op: KeyOp::Move {
                    index: 0,
                    time: past,
                },
            }],
            ..Default::default()
        });
        let export = export_at(&after, at);
        assert_complete(export);
        let now = find_field(&export.properties, &|f| {
            f.name == channel.name && f.element == channel.element && f.span.is_some()
        })
        .expect("the channel after");
        let moved = keys(now);
        let mut expected: Vec<i64> = was.iter().skip(1).map(|(frame, _, _)| *frame).collect();
        expected.push(i64::from(past));
        expected.sort_unstable();
        assert_eq!(
            moved.iter().map(|(frame, _, _)| *frame).collect::<Vec<_>>(),
            expected
        );
        let landed = moved
            .iter()
            .find(|(frame, _, _)| *frame == i64::from(past))
            .expect("the moved key");
        assert!(
            (landed.1 - was[0].1).abs() < 1e-6,
            "the value travelled: {} became {}",
            was[0].1,
            landed.1
        );

        let within = ((was[0].0 + was[1].0) / 2) as i32;
        assert!(i64::from(within) > was[0].0 && i64::from(within) < was[1].0);
        let (_, retimed) = fixture.apply(vec![edit_of(
            &was[1].2,
            EditOp::Set {
                text: within.to_string(),
            },
        )]);
        let export = export_at(&retimed, at);
        assert_complete(export);
        let now = find_field(&export.properties, &|f| {
            f.name == channel.name && f.element == channel.element && f.span.is_some()
        })
        .expect("the channel after");
        let mut expected: Vec<i64> = was.iter().map(|(frame, _, _)| *frame).collect();
        expected[1] = i64::from(within);
        assert_eq!(
            keys(now)
                .iter()
                .map(|(frame, _, _)| *frame)
                .collect::<Vec<_>>(),
            expected
        );

        let err = rivals_uasset::patch_values(
            &fixture.bundle(),
            &before,
            &[edit_of(
                &was[1].2,
                EditOp::Set {
                    text: (was[2].0 + 1).to_string(),
                },
            )],
            None,
        )
        .err()
        .expect("a frame past its neighbour is refused");
        assert!(err.contains("out of order"), "{err}");
    }

    /// Pointing an element of an object array at another export of the package promises the
    /// loader that export first: the pointing export gains a create-before-serialize edge, as a
    /// scalar object edit already does.
    #[test]
    fn an_object_element_set_gains_a_create_before_serialize_edge() {
        let Some(fixture) = Fixture::open(REDIRECT_CUE) else {
            return;
        };
        let before = fixture.parse();
        let header = rivals_uasset::read_header(&fixture.bundle()).expect("header");
        let created_before = |header: &retoc::legacy_asset::FLegacyPackageHeader, owner: usize| {
            let export = &header.exports[owner];
            let first = usize::try_from(export.first_export_dependency_index).unwrap_or(0);
            let skip = usize::try_from(export.serialize_before_serialize_dependencies).unwrap_or(0);
            let count = usize::try_from(export.create_before_serialize_dependencies).unwrap_or(0);
            header.preload_dependencies[first + skip..first + skip + count]
                .iter()
                .map(|dep| dep.index)
                .collect::<Vec<i32>>()
        };
        let mut found = None;
        'exports: for export in &before.exports {
            if !matches!(export.status, ExportStatus::Complete) {
                continue;
            }
            for field in &export.properties {
                let PropertyValue::Array { items } = &field.value else {
                    continue;
                };
                if field.span.is_none()
                    || !items.iter().all(
                        |item| matches!(item, PropertyValue::Object { index, .. } if *index > 0),
                    )
                    || items.is_empty()
                {
                    continue;
                }
                let held = created_before(&header, export.index as usize);
                let target = before.exports.iter().find(|candidate| {
                    candidate.index != export.index
                        && !held.contains(&(candidate.index as i32 + 1))
                        && matches!(candidate.status, ExportStatus::Complete)
                        && !candidate.path.is_empty()
                });
                if let Some(target) = target {
                    found = Some((
                        export.index,
                        field.clone(),
                        target.index,
                        target.path.clone(),
                    ));
                    break 'exports;
                }
            }
        }
        let (owner, field, target, path) = found.expect("an object array and an unlinked export");
        let (patched, after) = fixture.apply(vec![edit_of(
            &field,
            EditOp::SetElement {
                index: 0,
                text: path,
            },
        )]);
        assert!(
            matches!(export_at(&after, owner).status, ExportStatus::Complete),
            "{:?}",
            export_at(&after, owner).status
        );
        let patched_header = rivals_uasset::read_header(&AssetBundle {
            asset: &patched.asset,
            exports: &patched.exports,
        })
        .expect("patched header");
        assert!(
            created_before(&patched_header, owner as usize).contains(&(target as i32 + 1)),
            "export {owner} now creates export {target} before it serializes"
        );
    }

    /// A map keyed by a struct grows by the key type's default, which reads back as a None-named
    /// variable; the same default asked for twice in one save is refused.
    #[test]
    fn a_struct_keyed_map_grows_by_its_default_key_once() {
        let Some(fixture) = Fixture::open(GPU_SCRIPT) else {
            return;
        };
        let before = fixture.parse();
        let keyed_by_struct = |f: &PropertyEntry| {
            let PropertyValue::Map { entries } = &f.value else {
                return false;
            };
            f.span.is_some()
                && entries.first().is_some_and(|pair| {
                    matches!(&pair.key, PropertyValue::Struct { name, .. } if name.starts_with("NiagaraVariable"))
                })
        };
        let (export_index, field) = before
            .exports
            .iter()
            .filter(|export| {
                !matches!(
                    export.status,
                    ExportStatus::Failed { .. } | ExportStatus::Partial { .. }
                )
            })
            .find_map(|export| {
                find_field(&export.properties, &keyed_by_struct).map(|f| (export.index, f.clone()))
            })
            .expect("a map keyed by Niagara variables");
        let PropertyValue::Map { entries } = &field.value else {
            unreachable!()
        };
        let insert = || {
            edit_of(
                &field,
                EditOp::Insert {
                    index: entries.len() as u32,
                    key: None,
                },
            )
        };
        let (patched, after) = fixture.apply(vec![insert()]);
        let done = &patched.applied[0];
        let export = export_at(&after, export_index);
        assert!(
            !matches!(
                export.status,
                ExportStatus::Failed { .. } | ExportStatus::Partial { .. }
            ),
            "{:?}",
            export.status
        );
        let now = find_field(&export.properties, &|f| {
            f.name == field.name && f.span.is_some_and(|(s, _)| s == done.offset_after)
        })
        .expect("the grown map");
        let PropertyValue::Map { entries: grown } = &now.value else {
            panic!("{:?}", now.value);
        };
        assert_eq!(grown.len(), entries.len() + 1);
        let PropertyValue::Struct { fields, .. } = &grown[grown.len() - 1].key else {
            panic!("{:?}", grown[grown.len() - 1].key);
        };
        assert!(
            matches!(&fields[0].value, PropertyValue::Name { value } if value == "None"),
            "{:?}",
            fields[0].value
        );

        let err =
            rivals_uasset::patch_values(&fixture.bundle(), &before, &[insert(), insert()], None)
                .err()
                .expect("the default key twice is refused");
        assert!(err.contains("twice"), "{err}");
    }

    /// A channel's keys are two bulk arrays: a key added between two others lands in both at the
    /// same position with the value it was given, a removed key leaves both, and the section reads
    /// through as before.
    #[test]
    fn a_channel_key_can_be_added_between_two_and_another_removed() {
        let Some(fixture) = Fixture::open(FLOAT_SECTION) else {
            return;
        };
        let before = fixture.parse();
        let export = export_of_class(&before, "MovieSceneFloatSection");
        let at = export.index;
        assert_complete(export);
        let channel = find_field(&export.properties, &|f| {
            f.span.is_some()
                && matches!(&f.value, PropertyValue::Struct { name, .. } if name == "MovieSceneFloatChannel")
        })
        .expect("a float channel")
        .clone();
        let frames = |entry: &PropertyEntry| -> Vec<i64> {
            let PropertyValue::Struct { fields, .. } = &entry.value else {
                panic!("a struct");
            };
            fields
                .iter()
                .filter(|f| f.name == "Keys")
                .filter_map(|key| match &key.value {
                    PropertyValue::Struct { fields, .. } => match fields[0].value {
                        PropertyValue::Int { value } => Some(value),
                        _ => None,
                    },
                    _ => None,
                })
                .collect()
        };
        let was = frames(&channel);
        assert!(was.len() >= 3, "{was:?}");
        let between = ((was[0] + was[1]) / 2) as i32;
        assert!(!was.contains(&i64::from(between)));
        let offset = channel.span.expect("span").0;
        let edit = |op: KeyOp| KeyEdit {
            offset,
            expect_name: channel.name.clone(),
            expect_element: channel.element,
            op,
        };
        let (_, after) = fixture.apply_changes(PackageEdits {
            keys: vec![
                edit(KeyOp::Add {
                    time: between,
                    value: 0.375,
                }),
                edit(KeyOp::Remove {
                    index: (was.len() - 1) as u32,
                }),
            ],
            ..Default::default()
        });
        let export = export_at(&after, at);
        assert_complete(export);
        let now = find_field(&export.properties, &|f| {
            f.name == channel.name
                && f.element == channel.element
                && matches!(&f.value, PropertyValue::Struct { name, .. } if name == "MovieSceneFloatChannel")
        })
        .expect("the channel after");
        let mut expected = was.clone();
        expected.pop();
        expected.push(i64::from(between));
        expected.sort_unstable();
        assert_eq!(frames(now), expected);
        let PropertyValue::Struct { fields, .. } = &now.value else {
            unreachable!()
        };
        let added = fields
            .iter()
            .find(|f| {
                f.name == "Keys"
                    && matches!(&f.value, PropertyValue::Struct { fields, .. }
                        if matches!(fields[0].value, PropertyValue::Int { value } if value == i64::from(between)))
            })
            .expect("the added key");
        let PropertyValue::Struct { fields: key, .. } = &added.value else {
            unreachable!()
        };
        assert!(
            matches!(key[1].value, PropertyValue::Float { value } if (value - 0.375).abs() < 1e-6),
            "{}",
            key[1].value.summary()
        );
    }

    /// The marker after a source rewrites in place: the lobby table's `Encrypt` comes off one entry
    /// and everything else reads as before.
    #[test]
    fn a_string_table_tag_can_be_cleared() {
        let Some(fixture) = Fixture::open(TAGGED_TABLE) else {
            return;
        };
        let before = fixture.parse();
        let was = before.exports[0]
            .string_table
            .clone()
            .expect("a string table");
        let index = was
            .entries
            .iter()
            .position(|entry| entry.tag == "Encrypt")
            .expect("a tagged entry");
        let (_, after) = fixture.apply_changes(PackageEdits {
            strings: vec![StringEdit {
                export: 0,
                op: StringOp::SetTag {
                    index: index as u32,
                    key: was.entries[index].key.clone(),
                    to: String::new(),
                },
            }],
            ..Default::default()
        });
        assert_complete(&after.exports[0]);
        let now = after.exports[0].string_table.as_ref().expect("the table");
        assert_eq!(now.entries[index].tag, "");
        assert_eq!(now.entries[index].source, was.entries[index].source);
        assert_eq!(now.entries.len(), was.entries.len());
    }

    /// Metadata edits in one save: an existing item rewritten and a new one appended on an entry
    /// with a record, a first item on an entry without one (a new record at the end of the map),
    /// and a rename that carries the record along. A second save removes an item, and the record
    /// with it when it was the last.
    #[test]
    fn a_string_table_metadata_map_edits_by_entry_and_id() {
        let Some(fixture) = Fixture::open(METADATA_TABLE) else {
            return;
        };
        let before = fixture.parse();
        let was = before.exports[0]
            .string_table
            .clone()
            .expect("a string table");
        let with = was
            .entries
            .iter()
            .position(|entry| !entry.metadata.is_empty())
            .expect("an entry with metadata");
        let without = was
            .entries
            .iter()
            .position(|entry| entry.metadata.is_empty())
            .expect("an entry without metadata");
        let (id, _) = was.entries[with].metadata[0].clone();
        let key_with = was.entries[with].key.clone();
        let key_without = was.entries[without].key.clone();
        let edit = |op: StringOp| StringEdit { export: 0, op };
        let (patched, after) = fixture.apply_changes(PackageEdits {
            strings: vec![
                edit(StringOp::SetMetaData {
                    index: with as u32,
                    key: key_with.clone(),
                    id: id.clone(),
                    to: "Changed by the toolkit".into(),
                }),
                edit(StringOp::SetMetaData {
                    index: with as u32,
                    key: key_with.clone(),
                    id: "RivalsNote".into(),
                    to: "appended".into(),
                }),
                edit(StringOp::SetKey {
                    index: with as u32,
                    key: key_with.clone(),
                    to: "ZZ_Renamed".into(),
                }),
                edit(StringOp::SetMetaData {
                    index: without as u32,
                    key: key_without.clone(),
                    id: "RivalsNote".into(),
                    to: "fresh".into(),
                }),
            ],
            ..Default::default()
        });
        assert_complete(&after.exports[0]);
        let now = after.exports[0].string_table.as_ref().expect("the table");
        assert_eq!(now.entries[with].key, "ZZ_Renamed");
        assert!(
            now.entries[with]
                .metadata
                .contains(&(id.clone(), "Changed by the toolkit".to_string())),
            "{:?}",
            now.entries[with].metadata
        );
        assert!(
            now.entries[with]
                .metadata
                .contains(&("RivalsNote".to_string(), "appended".to_string()))
        );
        assert_eq!(
            now.entries[without].metadata,
            vec![("RivalsNote".to_string(), "fresh".to_string())]
        );
        assert_eq!(now.loose_metadata.len(), was.loose_metadata.len());

        let reread = AssetBundle {
            asset: &patched.asset,
            exports: &patched.exports,
        };
        let again = rivals_uasset::patch_package(
            &reread,
            &after,
            &PackageEdits {
                strings: vec![
                    edit(StringOp::RemoveMetaData {
                        index: without as u32,
                        key: key_without.clone(),
                        id: "RivalsNote".into(),
                    }),
                    edit(StringOp::RemoveMetaData {
                        index: with as u32,
                        key: "ZZ_Renamed".into(),
                        id: "RivalsNote".into(),
                    }),
                ],
                ..Default::default()
            },
            Some(&fixture.schema),
        )
        .expect("second save");
        let final_bundle = AssetBundle {
            asset: &again.asset,
            exports: &again.exports,
        };
        let last = Fixture::parse_bundle(&final_bundle, &fixture.schema, &fixture.source());
        assert_complete(&last.exports[0]);
        let table = last.exports[0].string_table.as_ref().expect("the table");
        assert!(table.entries[without].metadata.is_empty());
        assert!(
            !table.entries[with]
                .metadata
                .iter()
                .any(|(name, _)| name == "RivalsNote")
        );
        assert!(
            table.entries[with]
                .metadata
                .contains(&(id, "Changed by the toolkit".to_string()))
        );
    }

    /// The word closing a string table's entries counts the metadata records after them.
    #[test]
    fn a_string_table_reads_the_metadata_map_after_its_entries() {
        let Some(fixture) = Fixture::open(METADATA_TABLE) else {
            return;
        };
        let parsed = fixture.parse();
        let table_export = export_where(&parsed, "carrying a string table", |e| {
            e.string_table.is_some()
        });
        assert_complete(table_export);
        let table = table_export.string_table.as_ref().expect("a string table");
        let with_metadata = table
            .entries
            .iter()
            .filter(|entry| !entry.metadata.is_empty())
            .count();
        assert_eq!(with_metadata + table.loose_metadata.len(), 3);
    }

    #[test]
    fn a_compiled_hierarchy_reads_its_sub_sequence_tree() {
        let Some(fixture) = Fixture::open(SUB_SEQUENCE_TREE) else {
            return;
        };
        let parsed = fixture.parse();
        let compiled = export_of_class(&parsed, "MovieSceneCompiledData");
        assert_complete(compiled);
        let counter = find_field(&compiled.properties, &|f| {
            f.name == "RootToSequenceWarpCounter"
        })
        .expect("RootToSequenceWarpCounter");
        assert!(
            matches!(&counter.value, PropertyValue::Struct { name, .. } if name == "MovieSceneWarpCounter")
        );
    }

    /// The string after an entry's source is a marker, not a metadata count: read as a count, the
    /// eight-character `Encrypt` sent the reader eight pairs into the next entries.
    #[test]
    fn a_string_table_reads_the_marker_after_each_source() {
        let Some(fixture) = Fixture::open(TAGGED_TABLE) else {
            return;
        };
        let parsed = fixture.parse();
        let table_export = export_where(&parsed, "carrying a string table", |e| {
            e.string_table.is_some()
        });
        assert_complete(table_export);
        let table = table_export.string_table.as_ref().expect("a string table");
        assert!(table.entries.iter().any(|entry| entry.tag == "Encrypt"));
        assert!(table.entries.iter().any(|entry| entry.tag.is_empty()));
    }

    /// A component whose class is a Blueprint class in another package: that package has to be
    /// read too, or the chain never roots at Object and the component reads as unknown. The
    /// opposite case, a class this build cannot resolve at all, is
    /// `an_instance_of_a_class_this_build_lacks_is_an_opaque_payload`.
    #[test]
    fn a_blueprint_class_with_a_blueprint_parent_reads_through_its_parent_package() {
        let Some(fixture) = Fixture::open(PARENT_CHAIN) else {
            return;
        };
        let parsed = fixture.parse();
        for class in [
            "WC_M2201BallGameTerminalBP_C",
            "LevelScopeCheckComponentBP_C",
        ] {
            assert_complete(export_of_class(&parsed, class));
        }
        assert_complete(export_named(&parsed, "Default__M2201BallGameTerminalBP_C"));
    }

    /// A widget Blueprint class under a plugin mount point: the import's class is a
    /// `WidgetBlueprintGeneratedClass`, and its package is found by name rather than by a table
    /// of mount points.
    #[test]
    fn a_widget_blueprint_class_from_a_plugin_is_synthesised() {
        let Some(fixture) = Fixture::open(PLUGIN_WIDGET) else {
            return;
        };
        let parsed = fixture.parse();
        let row = export_of_class(&parsed, "UI_MovieRenderPipelineInfoTableRow_C");
        assert_complete(row);
    }

    /// The parameter infos serialize themselves without headers. Past them a GPU script carries
    /// only its cooked shader map, which the class tail names, so the block reads through.
    #[test]
    fn a_gpu_compute_script_reads_its_data_interface_parameters() {
        let Some(fixture) = Fixture::open(GPU_SCRIPT) else {
            return;
        };
        let parsed = fixture.parse();
        let script = export_named(&parsed, "GPUComputeScript");
        assert!(
            matches!(&script.status, ExportStatus::Payload { kind, .. } if *kind == "particle system data"),
            "{:?}",
            script.status
        );
        let infos = find_field(&script.properties, &|f| f.name == "DataInterfaceParamInfo")
            .expect("DataInterfaceParamInfo");
        assert!(matches!(&infos.value, PropertyValue::Array { items } if items.len() == 17));
    }

    /// The GPU lists are containers like any array. A function list grows by a copy of a
    /// neighbour; an empty specifier list grows by the recipe default, a pair of None names. Both
    /// come back out, the export size following each time, while the script keeps reading through
    /// to its shader map.
    #[test]
    fn gpu_function_and_specifier_lists_grow_and_shrink() {
        fn collect<'a>(fields: &'a [PropertyEntry], name: &str, out: &mut Vec<&'a PropertyEntry>) {
            for field in fields {
                if field.name == name {
                    out.push(field);
                }
                match &field.value {
                    PropertyValue::Struct { fields, .. } => collect(fields, name, out),
                    PropertyValue::Array { items } => {
                        for item in items {
                            if let PropertyValue::Struct { fields, .. } = item {
                                collect(fields, name, out);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        fn lists<'a>(
            parsed: &'a rivals_uasset::ParsedPackage,
            at: usize,
            name: &str,
        ) -> Vec<&'a PropertyEntry> {
            let mut out = Vec::new();
            collect(&parsed.exports[at].properties, name, &mut out);
            out
        }
        fn len_of(entry: &PropertyEntry) -> usize {
            match &entry.value {
                PropertyValue::Array { items } => items.len(),
                other => panic!("{}: {}", entry.name, other.summary()),
            }
        }

        let Some(fixture) = Fixture::open(GPU_SCRIPT) else {
            return;
        };
        let parsed = fixture.parse();
        let at = parsed
            .exports
            .iter()
            .position(|e| e.index == 151)
            .expect("export 151");
        let functions = lists(&parsed, at, "GeneratedFunctions");
        let specifiers = lists(&parsed, at, "Specifiers");
        let first = functions[0].clone();
        let last = specifiers[specifiers.len() - 1].clone();
        assert_eq!(len_of(&first), 1);
        assert_eq!(
            len_of(&last),
            0,
            "no function in this script has specifiers"
        );
        let PropertyValue::Array { items } = &first.value else {
            unreachable!()
        };
        let PropertyValue::Struct { fields, .. } = &items[0] else {
            panic!("a function");
        };
        let function_width =
            fields[fields.len() - 1].span.expect("span").1 - fields[0].span.expect("span").0;
        let original = fixture.loaded.exports_file_buffer.len();

        let (patched, after) = fixture.apply(vec![
            edit_of(
                &first,
                EditOp::Insert {
                    index: 0,
                    key: None,
                },
            ),
            edit_of(
                &last,
                EditOp::Insert {
                    index: 0,
                    key: None,
                },
            ),
        ]);
        assert_eq!(
            patched.exports.len(),
            original + function_width as usize + 16,
            "a copied function and a pair of None names"
        );
        assert!(
            matches!(&after.exports[at].status, ExportStatus::Payload { kind, .. } if *kind == "particle system data"),
            "{:?}",
            after.exports[at].status
        );
        let functions = lists(&after, at, "GeneratedFunctions");
        let specifiers = lists(&after, at, "Specifiers");
        assert_eq!(len_of(functions[0]), 2);
        assert_eq!(
            specifiers.len(),
            lists(&parsed, at, "Specifiers").len() + 1,
            "the copy brought its own list"
        );
        let grown = specifiers[specifiers.len() - 1];
        let PropertyValue::Array { items } = &grown.value else {
            unreachable!()
        };
        assert_eq!(items.len(), 1);
        let PropertyValue::Struct { fields, .. } = &items[0] else {
            panic!("a specifier");
        };
        assert!(
            fields
                .iter()
                .all(|f| matches!(&f.value, PropertyValue::Name { value } if value == "None"))
        );

        let reread = AssetBundle {
            asset: &patched.asset,
            exports: &patched.exports,
        };
        let again = rivals_uasset::patch_values(
            &reread,
            &after,
            &[
                edit_of(functions[0], EditOp::Remove { index: 1 }),
                edit_of(grown, EditOp::Remove { index: 0 }),
            ],
            None,
        )
        .expect("remove both");
        assert_eq!(again.exports.len(), original);
        let final_bundle = AssetBundle {
            asset: &again.asset,
            exports: &again.exports,
        };
        let back = Fixture::parse_bundle(&final_bundle, &fixture.schema, &fixture.source());
        assert_eq!(len_of(lists(&back, at, "GeneratedFunctions")[0]), 1);
        assert!(
            lists(&back, at, "Specifiers")
                .iter()
                .all(|l| len_of(l) == 0)
        );
    }

    /// The channel serializes itself as two bulk arrays that the reader zips into `Keys[i]`
    /// entries; each key value owns its bytes, so it edits in place like any scalar.
    #[test]
    fn a_float_channel_reads_its_keys_and_a_key_value_edits_in_place() {
        let Some(fixture) = Fixture::open(FLOAT_SECTION) else {
            return;
        };
        let parsed = fixture.parse();
        let section = export_of_class(&parsed, "MovieSceneFloatSection");
        let at = section.index;
        assert!(
            matches!(section.status, ExportStatus::Complete),
            "{:?}",
            section.status
        );
        let channel = section
            .properties
            .iter()
            .find(|p| p.name == "FloatCurve")
            .expect("FloatCurve");
        let PropertyValue::Struct { fields, .. } = &channel.value else {
            panic!("{:?}", channel.value);
        };
        let keys: Vec<_> = fields.iter().filter(|f| f.name == "Keys").collect();
        assert_eq!(keys.len(), 4);
        let PropertyValue::Struct { fields: key, .. } = &keys[1].value else {
            panic!("{:?}", keys[1].value);
        };
        let time = key.iter().find(|f| f.name == "Time").expect("Time");
        assert!(matches!(time.value, PropertyValue::Int { value: 357417 }));
        let numerator = find_field(fields, &|f| f.name == "Numerator").expect("Numerator");
        assert!(matches!(
            numerator.value,
            PropertyValue::Int { value: 24000 }
        ));

        let value = key.iter().find(|f| f.name == "Value").expect("Value");
        let (_, reread) = fixture.apply(vec![edit_of(
            value,
            EditOp::Set {
                text: "0.75".into(),
            },
        )]);
        let section = export_at(&reread, at);
        assert!(
            matches!(section.status, ExportStatus::Complete),
            "{:?}",
            section.status
        );
        let edited = find_field(&section.properties, &|f| {
            f.name == "Value" && f.span == value.span
        })
        .expect("the edited key");
        assert!(
            matches!(edited.value, PropertyValue::Float { value } if (value - 0.75).abs() < 1e-6),
            "{:?}",
            edited.value
        );
    }

    /// Transform sections store nine double channels as three static arrays of three.
    #[test]
    fn a_transform_section_reads_its_double_channels() {
        let Some(fixture) = Fixture::open(TRANSFORM_SECTION) else {
            return;
        };
        let parsed = fixture.parse();
        let section = export_of_class(&parsed, "MovieScene3DTransformSection");
        assert!(
            matches!(section.status, ExportStatus::Complete),
            "{:?}",
            section.status
        );
        let translation: Vec<_> = section
            .properties
            .iter()
            .filter(|p| p.name == "Translation")
            .collect();
        assert_eq!(translation.len(), 3);
        assert!(translation.iter().all(|p| matches!(
            &p.value,
            PropertyValue::Struct { name, .. } if name == "MovieSceneDoubleChannel"
        )));
    }

    /// Patch containers sort above the chunks they supersede, so a package a patch revised is read
    /// from the patch, and a mod saved from it carries the current content rather than the old.
    #[test]
    fn a_package_a_patch_revised_is_read_from_the_patch() {
        let Some(fixture) = Fixture::open(PATCHED_CUE) else {
            return;
        };
        let parsed = fixture.parse();
        let default_object = &parsed.exports[cdo(&parsed)];
        assert_eq!(default_object.class_name, "Cue_Summoner_Loop_10370301_BP_C");
        assert!(
            matches!(default_object.status, ExportStatus::Complete),
            "{:?}",
            default_object.status
        );
    }

    /// A cooked map opens with the keys it removes from its inherited defaults. Taking the first
    /// of them for the entry count derails the whole component.
    #[test]
    fn a_map_that_removes_inherited_keys_reads_past_them() {
        let Some(fixture) = Fixture::open(REDIRECT_CUE) else {
            return;
        };
        let parsed = fixture.parse();
        let components: Vec<&rivals_uasset::ParsedExport> = parsed
            .exports
            .iter()
            .filter(|e| e.class_name == "NiagaraComponent")
            .collect();
        assert!(components.len() >= 3, "{} found", components.len());
        for component in components {
            assert_complete(component);
        }
    }

    /// The base chunk's copy was cooked before the struct took the layout the mappings file
    /// describes, and a reader fitted to it misreads the current copy. With the patch admitted the
    /// current copy is what gets read, and it reads exactly as declared.
    #[test]
    fn input_mappings_are_read_from_the_patched_copy() {
        let Some(fixture) = Fixture::open(INPUT_CONTEXT) else {
            return;
        };
        let parsed = fixture.parse();
        let context = &parsed.exports[0];
        assert!(
            matches!(context.status, ExportStatus::Complete),
            "{:?}",
            context.status
        );
        let mappings = context
            .properties
            .iter()
            .find(|p| p.name == "Mappings")
            .expect("Mappings");
        let PropertyValue::Array { items } = &mappings.value else {
            panic!("{:?}", mappings.value);
        };
        assert_eq!(items.len(), 4);
        for item in items {
            let PropertyValue::Struct { fields, .. } = item else {
                panic!("{item:?}");
            };
            let key = fields.iter().find(|f| f.name == "Key").expect("Key");
            assert!(matches!(&key.value, PropertyValue::Struct { fields, .. }
                if fields.iter().any(|f| f.name == "KeyName" && stored(f))));
        }
    }

    /// A name that does not belong to the offset means the caller is working from a stale read.
    #[test]
    fn an_offset_whose_name_does_not_match_is_refused() {
        let Some(fixture) = Fixture::open(STRINGS) else {
            return;
        };
        let before = fixture.parse();
        let field = field_where(&before, "is a stored string", |f| {
            matches!(&f.value, PropertyValue::Str { .. }) && stored(f)
        });
        let mut edit = edit_of(&field, EditOp::Set { text: "x".into() });
        edit.expect_name = "NoSuchProperty".into();
        let error = match preview_edits(&fixture.request(vec![edit]), Some(&fixture.schema)) {
            Ok(_) => panic!("a stale request should be refused, not written"),
            Err(error) => error,
        };
        assert!(error.contains("NoSuchProperty"), "{error}");
    }
}
