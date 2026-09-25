//! Command-line front end for scripting Marvel Rivals pak INI edits and config tweaks.

#![deny(clippy::unwrap_used, clippy::expect_used)]

mod asset;
mod resolve;
mod settings;

use clap::{Args, Parser, Subcommand, ValueEnum};
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

    /// Path to a .usmap mappings file, or a folder holding one. Reading asset properties needs it.
    #[arg(long, global = true, value_name = "PATH")]
    usmap: Option<String>,

    /// A text file of native object paths the game's script objects table leaves out, one per
    /// line, added to the bundled list so their imports read by name.
    #[arg(long, global = true, value_name = "PATH")]
    script_objects: Option<String>,

    /// What an asset write leaves behind. The game reads packages only from an IoStore container;
    /// a plain pak is for tooling that converts it onward itself. Defaults to the app's setting.
    #[arg(long, global = true, value_enum, value_name = "KIND")]
    target: Option<TargetArg>,

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

    /// Read .uasset contents from a pak or IoStore container.
    #[command(subcommand)]
    Asset(AssetCmd),
}

#[derive(Subcommand)]
enum AssetCmd {
    /// List package paths inside a container.
    List(ListArgs),
    /// Summarise a package: flags, counts, and every export with its parse status.
    Info(AssetArgs),
    /// Print the decoded property tree.
    Dump(DumpArgs),
    /// Print a DataTable as rows.
    Table(AssetArgs),
    /// Print the byte range every property consumed, to locate a desync.
    Trace(DumpArgs),
    /// Print one export's raw bytes at the offsets the trace reports.
    Hex(HexArgs),
    /// Print the field records a class or struct export declares
    Fields(FieldsArgs),
    /// Disassemble the bytecode a function or class export stores.
    Script(FieldsArgs),
    /// Change a stored value and write the result into a mod pak.
    Set(AssetSetArgs),
    /// Set the same properties by name across every package a filter matches, in one mod.
    Sweep(SweepArgs),
    /// Point an import at another object, or add one, and write the result into a mod pak.
    Import(ImportArgs),
    /// Remove exports from a package, subobjects included, and write the result into a mod pak.
    RemoveExport(RemoveExportArgs),
    /// Drop every value an export stores so it inherits its class defaults, and write the result
    /// into a mod pak.
    ResetExport(ResetExportArgs),
    /// Copy an export and its subobjects to the end of the export table under a new name, and
    /// write the result into a mod pak.
    DuplicateExport(DuplicateExportArgs),
    /// Add, copy, rename or remove a DataTable row and write the result into a mod pak.
    Row(RowArgs),
    /// Change, add or remove a StringTable entry and write the result into a mod pak.
    Strings(StringsArgs),
    /// Add, copy or remove a MovieScene channel key and write the result into a mod pak.
    Keys(KeysArgs),
    /// Save an export's payload bytes to a file, or replace them from one.
    Payload(PayloadArgs),
    /// List the bulk data resources, save one's bytes to a file, or replace them from one.
    Bulk(BulkArgs),
    /// List the packages that import an object or a package, from the import index.
    Importers(ImportersArgs),
    /// Rename an export, move it, or change the flags the loader reads it by.
    ExportEdit(ExportEditArgs),
    /// List the import table with what names each entry, and which are named by nothing.
    Imports(ImportsArgs),
    /// Print the package name table that FName indices resolve against.
    Names(AssetArgs),
    /// Apply an edit file, or a manifest of them, writing each into its mod pak.
    Apply(AssetApplyArgs),
    /// Compare an edited JSON dump against the package it came from and write the edit file that
    /// would reproduce it.
    Diff(AssetDiffArgs),
    /// Show or replace the preload dependency runs an export declares.
    Deps(DepsArgs),
    /// Copy an export, and everything under it, out of another package into this one.
    CopyExport(CopyExportArgs),
    /// Parse every package in a container and report how much of it decodes cleanly.
    Audit(AuditArgs),
    /// Re-parse failing packages with one schema slot elided at a time, to find mappings entries
    /// the shipped build does not serialize.
    Diagnose(DiagnoseArgs),
    /// Read Blueprint struct definitions out of packages and check them against the mappings.
    SynthCheck(AuditArgs),
}

#[derive(Args)]
struct ListArgs {
    /// IoStore container to enumerate. A patch declares every package of the game in its header,
    /// so listing one lists them all.
    #[arg(long, value_name = "PATH")]
    container: String,

    /// Keep only paths containing this text, case insensitive.
    #[arg(long, value_name = "TEXT")]
    filter: Option<String>,
}

#[derive(Args)]
struct AssetArgs {
    /// Container holding the asset: a .pak or a .utoc. Omit when using `--file`.
    #[arg(long, value_name = "PATH", conflicts_with = "file")]
    container: Option<String>,

    /// Asset path inside the container, such as `Marvel/Content/.../DT_Thing.uasset`.
    #[arg(long, value_name = "PATH", required_unless_present = "file")]
    entry: Option<String>,

    /// An already-extracted .uasset on disk, read with its siblings.
    #[arg(long, value_name = "PATH")]
    file: Option<String>,

    /// Also list the properties an export declares but does not store, which take the value the
    /// object inherits. Their offsets are where `asset set` would store one.
    #[arg(long)]
    declared: bool,
}

#[derive(Args)]
struct DumpArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// Print only this export index.
    #[arg(long, value_name = "N")]
    export: Option<u32>,
}

#[derive(Args)]
struct HexArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// Export index to dump.
    #[arg(long, value_name = "N")]
    export: u32,

    /// Start at this file offset instead of the export's start.
    #[arg(long, value_name = "OFFSET", value_parser = parse_offset)]
    from: Option<u64>,
}

#[derive(Args)]
struct FieldsArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// Export index of the class or struct, as `asset info` prints it.
    #[arg(long, value_name = "N")]
    export: u32,
}

fn parse_offset(text: &str) -> Result<u64, String> {
    let trimmed = text.trim_start_matches("0x").trim_start_matches("0X");
    let radix = if trimmed.len() == text.len() { 10 } else { 16 };
    u64::from_str_radix(trimmed, radix).map_err(|e| e.to_string())
}

#[derive(Args)]
struct SweepArgs {
    /// IoStore container to sweep. Every package it declares is a candidate.
    #[arg(long, value_name = "PATH")]
    container: String,

    /// Keep only packages whose path contains this text, case insensitive.
    #[arg(long, value_name = "TEXT")]
    filter: Option<String>,

    /// Keep only packages holding an export of this class, such as `LegacyCameraShakePattern`.
    /// Narrows `--filter` rather than replacing it, and reads every package the filter allows.
    #[arg(long, value_name = "CLASS")]
    class: Option<String>,

    /// `Name=Value`: every property with that name, at any depth, takes that value. Repeatable.
    #[arg(long = "set", value_name = "NAME=VALUE", required = true)]
    sets: Vec<String>,

    /// Build on the mod's own copies where it already has them, instead of reading the source
    /// container again. Without it a second sweep over the same packages replaces what the first
    /// one wrote.
    #[arg(long)]
    layer: bool,

    /// Read and patch everything, write nothing.
    #[arg(long)]
    dry_run: bool,

    /// Stop after this many matching packages, to try a sweep out on a handful first.
    #[arg(long, value_name = "N")]
    limit: Option<usize>,

    /// Mod to write into. Its packages are replaced; whatever else it holds is carried over.
    #[arg(long, value_name = "NAME")]
    mod_name: Option<String>,
}

#[derive(Args)]
struct AssetSetArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// File offset of the value, as reported by `asset dump` or `asset trace`.
    #[arg(long, value_name = "OFFSET", value_parser = parse_offset)]
    offset: u64,

    /// The kind the value is expected to be, so a stale offset is refused rather than written.
    #[arg(long, value_name = "KIND")]
    kind: String,

    /// The property's name. A value holding its default occupies no bytes, so it shares its
    /// offset with the one stored next and the name is what picks between them.
    #[arg(long, value_name = "NAME")]
    name: String,

    /// Which slot of a static array, when the property declares more than one.
    #[arg(long, value_name = "N")]
    element: Option<u32>,

    /// `set` writes `--value`; `clear` flags the property as zero; `store` gives an unset struct,
    /// container or reference its empty form; `unset` drops it so the inherited value applies;
    /// `set-element`, `insert` and `remove` act on the container element at `--index`. In a set
    /// or a map, `insert` takes the new element's key from `--value`.
    #[arg(long, value_name = "OP", default_value = "set")]
    op: String,

    /// Which element of a container to act on.
    #[arg(long, value_name = "N")]
    index: Option<u32>,

    /// The new value.
    #[arg(long, value_name = "TEXT", default_value = "")]
    value: String,

    /// Mod pak to write into, created in `~mods` if it does not exist. Defaults to the name the
    /// desktop app last saved into, then to `AssetEdits`.
    #[arg(long, value_name = "NAME")]
    mod_name: Option<String>,

    /// Overwrite an edited copy of this asset that the mod pak already holds. Without it the
    /// command refuses, because the copy's earlier edits would be lost.
    #[arg(long)]
    replace: bool,
}

#[derive(Args)]
#[command(allow_negative_numbers = true)]
struct ImportArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// Which import to retarget, as the negative index `asset info` prints. Omit to add one.
    #[arg(long, value_name = "N")]
    index: Option<i32>,

    /// The object, in UE's dotted form: `/Game/Path/Asset.Object`, or `:Sub` for a subobject.
    #[arg(long, value_name = "PATH", required_unless_present = "remove")]
    path: Option<String>,

    /// Drop this import instead, as the negative index `asset info` prints. Repeatable. Every
    /// index above it moves down, so this is a save of its own.
    #[arg(long, value_name = "N", conflicts_with_all = ["index", "path"])]
    remove: Vec<i32>,

    /// Report what removing would do, and write nothing.
    #[arg(long)]
    dry_run: bool,

    /// The object's class, as its package and name. A retarget keeps the old class when these are
    /// omitted; an add falls back to `/Script/CoreUObject` `Object`, which the loader accepts for
    /// any object.
    #[arg(long, value_name = "PACKAGE", requires = "class_name")]
    class_package: Option<String>,

    #[arg(long, value_name = "NAME", requires = "class_package")]
    class_name: Option<String>,

    /// Mod pak to write into, created in `~mods` if it does not exist. Defaults to the name the
    /// desktop app last saved into, then to `AssetEdits`.
    #[arg(long, value_name = "NAME")]
    mod_name: Option<String>,

    /// Overwrite an edited copy of this asset that the mod pak already holds.
    #[arg(long)]
    replace: bool,
}

#[derive(Args)]
struct RemoveExportArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// Export index to remove, as `asset info` prints it. Repeat to remove several.
    #[arg(long = "export", value_name = "N", required = true)]
    exports: Vec<u32>,

    /// Print what the removal would do and stop.
    #[arg(long)]
    dry_run: bool,

    /// Go ahead although the plan carries warnings.
    #[arg(long)]
    accept_warnings: bool,

    /// Mod pak to write into, created in `~mods` if it does not exist. Defaults to the name the
    /// desktop app last saved into, then to `AssetEdits`.
    #[arg(long, value_name = "NAME")]
    mod_name: Option<String>,

    /// Overwrite an edited copy of this asset that the mod pak already holds.
    #[arg(long)]
    replace: bool,
}

#[derive(Args)]
struct ImportersArgs {
    /// The object in UE's dotted form, `/Game/Path/Asset.Object:Sub`, or a bare package path to
    /// list everything that imports any of its objects.
    #[arg(long, value_name = "PATH")]
    path: String,

    /// Build the index first, or rebuild it after the game updated. Reads every package once.
    #[arg(long)]
    build: bool,
}

#[derive(Args)]
struct ResetExportArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// Export index to reset, as `asset info` prints it.
    #[arg(long, value_name = "N")]
    export: u32,

    /// Mod pak to write into, created in `~mods` if it does not exist. Defaults to the name the
    /// desktop app last saved into, then to `AssetEdits`.
    #[arg(long, value_name = "NAME")]
    mod_name: Option<String>,

    /// Overwrite an edited copy of this asset that the mod pak already holds.
    #[arg(long)]
    replace: bool,
}

#[derive(Args)]
struct RowArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// Export index of the DataTable, as `asset info` prints it.
    #[arg(long, value_name = "N")]
    export: u32,

    /// What to do: `add`, `duplicate`, `remove` or `rename`.
    #[arg(long, value_name = "OP")]
    op: String,

    /// The row the operation is about: the name to add, or the row to copy, remove or rename.
    #[arg(long, value_name = "NAME")]
    row: String,

    /// The new name a copy or a rename takes.
    #[arg(long, value_name = "NAME")]
    name: Option<String>,

    /// Where an added or copied row goes: in front of the row at this position, counting from 0.
    /// Last when omitted.
    #[arg(long, value_name = "N")]
    at: Option<u32>,

    /// Mod pak to write into, created in `~mods` if it does not exist. Defaults to the name the
    /// desktop app last saved into, then to `AssetEdits`.
    #[arg(long, value_name = "NAME")]
    mod_name: Option<String>,

    /// Overwrite an edited copy of this asset that the mod pak already holds.
    #[arg(long)]
    replace: bool,
}

#[derive(Args)]
struct StringsArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// Export index of the StringTable, as `asset info` prints it.
    #[arg(long, value_name = "N")]
    export: u32,

    /// What to do: `set-key`, `set-source`, `set-tag`, `add`, `remove`, `meta-set` or
    /// `meta-remove`.
    #[arg(long, value_name = "OP")]
    op: String,

    /// Position of the entry, counting from 0, for everything but `add`.
    #[arg(long, value_name = "N")]
    index: Option<u32>,

    /// The entry's key as it reads now, or the key an added entry takes.
    #[arg(long, value_name = "KEY")]
    key: String,

    /// The new key, source string or tag, an added entry's source string, or a metadata value.
    #[arg(long, value_name = "TEXT")]
    text: Option<String>,

    /// The metadata item's id, for `meta-set` and `meta-remove`.
    #[arg(long, value_name = "ID")]
    id: Option<String>,

    /// Mod pak to write into, created in `~mods` if it does not exist. Defaults to the name the
    /// desktop app last saved into, then to `AssetEdits`.
    #[arg(long, value_name = "NAME")]
    mod_name: Option<String>,

    /// Overwrite an edited copy of this asset that the mod pak already holds.
    #[arg(long)]
    replace: bool,
}

#[derive(Args)]
struct KeysArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// File offset of the channel, as reported by `asset dump` or `asset trace`.
    #[arg(long, value_name = "OFFSET", value_parser = parse_offset)]
    offset: u64,

    /// The channel property's name, so a stale offset is refused rather than written.
    #[arg(long, value_name = "NAME")]
    name: String,

    /// Which slot of a static array, when the property declares more than one.
    #[arg(long, value_name = "N")]
    element: Option<u32>,

    /// `add` puts a key at `--time` holding `--value`; `duplicate` copies key `--index` to
    /// `--time`; `move` carries key `--index` to `--time`; `remove` drops key `--index`.
    #[arg(long, value_name = "OP")]
    op: String,

    /// Which key, counting from 0 in frame order.
    #[arg(long, value_name = "N")]
    index: Option<u32>,

    /// The frame of the new key.
    #[arg(long, value_name = "FRAME")]
    time: Option<i32>,

    /// The value of the added key.
    #[arg(long, value_name = "NUMBER")]
    value: Option<f64>,

    /// Mod pak to write into, created in `~mods` if it does not exist. Defaults to the name the
    /// desktop app last saved into, then to `AssetEdits`.
    #[arg(long, value_name = "NAME")]
    mod_name: Option<String>,

    /// Overwrite an edited copy of this asset that the mod pak already holds.
    #[arg(long)]
    replace: bool,
}

#[derive(Args)]
struct DuplicateExportArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// Export index to copy, as `asset info` prints it.
    #[arg(long, value_name = "N")]
    export: u32,

    /// The copy's object name, unique beside the original.
    #[arg(long, value_name = "NAME")]
    name: String,

    /// List the copy in this Level export's actors, so the game spawns it. Without this the copy
    /// loads with the package and never appears.
    #[arg(long, value_name = "N")]
    into_level: Option<u32>,

    /// Mod pak to write into, created in `~mods` if it does not exist. Defaults to the name the
    /// desktop app last saved into, then to `AssetEdits`.
    #[arg(long, value_name = "NAME")]
    mod_name: Option<String>,

    /// Overwrite an edited copy of this asset that the mod pak already holds.
    #[arg(long)]
    replace: bool,
}

#[derive(Args)]
struct PayloadArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// Export index, as `asset info` prints it.
    #[arg(long, value_name = "N")]
    export: u32,

    /// Write the payload's bytes to this file.
    #[arg(long, value_name = "PATH", conflicts_with = "input")]
    out: Option<String>,

    /// Replace the payload with this file's bytes and write the result into a mod pak.
    #[arg(long = "in", value_name = "PATH")]
    input: Option<String>,

    /// Mod pak to write into, created in `~mods` if it does not exist. Defaults to the name the
    /// desktop app last saved into, then to `AssetEdits`.
    #[arg(long, value_name = "NAME")]
    mod_name: Option<String>,

    /// Overwrite an edited copy of this asset that the mod pak already holds.
    #[arg(long)]
    replace: bool,
}

#[derive(Args)]
struct BulkArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// `list` prints every resource; `export` writes one's bytes to `--out`; `replace` swaps them
    /// for the bytes of `--in`.
    #[arg(long, value_name = "OP", default_value = "list")]
    op: String,

    /// Which resource, by its position in the bulk data table.
    #[arg(long, value_name = "N")]
    resource: Option<u32>,

    /// Where `export` writes the bytes.
    #[arg(long, value_name = "PATH")]
    out: Option<String>,

    /// The file `replace` takes the bytes from.
    #[arg(long = "in", value_name = "PATH")]
    input: Option<String>,

    /// Mod pak to write into, created in `~mods` if it does not exist. Defaults to the name the
    /// desktop app last saved into, then to `AssetEdits`.
    #[arg(long, value_name = "NAME")]
    mod_name: Option<String>,

    /// Overwrite an edited copy of this asset that the mod pak already holds.
    #[arg(long)]
    replace: bool,
}

#[derive(Args)]
struct AuditArgs {
    /// IoStore container to walk. Mutually exclusive with `--dir`.
    #[arg(long, value_name = "PATH", conflicts_with = "dir")]
    container: Option<String>,

    /// Folder of already-extracted .uasset files to walk instead of a container.
    #[arg(long, value_name = "DIR")]
    dir: Option<String>,

    /// Stop after this many packages.
    #[arg(long, value_name = "N")]
    limit: Option<usize>,

    /// Only walk packages whose path contains this text, case insensitive.
    #[arg(long, value_name = "TEXT")]
    filter: Option<String>,

    /// Leave out exports whose class is Blueprint-generated. Only a mappings dump taken with
    /// those Blueprints loaded describes them, so a native-only dump reports them as gaps that
    /// say nothing about native coverage.
    #[arg(long)]
    skip_blueprint: bool,

    /// Report progress to stderr while scanning.
    #[arg(long)]
    progress: bool,
}

#[derive(Args)]
struct DiagnoseArgs {
    #[command(flatten)]
    audit: AuditArgs,

    /// Only sweep this struct, instead of every struct that appears in a failure.
    #[arg(long = "struct", value_name = "NAME")]
    only: Option<String>,

    /// How many slots to elide at once when no single one helps.
    #[arg(long, value_name = "N", default_value_t = 1)]
    depth: usize,
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
    /// Set CVars in every INI the pak ships, the same way the app does.
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
    let script_objects = rivals_core::script_objects::resolve(
        cli.script_objects.as_deref(),
        app.extra_script_objects_path.as_deref(),
    )?;
    if let Some(path) = &script_objects {
        rivals_core::script_objects::load(path)?;
    }
    rivals_core::script_objects::set_user_list(script_objects);

    match &cli.command {
        Command::Tweaks(TweaksCmd::List) => tweaks_list(cli),
        Command::Tweaks(TweaksCmd::Status(a)) => tweaks_status(cli, &app, a),
        Command::Tweaks(TweaksCmd::Apply(a)) => tweaks_apply(cli, &app, a),
        Command::Ini(IniCmd::List(a)) => ini_list(cli, &app, a),
        Command::Ini(IniCmd::Get(a)) => ini_get(cli, &app, a),
        Command::Ini(IniCmd::Set(a)) => ini_set(cli, &app, a),
        Command::Ini(IniCmd::Unset(a)) => ini_unset(cli, &app, a),
        Command::Paks(PaksCmd::List(a)) => paks_list(cli, &app, a),
        Command::Asset(AssetCmd::List(a)) => asset_list(cli, &app, a),
        Command::Asset(AssetCmd::Info(a)) => asset_info(cli, &app, a),
        Command::Asset(AssetCmd::Dump(a)) => asset_dump(cli, &app, a),
        Command::Asset(AssetCmd::Table(a)) => asset_table(cli, &app, a),
        Command::Asset(AssetCmd::Trace(a)) => asset_trace(cli, &app, a),
        Command::Asset(AssetCmd::Hex(a)) => asset_hex(cli, &app, a),
        Command::Asset(AssetCmd::Fields(a)) => asset_fields(cli, &app, a),
        Command::Asset(AssetCmd::Script(a)) => asset_script(cli, &app, a),
        Command::Asset(AssetCmd::Set(a)) => asset_set(cli, &app, a),
        Command::Asset(AssetCmd::Sweep(a)) => asset_sweep(cli, &app, a),
        Command::Asset(AssetCmd::Import(a)) => asset_import(cli, &app, a),
        Command::Asset(AssetCmd::RemoveExport(a)) => asset_remove_export(cli, &app, a),
        Command::Asset(AssetCmd::ResetExport(a)) => asset_reset_export(cli, &app, a),
        Command::Asset(AssetCmd::Row(a)) => asset_row(cli, &app, a),
        Command::Asset(AssetCmd::Strings(a)) => asset_strings(cli, &app, a),
        Command::Asset(AssetCmd::Keys(a)) => asset_keys(cli, &app, a),
        Command::Asset(AssetCmd::Payload(a)) => asset_payload(cli, &app, a),
        Command::Asset(AssetCmd::DuplicateExport(a)) => asset_duplicate_export(cli, &app, a),
        Command::Asset(AssetCmd::Bulk(a)) => asset_bulk(cli, &app, a),
        Command::Asset(AssetCmd::Importers(a)) => asset_importers(cli, &app, a),
        Command::Asset(AssetCmd::ExportEdit(a)) => asset_export_edit(cli, &app, a),
        Command::Asset(AssetCmd::Imports(a)) => asset_imports_table(cli, &app, a),
        Command::Asset(AssetCmd::Names(a)) => asset_names(cli, &app, a),
        Command::Asset(AssetCmd::Apply(a)) => asset_apply(cli, &app, a),
        Command::Asset(AssetCmd::Diff(a)) => asset_diff(cli, &app, a),
        Command::Asset(AssetCmd::Deps(a)) => asset_deps(cli, &app, a),
        Command::Asset(AssetCmd::CopyExport(a)) => asset_copy_export(cli, &app, a),
        Command::Asset(AssetCmd::Audit(a)) => asset_audit(cli, &app, a),
        Command::Asset(AssetCmd::Diagnose(a)) => asset_diagnose(cli, &app, a),
        Command::Asset(AssetCmd::SynthCheck(a)) => asset_synth_check(cli, &app, a),
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

    // Unknown ids and repeats are rejected by core, which the desktop app shares. The error can
    // name several entries at once, so the hint goes on its own line rather than trailing the
    // last one.
    let edits = pak_tweaks::edits_for_settings(&settings).map_err(|e| {
        if e.contains("no tweak with id") {
            format!("{e}\nRun `rivals-cli tweaks list` for valid ids.")
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
                staged_path: None,
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
            if pak.has_base_device_profiles {
                kinds.push("BaseDeviceProfiles");
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

// ---- asset ----

/// A `--file` target carries no container, which is how the loose source is selected.
fn asset_request<'a>(
    cli: &'a Cli,
    app: &'a settings::AppSettings,
    args: &'a AssetArgs,
    game_root: &'a str,
) -> asset::Request<'a> {
    asset::Request {
        game_root,
        container: args.container.as_deref().unwrap_or_default(),
        entry: args
            .file
            .as_deref()
            .or(args.entry.as_deref())
            .unwrap_or_default(),
        usmap: cli.usmap.as_deref(),
        configured_usmap: app.usmap_path.as_deref(),
        declared: args.declared,
        target: cli
            .target
            .map(Into::into)
            .or(app.asset_save_target)
            .unwrap_or_default(),
    }
}

fn asset_list(cli: &Cli, app: &settings::AppSettings, args: &ListArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let paths = asset::list(&root, &args.container, args.filter.as_deref())?;
    emit(cli, &paths, || {
        for path in &paths {
            outln!("{path}");
        }
        outln!(
            "
{} package(s)",
            paths.len()
        );
    })
}

fn asset_info(cli: &Cli, app: &settings::AppSettings, args: &AssetArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let report = asset::info(&asset_request(cli, app, args, &root))?;
    emit(cli, &report, || {
        asset::print_info(&report, &mut |line| outln!("{line}"));
    })
}

fn asset_dump(cli: &Cli, app: &settings::AppSettings, args: &DumpArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let parsed = asset::dump(&asset_request(cli, app, &args.asset, &root), args.export)?;
    emit(cli, &parsed, || {
        asset::print_dump(&parsed, &mut |line| outln!("{line}"));
    })
}

fn asset_table(cli: &Cli, app: &settings::AppSettings, args: &AssetArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let report = asset::table(&asset_request(cli, app, args, &root))?;
    emit(cli, &report, || {
        asset::print_table(&report, &mut |line| outln!("{line}"));
    })
}

fn asset_trace(cli: &Cli, app: &settings::AppSettings, args: &DumpArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let lines = asset::trace(&asset_request(cli, app, &args.asset, &root), args.export)?;
    emit(cli, &lines, || {
        for line in &lines {
            outln!("{line}");
        }
    })
}

fn asset_hex(cli: &Cli, app: &settings::AppSettings, args: &HexArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let lines = asset::hex(
        &asset_request(cli, app, &args.asset, &root),
        args.export,
        args.from,
    )?;
    emit(cli, &lines, || {
        for line in &lines {
            outln!("{line}");
        }
    })
}

fn asset_fields(cli: &Cli, app: &settings::AppSettings, args: &FieldsArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let lines = asset::fields(&asset_request(cli, app, &args.asset, &root), args.export)?;
    emit(cli, &lines, || {
        for line in &lines {
            outln!("{line}");
        }
    })
}

#[derive(Args)]
#[command(allow_negative_numbers = true)]
struct ExportEditArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// Export index, as `asset info` prints it.
    #[arg(long, value_name = "N")]
    export: u32,

    /// A new object name.
    #[arg(long, value_name = "NAME")]
    rename: Option<String>,

    /// Move the object under this export, as `asset info` prints it.
    #[arg(long, value_name = "N")]
    outer: Option<u32>,

    /// Point the object at this archetype, which its unset values inherit from.
    #[arg(long, value_name = "N")]
    template: Option<i32>,

    /// Retype the object to the class at this package index. Its stored values do not survive:
    /// the export is emptied and takes the new class's defaults.
    #[arg(long, value_name = "N")]
    class: Option<i32>,

    /// Reparent a class, struct or enum to the one at this package index.
    #[arg(long, value_name = "N")]
    parent: Option<i32>,

    /// Flags to turn on, by name: Public, Standalone, Transactional, ArchetypeObject and so on.
    #[arg(long, value_name = "NAME", num_args = 1..)]
    set_flag: Vec<String>,

    /// Flags to turn off, by the same names.
    #[arg(long, value_name = "NAME", num_args = 1..)]
    clear_flag: Vec<String>,

    /// Whether other packages may import the object by hash.
    #[arg(long)]
    public_hash: Option<bool>,

    /// Report what would change and write nothing.
    #[arg(long)]
    dry_run: bool,

    /// Mod to write into. Defaults to the name the desktop app last saved into.
    #[arg(long, value_name = "NAME")]
    mod_name: Option<String>,

    #[arg(long)]
    replace: bool,
}

fn flag_mask(names: &[String]) -> Result<u32, String> {
    let mut mask = 0u32;
    for name in names {
        let found = rivals_uasset::EDITABLE_FLAGS
            .iter()
            .find(|(_, known)| known.eq_ignore_ascii_case(name))
            .ok_or_else(|| {
                format!(
                    "{name} is not a flag this editor writes; the ones it does are {}",
                    rivals_uasset::EDITABLE_FLAGS
                        .iter()
                        .map(|(_, known)| *known)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;
        mask |= found.0;
    }
    Ok(mask)
}

fn asset_export_edit(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &ExportEditArgs,
) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let request = asset_request(cli, app, &args.asset, &root);
    let export = args.export;
    let mut edits = Vec::new();
    if let Some(name) = &args.rename {
        edits.push(rivals_uasset::ExportEdit::Rename {
            export,
            name: name.clone(),
        });
    }
    if let Some(outer) = args.outer {
        edits.push(rivals_uasset::ExportEdit::SetOuter { export, outer });
    }
    if let Some(template) = args.template {
        edits.push(rivals_uasset::ExportEdit::SetTemplate { export, template });
    }
    // A retype empties the export, so the reset goes in the same save rather than being asked for.
    let mut resets = Vec::new();
    if let Some(class) = args.class {
        edits.push(rivals_uasset::ExportEdit::SetClass { export, class });
        resets.push(export);
    }
    if let Some(super_index) = args.parent {
        edits.push(rivals_uasset::ExportEdit::SetSuper {
            export,
            super_index,
        });
    }
    if !args.set_flag.is_empty() || !args.clear_flag.is_empty() {
        edits.push(rivals_uasset::ExportEdit::SetFlags {
            export,
            set: flag_mask(&args.set_flag)?,
            clear: flag_mask(&args.clear_flag)?,
        });
    }
    if let Some(on) = args.public_hash {
        edits.push(rivals_uasset::ExportEdit::SetPublicHash { export, on });
    }
    if edits.is_empty() {
        return Err("name a change: --rename, --outer, --class, --parent, --template, --set-flag, --clear-flag or --public-hash".to_string());
    }
    let plan = asset::plan_export_edits(&request, &edits, &resets)?;
    if args.dry_run || !plan.blockers.is_empty() {
        let failed = !plan.blockers.is_empty();
        emit(cli, &plan, || {
            asset::print_export_plan(&plan, &mut |line| outln!("{line}"))
        })?;
        return if failed {
            Err("these changes cannot be made".to_string())
        } else {
            Ok(())
        };
    }
    if !cli.force && rivals_core::game_status::should_block_for_game() {
        return Err(rivals_core::game_status::game_running_error());
    }
    let message = asset::export_edit(
        &request,
        edits,
        resets,
        mod_name_of(app, args.mod_name.as_deref()),
        args.replace,
    )?;
    emit(cli, &message, || outln!("{message}"))
}

#[derive(Args)]
struct ImportsArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// Only the imports nothing in the package names.
    #[arg(long)]
    unused: bool,
}

fn asset_imports_table(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &ImportsArgs,
) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let report = asset::imports(&asset_request(cli, app, &args.asset, &root), args.unused)?;
    emit(cli, &report, || {
        asset::print_imports(&report, &mut |line| outln!("{line}"))
    })
}

fn asset_script(cli: &Cli, app: &settings::AppSettings, args: &FieldsArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let report = asset::script(&asset_request(cli, app, &args.asset, &root), args.export)?;
    emit(cli, &report, || {
        asset::print_script(&report, &mut |line| outln!("{line}"))
    })
}

/// Where asset edits go when neither the flag nor the desktop app has chosen a mod.
const DEFAULT_MOD_NAME: &str = "AssetEdits";

#[derive(Args)]
struct AssetApplyArgs {
    /// One edit file: a container, an entry and the changes to make.
    #[arg(
        long,
        value_name = "FILE",
        conflicts_with = "manifest",
        required_unless_present = "manifest"
    )]
    edits: Option<String>,
    /// A manifest naming several edit files, or holding them inline. Each is applied on its own.
    #[arg(long, value_name = "FILE")]
    manifest: Option<String>,
    /// Mod pak to write into, whatever the files name.
    #[arg(long, value_name = "NAME")]
    mod_name: Option<String>,
    /// Overwrite an edited copy a mod pak already holds.
    #[arg(long)]
    replace: bool,
    /// Patch and verify every item, then write nothing.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args)]
struct CopyExportArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// Container holding the package to copy out of: a .pak or a .utoc, or a bare mod name.
    #[arg(long, value_name = "PATH")]
    from_container: String,

    /// Asset path inside that container.
    #[arg(long, value_name = "PATH")]
    from_entry: String,

    /// Export index in the source package, as its `asset info` prints it.
    #[arg(long, value_name = "N")]
    export: u32,

    /// Destination export the copy sits under. Omit for the package root.
    #[arg(long, value_name = "N", default_value_t = 0)]
    into_outer: u32,

    /// List the copy in this Level export's actors, so the game spawns it. A copied actor the
    /// level does not name loads with the package and never appears.
    #[arg(long, value_name = "N")]
    into_level: Option<u32>,

    /// The copy's object name. Omit to keep the source's.
    #[arg(long, value_name = "NAME")]
    name: Option<String>,

    /// Report what the copy would bring across and write nothing.
    #[arg(long)]
    dry_run: bool,

    /// Mod pak to write into, created in `~mods` if it does not exist.
    #[arg(long, value_name = "NAME")]
    mod_name: Option<String>,

    /// Overwrite an edited copy of this asset that the mod pak already holds.
    #[arg(long)]
    replace: bool,
}

fn asset_copy_export(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &CopyExportArgs,
) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let request = asset_request(cli, app, &args.asset, &root);
    let copy = asset::CopyArgs {
        from_container: &args.from_container,
        from_entry: &args.from_entry,
        export: args.export,
        into_outer: args.into_outer,
        name: args.name.as_deref().unwrap_or_default(),
        into_level: args.into_level,
    };
    let plan = asset::plan_copy(&request, &copy)?;
    if args.dry_run || !plan.blockers.is_empty() {
        let failed = !plan.blockers.is_empty();
        emit(cli, &plan, || {
            asset::print_copy_plan(&plan, &mut |line| outln!("{line}"))
        })?;
        return if failed {
            Err("the copy cannot be made".to_string())
        } else {
            Ok(())
        };
    }
    if !cli.force && rivals_core::game_status::should_block_for_game() {
        return Err(rivals_core::game_status::game_running_error());
    }
    let message = asset::copy_export(
        &request,
        &copy,
        mod_name_of(app, args.mod_name.as_deref()),
        args.replace,
    )?;
    emit(cli, &message, || outln!("{message}"))
}

#[derive(Args)]
#[command(allow_negative_numbers = true)]
struct DepsArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// Export index whose runs to show or replace.
    #[arg(long, value_name = "N")]
    export: u32,

    /// Objects that must be fully read before this one is. Package indices: an export's position
    /// plus one, or minus an import's position plus one.
    #[arg(long, value_name = "LIST", value_delimiter = ',', num_args = 0..)]
    sbs: Option<Vec<i32>>,

    /// Objects that must exist before this one is read, which is what a reference needs.
    #[arg(long, value_name = "LIST", value_delimiter = ',', num_args = 0..)]
    cbs: Option<Vec<i32>>,

    /// Objects that must be fully read before this one is built.
    #[arg(long, value_name = "LIST", value_delimiter = ',', num_args = 0..)]
    sbc: Option<Vec<i32>>,

    /// Objects that must exist before this one is built.
    #[arg(long, value_name = "LIST", value_delimiter = ',', num_args = 0..)]
    cbc: Option<Vec<i32>>,

    /// Report what the change would do and write nothing.
    #[arg(long)]
    dry_run: bool,

    /// Mod pak to write into, created in `~mods` if it does not exist.
    #[arg(long, value_name = "NAME")]
    mod_name: Option<String>,

    /// Overwrite an edited copy of this asset that the mod pak already holds.
    #[arg(long)]
    replace: bool,
}

#[derive(Args)]
struct AssetDiffArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// The edited dump, as `asset dump --json --declared` wrote it and you changed it.
    #[arg(long, value_name = "FILE")]
    edited: String,

    /// Where to write the edit file. Printed to stdout when omitted.
    #[arg(long, value_name = "FILE")]
    out: Option<String>,

    /// Mod pak the edit file names, for `asset apply` to write into.
    #[arg(long, value_name = "NAME")]
    mod_name: Option<String>,
}

#[derive(Clone, Copy, ValueEnum)]
enum TargetArg {
    Iostore,
    Pak,
}

impl From<TargetArg> for rivals_core::asset_edit::SaveTarget {
    fn from(value: TargetArg) -> Self {
        match value {
            TargetArg::Iostore => Self::IoStore,
            TargetArg::Pak => Self::Pak,
        }
    }
}

fn asset_deps(cli: &Cli, app: &settings::AppSettings, args: &DepsArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let request = asset_request(cli, app, &args.asset, &root);
    // With no run given the command reports rather than edits, so a run can be read before it is
    // replaced. An explicitly empty run is still an edit.
    let given = [&args.sbs, &args.cbs, &args.sbc, &args.cbc];
    if given.iter().all(|run| run.is_none()) {
        let report = asset::dependencies(&request, args.export)?;
        return emit(cli, &report, || {
            asset::print_dependencies(&report, &mut |line| outln!("{line}"))
        });
    }
    let held = asset::dependencies(&request, args.export)?.runs;
    let runs = rivals_uasset::Runs {
        serialize_before_serialize: args.sbs.clone().unwrap_or(held.serialize_before_serialize),
        create_before_serialize: args.cbs.clone().unwrap_or(held.create_before_serialize),
        serialize_before_create: args.sbc.clone().unwrap_or(held.serialize_before_create),
        create_before_create: args.cbc.clone().unwrap_or(held.create_before_create),
    };
    if args.dry_run {
        let plan = asset::plan_dependencies(&request, args.export, runs)?;
        let blocked = !plan.blockers.is_empty();
        emit(cli, &plan, || {
            asset::print_dependency_plan(&plan, &mut |line| outln!("{line}"))
        })?;
        return if blocked {
            Err("the change is blocked".to_string())
        } else {
            Ok(())
        };
    }
    if !cli.force && rivals_core::game_status::should_block_for_game() {
        return Err(rivals_core::game_status::game_running_error());
    }
    let message = asset::set_dependencies(
        &request,
        args.export,
        runs,
        mod_name_of(app, args.mod_name.as_deref()),
        args.replace,
    )?;
    emit(cli, &message, || outln!("{message}"))
}

fn asset_diff(cli: &Cli, app: &settings::AppSettings, args: &AssetDiffArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let request = asset_request(cli, app, &args.asset, &root);
    let target = request.target;
    let file = asset::diff(
        &request,
        std::path::Path::new(&args.edited),
        args.mod_name.as_deref().or(app.asset_mod_name.as_deref()),
        target,
    )?;
    let text = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
    match &args.out {
        Some(path) => {
            std::fs::write(path, format!("{text}\n")).map_err(|e| format!("write {path}: {e}"))?;
            emit(cli, &file, || {
                outln!("Wrote {path}");
                asset::print_diff(&file, &mut |line| outln!("{line}"));
            })
        }
        None => emit(cli, &file, || outln!("{text}")),
    }
}

fn asset_apply(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &AssetApplyArgs,
) -> Result<(), String> {
    // A dry run writes nothing, so the game holding the paks open cannot spoil it.
    if !args.dry_run && !cli.force && rivals_core::game_status::should_block_for_game() {
        return Err(rivals_core::game_status::game_running_error());
    }
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let items = match (&args.edits, &args.manifest) {
        (Some(path), _) => {
            let path = std::path::Path::new(path);
            let base = path
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .to_path_buf();
            vec![(base, rivals_core::asset_edit::json::read_edit_file(path)?)]
        }
        (_, Some(path)) => {
            rivals_core::asset_edit::json::read_manifest(std::path::Path::new(path))?
        }
        _ => return Err("pass --edits or --manifest".to_string()),
    };
    let report = asset::apply(
        &root,
        &items,
        &asset::ApplyOverrides {
            mod_name: args.mod_name.as_deref().or(app.asset_mod_name.as_deref()),
            replace: args.replace,
            dry_run: args.dry_run,
            target: cli.target.map(Into::into).or(app.asset_save_target),
        },
        cli.usmap.as_deref(),
        app.usmap_path.as_deref(),
        DEFAULT_MOD_NAME,
    );
    let failed = report.failed;
    emit(cli, &report, || {
        asset::print_apply(&report, &mut |line| outln!("{line}"))
    })?;
    if failed > 0 {
        return Err(format!("{failed} item(s) failed"));
    }
    Ok(())
}

fn asset_set(cli: &Cli, app: &settings::AppSettings, args: &AssetSetArgs) -> Result<(), String> {
    if !cli.force && rivals_core::game_status::should_block_for_game() {
        return Err(rivals_core::game_status::game_running_error());
    }
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let message = asset::set(
        &asset_request(cli, app, &args.asset, &root),
        vec![rivals_uasset::ValueEdit {
            offset: args.offset,
            expect_name: args.name.clone(),
            expect_element: args.element,
            expect_kind: args.kind.clone(),
            op: match (args.op.as_str(), args.index) {
                ("clear", _) => rivals_uasset::EditOp::Clear,
                ("store", _) => rivals_uasset::EditOp::Store,
                ("unset", _) => rivals_uasset::EditOp::Unset,
                ("set-element", Some(index)) => rivals_uasset::EditOp::SetElement {
                    index,
                    text: args.value.clone(),
                },
                ("insert", Some(index)) => rivals_uasset::EditOp::Insert {
                    index,
                    key: (!args.value.is_empty()).then(|| args.value.clone()),
                },
                ("remove", Some(index)) => rivals_uasset::EditOp::Remove { index },
                _ => rivals_uasset::EditOp::Set {
                    text: args.value.clone(),
                },
            },
        }],
        args.mod_name
            .as_deref()
            .or(app.asset_mod_name.as_deref())
            .unwrap_or(DEFAULT_MOD_NAME),
        args.replace,
    )?;
    emit(cli, &message, || outln!("{message}"))
}

fn asset_sweep(cli: &Cli, app: &settings::AppSettings, args: &SweepArgs) -> Result<(), String> {
    if !args.dry_run && !cli.force && rivals_core::game_status::should_block_for_game() {
        return Err(rivals_core::game_status::game_running_error());
    }
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let mut sets = Vec::with_capacity(args.sets.len());
    for spec in &args.sets {
        let (name, text) = spec
            .split_once('=')
            .ok_or_else(|| format!("--set takes Name=Value, not {spec}"))?;
        if name.trim().is_empty() {
            return Err(format!("--set {spec} names no property"));
        }
        sets.push(rivals_core::asset_edit::sweep::SweepSet {
            name: name.trim().to_string(),
            text: text.to_string(),
        });
    }
    let report = asset::sweep(
        cli.usmap.as_deref(),
        app.usmap_path.as_deref(),
        cli.target
            .map(Into::into)
            .or(app.asset_save_target)
            .unwrap_or_default(),
        &rivals_core::asset_edit::sweep::SweepRequest {
            game_root: &root,
            container: &args.container,
            filter: args.filter.as_deref(),
            class: args.class.as_deref(),
            mod_name: args
                .mod_name
                .as_deref()
                .or(app.asset_mod_name.as_deref())
                .unwrap_or(DEFAULT_MOD_NAME),
            sets,
            limit: args.limit,
            layer: args.layer,
            dry_run: args.dry_run,
        },
    )?;
    let failed = report.failed.len();
    emit(cli, &report, || {
        asset::print_sweep(&report, &mut |line| outln!("{line}"))
    })?;
    if failed > 0 {
        return Err(format!("{failed} package(s) failed"));
    }
    Ok(())
}

fn asset_import(cli: &Cli, app: &settings::AppSettings, args: &ImportArgs) -> Result<(), String> {
    if !cli.force && rivals_core::game_status::should_block_for_game() {
        return Err(rivals_core::game_status::game_running_error());
    }
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let request = asset_request(cli, app, &args.asset, &root);
    if !args.remove.is_empty() {
        let mut positions = Vec::with_capacity(args.remove.len());
        for &index in &args.remove {
            if index >= 0 {
                return Err(format!(
                    "{index} is not an import index; imports are negative, as `asset info` prints them"
                ));
            }
            positions.push((-index - 1) as u32);
        }
        let plan = asset::plan_import_removal(&request, &positions)?;
        if args.dry_run || !plan.blockers.is_empty() {
            let failed = !plan.blockers.is_empty();
            emit(cli, &plan, || {
                asset::print_import_plan(&plan, &mut |line| outln!("{line}"))
            })?;
            return if failed {
                Err("the imports named cannot be removed".to_string())
            } else {
                Ok(())
            };
        }
        let message = asset::import_remove(
            &request,
            &positions,
            mod_name_of(app, args.mod_name.as_deref()),
            args.replace,
        )?;
        return emit(cli, &message, || outln!("{message}"));
    }
    let path = args
        .path
        .clone()
        .ok_or("pass --path to retarget or add an import, or --remove to drop one")?;
    let class = args.class_package.clone().zip(args.class_name.clone());
    let edit = match args.index {
        Some(index) if index < 0 => rivals_uasset::ImportEdit::Retarget {
            import: (-index - 1) as u32,
            path: path.clone(),
            class,
        },
        Some(index) => {
            return Err(format!(
                "{index} is not an import index; imports are negative, as `asset info` prints them"
            ));
        }
        None => {
            let (class_package, class_name) =
                class.unwrap_or(("/Script/CoreUObject".into(), "Object".into()));
            rivals_uasset::ImportEdit::Add {
                path: path.clone(),
                class_package,
                class_name,
            }
        }
    };
    let message = asset::import_edit(
        &request,
        edit,
        args.mod_name
            .as_deref()
            .or(app.asset_mod_name.as_deref())
            .unwrap_or(DEFAULT_MOD_NAME),
        args.replace,
    )?;
    emit(cli, &message, || outln!("{message}"))
}

/// The mod pak an edit goes into: the one asked for, else the one the desktop app last used.
fn mod_name_of<'a>(app: &'a settings::AppSettings, explicit: Option<&'a str>) -> &'a str {
    explicit
        .or(app.asset_mod_name.as_deref())
        .unwrap_or(DEFAULT_MOD_NAME)
}

fn asset_remove_export(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &RemoveExportArgs,
) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let request = asset_request(cli, app, &args.asset, &root);
    let plan = asset::plan_removal(&request, &args.exports)?;
    if args.dry_run {
        return emit(cli, &plan, || {
            asset::print_plan(&plan, &mut |line| outln!("{line}"));
        });
    }
    if !plan.blockers.is_empty() {
        return Err(format!(
            "these exports cannot be removed: {}",
            plan.blockers.join("; ")
        ));
    }
    if !plan.warnings.is_empty() && !args.accept_warnings {
        return Err(format!(
            "the removal is not known to be safe:\n  {}\nPass --accept-warnings to go ahead, or --dry-run to see the whole plan.",
            plan.warnings.join("\n  ")
        ));
    }
    if !cli.force && rivals_core::game_status::should_block_for_game() {
        return Err(rivals_core::game_status::game_running_error());
    }
    let message = asset::remove_exports(
        &request,
        &args.exports,
        mod_name_of(app, args.mod_name.as_deref()),
        args.replace,
    )?;
    emit(cli, &message, || outln!("{message}"))
}

fn asset_reset_export(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &ResetExportArgs,
) -> Result<(), String> {
    if !cli.force && rivals_core::game_status::should_block_for_game() {
        return Err(rivals_core::game_status::game_running_error());
    }
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let message = asset::reset_export(
        &asset_request(cli, app, &args.asset, &root),
        args.export,
        mod_name_of(app, args.mod_name.as_deref()),
        args.replace,
    )?;
    emit(cli, &message, || outln!("{message}"))
}

fn asset_row(cli: &Cli, app: &settings::AppSettings, args: &RowArgs) -> Result<(), String> {
    if !cli.force && rivals_core::game_status::should_block_for_game() {
        return Err(rivals_core::game_status::game_running_error());
    }
    let new_name = || {
        args.name
            .clone()
            .ok_or_else(|| format!("--op {} needs --name", args.op))
    };
    let op = match args.op.as_str() {
        "add" => rivals_uasset::RowOp::Add {
            name: args.row.clone(),
            at: args.at,
        },
        "duplicate" => rivals_uasset::RowOp::Duplicate {
            source: args.row.clone(),
            name: new_name()?,
            at: args.at,
        },
        "remove" => rivals_uasset::RowOp::Remove {
            name: args.row.clone(),
        },
        "rename" => rivals_uasset::RowOp::Rename {
            name: args.row.clone(),
            to: new_name()?,
        },
        other => {
            return Err(format!(
                "{other} is not a row operation; use add, duplicate, remove or rename"
            ));
        }
    };
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let message = asset::row(
        &asset_request(cli, app, &args.asset, &root),
        rivals_uasset::RowEdit {
            export: args.export,
            op,
        },
        mod_name_of(app, args.mod_name.as_deref()),
        args.replace,
    )?;
    emit(cli, &message, || outln!("{message}"))
}

fn asset_duplicate_export(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &DuplicateExportArgs,
) -> Result<(), String> {
    if !cli.force && rivals_core::game_status::should_block_for_game() {
        return Err(rivals_core::game_status::game_running_error());
    }
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let message = asset::duplicate_export(
        &asset_request(cli, app, &args.asset, &root),
        args.export,
        &args.name,
        args.into_level,
        mod_name_of(app, args.mod_name.as_deref()),
        args.replace,
    )?;
    emit(cli, &message, || outln!("{message}"))
}

fn asset_payload(cli: &Cli, app: &settings::AppSettings, args: &PayloadArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let request = asset_request(cli, app, &args.asset, &root);
    match (&args.out, &args.input) {
        (Some(out), None) => {
            let bytes = asset::payload_bytes(&request, args.export)?;
            std::fs::write(out, &bytes).map_err(|e| format!("could not write {out}: {e}"))?;
            let message = format!("Wrote {} bytes to {out}", bytes.len());
            emit(cli, &message, || outln!("{message}"))
        }
        (None, Some(input)) => {
            if !cli.force && rivals_core::game_status::should_block_for_game() {
                return Err(rivals_core::game_status::game_running_error());
            }
            let bytes = std::fs::read(input).map_err(|e| format!("could not read {input}: {e}"))?;
            let message = asset::replace_payload(
                &request,
                args.export,
                bytes,
                mod_name_of(app, args.mod_name.as_deref()),
                args.replace,
            )?;
            emit(cli, &message, || outln!("{message}"))
        }
        _ => Err("pass --out PATH to save the payload or --in PATH to replace it".into()),
    }
}

fn asset_bulk(cli: &Cli, app: &settings::AppSettings, args: &BulkArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let request = asset_request(cli, app, &args.asset, &root);
    let resource = || {
        args.resource
            .ok_or_else(|| format!("--op {} needs --resource", args.op))
    };
    match args.op.as_str() {
        "list" => {
            let resources = asset::bulk_list(&request)?;
            emit(cli, &resources, || {
                for r in &resources {
                    let owner = r
                        .owner
                        .map_or(String::new(), |owner| format!(" in export {owner}"));
                    let locked = r
                        .locked
                        .as_deref()
                        .map_or(String::new(), |why| format!("  ({why})"));
                    outln!(
                        "#{:<4} {:<9} offset {:<10} size {:<10} flags 0x{:X}{owner}{locked}",
                        r.index,
                        r.placement,
                        r.serial_offset,
                        r.serial_size,
                        r.flags
                    );
                }
            })
        }
        "export" => {
            let out = args.out.as_deref().ok_or("--op export needs --out PATH")?;
            let bytes = asset::bulk_bytes(&request, resource()?)?;
            std::fs::write(out, &bytes).map_err(|e| format!("could not write {out}: {e}"))?;
            let message = format!("Wrote {} bytes to {out}", bytes.len());
            emit(cli, &message, || outln!("{message}"))
        }
        "replace" => {
            if !cli.force && rivals_core::game_status::should_block_for_game() {
                return Err(rivals_core::game_status::game_running_error());
            }
            let input = args
                .input
                .as_deref()
                .ok_or("--op replace needs --in PATH")?;
            let bytes = std::fs::read(input).map_err(|e| format!("could not read {input}: {e}"))?;
            let message = asset::replace_bulk(
                &request,
                resource()?,
                bytes,
                mod_name_of(app, args.mod_name.as_deref()),
                args.replace,
            )?;
            emit(cli, &message, || outln!("{message}"))
        }
        other => Err(format!(
            "{other} is not a bulk data operation; use list, export or replace"
        )),
    }
}

fn asset_keys(cli: &Cli, app: &settings::AppSettings, args: &KeysArgs) -> Result<(), String> {
    if !cli.force && rivals_core::game_status::should_block_for_game() {
        return Err(rivals_core::game_status::game_running_error());
    }
    let index = || {
        args.index
            .ok_or_else(|| format!("--op {} needs --index", args.op))
    };
    let time = || {
        args.time
            .ok_or_else(|| format!("--op {} needs --time", args.op))
    };
    let op = match args.op.as_str() {
        "add" => rivals_uasset::KeyOp::Add {
            time: time()?,
            value: args.value.ok_or("--op add needs --value")?,
        },
        "duplicate" => rivals_uasset::KeyOp::Duplicate {
            index: index()?,
            time: time()?,
        },
        "move" => rivals_uasset::KeyOp::Move {
            index: index()?,
            time: time()?,
        },
        "remove" => rivals_uasset::KeyOp::Remove { index: index()? },
        other => {
            return Err(format!(
                "{other} is not a key operation; use add, duplicate, move or remove"
            ));
        }
    };
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let message = asset::keys(
        &asset_request(cli, app, &args.asset, &root),
        rivals_uasset::KeyEdit {
            offset: args.offset,
            expect_name: args.name.clone(),
            expect_element: args.element,
            op,
        },
        mod_name_of(app, args.mod_name.as_deref()),
        args.replace,
    )?;
    emit(cli, &message, || outln!("{message}"))
}

fn asset_strings(cli: &Cli, app: &settings::AppSettings, args: &StringsArgs) -> Result<(), String> {
    if !cli.force && rivals_core::game_status::should_block_for_game() {
        return Err(rivals_core::game_status::game_running_error());
    }
    let index = || {
        args.index
            .ok_or_else(|| format!("--op {} needs --index", args.op))
    };
    let text = || {
        args.text
            .clone()
            .ok_or_else(|| format!("--op {} needs --text", args.op))
    };
    let id = || {
        args.id
            .clone()
            .ok_or_else(|| format!("--op {} needs --id", args.op))
    };
    let op = match args.op.as_str() {
        "set-key" => rivals_uasset::StringOp::SetKey {
            index: index()?,
            key: args.key.clone(),
            to: text()?,
        },
        "set-tag" => rivals_uasset::StringOp::SetTag {
            index: index()?,
            key: args.key.clone(),
            to: text()?,
        },
        "meta-set" => rivals_uasset::StringOp::SetMetaData {
            index: index()?,
            key: args.key.clone(),
            id: id()?,
            to: text()?,
        },
        "meta-remove" => rivals_uasset::StringOp::RemoveMetaData {
            index: index()?,
            key: args.key.clone(),
            id: id()?,
        },
        "set-source" => rivals_uasset::StringOp::SetSource {
            index: index()?,
            key: args.key.clone(),
            to: text()?,
        },
        "add" => rivals_uasset::StringOp::Add {
            key: args.key.clone(),
            source: text()?,
        },
        "remove" => rivals_uasset::StringOp::Remove {
            index: index()?,
            key: args.key.clone(),
        },
        other => {
            return Err(format!(
                "{other} is not a string table operation; use set-key, set-source, set-tag, add, \
                 remove, meta-set or meta-remove"
            ));
        }
    };
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let message = asset::strings(
        &asset_request(cli, app, &args.asset, &root),
        rivals_uasset::StringEdit {
            export: args.export,
            op,
        },
        mod_name_of(app, args.mod_name.as_deref()),
        args.replace,
    )?;
    emit(cli, &message, || outln!("{message}"))
}

fn asset_importers(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &ImportersArgs,
) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let found = asset::importers(&root, &args.path, args.build, &mut |done, total| {
        eprint!("\rindexing {done}/{total} packages");
        if done == total {
            eprintln!();
        }
    })?;
    emit(cli, &found, || {
        if found.packages.is_empty() {
            outln!("no indexed package imports {}", found.path);
        }
        for package in &found.packages {
            outln!("{package}");
        }
    })
}

fn asset_names(cli: &Cli, app: &settings::AppSettings, args: &AssetArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let lines = asset::names(&asset_request(cli, app, args, &root))?;
    emit(cli, &lines, || {
        for line in &lines {
            outln!("{line}");
        }
    })
}

fn asset_diagnose(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &DiagnoseArgs,
) -> Result<(), String> {
    let audit = &args.audit;
    if audit.container.is_none() && audit.dir.is_none() {
        return Err("pass either --container or --dir".into());
    }
    let show_progress = audit.progress;
    let tick = |stage: &str, current: usize, total: usize| {
        if show_progress && (current.is_multiple_of(50) || current == total) {
            eprint!("\r  {stage}: {current}/{total}          ");
        }
    };
    let root = resolve::game_root(cli.game_root.as_deref(), app).unwrap_or_default();
    let report = asset::diagnose(
        &asset::DiagnoseRequest {
            dir: audit.dir.as_deref(),
            game_root: &root,
            container: audit.container.as_deref().unwrap_or_default(),
            limit: audit.limit,
            filter: audit.filter.as_deref(),
            only: args.only.as_deref(),
            depth: args.depth,
            usmap: cli.usmap.as_deref(),
            configured_usmap: app.usmap_path.as_deref(),
        },
        tick,
    )?;
    if show_progress {
        eprintln!();
    }
    emit(cli, &report, || {
        asset::print_diagnose(&report, &mut |line| outln!("{line}"));
    })
}

fn asset_synth_check(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &AuditArgs,
) -> Result<(), String> {
    let container = args
        .container
        .as_deref()
        .ok_or("pass --container: struct definitions are read from packages")?;
    let show_progress = args.progress;
    let tick = |current: usize, total: usize| {
        if show_progress && (current.is_multiple_of(200) || current == total) {
            eprint!("\r  {current}/{total} packages");
        }
    };
    let report = asset::synth_check(
        &resolve::game_root(cli.game_root.as_deref(), app)?,
        container,
        args.limit,
        args.filter.as_deref(),
        cli.usmap.as_deref(),
        app.usmap_path.as_deref(),
        tick,
    )?;
    if show_progress {
        eprintln!();
    }
    emit(cli, &report, || {
        asset::print_synth_check(&report, &mut |line| outln!("{line}"));
    })
}

fn asset_audit(cli: &Cli, app: &settings::AppSettings, args: &AuditArgs) -> Result<(), String> {
    let show_progress = args.progress;
    let tick = |current: usize, total: usize| {
        if show_progress && (current.is_multiple_of(200) || current == total) {
            eprint!("\r  {current}/{total} packages");
        }
    };
    let report = match (&args.container, &args.dir) {
        (_, Some(dir)) => asset::audit_dir(
            dir,
            args.limit,
            args.filter.as_deref(),
            cli.usmap.as_deref(),
            app.usmap_path.as_deref(),
            args.skip_blueprint,
            tick,
        )?,
        (Some(container), None) => asset::audit(
            &resolve::game_root(cli.game_root.as_deref(), app)?,
            container,
            args.limit,
            args.filter.as_deref(),
            cli.usmap.as_deref(),
            app.usmap_path.as_deref(),
            args.skip_blueprint,
            tick,
        )?,
        (None, None) => return Err("pass either --container or --dir".into()),
    };
    if show_progress {
        eprintln!();
    }
    emit(cli, &report, || {
        asset::print_audit(&report, &mut |line| outln!("{line}"));
    })
}
