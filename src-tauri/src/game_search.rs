//! Tauri commands for searching every script in the game, and its stored values when asked.

use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use rivals_core::game_search::{self, GameSearch, GameSearchResult, SearchPhase};
use rivals_core::mappings;
use rivals_core::mod_search::{HitKind, Query};

use crate::settings::SettingsState;

/// The most places one search lists. A broader query is better narrowed than scrolled.
const MAX_HITS: usize = 2000;

static CANCEL: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Serialize)]
struct GameSearchProgress {
    phase: SearchPhase,
    current: usize,
    total: usize,
}

/// Searches the game as it loads, the enabled mods included when `mods` is set, reporting
/// `game-search-progress` on the way. `kinds` narrows it to those kinds of term, every kind when
/// empty. A cancelled search returns what it found before it stopped.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn search_game(
    app: AppHandle,
    state: State<'_, SettingsState>,
    game_root: String,
    query: String,
    values: bool,
    filter: Option<String>,
    kinds: Vec<HitKind>,
    whole_word: bool,
    mods: bool,
) -> Result<GameSearchResult, String> {
    let usmap = state.lock().ok().and_then(|s| s.usmap_path.clone());
    tauri::async_runtime::spawn_blocking(move || {
        CANCEL.store(false, Ordering::Relaxed);
        let mappings = mappings::resolve(None, usmap.as_deref())
            .and_then(|path| mappings::load(&path))
            .map_err(|e| format!("Searching the game needs a .usmap mappings file: {e}"))?;
        let query = Query::new(&query, kinds, values)?.whole_word(whole_word);
        let filter = filter.as_deref().map(str::trim).filter(|f| !f.is_empty());
        crate::concurrency::POOL.install(|| {
            game_search::game_search(
                &game_root,
                &mappings,
                &GameSearch {
                    query: &query,
                    filter,
                    mods,
                    max_hits: Some(MAX_HITS),
                },
                &CANCEL,
                &|phase, current, total| {
                    let _ = app.emit(
                        "game-search-progress",
                        GameSearchProgress {
                            phase,
                            current,
                            total,
                        },
                    );
                },
            )
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) fn cancel_game_search() {
    CANCEL.store(true, Ordering::Relaxed);
}
