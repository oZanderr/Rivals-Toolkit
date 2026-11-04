//! Tauri commands over a mod's packages and the asset settings: its contents report and search, reverting or renaming a package, the mappings file and the mod a save goes into.

use serde::Serialize;
use tauri::State;

use rivals_core::asset::AssetSource;
use rivals_core::asset_edit::{self, AssetEditRequest};
use rivals_core::mappings;
use rivals_uasset::PackageEdits;

use crate::settings::SettingsState;

#[derive(Serialize)]
pub(crate) struct MappingsStatus {
    pub path: Option<String>,
    pub loaded: bool,
    pub struct_count: usize,
    pub enum_count: usize,
    /// Why no mappings could be used, so the UI can explain rather than just disable itself.
    pub error: Option<String>,
    /// The mappings predate the game's newest patch.
    pub warning: Option<String>,
}

pub(crate) fn configured_usmap(state: &State<'_, SettingsState>) -> Option<String> {
    state.lock().ok().and_then(|s| s.usmap_path.clone())
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

/// Takes one package back out of the mod `container` names, by its `.pak` or its `.utoc`, so the
/// game's own copy loads again. A mod left holding nothing is deleted.
#[tauri::command]
pub(crate) async fn revert_mod_asset(
    container: String,
    entry: String,
) -> Result<asset_edit::RevertOutcome, String> {
    if crate::game_status::should_block_for_game() {
        return Err(crate::game_status::game_running_error());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let pak = std::path::Path::new(&container).with_extension("pak");
        let utoc = pak.with_extension("utoc");
        let outcome = asset_edit::revert_asset(&pak, &entry, &Default::default())?;
        crate::pak::invalidate_list_caches(&[&pak, &utoc]);
        Ok(outcome)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Moves a package a mod added to another path inside that mod, in one container rewrite. The
/// message carries the save's warnings after it.
#[tauri::command]
pub(crate) async fn rename_mod_package(
    state: State<'_, SettingsState>,
    game_root: String,
    container: String,
    entry: String,
    to: String,
    rename_objects: Option<bool>,
) -> Result<String, String> {
    if crate::game_status::should_block_for_game() {
        return Err(crate::game_status::game_running_error());
    }
    let usmap = configured_usmap(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let schema = mappings::resolve(None, usmap.as_deref())
            .and_then(|path| mappings::load(&path))
            .ok();
        let mod_name = asset_edit::mod_name_of(std::path::Path::new(&container))?;
        let outcome = asset_edit::rename_mod_package(
            &AssetEditRequest {
                game_root: &game_root,
                container: &container,
                entry: &entry,
                kind: source_of(&container),
                mod_name: &mod_name,
                changes: PackageEdits {
                    save_as: Some(rivals_uasset::SaveAs {
                        package: to,
                        rename_objects: rename_objects.unwrap_or(true),
                    }),
                    ..Default::default()
                },
            },
            schema.as_deref(),
            &asset_edit::SaveOptions::default(),
        )?;
        let pak = std::path::Path::new(&container).with_extension("pak");
        crate::pak::invalidate_list_caches(&[&pak, &pak.with_extension("utoc")]);
        match outcome {
            asset_edit::SaveOutcome::Written {
                message, warnings, ..
            } => Ok(std::iter::once(message)
                .chain(warnings)
                .collect::<Vec<_>>()
                .join(". ")),
            asset_edit::SaveOutcome::HoldsCopy { pak } => Err(format!("{pak} already holds it")),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn get_mappings_status(
    state: State<'_, SettingsState>,
) -> Result<MappingsStatus, String> {
    let configured = configured_usmap(&state);
    let game_root = state.lock().ok().and_then(|s| s.game_path.clone());
    tauri::async_runtime::spawn_blocking(move || {
        match mappings::resolve(None, configured.as_deref()) {
            Ok(path) => {
                let status = mappings::status(Some(&path));
                MappingsStatus {
                    path: Some(path.display().to_string()),
                    loaded: status.error.is_none(),
                    struct_count: status.struct_count,
                    enum_count: status.enum_count,
                    error: status
                        .error
                        .map(|why| format!("the mappings file could not be read: {why}")),
                    warning: game_root
                        .as_deref()
                        .and_then(|root| mappings::drift_warning(root, &path)),
                }
            }
            Err(error) => MappingsStatus {
                path: None,
                loaded: false,
                struct_count: 0,
                enum_count: 0,
                error: Some(error),
                warning: None,
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

#[tauri::command]
pub(crate) fn set_mappings_path(
    state: State<'_, SettingsState>,
    path: Option<String>,
) -> Result<(), String> {
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    guard.usmap_path = path.filter(|p| !p.is_empty());
    guard.save()
}
