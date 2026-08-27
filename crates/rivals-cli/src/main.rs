//! Command-line front end for scripting Marvel Rivals pak INI edits and config tweaks.

#![deny(clippy::unwrap_used, clippy::expect_used)]

mod resolve;
mod settings;

use clap::{Args, Parser, Subcommand};
use rivals_core::pak_tweaks::{self, PakIniFileContent, PakTweakEdit};
use rivals_core::tweaks::{TweakDefinition, TweakKind, TweakSetting, catalogue::tweak_catalogue};
use serde::Serialize;

/// `outln!` that exits quietly when the reader closes the pipe, as `| head` or quitting `less`
/// does. The standard macro panics on that write error, which is noise, not a failure.
macro_rules! outln {
    ($($arg:tt)*) => {{
        use std::io::Write;
        if writeln!(std::io::stdout(), $($arg)*).is_err() {
            std::process::exit(0);
        }
    }};
}

/// `out!` counterpart to [`outln`], for output that must not gain a trailing newline.
macro_rules! out {
    ($($arg:tt)*) => {{
        use std::io::Write;
        if write!(std::io::stdout(), $($arg)*).is_err() {
            std::process::exit(0);
        }
    }};
}

#[derive(Parser)]
#[command(
    name = "rivals-cli",
    version,
    about = "Script Marvel Rivals pak edits and config tweaks",
    disable_help_subcommand = true
)]
struct Cli {
    /// Emit machine-readable JSON instead of text.
    #[arg(long, global = true)]
    json: bool,

    /// Game install root. Defaults to the path saved by the desktop app.
    #[arg(long, global = true, value_name = "DIR")]
    game_root: Option<String>,

    /// Edit paks even while Marvel Rivals is running. The game holds these files open, so an edit
    /// can fail or be reverted on exit.
    #[arg(long, global = true)]
    force: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Curated tweaks from the shared catalogue.
    #[command(subcommand)]
    Tweaks(TweaksCmd),

    /// Raw CVar and INI access inside a pak.
    #[command(subcommand)]
    Ini(IniCmd),

    /// Pak files installed in `~mods`.
    #[command(subcommand)]
    Paks(PaksCmd),
}

#[derive(Subcommand)]
enum PaksCmd {
    /// List paks carrying editable INI files.
    List(PaksListArgs),
}

#[derive(Subcommand)]
enum TweaksCmd {
    /// Show every tweak in the catalogue.
    List,
    /// Report which tweaks a pak currently has on.
    Status(PakArgs),
    /// Turn tweaks on or off in a pak.
    Apply(ApplyArgs),
}

#[derive(Subcommand)]
enum IniCmd {
    /// List the INI files a pak ships.
    List(PakArgs),
    /// Print merged CVar state, or one INI file's raw contents with `--entry`.
    Get(GetArgs),
    /// Set CVars, resolving the target INI the same way the app does.
    Set(SetArgs),
    /// Remove CVars from every INI in the pak that sets them.
    Unset(UnsetArgs),
}

#[derive(Args)]
struct PakArgs {
    /// Pak file path, or a mod name to look up in `~mods`.
    #[arg(long, value_name = "PAK")]
    pak: String,
}

#[derive(Args)]
struct ApplyArgs {
    #[command(flatten)]
    pak: PakArgs,

    /// Tweak id to turn on. Repeatable.
    #[arg(long = "on", value_name = "ID")]
    on: Vec<String>,

    /// Tweak id to turn off. Repeatable.
    #[arg(long = "off", value_name = "ID")]
    off: Vec<String>,

    /// Turn a slider tweak on at a specific value, as `id=value`. Repeatable.
    #[arg(long = "set", value_name = "ID=VALUE")]
    set: Vec<String>,

    /// Report the edits without writing to the pak.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args)]
struct GetArgs {
    #[command(flatten)]
    pak: PakArgs,

    /// Print this INI file's raw contents instead of the merged CVar state.
    #[arg(long, value_name = "ENTRY")]
    entry: Option<String>,

    /// Print only this CVar's value. Exits non-zero when the pak does not set it.
    #[arg(long, value_name = "KEY", conflicts_with = "entry")]
    key: Option<String>,
}

#[derive(Args)]
struct SetArgs {
    #[command(flatten)]
    pak: PakArgs,

    /// CVar assignments to write, each as `key=value`.
    #[arg(value_name = "KEY=VALUE", required_unless_present = "file")]
    assignments: Vec<String>,

    /// Replace an INI file wholesale from a local file, as `entry=path`. Repeatable.
    #[arg(long = "file", value_name = "ENTRY=PATH")]
    file: Vec<String>,

    /// Engine.ini section for the edits. Defaults to the app's own resolution.
    #[arg(long, value_name = "SECTION")]
    section: Option<String>,

    /// Report the edits without writing to the pak.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args)]
struct UnsetArgs {
    #[command(flatten)]
    pak: PakArgs,

    /// CVars to remove.
    #[arg(value_name = "KEY", required = true)]
    keys: Vec<String>,

    /// Report the edits without writing to the pak.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args)]
struct PaksListArgs {
    /// Include paks holding any INI file, not just the ones the tweak catalogue understands.
    #[arg(long)]
    all_ini: bool,

    /// Search `~mods` subfolders. Defaults to the desktop app's setting.
    #[arg(long)]
    recursive: bool,

    /// Scan only the top level of `~mods`.
    #[arg(long, conflicts_with = "recursive")]
    no_recursive: bool,
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(message) => {
            if cli.json {
                let body = serde_json::json!({ "ok": false, "error": message });
                outln!("{body}");
            } else {
                eprintln!("error: {message}");
            }
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<(), String> {
    let app = settings::load();
    // `--force` is the only way to skip the guard; otherwise the app's own setting decides, so the
    // two front ends agree about whether a running game blocks an edit.
    rivals_core::game_status::set_check_enabled(!cli.force && app.game_running_check_enabled);

    match &cli.command {
        Command::Tweaks(TweaksCmd::List) => tweaks_list(cli),
        Command::Tweaks(TweaksCmd::Status(a)) => tweaks_status(cli, &app, a),
        Command::Tweaks(TweaksCmd::Apply(a)) => tweaks_apply(cli, &app, a),
        Command::Ini(IniCmd::List(a)) => ini_list(cli, &app, a),
        Command::Ini(IniCmd::Get(a)) => ini_get(cli, &app, a),
        Command::Ini(IniCmd::Set(a)) => ini_set(cli, &app, a),
        Command::Ini(IniCmd::Unset(a)) => ini_unset(cli, &app, a),
        Command::Paks(PaksCmd::List(a)) => paks_list(cli, &app, a),
    }
}

fn emit<T: Serialize>(cli: &Cli, value: &T, human: impl FnOnce()) -> Result<(), String> {
    if cli.json {
        let rendered = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
        outln!("{rendered}");
    } else {
        human();
    }
    Ok(())
}

/// A pak edit is refused while the game is running, matching the desktop app's guard.
fn guard_running_game() -> Result<(), String> {
    if rivals_core::game_status::should_block_for_game() {
        return Err(format!(
            "{} Pass --force to edit anyway.",
            rivals_core::game_status::game_running_error()
        ));
    }
    Ok(())
}

fn split_pair<'a>(raw: &'a str, expected: &str) -> Result<(&'a str, &'a str), String> {
    raw.split_once('=')
        .filter(|(left, _)| !left.trim().is_empty())
        .ok_or_else(|| format!("expected {expected}, got `{raw}`"))
}

// ---- tweaks ----

/// One-word summary of what a tweak does to a pak, for the `list` table.
fn kind_label(kind: &TweakKind) -> &'static str {
    match kind {
        TweakKind::RemoveLines { remove_only, .. } => {
            if *remove_only {
                "remove-only"
            } else {
                "lines"
            }
        }
        TweakKind::Toggle { .. } => "toggle",
        TweakKind::BatchToggle { .. } => "batch",
        TweakKind::Slider { .. } => "slider",
    }
}

/// The CVars a tweak writes, so `list` shows what it will actually touch.
fn tweak_keys(def: &TweakDefinition) -> Vec<String> {
    match &def.kind {
        TweakKind::RemoveLines { lines, .. } => lines
            .iter()
            .map(|l| {
                l.pattern
                    .split_once('=')
                    .map_or(l.pattern.as_str(), |(k, _)| k)
                    .to_string()
            })
            .collect(),
        TweakKind::Toggle { key, .. } | TweakKind::Slider { key, .. } => vec![key.clone()],
        TweakKind::BatchToggle { entries, .. } => entries.iter().map(|e| e.key.clone()).collect(),
    }
}

#[derive(Serialize)]
struct TweakRow {
    id: String,
    label: String,
    category: String,
    kind: &'static str,
    pak_only: bool,
    cvars: Vec<String>,
}

fn tweaks_list(cli: &Cli) -> Result<(), String> {
    let rows: Vec<TweakRow> = tweak_catalogue()
        .iter()
        .map(|d| TweakRow {
            id: d.id.clone(),
            label: d.label.clone(),
            category: d.category.clone(),
            kind: kind_label(&d.kind),
            pak_only: d.pak_only,
            cvars: tweak_keys(d),
        })
        .collect();

    emit(cli, &rows, || {
        let width = rows.iter().map(|r| r.id.len()).max().unwrap_or(0);
        let mut category = String::new();
        for row in &rows {
            if row.category != category {
                category = row.category.clone();
                outln!("\n{category}");
            }
            outln!("  {:width$}  {}  {}", row.id, row.kind, row.label);
        }
        outln!("\n{} tweaks", rows.len());
    })
}

#[derive(Serialize)]
struct StatusRow {
    id: String,
    label: String,
    active: bool,
    current_value: Option<String>,
}

fn tweaks_status(cli: &Cli, app: &settings::AppSettings, pak: &PakArgs) -> Result<(), String> {
    let path = resolve::pak(&pak.pak, cli.game_root.as_deref(), app)?;
    let states = pak_tweaks::detect_pak_tweaks(&path)?;
    let catalogue = tweak_catalogue();

    let rows: Vec<StatusRow> = states
        .into_iter()
        .map(|s| {
            let label = catalogue
                .iter()
                .find(|d| d.id == s.id)
                .map_or_else(|| s.id.clone(), |d| d.label.clone());
            StatusRow {
                id: s.id,
                label,
                active: s.active,
                current_value: s.current_value,
            }
        })
        .collect();

    emit(cli, &rows, || {
        let width = rows.iter().map(|r| r.id.len()).max().unwrap_or(0);
        for row in rows.iter().filter(|r| r.active) {
            match &row.current_value {
                Some(v) => outln!("on   {:width$}  {} = {v}", row.id, row.label),
                None => outln!("on   {:width$}  {}", row.id, row.label),
            }
        }
        let on = rows.iter().filter(|r| r.active).count();
        outln!("\n{on} of {} tweaks active", rows.len());
    })
}

#[derive(Serialize)]
struct ApplyResult {
    pak: String,
    edits: Vec<PakTweakEdit>,
    applied: bool,
    message: Option<String>,
}

fn tweaks_apply(cli: &Cli, app: &settings::AppSettings, args: &ApplyArgs) -> Result<(), String> {
    let path = resolve::pak(&args.pak.pak, cli.game_root.as_deref(), app)?;

    let mut settings: Vec<TweakSetting> = Vec::new();
    let mut push = |id: &str, enabled: bool, value: Option<String>| {
        settings.push(TweakSetting {
            id: id.to_string(),
            enabled,
            value,
        });
    };
    for id in &args.on {
        push(id, true, None);
    }
    for id in &args.off {
        push(id, false, None);
    }
    for raw in &args.set {
        let (id, value) = split_pair(raw, "`--set ID=VALUE`")?;
        push(id, true, Some(value.to_string()));
    }

    if settings.is_empty() {
        return Err("nothing to do: pass --on, --off, or --set".to_string());
    }

    // Unknown ids and repeats are rejected by core, which the desktop app shares.
    let edits = pak_tweaks::edits_for_settings(&settings).map_err(|e| {
        if e.starts_with("no tweak with id") {
            format!("{e} (see `rivals-cli tweaks list`)")
        } else {
            e
        }
    })?;

    apply_edits(cli, &path, edits, args.dry_run)
}

// ---- ini ----

#[derive(Serialize)]
struct IniListing {
    pak_name: String,
    pak_path: String,
    ini_entries: Vec<String>,
}

fn ini_list(cli: &Cli, app: &settings::AppSettings, pak: &PakArgs) -> Result<(), String> {
    let path = resolve::pak(&pak.pak, cli.game_root.as_deref(), app)?;
    let listing = pak_tweaks::inspect_single_pak_any_ini(&path)?
        .ok_or_else(|| format!("{path} contains no INI files"))?;
    let listing = IniListing {
        pak_name: listing.pak_name,
        pak_path: listing.pak_path,
        ini_entries: listing.ini_entries,
    };

    emit(cli, &listing, || {
        for entry in &listing.ini_entries {
            outln!("{entry}");
        }
    })
}

fn ini_get(cli: &Cli, app: &settings::AppSettings, args: &GetArgs) -> Result<(), String> {
    let path = resolve::pak(&args.pak.pak, cli.game_root.as_deref(), app)?;

    if let Some(entry) = &args.entry {
        let content = pak_tweaks::extract_pak_ini(&path, entry)?;
        return emit(
            cli,
            &serde_json::json!({ "entry": entry, "content": content }),
            || out!("{content}"),
        );
    }

    let cvars = pak_tweaks::read_pak_cvars(&path)?;

    if let Some(key) = &args.key {
        let hit = cvars
            .iter()
            .find(|c| c.key.eq_ignore_ascii_case(key))
            .ok_or_else(|| format!("{path} does not set '{key}'"))?;
        return emit(cli, hit, || outln!("{}", hit.value));
    }

    emit(cli, &cvars, || {
        let width = cvars.iter().map(|c| c.key.len()).max().unwrap_or(0);
        for cvar in &cvars {
            outln!("{:width$} = {}  ({})", cvar.key, cvar.value, cvar.source);
        }
    })
}

fn ini_set(cli: &Cli, app: &settings::AppSettings, args: &SetArgs) -> Result<(), String> {
    let path = resolve::pak(&args.pak.pak, cli.game_root.as_deref(), app)?;

    if !args.file.is_empty() {
        let mut files = Vec::new();
        for raw in &args.file {
            let (entry, source) = split_pair(raw, "`--file ENTRY=PATH`")?;
            let content = std::fs::read_to_string(source)
                .map_err(|e| format!("cannot read {source}: {e}"))?;
            files.push(PakIniFileContent {
                entry: entry.to_string(),
                content,
            });
        }
        if args.dry_run {
            let names: Vec<&str> = files.iter().map(|f| f.entry.as_str()).collect();
            return emit(
                cli,
                &serde_json::json!({ "pak": path, "replaces": names, "applied": false }),
                || outln!("would replace {} INI file(s) in {path}", names.len()),
            );
        }
        guard_running_game()?;
        let message = pak_tweaks::save_pak_ini(&path, files, Vec::new())?;
        return emit(
            cli,
            &serde_json::json!({ "pak": path, "applied": true, "message": message }),
            || outln!("{message}"),
        );
    }

    let mut edits = Vec::new();
    for raw in &args.assignments {
        let (key, value) = split_pair(raw, "`KEY=VALUE`")?;
        edits.push(PakTweakEdit {
            key: key.trim().to_string(),
            value: Some(value.to_string()),
            engine_section: args.section.clone(),
        });
    }
    apply_edits(cli, &path, edits, args.dry_run)
}

fn ini_unset(cli: &Cli, app: &settings::AppSettings, args: &UnsetArgs) -> Result<(), String> {
    let path = resolve::pak(&args.pak.pak, cli.game_root.as_deref(), app)?;
    let edits = args
        .keys
        .iter()
        .map(|key| PakTweakEdit {
            key: key.trim().to_string(),
            value: None,
            engine_section: None,
        })
        .collect();
    apply_edits(cli, &path, edits, args.dry_run)
}

fn apply_edits(
    cli: &Cli,
    path: &str,
    edits: Vec<PakTweakEdit>,
    dry_run: bool,
) -> Result<(), String> {
    let describe = |e: &PakTweakEdit| match &e.value {
        Some(v) => format!("{} = {v}", e.key),
        None => format!("{} (removed)", e.key),
    };

    if dry_run {
        let result = ApplyResult {
            pak: path.to_string(),
            edits,
            applied: false,
            message: None,
        };
        return emit(cli, &result, || {
            for edit in &result.edits {
                outln!("would set {}", describe(edit));
            }
        });
    }

    guard_running_game()?;
    let message = pak_tweaks::apply_pak_tweaks(path, &edits)?;
    let result = ApplyResult {
        pak: path.to_string(),
        edits,
        applied: true,
        message: Some(message),
    };
    emit(cli, &result, || {
        for edit in &result.edits {
            outln!("set {}", describe(edit));
        }
        if let Some(message) = &result.message {
            outln!("{message}");
        }
    })
}

// ---- paks ----

fn paks_list(cli: &Cli, app: &settings::AppSettings, args: &PaksListArgs) -> Result<(), String> {
    let game_root = resolve::game_root(cli.game_root.as_deref(), app)?;
    // Neither flag given means "whatever the desktop app is set to".
    let recursive = match (args.recursive, args.no_recursive) {
        (true, _) => true,
        (_, true) => false,
        _ => app.recursive_mod_scan,
    };

    if args.all_ini {
        let found = pak_tweaks::scan_mod_paks_any_ini(&game_root, recursive)?;
        return emit(cli, &found, || {
            for pak in &found.paks {
                outln!("{}  ({} INI files)", pak.pak_name, pak.ini_entries.len());
            }
            report_unreadable(found.unreadable.len(), found.paks.len());
            for bad in &found.unreadable {
                eprintln!("  {}: {}", bad.pak_name, bad.error);
            }
        });
    }

    let found = pak_tweaks::scan_mod_paks(&game_root, recursive)?;
    emit(cli, &found, || {
        for pak in &found.paks {
            let mut kinds = Vec::new();
            if pak.has_device_profiles {
                kinds.push("DeviceProfiles");
            }
            if pak.has_windows_engine {
                kinds.push("WindowsEngine");
            }
            if pak.has_engine_ini {
                kinds.push("Engine");
            }
            if pak.has_base_engine {
                kinds.push("BaseEngine");
            }
            outln!("{}  [{}]", pak.pak_name, kinds.join(", "));
        }
        report_unreadable(found.unreadable.len(), found.paks.len());
        for bad in &found.unreadable {
            eprintln!("  {}: {}", bad.pak_name, bad.error);
        }
    })
}

fn report_unreadable(unreadable: usize, paks: usize) {
    outln!("\n{paks} pak(s) with editable INI files");
    if unreadable > 0 {
        eprintln!("{unreadable} pak(s) could not be read:");
    }
}
