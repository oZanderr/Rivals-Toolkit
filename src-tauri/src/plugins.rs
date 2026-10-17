//! Tauri commands over the ASI plugin and loader management in `rivals_core::plugins`, tagging the files the signature bypass owns.

use std::path::{Path, PathBuf};

use rivals_core::plugins::{self, Loader, Plugin, PluginInstall};
use serde::Serialize;

use crate::game_status;
use crate::mods::{BulkOpResult, BypassRole, bypass_role};

#[derive(Serialize)]
pub(crate) struct Tagged<T> {
    #[serde(flatten)]
    entry: T,
    bypass: Option<BypassRole>,
}

#[derive(Serialize)]
pub(crate) struct PluginsStatus {
    win64_dir: String,
    plugins_dir: String,
    win64_exists: bool,
    plugins_dir_exists: bool,
    loaders: Vec<Tagged<Loader>>,
    plugins: Vec<Tagged<Plugin>>,
}

fn guard() -> Result<(), String> {
    if game_status::should_block_for_game() {
        return Err(game_status::game_running_error());
    }
    Ok(())
}

fn paths_of(paths: &[String]) -> Vec<PathBuf> {
    paths.iter().map(PathBuf::from).collect()
}

#[tauri::command]
pub(crate) async fn get_plugins_status(game_root: String) -> Result<PluginsStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let status = plugins::scan(&game_root);
        let tag = |path: &str| bypass_role(Path::new(path));
        PluginsStatus {
            win64_dir: status.win64_dir,
            plugins_dir: status.plugins_dir,
            win64_exists: status.win64_exists,
            plugins_dir_exists: status.plugins_dir_exists,
            loaders: status
                .loaders
                .into_iter()
                .map(|entry| Tagged {
                    bypass: tag(&entry.path),
                    entry,
                })
                .collect(),
            plugins: status
                .plugins
                .into_iter()
                .map(|entry| Tagged {
                    bypass: tag(&entry.path),
                    entry,
                })
                .collect(),
        }
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) fn set_plugin_enabled(
    game_root: String,
    path: String,
    enabled: bool,
) -> Result<(), String> {
    guard()?;
    plugins::set_enabled(&game_root, Path::new(&path), enabled).map(|_| ())
}

#[tauri::command]
pub(crate) fn set_plugins_enabled(
    game_root: String,
    paths: Vec<String>,
    enabled: bool,
) -> Result<BulkOpResult, String> {
    guard()?;
    Ok(plugins::set_enabled_many(
        &game_root,
        &paths_of(&paths),
        enabled,
    ))
}

#[tauri::command]
pub(crate) fn delete_plugins(
    game_root: String,
    paths: Vec<String>,
    with_companions: bool,
) -> Result<BulkOpResult, String> {
    guard()?;
    Ok(plugins::delete_many(
        &game_root,
        &paths_of(&paths),
        with_companions,
    ))
}

#[tauri::command]
pub(crate) async fn install_plugins(
    game_root: String,
    source_paths: Vec<String>,
    replace: bool,
) -> Result<Vec<PluginInstall>, String> {
    guard()?;
    tauri::async_runtime::spawn_blocking(move || {
        plugins::install(&game_root, &paths_of(&source_paths), replace)
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) fn install_asi_loader(game_root: String) -> Result<String, String> {
    guard()?;
    crate::mods::install_asi_loader(&game_root)
}
