//! Tauri commands for reading the decoded contents of a .uasset from a pak or IoStore container.

use serde::Serialize;
use tauri::State;

use rivals_core::asset::{self, AssetSource};
use rivals_core::asset_edit::{self, AssetEditRequest};
use rivals_core::mappings;
use rivals_core::schema_synth::{self, PackageSource};
use rivals_uasset::{AssetBundle, HexRow, PackageEdits, ParsedPackage, RemovalPlan};

use crate::settings::SettingsState;

#[derive(Serialize)]
pub(crate) struct MappingsStatus {
    pub path: Option<String>,
    pub loaded: bool,
    pub struct_count: usize,
    pub enum_count: usize,
    /// Why no mappings could be used, so the UI can explain rather than just disable itself.
    pub error: Option<String>,
}

fn configured_usmap(state: &State<'_, SettingsState>) -> Option<String> {
    state.lock().ok().and_then(|s| s.usmap_path.clone())
}

#[derive(Serialize)]
pub(crate) struct EnumOption {
    pub value: i64,
    pub name: String,
}

/// The enumerators of one enum in value order, for a picker; empty when the mappings do not know
/// the enum, in which case the cell stays free text.
#[tauri::command]
pub(crate) fn enum_options(
    state: State<'_, SettingsState>,
    enum_type: String,
) -> Result<Vec<EnumOption>, String> {
    let path = mappings::resolve(None, configured_usmap(&state).as_deref())?;
    let schema = mappings::load(&path)?;
    Ok(schema
        .enumerators(&enum_type)
        .into_iter()
        .map(|(value, name)| EnumOption { value, name })
        .collect())
}

/// An empty container means the entry is a path to a file on disk.
fn source_of(container: &str) -> AssetSource {
    if container.is_empty() {
        return AssetSource::Loose;
    }
    match std::path::Path::new(container)
        .extension()
        .and_then(|e| e.to_str())
    {
        Some("utoc") => AssetSource::Utoc,
        _ => AssetSource::Pak,
    }
}

/// Mappings are looked up but not required: a tagged package carries its own type information,
/// so an asset saved out of the editor opens with nothing configured.
fn parse(
    game_root: &str,
    container: &str,
    entry: &str,
    usmap: Option<String>,
) -> Result<ParsedPackage, String> {
    let schema = mappings::resolve(None, usmap.as_deref())
        .and_then(|path| mappings::load(&path))
        .ok();
    let kind = source_of(container);
    let bundle = asset::load_bundle(game_root, container, entry, kind)?;
    schema_synth::parse_package_opts(
        &AssetBundle {
            asset: &bundle.asset_file_buffer,
            exports: &bundle.exports_file_buffer,
        },
        schema.as_deref(),
        &PackageSource {
            game_root,
            container,
            entry,
            kind,
        },
        // Inherited values are shown so they can be given one of their own.
        rivals_uasset::ParseOptions {
            declared_slots: true,
            ..Default::default()
        },
    )
}

/// One export's raw bytes, with what the reader made of them.
#[derive(Serialize)]
pub(crate) struct BytesView {
    rows: Vec<HexRow>,
    base: u64,
    size: u64,
    /// Where parsing stopped, when it did not reach the end.
    stopped_at: Option<u64>,
    /// The byte run each property consumed, so the viewer can show which is which.
    ranges: Vec<ByteRange>,
}

#[derive(Serialize)]
pub(crate) struct ByteRange {
    name: String,
    kind: String,
    depth: u32,
    start: u64,
    end: u64,
}

/// Mappings are only needed for the property ranges, so the bytes still show without a .usmap.
fn bytes_view(
    game_root: &str,
    container: &str,
    entry: &str,
    export: u32,
    usmap: Option<String>,
) -> Result<BytesView, String> {
    let bundle = asset::load_bundle(game_root, container, entry, source_of(container))?;
    let bundle = AssetBundle {
        asset: &bundle.asset_file_buffer,
        exports: &bundle.exports_file_buffer,
    };
    let header = rivals_uasset::read_header(&bundle)?;
    let (bytes, offset) = rivals_uasset::export_bytes(&bundle, &header, export)?;
    let base = u64::try_from(offset).map_err(|_| "export has a negative offset")?;
    let end = base + bytes.len() as u64;

    let schema = mappings::resolve(None, usmap.as_deref())
        .and_then(|path| mappings::load(&path))
        .ok();
    let source = PackageSource {
        game_root,
        container,
        entry,
        kind: source_of(container),
    };
    let (stopped_at, ranges) =
        match schema_synth::parse_package_traced(&bundle, schema.as_deref(), &source) {
            Ok((parsed, trace)) => {
                let stopped = parsed
                    .exports
                    .iter()
                    .find(|e| e.index == export)
                    .and_then(|e| stop_offset(e, base));
                let ranges = trace
                    .into_iter()
                    .filter(|e| e.start >= base && e.start < end)
                    .map(|e| ByteRange {
                        name: e.name,
                        kind: e.kind.to_string(),
                        depth: e.depth,
                        start: e.start,
                        end: e.end,
                    })
                    .collect();
                (stopped, ranges)
            }
            Err(_) => (None, Vec::new()),
        };

    Ok(BytesView {
        rows: rivals_uasset::hex_rows(bytes, base),
        base,
        size: bytes.len() as u64,
        stopped_at,
        ranges,
    })
}

fn stop_offset(export: &rivals_uasset::ParsedExport, base: u64) -> Option<u64> {
    match &export.status {
        rivals_uasset::ExportStatus::Complete => None,
        rivals_uasset::ExportStatus::Payload { consumed, .. }
        | rivals_uasset::ExportStatus::Partial { consumed, .. } => Some(base + consumed),
        rivals_uasset::ExportStatus::Failed { .. } => None,
    }
}

#[tauri::command]
pub(crate) async fn inspect_asset(
    state: State<'_, SettingsState>,
    game_root: String,
    container: String,
    entry: String,
) -> Result<ParsedPackage, String> {
    let usmap = configured_usmap(&state);
    tauri::async_runtime::spawn_blocking(move || parse(&game_root, &container, &entry, usmap))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn export_bytes_view(
    state: State<'_, SettingsState>,
    game_root: String,
    container: String,
    entry: String,
    export: u32,
) -> Result<BytesView, String> {
    let usmap = configured_usmap(&state);
    tauri::async_runtime::spawn_blocking(move || {
        bytes_view(&game_root, &container, &entry, export, usmap)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// One export's bytecode, disassembled for reading.
#[derive(Serialize)]
pub(crate) struct ScriptView {
    lines: Vec<rivals_uasset::ScriptLine>,
    /// The events that enter this function, when it is a Blueprint's Ubergraph, by the offset
    /// each one starts at.
    entries: Vec<(u32, String)>,
    /// The function's parameters and locals, when the export is a function.
    signature: Option<rivals_uasset::FunctionSignature>,
    /// The signature written out, `Name(In: T) -> Out: T`.
    signature_text: Option<String>,
    /// The functions in this package that call this one, with each call's offset.
    callers: Vec<(String, u32)>,
    /// Whether the walk reached the end. A script that stopped is shown as far as it got.
    complete: bool,
    stopped: Option<String>,
    buffer_size: u32,
    storage_size: u32,
    statements: usize,
}

#[tauri::command]
pub(crate) async fn export_script_view(
    state: State<'_, SettingsState>,
    game_root: String,
    container: String,
    entry: String,
    export: u32,
) -> Result<ScriptView, String> {
    let usmap = configured_usmap(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let parsed = parse(&game_root, &container, &entry, usmap)?;
        let found = parsed
            .exports
            .iter()
            .find(|e| e.index == export)
            .ok_or_else(|| format!("no export {export}"))?;
        let script = found
            .script
            .as_ref()
            .ok_or_else(|| format!("{} carries no bytecode", found.object_name))?;
        let scripts = || {
            parsed
                .exports
                .iter()
                .filter_map(|e| Some((e.object_name.as_str(), e.script.as_ref()?)))
        };
        let entries = rivals_uasset::ubergraph_entries(scripts())
            .remove(&found.object_name)
            .unwrap_or_default();
        let callers = rivals_uasset::call_sites(scripts())
            .remove(&found.object_name)
            .unwrap_or_default();
        Ok(ScriptView {
            lines: rivals_uasset::script_lines(script),
            entries,
            signature_text: found
                .signature
                .as_ref()
                .map(|s| s.render(&found.object_name)),
            signature: found.signature.clone(),
            callers,
            complete: script.stopped.is_none(),
            stopped: script.stopped.as_ref().map(|stop| stop.reason.clone()),
            buffer_size: script.buffer_size,
            storage_size: script.storage_size,
            statements: script.statements.len(),
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The `.utoc` behind a container the Asset Manager has selected, which is named by its `.pak`.
fn mod_utoc(container: &str) -> Result<std::path::PathBuf, String> {
    let utoc = std::path::Path::new(container).with_extension("utoc");
    if utoc.is_file() {
        Ok(utoc)
    } else {
        Err(format!(
            "{} has no .utoc, so there is no IoStore content to read",
            utoc.display()
        ))
    }
}

/// What an IoStore mod ships and reaches for: see `rivals_core::mod_report`.
#[tauri::command]
pub(crate) async fn get_mod_report(
    state: State<'_, SettingsState>,
    game_root: String,
    container: String,
) -> Result<rivals_core::mod_report::ModReport, String> {
    let usmap = configured_usmap(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let utoc = mod_utoc(&container)?;
        let schema = mappings::resolve(None, usmap.as_deref())
            .and_then(|path| mappings::load(&path))
            .ok();
        rivals_core::mod_report::mod_report(&game_root, &utoc, schema.as_deref())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Where an IoStore mod's scripts and values name `query`: see `rivals_core::mod_search`.
#[tauri::command]
pub(crate) async fn search_mod(
    state: State<'_, SettingsState>,
    game_root: String,
    container: String,
    query: String,
) -> Result<rivals_core::mod_search::SearchResult, String> {
    let usmap = configured_usmap(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let utoc = mod_utoc(&container)?;
        let schema = mappings::resolve(None, usmap.as_deref())
            .and_then(|path| mappings::load(&path))
            .ok();
        rivals_core::mod_search::mod_search(&game_root, &utoc, schema.as_deref(), &query)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// A request that reads the package without changing it.
fn read_request<'a>(
    game_root: &'a str,
    container: &'a str,
    entry: &'a str,
) -> AssetEditRequest<'a> {
    AssetEditRequest {
        game_root,
        container,
        entry,
        kind: source_of(container),
        mod_name: "",
        changes: PackageEdits::default(),
    }
}

/// Writes the payload an export carries after its properties to `path`; the byte count comes back.
#[tauri::command]
pub(crate) async fn export_payload(
    state: State<'_, SettingsState>,
    game_root: String,
    container: String,
    entry: String,
    export: u32,
    path: String,
) -> Result<u64, String> {
    let usmap = configured_usmap(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let schema = mappings::resolve(None, usmap.as_deref())
            .and_then(|path| mappings::load(&path))
            .ok();
        let bytes = asset_edit::read_payload(
            &read_request(&game_root, &container, &entry),
            schema.as_deref(),
            export,
        )?;
        std::fs::write(&path, &bytes).map_err(|e| format!("Could not write {path}: {e}"))?;
        Ok(bytes.len() as u64)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Writes one bulk data resource's bytes to `path`; the byte count comes back.
#[tauri::command]
pub(crate) async fn export_bulk(
    game_root: String,
    container: String,
    entry: String,
    resource: u32,
    path: String,
) -> Result<u64, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let bytes = asset_edit::read_bulk(&read_request(&game_root, &container, &entry), resource)?;
        std::fs::write(&path, &bytes).map_err(|e| format!("Could not write {path}: {e}"))?;
        Ok(bytes.len() as u64)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// What a save did, so the UI can ask before an edited copy already in the mod is replaced.
#[derive(Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub(crate) enum SaveResult {
    /// `pak` is the mod pak's full path, so the edited copy can be opened from it.
    Written {
        message: String,
        pak: String,
        warnings: Vec<String>,
    },
    HoldsCopy {
        pak: String,
    },
}

fn save_result(outcome: asset_edit::SaveOutcome) -> SaveResult {
    match outcome {
        asset_edit::SaveOutcome::Written {
            message,
            pak,
            warnings,
        } => SaveResult::Written {
            message,
            pak: pak.to_string_lossy().into_owned(),
            warnings,
        },
        asset_edit::SaveOutcome::HoldsCopy { pak } => SaveResult::HoldsCopy { pak },
    }
}

/// The mod's own edited copy of the inspected asset, when the mod already carries one, so the
/// inspector can offer to open it and have edits build on it.
#[tauri::command]
pub(crate) async fn mod_copy_of(
    state: State<'_, SettingsState>,
    game_root: String,
    container: String,
    entry: String,
    mod_name: String,
    target: Option<asset_edit::SaveTarget>,
) -> Result<Option<String>, String> {
    let target = target.or_else(|| state.lock().ok().and_then(|s| s.asset_save_target));
    tauri::async_runtime::spawn_blocking(move || {
        if mod_name.trim().is_empty() {
            return Ok(None);
        }
        let copy = asset_edit::mod_copy_of(
            &AssetEditRequest {
                game_root: &game_root,
                container: &container,
                entry: &entry,
                kind: source_of(&container),
                mod_name: &mod_name,
                changes: Default::default(),
            },
            target.unwrap_or_default(),
        )?;
        Ok(copy.map(|path| path.to_string_lossy().into_owned()))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Writes changed values into a mod pak that overrides the asset. Nothing is written unless the
/// patched package reads back exactly as it was, bar the edited values.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn save_asset_edits(
    state: State<'_, SettingsState>,
    game_root: String,
    container: String,
    entry: String,
    mod_name: String,
    replace: Option<bool>,
    layer: Option<bool>,
    allow_drift: Option<bool>,
    target: Option<asset_edit::SaveTarget>,
    mut edits: asset_edit::json::EditList,
) -> Result<SaveResult, String> {
    edits.allow_drift = allow_drift.unwrap_or(false);
    if crate::game_status::should_block_for_game() {
        return Err(crate::game_status::game_running_error());
    }
    let usmap = configured_usmap(&state);
    let target = target.or_else(|| state.lock().ok().and_then(|s| s.asset_save_target));
    tauri::async_runtime::spawn_blocking(move || {
        let schema = mappings::resolve(None, usmap.as_deref())
            .and_then(|path| mappings::load(&path))
            .ok();
        // Payload and bulk files come from the picker as absolute paths, so nothing resolves
        // against a base directory here.
        let changes = edits.resolve(std::path::Path::new(""))?;
        let outcome = asset_edit::save_edits(
            &AssetEditRequest {
                game_root: &game_root,
                container: &container,
                entry: &entry,
                kind: source_of(&container),
                mod_name: &mod_name,
                changes,
            },
            schema.as_deref(),
            &asset_edit::SaveOptions {
                replace: replace.unwrap_or(false),
                layer: layer.unwrap_or(false),
                target: target.unwrap_or_default(),
                ..Default::default()
            },
        )?;
        Ok(save_result(outcome))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// What removing `exports` from the asset would do: the subobjects that go with them, the
/// references set to None, and what cannot be made safe. Nothing is written.
#[tauri::command]
pub(crate) async fn plan_export_removal(
    state: State<'_, SettingsState>,
    game_root: String,
    container: String,
    entry: String,
    exports: Vec<u32>,
) -> Result<RemovalPlan, String> {
    let usmap = configured_usmap(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let schema = mappings::resolve(None, usmap.as_deref())
            .and_then(|path| mappings::load(&path))
            .ok();
        asset_edit::plan_export_removal(
            &AssetEditRequest {
                game_root: &game_root,
                container: &container,
                entry: &entry,
                kind: source_of(&container),
                mod_name: "",
                changes: PackageEdits {
                    remove_exports: exports,
                    ..Default::default()
                },
            },
            schema.as_deref(),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

/// What dropping `imports` would do: the rows that go, the references cleared, and what blocks it.
/// Nothing is written.
#[tauri::command]
pub(crate) async fn plan_import_removal(
    state: State<'_, SettingsState>,
    game_root: String,
    container: String,
    entry: String,
    imports: Vec<u32>,
) -> Result<rivals_uasset::ImportRemovalPlan, String> {
    let usmap = configured_usmap(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let schema = mappings::resolve(None, usmap.as_deref())
            .and_then(|path| mappings::load(&path))
            .ok();
        asset_edit::plan_import_removal(
            &AssetEditRequest {
                game_root: &game_root,
                container: &container,
                entry: &entry,
                kind: source_of(&container),
                mod_name: "",
                changes: PackageEdits {
                    imports: imports
                        .into_iter()
                        .map(|import| rivals_uasset::ImportEdit::Remove { import })
                        .collect(),
                    ..Default::default()
                },
            },
            schema.as_deref(),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

/// What those export table edits would do: the paths that move, the packages that import them by a
/// hash the move changes, and what blocks them. `resets` names the exports the same save would
/// empty, which is what a retype needs. Nothing is written.
#[tauri::command]
pub(crate) async fn plan_export_edits(
    state: State<'_, SettingsState>,
    game_root: String,
    container: String,
    entry: String,
    edits: Vec<rivals_uasset::ExportEdit>,
    resets: Option<Vec<u32>>,
) -> Result<rivals_uasset::ExportEditPlan, String> {
    let usmap = configured_usmap(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let schema = mappings::resolve(None, usmap.as_deref())
            .and_then(|path| mappings::load(&path))
            .ok();
        asset_edit::plan_export_edits(
            &AssetEditRequest {
                game_root: &game_root,
                container: &container,
                entry: &entry,
                kind: source_of(&container),
                mod_name: "",
                changes: PackageEdits {
                    exports: edits,
                    reset_exports: resets.unwrap_or_default(),
                    ..Default::default()
                },
            },
            schema.as_deref(),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

/// What replacing those preload dependency runs would do. Nothing is written.
#[tauri::command]
pub(crate) async fn plan_dependency_edits(
    state: State<'_, SettingsState>,
    game_root: String,
    container: String,
    entry: String,
    edits: Vec<rivals_uasset::DependencyEdit>,
) -> Result<rivals_uasset::DependencyPlan, String> {
    let usmap = configured_usmap(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let schema = mappings::resolve(None, usmap.as_deref())
            .and_then(|path| mappings::load(&path))
            .ok();
        asset_edit::plan_dependency_edits(
            &AssetEditRequest {
                game_root: &game_root,
                container: &container,
                entry: &entry,
                kind: source_of(&container),
                mod_name: "",
                changes: PackageEdits {
                    dependencies: edits,
                    ..Default::default()
                },
            },
            schema.as_deref(),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

/// What copying an export in from another package would bring across, and what blocks it. Nothing
/// is written.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub(crate) async fn plan_export_copy(
    state: State<'_, SettingsState>,
    game_root: String,
    container: String,
    entry: String,
    from_container: String,
    from_entry: String,
    export: u32,
    into_outer: Option<u32>,
    name: String,
    into_level: Option<u32>,
) -> Result<rivals_uasset::CopyPlan, String> {
    let usmap = configured_usmap(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let schema = mappings::resolve(None, usmap.as_deref())
            .and_then(|path| mappings::load(&path))
            .ok();
        let from = asset_edit::CopyFrom {
            container: from_container,
            entry: from_entry,
        };
        asset_edit::plan_copy(
            &asset_edit::CopyRequest {
                game_root: &game_root,
                container: &container,
                entry: &entry,
                kind: source_of(&container),
                mod_name: "",
                sources: vec![from.clone()],
                copies: vec![rivals_uasset::CopyExport {
                    from: from.key(),
                    export,
                    into_outer,
                    name,
                    into_level,
                }],
            },
            schema.as_deref(),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Copies an export in from another package and writes the result, which is a save of its own.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub(crate) async fn save_export_copy(
    state: State<'_, SettingsState>,
    game_root: String,
    container: String,
    entry: String,
    from_container: String,
    from_entry: String,
    export: u32,
    into_outer: Option<u32>,
    name: String,
    into_level: Option<u32>,
    mod_name: String,
    replace: Option<bool>,
    layer: Option<bool>,
    target: Option<asset_edit::SaveTarget>,
) -> Result<SaveResult, String> {
    if crate::game_status::should_block_for_game() {
        return Err(crate::game_status::game_running_error());
    }
    let usmap = configured_usmap(&state);
    let target = target.or_else(|| state.lock().ok().and_then(|s| s.asset_save_target));
    tauri::async_runtime::spawn_blocking(move || {
        let schema = mappings::resolve(None, usmap.as_deref())
            .and_then(|path| mappings::load(&path))
            .ok();
        let from = asset_edit::CopyFrom {
            container: from_container,
            entry: from_entry,
        };
        let outcome = asset_edit::save_copy(
            &asset_edit::CopyRequest {
                game_root: &game_root,
                container: &container,
                entry: &entry,
                kind: source_of(&container),
                mod_name: &mod_name,
                sources: vec![from.clone()],
                copies: vec![rivals_uasset::CopyExport {
                    from: from.key(),
                    export,
                    into_outer,
                    name,
                    into_level,
                }],
            },
            schema.as_deref(),
            &asset_edit::SaveOptions {
                replace: replace.unwrap_or(false),
                layer: layer.unwrap_or(false),
                target: target.unwrap_or_default(),
                ..Default::default()
            },
        )?;
        Ok(save_result(outcome))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn get_mappings_status(
    state: State<'_, SettingsState>,
) -> Result<MappingsStatus, String> {
    let configured = configured_usmap(&state);
    tauri::async_runtime::spawn_blocking(move || {
        match mappings::resolve(None, configured.as_deref()) {
            Ok(path) => {
                let status = mappings::status(Some(&path));
                MappingsStatus {
                    path: Some(path.display().to_string()),
                    loaded: status.struct_count > 0,
                    struct_count: status.struct_count,
                    enum_count: status.enum_count,
                    error: (status.struct_count == 0)
                        .then(|| "the mappings file could not be parsed".to_string()),
                }
            }
            Err(error) => MappingsStatus {
                path: None,
                loaded: false,
                struct_count: 0,
                enum_count: 0,
                error: Some(error),
            },
        }
    })
    .await
    .map_err(|e| e.to_string())
}

/// Where asset edits go until the user picks a mod of their own.
const DEFAULT_MOD_NAME: &str = "AssetEdits";

#[tauri::command]
pub(crate) fn get_asset_mod_name(state: State<'_, SettingsState>) -> String {
    state
        .lock()
        .ok()
        .and_then(|s| s.asset_mod_name.clone())
        .unwrap_or_else(|| DEFAULT_MOD_NAME.to_string())
}

#[tauri::command]
pub(crate) fn set_asset_mod_name(
    state: State<'_, SettingsState>,
    name: String,
) -> Result<(), String> {
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    let name = name.trim();
    guard.asset_mod_name = (!name.is_empty()).then(|| name.to_string());
    guard.save()
}

/// What an asset edit is saved as, for the picker beside the mod name.
#[tauri::command]
pub(crate) fn get_asset_save_target(state: State<'_, SettingsState>) -> asset_edit::SaveTarget {
    state
        .lock()
        .ok()
        .and_then(|s| s.asset_save_target)
        .unwrap_or_default()
}

#[tauri::command]
pub(crate) fn set_asset_save_target(
    state: State<'_, SettingsState>,
    target: asset_edit::SaveTarget,
) -> Result<(), String> {
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    guard.asset_save_target = Some(target);
    guard.save()
}

#[tauri::command]
pub(crate) fn set_mappings_path(
    state: State<'_, SettingsState>,
    path: Option<String>,
) -> Result<(), String> {
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    guard.usmap_path = path.filter(|p| !p.is_empty());
    guard.save()
}

/// Set `RIVALS_GAME_ROOT` and `RIVALS_USMAP` to a real install to run this. Skipped otherwise.
#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod game_data_tests {
    use super::*;

    /// The viewer is only worth having if its offsets line up with the ones traces and failure
    /// messages quote, so the rows must start on file-offset boundaries and the property ranges
    /// must fall inside the export.
    #[test]
    fn a_bytes_view_labels_rows_with_file_offsets_and_ranges_inside_the_export() {
        let (Ok(root), Ok(usmap)) = (
            std::env::var("RIVALS_GAME_ROOT"),
            std::env::var("RIVALS_USMAP"),
        ) else {
            return;
        };
        let container = format!(
            "{}/MarvelGame/Marvel/Content/Paks/pakchunk0-Windows.utoc",
            root.replace('\\', "/")
        );
        // Mirrored as SYNTH_ROW_TABLE in rivals-core's fixture audit, which is what notices a game
        // patch removing it. A rivals-core test cannot scan this crate, so change both together.
        let entry =
            "Marvel/Content/Marvel/Data/DataTable/GameMode/2206/2206_UIHeroInfoTable.uasset";
        let view = bytes_view(&root, &container, entry, 0, Some(usmap)).expect("bytes view");

        assert!(view.size > 0);
        let first = view.rows.first().expect("a row");
        // The export's own offset need not be aligned, but the row it lands in must be.
        assert!(first.offset <= view.base);
        assert_eq!(first.offset % rivals_uasset::ROW_BYTES as u64, 0);
        assert_eq!(first.hex.split(' ').count(), rivals_uasset::ROW_BYTES);
        assert!(view.rows.len() >= view.size as usize / rivals_uasset::ROW_BYTES);

        // This table decodes cleanly now that its row struct is synthesised, so nothing stopped.
        assert_eq!(view.stopped_at, None);
        assert!(!view.ranges.is_empty(), "expected property ranges");
        let end = view.base + view.size;
        assert!(
            view.ranges
                .iter()
                .all(|r| r.start >= view.base && r.end <= end),
            "a range fell outside the export"
        );
    }
}
