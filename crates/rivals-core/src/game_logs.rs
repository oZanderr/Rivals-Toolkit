//! Finds the game's engine logs and crash reports under `%LOCALAPPDATA%\Marvel\Saved`, decodes them and tells how each session went.

pub mod analyze;
pub mod container;
pub mod crash;
pub mod decode;
pub mod lines;
pub mod share;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub use analyze::{
    Baseline, ErrorGroup, FatalBlock, FatalMod, LogAnalysis, ModHit, ModState, ModsContext,
    ModsSource, Outcome, PyFrame, SessionMod, analyze,
};
pub use crash::{CrashReport, Frame, LoadedModule, ModuleClass};
pub use decode::{DecodedLog, KeySource, LogKey};
pub use lines::{CategoryCount, LogFilter, ParsedLog, Verbosity};
pub use share::{outcome_label, redact, share_text};

use crate::paths::{crashes_dir, logs_dir};

/// How much of a log's end listing decrypts to find how it ended: a fatal error can sit a few
/// records before the last.
const TAIL_BYTES: usize = 128 * 1024;
const TAIL_RECORDS: usize = 4;
/// Sessions without mods a baseline reads, the newest first, and the largest log it decodes.
const BASELINE_SESSIONS: usize = 12;
const BASELINE_MAX_FILE: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// The log's file stem, `2026.10.14_02.13.57-8832`, or the crash folder's name when its log
    /// is gone.
    pub name: String,
    pub log_path: Option<String>,
    pub crash_dir: Option<String>,
    pub pid: Option<u32>,
    /// Unix seconds.
    pub started: Option<u64>,
    pub ended: Option<u64>,
    pub size: u64,
    pub build: Option<String>,
    pub outcome: Outcome,
    pub crash_type: Option<String>,
    pub crash_message: Option<String>,
    pub crash_signature: Option<String>,
    /// How many crash folders share this crash's signature, this one included.
    pub repeat_count: u32,
    pub error: Option<String>,
}

/// A session's files, before anything is decoded.
#[derive(Debug, Clone)]
pub struct SessionRef {
    pub name: String,
    pub log_path: Option<PathBuf>,
    pub crash_dir: Option<PathBuf>,
}

fn unix_seconds(time: SystemTime) -> Option<u64> {
    time.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
}

fn pid_of(name: &str) -> Option<u32> {
    name.rsplit_once('-')?.1.parse().ok()
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn log_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x.eq_ignore_ascii_case("log")))
        .collect()
}

/// The `.log` a crash folder holds, copied there by the crash reporter.
fn crash_log(dir: &Path) -> Option<PathBuf> {
    log_files(dir).into_iter().next()
}

/// Every session the Logs and Crashes folders hold, a crash joined to its log by file name. The
/// copy in Logs is preferred: it runs past the moment the crash reporter copied it.
pub fn session_refs() -> Vec<SessionRef> {
    let mut sessions: Vec<SessionRef> = logs_dir()
        .map(|dir| log_files(&dir))
        .unwrap_or_default()
        .into_iter()
        .map(|path| SessionRef {
            name: file_stem(&path),
            log_path: Some(path),
            crash_dir: None,
        })
        .collect();
    let mut by_name: HashMap<String, usize> = sessions
        .iter()
        .enumerate()
        .map(|(at, s)| (s.name.to_lowercase(), at))
        .collect();
    for dir in crashes_dir()
        .map(|d| crash::crash_dirs(&d))
        .unwrap_or_default()
    {
        let copy = crash_log(&dir);
        let name = copy.as_deref().map(file_stem);
        match name
            .as_ref()
            .and_then(|name| by_name.get(&name.to_lowercase()))
        {
            Some(&at) => sessions[at].crash_dir = Some(dir),
            None => {
                let name = name.unwrap_or_else(|| {
                    dir.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                });
                by_name.insert(name.to_lowercase(), sessions.len());
                sessions.push(SessionRef {
                    name,
                    log_path: copy,
                    crash_dir: Some(dir),
                });
            }
        }
    }
    sessions
}

#[derive(Debug, Clone, Default)]
struct Probe {
    build: Option<String>,
    mods: Option<Vec<String>>,
    closed: bool,
    fatal: bool,
    empty: bool,
    error: Option<String>,
}

fn build_in(text: &[u8]) -> Option<String> {
    const NEEDLE: &[u8] = b"LogInit: Display: Build: ";
    let at = text.windows(NEEDLE.len()).position(|w| w == NEEDLE)? + NEEDLE.len();
    let end = text[at..]
        .iter()
        .position(|&b| b == b'\n' || b == b'\r')
        .map_or(text.len(), |e| at + e);
    Some(String::from_utf8_lossy(&text[at..end]).trim().to_string())
}

/// The mods `text` says the game mounted, or None when it says nothing of mounting. The game
/// mounts its paks, the mods' among them, before anything else it logs.
fn mods_in(text: &[u8]) -> Option<Vec<String>> {
    let text = String::from_utf8_lossy(text);
    let mut saw = false;
    let mut mods: Vec<String> = Vec::new();
    for line in text.lines().filter(|l| l.contains("Mounted Pak file '")) {
        saw = true;
        if let Some(name) = analyze::mounted_mod(line)
            && !mods.iter().any(|m| m.eq_ignore_ascii_case(name))
        {
            mods.push(name.to_string());
        }
    }
    saw.then_some(mods)
}

fn contains(text: &[u8], needle: &[u8]) -> bool {
    text.windows(needle.len()).any(|w| w == needle)
}

fn is_fatal(text: &[u8]) -> bool {
    contains(text, b"appError called") || contains(text, b"=== Critical error: ===")
}

/// How a log ended, from its first record and its last few: enough to list it without decoding
/// a log that can run to 100 MB.
fn probe(blob: &[u8], key: &LogKey) -> Probe {
    let parsed = match container::parse(blob) {
        Ok(parsed) => parsed,
        Err(error) => {
            return Probe {
                error: Some(error),
                ..Probe::default()
            };
        }
    };
    let mut probe = Probe {
        empty: parsed.is_empty(),
        ..Probe::default()
    };
    if let Some(first) = parsed.records.first() {
        match decode::decrypt_records(key, std::slice::from_ref(first)) {
            Ok(texts) => {
                probe.build = texts.first().and_then(|(text, _)| build_in(text));
                probe.mods = texts.first().and_then(|(text, _)| mods_in(text));
            }
            Err(error) => {
                probe.error = Some(error);
                return probe;
            }
        }
        let mut taken = 0;
        let tail = parsed
            .records
            .iter()
            .rev()
            .take_while(|record| {
                let keep = taken < TAIL_BYTES;
                taken += record.payload.len();
                keep
            })
            .take(TAIL_RECORDS);
        for record in tail {
            let (text, _) = decode::decrypt_record(key, record.payload);
            probe.closed |= contains(&text, b"Log file closed");
            probe.fatal |= is_fatal(&text);
        }
    }
    if let Some(trailer) = parsed.trailer {
        probe.build = probe.build.or_else(|| build_in(trailer));
        if parsed.records.is_empty() {
            probe.mods = mods_in(trailer);
        }
        probe.closed |= contains(trailer, b"Log file closed");
        probe.fatal |= is_fatal(trailer);
    }
    probe
}

struct CachedProbe {
    len: u64,
    modified: Option<SystemTime>,
    key: LogKey,
    probe: Probe,
}

static PROBES: LazyLock<Mutex<HashMap<PathBuf, CachedProbe>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn probe_file(path: &Path, meta: &std::fs::Metadata, key: &LogKey) -> Probe {
    let modified = meta.modified().ok();
    {
        let cache = PROBES.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(hit) = cache.get(path)
            && hit.len == meta.len()
            && hit.modified == modified
            && hit.key == *key
        {
            return hit.probe.clone();
        }
    }
    let probe = match std::fs::read(path) {
        Ok(blob) => probe(&blob, key),
        Err(error) => Probe {
            error: Some(format!("{}: {error}", path.display())),
            ..Probe::default()
        },
    };
    PROBES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(
            path.to_path_buf(),
            CachedProbe {
                len: meta.len(),
                modified,
                key: key.clone(),
                probe: probe.clone(),
            },
        );
    probe
}

/// Every session, the newest first. `running` holds the game processes' ids, whose sessions are
/// still being written.
pub fn list_sessions(running: &[u32], key: &LogKey) -> Vec<Session> {
    let refs = session_refs();
    let crashes: Vec<Option<CrashReport>> = refs
        .iter()
        .map(|r| r.crash_dir.as_deref().and_then(|d| crash::read(d).ok()))
        .collect();
    let mut repeats: HashMap<&str, u32> = HashMap::new();
    for report in crashes.iter().flatten() {
        *repeats.entry(report.signature.as_str()).or_default() += 1;
    }

    let mut sessions: Vec<Session> = refs
        .iter()
        .zip(&crashes)
        .map(|(r, report)| {
            let meta = r
                .log_path
                .as_deref()
                .and_then(|p| std::fs::metadata(p).ok());
            let probe = match (&r.log_path, &meta) {
                (Some(path), Some(meta)) => probe_file(path, meta, key),
                _ => Probe {
                    empty: true,
                    ..Probe::default()
                },
            };
            let pid = pid_of(&r.name).or(report.as_ref().and_then(|c| c.process_id));
            let outcome = if report.is_some() {
                Outcome::Crashed
            } else if pid.is_some_and(|pid| running.contains(&pid)) {
                Outcome::Running
            } else if probe.error.is_some() {
                Outcome::Unreadable
            } else if probe.fatal {
                Outcome::FatalError
            } else if probe.closed {
                Outcome::CleanExit
            } else if probe.empty {
                Outcome::Empty
            } else {
                Outcome::EndedAbruptly
            };
            let started = meta
                .as_ref()
                .and_then(|m| m.created().ok())
                .and_then(unix_seconds)
                .or_else(|| {
                    let c = report.as_ref()?;
                    c.crashed_at?
                        .checked_sub(c.seconds_since_start.unwrap_or(0))
                });
            let ended = meta
                .as_ref()
                .and_then(|m| m.modified().ok())
                .and_then(unix_seconds)
                .or(report.as_ref().and_then(|c| c.crashed_at));
            Session {
                name: r.name.clone(),
                log_path: r
                    .log_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned()),
                crash_dir: r
                    .crash_dir
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned()),
                pid,
                started,
                ended,
                size: meta.as_ref().map_or(0, std::fs::Metadata::len),
                build: probe
                    .build
                    .or_else(|| report.as_ref().and_then(|c| c.build_version.clone())),
                outcome,
                crash_type: report.as_ref().map(|c| c.crash_type.clone()),
                crash_message: report.as_ref().map(|c| c.error_message.clone()),
                crash_signature: report.as_ref().map(|c| c.signature.clone()),
                repeat_count: report
                    .as_ref()
                    .and_then(|c| repeats.get(c.signature.as_str()).copied())
                    .unwrap_or(0),
                error: probe.error,
            }
        })
        .collect();
    sessions.sort_by(|a, b| {
        b.started
            .or(b.ended)
            .cmp(&a.started.or(a.ended))
            .then_with(|| b.name.cmp(&a.name))
    });
    sessions
}

/// The session `query` names: a `.log` file, a crash folder, a session's whole name, `latest`
/// or `latest-crash`.
pub fn find_session(query: &str) -> Result<SessionRef, String> {
    let path = Path::new(query);
    if path.is_file() {
        let name = file_stem(path);
        let beside = path
            .parent()
            .filter(|dir| dir.join(crash::CONTEXT_FILE).is_file())
            .map(Path::to_path_buf);
        let crash_dir = beside.or_else(|| {
            session_refs()
                .into_iter()
                .find(|s| s.name.eq_ignore_ascii_case(&name))
                .and_then(|s| s.crash_dir)
        });
        return Ok(SessionRef {
            name,
            log_path: Some(path.to_path_buf()),
            crash_dir,
        });
    }
    if path.is_dir() && path.join(crash::CONTEXT_FILE).is_file() {
        let found = session_refs()
            .into_iter()
            .find(|s| s.crash_dir.as_deref().is_some_and(|d| same_path(d, path)));
        return Ok(found.unwrap_or_else(|| SessionRef {
            name: file_stem(&crash_log(path).unwrap_or_else(|| path.to_path_buf())),
            log_path: crash_log(path),
            crash_dir: Some(path.to_path_buf()),
        }));
    }

    let mut refs = session_refs();
    refs.sort_by(|a, b| b.name.cmp(&a.name));
    match query {
        "latest" => {
            return newest(refs.into_iter()).ok_or_else(|| "There are no game logs.".to_string());
        }
        "latest-crash" => {
            return newest(refs.into_iter().filter(|s| s.crash_dir.is_some()))
                .ok_or_else(|| "There are no crash reports.".to_string());
        }
        _ => {}
    }
    let wanted = query.strip_suffix(".log").unwrap_or(query);
    if let Some(found) = refs.iter().find(|s| {
        s.name.eq_ignore_ascii_case(wanted)
            || s.crash_dir.as_deref().is_some_and(|d| {
                d.file_name()
                    .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(wanted))
            })
    }) {
        return Ok(found.clone());
    }
    let near: Vec<&str> = refs
        .iter()
        .filter(|s| s.name.to_lowercase().contains(&wanted.to_lowercase()))
        .map(|s| s.name.as_str())
        .take(5)
        .collect();
    Err(if near.is_empty() {
        format!("No game log or crash report is named {query}.")
    } else {
        format!(
            "No game log or crash report is named {query}. Did you mean: {}?",
            near.join(", ")
        )
    })
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// The most recently written of `refs`.
fn newest(refs: impl Iterator<Item = SessionRef>) -> Option<SessionRef> {
    refs.max_by_key(|s| {
        s.log_path
            .as_deref()
            .and_then(|p| std::fs::metadata(p).ok())
            .and_then(|m| m.modified().ok())
            .or_else(|| {
                s.crash_dir
                    .as_deref()
                    .and_then(|d| std::fs::metadata(d).ok())
                    .and_then(|m| m.modified().ok())
            })
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyCheck {
    pub source: KeySource,
    /// The newest log with records to test on, by name.
    pub session: Option<String>,
    /// None when there is no log to test on.
    pub opens: Option<bool>,
    pub error: Option<String>,
}

/// Whether `key` opens the newest log that has records, which is the one a game patch would
/// have changed the key for.
pub fn check_key(key: &LogKey) -> KeyCheck {
    let mut logs: Vec<(Option<SystemTime>, PathBuf)> = logs_dir()
        .map(|dir| log_files(&dir))
        .unwrap_or_default()
        .into_iter()
        .map(|p| (std::fs::metadata(&p).and_then(|m| m.modified()).ok(), p))
        .collect();
    logs.sort_by_key(|log| std::cmp::Reverse(log.0));
    for (_, path) in logs {
        let Ok(blob) = std::fs::read(&path) else {
            continue;
        };
        let Some(first) = container::parse(&blob)
            .ok()
            .and_then(|parsed| parsed.records.into_iter().next())
        else {
            continue;
        };
        let error = decode::decrypt_records(key, std::slice::from_ref(&first)).err();
        return KeyCheck {
            source: key.source,
            session: Some(file_stem(&path)),
            opens: Some(error.is_none()),
            error,
        };
    }
    KeyCheck {
        source: key.source,
        session: None,
        opens: None,
        error: None,
    }
}

struct CachedKeys {
    len: u64,
    modified: Option<SystemTime>,
    key: LogKey,
    keys: Arc<HashSet<String>>,
}

static PROBLEM_KEYS: LazyLock<Mutex<HashMap<PathBuf, CachedKeys>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn problem_keys_of(path: &Path, meta: &std::fs::Metadata, key: &LogKey) -> Arc<HashSet<String>> {
    let modified = meta.modified().ok();
    {
        let cache = PROBLEM_KEYS.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(hit) = cache.get(path)
            && hit.len == meta.len()
            && hit.modified == modified
            && hit.key == *key
        {
            return hit.keys.clone();
        }
    }
    let keys = Arc::new(
        std::fs::read(path)
            .ok()
            .and_then(|blob| decode::decode(&blob, key).ok())
            .and_then(|decoded| ParsedLog::new(decoded).ok())
            .map(|log| analyze::problem_keys(&log))
            .unwrap_or_default(),
    );
    PROBLEM_KEYS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(
            path.to_path_buf(),
            CachedKeys {
                len: meta.len(),
                modified,
                key: key.clone(),
                keys: keys.clone(),
            },
        );
    keys
}

/// The problems the game logged in the latest sessions of `build` that mounted no mods, leaving
/// out the session named `exclude`.
pub fn baseline(build: &str, exclude: &str, key: &LogKey) -> Baseline {
    let mut candidates: Vec<(Option<SystemTime>, PathBuf, std::fs::Metadata)> = session_refs()
        .into_iter()
        .filter(|s| !s.name.eq_ignore_ascii_case(exclude))
        .filter_map(|s| {
            let path = s.log_path?;
            let meta = std::fs::metadata(&path).ok()?;
            (meta.len() <= BASELINE_MAX_FILE).then_some((path, meta))
        })
        .filter(|(path, meta)| {
            let probe = probe_file(path, meta, key);
            probe.error.is_none()
                && probe.build.as_deref() == Some(build)
                && probe.mods.as_ref().is_some_and(Vec::is_empty)
        })
        .map(|(path, meta)| (meta.modified().ok(), path, meta))
        .collect();
    candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.0));
    candidates.truncate(BASELINE_SESSIONS);
    let mut keys = HashSet::new();
    for (_, path, meta) in &candidates {
        keys.extend(problem_keys_of(path, meta, key).iter().cloned());
    }
    Baseline {
        sessions: candidates.len() as u32,
        keys,
    }
}

/// What brought the game down, when the crash says so outright.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Culprit {
    /// The fatal error or the crash report names packages these mods ship.
    Mods {
        mods: Vec<String>,
        packages: Vec<String>,
    },
    /// It crashed inside third-party code loaded into the game.
    ThirdParty { module: String },
}

fn culprit(analysis: &LogAnalysis, crash: Option<&CrashReport>) -> Option<Culprit> {
    if !analysis.fatal_mods.is_empty() {
        let mut packages: Vec<String> = Vec::new();
        for package in analysis.fatal_mods.iter().flat_map(|m| &m.packages) {
            if !packages.contains(package) {
                packages.push(package.clone());
            }
        }
        return Some(Culprit::Mods {
            mods: analysis
                .fatal_mods
                .iter()
                .map(|m| m.mod_name.clone())
                .collect(),
            packages,
        });
    }
    let report = crash?;
    [report.faulting_frame, report.culprit_frame]
        .into_iter()
        .flatten()
        .filter_map(|at| report.frames.get(at))
        .find(|f| f.class == ModuleClass::ThirdParty)
        .map(|f| Culprit::ThirdParty {
            module: f.module.clone(),
        })
}

/// A session decoded and read through.
pub struct OpenedSession {
    pub session: SessionRef,
    pub log: ParsedLog,
    pub crash: Option<CrashReport>,
    pub analysis: LogAnalysis,
    pub key_source: Option<KeySource>,
    /// Why the log could not be decoded, when it could not.
    pub key_error: Option<String>,
    pub culprit: Option<Culprit>,
}

fn empty_log(notes: Vec<String>) -> Result<ParsedLog, String> {
    ParsedLog::new(DecodedLog {
        bytes: Vec::new(),
        key_source: None,
        records: 0,
        trailer_offset: None,
        notes,
    })
}

/// Decodes the session's log whole. A crash still reads when its log does not, so a wrong key
/// leaves an empty log and says why.
pub fn open(
    session: SessionRef,
    key: &LogKey,
    running: &[u32],
    mods: Option<ModsContext<'_>>,
) -> Result<OpenedSession, String> {
    let crash = session.crash_dir.as_deref().map(crash::read).transpose()?;
    let mut key_error = None;
    let (log, key_source) = match &session.log_path {
        Some(path) => {
            let blob = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
            match decode::decode(&blob, key) {
                Ok(decoded) => {
                    let source = decoded.key_source;
                    (ParsedLog::new(decoded)?, source)
                }
                Err(error) => {
                    key_error = Some(error.clone());
                    (empty_log(vec![error])?, None)
                }
            }
        }
        None => (
            empty_log(vec!["The crash report kept no log.".to_string()])?,
            None,
        ),
    };
    let pid = pid_of(&session.name).or(crash.as_ref().and_then(|c| c.process_id));

    // The build and the mods are logged first, so the head of the text is enough.
    let head = &log.text().as_bytes()[..log.text().len().min(4 << 20)];
    let mounted_mods = mods_in(head).is_some_and(|m| !m.is_empty());
    let baseline = match build_in(head) {
        Some(build) if mounted_mods => Some(baseline(&build, &session.name, key)),
        _ => None,
    };

    let mut analysis = analyze(
        &log,
        crash.as_ref(),
        pid.is_some_and(|pid| running.contains(&pid)),
        mods,
        baseline.as_ref().filter(|b| b.sessions > 0),
    );
    if key_error.is_some() && crash.is_none() {
        analysis.outcome = Outcome::Unreadable;
    }

    let culprit = culprit(&analysis, crash.as_ref());
    Ok(OpenedSession {
        session,
        log,
        crash,
        analysis,
        key_source,
        key_error,
        culprit,
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::decode::tests::log_file;
    use super::*;

    #[test]
    fn probing_reads_the_build_and_how_it_ended() {
        let key = LogKey::bundled();
        let filler = "LogTemp: Display: filler\n".repeat(50);
        let blob = log_file(
            &key,
            &[
                "LogInit: Display: Build: 1.1.42.42\r\n",
                &filler,
                "LogWindows: Error: appError called: Fatal error\n",
                &filler,
                "Log file closed, 10/14/26\n",
            ],
            None,
        );
        let probed = probe(&blob, &key);
        assert_eq!(probed.build.as_deref(), Some("1.1.42.42"));
        assert!(probed.fatal);
        assert!(probed.closed);
        assert!(probed.error.is_none());

        let wrong = LogKey::manual(&"11".repeat(16), &"22".repeat(16)).unwrap();
        assert!(probe(&blob, &wrong).error.is_some());
        assert!(probe(container::MAGIC, &key).empty);
    }

    #[test]
    fn names_a_cause_only_when_the_crash_says_so() {
        let log = empty_log(Vec::new()).unwrap();
        let mut analysis = analyze(&log, None, false, None, None);
        assert!(culprit(&analysis, None).is_none());

        let report = crash::parse(crash::tests::CONTEXT, Path::new("x")).unwrap();
        let Some(Culprit::ThirdParty { module }) = culprit(&analysis, Some(&report)) else {
            panic!("expected the third-party module");
        };
        assert_eq!(module, "Tool");

        analysis.fatal_mods = vec![FatalMod {
            mod_name: "Shake_9999999_P.pak".to_string(),
            state: ModState::Disabled,
            packages: vec!["/Game/Weapon".to_string()],
        }];
        let Some(Culprit::Mods { mods, packages }) = culprit(&analysis, Some(&report)) else {
            panic!("expected the mod");
        };
        assert_eq!(mods, ["Shake_9999999_P.pak"]);
        assert_eq!(packages, ["/Game/Weapon"]);
    }

    #[test]
    fn a_session_name_carries_its_process_id() {
        assert_eq!(pid_of("2026.10.14_02.13.57-8832"), Some(8832));
        assert_eq!(pid_of("UECC-Windows-ABC_0000"), None);
    }

    /// Decodes every log on this machine. Set `RIVALS_GAME_LOGS=1` to run it.
    #[test]
    fn every_local_log_decodes() {
        if std::env::var_os("RIVALS_GAME_LOGS").is_none() {
            return;
        }
        let key = LogKey::bundled();
        let sessions = list_sessions(&[], &key);
        assert!(!sessions.is_empty());
        for session in &sessions {
            assert!(
                session.error.is_none(),
                "{}: {:?}",
                session.name,
                session.error
            );
        }
        for session in &sessions {
            let found = find_session(&session.name).unwrap();
            let opened = open(found, &key, &[], None).unwrap();
            assert!(opened.key_error.is_none(), "{}", session.name);
            assert_eq!(opened.analysis.outcome, session.outcome, "{}", session.name);
        }
    }
}
