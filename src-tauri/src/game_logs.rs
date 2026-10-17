//! Tauri commands over the game log and crash report reader in `rivals_core::game_logs`, holding the open log so the viewer pages through it.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use rivals_core::game_logs::{
    self, CategoryCount, CrashReport, Culprit, KeyCheck, KeySource, LogAnalysis, LogFilter, LogKey,
    ModsContext, OpenedSession, Session, SessionRef, Verbosity,
};
use serde::Serialize;
use tauri::State;

use crate::game_status::game_pids;
use crate::mods::CachedLister;
use crate::settings::SettingsState;

/// A line past this is cut for display; the export keeps it whole.
const MAX_LINE_CHARS: usize = 4000;
/// The largest decoded log the clipboard is offered; past it, exporting to a file is the way.
const MAX_CLIPBOARD_BYTES: usize = 32 * 1024 * 1024;

static NEXT_SNAPSHOT: AtomicU64 = AtomicU64::new(1);

struct OpenLog {
    snapshot: u64,
    session: Arc<OpenedSession>,
    /// The lines the current filter keeps.
    rows: Vec<u32>,
}

/// The one log the viewer has open. A snapshot is fixed when it opens, so paging a log the game
/// is still writing never decodes it again; reopening takes the new lines.
#[derive(Default)]
pub(crate) struct GameLogCache(Mutex<Option<OpenLog>>);

impl GameLogCache {
    fn with<T>(&self, snapshot: u64, f: impl FnOnce(&mut OpenLog) -> T) -> Result<T, String> {
        let mut guard = self.0.lock().map_err(|e| e.to_string())?;
        match guard.as_mut() {
            Some(open) if open.snapshot == snapshot => Ok(f(open)),
            _ => Err("stale log snapshot".to_string()),
        }
    }
}

fn log_key(state: &State<'_, SettingsState>) -> Result<LogKey, String> {
    let guard = state.lock().map_err(|e| e.to_string())?;
    LogKey::resolve(guard.log_key.as_deref(), guard.log_iv.as_deref())
}

#[tauri::command]
pub(crate) async fn list_game_sessions(
    state: State<'_, SettingsState>,
) -> Result<Vec<Session>, String> {
    let key = log_key(&state)?;
    tauri::async_runtime::spawn_blocking(move || game_logs::list_sessions(&game_pids(), &key))
        .await
        .map_err(|e| e.to_string())
}

#[derive(Serialize)]
pub(crate) struct VerbosityCount {
    verbosity: Verbosity,
    count: u32,
}

#[derive(Serialize)]
pub(crate) struct LogOverview {
    snapshot: u64,
    name: String,
    log_path: Option<String>,
    crash_dir: Option<String>,
    line_count: usize,
    verbosity_counts: Vec<VerbosityCount>,
    categories: Vec<CategoryCount>,
    analysis: LogAnalysis,
    crash: Option<CrashReport>,
    culprit: Option<Culprit>,
    key_source: Option<KeySource>,
    key_error: Option<String>,
}

#[tauri::command]
pub(crate) async fn open_game_log(
    state: State<'_, SettingsState>,
    cache: State<'_, GameLogCache>,
    name: String,
    log_path: Option<String>,
    crash_dir: Option<String>,
    game_root: String,
) -> Result<LogOverview, String> {
    let key = log_key(&state)?;
    let session = SessionRef {
        name,
        log_path: log_path.map(PathBuf::from),
        crash_dir: crash_dir.map(PathBuf::from),
    };
    let opened = tauri::async_runtime::spawn_blocking(move || {
        let mods = (!game_root.is_empty()).then(|| ModsContext {
            game_root: &game_root,
            lister: &CachedLister,
        });
        game_logs::open(session, &key, &game_pids(), mods)
    })
    .await
    .map_err(|e| e.to_string())??;

    let snapshot = NEXT_SNAPSHOT.fetch_add(1, Ordering::Relaxed);
    let overview = LogOverview {
        snapshot,
        name: opened.session.name.clone(),
        log_path: path_string(opened.session.log_path.as_ref()),
        crash_dir: path_string(opened.session.crash_dir.as_ref()),
        line_count: opened.log.len(),
        verbosity_counts: opened
            .log
            .verbosity_counts()
            .into_iter()
            .map(|(verbosity, count)| VerbosityCount { verbosity, count })
            .collect(),
        categories: opened.log.category_counts(),
        analysis: opened.analysis.clone(),
        crash: opened.crash.clone(),
        culprit: opened.culprit.clone(),
        key_source: opened.key_source,
        key_error: opened.key_error.clone(),
    };
    let rows = (0..opened.log.len() as u32).collect();
    *cache.0.lock().map_err(|e| e.to_string())? = Some(OpenLog {
        snapshot,
        session: Arc::new(opened),
        rows,
    });
    Ok(overview)
}

fn path_string(path: Option<&PathBuf>) -> Option<String> {
    path.map(|p| p.to_string_lossy().into_owned())
}

/// Keeps the lines `filter` matches as the rows the viewer pages through, and says how many.
#[tauri::command]
pub(crate) async fn filter_game_log(
    cache: State<'_, GameLogCache>,
    snapshot: u64,
    filter: LogFilter,
) -> Result<u32, String> {
    let session = cache.with(snapshot, |open| open.session.clone())?;
    let rows = tauri::async_runtime::spawn_blocking(move || session.log.filter(&filter))
        .await
        .map_err(|e| e.to_string())??;
    let total = rows.len() as u32;
    cache.with(snapshot, |open| open.rows = rows)?;
    Ok(total)
}

#[derive(Serialize)]
pub(crate) struct LogLine {
    /// The line's index in the whole log.
    n: u32,
    verbosity: Verbosity,
    category: String,
    text: String,
    continuation: bool,
    trailer: bool,
    /// A trailer line the records already hold.
    repeat: bool,
}

#[tauri::command]
pub(crate) fn game_log_rows(
    cache: State<'_, GameLogCache>,
    snapshot: u64,
    start: u32,
    count: u32,
) -> Result<Vec<LogLine>, String> {
    cache.with(snapshot, |open| {
        let log = &open.session.log;
        let start = (start as usize).min(open.rows.len());
        let end = start.saturating_add(count as usize).min(open.rows.len());
        open.rows[start..end]
            .iter()
            .map(|&n| {
                let line = n as usize;
                let text = log.line(line);
                let text = match text.char_indices().nth(MAX_LINE_CHARS) {
                    Some((cut, _)) => format!("{}…", &text[..cut]),
                    None => text.to_string(),
                };
                LogLine {
                    n,
                    verbosity: log.verbosity(line),
                    category: log.category(line).to_string(),
                    text,
                    continuation: log.is_continuation(line),
                    trailer: log.is_trailer(line),
                    repeat: log.is_trailer_repeat(line),
                }
            })
            .collect()
    })
}

/// The row showing `line`, or the first row after it when the filter hides it.
#[tauri::command]
pub(crate) fn game_log_row_of(
    cache: State<'_, GameLogCache>,
    snapshot: u64,
    line: u32,
) -> Result<Option<u32>, String> {
    cache.with(snapshot, |open| {
        let at = open.rows.partition_point(|&n| n < line);
        let at = at.min(open.rows.len().saturating_sub(1));
        (!open.rows.is_empty()).then_some(at as u32)
    })
}

/// The next row, forward or back from `from`, whose line is one of `verbosities` and starts an
/// entry rather than continuing one.
#[tauri::command]
pub(crate) fn game_log_seek(
    cache: State<'_, GameLogCache>,
    snapshot: u64,
    from: i64,
    forward: bool,
    verbosities: Vec<Verbosity>,
) -> Result<Option<u32>, String> {
    cache.with(snapshot, |open| {
        let log = &open.session.log;
        let hit = |row: &usize| {
            let line = open.rows[*row] as usize;
            !log.is_continuation(line) && verbosities.contains(&log.verbosity(line))
        };
        let len = open.rows.len();
        let found = if forward {
            let start = usize::try_from(from.saturating_add(1)).unwrap_or(0);
            (start.min(len)..len).find(hit)
        } else {
            let end = usize::try_from(from).unwrap_or(0).min(len);
            (0..end).rev().find(hit)
        };
        found.map(|row| row as u32)
    })
}

/// The whole decoded log, for the clipboard.
#[tauri::command]
pub(crate) fn game_log_text(
    cache: State<'_, GameLogCache>,
    snapshot: u64,
) -> Result<String, String> {
    let session = cache.with(snapshot, |open| open.session.clone())?;
    let text = session.log.text();
    if text.len() > MAX_CLIPBOARD_BYTES {
        return Err(format!(
            "This log is {} MB decoded, too large for the clipboard. Export it instead.",
            text.len() / (1024 * 1024)
        ));
    }
    Ok(text.to_string())
}

#[tauri::command]
pub(crate) async fn export_game_log(
    cache: State<'_, GameLogCache>,
    snapshot: u64,
    dest: String,
) -> Result<(), String> {
    let session = cache.with(snapshot, |open| open.session.clone())?;
    tauri::async_runtime::spawn_blocking(move || {
        std::fs::write(&dest, session.log.text()).map_err(|e| format!("{dest}: {e}"))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(Serialize)]
pub(crate) struct LogKeyOverride {
    key: String,
    iv: String,
}

#[tauri::command]
pub(crate) fn get_log_key_override(
    state: State<'_, SettingsState>,
) -> Result<LogKeyOverride, String> {
    let guard = state.lock().map_err(|e| e.to_string())?;
    Ok(LogKeyOverride {
        key: guard.log_key.clone().unwrap_or_default(),
        iv: guard.log_iv.clone().unwrap_or_default(),
    })
}

/// Saves a manual key and IV, or clears them when both are empty to go back to the bundled key.
#[tauri::command]
pub(crate) fn set_log_key_override(
    state: State<'_, SettingsState>,
    key: String,
    iv: String,
) -> Result<(), String> {
    let key = key.trim().to_string();
    let iv = iv.trim().to_string();
    LogKey::resolve(Some(&key), Some(&iv))?;
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    guard.log_key = Some(key).filter(|k| !k.is_empty());
    guard.log_iv = Some(iv).filter(|v| !v.is_empty());
    guard.save()
}

#[tauri::command]
pub(crate) async fn get_log_key_status(
    state: State<'_, SettingsState>,
) -> Result<KeyCheck, String> {
    let key = log_key(&state)?;
    tauri::async_runtime::spawn_blocking(move || game_logs::check_key(&key))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) fn get_game_logs_dir() -> Option<String> {
    rivals_core::paths::logs_dir().map(|p| p.to_string_lossy().into_owned())
}
