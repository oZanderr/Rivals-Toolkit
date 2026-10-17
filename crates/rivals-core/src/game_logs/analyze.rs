//! Reads a parsed log for how the session ended, its errors grouped alike, and which of the session's mods ship the packages those errors name.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::crash::CrashReport;
use super::lines::{ParsedLog, Verbosity, split_prefix};
use crate::mod_library::ContentLister;
use crate::mods::walk_mod_files;
use crate::paths::mods_dir;

/// Groups past this are counted in `groups_total` but not returned.
const MAX_GROUPS: usize = 500;
const MAX_GROUP_PACKAGES: usize = 50;
const MAX_FATAL_LINES: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Running,
    CleanExit,
    Crashed,
    /// A fatal error in the log, with no crash folder left for it.
    FatalError,
    /// No close and no error: killed, or the machine went down.
    EndedAbruptly,
    Empty,
    /// The key does not open it.
    Unreadable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModState {
    Enabled,
    /// Switched off since the session.
    Disabled,
    /// Removed or renamed since the session.
    Missing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModsSource {
    /// The mods the log says the game mounted.
    Log,
    /// The log does not say, so the mods switched on now stand in.
    Enabled,
    /// No game folder to look in.
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FatalBlock {
    pub line: u32,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorGroup {
    pub verbosity: Verbosity,
    pub category: String,
    /// The first line's message; the others differ from it only in numbers and package paths.
    pub message: String,
    pub count: u32,
    pub first_line: u32,
    pub packages: Vec<String>,
    /// The game logs this too in recent sessions with no mods mounted.
    pub without_mods: bool,
}

/// A frame of the Python stack a thread was running when the game died, as its fault handler
/// wrote it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PyFrame {
    pub file: String,
    pub line: u32,
    pub function: String,
    /// A script from outside the game's own, such as a plugin's under `Binaries/Win64`.
    pub third_party: bool,
}

/// The problems the game logs with no mods mounted, so they are not blamed on mods.
#[derive(Debug, Clone, Default)]
pub struct Baseline {
    pub sessions: u32,
    pub keys: HashSet<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMod {
    /// As mounted, below `~mods`.
    pub name: String,
    pub state: ModState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModHit {
    pub mod_name: String,
    pub state: ModState,
    /// The packages it ships that error and warning lines name.
    pub packages: Vec<String>,
    /// Indices into `LogAnalysis::groups`.
    pub groups: Vec<usize>,
    /// Error lines naming its packages.
    pub errors: u32,
    pub warnings: u32,
}

/// A session mod shipping a package the fatal error or the crash report names.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FatalMod {
    pub mod_name: String,
    pub state: ModState,
    pub packages: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogAnalysis {
    pub outcome: Outcome,
    pub build: Option<String>,
    pub engine_version: Option<String>,
    pub command_line: Option<String>,
    pub fatal: Option<FatalBlock>,
    pub fatal_count: u32,
    pub error_count: u32,
    pub warning_count: u32,
    /// Fatal and error groups first, then warnings, each the most frequent first.
    pub groups: Vec<ErrorGroup>,
    pub groups_total: usize,
    /// How many sessions without mods the `without_mods` flags were read from.
    pub baseline_sessions: u32,
    /// The Python stack the game thread was in when the game died, innermost first.
    pub game_thread_python: Vec<PyFrame>,
    pub mods: Vec<SessionMod>,
    pub mods_source: ModsSource,
    pub mod_hits: Vec<ModHit>,
    /// The mods shipping a package the fatal error or the crash report names: the most direct
    /// sign of which mod brought the game down.
    pub fatal_mods: Vec<FatalMod>,
    pub notes: Vec<String>,
}

/// Where to look the session's mods up, which a front end may answer from a cache.
pub struct ModsContext<'a> {
    pub game_root: &'a str,
    pub lister: &'a dyn ContentLister,
}

/// The `/Game/` package paths a message names, without `.Object` or `:Subobject` suffixes.
pub fn packages_in(message: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = message;
    while let Some(at) = rest.find("/Game/") {
        let tail = &rest[at..];
        let len = tail
            .char_indices()
            .find(|&(_, c)| !(c.is_alphanumeric() || matches!(c, '_' | '/' | '-')))
            .map_or(tail.len(), |(i, _)| i);
        let path = tail[..len].trim_end_matches('/');
        if path.len() > "/Game/".len() && !out.contains(&path) {
            out.push(path);
        }
        rest = &tail[len.max(1)..];
    }
    out
}

/// What groups alike problem lines, and what a baseline matches them by.
fn problem_key(verbosity: Verbosity, category: &str, message: &str) -> String {
    format!("{verbosity:?}|{category}|{}", group_key(message))
}

/// The kinds of problem a log holds, for a baseline.
pub fn problem_keys(log: &ParsedLog) -> HashSet<String> {
    (0..log.len())
        .filter(|&n| !log.is_trailer_repeat(n) && !log.is_continuation(n))
        .filter(|&n| log.verbosity(n).is_problem())
        .filter_map(|n| {
            let message = split_prefix(log.line(n)).message;
            (!message.trim().is_empty())
                .then(|| problem_key(log.verbosity(n), log.category(n), message))
        })
        .collect()
}

/// `  File "Scripts/ui/PyUIManager.py", line 99 in OnGameInstanceStart`
fn py_frame(line: &str) -> Option<PyFrame> {
    let rest = line.trim_start().strip_prefix("File \"")?;
    let (file, rest) = rest.split_once('"')?;
    let (number, function) = rest.strip_prefix(", line ")?.split_once(" in ")?;
    Some(PyFrame {
        third_party: file.contains(":\\") || file.contains(":/"),
        file: file.to_string(),
        line: number.trim().parse().ok()?,
        function: function.trim().to_string(),
    })
}

/// A message with what varies between alike lines folded: numbers, long hex ids, package paths.
fn group_key(message: &str) -> String {
    let mut out = String::with_capacity(message.len().min(400));
    let mut rest = message;
    while let Some(c) = rest.chars().next() {
        if out.len() >= 400 {
            break;
        }
        if rest.starts_with("/Game/") {
            let len = rest
                .char_indices()
                .find(|&(_, c)| !(c.is_alphanumeric() || matches!(c, '_' | '/' | '-' | '.')))
                .map_or(rest.len(), |(i, _)| i);
            out.push_str("<package>");
            rest = &rest[len..];
            continue;
        }
        if c.is_ascii_alphanumeric() {
            let len = rest.bytes().take_while(u8::is_ascii_alphanumeric).count();
            let word = &rest[..len];
            let hex = word.strip_prefix("0x").unwrap_or(word);
            let is_id = hex.len() >= 8
                && hex.bytes().all(|b| b.is_ascii_hexdigit())
                && hex.bytes().any(|b| b.is_ascii_digit());
            if is_id
                || (word.starts_with("0x")
                    && !hex.is_empty()
                    && hex.bytes().all(|b| b.is_ascii_hexdigit()))
            {
                out.push('#');
            } else {
                let mut in_digits = false;
                for b in word.bytes() {
                    if b.is_ascii_digit() {
                        if !in_digits {
                            out.push('#');
                        }
                        in_digits = true;
                    } else {
                        out.push(char::from(b));
                        in_digits = false;
                    }
                }
            }
            rest = &rest[len..];
            continue;
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// A package key from a mod's file: `/game/...` for any project's content, lowercased, the way
/// the game overlays a mod cooked under another project name.
fn package_key(entry: &str) -> Option<String> {
    let lower = entry.replace('\\', "/").to_lowercase();
    let stem = lower
        .strip_suffix(".uasset")
        .or_else(|| lower.strip_suffix(".umap"))?;
    let at = stem.find("/content/")?;
    let project = stem[..at].rsplit('/').next().unwrap_or("");
    if project.is_empty() || project == "engine" || stem[..at].contains("/plugins/") {
        return None;
    }
    Some(format!("/game/{}", &stem[at + "/content/".len()..]))
}

fn mod_state(mods: &Path, name: &str) -> ModState {
    if mods.join(name).is_file() {
        ModState::Enabled
    } else if mods.join(format!("{name}.disabled")).is_file() {
        ModState::Disabled
    } else {
        ModState::Missing
    }
}

/// Which session mod ships each package, by package key.
fn package_index(ctx: &ModsContext<'_>, mods: &[SessionMod]) -> HashMap<String, Vec<usize>> {
    let dir = mods_dir(ctx.game_root);
    let mut index: HashMap<String, Vec<usize>> = HashMap::new();
    for (at, session_mod) in mods.iter().enumerate() {
        let suffix = match session_mod.state {
            ModState::Enabled => "",
            ModState::Disabled => ".disabled",
            ModState::Missing => continue,
        };
        let pak = dir.join(format!("{}{suffix}", session_mod.name));
        let stem = session_mod
            .name
            .strip_suffix(".pak")
            .unwrap_or(&session_mod.name);
        let utoc = dir.join(format!("{stem}.utoc{suffix}"));
        let mut entries = if utoc.is_file() {
            ctx.lister.utoc(&utoc).unwrap_or_default()
        } else {
            Vec::new()
        };
        entries.extend(ctx.lister.pak(&pak).unwrap_or_default());
        for entry in entries {
            if let Some(key) = package_key(&entry) {
                let holders = index.entry(key).or_default();
                if !holders.contains(&at) {
                    holders.push(at);
                }
            }
        }
    }
    index
}

/// The mod a `Mounted Pak file '../../../Marvel/Content/Paks/~mods/X.pak'` line names.
pub(crate) fn mounted_mod(message: &str) -> Option<&str> {
    let quoted = message
        .split_once("Mounted Pak file '")?
        .1
        .split('\'')
        .next()?;
    let lower = quoted.to_ascii_lowercase();
    let at = lower.find("/~mods/")?;
    Some(&quoted[at + "/~mods/".len()..])
}

struct GroupBuild {
    without_mods: bool,
    verbosity: Verbosity,
    category: String,
    message: String,
    count: u32,
    first_line: u32,
    packages: BTreeSet<String>,
}

#[derive(Default)]
struct HitBuild {
    packages: BTreeSet<String>,
    groups: BTreeSet<usize>,
    errors: u32,
    warnings: u32,
}

pub fn analyze(
    log: &ParsedLog,
    crash: Option<&CrashReport>,
    running: bool,
    mods_ctx: Option<ModsContext<'_>>,
    baseline: Option<&Baseline>,
) -> LogAnalysis {
    let mut notes = log.notes.clone();
    let (mut build, mut engine_version, mut command_line) = (None, None, None);
    let mut fatal: Option<FatalBlock> = None;
    let mut crash_handler = false;
    let mut closed = false;
    let mut saw_mount = false;
    let mut mounted: Vec<String> = Vec::new();
    let (mut fatal_count, mut error_count, mut warning_count) = (0u32, 0u32, 0u32);
    let mut groups: Vec<GroupBuild> = Vec::new();
    let mut group_ids: HashMap<String, usize> = HashMap::new();
    let mut game_thread_python: Vec<PyFrame> = Vec::new();
    // Per line: its group, and the packages it names, for matching against mods once known.
    let mut problem_lines: Vec<(usize, Verbosity, Vec<String>)> = Vec::new();

    for n in 0..log.len() {
        let line = log.line(n);
        // Logs without timestamps write this line bare.
        if line.contains("Log file closed") {
            closed = true;
        }
        if log.is_trailer_repeat(n) || log.is_continuation(n) {
            continue;
        }
        let prefix = split_prefix(line);
        let message = prefix.message;
        let category = log.category(n);

        if category == "LogInit" {
            let field = |name: &str| message.strip_prefix(name).map(|v| v.trim().to_string());
            build = build.or_else(|| field("Build:"));
            engine_version = engine_version.or_else(|| field("Engine Version:"));
            command_line = command_line.or_else(|| field("Command Line:"));
        }
        if message.contains("Mounted Pak file '") {
            saw_mount = true;
            if let Some(name) = mounted_mod(message)
                && !mounted.iter().any(|m| m.eq_ignore_ascii_case(name))
            {
                mounted.push(name.to_string());
            }
        }
        // The fault handler dumps every Python thread when the game dies; the last dump counts.
        if message.ends_with("(most recent call first):") && message.starts_with("GameThread") {
            game_thread_python = (n + 1..log.len())
                .take_while(|&next| log.is_continuation(next))
                .map_while(|next| py_frame(log.line(next)))
                .collect();
        }
        if message.contains("FOutputDeviceRedirector Memory Statistics (Panic Mode)") {
            crash_handler = true;
        }
        if fatal.is_none()
            && (message.contains("appError called") || message.contains("=== Critical error: ==="))
        {
            // The message runs on in bare lines, and the crash handler's report in error lines.
            let mut text = vec![line.to_string()];
            let mut next = n + 1;
            while next < log.len()
                && text.len() < MAX_FATAL_LINES
                && (log.is_continuation(next)
                    || matches!(log.verbosity(next), Verbosity::Error | Verbosity::Fatal))
            {
                let more = log.line(next);
                let more_message = split_prefix(more).message;
                // A crash report lists the same stack with module offsets.
                let repeats_crash = crash.is_some() && more_message.starts_with("[Callstack]");
                if !more_message.trim().is_empty() && !repeats_crash {
                    text.push(more.to_string());
                }
                next += 1;
            }
            fatal = Some(FatalBlock {
                line: n as u32,
                text: text.join("\n"),
            });
        }

        let verbosity = log.verbosity(n);
        if verbosity.is_problem() {
            match verbosity {
                Verbosity::Fatal => fatal_count += 1,
                Verbosity::Error => error_count += 1,
                _ => warning_count += 1,
            }
            // From the fatal error or the crash handler's first output on, the log reports the crash.
            let crashing = crash_handler || fatal.is_some();
            if crashing || message.trim().is_empty() || message.starts_with("[Callstack]") {
                continue;
            }
            let key = problem_key(verbosity, category, message);
            let without_mods = baseline.is_some_and(|b| b.keys.contains(&key));
            let id = *group_ids.entry(key).or_insert_with(|| {
                groups.push(GroupBuild {
                    without_mods,
                    verbosity,
                    category: category.to_string(),
                    message: message.chars().take(1000).collect(),
                    count: 0,
                    first_line: n as u32,
                    packages: BTreeSet::new(),
                });
                groups.len() - 1
            });
            let packages: Vec<String> = packages_in(message)
                .into_iter()
                .map(str::to_string)
                .collect();
            if let Some(group) = groups.get_mut(id) {
                group.count += 1;
                for package in &packages {
                    if group.packages.len() < MAX_GROUP_PACKAGES {
                        group.packages.insert(package.clone());
                    }
                }
            }
            // A problem the game has without mods is not the mods' doing.
            if !packages.is_empty() && !without_mods {
                problem_lines.push((id, verbosity, packages));
            }
        }
    }

    let mut mods_source = ModsSource::Unavailable;
    let mut session_mods: Vec<SessionMod> = Vec::new();
    let mut hits: Vec<HitBuild> = Vec::new();
    let mut fatal_mods: Vec<FatalMod> = Vec::new();
    let mut fatal_packages: Vec<String> = Vec::new();
    let fatal_texts = [
        fatal.as_ref().map(|f| f.text.as_str()),
        crash.map(|c| c.error_message.as_str()),
    ];
    for package in fatal_texts.into_iter().flatten().flat_map(packages_in) {
        if !fatal_packages
            .iter()
            .any(|p| p.eq_ignore_ascii_case(package))
        {
            fatal_packages.push(package.to_string());
        }
    }
    match &mods_ctx {
        Some(ctx) if !ctx.game_root.is_empty() => {
            let dir = mods_dir(ctx.game_root);
            if saw_mount {
                mods_source = ModsSource::Log;
                session_mods = mounted
                    .iter()
                    .map(|name| SessionMod {
                        state: mod_state(&dir, name),
                        name: name.clone(),
                    })
                    .collect();
            } else {
                mods_source = ModsSource::Enabled;
                notes.push(
                    "The log does not say which mods the game mounted, so the mods switched on \
                     now stand in."
                        .to_string(),
                );
                let mut names: Vec<String> = walk_mod_files(&dir, true)
                    .into_iter()
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                    .filter(|name| name.ends_with(".pak"))
                    .collect();
                names.sort_by_key(|n| n.to_lowercase());
                session_mods = names
                    .into_iter()
                    .map(|name| SessionMod {
                        name,
                        state: ModState::Enabled,
                    })
                    .collect();
            }
            hits = (0..session_mods.len())
                .map(|_| HitBuild::default())
                .collect();
            if !problem_lines.is_empty() || !fatal_packages.is_empty() {
                let index = package_index(ctx, &session_mods);
                fatal_mods = session_mods
                    .iter()
                    .enumerate()
                    .filter_map(|(at, session_mod)| {
                        let packages: Vec<String> = fatal_packages
                            .iter()
                            .filter(|p| {
                                index
                                    .get(&p.to_lowercase())
                                    .is_some_and(|holders| holders.contains(&at))
                            })
                            .cloned()
                            .collect();
                        (!packages.is_empty()).then(|| FatalMod {
                            mod_name: session_mod.name.clone(),
                            state: session_mod.state,
                            packages,
                        })
                    })
                    .collect();
                for (group, verbosity, packages) in &problem_lines {
                    for package in packages {
                        let Some(holders) = index.get(&package.to_lowercase()) else {
                            continue;
                        };
                        for &holder in holders {
                            let Some(hit) = hits.get_mut(holder) else {
                                continue;
                            };
                            hit.packages.insert(package.clone());
                            hit.groups.insert(*group);
                        }
                    }
                    let mut owners: BTreeSet<usize> = BTreeSet::new();
                    for package in packages {
                        owners.extend(index.get(&package.to_lowercase()).into_iter().flatten());
                    }
                    for owner in owners {
                        if let Some(hit) = hits.get_mut(owner) {
                            if *verbosity == Verbosity::Warning {
                                hit.warnings += 1;
                            } else {
                                hit.errors += 1;
                            }
                        }
                    }
                }
            }
        }
        _ => {
            if saw_mount {
                session_mods = mounted
                    .iter()
                    .map(|name| SessionMod {
                        name: name.clone(),
                        state: ModState::Missing,
                    })
                    .collect();
            }
            notes.push("No game folder is set, so errors were not matched to mods.".to_string());
        }
    }

    // Severity first, then the most frequent, then the earliest.
    let mut order: Vec<usize> = (0..groups.len()).collect();
    order.sort_by(|&a, &b| {
        let (a, b) = (&groups[a], &groups[b]);
        a.verbosity
            .cmp(&b.verbosity)
            .then_with(|| b.count.cmp(&a.count))
            .then_with(|| a.first_line.cmp(&b.first_line))
    });
    let groups_total = groups.len();
    order.truncate(MAX_GROUPS);
    let mut position = vec![None; groups.len()];
    for (at, &id) in order.iter().enumerate() {
        position[id] = Some(at);
    }
    let mut slots: Vec<Option<GroupBuild>> = groups.into_iter().map(Some).collect();
    let groups: Vec<ErrorGroup> = order
        .iter()
        .filter_map(|&id| slots.get_mut(id).and_then(Option::take))
        .map(|g| ErrorGroup {
            verbosity: g.verbosity,
            category: g.category,
            message: g.message,
            count: g.count,
            first_line: g.first_line,
            packages: g.packages.into_iter().collect(),
            without_mods: g.without_mods,
        })
        .collect();

    let mut mod_hits: Vec<ModHit> = session_mods
        .iter()
        .zip(hits)
        .filter(|(_, hit)| !hit.packages.is_empty())
        .map(|(session_mod, hit)| ModHit {
            mod_name: session_mod.name.clone(),
            state: session_mod.state,
            packages: hit.packages.into_iter().collect(),
            groups: hit
                .groups
                .into_iter()
                .filter_map(|id| position[id])
                .collect(),
            errors: hit.errors,
            warnings: hit.warnings,
        })
        .collect();
    mod_hits.sort_by(|a, b| {
        b.errors
            .cmp(&a.errors)
            .then_with(|| b.warnings.cmp(&a.warnings))
            .then_with(|| a.mod_name.to_lowercase().cmp(&b.mod_name.to_lowercase()))
    });

    let outcome = if crash.is_some() {
        Outcome::Crashed
    } else if running {
        Outcome::Running
    } else if fatal.is_some() {
        Outcome::FatalError
    } else if closed {
        Outcome::CleanExit
    } else if log.is_empty() {
        Outcome::Empty
    } else {
        Outcome::EndedAbruptly
    };

    LogAnalysis {
        outcome,
        build,
        engine_version,
        command_line,
        fatal,
        fatal_count,
        error_count,
        warning_count,
        groups,
        groups_total,
        baseline_sessions: baseline.map_or(0, |b| b.sessions),
        game_thread_python,
        mods: session_mods,
        mods_source,
        mod_hits,
        fatal_mods,
        notes,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use std::path::PathBuf;

    use super::super::decode::DecodedLog;
    use super::*;

    fn parsed(text: &str) -> ParsedLog {
        ParsedLog::new(DecodedLog {
            bytes: text.as_bytes().to_vec(),
            key_source: None,
            records: 1,
            trailer_offset: None,
            notes: Vec::new(),
        })
        .unwrap()
    }

    struct FakeLister;

    impl ContentLister for FakeLister {
        fn utoc(&self, utoc: &Path) -> Result<Vec<String>, String> {
            let name = utoc.file_name().unwrap().to_string_lossy().into_owned();
            Ok(match name.as_str() {
                "Skin_9999999_P.utoc" => vec![
                    "Marvel/Content/Marvel/Characters/1011/Mesh.uasset".to_string(),
                    "Marvel/Content/Marvel/UI/Shared.uasset".to_string(),
                ],
                "Off_9999999_P.utoc.disabled" => {
                    vec!["Marvel/Content/Marvel/UI/Shared.uasset".to_string()]
                }
                _ => Vec::new(),
            })
        }

        fn pak(&self, _pak: &Path) -> Result<Vec<String>, String> {
            Ok(Vec::new())
        }
    }

    fn game_root(mods: &[&str]) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "rivals-game-logs-{}-{}",
            std::process::id(),
            mods.len()
        ));
        let dir = mods_dir(&root.to_string_lossy());
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&dir).unwrap();
        for name in mods {
            std::fs::write(dir.join(name), b"").unwrap();
        }
        root
    }

    #[test]
    fn finds_package_paths_in_messages() {
        assert_eq!(
            packages_in(
                "CreateExport: /Game/Marvel/UI/Foo (0x8C7E) /Game/Marvel/UI/Foo (0x8C7E) - Could not find class object"
            ),
            ["/Game/Marvel/UI/Foo"]
        );
        assert_eq!(
            packages_in("Failed to find object 'SkeletalMesh /Game/A/B.B_C:Sub', and '/Game/X/Y'."),
            ["/Game/A/B", "/Game/X/Y"]
        );
        assert!(packages_in("nothing to see in /Script/Engine").is_empty());
    }

    #[test]
    fn alike_messages_group_together() {
        assert_eq!(
            group_key("Missing Dependency, missing export 12 in package /Game/A/B"),
            group_key("Missing Dependency, missing export 7 in package /Game/C.D")
        );
        assert_eq!(
            group_key(
                "Failed to create pipeline state with combined hash B23B6DEF4F32B8DE, error 80070057."
            ),
            group_key(
                "Failed to create pipeline state with combined hash FD24A62086873081, error 80070057."
            )
        );
        assert_ne!(
            group_key("Requested Gameplay Tag Hero.ID.0 was not found"),
            group_key("Requested Gameplay Tag Signal.Base.NeedHealing was not found")
        );
        assert_eq!(
            package_key("Marvel/Content/Marvel/UI/A.uasset").as_deref(),
            Some("/game/marvel/ui/a")
        );
        assert_eq!(
            package_key("Other/Content/X.umap").as_deref(),
            Some("/game/x")
        );
        assert_eq!(package_key("Engine/Content/X.uasset"), None);
        assert_eq!(package_key("Marvel/Content/X.uexp"), None);
    }

    #[test]
    fn reads_the_ending_and_the_fatal_error() {
        let log = parsed(
            "LogInit: Display: Build: 1.1.3922109.3922109\n\
             LogInit: Display: Command Line: -bylauncher\n\
             Warning: Missing Dependency, missing export 12 in package /Game/A/B\n\
             LogWindows: Error: appError called: Fatal error: [File:X] [Line: 1392] \n\
             GameThread timed out waiting for RenderThread after 60.00 secs\n\
             Warning: ===== FOutputDeviceRedirector Memory Statistics (Panic Mode) =====\n\
             Error: Crash in runnable thread RenderThread\n",
        );
        let analysis = analyze(&log, None, false, None, None);
        assert_eq!(analysis.outcome, Outcome::FatalError);
        // What the crash handler logs after the fatal error is not grouped as problems.
        let messages: Vec<&str> = analysis.groups.iter().map(|g| g.message.as_str()).collect();
        assert_eq!(
            messages,
            ["Missing Dependency, missing export 12 in package /Game/A/B"]
        );
        assert_eq!(analysis.build.as_deref(), Some("1.1.3922109.3922109"));
        assert_eq!(analysis.command_line.as_deref(), Some("-bylauncher"));
        let fatal = analysis.fatal.unwrap();
        assert_eq!(fatal.line, 3);
        assert!(fatal.text.ends_with("after 60.00 secs"));
        assert_eq!(analysis.mods_source, ModsSource::Unavailable);

        // An unhandled exception has no appError line; the handler's memory dump comes first.
        let exception = parsed(
            "Warning: Missing Dependency, missing export 12 in package /Game/A/B\n\
             Warning: ===== FOutputDeviceRedirector Memory Statistics (Panic Mode) =====\n\
             Warning: BufferedItems Queue: Count=1, Memory=618 B\n\
             Error: === Critical error: ===\n\
             Error: Unhandled Exception: EXCEPTION_ACCESS_VIOLATION reading address 0x28\n",
        );
        let analysis = analyze(&exception, None, false, None, None);
        let messages: Vec<&str> = analysis.groups.iter().map(|g| g.message.as_str()).collect();
        assert_eq!(
            messages,
            ["Missing Dependency, missing export 12 in package /Game/A/B"]
        );
        assert_eq!(analysis.fatal.unwrap().line, 3);

        let clean = parsed(
            "LogInit: Display: hi\n[2026.10.14-02.14.00:151 -0700][  9]Log file closed, 10/14/26\n",
        );
        assert_eq!(
            analyze(&clean, None, false, None, None).outcome,
            Outcome::CleanExit
        );
        assert_eq!(
            analyze(&clean, None, true, None, None).outcome,
            Outcome::Running
        );
        assert_eq!(
            analyze(&parsed(""), None, false, None, None).outcome,
            Outcome::Empty
        );
        assert_eq!(
            analyze(&parsed("LogInit: Display: hi\n"), None, false, None, None).outcome,
            Outcome::EndedAbruptly
        );
    }

    #[test]
    fn a_problem_the_game_has_without_mods_is_not_blamed_on_them() {
        let root = game_root(&["Skin_9999999_P.pak", "Skin_9999999_P.utoc"]);
        let root_str = root.to_string_lossy().into_owned();
        let vanilla = parsed(
            "Error: CreateExport: /Game/Marvel/UI/Other (0x9) - Could not find class object\n",
        );
        let baseline = Baseline {
            sessions: 3,
            keys: problem_keys(&vanilla),
        };
        let log = parsed(
            "LogPakFile: Display: Mounted Pak file '../../../Marvel/Content/Paks/~mods/Skin_9999999_P.pak', mount point: '../../../'\n\
             Error: CreateExport: /Game/Marvel/UI/Shared (0x1) - Could not find class object\n\
             Warning: Failed to find object 'SkeletalMesh /Game/Marvel/Characters/1011/Mesh.Mesh'\n",
        );
        let analysis = analyze(
            &log,
            None,
            false,
            Some(ModsContext {
                game_root: &root_str,
                lister: &FakeLister,
            }),
            Some(&baseline),
        );
        let _ = std::fs::remove_dir_all(&root);

        assert_eq!(analysis.baseline_sessions, 3);
        let flags: Vec<bool> = analysis.groups.iter().map(|g| g.without_mods).collect();
        assert_eq!(flags, [true, false]);
        let hit = &analysis.mod_hits[0];
        assert_eq!((hit.errors, hit.warnings), (0, 1));
        assert_eq!(hit.packages, ["/Game/Marvel/Characters/1011/Mesh"]);
    }

    #[test]
    fn names_the_mod_shipping_the_package_the_fatal_error_names() {
        let root = game_root(&[
            "Skin_9999999_P.pak",
            "Skin_9999999_P.utoc",
            "Quiet_9999999_P.pak",
        ]);
        let root_str = root.to_string_lossy().into_owned();
        let log = parsed(
            "LogPakFile: Display: Mounted Pak file '../../../Marvel/Content/Paks/~mods/Skin_9999999_P.pak', mount point: '../../../'\n\
             LogPakFile: Display: Mounted Pak file '../../../Marvel/Content/Paks/~mods/Quiet_9999999_P.pak', mount point: '../../../'\n\
             LogWindows: Error: appError called: Fatal error: [File:AsyncLoading2.cpp] [Line: 1734] \n\
             ObjectSerializationError: /Game/Marvel/UI/Shared (0xD6) - Shared_C /Game/Marvel/UI/Shared.Default__Shared_C: Bad export index 50790911/21.\n",
        );
        let analysis = analyze(
            &log,
            None,
            false,
            Some(ModsContext {
                game_root: &root_str,
                lister: &FakeLister,
            }),
            None,
        );
        let _ = std::fs::remove_dir_all(&root);
        let named: Vec<(&str, &[String])> = analysis
            .fatal_mods
            .iter()
            .map(|m| (m.mod_name.as_str(), m.packages.as_slice()))
            .collect();
        assert_eq!(
            named,
            [(
                "Skin_9999999_P.pak",
                &["/Game/Marvel/UI/Shared".to_string()][..]
            )]
        );
    }

    #[test]
    fn reads_the_game_threads_python_stack_from_the_fault_dump() {
        let log = parsed(
            "Display: [4652]: Thread 0x0000122c (most recent call first):\n\
             \x20 File \"lib/socket.py\", line 848 in create_connection\n\
             \n\
             Display: GameThread[8088]: Thread 0x00001f98 (most recent call first):\n\
             \x20 File \"Scripts/subclassing/ui/PyUIManager.py\", line 99 in OnGameInstanceStart\n\
             \x20 File \"C:\\Game\\plugins\\scripts\\overlay.py\", line 12 in tick\n\
             \n\
             Error: === Critical error: ===\n",
        );
        let frames = analyze(&log, None, false, None, None).game_thread_python;
        let read: Vec<(&str, u32, &str, bool)> = frames
            .iter()
            .map(|f| (f.file.as_str(), f.line, f.function.as_str(), f.third_party))
            .collect();
        assert_eq!(
            read,
            [
                (
                    "Scripts/subclassing/ui/PyUIManager.py",
                    99,
                    "OnGameInstanceStart",
                    false
                ),
                ("C:\\Game\\plugins\\scripts\\overlay.py", 12, "tick", true),
            ]
        );
    }

    #[test]
    fn matches_errors_to_the_mods_that_ship_their_packages() {
        let root = game_root(&[
            "Skin_9999999_P.pak",
            "Skin_9999999_P.utoc",
            "Off_9999999_P.pak.disabled",
            "Off_9999999_P.utoc.disabled",
            "Quiet_9999999_P.pak",
        ]);
        let root_str = root.to_string_lossy().into_owned();
        let log = parsed(
            "LogPakFile: Display: Mounted Pak file '../../../Marvel/Content/Paks/~mods/Skin_9999999_P.pak', mount point: '../../../'\n\
             LogPakFile: Display: Mounted Pak file '../../../Marvel/Content/Paks/~mods/Off_9999999_P.pak', mount point: '../../../'\n\
             LogPakFile: Display: Mounted Pak file '../../../Marvel/Content/Paks/~mods/Quiet_9999999_P.pak', mount point: '../../../'\n\
             LogPakFile: Display: Mounted Pak file '../../../Marvel/Content/Paks/~mods/Gone_9999999_P.pak', mount point: '../../../'\n\
             Error: CreateExport: /Game/Marvel/UI/Shared (0x1) - Could not find class object\n\
             Error: CreateExport: /Game/Marvel/UI/Shared (0x2) - Could not find class object\n\
             Warning: Failed to find object 'SkeletalMesh /Game/Marvel/Characters/1011/Mesh.Mesh'\n\
             Warning: Failed to find object 'SkeletalMesh /Game/Marvel/Elsewhere/Mesh.Mesh'\n",
        );
        let analysis = analyze(
            &log,
            None,
            false,
            Some(ModsContext {
                game_root: &root_str,
                lister: &FakeLister,
            }),
            None,
        );
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(analysis.baseline_sessions, 0);
        assert!(analysis.groups.iter().all(|g| !g.without_mods));

        assert_eq!(analysis.mods_source, ModsSource::Log);
        let states: Vec<(&str, ModState)> = analysis
            .mods
            .iter()
            .map(|m| (m.name.as_str(), m.state))
            .collect();
        assert_eq!(
            states,
            [
                ("Skin_9999999_P.pak", ModState::Enabled),
                ("Off_9999999_P.pak", ModState::Disabled),
                ("Quiet_9999999_P.pak", ModState::Enabled),
                ("Gone_9999999_P.pak", ModState::Missing),
            ]
        );
        assert_eq!(analysis.error_count, 2);
        assert_eq!(analysis.groups.len(), 2);
        assert_eq!(analysis.groups[0].count, 2);

        let hit = |name: &str| {
            analysis
                .mod_hits
                .iter()
                .find(|h| h.mod_name == name)
                .unwrap()
        };
        let skin = hit("Skin_9999999_P.pak");
        assert_eq!((skin.errors, skin.warnings), (2, 1));
        assert_eq!(
            skin.packages,
            [
                "/Game/Marvel/Characters/1011/Mesh",
                "/Game/Marvel/UI/Shared"
            ]
        );
        assert_eq!(skin.groups, [0, 1]);
        let off = hit("Off_9999999_P.pak");
        assert_eq!(off.state, ModState::Disabled);
        assert_eq!((off.errors, off.warnings), (2, 0));
        assert_eq!(analysis.mod_hits.len(), 2);
    }
}
