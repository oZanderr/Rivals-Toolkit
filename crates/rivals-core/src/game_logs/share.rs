//! Builds a plain-text summary of a session to share, and strips what identifies the player from it and from exported logs.

use std::fmt::Write;
use std::sync::LazyLock;

use regex::{Captures, Regex};

use super::analyze::{LogAnalysis, ModState, Outcome};
use super::crash::{CrashReport, ModuleClass};

const SHARE_FRAMES: usize = 12;
const SHARE_GROUPS: usize = 15;

fn pattern(text: &str) -> Option<Regex> {
    Regex::new(text).ok()
}

/// Identifying values by the key they follow: `?UID=..`, `"udid":".."`, `<MachineId>..`,
/// `key:urs, value:..`.
static KEYED: LazyLock<Option<Regex>> = LazyLock::new(|| {
    pattern(
        r#"(?i)\b(uid|udid|token|ticket|transid|urs|aim|openid|account_?id|epicaccountid|machineid|loginid|deviceid|executionguid|session_?id|steam_?id|nick_?name|player_?name|role_?name|display_?name)\b(["']?\s*[:=]\s*["']?|>|,\s*value:\s*)([^"'&?,;\s<>\])]+)"#,
    )
});
static IPV4: LazyLock<Option<Regex>> = LazyLock::new(|| {
    pattern(r"\b(?:(?:25[0-5]|2[0-4]\d|1?\d?\d)\.){3}(?:25[0-5]|2[0-4]\d|1?\d?\d)\b")
});
static STEAM_ID: LazyLock<Option<Regex>> = LazyLock::new(|| pattern(r"\b7656119\d{10}\b"));
static EMAIL: LazyLock<Option<Regex>> =
    LazyLock::new(|| pattern(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}"));
static BID: LazyLock<Option<Regex>> = LazyLock::new(|| pattern(r"\[Bid:[^\]\r\n]*\]"));
static COMMAND_LINE: LazyLock<Option<Regex>> =
    LazyLock::new(|| pattern(r"(Command Line:|<CommandLine>)([^\r\n<]*)"));
static USER_NAMES: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    let mut names: Vec<String> = std::env::var("USERNAME").into_iter().collect();
    if let Some(home) =
        dirs::home_dir().and_then(|h| h.file_name().map(|n| n.to_string_lossy().into_owned()))
    {
        names.push(home);
    }
    names.sort();
    names.dedup();
    names
        .iter()
        .filter(|name| name.chars().count() >= 3)
        .filter_map(|name| pattern(&format!(r"(?i)\b{}\b", regex::escape(name))))
        .collect()
});

/// A command line keeping only plain flag names: values and anything token-like go.
pub fn redact_command_line(line: &str) -> String {
    let plain = |name: &str| {
        !name.is_empty()
            && name.len() <= 32
            && name.bytes().all(|b| b.is_ascii_alphabetic() || b == b'_')
    };
    line.split_whitespace()
        .map(|token| {
            let (dash, body) = match token.strip_prefix('-') {
                Some(body) => ("-", body),
                None => ("", token),
            };
            match body.split_once('=') {
                Some((name, _)) if plain(name) => format!("{dash}{name}=<redacted>"),
                None if plain(body) => token.to_string(),
                _ => "<redacted>".to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `text` with the player's Windows name, account and machine ids, tokens, addresses, emails,
/// match ids and command-line values replaced.
pub fn redact(text: &str) -> String {
    let mut out = text.to_string();
    for name in USER_NAMES.iter() {
        out = name.replace_all(&out, "<user>").into_owned();
    }
    if let Some(re) = COMMAND_LINE.as_ref() {
        out = re
            .replace_all(&out, |c: &Captures<'_>| {
                format!("{}{}", &c[1], {
                    let value = redact_command_line(&c[2]);
                    if value.is_empty() {
                        String::new()
                    } else {
                        format!(" {value}")
                    }
                })
            })
            .into_owned();
    }
    if let Some(re) = KEYED.as_ref() {
        out = re.replace_all(&out, "$1$2<redacted>").into_owned();
    }
    for (re, with) in [
        (&*STEAM_ID, "<steamid>"),
        (&*EMAIL, "<email>"),
        (&*IPV4, "<ip>"),
        (&*BID, "[Bid]"),
    ] {
        if let Some(re) = re.as_ref() {
            out = re.replace_all(&out, with).into_owned();
        }
    }
    out
}

fn class_label(class: ModuleClass) -> &'static str {
    match class {
        ModuleClass::Game => "game",
        ModuleClass::Windows => "Windows",
        ModuleClass::GpuDriver => "GPU driver",
        ModuleClass::ThirdParty => "third-party",
        ModuleClass::Unknown => "unknown",
    }
}

pub fn outcome_label(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Running => "Running",
        Outcome::CleanExit => "Clean exit",
        Outcome::Crashed => "Crashed",
        Outcome::FatalError => "Fatal error",
        Outcome::EndedAbruptly => "Ended abruptly",
        Outcome::Empty => "Empty",
        Outcome::Unreadable => "Unreadable",
    }
}

fn verbosity_label(verbosity: super::lines::Verbosity) -> &'static str {
    use super::lines::Verbosity;
    match verbosity {
        Verbosity::Fatal => "Fatal",
        Verbosity::Error => "Error",
        Verbosity::Warning => "Warning",
        Verbosity::Display => "Display",
        Verbosity::Log => "Log",
        Verbosity::Verbose => "Verbose",
    }
}

/// A summary of the session to paste into a bug report, redacted.
pub fn share_text(name: &str, analysis: &LogAnalysis, crash: Option<&CrashReport>) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Marvel Rivals session {name}");
    if let Some(build) = &analysis.build {
        let engine = analysis
            .engine_version
            .as_deref()
            .or(crash.and_then(|c| c.engine_version.as_deref()));
        match engine {
            Some(engine) => {
                let _ = writeln!(out, "Build: {build} ({engine})");
            }
            None => {
                let _ = writeln!(out, "Build: {build}");
            }
        }
    }
    let _ = write!(out, "Outcome: {}", outcome_label(analysis.outcome));
    if let Some(seconds) = crash.and_then(|c| c.seconds_since_start) {
        let _ = write!(out, " after {seconds} s");
    }
    out.push('\n');

    if let Some(crash) = crash {
        if crash.crash_type == "Crash" {
            let _ = writeln!(out, "Crash: {}", crash.error_message);
        } else {
            let _ = writeln!(out, "Crash ({}): {}", crash.crash_type, crash.error_message);
        }
        if let Some(thread) = &crash.crashed_thread {
            let _ = writeln!(out, "Crashed thread: {thread}");
        }
        let frame = |at: Option<usize>| at.and_then(|at| crash.frames.get(at));
        if let Some(faulting) = frame(crash.faulting_frame) {
            let _ = writeln!(
                out,
                "Faulting module: {} ({})",
                faulting.module,
                class_label(faulting.class)
            );
        }
        if crash.culprit_frame != crash.faulting_frame
            && let Some(culprit) = frame(crash.culprit_frame)
        {
            let _ = writeln!(
                out,
                "Called from: {} ({})",
                culprit.module,
                class_label(culprit.class)
            );
        }
        if !crash.frames.is_empty() {
            out.push_str("Stack:\n");
            for frame in crash.frames.iter().take(SHARE_FRAMES) {
                let _ = writeln!(out, "  {}", frame.label());
            }
        }
        let third: Vec<&str> = crash
            .third_party_modules()
            .map(|m| m.name.as_str())
            .collect();
        if !third.is_empty() {
            let _ = writeln!(out, "Third-party modules: {}", third.join(", "));
        }
        let system: Vec<String> = [
            crash.cpu.clone(),
            crash
                .gpu
                .as_ref()
                .map(|gpu| match (&crash.gpu_driver, &crash.rhi) {
                    (Some(driver), Some(rhi)) => format!("{gpu}, driver {driver} ({rhi})"),
                    (Some(driver), None) => format!("{gpu}, driver {driver}"),
                    _ => gpu.clone(),
                }),
            crash.memory_gb.map(|gb| format!("{gb} GB")),
            crash.os.clone(),
        ]
        .into_iter()
        .flatten()
        .collect();
        if !system.is_empty() {
            let _ = writeln!(out, "System: {}", system.join("; "));
        }
    }

    if let Some(fatal) = &analysis.fatal {
        out.push_str("Fatal error:\n");
        // The crash's own stack above says the same with module offsets.
        for line in fatal
            .text
            .lines()
            .filter(|line| crash.is_none() || !line.contains("[Callstack]"))
        {
            let _ = writeln!(out, "  {line}");
        }
    }

    if !analysis.mods.is_empty() {
        let names: Vec<String> = analysis
            .mods
            .iter()
            .map(|m| match m.state {
                ModState::Missing => format!("{} (since removed)", m.name),
                _ => m.name.clone(),
            })
            .collect();
        let _ = writeln!(out, "Mods ({}): {}", names.len(), names.join(", "));
    }
    if !analysis.mod_hits.is_empty() {
        out.push_str("Errors naming packages these mods ship:\n");
        for hit in &analysis.mod_hits {
            let _ = writeln!(
                out,
                "  {}: {} error line(s), {} warning line(s)",
                hit.mod_name, hit.errors, hit.warnings
            );
            for &group in hit.groups.iter().take(5) {
                if let Some(group) = analysis.groups.get(group) {
                    let _ = writeln!(
                        out,
                        "    [{}] x{} {}",
                        verbosity_label(group.verbosity),
                        group.count,
                        group.message
                    );
                }
            }
        }
    }

    let _ = writeln!(
        out,
        "Lines: {} fatal, {} error, {} warning",
        analysis.fatal_count, analysis.error_count, analysis.warning_count
    );
    let top: Vec<_> = analysis.groups.iter().take(SHARE_GROUPS).collect();
    if !top.is_empty() {
        out.push_str("Top errors and warnings:\n");
        for group in top {
            let category = if group.category.is_empty() {
                String::new()
            } else {
                format!("{}: ", group.category)
            };
            let _ = writeln!(
                out,
                "  [{}] x{} {category}{}",
                verbosity_label(group.verbosity),
                group.count,
                group.message
            );
        }
    }
    redact(&out)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn strips_ids_tokens_and_addresses() {
        let text = "Sending Login ?UID=123456?TOKEN=abcDEF123?x=1\n\
            {\"udid\":\"ABCDEF-123\",\"aim\":\"203.0.113.9\"}\n\
            key:urs, value:Somebody\n\
            <EpicAccountId>efdbf7ee38984603</EpicAccountId><MachineId>89E23B2B</MachineId>\n\
            battle_ip=136.66.155.246, battle_port=7782\n\
            steam 76561198000000000 and me@example.com\n\
            [Bid:65613968_1791969418_1247567_11001_13]Error: x\n\
            LogInit: Display: Command Line: -bylauncher -launcher=YYP@ -G7kL2abcdef0123\n\
            Windows 11 [10.0.26200.9457] build 1.1.3922109.3922109";
        let out = redact(text);
        for secret in [
            "123456",
            "abcDEF123",
            "ABCDEF-123",
            "203.0.113.9",
            "Somebody",
            "efdbf7ee",
            "89E23B2B",
            "136.66.155.246",
            "76561198000000000",
            "me@example.com",
            "65613968",
            "YYP@",
            "G7kL2",
        ] {
            assert!(!out.contains(secret), "{secret} survived:\n{out}");
        }
        assert!(out.contains("Command Line: -bylauncher -launcher=<redacted> <redacted>"));
        assert!(out.contains("10.0.26200.9457"));
        assert!(out.contains("1.1.3922109.3922109"));
        assert!(out.contains("battle_port=7782"));
    }

    #[test]
    fn strips_the_windows_user_name() {
        let Ok(user) = std::env::var("USERNAME") else {
            return;
        };
        if user.chars().count() < 3 {
            return;
        }
        let out = redact(&format!(
            "C:\\Users\\{user}\\AppData and C:/Users/{}/x",
            user.to_uppercase()
        ));
        assert!(!out.to_lowercase().contains(&user.to_lowercase()), "{out}");
        assert!(out.contains("C:\\Users\\<user>\\AppData"));
    }
}
