//! Tauri commands over core's textures: a texture's details, preview and image file, and the
//! batches that save or replace the textures of many packages at once.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use rivals_core::asset_edit;
use rivals_core::image_file::ImageKind;
use rivals_core::mappings;
use rivals_core::texture::{self, Quality, SavedImage, TextureInfo};
use rivals_core::texture_batch::{
    self, BatchPhase, TextureImports, TextureSource, TexturesIn, TexturesOut,
};

use crate::asset_view::{configured_usmap, read_request};
use crate::settings::SettingsState;

/// How large a preview's mip may be on a side.
const PREVIEW_SIDE: u32 = 512;

static CANCEL: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Serialize)]
struct BatchProgress {
    phase: BatchPhase,
    current: usize,
    total: usize,
}

fn schema(usmap: Option<&str>) -> Option<std::sync::Arc<rivals_uasset::Mappings>> {
    mappings::resolve(None, usmap)
        .and_then(|path| mappings::load(&path))
        .ok()
}

#[tauri::command]
pub(crate) async fn texture_info(
    state: State<'_, SettingsState>,
    game_root: String,
    container: String,
    entry: String,
    export: Option<u32>,
) -> Result<TextureInfo, String> {
    let usmap = configured_usmap(&state);
    tauri::async_runtime::spawn_blocking(move || {
        texture::texture_info(
            &read_request(&game_root, &container, &entry),
            schema(usmap.as_deref()).as_deref(),
            export,
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

/// A PNG of the texture, its bytes as they are rather than a JSON array of them.
#[tauri::command]
pub(crate) async fn texture_preview(
    state: State<'_, SettingsState>,
    game_root: String,
    container: String,
    entry: String,
    export: Option<u32>,
) -> Result<tauri::ipc::Response, String> {
    let usmap = configured_usmap(&state);
    let png = tauri::async_runtime::spawn_blocking(move || {
        texture::preview_png(
            &read_request(&game_root, &container, &entry),
            schema(usmap.as_deref()).as_deref(),
            export,
            PREVIEW_SIDE,
        )
    })
    .await
    .map_err(|e| e.to_string())??;
    Ok(tauri::ipc::Response::new(png))
}

/// Saves a mip of the texture at `path`, as the kind of image its extension names.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn export_texture(
    state: State<'_, SettingsState>,
    game_root: String,
    container: String,
    entry: String,
    export: Option<u32>,
    mip: Option<usize>,
    path: String,
) -> Result<SavedImage, String> {
    let usmap = configured_usmap(&state);
    tauri::async_runtime::spawn_blocking(move || {
        texture::save_image(
            &read_request(&game_root, &container, &entry),
            schema(usmap.as_deref()).as_deref(),
            export,
            mip,
            Path::new(&path),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

fn report_progress(app: &AppHandle) -> impl Fn(BatchPhase, usize, usize) + Sync + '_ {
    move |phase, current, total| {
        let _ = app.emit(
            "texture-batch-progress",
            BatchProgress {
                phase,
                current,
                total,
            },
        );
    }
}

/// Saves the textures of every package in `container` the filters keep under `out`, reporting
/// `texture-batch-progress` on the way. A cancelled batch keeps what it saved.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn export_textures(
    app: AppHandle,
    state: State<'_, SettingsState>,
    game_root: String,
    container: String,
    filters: Vec<String>,
    out: String,
    kind: ImageKind,
    mip: Option<usize>,
) -> Result<TexturesOut, String> {
    let usmap = configured_usmap(&state);
    tauri::async_runtime::spawn_blocking(move || {
        CANCEL.store(false, Ordering::Relaxed);
        let schema = schema(usmap.as_deref());
        crate::concurrency::POOL.install(|| {
            texture_batch::export_textures(
                &TextureSource {
                    game_root: &game_root,
                    container: &container,
                    whole_game: false,
                    filters: &filters,
                },
                schema.as_deref(),
                Path::new(&out),
                kind,
                mip,
                &CANCEL,
                &report_progress(&app),
            )
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Gives the textures of `container`'s packages the images in `images`, each named for its
/// texture as an export names them, and writes them into `mod_name` in one rewrite, reporting
/// `texture-batch-progress` on the way. A cancelled batch leaves the mod as it was.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn import_textures(
    app: AppHandle,
    state: State<'_, SettingsState>,
    game_root: String,
    container: String,
    filters: Vec<String>,
    images: String,
    mod_name: String,
    quality: Quality,
    replace: bool,
) -> Result<TexturesIn, String> {
    if crate::game_status::should_block_for_game() {
        return Err(crate::game_status::game_running_error());
    }
    let usmap = configured_usmap(&state);
    let target = state.lock().ok().and_then(|s| s.asset_save_target);
    tauri::async_runtime::spawn_blocking(move || {
        CANCEL.store(false, Ordering::Relaxed);
        let schema = schema(usmap.as_deref());
        crate::concurrency::POOL.install(|| {
            texture_batch::import_textures(
                &TextureImports {
                    source: TextureSource {
                        game_root: &game_root,
                        container: &container,
                        whole_game: false,
                        filters: &filters,
                    },
                    images: Path::new(&images),
                    mod_name: &mod_name,
                    quality,
                    dry_run: false,
                },
                schema.as_deref(),
                &asset_edit::SaveOptions {
                    replace,
                    target: target.unwrap_or_default(),
                    ..Default::default()
                },
                &CANCEL,
                &report_progress(&app),
            )
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) fn cancel_texture_batch() {
    CANCEL.store(true, Ordering::Relaxed);
}
