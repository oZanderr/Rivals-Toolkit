//! Tauri commands for the import index that names the packages an export removal would break, and
//! for checking that a path an import is pointed at exists.

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use rivals_core::import_index::{self, ImportIndexStatus, Importers};
use rivals_core::object_check::{self, Existence};

#[derive(Clone, Serialize)]
struct ImportIndexProgress {
    current: usize,
    total: usize,
}

#[tauri::command]
pub(crate) async fn import_index_status(game_root: String) -> Result<ImportIndexStatus, String> {
    tauri::async_runtime::spawn_blocking(move || import_index::status(&game_root))
        .await
        .map_err(|e| e.to_string())?
}

/// Walks every package once, reporting `import-index-progress` on the way, and caches the result.
#[tauri::command]
pub(crate) async fn build_import_index(
    app: AppHandle,
    game_root: String,
) -> Result<ImportIndexStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        import_index::build(&game_root, &mut |current, total| {
            let _ = app.emit(
                "import-index-progress",
                ImportIndexProgress { current, total },
            );
        })?;
        import_index::status(&game_root)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn importers_of(
    game_root: String,
    paths: Vec<String>,
) -> Result<Vec<Importers>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let index = import_index::load(&game_root)?
            .ok_or("no import index has been built for this game yet")?;
        Ok(paths.iter().map(|path| index.importers_of(path)).collect())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Whether an object path names something the game or an enabled mod has, for a path typed into
/// an import before it is saved.
#[tauri::command]
pub(crate) async fn check_object_path(
    game_root: String,
    container: String,
    path: String,
) -> Result<Existence, String> {
    tauri::async_runtime::spawn_blocking(move || {
        object_check::object_exists(&game_root, &container, &path)
    })
    .await
    .map_err(|e| e.to_string())
}
