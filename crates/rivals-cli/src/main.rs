//! Command-line front end for scripting Marvel Rivals pak INI edits and config tweaks.

#![deny(clippy::unwrap_used, clippy::expect_used)]

/// The default Windows heap makes threads queue for the large allocations a walk over the game
/// makes on every thread at once; mimalloc gives each thread its own.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod asset;
mod resolve;
mod settings;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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
    #[arg(long, global = true, help_heading = "Global options")]
    json: bool,

    /// Game install root. Defaults to the path saved by the desktop app.
    #[arg(
        long,
        global = true,
        help_heading = "Global options",
        value_name = "DIR"
    )]
    game_root: Option<String>,

    /// Edit paks even while Marvel Rivals is running. The game holds these files open, so an edit
    /// can fail or be reverted on exit.
    #[arg(long, global = true, help_heading = "Global options")]
    force: bool,

    /// Path to a .usmap mappings file, or a folder holding one. Reading asset properties needs it.
    #[arg(
        long,
        global = true,
        help_heading = "Global options",
        value_name = "PATH"
    )]
    usmap: Option<String>,

    /// A text file of native object paths the game's script objects table leaves out, one per
    /// line, added to the bundled list so their imports read by name.
    #[arg(
        long,
        global = true,
        help_heading = "Global options",
        value_name = "PATH"
    )]
    script_objects: Option<String>,

    /// The language texts are shown in, as the game names it (`en`, `ja`, `zh-hans`, ...).
    /// Defaults to the app's setting, then English.
    #[arg(
        long,
        global = true,
        help_heading = "Global options",
        value_name = "CULTURE"
    )]
    culture: Option<String>,

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

    /// The mods in `~mods`: list, check, tidy, convert, install and manage them.
    #[command(subcommand, alias = "paks")]
    Mods(ModsCmd),

    /// Read and edit the packages inside mods and the game.
    #[command(subcommand)]
    Asset(AssetCmd),

    /// The game's engine logs and crash reports: list sessions, decode a log, and read how a
    /// session went.
    #[command(subcommand)]
    Logs(LogsCmd),
}

#[derive(Subcommand)]
enum AssetCmd {
    /// List package paths inside a container.
    List(ListArgs),
    /// Summarise a package: flags, counts, and every export with its parse status.
    Info(InfoArgs),
    /// Print the decoded property tree.
    Dump(DumpArgs),
    /// Print a DataTable as rows.
    Table(TableArgs),
    /// Print the byte range every property consumed, to locate a desync.
    Trace(TraceArgs),
    /// Print one export's raw bytes at the offsets the trace reports.
    Hex(HexArgs),
    /// Print the field records a class or struct export declares
    Fields(FieldsArgs),
    /// Disassemble the bytecode a function or class export stores.
    Script(ScriptArgs),
    /// Disassemble the VM bytecode a Niagara script runs on the CPU, naming what each constant
    /// reads and each output writes. Without `--export`, list the package's Niagara scripts.
    Vm(VmArgs),
    /// Give a constant a Niagara script's VM program reads a new value, and write the result into
    /// a mod pak: the literal itself where the operand is its only reader, or else a literal of
    /// the operand's own, so other readers of the old one keep it.
    EditVm(EditVmArgs),
    /// List a Blueprint's variables and functions with what references each, in the package and
    /// the rest of its mod, and which nothing does.
    Members(MembersArgs),
    /// Show a texture's pixel format, size and mips, save one of its mips as a PNG or an OpenEXR
    /// file, or give it new pixels from one.
    Texture(TextureArgs),
    /// Rename a Blueprint's variable, function, event or widget, or a function's parameter or
    /// local, along with every use of it in the package and the rest of the mod it is saved into.
    RenameMember(RenameMemberArgs),
    /// Take a Blueprint's variables or functions, or a function's locals, out once nothing runs or
    /// reads them, with the values its objects store for them, and write the result into a mod pak.
    RemoveMember(RemoveMemberArgs),
    /// Change something inside a function's bytecode, a literal, an object constant or a branch's
    /// condition, and write the result into a mod pak.
    EditScript(EditScriptArgs),
    /// Rewrite every literal in a function, or in every function of the package, in its widest
    /// form with the same meaning, and write the result into a mod pak. Nothing behaves
    /// differently, but every offset after each literal moves, which checks in game that a
    /// script can change size safely.
    WidenScript(WidenScriptArgs),
    /// Write a function's whole script anew from assembler text, as `asset script --as-text`
    /// prints it, and write the result into a mod pak. Whatever outside the function points into it
    /// follows the labels the text keeps.
    AssembleScript(AssembleScriptArgs),
    /// Add a variable to a Blueprint class whose objects all live in its own package: the class
    /// declares it after its own, and each object of the class there is renumbered to match.
    /// Refused when another package makes an object of the class or a class derived from it.
    AddVariable(AddVariableArgs),
    /// Add a field to a Blueprint struct, after its own, named as the editor names a member.
    /// Values other packages hold read as before, the new field unset, and a Blueprint type loads
    /// before the struct, and a script building it as a constant gets the new fields' defaults.
    /// Refused when another package lays the struct out by position: a value of it sent over the
    /// network, or a Niagara object using it as a type.
    AddField(AddFieldArgs),
    /// Change a stored value and write the result into a mod pak.
    Set(Box<AssetSetArgs>),
    /// Set the same properties by name across every package a filter matches, in one mod.
    Sweep(SweepArgs),
    /// Point an import at another object, or add one, and write the result into a mod pak.
    EditImports(EditImportsArgs),
    /// Remove exports from a package, subobjects included, and write the result into a mod pak.
    RemoveExport(RemoveExportArgs),
    /// Take one asset back out of a mod so the game's own copy loads again. A mod left holding
    /// nothing is deleted.
    Revert(RevertArgs),
    /// Drop every value an export stores so it inherits its class defaults, and write the result
    /// into a mod pak.
    ResetExport(ResetExportArgs),
    /// Add an object of a class to the package, storing nothing so it takes every value from its
    /// class, and write the result into a mod pak. Its values are set with `asset set` after.
    AddExport(AddExportArgs),
    /// Add a component to a Blueprint by duplicating one its construction script builds, and write
    /// the result into a mod pak.
    AddComponent(AddComponentArgs),
    /// Take a component out of a Blueprint: the construction script node that builds it and its
    /// template. The components under it take its place, or go with it.
    RemoveComponent(RemoveComponentArgs),
    /// Write the asset, unchanged but for its name, under another package name: a new asset, or a
    /// replacement for the one at that path.
    SaveAs(SaveAsArgs),
    /// Move a package a mod added to another path inside the same mod. Read it from the mod's own
    /// container.
    RenamePackage(RenamePackageArgs),
    /// Copy an export and its subobjects to the end of the export table under a new name, and
    /// write the result into a mod pak.
    DuplicateExport(DuplicateExportArgs),
    /// Add, copy, rename or remove a DataTable row and write the result into a mod pak.
    EditRow(EditRowArgs),
    /// Change, add or remove a StringTable entry and write the result into a mod pak.
    EditStrings(EditStringsArgs),
    /// Add, copy or remove a MovieScene channel key and write the result into a mod pak.
    EditKeys(EditKeysArgs),
    /// Save an export's payload bytes to a file, or replace them from one.
    Payload(PayloadArgs),
    /// List the bulk data resources, save one's bytes to a file with `--out`, or replace them from
    /// one with `--in`.
    Bulk(BulkArgs),
    /// List the packages that import an object or a package, from the import index.
    Importers(ImportersArgs),
    /// Find where the game's scripts name something: a call, a delegate, a string, a name, an
    /// object or a variable read or written. Reads every package the game loads, the enabled
    /// mods' included.
    Search(AssetSearchArgs),
    /// What an ability is made of, from its six-digit id: its row in its hero's ability table, the
    /// inputs its item binds, and every package carrying the id, the game's own and each mode's
    /// and event's version, with the Python classes, summons, gameplay tags and telling calls they
    /// hold. A hero's four-digit id lists its abilities instead.
    Ability(AbilityArgs),
    /// Rename an export, move it, or change the flags the loader reads it by, and write the result
    /// into a mod pak.
    EditExport(EditExportArgs),
    /// List the import table with what names each entry, and which are named by nothing.
    Imports(ImportsArgs),
    /// Print the package name table that FName indices resolve against, or drop the names nothing
    /// in the package uses.
    Names(NamesArgs),
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
    CheckStructs(CheckStructsArgs),
}

#[derive(Args)]
struct ListArgs {
    /// The container to list: `game` for every package the game ships, an installed mod by its
    /// whole name or a path, a game container such as `pakchunk0-Windows`, or a plain .pak.
    #[arg(value_name = "SOURCE")]
    source: String,

    /// Keep only paths containing this text, case insensitive: a piece of the container path (`Marvel/UI/Setting`) or of the package name (`/Game/Marvel/UI/Setting`), with either slash.
    #[arg(long, value_name = "TEXT")]
    filter: Option<String>,
}

/// The asset a command works on, and where it is read from.
#[derive(Args)]
struct AssetArgs {
    /// The asset: a package name such as `/Game/Marvel/UI/WBP_Thing`, its path inside a container
    /// such as `Marvel/Content/Marvel/UI/WBP_Thing.uasset`, or an extracted .uasset or .umap on
    /// disk, read with the files beside it.
    #[arg(value_name = "ASSET")]
    asset: String,

    /// Where to read it: `game` for the game's own copy, an installed mod by its whole name, a mod
    /// anywhere by a path to one of its files, a game container such as `pakchunk0-Windows`, or a
    /// plain .pak. An IoStore mod's .pak names its container, which holds its packages. A read
    /// defaults to the copy the game loads. A save defaults to the mod's own copy when it holds
    /// one, else to the game's, and asks for this when another switched-on mod overrides the asset.
    #[arg(long, value_name = "SOURCE")]
    from: Option<String>,
}

/// Where a save goes, and how it is checked: what every command saving into a mod takes.
#[derive(Args)]
struct SaveArgs {
    /// The mod to save into: an installed mod by a path to one of its files or its whole name, an
    /// IoStore mod anywhere else by a path to one of its files, written where it is, or a name no
    /// installed mod has, which the save makes in `~mods`, saying so. Defaults to the one the
    /// desktop app last saved into, then to `AssetEdits`.
    #[arg(long, help_heading = "Save options", value_name = "MOD")]
    into: Option<String>,

    /// Write over the copy the mod already holds, starting again from `--from`, or without it from
    /// the game's copy. Without this a save builds on the mod's own copy, and one reading the asset
    /// from somewhere else is refused.
    #[arg(long, help_heading = "Save options")]
    overwrite: bool,

    /// Patch and check everything, say which mod the save would go into, which copy it would read
    /// and what it would change, and write nothing.
    #[arg(long, help_heading = "Save options")]
    dry_run: bool,

    /// What the save leaves behind. The game loads packages only from an IoStore container; a
    /// plain pak is for tooling that converts it onward itself. Defaults to the desktop app's
    /// setting.
    #[arg(long, help_heading = "Save options", value_enum)]
    format: Option<FormatArg>,

    /// Save edits that point an import or an object reference at a path neither the game nor an
    /// enabled mod has, for an asset something loaded alongside provides.
    #[arg(long, help_heading = "Save options")]
    allow_missing: bool,

    /// Save edits whose target cannot be confirmed: a script's object constant or call pointed at
    /// something whose kind cannot be confirmed to match what the script held there, or an enum
    /// value the mappings do not name, such as one a mod's copy of the enum adds. Also saves one
    /// that leaves the load order disagreeing with what the package names, and one typing part of
    /// a struct written whole whose other fields inherit from native code, writing those as zero.
    #[arg(long, help_heading = "Save options")]
    allow_unchecked: bool,

    /// Keep the imports a save itself leaves named by nothing, which it otherwise drops in the same
    /// write with the load-order edges still holding them. Imports the cook left held by edges
    /// alone stay either way.
    #[arg(long, help_heading = "Save options")]
    keep_orphaned_imports: bool,

    /// Keep the locals of each function a script text rewrites that the rewrite left unused, which
    /// it otherwise drops in the same write. A local unused before the save stays either way, as do
    /// parameters.
    #[arg(long, help_heading = "Save options")]
    keep_unused_locals: bool,
}

impl SaveArgs {
    fn target(&self, app: &settings::AppSettings) -> rivals_core::asset_edit::SaveTarget {
        self.format
            .map(Into::into)
            .or(app.asset_save_target)
            .unwrap_or_default()
    }
}

/// A save of one asset: where it goes, and the name it goes under.
#[derive(Args)]
struct AssetWriteArgs {
    #[command(flatten)]
    save: SaveArgs,

    /// Save under this package name instead of the asset's own, such as
    /// `/Game/Mods/MyThing/DA_Sword`: a new asset, or a replacement for the asset at that path.
    #[arg(long = "as", help_heading = "Save options", value_name = "PACKAGE")]
    save_as: Option<String>,

    /// With `--as`, keep the names of the objects named after the package instead of renaming the
    /// asset, and a Blueprint's class and default object, along with it.
    #[arg(long, help_heading = "Save options", requires = "save_as")]
    keep_object_names: bool,
}

#[derive(Args)]
struct InfoArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// Also list the properties an export declares but does not store, which take the value the
    /// object inherits.
    #[arg(long)]
    declared: bool,
}

#[derive(Args)]
struct DumpArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// Print only this export: its index, as `asset info` prints it, or its name or path.
    #[arg(long, value_name = "EXPORT")]
    export: Option<String>,

    /// Also list the properties an export declares but does not store, which take the value the
    /// object inherits. Their offsets are where `asset set` would store one.
    #[arg(long)]
    declared: bool,

    /// Show the values an export does not store as its archetypes hold them, in place of "not
    /// stored". Reads each archetype's package; native classes keep theirs in code.
    #[arg(long)]
    inherited: bool,
}

#[derive(Args)]
struct TraceArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// Trace only this export: its index, as `asset info` prints it, or its name or path.
    #[arg(long, value_name = "EXPORT")]
    export: Option<String>,

    /// Also trace the properties an export declares but does not store.
    #[arg(long)]
    declared: bool,
}

#[derive(Args)]
struct HexArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// The export to dump: its index, or its name or path.
    #[arg(long, value_name = "EXPORT")]
    export: String,

    /// Start at this file offset instead of the export's start.
    #[arg(long, value_name = "OFFSET", value_parser = parse_offset)]
    offset: Option<u64>,
}

#[derive(Args)]
struct WidenScriptArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// The one function to widen: its export index, as `asset info` prints it, or its name.
    #[arg(
        long,
        value_name = "EXPORT",
        conflicts_with = "all",
        required_unless_present = "all"
    )]
    export: Option<String>,

    /// Widen every function in the package.
    #[arg(long)]
    all: bool,

    #[command(flatten)]
    write: AssetWriteArgs,
}

#[derive(Args)]
struct AssembleScriptArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// The function: its export index, as `asset info` prints it, or its name.
    #[arg(long, value_name = "EXPORT", required_unless_present = "new_function")]
    export: Option<String>,

    /// Add a function to the Blueprint class under this name, its script written from the text,
    /// rather than rewrite one the class has.
    #[arg(
        long,
        value_name = "NAME",
        conflicts_with = "export",
        requires = "signature"
    )]
    new_function: Option<String>,

    /// The new function's parameters, as a signature prints: `(A: Int, ref B: Array<Int>) -> Hit:
    /// Bool`, or `-> (Hit: Bool, Count: Int)` for several outputs. Types as a `local` line takes
    /// them, classes, structs and enums by full path.
    #[arg(long, value_name = "SIGNATURE", requires = "new_function")]
    signature: Option<String>,

    /// The class export the new function goes to, when the package holds more than one.
    #[arg(long, value_name = "EXPORT", requires = "new_function")]
    class: Option<String>,

    /// The text to assemble, as `asset script --as-text` prints it: UTF-8, or UTF-16 behind a byte
    /// order mark.
    #[arg(long, value_name = "FILE")]
    text_file: std::path::PathBuf,

    #[command(flatten)]
    write: AssetWriteArgs,
}

#[derive(Args)]
struct AddFieldArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// The name the field shows.
    #[arg(long, value_name = "NAME")]
    name: String,

    /// Its type, as a `local` line takes it: `Int`, `Array<Name>`, `Map<Name, Text>`,
    /// `Struct</Game/Data/S.S>`. A container of containers is refused, as is a set element or map
    /// key that cannot be hashed; a struct key needs `--allow-unchecked`.
    #[arg(long = "type", value_name = "TYPE")]
    ty: String,

    /// The struct export, when the package holds more than one Blueprint struct.
    #[arg(long = "struct", value_name = "EXPORT")]
    strukt: Option<String>,

    #[command(flatten)]
    write: AssetWriteArgs,
}

#[derive(Args)]
struct AddVariableArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// The variable's name.
    #[arg(long, value_name = "NAME")]
    name: String,

    /// Its type, as a `local` line takes it: `Int`, `Object</Script/Engine.Actor>`, `Array<Name>`.
    #[arg(long = "type", value_name = "TYPE")]
    ty: String,

    /// The class export, when the package holds more than one Blueprint class.
    #[arg(long, value_name = "EXPORT")]
    class: Option<String>,

    #[command(flatten)]
    write: AssetWriteArgs,
}

#[derive(Args)]
struct ScriptArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// The function or class: its export index, as `asset info` prints it, or its name or path.
    #[arg(long, value_name = "EXPORT")]
    export: String,

    /// Under each statement, list everything in it an edit can address, by the offset it starts
    /// at: literals, object constants, texts, calls and conditions.
    #[arg(long, conflicts_with = "as_text")]
    expressions: bool,

    /// Print only the assembler text, which `asset assemble-script` takes back: no offsets, no
    /// header, labels where code is jumped to.
    #[arg(long)]
    as_text: bool,
}

#[derive(Args)]
struct VmArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// The Niagara script: its export index, as `asset info` prints it, or its name or path.
    /// Without it, the package's Niagara scripts are listed.
    #[arg(long, value_name = "EXPORT")]
    export: Option<String>,

    /// Also list the script's literals, with what reads each, and its parameter store.
    #[arg(long)]
    constants: bool,

    /// Trace each output whose name starts with this, such as `Particles.Color`, back to the
    /// literals, parameters, inputs and calls it is computed from. Given bare, trace them all.
    #[arg(long, value_name = "NAME", num_args = 0..=1, default_missing_value = "")]
    trace: Option<String>,
}

#[derive(Args)]
struct EditVmArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// The Niagara script: its export index, as `asset info` prints it, or its name or path.
    #[arg(long, value_name = "EXPORT")]
    export: String,

    /// The instruction, by the offset `asset vm` prints at the start of its line.
    #[arg(long, value_name = "OFFSET", value_parser = parse_offset)]
    at: u64,

    /// Which of its operands, counted from 0 in the order they are stored, as `asset vm --trace`
    /// lists them after `#`.
    #[arg(long, value_name = "K")]
    operand: u32,

    /// The new value: a number for a float operand, an integer for an int one, or typed where the
    /// operand takes either: `float:1`, `int:-1`, `0x3F800000`, `true`.
    #[arg(long, value_name = "VALUE", allow_hyphen_values = true)]
    value: String,

    /// Give the operand a literal of its own even where it is the only reader of the one it reads.
    #[arg(long)]
    retarget: bool,

    #[command(flatten)]
    write: AssetWriteArgs,
}

#[derive(Args)]
struct TableArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// Only this row, by name, printed in full with every nested struct, array and map. The JSON
    /// output holds the nested values for every row either way.
    #[arg(long, value_name = "NAME")]
    row: Option<String>,
}

#[derive(Args)]
struct FieldsArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// The class or struct: its export index, as `asset info` prints it, or its name or path.
    #[arg(long, value_name = "EXPORT")]
    export: String,
}

fn parse_offset(text: &str) -> Result<u64, String> {
    let trimmed = text.trim_start_matches("0x").trim_start_matches("0X");
    let radix = if trimmed.len() == text.len() { 10 } else { 16 };
    u64::from_str_radix(trimmed, radix).map_err(|e| e.to_string())
}

/// One dependency run as typed: package indices in a single comma-separated value, which may
/// start with a minus sign, or nothing for an empty run.
#[derive(Clone, Debug)]
struct RunList(Vec<i32>);

fn parse_run(text: &str) -> Result<RunList, String> {
    text.split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| {
            part.parse::<i32>()
                .map_err(|_| format!("{part} is not a package index"))
        })
        .collect::<Result<_, _>>()
        .map(RunList)
}

#[derive(Args)]
struct SweepArgs {
    /// What to sweep: `game` for every package the game ships, an installed mod by its whole name or
    /// a path, or a game container such as `pakchunk0-Windows`. Every package it holds is a
    /// candidate.
    #[arg(value_name = "SOURCE")]
    source: String,

    /// Keep only packages whose path contains this text, case insensitive: a piece of the container path (`Marvel/UI/Setting`) or of the package name (`/Game/Marvel/UI/Setting`), with either slash.
    #[arg(long, value_name = "TEXT")]
    filter: Option<String>,

    /// Keep only packages holding an export of this class, such as `LegacyCameraShakePattern`.
    /// Narrows `--filter` rather than replacing it, and reads every package the filter allows.
    #[arg(long, value_name = "NAME")]
    class: Option<String>,

    /// `Name=Value`: every property with that name, at any depth, takes that value. Over an array,
    /// `Name=[a, b, c]` makes it hold exactly those elements (quote one that holds a comma) and
    /// `Name[2]=Value` sets one element. Repeatable.
    #[arg(long = "set", value_name = "NAME=VALUE", required = true)]
    sets: Vec<String>,

    /// Stop after this many matching packages, to try a sweep out on a handful first.
    #[arg(long, value_name = "N")]
    limit: Option<usize>,

    /// The mod to save into, named as a save's `--into` is. Its packages are written; whatever else
    /// it holds is carried over. Defaults to the one the desktop app last saved into, then to
    /// `AssetEdits`.
    #[arg(long, value_name = "MOD")]
    into: Option<String>,

    /// Start each package the mod already holds again from SOURCE. Without this a package the mod
    /// holds is read from the mod, so sweeps build on each other.
    #[arg(long)]
    overwrite: bool,

    /// Read and patch everything, and write nothing.
    #[arg(long)]
    dry_run: bool,

    /// What the save leaves behind: an IoStore container, or a plain pak for tooling that
    /// converts it onward itself. Defaults to the desktop app's setting.
    #[arg(long, value_enum)]
    format: Option<FormatArg>,
}

#[derive(Args)]
struct AssetSetArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// The object the value is in, for an edit named by `--path`: its path below the package
    /// (`Table`, `BP_C:Mesh_GEN_VARIABLE`), its full path, its name when no other object has it,
    /// or its index.
    #[arg(long, value_name = "EXPORT", requires = "path")]
    export: Option<String>,

    /// The DataTable row `--path` starts in.
    #[arg(
        long,
        value_name = "NAME",
        requires = "path",
        conflicts_with = "defaults"
    )]
    row: Option<String>,

    /// `--path` starts in the default values a Blueprint struct keeps.
    #[arg(long, requires = "path")]
    defaults: bool,

    /// The value by the fields leading to it, rather than by offset: `A.B.C`, with `Name[2]` for a
    /// static array's slot or a container's element and `Name{Key}` for a map's pair or a set's
    /// element by its key. It is found in the package as the save reads it, so the same command
    /// fits a copy a patch has moved, and a value it already holds is left as it is.
    #[arg(
        long,
        value_name = "PATH",
        requires = "export",
        conflicts_with_all = ["offset", "kind", "name", "element", "field"]
    )]
    path: Option<String>,

    /// File offset of the value, as reported by `asset dump` or `asset trace`.
    #[arg(long, value_name = "OFFSET", value_parser = parse_offset, required_unless_present = "path")]
    offset: Option<u64>,

    /// The kind the value at `--offset` is expected to be, so a stale offset is refused rather
    /// than written: `int`, `float`, `str`, `struct` and so on, or for a value not stored, the type
    /// it declares.
    #[arg(
        long = "expect-kind",
        value_name = "KIND",
        required_unless_present = "path"
    )]
    kind: Option<String>,

    /// The name of the property at `--offset`. A value holding its default occupies no bytes, so
    /// it shares its offset with the one stored next and the name is what picks between them.
    #[arg(
        long = "expect-name",
        value_name = "NAME",
        required_unless_present = "path"
    )]
    name: Option<String>,

    /// Which slot of a static array, when the property declares more than one.
    #[arg(long, value_name = "N")]
    element: Option<u32>,

    /// A value inside the struct or container `--name` names, as `A.B.C` from the struct down, or
    /// `[2].X` for field `X` of element 2. Any struct on the way that stores nothing yet is stored
    /// to hold it, and an element counts the container once the save's own inserts and removals
    /// have landed, all in one save.
    #[arg(long, value_name = "PATH", conflicts_with_all = ["op", "index"])]
    field: Option<String>,

    /// What to do to the value. With `--path` the element is the one the path names, `--index`
    /// adds it to the path, and an insert with no `--index` goes last.
    #[arg(long, value_enum, default_value = "set")]
    op: SetOpArg,

    /// Which element of a container to act on.
    #[arg(long, value_name = "N")]
    index: Option<u32>,

    /// The key `set-key` gives a map's pair, or the one `insert` adds a set's or a map's new
    /// element under.
    #[arg(long, value_name = "KEY", allow_hyphen_values = true)]
    key: Option<String>,

    /// For `reorder`, each element's index as read, in its new order: `2,0,1` moves the last
    /// element to the front.
    #[arg(long, value_name = "INDICES", value_delimiter = ',')]
    order: Option<Vec<u32>>,

    /// For `set-raw`, the value's new bytes as hex digits, two to a byte, at any length. Spaces
    /// are ignored.
    #[arg(long, value_name = "HEX")]
    hex: Option<String>,

    /// The new value.
    #[arg(
        long,
        value_name = "TEXT",
        default_value = "",
        allow_hyphen_values = true
    )]
    value: String,

    #[command(flatten)]
    write: AssetWriteArgs,
}

#[derive(Args)]
#[command(allow_negative_numbers = true)]
struct EditImportsArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// The import to retarget, by the negative index `asset info` prints. Omit to add one.
    #[arg(long, value_name = "INDEX")]
    import: Option<i32>,

    /// The object, in UE's dotted form: `/Game/Path/Asset.Object`, or `:Sub` for a subobject.
    #[arg(long, value_name = "PATH", required_unless_present = "remove")]
    path: Option<String>,

    /// Drop this import instead, by the negative index `asset info` prints. Repeatable. Every
    /// index above it moves down, so this is a save of its own.
    #[arg(long, value_name = "INDEX", conflicts_with_all = ["import", "path"])]
    remove: Vec<i32>,

    /// The object's class, as its path: `/Script/Engine.StaticMesh`. A retarget keeps the old
    /// class when this is omitted; an add falls back to `/Script/CoreUObject.Object`, which the
    /// loader accepts for any object.
    #[arg(long, value_name = "PATH")]
    class: Option<String>,

    #[command(flatten)]
    write: AssetWriteArgs,
}

#[derive(Args)]
struct RevertArgs {
    /// The asset: a package name such as `/Game/Marvel/UI/WBP_Thing`, or its path inside a
    /// container.
    #[arg(value_name = "ASSET")]
    asset: String,

    /// The mod to take it out of, by a path to one of its files or its whole name. Defaults to the
    /// one the desktop app last saved into, then to `AssetEdits`.
    #[arg(long, value_name = "MOD")]
    from: Option<String>,

    /// Say what would be taken out, and write nothing.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args)]
struct RemoveExportArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// An export to remove: its index, as `asset info` prints it, or its name or path. Repeat to
    /// remove several.
    #[arg(long = "export", value_name = "EXPORT", required = true)]
    exports: Vec<String>,

    /// Go ahead although the plan carries warnings.
    #[arg(long)]
    accept_warnings: bool,

    #[command(flatten)]
    write: AssetWriteArgs,
}

#[derive(Args)]
struct AssetSearchArgs {
    /// Text to find, anywhere in a term, case aside.
    query: String,

    /// Search stored values too: strings, names, texts, object and asset paths, enumerators and
    /// delegates, at any depth, DataTable rows and StringTable entries included, and fields by
    /// their name with what each holds. A table's value names its row and the row's id. Reads
    /// every package rather than only the ones holding functions, which takes about a minute for
    /// the whole game; `--filter` narrows it.
    #[arg(long)]
    values: bool,

    /// Keep only packages whose path contains this text, case insensitive: a piece of the
    /// container path (`Marvel/UI/Setting`) or of the package name (`/Game/Marvel/UI/Setting`).
    /// Repeatable.
    #[arg(long, value_name = "TEXT")]
    filter: Vec<String>,

    /// Only terms of this kind. Repeatable. `write` is where a variable is assigned, or changed
    /// by an engine function known to change what it is handed: an array, map or set, a timer
    /// handle, a random stream or a gameplay tag container. `read` is everywhere else it is
    /// named. `bind` is where a delegate is bound or unbound, `broadcast` where one is fired, and
    /// `delegate` both. `value` and `field` are stored values found by what they hold and by
    /// their field's name, and imply `--values`.
    #[arg(long, value_name = "KIND")]
    kind: Vec<SearchKindArg>,

    /// Match the text only as a whole word, not inside a longer name: `Delay` then finds
    /// `KismetSystemLibrary:Delay` but not `DelayUntilNextTick`.
    #[arg(long)]
    word: bool,

    /// Leave the enabled mods out and read the base game alone.
    #[arg(long)]
    no_mods: bool,

    /// Leave out a package's paths to its own objects where its own name is what matched, so an
    /// id finds the packages that refer to it rather than the one it names.
    #[arg(long)]
    no_self: bool,

    /// List the places under each hero, or each ability, their package belongs to. A mode's or
    /// an event's version of an ability is flagged and follows the game's own.
    #[arg(long, value_name = "BY")]
    by: Option<SearchGroupArg>,

    /// List at most this many places, the first by path; every place is still counted. 0 lists
    /// them all.
    #[arg(long, value_name = "N", default_value_t = 5000)]
    limit: usize,
}

/// What `asset set` does to the value it names.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum SetOpArg {
    /// Write `--value`.
    Set,
    /// Flag the property as zero.
    Clear,
    /// Give an unset struct, container or reference its empty form.
    Store,
    /// Drop it, so the inherited value applies.
    Unset,
    /// Set the container element at `--index` to `--value`.
    SetElement,
    /// Add an element at `--index`; a set's or a map's takes its key from `--key`, or `--value`.
    /// An array's new element is a copy of the one at `--index`, or of the last when it goes last,
    /// to be given its own value by another edit.
    Insert,
    /// Take out the container element at `--index`.
    Remove,
    /// Give the map pair at `--index` the key `--key`.
    SetKey,
    /// Put the elements in the order `--order` gives.
    Reorder,
    /// Replace the value's bytes with `--hex`.
    SetRaw,
}

/// What `asset edit-row` does.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum RowOpArg {
    /// Add a row named `--row`.
    Add,
    /// Copy the row `--row` to a new one named `--name`.
    Duplicate,
    /// Take out the row `--row`.
    Remove,
    /// Rename the row `--row` to `--name`.
    Rename,
}

/// What `asset edit-strings` does.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum StringOpArg {
    /// Give the entry a new key, `--text`.
    SetKey,
    /// Give the entry a new source string, `--text`.
    SetSource,
    /// Give the entry a new tag, `--text`.
    SetTag,
    /// Add an entry under `--key` whose source string is `--text`.
    Add,
    /// Take the entry out.
    Remove,
    /// Set the entry's metadata item `--id` to `--text`.
    MetaSet,
    /// Take out the entry's metadata item `--id`.
    MetaRemove,
}

/// What `asset edit-keys` does.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum KeyOpArg {
    /// Put a key at `--time` holding `--value`.
    Add,
    /// Copy key `--index` to `--time`.
    Duplicate,
    /// Carry key `--index` to `--time`.
    Move,
    /// Drop key `--index`.
    Remove,
}

/// What a rename does with a use by name on an object the package does not say is of the class.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum PossibleArg {
    /// List them and write nothing.
    Refuse,
    /// Rename them too.
    Follow,
    /// Keep them as they are.
    Leave,
}

/// The word a fixed choice is typed as, for messages naming it.
fn choice<T: ValueEnum>(value: &T) -> String {
    value
        .to_possible_value()
        .map(|value| value.get_name().to_string())
        .unwrap_or_default()
}

/// The kinds of term a search can be narrowed to.
#[derive(Clone, Copy, ValueEnum)]
enum SearchKindArg {
    Call,
    Bind,
    Broadcast,
    Delegate,
    String,
    Name,
    Object,
    Read,
    Write,
    Value,
    Field,
}

impl From<SearchKindArg> for rivals_core::mod_search::HitKind {
    fn from(kind: SearchKindArg) -> Self {
        match kind {
            SearchKindArg::Call => Self::Call,
            SearchKindArg::Bind => Self::Bind,
            SearchKindArg::Broadcast => Self::Broadcast,
            SearchKindArg::Delegate => Self::Delegate,
            SearchKindArg::String => Self::String,
            SearchKindArg::Name => Self::Name,
            SearchKindArg::Object => Self::Object,
            SearchKindArg::Read => Self::Read,
            SearchKindArg::Write => Self::Write,
            SearchKindArg::Value => Self::Value,
            SearchKindArg::Field => Self::Field,
        }
    }
}

/// What a search's places are listed under, beside their package.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum SearchGroupArg {
    Hero,
    Ability,
}

#[derive(Args)]
struct AbilityArgs {
    /// An ability's id, such as 101461, or a hero's, such as 1014.
    id: u32,

    /// List every call the ability's scripts make, not only launching, spawning, summoning,
    /// timers and delegates.
    #[arg(long)]
    calls: bool,
}

#[derive(Args)]
struct ImportersArgs {
    /// The object in UE's dotted form, `/Game/Path/Asset.Object:Sub`, or a bare package path to
    /// list everything that imports any of its objects.
    #[arg(value_name = "PATH")]
    path: String,

    /// Build the index first, or rebuild it after the game updated. Reads every package once.
    #[arg(long)]
    build: bool,
}

#[derive(Args)]
struct ResetExportArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// The export to reset: its index, as `asset info` prints it, or its name or path.
    #[arg(long, value_name = "EXPORT")]
    export: String,

    #[command(flatten)]
    write: AssetWriteArgs,
}

#[derive(Args)]
struct AddExportArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// The class, as an object path: `/Script/Module.Class`, or `/Game/Path/BP_Thing.BP_Thing_C`
    /// for a Blueprint.
    #[arg(long, value_name = "PATH")]
    class: String,

    /// The export to put it under: its index, as `asset info` prints it, or its name or path. Omit
    /// for an object at the top of the package.
    #[arg(long, value_name = "EXPORT")]
    outer: Option<String>,

    /// Its name, unique among what shares its outer.
    #[arg(long, value_name = "NAME")]
    name: String,

    #[command(flatten)]
    write: AssetWriteArgs,
}

#[derive(Args)]
struct RemoveComponentArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// The construction script node that builds the component, an `SCS_Node`: its export index, as
    /// `asset info` prints it, or its name or path.
    #[arg(long, value_name = "EXPORT")]
    node: String,

    /// Take the components under it too, rather than hang them where it was.
    #[arg(long)]
    with_children: bool,

    #[command(flatten)]
    write: AssetWriteArgs,
}

#[derive(Args)]
struct AddComponentArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// The construction script node that builds the component to copy, an `SCS_Node`: its export
    /// index, as `asset info` prints it, or its name or path.
    #[arg(long, value_name = "EXPORT", required_unless_present = "from_parent")]
    node: Option<String>,

    /// Copy the component the parent Blueprint adds under this variable name instead, as a
    /// component of this Blueprint's own attached where the original is.
    #[arg(long, value_name = "VARIABLE", conflicts_with_all = ["node", "with_children"])]
    from_parent: Option<String>,

    /// The new component's variable name.
    #[arg(long, value_name = "NAME")]
    name: String,

    /// Copy the components under it too, each under the next free name after its own.
    #[arg(long)]
    with_children: bool,

    #[command(flatten)]
    write: AssetWriteArgs,
}

#[derive(Args)]
struct SaveAsArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// The package name to save it as, such as `/Game/Mods/MyThing/DA_Sword`.
    #[arg(long, value_name = "PACKAGE")]
    to: String,

    /// Keep the names of the objects named after the package instead of renaming the asset, and a
    /// Blueprint's class and default object, along with it.
    #[arg(long)]
    keep_object_names: bool,

    #[command(flatten)]
    save: SaveArgs,
}

#[derive(Args)]
struct RenamePackageArgs {
    /// The package to move, by its name or its path inside the mod.
    #[arg(value_name = "ASSET")]
    asset: String,

    /// The package name to move it to, such as `/Game/Mods/MyThing/DA_Sword`.
    #[arg(long, value_name = "PACKAGE")]
    to: String,

    /// The mod the package is in, and stays in, by a path to one of its files or its whole name.
    /// Defaults to the one the desktop app last saved into, then to `AssetEdits`.
    #[arg(long, value_name = "MOD")]
    into: Option<String>,

    /// Leave the mod's other packages naming the old path instead of pointing them at the new one.
    #[arg(long)]
    keep_referencers: bool,

    /// Save the move although what it changes cannot all be confirmed, as a save's
    /// `--allow-unchecked` does.
    #[arg(long)]
    allow_unchecked: bool,

    /// Say what the move would do, and write nothing.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args)]
struct NamesArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// Only the names nothing in the package uses.
    #[arg(long)]
    unused: bool,

    /// Drop the names nothing in the package uses and write the result into a mod pak. Refused for
    /// a package with bytes the reader cannot follow, which may hold names nothing tracks.
    #[arg(long)]
    compact: bool,

    #[command(flatten)]
    write: AssetWriteArgs,
}

#[derive(Args)]
struct EditRowArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// The DataTable: its export index, as `asset info` prints it, or its name or path.
    #[arg(long, value_name = "EXPORT")]
    export: String,

    /// What to do to the row.
    #[arg(long, value_enum)]
    op: RowOpArg,

    /// The row the operation is about: the name to add, or the row to copy, remove or rename.
    #[arg(long, value_name = "NAME", allow_hyphen_values = true)]
    row: String,

    /// The new name a copy or a rename takes.
    #[arg(long, value_name = "NAME", allow_hyphen_values = true)]
    name: Option<String>,

    /// Where an added or copied row goes: in front of the row at this position, counting from 0.
    /// Last when omitted.
    #[arg(long, value_name = "N")]
    position: Option<u32>,

    #[command(flatten)]
    write: AssetWriteArgs,
}

#[derive(Args)]
struct EditStringsArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// The StringTable: its export index, as `asset info` prints it, or its name or path.
    #[arg(long, value_name = "EXPORT")]
    export: String,

    /// What to do to the entry.
    #[arg(long, value_enum)]
    op: StringOpArg,

    /// Position of the entry, counting from 0, for everything but `add`.
    #[arg(long, value_name = "N")]
    index: Option<u32>,

    /// The entry's key as it reads now, or the key an added entry takes.
    #[arg(long, value_name = "KEY", allow_hyphen_values = true)]
    key: String,

    /// The new key, source string or tag, an added entry's source string, or a metadata value.
    #[arg(long, value_name = "TEXT", allow_hyphen_values = true)]
    text: Option<String>,

    /// The metadata item's id, for `meta-set` and `meta-remove`.
    #[arg(long, value_name = "ID")]
    id: Option<String>,

    #[command(flatten)]
    write: AssetWriteArgs,
}

#[derive(Args)]
struct EditKeysArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// File offset of the channel, as reported by `asset dump` or `asset trace`.
    #[arg(long, value_name = "OFFSET", value_parser = parse_offset)]
    offset: u64,

    /// The name of the channel property at `--offset`, so a stale offset is refused rather than
    /// written.
    #[arg(long = "expect-name", value_name = "NAME")]
    name: String,

    /// Which slot of a static array, when the property declares more than one.
    #[arg(long, value_name = "N")]
    element: Option<u32>,

    /// What to do to the channel's keys.
    #[arg(long, value_enum)]
    op: KeyOpArg,

    /// Which key, counting from 0 in frame order.
    #[arg(long, value_name = "N")]
    index: Option<u32>,

    /// The frame of the new key.
    #[arg(long, value_name = "FRAME", allow_hyphen_values = true)]
    time: Option<i32>,

    /// The value of the added key.
    #[arg(long, value_name = "NUMBER", allow_hyphen_values = true)]
    value: Option<f64>,

    #[command(flatten)]
    write: AssetWriteArgs,
}

#[derive(Args)]
struct DuplicateExportArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// The export to copy: its index, as `asset info` prints it, or its name or path.
    #[arg(long, value_name = "EXPORT")]
    export: String,

    /// The copy's object name, unique beside the original.
    #[arg(long, value_name = "NAME")]
    name: String,

    /// List the copy in this Level export's actors, so the game spawns it. Without this the copy
    /// loads with the package and never appears.
    #[arg(long, value_name = "EXPORT")]
    level: Option<String>,

    #[command(flatten)]
    write: AssetWriteArgs,
}

#[derive(Args)]
#[command(allow_negative_numbers = true)]
struct EditScriptArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// The function: its export index, as `asset info` prints it, or its name.
    #[arg(long, value_name = "EXPORT")]
    export: String,

    /// The statement holding what is changed, by the offset `asset script` prints at the start
    /// of its line.
    #[arg(long, value_name = "OFFSET", value_parser = parse_offset)]
    statement: u64,

    /// Which literal in that statement, counted from 0 left to right. A wrong index lists them.
    #[arg(long = "const", value_name = "K", default_value_t = 0)]
    constant: u32,

    /// The expression to change, by the offset it starts at, as `asset script --expressions`
    /// lists them.
    #[arg(long, value_name = "OFFSET", value_parser = parse_offset, conflicts_with_all = ["object", "text", "call", "condition"])]
    at: Option<u64>,

    /// The K-th object constant in the statement, counted from 0 left to right.
    #[arg(long = "object-const", value_name = "K", conflicts_with_all = ["text", "call", "condition"])]
    object: Option<u32>,

    /// The K-th text in the statement, counted from 0 left to right. It takes UE's text literal
    /// syntax, `INVTEXT("...")`, `NSLOCTEXT("ns", "key", "...")` or `LOCTABLE("/Game/...", "key")`,
    /// or a plain string: a new source for a text that has one, `Table:Key` to repoint a string
    /// table text, and otherwise the words to show in every language.
    #[arg(long = "text-const", value_name = "K", conflicts_with_all = ["call", "condition"])]
    text: Option<u32>,

    /// The K-th call in the statement, counted from 0 left to right, pointed at another function:
    /// a final call takes the function's path as the disassembly prints it, a virtual call takes
    /// a bare name. The new function has to take the same arguments and give back what the call
    /// keeps, as its package or the game's own calls to it show.
    #[arg(long, value_name = "K", conflicts_with = "condition")]
    call: Option<u32>,

    /// Fix the condition of the statement, a `JumpIfNot` or `PopExecutionFlowIfNot`, as always
    /// `true` or always `false` with `--value`.
    #[arg(long)]
    condition: bool,

    /// The new value. A literal takes its own kind: an integer, a number, a string, a name, or
    /// `x,y,z` for a vector, at the width the old value took. `True`/`False` and a condition take
    /// `true` or `false`; an object constant takes the object's path as the disassembly prints
    /// it, or `None`.
    #[arg(long, value_name = "TEXT", allow_hyphen_values = true)]
    value: String,

    #[command(flatten)]
    write: AssetWriteArgs,
}

#[derive(Args)]
struct PayloadArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// The export: its index, as `asset info` prints it, or its name or path.
    #[arg(long, value_name = "EXPORT")]
    export: String,

    /// Write the payload's bytes to this file.
    #[arg(long, value_name = "FILE", conflicts_with = "input")]
    out: Option<String>,

    /// Replace the payload with this file's bytes and save the result into a mod.
    #[arg(long = "in", value_name = "FILE")]
    input: Option<String>,

    #[command(flatten)]
    write: AssetWriteArgs,
}

#[derive(Args)]
struct BulkArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// Which resource, by its position in the bulk data table. Without `--out` or `--in` the
    /// table is listed.
    #[arg(long, value_name = "N", requires = "bytes")]
    resource: Option<u32>,

    /// Write the resource's bytes to this file.
    #[arg(long, value_name = "FILE", group = "bytes")]
    out: Option<String>,

    /// Replace the resource's bytes with this file's and save the result into a mod.
    #[arg(long = "in", value_name = "FILE", group = "bytes")]
    input: Option<String>,

    #[command(flatten)]
    write: AssetWriteArgs,
}

#[derive(Args)]
struct AuditArgs {
    /// What to walk: `game` for every package the game loads, as `--all` walks, an installed mod by
    /// its whole name or a path, or a game container such as `pakchunk0-Windows`.
    #[arg(value_name = "SOURCE", conflicts_with = "dir")]
    source: Option<String>,

    /// Walk every package the game loads, each from the container that wins: the newest patch
    /// declares them all, and an install with no patch container has them listed from every
    /// container it mounts. Takes a minute or two, spread over every core.
    #[arg(long, conflicts_with_all = ["source", "dir"])]
    all: bool,

    /// Folder of already-extracted .uasset files to walk instead of a container.
    #[arg(long, value_name = "DIR")]
    dir: Option<String>,

    /// Stop after this many packages.
    #[arg(long, value_name = "N")]
    limit: Option<usize>,

    /// Only walk packages whose path contains this text, case insensitive: a piece of the container path (`Marvel/UI/Setting`) or of the package name (`/Game/Marvel/UI/Setting`), with either slash.
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

    /// Widen every literal in every script that can change size, in memory, and hold the result
    /// to what relocation promises: every offset into each script moved with the code it named.
    /// Each literal is then narrowed back, which has to give the package's own bytes again.
    #[arg(long)]
    relocation_check: bool,

    /// Print every whole script as assembler text and assemble the text again, which has to give
    /// back the package's own bytes: the check that a script's text says all its bytes do.
    #[arg(long)]
    text_check: bool,
}

#[derive(Args)]
struct CheckStructsArgs {
    /// Where the structs are read: `game` for every package the game ships, an installed mod by its
    /// whole name or a path, or a game container such as `pakchunk0-Windows`.
    #[arg(value_name = "SOURCE")]
    source: String,

    /// Only read packages whose path contains this text, case insensitive.
    #[arg(long, value_name = "TEXT")]
    filter: Option<String>,

    /// Stop after this many packages.
    #[arg(long, value_name = "N")]
    limit: Option<usize>,

    /// Report progress to stderr while reading.
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
enum ModsCmd {
    /// List every mod in `~mods` with its state, kind, load order, size and obfuscation. `--ini`
    /// lists the paks carrying editable INI files instead.
    List(ModsListArgs),
    /// List the files more than one switched-on mod carries, and whose copy the game loads.
    Conflicts(ModsConflictsArgs),
    /// Convert the packages an IoStore mod ships back into loose .uasset/.uexp files, with the
    /// files its pak carries beside them, or unpack a plain pak.
    Extract(ModsExtractArgs),
    /// List the files a mod holds: its container's packages and its pak's own files, switched on
    /// or off.
    Files(ModsFilesArgs),
    /// Summarise an IoStore mod: overrides, natives it calls, files and save slots it touches.
    Report(ModsReportArgs),
    /// Check a whole IoStore mod in one go: every package reads and its scripts come back from
    /// their own text, every import is there, its overrides match the game's layout, nothing is
    /// left unused, its pak holds nothing the game does not support, its combined tables agree
    /// with their parents, and it names no text forbidden. Fails when a package does not read back
    /// or an import or forbidden text is found.
    Check(ModsCheckArgs),
    /// Drop what a mod's packages hold that nothing in them uses, in one write: load-order edges
    /// to another export nothing in the waiting export names, imports nothing names and names
    /// nothing uses, which is what `mods check` warns about under unused and load order, and the
    /// entries in its pak the game does not support, under pak entries. `--pak-only` takes out
    /// those entries alone, for one mod or with `--all` every installed IoStore mod.
    Tidy(ModsTidyArgs),
    /// Check the packages a mod overrides against the game's current copies: what the game's copy
    /// has that the mod's lacks or declares differently, which a game update leaves behind.
    Overrides(ModsOverridesArgs),
    /// Compare two IoStore mods package by package: the packages only one holds, how many both hold
    /// the same, and how each package both hold differs: its objects, members, scripts, values and
    /// imports.
    Diff(ModsDiffArgs),
    /// Find where an IoStore mod's scripts and values name a string, function, variable or object.
    Search(ModsSearchArgs),
    /// Replace text in a mod's script strings, stored strings and texts and string table entries,
    /// in one write. Names and paths are left alone: rename or move those.
    Replace(ModsReplaceArgs),
    /// Move packages a mod added, or whole folders of them, to other paths inside it: the mod's
    /// other packages, path strings and map keys follow.
    Move(ModsMoveArgs),
    /// Copy a mod to a new name, rebuilt so the game mounts it as a mod of its own, whole or only
    /// some of its packages.
    Copy(ModsCopyArgs),
    /// Rename a mod where it is, rebuilt under the new name its container's id comes from.
    Rename(ModsRenameArgs),
    /// Take packages out of a mod in one write. Refused while a package it keeps imports one it
    /// takes out.
    Remove(ModsRemoveArgs),
    /// Switch mods on: their files lose their `.disabled` suffix.
    Enable(ModsToggleArgs),
    /// Switch mods off: their files gain a `.disabled` suffix, which the game skips.
    Disable(ModsToggleArgs),
    /// Delete mods, their .pak, .utoc and .ucas switched on or off.
    Delete(ModsDeleteArgs),
    /// Pack every switched-on mod in `~mods` into a .zip or .7z, to share or keep.
    ExportArchive(ModsExportArchiveArgs),
    /// Pack a folder of loose packages, as `mods extract` writes them, into an IoStore mod.
    Pack(ModsPackArgs),
    /// Convert mods where they are between a plain pak and IoStore. What the mod held is checked to
    /// be in what it becomes before that is swapped in.
    Convert(ModsConvertArgs),
    /// Install mods into `~mods`: a .pak with its .utoc and .ucas, either of those for the .pak
    /// beside it, a .zip, .7z or .rar, or a folder holding any mix of them and mods' loose files,
    /// at any depth. A folder of loose files, and a plain pak holding packages, which the game
    /// loads none of from one, are packed into a container as they go in. An IoStore mod's pak
    /// goes in without the entries older packers left there, which the game does not support.
    Install(ModsInstallArgs),
    /// Show what each package of a mod imports and names, what imports it, and which ones the
    /// game reaches from the packages the mod overrides.
    Deps(ModsDepsArgs),
}

#[derive(Subcommand)]
enum LogsCmd {
    /// List the game's sessions, the newest first: how each ended, its build, and its crash.
    List(LogsListArgs),
    /// Decode a session's log into its text, whole or only the lines a filter keeps.
    Decode(LogsDecodeArgs),
    /// Summarise a session: how it ended, its crash, its errors grouped alike, and the mods
    /// shipping the packages those errors name.
    Analyze(LogsAnalyzeArgs),
    /// Say which key decodes logs, the bundled one or the one set in the desktop app, and whether
    /// it opens the newest log.
    Key,
}

#[derive(Args)]
struct LogsListArgs {
    /// Only the sessions that crashed.
    #[arg(long)]
    crashes: bool,

    /// List at most this many.
    #[arg(long, value_name = "N")]
    limit: Option<usize>,
}

#[derive(Args)]
struct LogArg {
    /// The session: a `.log` file, a crash folder, its whole name such as
    /// `2026.10.14_02.13.57-8832`, `latest` or `latest-crash`.
    #[arg(value_name = "LOG")]
    log: String,
}

#[derive(Clone, Copy, ValueEnum)]
enum VerbosityArg {
    Fatal,
    Error,
    Warning,
    Display,
    Log,
    Verbose,
}

impl From<VerbosityArg> for rivals_core::game_logs::Verbosity {
    fn from(arg: VerbosityArg) -> Self {
        match arg {
            VerbosityArg::Fatal => Self::Fatal,
            VerbosityArg::Error => Self::Error,
            VerbosityArg::Warning => Self::Warning,
            VerbosityArg::Display => Self::Display,
            VerbosityArg::Log => Self::Log,
            VerbosityArg::Verbose => Self::Verbose,
        }
    }
}

#[derive(Args)]
struct LogsDecodeArgs {
    #[command(flatten)]
    target: LogArg,

    /// Write the text to this file instead of printing it.
    #[arg(long, value_name = "FILE")]
    out: Option<PathBuf>,

    /// Write over a file already there.
    #[arg(long)]
    overwrite: bool,

    /// Say what would be written, and write nothing.
    #[arg(long)]
    dry_run: bool,

    /// Replace the player's Windows name, account and machine ids, tokens, addresses and
    /// command-line values, to share the log.
    #[arg(long)]
    redact: bool,

    /// Keep only lines of these verbosities. Repeat it or separate them with commas. A line with
    /// no prefix, such as a traceback's, takes the verbosity of the line it continues.
    #[arg(long, value_enum, value_delimiter = ',')]
    verbosity: Vec<VerbosityArg>,

    /// Keep only lines of these categories, such as `LogPakFile`. Repeat it or separate them with
    /// commas.
    #[arg(long, value_delimiter = ',', value_name = "CATEGORY")]
    category: Vec<String>,

    /// Keep only lines holding this text, in any case.
    #[arg(long, value_name = "TEXT")]
    filter: Option<String>,
}

#[derive(Args)]
struct LogsAnalyzeArgs {
    #[command(flatten)]
    target: LogArg,

    /// Print the redacted summary to paste into a bug report instead.
    #[arg(long)]
    share: bool,

    /// List every error group, not only the first 20.
    #[arg(long)]
    all: bool,
}

#[derive(Subcommand)]
enum TweaksCmd {
    /// Show every tweak in the catalogue.
    List,
    /// Report which tweaks a pak currently has on.
    Status(PakArgs),
    /// Turn tweaks on or off in a pak.
    Apply(ApplyArgs),
    /// List the tweak presets saved by the desktop app.
    Presets,
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

    /// Apply a preset saved by the desktop app, by name. `--on`, `--off` and `--set` still apply
    /// on top of it.
    #[arg(long, value_name = "NAME")]
    preset: Option<String>,

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
struct ModsListArgs {
    /// List the paks carrying INI files the tweak catalogue understands, instead of every mod.
    #[arg(long)]
    ini: bool,

    /// List the paks holding any INI file, not just the ones the tweak catalogue understands.
    #[arg(long)]
    all_ini: bool,

    #[command(flatten)]
    scan: RecursiveArgs,
}

/// How far into `~mods` to look.
#[derive(Args, Clone, Copy, Default)]
struct RecursiveArgs {
    /// Search `~mods` subfolders. Defaults to the desktop app's setting.
    #[arg(long)]
    recursive: bool,

    /// Scan only the top level of `~mods`.
    #[arg(long, conflicts_with = "recursive")]
    no_recursive: bool,
}

impl RecursiveArgs {
    /// Neither flag given means whatever the desktop app is set to.
    fn resolve(&self, app: &settings::AppSettings) -> bool {
        match (self.recursive, self.no_recursive) {
            (true, _) => true,
            (_, true) => false,
            _ => app.recursive_mod_scan,
        }
    }
}

#[derive(Args)]
struct ModsConflictsArgs {
    /// List every file more than one mod carries, not just each mod's count.
    #[arg(long)]
    assets: bool,

    #[command(flatten)]
    scan: RecursiveArgs,
}

#[derive(Args)]
struct ModsExtractArgs {
    /// The mod. A plain pak is unpacked as it is.
    #[command(flatten)]
    from: ModFileArgs,

    /// Folder the package tree is written under.
    #[arg(long, value_name = "DIR")]
    out: PathBuf,

    /// Only packages, or files of a plain pak, whose path contains this text, case insensitive: a
    /// piece of the container path (`Marvel/UI/Setting`) or of the package name
    /// (`/Game/Marvel/UI/Setting`), with either slash. Repeatable.
    #[arg(long, value_name = "TEXT")]
    filter: Vec<String>,
}

#[derive(Args)]
struct ModsFilesArgs {
    #[command(flatten)]
    from: ModFileArgs,

    /// Only the files whose path or package name contains this, ignoring case. Repeatable.
    #[arg(long, value_name = "TEXT")]
    filter: Vec<String>,
}

#[derive(Args)]
struct ModsSearchArgs {
    #[command(flatten)]
    target: ModFileArgs,

    /// Text to look for, ignoring case.
    #[arg(long, value_name = "TEXT")]
    find: String,

    /// Also look in each package's name table, where a name nothing uses any more is still found,
    /// and say which of those it finds are unused.
    #[arg(long)]
    names: bool,
}

#[derive(Args)]
struct ModFileArgs {
    /// The mod: a path to its .pak or .utoc, or its whole name in `~mods` with or without the
    /// extension, in any case, switched on or off.
    #[arg(value_name = "MOD")]
    target_mod: String,
}

impl ModFileArgs {
    fn get(&self) -> &str {
        &self.target_mod
    }
}

/// The mods a command works on, given first.
#[derive(Args)]
struct ModsArgs {
    /// The mods: each a path to its .pak or .utoc, or its whole name in `~mods` with or without the
    /// extension, in any case.
    #[arg(value_name = "MOD")]
    target_mods: Vec<String>,
}

impl ModsArgs {
    fn all(&self) -> &[String] {
        &self.target_mods
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum CompressionArg {
    /// What the game's own files use.
    Oodle,
    /// Needs no Oodle library to read or write.
    Zlib,
}

/// How a command compresses the files it writes.
#[derive(Args, Clone, Copy, Default)]
struct CompressArgs {
    /// How the files written are compressed. Defaults to Oodle, as the game's are.
    #[arg(long, value_enum)]
    compression: Option<CompressionArg>,

    /// How hard Oodle works: none, fast, normal, optimal1, optimal2 or optimal3. Defaults to the
    /// desktop app's mod setting, then to normal.
    #[arg(long, value_name = "LEVEL", value_parser = rivals_core::pak::profile::OodleLevel::parse)]
    oodle_level: Option<rivals_core::pak::profile::OodleLevel>,
}

/// How a command writes a mod it rewrites.
#[derive(Args, Clone, Copy, Default)]
struct WriteArgs {
    #[command(flatten)]
    compress: CompressArgs,

    /// Compress what the mod already holds again, at `--compression` and `--oodle-level`. Without
    /// this, a rewrite keeps each chunk's blocks as they were stored, the mod's own compression
    /// and size, and compresses again only a block in another method.
    #[arg(long)]
    recompress: bool,
}

impl WriteArgs {
    /// The container's options, `obfuscate` as the command says.
    fn io(
        &self,
        app: &settings::AppSettings,
        obfuscate: Option<bool>,
    ) -> rivals_core::pak::iostore_out::IoStoreOptions {
        rivals_core::pak::iostore_out::IoStoreOptions {
            reencode: self.recompress,
            ..self.compress.io(app, obfuscate)
        }
    }

    fn pak(&self, app: &settings::AppSettings) -> rivals_core::pak::plain::PakOptions {
        self.compress.pak(app)
    }
}

impl CompressArgs {
    fn compression(&self) -> rivals_core::pak::profile::PackCompression {
        use rivals_core::pak::profile::PackCompression;
        match self.compression {
            Some(CompressionArg::Zlib) => PackCompression::Zlib,
            Some(CompressionArg::Oodle) | None => PackCompression::Oodle,
        }
    }

    fn level(&self, app: &settings::AppSettings) -> rivals_core::pak::profile::OodleLevel {
        self.oodle_level
            .or(app.mod_compression_level)
            .unwrap_or_default()
    }

    /// The options of a container written anew, `obfuscate` as the command says.
    fn io(
        &self,
        app: &settings::AppSettings,
        obfuscate: Option<bool>,
    ) -> rivals_core::pak::iostore_out::IoStoreOptions {
        rivals_core::pak::iostore_out::IoStoreOptions {
            compression: self.compression(),
            oodle_level: Some(self.level(app).to_oodle()),
            obfuscate,
            reencode: false,
        }
    }

    fn pak(&self, app: &settings::AppSettings) -> rivals_core::pak::plain::PakOptions {
        rivals_core::pak::plain::PakOptions {
            compression: self.compression(),
            oodle_level: Some(self.level(app).to_oodle()),
        }
    }
}

#[derive(Args)]
struct ModsCopyArgs {
    #[command(flatten)]
    from: ModFileArgs,

    /// The copy's name, in `~mods`, or a path to put it elsewhere. A folder that exists, or a path
    /// ending in a separator, takes the copy under the mod's own name, as `cp` does. Keep the
    /// `_<number>_P` suffix so it mounts above the game's patches.
    #[arg(long, value_name = "NAME", required_unless_present = "into")]
    to: Option<String>,

    /// Copy the packages into this IoStore mod instead, switched on or off, keeping everything else
    /// it holds. Each goes in as the mod stores it, over its own copy of it.
    #[arg(long, value_name = "MOD", conflicts_with_all = ["to", "overwrite"])]
    into: Option<String>,

    /// Keep only the packages whose path or name contains this, ignoring case. Repeat to keep
    /// several; the rest are left out. `--allow-missing` lets a kept package import one left out.
    #[arg(long = "filter", value_name = "TEXT")]
    filters: Vec<String>,

    /// Write over a mod already at the copy's name.
    #[arg(long)]
    overwrite: bool,

    /// Copy packages that import one left out, which then fail to load unless what imports it is
    /// taken out too.
    #[arg(long)]
    allow_missing: bool,

    /// List what the copy keeps and leaves out, and write nothing.
    #[arg(long)]
    dry_run: bool,

    #[command(flatten)]
    write: WriteArgs,
}

#[derive(Args)]
struct ModsRenameArgs {
    #[command(flatten)]
    from: ModFileArgs,

    /// The new name, with or without an extension.
    #[arg(long, value_name = "NAME")]
    to: String,

    /// Say what the rename would do, and write nothing.
    #[arg(long)]
    dry_run: bool,

    #[command(flatten)]
    write: WriteArgs,
}

#[derive(Args)]
struct ModsRemoveArgs {
    #[command(flatten)]
    from: ModFileArgs,

    /// A package to take out, by its path in the container or its package name. Repeatable.
    #[arg(long = "package", value_name = "PACKAGE")]
    entries: Vec<String>,

    /// Take out every package whose path or name contains this, ignoring case. Repeatable.
    #[arg(long = "filter", value_name = "TEXT")]
    filters: Vec<String>,

    /// Take out packages a kept one imports, which then fails to load unless what imports it goes
    /// too.
    #[arg(long)]
    allow_missing: bool,

    /// List what would go, and write nothing.
    #[arg(long)]
    dry_run: bool,

    #[command(flatten)]
    write: WriteArgs,
}

#[derive(Args)]
struct ModsPackArgs {
    /// The folder of loose packages, laid out from `Marvel/Content` as `mods extract` writes it, or
    /// a plain pak, switched on or off, which is unpacked first. Anything that is not a package goes
    /// in the mod's pak.
    #[arg(value_name = "DIR|PAK")]
    dir: String,

    /// The mod to write: a name in `~mods`, or a path to put it elsewhere that ends in its name, so
    /// a folder alone is refused. Keep the `_<number>_P` suffix so it mounts above the game's
    /// patches.
    #[arg(long, value_name = "NAME", required_unless_present = "into")]
    to: Option<String>,

    /// Pack the folder's packages into this IoStore mod instead, switched on or off, keeping
    /// everything else it holds. Files that are not packages are named and left out.
    #[arg(long, value_name = "MOD", conflicts_with_all = ["to", "overwrite", "obfuscate", "format"])]
    into: Option<String>,

    /// What the mod is. `pak` packs every file into a plain pak, with no container: for INIs,
    /// movies and other loose files, since the game loads no package from a plain pak. Defaults
    /// to iostore.
    #[arg(long, value_enum)]
    format: Option<FormatArg>,

    /// Obfuscate the container the way the game's own are.
    #[arg(long)]
    obfuscate: bool,

    /// Write over a mod already at that name.
    #[arg(long)]
    overwrite: bool,

    /// Build the mod and check it, and write nothing.
    #[arg(long)]
    dry_run: bool,

    #[command(flatten)]
    compress: CompressArgs,

    /// With `--into`, compress what that mod already holds again, at `--compression` and
    /// `--oodle-level`. Without this, its other packages keep their blocks as they were stored.
    #[arg(long, conflicts_with = "to")]
    recompress: bool,
}

#[derive(Args)]
struct ModsInstallArgs {
    /// What to install: a mod's .pak, .utoc or .ucas, an archive, or a folder. A folder holding a
    /// Marvel or Engine folder is a mod's loose files, named after it, and any other folder is
    /// looked through for mods.
    #[arg(required = true, value_name = "PATH")]
    files: Vec<PathBuf>,

    /// Install into this folder rather than the game's `~mods`, such as to make fixed copies of
    /// mods to share.
    #[arg(long, value_name = "DIR")]
    to: Option<PathBuf>,

    /// Write over a mod of the same name already installed, switched on or off.
    #[arg(long)]
    overwrite: bool,

    /// Say what would be installed, and copy nothing.
    #[arg(long)]
    dry_run: bool,

    /// How a mod packed as it goes in, and a pak written again without what does not belong in
    /// it, are compressed.
    #[command(flatten)]
    compress: CompressArgs,
}

#[derive(Clone, Copy, ValueEnum)]
enum FormatArg {
    /// A plain pak, which delivers loose files only.
    Pak,
    /// A container the game loads packages from, with a pak beside it.
    Iostore,
}

#[derive(Args)]
#[command(group(clap::ArgGroup::new("named").required(true).multiple(true).args(["target_mods", "all"])))]
struct ModsConvertArgs {
    #[command(flatten)]
    mods: ModsArgs,

    /// Every switched-on mod in `~mods` not already in that format.
    #[arg(long, conflicts_with = "target_mods")]
    all: bool,

    /// What the mods become.
    #[arg(long, value_enum)]
    format: FormatArg,

    /// Convert each mod and check what it becomes, and leave the mod as it was.
    #[arg(long)]
    dry_run: bool,

    /// Obfuscate the container. Without this or `--no-obfuscate`, a container keeps what it had.
    #[arg(long, conflicts_with = "no_obfuscate")]
    obfuscate: bool,

    /// Write the container plain.
    #[arg(long)]
    no_obfuscate: bool,

    /// With `--all`, how far into `~mods` to look.
    #[command(flatten)]
    scan: RecursiveArgs,

    #[command(flatten)]
    write: WriteArgs,
}

#[derive(Args)]
struct ModsDepsArgs {
    #[command(flatten)]
    from: ModFileArgs,

    /// List only the packages nothing the game loads leads to.
    #[arg(long)]
    unreached: bool,
}

fn mods_pack(cli: &Cli, app: &settings::AppSettings, args: &ModsPackArgs) -> Result<(), String> {
    let plain = matches!(args.format, Some(FormatArg::Pak));
    if plain && args.obfuscate {
        return Err("a plain pak has no container to obfuscate".into());
    }
    block_for_game(cli, args.dry_run)?;
    if let Some(into) = &args.into {
        let into = mod_files(cli, app, into)?;
        let report = rivals_core::mod_ops::pack_into_mod(
            Path::new(&args.dir),
            &into,
            &rivals_core::pak::iostore_out::IoStoreOptions {
                reencode: args.recompress,
                ..args.compress.io(app, None)
            },
            args.dry_run,
        )?;
        return emit(cli, &report, || print_into(&report, "packed", "would pack"));
    }
    let to_name = args.to.as_deref().unwrap_or_default();
    let to = Path::new(to_name);
    let mods = if to.components().count() > 1 || to.is_absolute() {
        PathBuf::new()
    } else {
        rivals_core::paths::mods_dir(&resolve::game_root(cli.game_root.as_deref(), app)?)
    };
    let report = if plain {
        rivals_core::mod_ops::pack_plain_mod(
            Path::new(&args.dir),
            to_name,
            &mods,
            args.overwrite,
            &args.compress.pak(app),
            args.dry_run,
        )?
    } else {
        // Only a shader archive in the folder needs the game's own libraries, so the game is
        // looked for then, even for a mod written outside it.
        let game_libraries = || {
            let root = resolve::game_root(cli.game_root.as_deref(), app).map_err(|e| {
                format!("checking a shader archive against the game's own libraries: {e}")
            })?;
            rivals_core::pak::shader_library::game_library_names(&rivals_core::paths::paks_dir(
                &root,
            ))
        };
        rivals_core::mod_ops::pack_mod(
            Path::new(&args.dir),
            rivals_core::pak::pack::Take::Everything,
            to_name,
            &mods,
            args.overwrite,
            &args.compress.io(app, Some(args.obfuscate)),
            &game_libraries,
            args.dry_run,
        )?
    };
    emit(cli, &report, || {
        let verb = if args.dry_run { "would pack" } else { "packed" };
        if plain {
            outln!(
                "{verb} {} file(s) into the plain pak {}",
                report.loose.len(),
                report.mod_file
            );
        } else {
            outln!(
                "{verb} {} package(s) into {}",
                report.packages,
                report.mod_file
            );
            for library in &report.shader_libraries {
                outln!("  shader library: {library}");
            }
            for loose in &report.loose {
                outln!("  in its pak: {loose}");
            }
        }
        for warning in &report.warnings {
            outln!("warning: {warning}");
        }
    })
}

/// Ends a command that worked through several mods, some of which failed: the results said which
/// and why, so JSON gets no second document, and otherwise a line says how many.
fn some_failed(cli: &Cli, failed: usize, of: usize, what: &str) -> Result<(), String> {
    if failed == 0 {
        return Ok(());
    }
    if cli.json {
        std::process::exit(1);
    }
    Err(format!("{failed} of {of} {what}"))
}

fn mods_install(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &ModsInstallArgs,
) -> Result<(), String> {
    use rivals_core::mod_library::{Found, InstallResult, PackedFrom};

    let mods = match &args.to {
        Some(dir) => dir.clone(),
        None => {
            block_for_game(cli, args.dry_run)?;
            rivals_core::paths::mods_dir(&resolve::game_root(cli.game_root.as_deref(), app)?)
        }
    };
    // Only a shader archive in a mod packed as it goes in needs the game's own libraries, so the
    // game is looked for then, even for mods installed elsewhere.
    let game_libraries = || {
        let root = resolve::game_root(cli.game_root.as_deref(), app).map_err(|e| {
            format!("checking a shader archive against the game's own libraries: {e}")
        })?;
        rivals_core::pak::shader_library::game_library_names(&rivals_core::paths::paks_dir(&root))
    };
    let options = rivals_core::mod_library::InstallOptions {
        replace: args.overwrite,
        dry_run: args.dry_run,
        io: args.compress.io(app, None),
        game_libraries: &game_libraries,
    };
    // A mod named by several of its files installs once.
    let results = rivals_core::mod_library::install_each(&mods, &args.files, &options);
    let found_json = |found: &Found| match &found.result {
        Ok(installed) => serde_json::json!({ "at": found.at, "ok": true, "installed": installed }),
        Err(error) => serde_json::json!({ "at": found.at, "ok": false, "error": error }),
    };
    let json: Vec<serde_json::Value> = results
        .iter()
        .map(|each| {
            let sources: Vec<String> = each
                .named
                .iter()
                .map(|file| file.display().to_string())
                .collect();
            match &each.result {
                Ok(found) => serde_json::json!({
                    "sources": sources,
                    "ok": found.iter().all(|found| found.result.is_ok()),
                    "mods": found.iter().map(found_json).collect::<Vec<_>>(),
                }),
                Err(error) => serde_json::json!({
                    "sources": sources,
                    "ok": false,
                    "error": error,
                }),
            }
        })
        .collect();
    let (mut tried, mut failed) = (0, 0);
    for each in &results {
        match &each.result {
            Ok(found) => {
                tried += found.len();
                failed += found.iter().filter(|found| found.result.is_err()).count();
            }
            Err(_) => {
                tried += 1;
                failed += 1;
            }
        }
    }
    let print = |installed: &InstallResult, from: &str| {
        let how = match (installed.replaced_enabled, installed.replaced_disabled) {
            (true, _) => ", over the copy that was there",
            (false, true) => ", over the switched-off copy, now switched on",
            _ => "",
        };
        let without = if installed.stripped.is_empty() {
            String::new()
        } else {
            format!(
                ", without the {} its pak carried, which do not belong in it",
                installed.stripped.join(" and ")
            )
        };
        let packed = match &installed.packed {
            Some(packed) if packed.from == PackedFrom::Folder => format!(
                ", packed from its loose files: {} package(s)",
                packed.packages
            ),
            Some(packed) => format!(
                ", packed into a container, since the game loads no package from a plain pak: \
                 {} package(s)",
                packed.packages
            ),
            None => String::new(),
        };
        outln!(
            "{} {}{from}{how}{without}{packed}",
            if args.dry_run {
                "would install"
            } else {
                "installed"
            },
            installed.file_name
        );
        if let Some(packed) = &installed.packed {
            for library in &packed.shader_libraries {
                outln!("  shader library: {library}");
            }
            for loose in &packed.loose {
                outln!("  in its pak: {loose}");
            }
            for warning in &packed.warnings {
                outln!("  warning: {warning}");
            }
        }
    };
    emit(cli, &serde_json::json!({ "results": json }), || {
        for each in &results {
            let named = each.named[0].display().to_string();
            match &each.result {
                Err(error) => {
                    let sources: Vec<String> = each
                        .named
                        .iter()
                        .map(|file| file.display().to_string())
                        .collect();
                    outln!("{}: not installed: {error}", sources.join(", "));
                }
                Ok(found) => {
                    for found in found {
                        match &found.result {
                            Ok(installed) if found.at == named => print(installed, ""),
                            Ok(installed) => print(installed, &format!(" from {}", found.at)),
                            Err(error) => outln!("{}: not installed: {error}", found.at),
                        }
                    }
                }
            }
        }
    })?;
    some_failed(cli, failed, tried, "mod(s) did not install")
}

fn mods_convert(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &ModsConvertArgs,
) -> Result<(), String> {
    use rivals_core::mod_convert::{ConvertPhase, ModFormat};
    use rivals_core::mod_ops::ModFiles;

    block_for_game(cli, args.dry_run)?;
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let to = match args.format {
        FormatArg::Pak => ModFormat::Pak,
        FormatArg::Iostore => ModFormat::Iostore,
    };
    let targets: Vec<ModFiles> = if args.all {
        let mods = rivals_core::paths::mods_dir(&root);
        rivals_core::mods::walk_mod_files(&mods, args.scan.resolve(app))
            .into_iter()
            .filter(|rel| rel.extension().is_some_and(|e| e == "pak"))
            .filter_map(|rel| ModFiles::of(&mods.join(rel)).ok())
            .filter(|files| files.is_iostore() != (to == ModFormat::Iostore))
            .collect()
    } else {
        args.mods
            .all()
            .iter()
            .map(|pak| mod_files(cli, app, pak))
            .collect::<Result<_, _>>()?
    };
    let obfuscate = match (args.obfuscate, args.no_obfuscate) {
        (true, _) => Some(true),
        (_, true) => Some(false),
        _ => None,
    };
    let io = args.write.io(app, obfuscate);
    let quiet = cli.json;

    let mut results = Vec::new();
    for files in &targets {
        let shown = std::sync::atomic::AtomicBool::new(false);
        let progress = |phase: ConvertPhase, done: usize, total: usize| {
            if !quiet {
                let doing = match phase {
                    ConvertPhase::Extracting => "extracting",
                    ConvertPhase::Repacking => "repacking",
                };
                eprint!("\r{}: {doing} {done}/{total}   ", files.stem);
                shown.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        };
        let outcome = rivals_core::mod_convert::convert_mod(
            &root,
            files,
            to,
            &io,
            args.dry_run,
            None,
            &std::sync::atomic::AtomicBool::new(false),
            &progress,
        );
        if shown.load(std::sync::atomic::Ordering::Relaxed) {
            eprintln!();
        }
        results.push(match outcome {
            Ok(report) => serde_json::json!({ "mod": files.stem, "ok": true, "report": report }),
            Err(error) => serde_json::json!({ "mod": files.stem, "ok": false, "error": error }),
        });
    }
    let failed = results
        .iter()
        .filter(|result| result["ok"] == false)
        .count();
    emit(cli, &serde_json::json!({ "results": results }), || {
        if results.is_empty() {
            outln!("no mod to convert to {}", to.name());
        }
        for result in &results {
            let name = result["mod"].as_str().unwrap_or_default();
            if result["ok"] == true {
                let report = &result["report"];
                let obfuscated = if report["obfuscated"] == true {
                    ", obfuscated"
                } else {
                    ""
                };
                outln!(
                    "{name}: {} {}{obfuscated}, {} item(s) in, {} out, {} carried in its pak",
                    if args.dry_run { "would become" } else { "now" },
                    to.name(),
                    report["assets_in"],
                    report["assets_out"],
                    report["carried_pak_entries"]
                );
                if report["direct"] == true {
                    outln!(
                        "  its blocks copied as stored: {} copied, {} compressed again",
                        report["copied_blocks"],
                        report["recompressed_blocks"]
                    );
                }
                for library in report["shader_libraries"].as_array().into_iter().flatten() {
                    outln!("  shader library: {}", library.as_str().unwrap_or_default());
                }
                for warning in report["warnings"].as_array().into_iter().flatten() {
                    outln!("  warning: {}", warning.as_str().unwrap_or_default());
                }
            } else {
                outln!(
                    "{name}: not converted: {}",
                    result["error"].as_str().unwrap_or_default()
                );
            }
        }
    })?;
    some_failed(cli, failed, results.len(), "mod(s) did not convert")
}

fn mods_deps(cli: &Cli, app: &settings::AppSettings, args: &ModsDepsArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let files = mod_files(cli, app, args.from.get())?;
    let report = rivals_core::mod_ops::mod_deps(&root, &files)?;
    emit(cli, &report, || {
        use rivals_core::mod_ops::Reach;
        if !args.unreached {
            outln!(
                "{}: {} package(s), {} overriding the game's, {} added",
                files.stem,
                report.packages.len(),
                report.overrides,
                report.packages.len() - report.overrides
            );
            for package in &report.packages {
                let how = match package.reach {
                    Reach::Override => "overrides the game's",
                    Reach::Imported => "imported",
                    Reach::Named => "named only",
                    Reach::Unreached => "unreached",
                };
                outln!("{}  ({how})", package.package);
                if !package.imports.is_empty() || package.game_imports > 0 {
                    outln!(
                        "  imports {}{}",
                        package.imports.join(", "),
                        match (package.imports.is_empty(), package.game_imports) {
                            (_, 0) => String::new(),
                            (true, n) => format!("{n} of the game's"),
                            (false, n) => format!(", and {n} of the game's"),
                        }
                    );
                }
                if !package.names.is_empty() {
                    outln!("  names {}", package.names.join(", "));
                }
                if !package.imported_by.is_empty() {
                    outln!("  imported by {}", package.imported_by.join(", "));
                }
                for missing in &package.missing {
                    outln!("  missing: imports {missing}, which neither the mod nor the game has");
                }
            }
        }
        if report.unreached.is_empty() {
            outln!("every package is reached from one the mod overrides");
        } else {
            outln!(
                "{} package(s) nothing the game loads leads to, unless native code asks for them by path:",
                report.unreached.len()
            );
            for package in &report.unreached {
                outln!("  {package}");
            }
        }
    })
}

fn mod_files(
    cli: &Cli,
    app: &settings::AppSettings,
    name: &str,
) -> Result<rivals_core::mod_ops::ModFiles, String> {
    let path = resolve::installed_mod(name, cli.game_root.as_deref(), app)?;
    rivals_core::mod_ops::ModFiles::of(&path)
}

/// The plain pak of the mod `name` names, which is where its INI files are.
fn ini_pak(cli: &Cli, app: &settings::AppSettings, name: &str) -> Result<String, String> {
    Ok(mod_files(cli, app, name)?
        .part("pak")
        .to_string_lossy()
        .into_owned())
}

/// The container of the IoStore mod `pak` names, switched on or off. A command that rewrites it
/// asks for it `switched_on`.
fn mod_utoc(
    cli: &Cli,
    app: &settings::AppSettings,
    pak: &str,
    switched_on: bool,
) -> Result<PathBuf, String> {
    let files = mod_files(cli, app, pak)?;
    if !files.is_iostore() {
        return Err(format!("{} is not an IoStore mod", files.stem));
    }
    if switched_on && files.disabled {
        return Err(format!(
            "{} is switched off; `mods enable` it to change it",
            files.stem
        ));
    }
    Ok(files.part("utoc"))
}

fn block_for_game(cli: &Cli, dry_run: bool) -> Result<(), String> {
    if !dry_run && !cli.force && rivals_core::game_status::should_block_for_game() {
        return Err(rivals_core::game_status::game_running_error());
    }
    Ok(())
}

fn print_needed(needed: &[rivals_core::mod_ops::Needed]) {
    for need in needed {
        if need.imported {
            outln!("  needed: {} imports {}", need.package, need.needs);
        } else {
            outln!(
                "  note: {} names {}, which it may load when it runs",
                need.package,
                need.needs
            );
        }
    }
}

/// What went into an existing mod, or would: `done` and `would` say it in the command's words.
fn print_into(report: &rivals_core::mod_ops::IntoReport, done: &str, would: &str) {
    let done = if report.written { done } else { would };
    outln!(
        "{done} {} package(s) from {} into {}",
        report.copied.len(),
        report.from,
        report.into
    );
    for copied in &report.copied {
        let over = if report.replaced.contains(copied) {
            ", over its copy"
        } else {
            ""
        };
        outln!("  {copied}{over}");
    }
    print_needed(&report.needed);
    for warning in &report.warnings {
        outln!("warning: {warning}");
    }
}

fn mods_copy(cli: &Cli, app: &settings::AppSettings, args: &ModsCopyArgs) -> Result<(), String> {
    block_for_game(cli, args.dry_run)?;
    let from = mod_files(cli, app, args.from.get())?;
    let options = rivals_core::mod_ops::CopyOptions {
        only: args.filters.clone(),
        allow_missing: args.allow_missing,
        replace: args.overwrite,
        dry_run: args.dry_run,
    };
    let mods = resolve::game_root(cli.game_root.as_deref(), app)
        .map(|root| rivals_core::paths::mods_dir(&root));
    if let Some(into) = &args.into {
        let into = mod_files(cli, app, into)?;
        // Two mods named by their paths copy without a game; then neither is known to mount.
        let report = rivals_core::mod_ops::copy_into_mod(
            &from,
            &into,
            mods.as_deref().unwrap_or(Path::new("")),
            &options,
            &args.write.io(app, None),
        )?;
        return emit(cli, &report, || print_into(&report, "copied", "would copy"));
    }
    let report = rivals_core::mod_ops::copy_mod(
        &from,
        args.to.as_deref().unwrap_or_default(),
        &mods?,
        &options,
        &args.write.io(app, None),
    )?;
    emit(cli, &report, || {
        let verb = if report.written {
            "copied"
        } else {
            "would copy"
        };
        outln!("{verb} {} to {}", report.from, report.to);
        if !report.dropped.is_empty() || args.dry_run {
            for kept in &report.kept {
                outln!("  keeps {kept}");
            }
            for dropped in &report.dropped {
                outln!("  leaves out {dropped}");
            }
        }
        print_needed(&report.needed);
        for warning in &report.warnings {
            outln!("warning: {warning}");
        }
    })
}

fn mods_rename(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &ModsRenameArgs,
) -> Result<(), String> {
    block_for_game(cli, args.dry_run)?;
    let from = mod_files(cli, app, args.from.get())?;
    let report = if args.dry_run {
        rivals_core::mod_ops::plan_rename(&from, &args.to)?
    } else {
        rivals_core::mod_ops::rename_mod(&from, &args.to, &args.write.io(app, None))?
    };
    emit(cli, &report, || {
        let verb = if args.dry_run {
            "would rename"
        } else {
            "renamed"
        };
        let how = if report.rebuilt {
            ", rebuilt under its new name"
        } else {
            ""
        };
        outln!("{verb} {} to {}{how}", report.from, report.to);
        for warning in &report.warnings {
            outln!("warning: {warning}");
        }
    })
}

fn mods_remove(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &ModsRemoveArgs,
) -> Result<(), String> {
    block_for_game(cli, args.dry_run)?;
    if args.entries.is_empty() && args.filters.is_empty() {
        return Err("name what to take out with --package or --filter".into());
    }
    let files = mod_files(cli, app, args.from.get())?;
    let report = rivals_core::mod_ops::remove_packages(
        &files,
        &rivals_core::mod_ops::RemoveOptions {
            entries: args.entries.clone(),
            filters: args.filters.clone(),
            allow_missing: args.allow_missing,
            dry_run: args.dry_run,
        },
        &args.write.io(app, None),
    )?;
    emit(cli, &report, || {
        let verb = if report.written { "took" } else { "would take" };
        outln!(
            "{verb} {} package(s) out of {}, keeping {}",
            report.removed.len(),
            report.mod_file,
            report.kept
        );
        for removed in &report.removed {
            outln!("  {removed}");
        }
        print_needed(&report.needed);
        for warning in &report.warnings {
            outln!("warning: {warning}");
        }
    })
}

#[derive(Args)]
#[command(group(clap::ArgGroup::new("named").required(true).multiple(true).args(["target_mods", "all"])))]
struct ModsToggleArgs {
    #[command(flatten)]
    mods: ModsArgs,

    /// Every mod in `~mods` in the other state.
    #[arg(long, conflicts_with = "target_mods")]
    all: bool,

    /// Say what would switch, and rename nothing.
    #[arg(long)]
    dry_run: bool,

    /// With `--all`, how far into `~mods` to look.
    #[command(flatten)]
    scan: RecursiveArgs,
}

#[derive(Args)]
#[command(group(clap::ArgGroup::new("named").required(true).multiple(true).args(["target_mods"])))]
struct ModsDeleteArgs {
    #[command(flatten)]
    mods: ModsArgs,

    /// List what would go, and delete nothing.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args)]
struct ModsExportArchiveArgs {
    /// The archive to write: a .zip or a .7z.
    #[arg(value_name = "FILE")]
    out: PathBuf,

    /// Write over a file already there.
    #[arg(long)]
    overwrite: bool,

    /// Say how many mods would go in, and write nothing.
    #[arg(long)]
    dry_run: bool,

    #[command(flatten)]
    scan: RecursiveArgs,
}

/// Every mod below `~mods`, below its subfolders too when the flag says, whose pak the test
/// takes by its name there.
type AllMods<'a> = (bool, &'a dyn Fn(&str) -> bool);

/// Each mod as it was named, and what it resolved to.
type NamedMods = Vec<(String, Result<rivals_core::mod_ops::ModFiles, String>)>;

/// The mods named, or with `all`, the mods it picks. A name that does not resolve stays an
/// error of its own.
fn mods_named(
    cli: &Cli,
    app: &settings::AppSettings,
    names: &[String],
    all: Option<AllMods<'_>>,
) -> Result<NamedMods, String> {
    if let Some((recursive, keep)) = all {
        let root = resolve::game_root(cli.game_root.as_deref(), app)?;
        let mods = rivals_core::paths::mods_dir(&root);
        // The name as `mods list` shows it, and the file by the path as the system spells it.
        return Ok(rivals_core::mods::walk_mod_files(&mods, recursive)
            .into_iter()
            .map(|relative| (relative.to_string_lossy().replace('\\', "/"), relative))
            .filter(|(name, _)| keep(name))
            .map(|(name, relative)| {
                let files = rivals_core::mod_ops::ModFiles::of(&mods.join(&relative));
                (name, files)
            })
            .collect());
    }
    Ok(names
        .iter()
        .map(|pak| (pak.clone(), mod_files(cli, app, pak)))
        .collect())
}

fn mods_set_enabled(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &ModsToggleArgs,
    enabled: bool,
) -> Result<(), String> {
    block_for_game(cli, args.dry_run)?;
    // `--all` takes the mods in the other state, which are the ones the switch moves.
    let other = |name: &str| {
        if enabled {
            name.ends_with(".pak.disabled")
        } else {
            name.ends_with(".pak")
        }
    };
    let targets = mods_named(
        cli,
        app,
        args.mods.all(),
        args.all
            .then_some((args.scan.resolve(app), &other as &dyn Fn(&str) -> bool)),
    )?;
    let switch = if args.dry_run {
        rivals_core::mod_ops::plan_set_enabled
    } else {
        rivals_core::mod_ops::set_enabled
    };
    let results: Vec<serde_json::Value> = targets
        .into_iter()
        .map(
            |(name, files)| match files.and_then(|files| switch(&files, enabled)) {
                Ok((now, changed)) => {
                    serde_json::json!({ "mod": now.stem, "ok": true, "changed": changed })
                }
                Err(error) => serde_json::json!({ "mod": name, "ok": false, "error": error }),
            },
        )
        .collect();
    let failed = results
        .iter()
        .filter(|result| result["ok"] == false)
        .count();
    let state = if enabled { "on" } else { "off" };
    emit(cli, &serde_json::json!({ "results": results }), || {
        if results.is_empty() {
            outln!("no mod to switch {state}");
        }
        for result in &results {
            let name = result["mod"].as_str().unwrap_or_default();
            if result["ok"] == false {
                outln!(
                    "{name}: not switched {state}: {}",
                    result["error"].as_str().unwrap_or_default()
                );
            } else if result["changed"] == true {
                let verb = if args.dry_run {
                    "would switch"
                } else {
                    "switched"
                };
                outln!("{verb} {name} {state}");
            } else {
                outln!("{name} was already {state}");
            }
        }
    })?;
    some_failed(
        cli,
        failed,
        results.len(),
        &format!("mod(s) did not switch {state}"),
    )
}

fn mods_delete(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &ModsDeleteArgs,
) -> Result<(), String> {
    block_for_game(cli, args.dry_run)?;
    let results: Vec<serde_json::Value> = mods_named(cli, app, args.mods.all(), None)?
        .into_iter()
        .map(|(name, files)| {
            let outcome = files.and_then(|files| {
                let pak = format!(
                    "{}.pak{}",
                    files.stem,
                    if files.disabled { ".disabled" } else { "" }
                );
                let going = rivals_core::mods::files_to_delete(&files.dir, &pak)?;
                if !args.dry_run {
                    rivals_core::mods::delete_mod(&files.dir, &pak)?;
                }
                Ok((files.stem, going))
            });
            match outcome {
                Ok((stem, going)) => serde_json::json!({
                    "mod": stem,
                    "ok": true,
                    "files": going.iter().map(|path| path.display().to_string()).collect::<Vec<_>>(),
                }),
                Err(error) => serde_json::json!({ "mod": name, "ok": false, "error": error }),
            }
        })
        .collect();
    let failed = results
        .iter()
        .filter(|result| result["ok"] == false)
        .count();
    emit(cli, &serde_json::json!({ "results": results }), || {
        let verb = if args.dry_run {
            "would delete"
        } else {
            "deleted"
        };
        for result in &results {
            let name = result["mod"].as_str().unwrap_or_default();
            if result["ok"] == false {
                outln!(
                    "{name}: not deleted: {}",
                    result["error"].as_str().unwrap_or_default()
                );
                continue;
            }
            outln!("{verb} {name}");
            for file in result["files"].as_array().into_iter().flatten() {
                outln!("  {}", file.as_str().unwrap_or_default());
            }
        }
    })?;
    some_failed(cli, failed, results.len(), "mod(s) were not deleted")
}

fn mods_export_archive(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &ModsExportArchiveArgs,
) -> Result<(), String> {
    if args.out.exists() && !args.overwrite {
        return Err(format!(
            "{} is already there: pass --overwrite to write over it",
            args.out.display()
        ));
    }
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let mods = rivals_core::paths::mods_dir(&root);
    let recursive = args.scan.resolve(app);
    let count = if args.dry_run {
        rivals_core::mod_library::archived_mod_count(&mods, &args.out, recursive)?
    } else {
        rivals_core::mod_library::export_archive(&mods, &args.out, recursive)?
    };
    let report = serde_json::json!({ "out": args.out.display().to_string(), "mods": count });
    emit(cli, &report, || {
        let verb = if args.dry_run {
            "would export"
        } else {
            "exported"
        };
        outln!(
            "{verb} {count} switched-on mod(s) to {}",
            args.out.display()
        );
    })
}

#[derive(Args)]
struct ModsMoveArgs {
    #[command(flatten)]
    target: ModFileArgs,

    /// Move every package under a folder, as `/Game/Old=/Game/New`. Repeat for several.
    #[arg(long = "folder", value_name = "OLD=NEW")]
    folders: Vec<String>,

    /// Move one package, as `/Game/Old/Thing=/Game/New/Other`. Repeat for several.
    #[arg(long = "package", value_name = "OLD=NEW")]
    maps: Vec<String>,

    /// Also rename moved packages whose name starts with OLD to start with NEW, their class and
    /// default object with them, as `GAL_=SIG_`.
    #[arg(long, value_name = "OLD=NEW")]
    name_prefix: Option<String>,

    /// Keep the names of the objects named after a moved package instead of renaming them with it.
    #[arg(long)]
    keep_object_names: bool,

    /// Leave the mod's other packages naming the old paths instead of pointing them at the new
    /// ones.
    #[arg(long)]
    keep_referencers: bool,

    /// Save the moves although what they change cannot all be confirmed, as a save's
    /// `--allow-unchecked` does.
    #[arg(long)]
    allow_unchecked: bool,

    /// List every move, and write nothing.
    #[arg(long)]
    dry_run: bool,

    #[command(flatten)]
    write: WriteArgs,
}

/// `OLD=NEW`, split at its first `=`.
fn pair(text: &str, flag: &str) -> Result<(String, String), String> {
    text.split_once('=')
        .map(|(old, new)| (old.trim().to_string(), new.trim().to_string()))
        .filter(|(old, new)| !old.is_empty() && !new.is_empty())
        .ok_or_else(|| format!("{flag} takes OLD=NEW, not {text}"))
}

fn mods_move(cli: &Cli, app: &settings::AppSettings, args: &ModsMoveArgs) -> Result<(), String> {
    if !args.dry_run && !cli.force && rivals_core::game_status::should_block_for_game() {
        return Err(rivals_core::game_status::game_running_error());
    }
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let utoc = mod_utoc(cli, app, args.target.get(), true)?;
    let schema = rivals_core::mappings::resolve(cli.usmap.as_deref(), app.usmap_path.as_deref())
        .and_then(|path| rivals_core::mappings::load(&path))
        .ok();
    let options = rivals_core::mod_move::MoveOptions {
        folders: args
            .folders
            .iter()
            .map(|text| pair(text, "--folder"))
            .collect::<Result<_, _>>()?,
        maps: args
            .maps
            .iter()
            .map(|text| pair(text, "--package"))
            .collect::<Result<_, _>>()?,
        name_prefix: args
            .name_prefix
            .as_deref()
            .map(|text| pair(text, "--name-prefix"))
            .transpose()?,
        keep_object_names: args.keep_object_names,
        keep_referencers: args.keep_referencers,
        iostore: args.write.io(app, None),
    };
    let report = rivals_core::mod_move::move_in_mod(
        &root,
        &utoc,
        schema.as_deref(),
        &options,
        args.allow_unchecked,
        args.dry_run,
    )?;
    emit(cli, &report, || {
        let verb = if args.dry_run { "would move" } else { "moved" };
        outln!("{verb} {} package(s)", report.moves.len());
        for planned in &report.moves {
            outln!("  {} -> {}", planned.from, planned.to);
        }
        for (old, new) in &report.graphs {
            outln!("  event graph {old} -> {new}");
        }
        if report.strings > 0 {
            outln!(
                "  {} string(s) naming a moved folder followed",
                report.strings
            );
        }
        for warning in &report.warnings {
            outln!("warning: {warning}");
        }
        if let Some(pak) = &report.pak {
            outln!("wrote {pak}");
        }
    })
}

#[derive(Args)]
struct ModsReplaceArgs {
    #[command(flatten)]
    target: ModFileArgs,

    /// The text to find.
    #[arg(long, value_name = "TEXT")]
    find: String,

    /// What it becomes.
    #[arg(long, value_name = "TEXT")]
    with: String,

    /// Match any case, and write the replacement in the case of what it replaces: all capitals,
    /// all lower case or capitalised.
    #[arg(long)]
    ignore_case: bool,

    /// Match only where the text stands as a word of its own.
    #[arg(long)]
    word: bool,

    /// Also change strings that are paths, such as `/Game/...`, which a move changes along with
    /// what they name.
    #[arg(long)]
    paths: bool,

    /// Only the packages whose path or package name holds this, in any case. Repeatable.
    #[arg(long = "filter", value_name = "TEXT")]
    filters: Vec<String>,

    /// Save the replacements although what they change cannot all be confirmed, as a save's
    /// `--allow-unchecked` does.
    #[arg(long)]
    allow_unchecked: bool,

    /// List every change, and write nothing.
    #[arg(long)]
    dry_run: bool,

    #[command(flatten)]
    write: WriteArgs,
}

fn mods_replace(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &ModsReplaceArgs,
) -> Result<(), String> {
    if !args.dry_run && !cli.force && rivals_core::game_status::should_block_for_game() {
        return Err(rivals_core::game_status::game_running_error());
    }
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let utoc = mod_utoc(cli, app, args.target.get(), false)?;
    let schema = rivals_core::mappings::resolve(cli.usmap.as_deref(), app.usmap_path.as_deref())
        .and_then(|path| rivals_core::mappings::load(&path))
        .ok();
    let container = utoc.to_string_lossy().into_owned();
    let report = rivals_core::mod_replace::replace_in_mod(
        &rivals_core::asset_edit::AssetEditRequest {
            game_root: &root,
            container: &container,
            entry: "",
            kind: rivals_core::asset::AssetSource::Utoc,
            mod_name: "",
            changes: rivals_uasset::PackageEdits {
                allow_unchecked: args.allow_unchecked,
                ..Default::default()
            },
        },
        schema.as_deref(),
        &rivals_core::mod_replace::ReplaceOptions {
            find: args.find.clone(),
            to: args.with.clone(),
            ignore_case: args.ignore_case,
            whole_word: args.word,
            paths: args.paths,
            filters: args.filters.clone(),
        },
        &args.write.io(app, None),
        args.dry_run,
    )?;
    emit(cli, &report, || {
        let count: usize = report.packages.iter().map(|p| p.changes.len()).sum();
        let verb = if report.pak.is_some() {
            "replaced"
        } else {
            "would replace"
        };
        outln!(
            "{verb} {count} match(es) in {} package(s)",
            report
                .packages
                .iter()
                .filter(|p| !p.changes.is_empty())
                .count()
        );
        for package in &report.packages {
            outln!("  {}", package.entry);
            for change in &package.changes {
                outln!(
                    "    {}: {:?} -> {:?}",
                    change.label,
                    change.before,
                    change.after
                );
            }
            for left in &package.left {
                outln!("    left: {left}");
            }
        }
        for warning in &report.warnings {
            outln!("warning: {warning}");
        }
        if let Some(pak) = &report.pak {
            outln!("wrote {pak}");
        }
    })
}

#[derive(Args)]
struct ModsReportArgs {
    #[command(flatten)]
    target: ModFileArgs,
}

#[derive(Args)]
struct ModsTidyArgs {
    /// The mod: a path to its .pak or .utoc, or its whole name in `~mods` with or without the
    /// extension, in any case, switched on or off.
    #[arg(value_name = "MOD", required_unless_present = "all")]
    target_mod: Option<String>,

    /// Only take out the entries in the mod's pak the game does not support, `chunknames` and
    /// `patched_files`, reading and writing no package: safe on a mod someone else made.
    #[arg(long)]
    pak_only: bool,

    /// With `--pak-only`, every IoStore mod in `~mods`, switched on or off.
    #[arg(long, requires = "pak_only", conflicts_with = "target_mod")]
    all: bool,

    /// With `--all`, how far into `~mods` to look.
    #[command(flatten)]
    scan: RecursiveArgs,

    /// List what would go, and write nothing.
    #[arg(long)]
    dry_run: bool,

    #[command(flatten)]
    write: WriteArgs,
}

#[derive(Args)]
struct ModsCheckArgs {
    #[command(flatten)]
    target: ModFileArgs,

    /// Fail when any package names this text, ignoring case: in its scripts, its stored values or
    /// its table of names. Repeat for several.
    #[arg(long = "forbid", value_name = "TEXT")]
    forbid: Vec<String>,

    /// Leave out the audit, which reads every package and writes each script back from its text.
    #[arg(long)]
    skip_audit: bool,
}

#[derive(Args)]
#[command(group(clap::ArgGroup::new("named").required(true).multiple(true).args(["target_mods", "all"])))]
struct ModsOverridesArgs {
    #[command(flatten)]
    mods: ModsArgs,
    /// Every switched-on IoStore mod in `~mods`.
    #[arg(long, conflicts_with = "target_mods")]
    all: bool,
    /// Also list each value an override holds differently from the game's copy: the fields, row
    /// cells and elements it changes, and the rows it adds.
    #[arg(long)]
    values: bool,
}

#[derive(Args)]
struct ModsDiffArgs {
    /// The first mod, A: a path to one of its files, or its whole name in `~mods` with or without
    /// the extension and in any case, switched on or off.
    #[arg(value_name = "A")]
    a: String,
    /// The second mod, B, named the same way.
    #[arg(value_name = "B")]
    b: String,
    /// Only say which packages differ and in which parts, reading none of them, so neither the
    /// game's containers nor mappings are needed.
    #[arg(long)]
    brief: bool,
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
        Command::Tweaks(TweaksCmd::Presets) => tweaks_presets(cli, &app),
        Command::Ini(IniCmd::List(a)) => ini_list(cli, &app, a),
        Command::Ini(IniCmd::Get(a)) => ini_get(cli, &app, a),
        Command::Ini(IniCmd::Set(a)) => ini_set(cli, &app, a),
        Command::Ini(IniCmd::Unset(a)) => ini_unset(cli, &app, a),
        Command::Mods(ModsCmd::List(a)) => mods_list(cli, &app, a),
        Command::Mods(ModsCmd::Conflicts(a)) => mods_conflicts(cli, &app, a),
        Command::Mods(ModsCmd::Extract(a)) => mods_extract(cli, &app, a),
        Command::Mods(ModsCmd::Files(a)) => mods_files(cli, &app, a),
        Command::Mods(ModsCmd::Report(a)) => mods_report(cli, &app, a),
        Command::Mods(ModsCmd::Check(a)) => mods_check(cli, &app, a),
        Command::Mods(ModsCmd::Tidy(a)) => mods_tidy(cli, &app, a),
        Command::Mods(ModsCmd::Overrides(a)) => mods_overrides(cli, &app, a),
        Command::Mods(ModsCmd::Diff(a)) => mods_diff(cli, &app, a),
        Command::Mods(ModsCmd::Search(a)) => mods_search(cli, &app, a),
        Command::Mods(ModsCmd::Replace(a)) => mods_replace(cli, &app, a),
        Command::Mods(ModsCmd::Move(a)) => mods_move(cli, &app, a),
        Command::Mods(ModsCmd::Copy(a)) => mods_copy(cli, &app, a),
        Command::Mods(ModsCmd::Rename(a)) => mods_rename(cli, &app, a),
        Command::Mods(ModsCmd::Remove(a)) => mods_remove(cli, &app, a),
        Command::Mods(ModsCmd::Enable(a)) => mods_set_enabled(cli, &app, a, true),
        Command::Mods(ModsCmd::Disable(a)) => mods_set_enabled(cli, &app, a, false),
        Command::Mods(ModsCmd::Delete(a)) => mods_delete(cli, &app, a),
        Command::Mods(ModsCmd::ExportArchive(a)) => mods_export_archive(cli, &app, a),
        Command::Mods(ModsCmd::Pack(a)) => mods_pack(cli, &app, a),
        Command::Mods(ModsCmd::Convert(a)) => mods_convert(cli, &app, a),
        Command::Mods(ModsCmd::Install(a)) => mods_install(cli, &app, a),
        Command::Mods(ModsCmd::Deps(a)) => mods_deps(cli, &app, a),
        Command::Asset(AssetCmd::List(a)) => asset_list(cli, &app, a),
        Command::Asset(AssetCmd::Info(a)) => asset_info(cli, &app, a),
        Command::Asset(AssetCmd::Dump(a)) => asset_dump(cli, &app, a),
        Command::Asset(AssetCmd::Table(a)) => asset_table(cli, &app, a),
        Command::Asset(AssetCmd::Trace(a)) => asset_trace(cli, &app, a),
        Command::Asset(AssetCmd::Hex(a)) => asset_hex(cli, &app, a),
        Command::Asset(AssetCmd::Fields(a)) => asset_fields(cli, &app, a),
        Command::Asset(AssetCmd::Script(a)) => asset_script(cli, &app, a),
        Command::Asset(AssetCmd::Vm(a)) => asset_vm(cli, &app, a),
        Command::Asset(AssetCmd::EditVm(a)) => asset_edit_vm(cli, &app, a),
        Command::Asset(AssetCmd::EditScript(a)) => asset_edit_script(cli, &app, a),
        Command::Asset(AssetCmd::WidenScript(a)) => asset_widen_script(cli, &app, a),
        Command::Asset(AssetCmd::AssembleScript(a)) => asset_assemble_script(cli, &app, a),
        Command::Asset(AssetCmd::AddVariable(a)) => asset_add_variable(cli, &app, a),
        Command::Asset(AssetCmd::AddField(a)) => asset_add_field(cli, &app, a),
        Command::Asset(AssetCmd::Set(a)) => asset_set(cli, &app, a),
        Command::Asset(AssetCmd::Sweep(a)) => asset_sweep(cli, &app, a),
        Command::Asset(AssetCmd::EditImports(a)) => asset_edit_imports(cli, &app, a),
        Command::Asset(AssetCmd::RemoveExport(a)) => asset_remove_export(cli, &app, a),
        Command::Asset(AssetCmd::Revert(a)) => asset_revert(cli, &app, a),
        Command::Asset(AssetCmd::ResetExport(a)) => asset_reset_export(cli, &app, a),
        Command::Asset(AssetCmd::SaveAs(a)) => asset_save_as(cli, &app, a),
        Command::Asset(AssetCmd::AddExport(a)) => asset_add_export(cli, &app, a),
        Command::Asset(AssetCmd::AddComponent(a)) => asset_add_component(cli, &app, a),
        Command::Asset(AssetCmd::RemoveComponent(a)) => asset_remove_component(cli, &app, a),
        Command::Asset(AssetCmd::RenamePackage(a)) => asset_rename_package(cli, &app, a),
        Command::Asset(AssetCmd::EditRow(a)) => asset_edit_row(cli, &app, a),
        Command::Asset(AssetCmd::EditStrings(a)) => asset_edit_strings(cli, &app, a),
        Command::Asset(AssetCmd::EditKeys(a)) => asset_edit_keys(cli, &app, a),
        Command::Asset(AssetCmd::Payload(a)) => asset_payload(cli, &app, a),
        Command::Asset(AssetCmd::DuplicateExport(a)) => asset_duplicate_export(cli, &app, a),
        Command::Asset(AssetCmd::Bulk(a)) => asset_bulk(cli, &app, a),
        Command::Asset(AssetCmd::Importers(a)) => asset_importers(cli, &app, a),
        Command::Asset(AssetCmd::Search(a)) => asset_search(cli, &app, a),
        Command::Asset(AssetCmd::Ability(a)) => asset_ability(cli, &app, a),
        Command::Asset(AssetCmd::EditExport(a)) => asset_edit_export(cli, &app, a),
        Command::Asset(AssetCmd::Imports(a)) => asset_imports_table(cli, &app, a),
        Command::Asset(AssetCmd::Members(a)) => asset_members(cli, &app, a),
        Command::Asset(AssetCmd::Texture(a)) => asset_texture(cli, &app, a),
        Command::Asset(AssetCmd::RenameMember(a)) => asset_rename_member(cli, &app, a),
        Command::Asset(AssetCmd::RemoveMember(a)) => asset_remove_member(cli, &app, a),
        Command::Asset(AssetCmd::Names(a)) => asset_names(cli, &app, a),
        Command::Asset(AssetCmd::Apply(a)) => asset_apply(cli, &app, a),
        Command::Asset(AssetCmd::Diff(a)) => asset_diff(cli, &app, a),
        Command::Asset(AssetCmd::Deps(a)) => asset_deps(cli, &app, a),
        Command::Asset(AssetCmd::CopyExport(a)) => asset_copy_export(cli, &app, a),
        Command::Asset(AssetCmd::Audit(a)) => asset_audit(cli, &app, a),
        Command::Asset(AssetCmd::Diagnose(a)) => asset_diagnose(cli, &app, a),
        Command::Asset(AssetCmd::CheckStructs(a)) => asset_check_structs(cli, &app, a),
        Command::Logs(LogsCmd::List(a)) => logs_list(cli, &app, a),
        Command::Logs(LogsCmd::Decode(a)) => logs_decode(cli, &app, a),
        Command::Logs(LogsCmd::Analyze(a)) => logs_analyze(cli, &app, a),
        Command::Logs(LogsCmd::Key) => logs_key(cli, &app),
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
    let path = ini_pak(cli, app, &pak.pak)?;
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

/// What a preset asks for, minus what cannot be asked for, and a note of what was dropped.
///
/// The desktop app resolves a preset against the catalogue and quietly ignores anything it cannot
/// place, so a preset saved by a different build still applies everything this one understands.
/// This has to match, or the same preset would work there and fail here. Two things get dropped:
///
/// - an id this build has no tweak for, which is a preset written against another catalogue;
/// - a remove-only tweak held as off, which asks to put back lines it can only delete;
/// - an engine-section tweak when the pak ships no engine INI, which has nowhere to be written.
fn preset_settings(
    app: &settings::AppSettings,
    name: &str,
    has_engine_ini: bool,
) -> Result<(Vec<TweakSetting>, Vec<String>), String> {
    let profile = app
        .tweak_profiles
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| {
            let known: Vec<&str> = app.tweak_profiles.iter().map(|p| p.name.as_str()).collect();
            if known.is_empty() {
                format!("no preset named '{name}'; the desktop app has none saved")
            } else {
                format!(
                    "no preset named '{name}'; saved presets: {}",
                    known.join(", ")
                )
            }
        })?;

    let catalogue = rivals_core::tweaks::catalogue::tweak_catalogue();
    let mut kept = Vec::new();
    let mut dropped = Vec::new();
    for setting in &profile.settings {
        let Some(def) = catalogue.iter().find(|d| d.id == setting.id) else {
            dropped.push(format!("{} (no such tweak in this build)", setting.id));
            continue;
        };
        let remove_only = matches!(
            def.kind,
            rivals_core::tweaks::catalogue::TweakKind::RemoveLines {
                remove_only: true,
                ..
            }
        );
        if !setting.enabled && remove_only {
            dropped.push(format!(
                "{} (only removes lines, cannot be turned off)",
                def.id
            ));
            continue;
        }
        if !has_engine_ini && pak_tweaks::needs_engine_ini(def) {
            dropped.push(format!(
                "{} (needs an Engine.ini this pak does not ship)",
                def.id
            ));
            continue;
        }
        kept.push(setting.clone());
    }
    Ok((kept, dropped))
}

fn tweaks_presets(cli: &Cli, app: &settings::AppSettings) -> Result<(), String> {
    let rows: Vec<serde_json::Value> = app
        .tweak_profiles
        .iter()
        .map(|p| {
            let on = p.settings.iter().filter(|s| s.enabled).count();
            serde_json::json!({ "name": p.name, "settings": p.settings.len(), "on": on })
        })
        .collect();
    emit(cli, &rows, || {
        if app.tweak_profiles.is_empty() {
            outln!("no presets saved");
            return;
        }
        for p in &app.tweak_profiles {
            let on = p.settings.iter().filter(|s| s.enabled).count();
            outln!("{:<24} {on} on of {}", p.name, p.settings.len());
        }
    })
}

fn tweaks_apply(cli: &Cli, app: &settings::AppSettings, args: &ApplyArgs) -> Result<(), String> {
    let path = ini_pak(cli, app, &args.pak.pak)?;

    let mut settings: Vec<TweakSetting> = Vec::new();
    let mut dropped: Vec<String> = Vec::new();
    if let Some(name) = &args.preset {
        // What the pak can hold decides what a preset can ask for, so this reads the pak before
        // resolving rather than letting the write land nowhere.
        let has_engine_ini = pak_tweaks::inspect_single_pak(&path)?.is_some_and(|info| {
            info.has_engine_ini || info.has_base_engine || info.has_windows_engine
        });
        let (kept, skipped) = preset_settings(app, name, has_engine_ini)?;
        settings.extend(kept);
        dropped = skipped;
    }
    let mut push = |id: &str, enabled: bool, value: Option<String>| {
        // A later entry for the same tweak replaces the preset's, so the flags act as overrides.
        settings.retain(|s| s.id != id);
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
        return Err("nothing to do: pass --preset, --on, --off, or --set".to_string());
    }

    // Said out loud rather than swallowed: a preset that silently shrinks is how a tweak goes
    // missing without anyone noticing.
    for entry in &dropped {
        eprintln!("skipped {entry}");
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
    let path = ini_pak(cli, app, &pak.pak)?;
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
    let path = ini_pak(cli, app, &args.pak.pak)?;

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
    let path = ini_pak(cli, app, &args.pak.pak)?;

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
    let path = ini_pak(cli, app, &args.pak.pak)?;
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

// ---- mods ----

/// A size the way a file browser shows one.
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

fn mods_list(cli: &Cli, app: &settings::AppSettings, args: &ModsListArgs) -> Result<(), String> {
    let game_root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let recursive = args.scan.resolve(app);

    if !args.ini && !args.all_ini {
        let library = rivals_core::mod_library::list_mods(&game_root, recursive);
        return emit(cli, &library, || {
            if !library.mods_folder_exists {
                outln!("no ~mods folder at {}", library.mods_folder_path);
                return;
            }
            outln!(
                "{:>4}  {:<3}  {:<7}  {:>8}  {:>9}  mod",
                "rank",
                "on",
                "kind",
                "priority",
                "size"
            );
            for entry in &library.mods {
                let rank = entry
                    .load_rank
                    .map_or_else(|| "-".to_string(), |rank| rank.to_string());
                let kind = match entry.kind {
                    rivals_core::mod_library::ModKind::Pak => "pak",
                    rivals_core::mod_library::ModKind::IoStore => "iostore",
                };
                let priority = entry
                    .priority
                    .map_or_else(|| "none".to_string(), |p| p.to_string());
                let mut notes = Vec::new();
                if entry.obfuscated {
                    notes.push("obfuscated");
                }
                if entry.stale {
                    notes.push("stale copy of a switched-on mod");
                }
                let notes = if notes.is_empty() {
                    String::new()
                } else {
                    format!("  ({})", notes.join(", "))
                };
                outln!(
                    "{rank:>4}  {:<3}  {kind:<7}  {priority:>8}  {:>9}  {}{notes}",
                    if entry.enabled { "on" } else { "off" },
                    human_size(entry.size_bytes),
                    entry.display_name
                );
            }
            let on = library.mods.iter().filter(|entry| entry.enabled).count();
            outln!(
                "\n{} mod(s), {on} switched on; rank 1 is the one whose copy of a shared file loads",
                library.mods.len()
            );
        });
    }

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

#[derive(Serialize)]
struct ExtractResult {
    out: String,
    extracted: Vec<String>,
    /// The files of the mod's pak, written beside the packages.
    loose: Vec<String>,
    /// Shader archives written from the container's libraries, each with its asset info.
    shader_libraries: Vec<String>,
    errors: Vec<String>,
    /// Imports each package still could not name, keyed by package path.
    unresolved: BTreeMap<String, Vec<String>>,
}

/// Whether a path passes any of the filters, each matched the way every `--filter` is.
fn passes(path: &str, filters: &[String]) -> bool {
    filters.is_empty()
        || filters
            .iter()
            .any(|filter| rivals_core::asset::PathFilter::new(Some(filter)).matches(path))
}

fn mods_extract(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &ModsExtractArgs,
) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let files = mod_files(cli, app, args.from.get())?;
    let pak = files.part("pak");
    // The pak's own files, which a container's extraction has nothing to say about. What an
    // older packer put beside a container is no file of the mod's.
    let loose = |skip_listing: bool| -> Result<Vec<String>, String> {
        if !pak.is_file() {
            return Ok(Vec::new());
        }
        rivals_core::pak::plain::extract(&pak, &args.out, &|name| {
            !(skip_listing && rivals_core::pak::plain::is_unsupported_entry(name))
                && passes(name, &args.filter)
        })
    };
    if !files.is_iostore() {
        let extracted = loose(false)?;
        let report = serde_json::json!({
            "out": args.out.display().to_string(),
            "loose": extracted,
        });
        return emit(cli, &report, || {
            outln!(
                "{} file(s) of the plain pak {} written under {}",
                extracted.len(),
                files.stem,
                args.out.display()
            );
        });
    }
    let utoc = files.part("utoc");
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let quiet = cli.json;
    let progress = |done: usize, total: usize| {
        if !quiet {
            eprint!("\rconverted {done}/{total}");
        }
    };
    let result = rivals_core::pak::extract::extract_legacy(
        &utoc,
        &root,
        &args.out,
        &args.filter,
        &cancel,
        &progress,
    )?;
    if !quiet {
        eprintln!();
    }

    let mut unresolved = BTreeMap::new();
    for path in result.extracted.iter().filter(|p| p.ends_with(".uasset")) {
        let bundle = rivals_core::asset::load_from_disk(&args.out.join(path))?;
        let names = rivals_uasset::unresolved_imports(&rivals_uasset::AssetBundle {
            asset: &bundle.asset_file_buffer,
            exports: &bundle.exports_file_buffer,
        })?;
        if !names.is_empty() {
            unresolved.insert(path.clone(), names);
        }
    }

    let report = ExtractResult {
        out: args.out.display().to_string(),
        extracted: result.extracted,
        loose: loose(true)?,
        shader_libraries: result.shader_libraries,
        errors: result.errors,
        unresolved,
    };
    emit(cli, &report, || {
        outln!(
            "{} package(s) written under {}",
            report.extracted.len(),
            report.out
        );
        for library in &report.shader_libraries {
            outln!("shader library {library}, with its asset info");
        }
        if !report.loose.is_empty() {
            outln!("{} file(s) from its pak beside them", report.loose.len());
        }
        for err in &report.errors {
            eprintln!("  failed: {err}");
        }
        if report.unresolved.is_empty() {
            outln!("every import resolved");
        }
        for (package, names) in &report.unresolved {
            outln!("{package}: {} unresolved import(s)", names.len());
            for name in names {
                outln!("    {name}");
            }
        }
    })
}

fn mods_conflicts(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &ModsConflictsArgs,
) -> Result<(), String> {
    let game_root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let report = rivals_core::mod_library::check_conflicts(
        &game_root,
        args.scan.resolve(app),
        &rivals_core::mod_library::DiskLister,
    )?;
    emit(cli, &report, || {
        if report.groups.is_empty() {
            outln!(
                "no file is carried by more than one of the {} switched-on mod(s)",
                report.mods_scanned
            );
            return;
        }
        for group in &report.groups {
            outln!(
                "{}: {} file(s) shared with {}",
                group.display_name,
                group.conflicting_asset_count,
                group.conflicts_with.join(", ")
            );
        }
        if args.assets {
            outln!();
            for conflict in &report.asset_conflicts {
                let game = if conflict.overrides_game {
                    "  (the game's own file)"
                } else {
                    ""
                };
                outln!("{}{game}", conflict.asset);
                for (at, holder) in conflict.mods.iter().enumerate() {
                    let loads = if at == 0 { "  loads" } else { "" };
                    outln!("    {holder}{loads}");
                }
            }
        } else {
            outln!("\n`--assets` lists each shared file and whose copy loads");
        }
        outln!(
            "{} shared file(s) across {} switched-on mod(s)",
            report.asset_conflicts.len(),
            report.mods_scanned
        );
    })
}

fn mods_files(cli: &Cli, app: &settings::AppSettings, args: &ModsFilesArgs) -> Result<(), String> {
    let files = mod_files(cli, app, args.from.get())?;
    let keep = |path: &String| passes(path, &args.filter);
    let container: Vec<String> = if files.is_iostore() {
        rivals_core::pak::containers::list_utoc_paths(&files.part("utoc"))?
            .into_iter()
            .filter(keep)
            .collect()
    } else {
        Vec::new()
    };
    let pak = files.part("pak");
    let mut loose: Vec<String> = if pak.is_file() {
        rivals_core::pak::plain::list_files(&pak)?
            .iter()
            .map(|name| rivals_core::pak::profile::strip_mount_prefix(name))
            .filter(keep)
            .collect()
    } else {
        Vec::new()
    };
    loose.sort();
    let report = serde_json::json!({
        "mod": files.stem,
        "kind": if files.is_iostore() { "iostore" } else { "pak" },
        "enabled": !files.disabled,
        "container": container,
        "pak": loose,
    });
    emit(cli, &report, || {
        let state = if files.disabled { ", switched off" } else { "" };
        if files.is_iostore() {
            outln!("{}  (IoStore{state})", files.stem);
            outln!("container: {} file(s)", container.len());
            for path in &container {
                outln!("  {path}");
            }
            outln!("pak: {} file(s)", loose.len());
        } else {
            outln!(
                "{}  (plain pak{state}): {} file(s)",
                files.stem,
                loose.len()
            );
        }
        for path in &loose {
            outln!("  {path}");
        }
    })
}

fn mods_search(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &ModsSearchArgs,
) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let utoc = mod_utoc(cli, app, args.target.get(), false)?;
    let schema = rivals_core::mappings::resolve(cli.usmap.as_deref(), app.usmap_path.as_deref())
        .and_then(|path| rivals_core::mappings::load(&path))
        .ok();
    let result = rivals_core::mod_search::mod_search_with(
        &root,
        &utoc,
        schema.as_deref(),
        &args.find,
        rivals_core::mod_search::SearchOptions { names: args.names },
    )?;
    emit(cli, &result, || {
        let mut package = "";
        for hit in &result.hits {
            if hit.package != package {
                package = &hit.package;
                outln!("{package}");
            }
            let at = hit
                .offset
                .map_or_else(String::new, |o| format!(" 0x{o:04X}"));
            outln!("  {}{at}  {}", hit.export, hit.line);
        }
        outln!("\n{} hit(s)", result.hits.len());
        for (path, reason) in &result.unreadable {
            eprintln!("not searched: {path}: {reason}");
        }
    })
}

fn mods_report(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &ModsReportArgs,
) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let utoc = mod_utoc(cli, app, args.target.get(), false)?;
    let schema = rivals_core::mappings::resolve(cli.usmap.as_deref(), app.usmap_path.as_deref())
        .and_then(|path| rivals_core::mappings::load(&path))
        .ok();
    let report = rivals_core::mod_report::mod_report(&root, &utoc, schema.as_deref())?;
    let shown = rivals_core::mod_ops::ModFiles::of(&utoc).map_or_else(
        |_| report.container.clone(),
        |files| resolve::shown_mod(&files, &rivals_core::paths::mods_dir(&root)),
    );
    // Each override against the game's current copy, which takes a store of the game alone.
    let overrides = if report.packages.iter().any(|package| package.overrides_game) {
        rivals_core::override_check::check_overrides(
            &root,
            std::slice::from_ref(&utoc),
            schema.as_deref(),
            false,
        )?
        .pop()
        .map(|checked| checked.packages)
        .unwrap_or_default()
    } else {
        Vec::new()
    };
    #[derive(Serialize)]
    struct Report<'a> {
        #[serde(flatten)]
        report: &'a rivals_core::mod_report::ModReport,
        overrides: &'a [rivals_core::override_check::OverrideCheck],
    }
    let out = Report {
        report: &report,
        overrides: &overrides,
    };
    emit(cli, &out, || {
        let priority = report
            .patch_priority
            .map_or_else(|| "none".to_string(), |p| p.to_string());
        outln!("{}  (patch priority {priority})", report.container);
        outln!("\npackages:");
        for package in &report.packages {
            let role = if package.overrides_game {
                "override"
            } else {
                "new"
            };
            outln!("  {role:<8} {:<18} {}", package.kind, package.path);
            if let Some(error) = &package.error {
                outln!("           not read: {error}");
            }
            if let Some(check) = overrides.iter().find(|check| check.package == package.path) {
                outln!("           {}", check.verdict());
            }
            if !package.unused_imports.is_empty() || !package.unused_names.is_empty() {
                outln!(
                    "           unused: {} import(s), {} name(s) (asset imports --unused, asset names --unused)",
                    package.unused_imports.len(),
                    package.unused_names.len()
                );
            }
            for line in &package.unwaited {
                outln!("           no edge: {line}");
            }
            for line in &package.stray_edges {
                outln!("           stray edge: {line}");
            }
        }
        if overrides.iter().any(|check| !check.findings.is_empty()) {
            outln!("\n`mods overrides {shown}` lists what differs.");
        }
        if !report.composites.is_empty() {
            outln!("\ncombined tables:");
        }
        for check in &report.composites {
            outln!(
                "  {}: stores {} of the {} row(s) its parents merge to",
                check.table,
                check.stored,
                check.merged
            );
            if let Some(reason) = &check.unreadable {
                outln!("      parents not read: {reason}");
            }
            for warning in check.warnings() {
                outln!("      warning: {warning}");
            }
        }
        for (title, section) in [
            ("runtime natives", &report.runtime_natives),
            ("python classes", &report.python_classes),
            ("files", &report.files),
            ("save slots", &report.save_slots),
            ("urls", &report.urls),
        ] {
            if section.is_empty() {
                continue;
            }
            outln!("\n{title}:");
            for (package, values) in section {
                outln!("  {package}");
                for value in values {
                    outln!("      {value}");
                }
            }
        }
    })
}

/// One step of `mods check`: how it came out, and what it found.
#[derive(Serialize)]
struct CheckStep {
    name: &'static str,
    status: &'static str,
    details: Vec<String>,
}

impl CheckStep {
    /// `fail` when anything failing was found, `warn` when only warnings were, `ok` otherwise.
    fn of(name: &'static str, failing: Vec<String>, warnings: Vec<String>, ok: String) -> Self {
        let (status, details) = match (failing.is_empty(), warnings.is_empty()) {
            (false, _) => ("fail", [failing, warnings].concat()),
            (true, false) => ("warn", warnings),
            (true, true) => ("ok", vec![ok]),
        };
        Self {
            name,
            status,
            details,
        }
    }
}

/// At most `shown` of `lines`, then how many more there were.
fn capped(mut lines: Vec<String>, shown: usize) -> Vec<String> {
    if lines.len() > shown {
        let more = lines.len() - shown;
        lines.truncate(shown);
        lines.push(format!("and {more} more"));
    }
    lines
}

fn mods_tidy(cli: &Cli, app: &settings::AppSettings, args: &ModsTidyArgs) -> Result<(), String> {
    block_for_game(cli, args.dry_run)?;
    if args.pak_only {
        return mods_tidy_paks(cli, app, args);
    }
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let target = args.target_mod.as_deref().ok_or("name the mod to tidy")?;
    let utoc = mod_utoc(cli, app, target, false)?;
    let schema = rivals_core::mappings::resolve(cli.usmap.as_deref(), app.usmap_path.as_deref())
        .and_then(|path| rivals_core::mappings::load(&path))
        .ok();
    let report = rivals_core::mod_tidy::tidy_mod(
        &root,
        &utoc,
        schema.as_deref(),
        &args.write.io(app, None),
        args.dry_run,
    )?;
    emit(cli, &report, || {
        if report.packages.is_empty() && report.pak_entries.is_empty() {
            outln!(
                "nothing to tidy: no package holds a stray edge, an unused local, an unused import or an unused name, and the pak holds nothing the game does not support"
            );
        }
        let verb = if report.pak.is_some() {
            "tidied"
        } else {
            "would tidy"
        };
        for package in &report.packages {
            outln!("{verb} {}", package.entry);
            for edge in &package.edges {
                outln!("  edge: {edge}");
            }
            for local in &package.locals {
                outln!("  local: {local}");
            }
            for import in &package.imports {
                outln!("  import: {import}");
            }
            if !package.names.is_empty() {
                outln!(
                    "  {} name(s): {}",
                    package.names.len(),
                    package.names.join(", ")
                );
            }
        }
        if !report.pak_entries.is_empty() {
            outln!(
                "{verb} the pak beside the container: {}, which the game does not support there",
                report.pak_entries.join(", ")
            );
        }
        for warning in &report.warnings {
            outln!("warning: {warning}");
        }
        if let Some(pak) = &report.pak {
            outln!("wrote {pak}");
        }
    })
}

/// `mods tidy --pak-only`: the named mod's pak, or with `--all` every installed IoStore mod's, rid
/// of what the game does not support there, each reported and failing on its own.
fn mods_tidy_paks(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &ModsTidyArgs,
) -> Result<(), String> {
    let options = args.write.pak(app);
    let paks = |name: &str| name.ends_with(".pak") || name.ends_with(".pak.disabled");
    let names: Vec<String> = args.target_mod.iter().cloned().collect();
    let targets = mods_named(
        cli,
        app,
        &names,
        args.all
            .then_some((args.scan.resolve(app), &paks as &dyn Fn(&str) -> bool)),
    )?;
    let mut results = Vec::new();
    for (name, files) in targets {
        let files = match files {
            // `--all` passes over the plain pak mods, which hold no container to sit beside.
            Ok(files) if args.all && !files.is_iostore() => continue,
            Ok(files) if !files.is_iostore() => {
                Err(format!("{} is not an IoStore mod", files.stem))
            }
            other => other,
        };
        results.push(
            match files.and_then(|files| {
                let done =
                    rivals_core::mod_tidy::tidy_pak(&files.part("utoc"), &options, args.dry_run)?;
                Ok((files.stem, done))
            }) {
                Ok((stem, done)) => serde_json::json!({
                    "mod": stem,
                    "ok": true,
                    "pak": done.pak,
                    "entries": done.entries,
                    "written": done.written,
                }),
                Err(error) => serde_json::json!({ "mod": name, "ok": false, "error": error }),
            },
        );
    }
    let failed = results
        .iter()
        .filter(|result| result["ok"] == false)
        .count();
    emit(cli, &serde_json::json!({ "results": results }), || {
        let mut clean = 0;
        for result in &results {
            let name = result["mod"].as_str().unwrap_or_default();
            if result["ok"] == false {
                outln!(
                    "{name}: not tidied: {}",
                    result["error"].as_str().unwrap_or_default()
                );
                continue;
            }
            let entries: Vec<&str> = result["entries"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .collect();
            if entries.is_empty() {
                clean += 1;
                if !args.all {
                    outln!("nothing to tidy: {name}'s pak holds nothing the game does not support");
                }
                continue;
            }
            let verb = if args.dry_run { "would take" } else { "took" };
            outln!(
                "{verb} {} out of {}",
                entries.join(" and "),
                result["pak"].as_str().unwrap_or_default()
            );
        }
        if args.all {
            outln!(
                "{} mod(s) looked at, {clean} holding nothing the game does not support",
                results.len()
            );
        }
    })?;
    some_failed(cli, failed, results.len(), "mod(s) could not be tidied")
}

fn mods_check(cli: &Cli, app: &settings::AppSettings, args: &ModsCheckArgs) -> Result<(), String> {
    const SHOWN: usize = 20;
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let files = mod_files(cli, app, args.target.get())?;
    if !files.is_iostore() {
        return Err(format!("{} is not an IoStore mod", files.stem));
    }
    let utoc = files.part("utoc");
    let container = utoc.to_string_lossy().into_owned();
    let schema = rivals_core::mappings::resolve(cli.usmap.as_deref(), app.usmap_path.as_deref())
        .and_then(|path| rivals_core::mappings::load(&path))
        .ok();
    let mut steps = Vec::new();

    if !args.skip_audit {
        let audit = asset::audit(
            &root,
            Some(&container),
            None,
            None,
            cli.usmap.as_deref(),
            app.usmap_path.as_deref(),
            false,
            false,
            true,
            |_, _| {},
        )?;
        let mut failing: Vec<String> = [
            (audit.exports_failed, "export(s) failed to read"),
            (audit.exports_partial, "export(s) read only in part"),
            (audit.packages_header_broken, "package header(s) broken"),
            (
                audit.headers_differing,
                "package header(s) write back differently",
            ),
            (audit.scripts_stopped, "script(s) stopped decoding"),
            (audit.undecoded_payloads, "payload(s) left undecoded"),
            (audit.failed_class_recovery, "class(es) not recovered"),
        ]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, what)| format!("{count} {what}"))
        .collect();
        let text_failed = |name: &str| {
            name.starts_with("text check failed") || name.starts_with("text check save failed")
        };
        for count in audit.relocation.iter().filter(|c| text_failed(&c.name)) {
            failing.push(format!("{} ({})", count.name, count.count));
            if let Some(seen) = audit
                .relocation_examples
                .iter()
                .find(|examples| examples.name == count.name)
            {
                failing.extend(seen.examples.iter().map(|example| format!("  {example}")));
            }
        }
        let back = audit
            .relocation
            .iter()
            .find(|c| c.name.starts_with("text check: functions"))
            .map_or(0, |c| c.count);
        steps.push(CheckStep::of(
            "audit",
            failing,
            Vec::new(),
            format!(
                "{} package(s) read, {} of {} export(s) whole, {back} function(s) back byte for byte from their text",
                audit.packages_scanned,
                audit.exports_complete + audit.exports_payload,
                audit.exports_total
            ),
        ));
    }

    let deps = rivals_core::mod_ops::mod_deps(&root, &files)?;
    let missing: Vec<String> = deps
        .packages
        .iter()
        .flat_map(|package| {
            package.missing.iter().map(move |gone| {
                format!(
                    "{} imports {gone}, which neither the mod nor the game has",
                    package.package
                )
            })
        })
        .collect();
    let unreached: Vec<String> = deps
        .unreached
        .iter()
        .map(|package| format!("nothing the game loads leads to {package}"))
        .collect();
    steps.push(CheckStep::of(
        "imports",
        capped(missing, SHOWN),
        capped(unreached, SHOWN),
        format!(
            "every import of its {} package(s) is there",
            deps.packages.len()
        ),
    ));

    let report = rivals_core::mod_report::mod_report(&root, &utoc, schema.as_deref())?;
    let overridden = report.packages.iter().filter(|p| p.overrides_game).count();
    let overrides = if overridden > 0 {
        rivals_core::override_check::check_overrides(
            &root,
            std::slice::from_ref(&utoc),
            schema.as_deref(),
            false,
        )?
        .pop()
        .map(|checked| checked.packages)
        .unwrap_or_default()
    } else {
        Vec::new()
    };
    let stale: Vec<String> = overrides
        .iter()
        .filter(|check| {
            check.unreadable.is_some()
                || check
                    .findings
                    .iter()
                    .any(|f| f.kind != rivals_core::override_check::FindingKind::Import)
        })
        .map(|check| format!("{}: {}", check.package, check.verdict()))
        .collect();
    steps.push(CheckStep::of(
        "overrides",
        Vec::new(),
        stale,
        format!("{overridden} override(s) match the game's current layout"),
    ));

    let unread: Vec<String> = report
        .packages
        .iter()
        .filter_map(|p| {
            p.error
                .as_ref()
                .map(|e| format!("{}: not read: {e}", p.path))
        })
        .collect();
    let unused: Vec<String> = report
        .packages
        .iter()
        .filter(|p| !p.unused_imports.is_empty() || !p.unused_names.is_empty())
        .map(|p| {
            format!(
                "{}: {} import(s) and {} name(s) unused",
                p.path,
                p.unused_imports.len(),
                p.unused_names.len()
            )
        })
        .collect();
    let shown = resolve::shown_mod(&files, &rivals_core::paths::mods_dir(&root));
    let tidy = format!("`mods tidy {shown}` drops these");
    let fixable = |mut lines: Vec<String>| {
        if !lines.is_empty() {
            lines.push(tidy.clone());
        }
        lines
    };
    let unused = fixable(capped(unused, SHOWN));
    steps.push(CheckStep::of(
        "unused",
        Vec::new(),
        [unread, unused].concat(),
        "no package holds an import or a name nothing uses".into(),
    ));

    // Inside the Blueprints: locals, which only their function can name, and class members, which
    // something outside the mod may still reach by name.
    let (idle, unseen) =
        rivals_core::members::mod_unused_members(&root, &container, schema.as_deref())?;
    let locals: Vec<String> = idle
        .iter()
        .flat_map(|package| {
            package
                .locals
                .iter()
                .map(move |local| format!("{}: local {local} nothing names", package.entry))
        })
        .collect();
    let mut members: Vec<String> = capped(
        idle.iter()
            .flat_map(|package| {
                package.members.iter().map(move |member| {
                    format!("{}: {member} nothing in the mod uses", package.entry)
                })
            })
            .collect(),
        SHOWN,
    );
    if !members.is_empty() {
        members.push(
            "the game's code or another mod may still reach these by name: check before `asset remove-member` takes them out"
                .into(),
        );
    }
    let unseen: Vec<String> = unseen
        .iter()
        .map(|entry| format!("{entry}: not read, so a use in it is not seen"))
        .collect();
    steps.push(CheckStep::of(
        "unused members",
        Vec::new(),
        [unseen, fixable(capped(locals, SHOWN)), members].concat(),
        "no Blueprint holds a member or a local nothing in the mod uses".into(),
    ));

    let unwaited: Vec<String> = report
        .packages
        .iter()
        .flat_map(|p| p.unwaited.iter().cloned())
        .collect();
    let stray: Vec<String> = report
        .packages
        .iter()
        .flat_map(|p| p.stray_edges.iter().cloned())
        .collect();
    steps.push(CheckStep::of(
        "load order",
        capped(unwaited, SHOWN),
        fixable(capped(stray, SHOWN)),
        "every export waits for the objects it names, and for no other export".into(),
    ));

    // What an older packer, or the folder the mod was packed from, left in the pak beside the
    // container.
    let pak = files.part("pak");
    let (unsupported, unread_pak) = match pak.is_file() {
        false => (Vec::new(), Vec::new()),
        true => match rivals_core::pak::plain::unsupported_entries(&pak) {
            Ok(found) => (
                found
                    .into_iter()
                    .map(|entry| {
                        let why = rivals_core::pak::plain::unsupported_why(&entry);
                        format!("{entry}: {why}")
                    })
                    .collect(),
                Vec::new(),
            ),
            Err(e) => (
                Vec::new(),
                vec![format!(
                    "the pak beside the container could not be read: {e}"
                )],
            ),
        },
    };
    let pak_tidy =
        format!("`mods tidy {shown} --pak-only` takes them out and leaves the packages alone");
    let unsupported = if unsupported.is_empty() {
        unsupported
    } else {
        [unsupported, vec![pak_tidy]].concat()
    };
    steps.push(CheckStep::of(
        "pak entries",
        Vec::new(),
        [unread_pak, unsupported].concat(),
        "the pak beside the container holds nothing the game does not support, and no file a \
         folder left"
            .into(),
    ));

    let combined: Vec<String> = report
        .composites
        .iter()
        .flat_map(|check| {
            let table = check
                .table
                .rsplit('.')
                .next()
                .unwrap_or(&check.table)
                .to_string();
            check
                .unreadable
                .iter()
                .map(|reason| format!("parents not read: {reason}"))
                .chain(check.warnings())
                .map(move |line| format!("{table}: {line}"))
                .collect::<Vec<_>>()
        })
        .collect();
    steps.push(CheckStep::of(
        "combined tables",
        Vec::new(),
        capped(combined, SHOWN),
        format!(
            "{} combined table(s) agree with their parents",
            report.composites.len()
        ),
    ));

    if !args.forbid.is_empty() {
        let mut found = Vec::new();
        let mut unsearched = Vec::new();
        for text in &args.forbid {
            let result = rivals_core::mod_search::mod_search_with(
                &root,
                &utoc,
                schema.as_deref(),
                text,
                rivals_core::mod_search::SearchOptions { names: true },
            )?;
            found.extend(
                result
                    .hits
                    .iter()
                    .map(|hit| format!("{text}: {} {}: {}", hit.package, hit.export, hit.line)),
            );
            unsearched.extend(
                result
                    .unreadable
                    .iter()
                    .map(|(path, reason)| format!("{path} not searched: {reason}")),
            );
        }
        unsearched.sort();
        unsearched.dedup();
        steps.push(CheckStep::of(
            "forbidden text",
            capped(found, SHOWN),
            unsearched,
            format!("no package names {}", args.forbid.join(", ")),
        ));
    }

    let count = |status: &str| steps.iter().filter(|step| step.status == status).count();
    let failed = count("fail");
    #[derive(Serialize)]
    struct Checked<'a> {
        #[serde(rename = "mod")]
        mod_name: &'a str,
        steps: &'a [CheckStep],
    }
    emit(
        cli,
        &Checked {
            mod_name: &files.stem,
            steps: &steps,
        },
        || {
            outln!("{}", files.stem);
            for step in &steps {
                outln!("  {:<5} {}", step.status, step.name);
                for detail in &step.details {
                    outln!("        {detail}");
                }
            }
            outln!(
                "\n{} ok, {} warn, {failed} fail",
                count("ok"),
                count("warn")
            );
        },
    )?;
    some_failed(cli, failed, steps.len(), "checks failed")
}

fn mods_overrides(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &ModsOverridesArgs,
) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let utocs = if args.all {
        rivals_core::pak::containers::mod_utocs(&rivals_core::paths::paks_dir(&root))
    } else {
        let mut utocs = Vec::new();
        for pak in args.mods.all() {
            let files = mod_files(cli, app, pak)?;
            if !files.is_iostore() {
                return Err(format!("{} is not an IoStore mod", files.stem));
            }
            utocs.push(files.part("utoc"));
        }
        utocs
    };
    let schema = rivals_core::mappings::resolve(cli.usmap.as_deref(), app.usmap_path.as_deref())
        .and_then(|path| rivals_core::mappings::load(&path))?;
    let checked =
        rivals_core::override_check::check_overrides(&root, &utocs, Some(&*schema), args.values)?;
    emit(cli, &checked, || {
        for (at, one) in checked.iter().enumerate() {
            if at > 0 {
                outln!();
            }
            outln!("{}", one.container);
            if let Some(error) = &one.error {
                outln!("  not read: {error}");
                continue;
            }
            if one.packages.is_empty() {
                outln!("  overrides nothing the game ships");
            }
            for check in &one.packages {
                outln!("  {}  ({})", check.package, check.kind);
                outln!("    {}", check.verdict());
                for finding in &check.findings {
                    let form = match (&finding.game, &finding.ours) {
                        (Some(game), Some(ours)) => format!("  game: {game}, here: {ours}"),
                        (Some(game), None) => format!("  ({game})"),
                        _ => String::new(),
                    };
                    outln!("      {:<9} {}{form}", finding.kind.label(), finding.what);
                }
                print_changes(&check.changes);
            }
        }
    })
}

fn mods_diff(cli: &Cli, app: &settings::AppSettings, args: &ModsDiffArgs) -> Result<(), String> {
    let a = mod_utoc(cli, app, &args.a, false)?;
    let b = mod_utoc(cli, app, &args.b, false)?;
    let diff = if args.brief {
        rivals_core::mod_diff::pair_mods(&a, &b)?
    } else {
        let root = resolve::game_root(cli.game_root.as_deref(), app)?;
        let schema =
            rivals_core::mappings::resolve(cli.usmap.as_deref(), app.usmap_path.as_deref())
                .and_then(|path| rivals_core::mappings::load(&path))
                .ok();
        rivals_core::mod_diff::mod_diff(&root, &a, &b, schema.as_deref())?
    };
    emit(cli, &diff, || print_mod_diff(&diff))
}

/// Two mods compared, as many of each list as read well; `--json` has every one.
fn print_mod_diff(diff: &rivals_core::mod_diff::ModDiff) {
    use rivals_core::mod_diff::Only;
    const LISTED: usize = 50;
    const DIFFERENCES: usize = 40;
    const LINES: usize = 60;
    const MOVED: usize = 10;
    const VALUES: usize = 40;
    let side = |only: Option<Only>| match only {
        Some(Only::A) => "only in A",
        Some(Only::B) => "only in B",
        None => "differs",
    };
    outln!("A  {}", diff.a);
    outln!("B  {}", diff.b);
    outln!();
    outln!(
        "{} package(s) the same, {} changed, {} only in A, {} only in B",
        diff.identical,
        diff.changed.len(),
        diff.only_a.len(),
        diff.only_b.len()
    );
    for (label, list) in [("only in A", &diff.only_a), ("only in B", &diff.only_b)] {
        if list.is_empty() {
            continue;
        }
        outln!();
        outln!("{label}:");
        for line in capped(list.clone(), LISTED) {
            outln!("  {line}");
        }
    }
    for package in &diff.changed {
        outln!();
        if package.kind.is_empty() {
            outln!("{}", package.package);
        } else {
            outln!("{}  ({})", package.package, package.kind);
        }
        let bulk: Vec<&str> = package
            .chunks
            .iter()
            .map(String::as_str)
            .filter(|chunk| *chunk != "uasset")
            .collect();
        if !bulk.is_empty() {
            outln!("  differs in: {}", bulk.join(", "));
        }
        if let Some(reason) = &package.unreadable {
            outln!("  not compared: {reason}");
            continue;
        }
        let differences: Vec<String> = package
            .differences
            .iter()
            .map(|difference| {
                let form = match (&difference.a, &difference.b, difference.only) {
                    (Some(a), Some(b), None) => format!("  A: {a}, B: {b}"),
                    (Some(a), _, Some(Only::A)) | (_, Some(a), Some(Only::B)) => format!("  ({a})"),
                    _ => String::new(),
                };
                format!(
                    "{:<9} {:<9} {}{form}",
                    side(difference.only),
                    difference.kind.label(),
                    difference.what
                )
            })
            .collect();
        for line in capped(differences, DIFFERENCES) {
            outln!("  {line}");
        }
        for script in &package.scripts {
            outln!(
                "  {}: {} statement(s) in A, {} in B",
                script.function,
                script.statements[0],
                script.statements[1]
            );
            let total: usize = script.hunks.iter().map(Vec::len).sum();
            let mut shown = 0;
            for (at, hunk) in script.hunks.iter().enumerate() {
                if shown >= LINES {
                    break;
                }
                if at > 0 {
                    outln!("      ...");
                }
                for line in hunk.iter().take(LINES - shown) {
                    let mark = match line.only {
                        Some(Only::A) => '-',
                        Some(Only::B) => '+',
                        None => ' ',
                    };
                    outln!("    {mark} {}", line.text);
                    shown += 1;
                }
            }
            if total > shown {
                outln!(
                    "    and {} more line(s); --json has every one",
                    total - shown
                );
            }
        }
        if !package.moved.is_empty() {
            outln!(
                "  only moved: {}",
                capped(package.moved.clone(), MOVED).join(", ")
            );
        }
        if !package.values.is_empty() {
            outln!("  {} value(s) differ:", package.values.len());
            for value in package.values.iter().take(VALUES) {
                let row = value
                    .row
                    .as_deref()
                    .map_or_else(String::new, |row| format!("[{row}]"));
                let at = if value.path.is_empty() {
                    String::new()
                } else {
                    format!(" {}", value.path)
                };
                // The export below its package, which the line above names.
                let export = value
                    .export
                    .split_once('.')
                    .map_or(value.export.as_str(), |(_, below)| below);
                outln!("    {export}{row}{at}: A {}, B {}", value.a, value.b);
            }
            if package.values.len() > VALUES {
                outln!("    and {} more", package.values.len() - VALUES);
            }
        }
        let compared = !package.kind.is_empty();
        let nothing = package.differences.is_empty()
            && package.scripts.is_empty()
            && package.moved.is_empty()
            && package.values.is_empty();
        if compared && nothing && package.chunks.iter().any(|chunk| chunk == "uasset") {
            outln!(
                "  nothing compared here differs: the bytes differ where this does not read, such \
                 as how the package lays out its names and tables"
            );
        }
    }
    if !diff.pak_files.is_empty() {
        outln!();
        outln!("pak files:");
        for file in &diff.pak_files {
            outln!("  {:<9} {}", side(file.only), file.file);
        }
    }
}

/// The values an override changes, as many as read well; `--json` has every one.
fn print_changes(changes: &[rivals_core::value_diff::ValueChange]) {
    const SHOWN: usize = 40;
    if changes.is_empty() {
        return;
    }
    outln!("    changes {} value(s):", changes.len());
    for change in changes.iter().take(SHOWN) {
        let row = change
            .row
            .as_deref()
            .map_or_else(String::new, |row| format!("[{row}]"));
        let at = if change.path.is_empty() {
            String::new()
        } else {
            format!(" {}", change.path)
        };
        // The export below its package, which the line above names.
        let export = change
            .export
            .split_once('.')
            .map_or(change.export.as_str(), |(_, below)| below);
        outln!(
            "      {export}{row}{at}: game {}, here {}",
            change.game,
            change.ours
        );
    }
    if changes.len() > SHOWN {
        outln!("      and {} more", changes.len() - SHOWN);
    }
}

fn report_unreadable(unreadable: usize, paks: usize) {
    outln!("\n{paks} pak(s) with editable INI files");
    if unreadable > 0 {
        eprintln!("{unreadable} pak(s) could not be read:");
    }
}

// ---- logs ----

fn log_key(app: &settings::AppSettings) -> Result<rivals_core::game_logs::LogKey, String> {
    rivals_core::game_logs::LogKey::resolve(app.log_key.as_deref(), app.log_iv.as_deref())
}

fn logs_list(cli: &Cli, app: &settings::AppSettings, args: &LogsListArgs) -> Result<(), String> {
    use rivals_core::game_logs;

    let key = log_key(app)?;
    let running = rivals_core::game_status::game_pids();
    let mut sessions = game_logs::list_sessions(&running, &key);
    if args.crashes {
        sessions.retain(|s| s.crash_dir.is_some());
    }
    if let Some(limit) = args.limit {
        sessions.truncate(limit);
    }
    emit(cli, &sessions, || {
        if sessions.is_empty() {
            let dir = rivals_core::paths::logs_dir()
                .map(|d| d.display().to_string())
                .unwrap_or_else(|| "the game's Logs folder".to_string());
            outln!("no game logs in {dir}");
            return;
        }
        for session in &sessions {
            let mut line = format!(
                "{:<28} {:<14} {:<22} {:>9}",
                session.name,
                game_logs::outcome_label(session.outcome),
                session.build.as_deref().unwrap_or("-"),
                human_size(session.size)
            );
            if let Some(message) = &session.crash_message {
                line.push_str(&format!("  {message}"));
                if session.repeat_count > 1 {
                    line.push_str(&format!(" (x{})", session.repeat_count));
                }
            }
            if let Some(error) = &session.error {
                line.push_str(&format!("  {error}"));
            }
            outln!("{line}");
        }
    })
}

fn logs_decode(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &LogsDecodeArgs,
) -> Result<(), String> {
    use rivals_core::game_logs::{self, LogFilter, ParsedLog, decode};

    if let Some(out) = &args.out
        && out.exists()
        && !args.overwrite
    {
        return Err(format!(
            "{} is already there: pass --overwrite to write over it",
            out.display()
        ));
    }
    let key = log_key(app)?;
    let session = game_logs::find_session(&args.target.log)?;
    let path = session
        .log_path
        .clone()
        .ok_or_else(|| format!("{} kept no log", session.name))?;
    let blob = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let decoded = decode::decode(&blob, &key)?;
    for note in &decoded.notes {
        eprintln!("note: {note}");
    }
    let filter = LogFilter {
        verbosities: args.verbosity.iter().map(|&v| v.into()).collect(),
        categories: args.category.clone(),
        text: args.filter.clone().unwrap_or_default(),
        case_sensitive: false,
    };
    let filtered =
        !filter.verbosities.is_empty() || !filter.categories.is_empty() || !filter.text.is_empty();
    let key_source = decoded.key_source;

    if cli.json && args.out.is_none() {
        let log = ParsedLog::new(decoded)?;
        let rows = log.filter(&filter)?;
        let lines: Vec<serde_json::Value> = rows
            .iter()
            .map(|&n| {
                let n = n as usize;
                let text = log.line(n);
                serde_json::json!({
                    "line": n + 1,
                    "verbosity": log.verbosity(n),
                    "category": log.category(n),
                    "text": if args.redact { game_logs::redact(text) } else { text.to_string() },
                })
            })
            .collect();
        let report = serde_json::json!({
            "source": path.display().to_string(),
            "key_source": key_source,
            "notes": log.notes,
            "lines": lines,
        });
        return emit(cli, &report, || {});
    }

    let bytes = if filtered || args.redact {
        let log = ParsedLog::new(decoded)?;
        let rows = if filtered {
            log.filter(&filter)?
        } else {
            (0..log.len() as u32).collect()
        };
        let mut text = String::new();
        for &n in &rows {
            text.push_str(log.line(n as usize));
            text.push('\n');
        }
        if args.redact {
            text = game_logs::redact(&text);
        }
        text.into_bytes()
    } else {
        decoded.bytes
    };

    let Some(out) = &args.out else {
        use std::io::Write;
        if std::io::stdout().write_all(&bytes).is_err() {
            std::process::exit(0);
        }
        return Ok(());
    };
    if !args.dry_run {
        std::fs::write(out, &bytes).map_err(|e| format!("{}: {e}", out.display()))?;
    }
    let lines = bytes.iter().filter(|&&b| b == b'\n').count();
    let report = serde_json::json!({
        "source": path.display().to_string(),
        "out": out.display().to_string(),
        "lines": lines,
        "bytes": bytes.len(),
        "dry_run": args.dry_run,
    });
    emit(cli, &report, || {
        let verb = if args.dry_run { "would write" } else { "wrote" };
        outln!(
            "{verb} {lines} line(s), {}, to {}",
            human_size(bytes.len() as u64),
            out.display()
        );
    })
}

fn logs_analyze(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &LogsAnalyzeArgs,
) -> Result<(), String> {
    use rivals_core::game_logs::{self, ModsContext};
    use rivals_core::mod_library::DiskLister;

    let key = log_key(app)?;
    let session = game_logs::find_session(&args.target.log)?;
    let running = rivals_core::game_status::game_pids();
    let root = resolve::game_root(cli.game_root.as_deref(), app).ok();
    let mods = root.as_deref().map(|game_root| ModsContext {
        game_root,
        lister: &DiskLister,
    });
    let opened = game_logs::open(session, &key, &running, mods)?;
    let name = opened.session.name.clone();
    let crash = opened.crash.as_ref();
    let analysis = &opened.analysis;

    if args.share {
        let text = game_logs::share_text(&name, analysis, crash);
        let report = serde_json::json!({ "session": name, "text": text });
        return emit(cli, &report, || out!("{text}"));
    }

    let report = serde_json::json!({
        "session": {
            "name": name,
            "log_path": opened.session.log_path,
            "crash_dir": opened.session.crash_dir,
        },
        "line_count": opened.log.len(),
        "key_source": opened.key_source,
        "key_error": opened.key_error,
        "analysis": analysis,
        "crash": crash,
        "culprit": opened.culprit,
    });
    emit(cli, &report, || {
        outln!("{name}  {}", game_logs::outcome_label(analysis.outcome));
        if let Some(build) = &analysis.build {
            match &analysis.engine_version {
                Some(engine) => outln!("build {build} ({engine})"),
                None => outln!("build {build}"),
            }
        }
        if let Some(path) = &opened.session.log_path {
            outln!("log   {} ({} lines)", path.display(), opened.log.len());
        }
        if let Some(dir) = &opened.session.crash_dir {
            outln!("crash {}", dir.display());
        }
        if let Some(error) = &opened.key_error {
            outln!("the log did not decode: {error}");
        }
        if let Some(crash) = crash {
            print_crash(crash);
        }
        if !analysis.game_thread_python.is_empty() {
            outln!("  python on the game thread when it died:");
            for frame in analysis.game_thread_python.iter().take(8) {
                outln!(
                    "    {}:{} in {}{}",
                    frame.file,
                    frame.line,
                    frame.function,
                    if frame.third_party {
                        " (third-party)"
                    } else {
                        ""
                    }
                );
            }
        }
        if let Some(culprit) = &opened.culprit {
            print_culprit(culprit);
        }
        if let Some(fatal) = &analysis.fatal {
            outln!();
            outln!("fatal error at line {}:", fatal.line + 1);
            for line in fatal.text.lines() {
                outln!("  {line}");
            }
        }
        outln!();
        outln!(
            "lines: {} fatal, {} error, {} warning",
            analysis.fatal_count,
            analysis.error_count,
            analysis.warning_count
        );
        print_session_mods(analysis);
        if !analysis.groups.is_empty() {
            let shown = if args.all { analysis.groups.len() } else { 20 };
            outln!(
                "error groups ({} of {}{}):",
                shown.min(analysis.groups.len()),
                analysis.groups_total,
                if args.all {
                    ""
                } else {
                    "; --all for every one"
                }
            );
            if analysis.baseline_sessions > 0 {
                let alike = analysis.groups.iter().filter(|g| g.without_mods).count();
                outln!(
                    "  {alike} of them also happen in the last {} session(s) without mods",
                    analysis.baseline_sessions
                );
            }
            for group in analysis.groups.iter().take(shown) {
                print_group(group, "  ");
            }
        }
        for note in &analysis.notes {
            outln!("note: {note}");
        }
    })
}

fn print_group(group: &rivals_core::game_logs::ErrorGroup, indent: &str) {
    let category = if group.category.is_empty() {
        String::new()
    } else {
        format!("{}: ", group.category)
    };
    outln!(
        "{indent}[{:?}] x{} {category}{} (line {}){}",
        group.verbosity,
        group.count,
        group.message,
        group.first_line + 1,
        if group.without_mods {
            " (also without mods)"
        } else {
            ""
        }
    );
}

fn print_culprit(culprit: &rivals_core::game_logs::Culprit) {
    use rivals_core::game_logs::Culprit;

    outln!();
    match culprit {
        Culprit::Mods { mods, packages } => outln!(
            "cause: {} ships {}, which the crash names",
            mods.join(", "),
            packages.join(", ")
        ),
        Culprit::ThirdParty { module } => {
            outln!("cause: it crashed inside {module}, third-party code loaded into the game")
        }
    }
}

fn print_crash(crash: &rivals_core::game_logs::CrashReport) {
    outln!();
    let mut header = format!("{}: {}", crash.crash_type, crash.error_message);
    if let Some(seconds) = crash.seconds_since_start {
        header.push_str(&format!(" (after {seconds} s"));
        if let Some(thread) = &crash.crashed_thread {
            header.push_str(&format!(", on {thread}"));
        }
        header.push(')');
    }
    outln!("{header}");
    let frame = |at: Option<usize>| at.and_then(|at| crash.frames.get(at));
    if let Some(faulting) = frame(crash.faulting_frame) {
        outln!(
            "  faulting module: {} ({:?})",
            faulting.module,
            faulting.class
        );
    }
    if crash.culprit_frame != crash.faulting_frame
        && let Some(culprit) = frame(crash.culprit_frame)
    {
        outln!(
            "  called from:     {} ({:?})",
            culprit.module,
            culprit.class
        );
    }
    for frame in crash.frames.iter().take(12) {
        outln!("    {}", frame.label());
    }
    let third: Vec<&str> = crash
        .third_party_modules()
        .map(|m| m.name.as_str())
        .collect();
    if !third.is_empty() {
        outln!("  third-party modules: {}", third.join(", "));
    }
}

fn print_session_mods(analysis: &rivals_core::game_logs::LogAnalysis) {
    use rivals_core::game_logs::{ModState, ModsSource};

    if !analysis.mods.is_empty() {
        let from = match analysis.mods_source {
            ModsSource::Log => "as the log names them",
            ModsSource::Enabled => "switched on now",
            ModsSource::Unavailable => "no game folder to look them up in",
        };
        outln!("mods ({}, {from}):", analysis.mods.len());
        for m in &analysis.mods {
            match m.state {
                ModState::Enabled => outln!("  {}", m.name),
                ModState::Disabled => outln!("  {} (disabled since this session)", m.name),
                ModState::Missing => outln!("  {} (uninstalled since this session)", m.name),
            }
        }
    }
    if !analysis.mod_hits.is_empty() {
        outln!("errors naming packages these mods ship:");
        for hit in &analysis.mod_hits {
            outln!(
                "  {}: {} error line(s), {} warning line(s)",
                hit.mod_name,
                hit.errors,
                hit.warnings
            );
            for package in hit.packages.iter().take(10) {
                outln!("    {package}");
            }
            for group in hit
                .groups
                .iter()
                .filter_map(|&g| analysis.groups.get(g))
                .take(10)
            {
                print_group(group, "    ");
            }
        }
    }
}

fn logs_key(cli: &Cli, app: &settings::AppSettings) -> Result<(), String> {
    use rivals_core::game_logs::{self, KeySource};

    let key = log_key(app)?;
    let check = game_logs::check_key(&key);
    emit(cli, &check, || {
        match key.source {
            KeySource::Bundled => outln!("using the bundled log key"),
            KeySource::Manual => outln!("using the manual log key from the desktop app's settings"),
        }
        match (&check.session, check.opens) {
            (Some(name), Some(true)) => outln!("it opens the newest log, {name}"),
            (Some(name), _) => outln!(
                "it does not open the newest log, {name}: {}",
                check.error.as_deref().unwrap_or("")
            ),
            _ => outln!("there is no log to test it on"),
        }
    })
}

// ---- asset ----

/// A read of the asset `args` names: from `--from`, a file on disk as it is, or the copy the game
/// loads.
fn asset_request<'a>(
    cli: &'a Cli,
    app: &'a settings::AppSettings,
    args: &AssetArgs,
    game_root: &'a str,
) -> Result<asset::Request<'a>, String> {
    let source = resolve::read_source(&args.asset, args.from.as_deref(), game_root)?;
    Ok(asset::Request::read(
        game_root,
        source.container,
        source.entry,
        cli.usmap.as_deref(),
        app.usmap_path.as_deref(),
    ))
}

/// A save of the asset `args` names into the mod `save` names, reading it where the source rules
/// say, with the mod's name.
fn save_request<'a>(
    cli: &'a Cli,
    app: &'a settings::AppSettings,
    args: &AssetArgs,
    save: &'a SaveArgs,
    game_root: &'a str,
) -> Result<(asset::Request<'a>, String), String> {
    let mod_name = into_mod(app, save.into.as_deref(), game_root, true)?;
    let pak = rivals_core::asset_edit::mod_pak(game_root, &mod_name)?;
    let read = resolve::save_source(
        &args.asset,
        args.from.as_deref(),
        game_root,
        &pak,
        save.overwrite,
        save.target(app),
        "--from",
    )?;
    if save.dry_run {
        // A mod in `~mods` is named by its name, and one anywhere else by its path.
        let into = resolve::Source {
            container: pak.with_extension("utoc").to_string_lossy().into_owned(),
            entry: String::new(),
        };
        eprintln!(
            "would save into {}, reading {}",
            into.describe(game_root),
            read.source.describe(game_root)
        );
    }
    let mut request = asset::Request::read(
        game_root,
        read.source.container,
        read.source.entry,
        cli.usmap.as_deref(),
        app.usmap_path.as_deref(),
    );
    request.target = save.target(app);
    request.layer = read.layer;
    request.allow_missing = save.allow_missing;
    request.allow_unchecked = save.allow_unchecked;
    request.drop_orphaned_imports = !save.keep_orphaned_imports;
    request.drop_unused_locals = !save.keep_unused_locals;
    Ok((request, mod_name))
}

/// [`save_request`] for a save of one asset, under the name `--as` gives it.
fn write_request<'a>(
    cli: &'a Cli,
    app: &'a settings::AppSettings,
    args: &AssetArgs,
    write: &'a AssetWriteArgs,
    game_root: &'a str,
) -> Result<(asset::Request<'a>, String), String> {
    let (mut request, mod_name) = save_request(cli, app, args, &write.save, game_root)?;
    request.save_as = write.save_as.as_deref();
    request.keep_object_names = write.keep_object_names;
    Ok((request, mod_name))
}

/// Saves `edits` into the mod, or with `--dry-run` says what the save would change.
fn save_or_preview(
    cli: &Cli,
    request: &asset::Request<'_>,
    mod_name: &str,
    save: &SaveArgs,
    edits: rivals_uasset::PackageEdits,
) -> Result<(), String> {
    if save.dry_run {
        let preview = asset::preview_edits(request, mod_name, save.overwrite, edits)?;
        return emit(cli, &preview, || print_edit_preview(&preview));
    }
    block_for_game(cli, false)?;
    let message = asset::save_changes(request, edits, mod_name, save.overwrite)?;
    emit(cli, &message, || outln!("{message}"))
}

/// A plan with what the save it plans would change and note.
#[derive(Serialize)]
struct PlannedPreview<'a, P: Serialize> {
    #[serde(flatten)]
    plan: &'a P,
    #[serde(flatten)]
    preview: &'a asset::EditPreview,
}

/// A dry run of a command that plans its save: the plan, then the save previewed. The save checks
/// the package it would write, such as its load order, which the plan does not build, so what it
/// would refuse is refused here too and not only on the real save.
fn preview_planned<P: Serialize>(
    cli: &Cli,
    preview: Result<asset::EditPreview, String>,
    plan: &P,
    print_plan: impl FnOnce(),
) -> Result<(), String> {
    let preview = match preview {
        Ok(preview) => preview,
        Err(refused) => {
            if !cli.json {
                print_plan();
            }
            return Err(refused);
        }
    };
    emit(
        cli,
        &PlannedPreview {
            plan,
            preview: &preview,
        },
        || {
            print_plan();
            print_edit_preview(&preview);
        },
    )
}

/// The export a command names, by its index or by its name or path, in the package as `request`
/// reads it.
fn export_of(request: &asset::Request<'_>, given: &str) -> Result<u32, String> {
    Ok(asset::export_indices(request, &[given])?[0])
}

fn export_opt(request: &asset::Request<'_>, given: Option<&str>) -> Result<Option<u32>, String> {
    given.map(|given| export_of(request, given)).transpose()
}

fn asset_list(cli: &Cli, app: &settings::AppSettings, args: &ListArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let paths = if resolve::is_game(&args.source) {
        asset::list_game(&root, args.filter.as_deref())?
    } else {
        asset::list(
            &root,
            &resolve::container(&args.source, &root)?,
            args.filter.as_deref(),
        )?
    };
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

fn asset_info(cli: &Cli, app: &settings::AppSettings, args: &InfoArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let mut request = asset_request(cli, app, &args.asset, &root)?;
    request.declared = args.declared;
    let report = asset::info(&request)?;
    emit(cli, &report, || {
        asset::print_info(&report, &mut |line| outln!("{line}"));
    })
}

/// The language texts are shown in, and whether it was asked for rather than defaulted.
fn culture(cli: &Cli, app: &settings::AppSettings) -> asset::Culture {
    match &cli.culture {
        Some(name) => asset::Culture {
            name: name.clone(),
            asked: true,
        },
        None => asset::Culture {
            name: app.text_culture.clone().unwrap_or_else(|| "en".to_string()),
            asked: false,
        },
    }
}

fn asset_dump(cli: &Cli, app: &settings::AppSettings, args: &DumpArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let mut request = asset_request(cli, app, &args.asset, &root)?;
    // What an export leaves out is only listed by the parse that declares every slot.
    request.declared = args.declared || args.inherited;
    let export = export_opt(&request, args.export.as_deref())?;
    let parsed = asset::dump(&request, export, &culture(cli, app))?;
    let inherited = if args.inherited {
        asset::inherited(&request, &parsed)?
    } else {
        std::collections::BTreeMap::new()
    };
    emit(
        cli,
        &asset::DumpReport {
            package: &parsed,
            inherited: &inherited,
        },
        || {
            asset::print_dump(&parsed, &inherited, &mut |line| outln!("{line}"));
        },
    )
}

fn asset_table(cli: &Cli, app: &settings::AppSettings, args: &TableArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let report = asset::table(
        &asset_request(cli, app, &args.asset, &root)?,
        &culture(cli, app),
        args.row.as_deref(),
    )?;
    emit(cli, &report, || {
        asset::print_table(&report, &mut |line| outln!("{line}"));
    })
}

fn asset_trace(cli: &Cli, app: &settings::AppSettings, args: &TraceArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let mut request = asset_request(cli, app, &args.asset, &root)?;
    request.declared = args.declared;
    let export = export_opt(&request, args.export.as_deref())?;
    let entries = asset::trace(&request, export)?;
    emit(cli, &entries, || {
        asset::print_trace(&entries, &mut |line| outln!("{line}"));
    })
}

fn asset_hex(cli: &Cli, app: &settings::AppSettings, args: &HexArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let request = asset_request(cli, app, &args.asset, &root)?;
    let rows = asset::hex(&request, export_of(&request, &args.export)?, args.offset)?;
    emit(cli, &rows, || {
        asset::print_hex(&rows, &mut |line| outln!("{line}"));
    })
}

fn asset_fields(cli: &Cli, app: &settings::AppSettings, args: &FieldsArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let request = asset_request(cli, app, &args.asset, &root)?;
    let report = asset::fields(&request, export_of(&request, &args.export)?)?;
    emit(cli, &report, || {
        asset::print_fields(&report, &mut |line| outln!("{line}"));
    })
}

#[derive(Args)]
#[command(allow_negative_numbers = true)]
struct EditExportArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// The export: its index, as `asset info` prints it, or its name or path.
    #[arg(long, value_name = "EXPORT")]
    export: String,

    /// The object's new name.
    #[arg(long = "to", value_name = "NAME")]
    rename: Option<String>,

    /// Move the object under this export: its index, as `asset info` prints it, or its name or
    /// path.
    #[arg(long, value_name = "EXPORT", conflicts_with = "outer_root")]
    outer: Option<String>,

    /// Move the object to the package root, where the package's own asset sits.
    #[arg(long)]
    outer_root: bool,

    /// Point the object at the archetype at this package index, which its unset values inherit
    /// from.
    #[arg(long, value_name = "INDEX")]
    template: Option<i32>,

    /// Retype the object to the class at this package index. Its stored values do not survive:
    /// the export is emptied and takes the new class's defaults.
    #[arg(long, value_name = "INDEX")]
    class: Option<i32>,

    /// Reparent a class, struct or enum to the one at this package index.
    #[arg(long, value_name = "INDEX")]
    parent: Option<i32>,

    /// A flag to turn on, by name: Public, Standalone, Transactional, ArchetypeObject and so on.
    /// Repeatable.
    #[arg(long, value_name = "NAME")]
    set_flag: Vec<String>,

    /// A flag to turn off, by the same names. Repeatable.
    #[arg(long, value_name = "NAME")]
    clear_flag: Vec<String>,

    /// Whether other packages may import the object by hash.
    #[arg(long)]
    public_hash: Option<bool>,

    /// Go ahead although the plan carries warnings, such as packages still naming the old path.
    #[arg(long)]
    accept_warnings: bool,

    /// When the object is renamed or moved, leave the mod's other packages naming its old path
    /// instead of pointing them at the new one.
    #[arg(long)]
    keep_referencers: bool,

    #[command(flatten)]
    write: AssetWriteArgs,
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

fn asset_edit_export(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &EditExportArgs,
) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let (mut request, mod_name) = write_request(cli, app, &args.asset, &args.write, &root)?;
    request.keep_referencers = args.keep_referencers;
    let export = export_of(&request, &args.export)?;
    let outer = export_opt(&request, args.outer.as_deref())?;
    let mut edits = Vec::new();
    if let Some(name) = &args.rename {
        edits.push(rivals_uasset::ExportEdit::Rename {
            export,
            name: name.clone(),
            from: None,
        });
    }
    if outer.is_some() || args.outer_root {
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
        return Err("name a change: --to, --outer, --outer-root, --class, --parent, --template, --set-flag, --clear-flag or --public-hash".to_string());
    }
    let plan = asset::plan_export_edits(&request, &edits, &resets)?;
    if !plan.blockers.is_empty() {
        emit(cli, &plan, || {
            asset::print_export_plan(&plan, &mut |line| outln!("{line}"))
        })?;
        return Err("these changes cannot be made".to_string());
    }
    if args.write.save.dry_run {
        let changes = rivals_uasset::PackageEdits {
            exports: edits,
            reset_exports: resets,
            ..Default::default()
        };
        let preview = asset::preview_edits(&request, &mod_name, args.write.save.overwrite, changes);
        return preview_planned(cli, preview, &plan, || {
            asset::print_export_plan(&plan, &mut |line| outln!("{line}"))
        });
    }
    if !plan.warnings.is_empty() && !args.accept_warnings {
        return Err(format!(
            "these changes are not known to be safe:\n  {}\nPass --accept-warnings to go ahead, or --dry-run to see the whole plan.",
            plan.warnings.join("\n  ")
        ));
    }
    if !cli.force && rivals_core::game_status::should_block_for_game() {
        return Err(rivals_core::game_status::game_running_error());
    }
    let message = asset::export_edit(
        &request,
        edits,
        resets,
        &mod_name,
        args.write.save.overwrite,
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

#[derive(Args)]
struct MembersArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// The class: its export index, as `asset info` prints it, or its name. Defaults to the
    /// package's Blueprint class.
    #[arg(long, value_name = "EXPORT")]
    export: Option<String>,

    /// List a function's parameters and locals instead: its export index, or its name.
    #[arg(long, value_name = "EXPORT", conflicts_with = "export")]
    function: Option<String>,

    /// One member by name, with every use of it listed.
    #[arg(long, value_name = "NAME")]
    member: Option<String>,

    /// Only the members nothing references.
    #[arg(long)]
    unused: bool,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum QualityArg {
    Fast,
    Default,
    Best,
}

#[derive(Args)]
struct TextureArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// The texture: its export index, as `asset info` prints it, or its name. Defaults to the
    /// package's first texture. Given with `--in` in pairs, several textures of the package take
    /// their images in one save: `--export A --in a.png --export B --in b.png`.
    #[arg(long, value_name = "EXPORT")]
    export: Vec<String>,

    /// Save a mip as this PNG file, or as an OpenEXR (.exr) file for a texture holding high dynamic
    /// range values. A cube's faces and an array's or a volume's slices are stacked top to bottom.
    #[arg(long, value_name = "FILE", conflicts_with = "input")]
    out: Option<String>,

    /// Give the texture this image's pixels, at the image's size, encoded in the texture's own pixel
    /// format with its mips rebuilt from them, and save it into the mod: a PNG, or an OpenEXR (.exr)
    /// file for a texture holding high dynamic range values. A cube's six faces and an array's or a
    /// volume's slices are stacked top to bottom, as `--out` writes them.
    #[arg(long = "in", value_name = "FILE")]
    input: Vec<String>,

    /// How hard `--in` works at BC7 and BC6H, the formats that take long.
    #[arg(long, value_enum, default_value = "default")]
    quality: QualityArg,

    /// The mip `--out` saves, 0 being the largest. Defaults to the largest the package has: the
    /// game does not install the optional ones some textures put their largest mips in.
    #[arg(long, value_name = "N", requires = "out")]
    mip: Option<usize>,

    #[command(flatten)]
    write: AssetWriteArgs,
}

#[derive(Args)]
struct RenameMemberArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// The class: its export index, as `asset info` prints it, or its name. Defaults to the
    /// package's Blueprint class.
    #[arg(long, value_name = "EXPORT")]
    export: Option<String>,

    /// Rename a parameter or local of this function instead: its export index, or its name.
    #[arg(long, value_name = "EXPORT", conflicts_with = "export")]
    function: Option<String>,

    /// The member's name as it reads now.
    #[arg(long, value_name = "NAME")]
    member: String,

    /// The name it takes.
    #[arg(long, value_name = "NAME")]
    to: String,

    /// What to do with uses by name on an object the package does not say is of the class, such
    /// as a variable set by name on an actor a spawn returned.
    #[arg(long, value_enum, default_value = "refuse")]
    possible: PossibleArg,

    #[command(flatten)]
    write: AssetWriteArgs,
}

#[derive(Args)]
struct RemoveMemberArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// The class: its export index, as `asset info` prints it, or its name. Defaults to the
    /// package's Blueprint class.
    #[arg(long, value_name = "EXPORT")]
    export: Option<String>,

    /// Take locals out of this function instead: its export index, or its name.
    #[arg(long, value_name = "EXPORT", conflicts_with = "export")]
    function: Option<String>,

    /// A member to take out, by name. Repeat for several. An event graph no event enters any more
    /// takes its frame and the class's reference to it along.
    #[arg(
        long = "member",
        value_name = "NAME",
        required_unless_present = "unused"
    )]
    members: Vec<String>,

    /// Take out every member `asset members --unused` lists.
    #[arg(long, conflicts_with = "members")]
    unused: bool,

    /// Also take out the binding entries naming a function, such as the input binding of an input
    /// event, which would otherwise keep it.
    #[arg(long)]
    drop_bindings: bool,

    #[command(flatten)]
    write: AssetWriteArgs,
}

fn asset_remove_member(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &RemoveMemberArgs,
) -> Result<(), String> {
    let dry_run = args.write.save.dry_run;
    block_for_game(cli, dry_run)?;
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let (request, mod_name) = write_request(cli, app, &args.asset, &args.write, &root)?;
    let report = asset::remove_member(
        &request,
        &mod_name,
        args.write.save.overwrite,
        export_opt(&request, args.export.as_deref())?,
        export_opt(&request, args.function.as_deref())?,
        &args.members,
        args.unused,
        args.drop_bindings,
        dry_run,
    )?;
    emit(cli, &report, || {
        let verb = if report.pak.is_some() {
            "removed"
        } else {
            "would remove"
        };
        outln!(
            "{verb} from {}: {}",
            report.owner,
            report.removed.join(", ")
        );
        for followed in &report.followed {
            outln!("  with {followed}");
        }
        for trim in &report.graph_code {
            if trim.assignments {
                outln!(
                    "  with {} assignment(s) in {} storing into them",
                    trim.dropped.len(),
                    trim.name
                );
            } else {
                outln!(
                    "  with {} statement(s) of {} only they reached",
                    trim.dropped.len(),
                    trim.name
                );
            }
            if dry_run {
                const SHOWN: usize = 40;
                for line in trim.dropped.iter().take(SHOWN) {
                    outln!("    @{:04X}: {}", line.offset, line.text);
                }
                if trim.dropped.len() > SHOWN {
                    outln!("    and {} more", trim.dropped.len() - SHOWN);
                }
            }
        }
        for local in &report.locals {
            outln!("  with local {local}");
        }
        for import in &report.imports {
            outln!("  with import {import}");
        }
        if report.packages.len() > 1 {
            outln!("  in {}", report.packages.join(", "));
        }
        for warning in &report.warnings {
            outln!("warning: {warning}");
        }
        if let Some(pak) = &report.pak {
            outln!("wrote {pak}");
        }
    })
}

fn asset_rename_member(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &RenameMemberArgs,
) -> Result<(), String> {
    let possible = match args.possible {
        PossibleArg::Refuse => rivals_uasset::PossibleUses::Refuse,
        PossibleArg::Follow => rivals_uasset::PossibleUses::Follow,
        PossibleArg::Leave => rivals_uasset::PossibleUses::Leave,
    };
    block_for_game(cli, args.write.save.dry_run)?;
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let (request, mod_name) = write_request(cli, app, &args.asset, &args.write, &root)?;
    let report = asset::rename_member(
        &request,
        &mod_name,
        args.write.save.overwrite,
        export_opt(&request, args.export.as_deref())?,
        export_opt(&request, args.function.as_deref())?,
        &args.member,
        &args.to,
        possible,
        args.write.save.dry_run,
    )?;
    emit(cli, &report, || {
        asset::print_rename_member(&report, &mut |line| outln!("{line}"))
    })
}

fn asset_members(cli: &Cli, app: &settings::AppSettings, args: &MembersArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let request = asset_request(cli, app, &args.asset, &root)?;
    let report = asset::members(
        &request,
        export_opt(&request, args.export.as_deref())?,
        export_opt(&request, args.function.as_deref())?,
        args.member.as_deref(),
    )?;
    emit(cli, &report, || {
        asset::print_members(&report, args.member.is_some(), args.unused, &mut |line| {
            outln!("{line}")
        })
    })
}

fn asset_texture(cli: &Cli, app: &settings::AppSettings, args: &TextureArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    if !args.input.is_empty() {
        let dry_run = args.write.save.dry_run;
        block_for_game(cli, dry_run)?;
        let (request, mod_name) = write_request(cli, app, &args.asset, &args.write, &root)?;
        let quality = match args.quality {
            QualityArg::Fast => rivals_core::texture::Quality::Fast,
            QualityArg::Default => rivals_core::texture::Quality::Default,
            QualityArg::Best => rivals_core::texture::Quality::Best,
        };
        if args.input.len() == 1 && args.export.len() <= 1 {
            let report = asset::import_texture_image(
                &request,
                &mod_name,
                args.write.save.overwrite,
                export_opt(&request, args.export.first().map(String::as_str))?,
                Path::new(&args.input[0]),
                quality,
                dry_run,
            )?;
            return emit(cli, &report, || {
                outln!("{}", imported(&report, report.written.is_some()));
                if let Some(written) = &report.written {
                    outln!("{written}");
                }
                for warning in &report.warnings {
                    outln!("warning: {warning}");
                }
                for brush in &report.brushes {
                    outln!("  drawn at a size its brush stores: {brush}");
                }
            });
        }
        if args.export.len() != args.input.len() {
            return Err(format!(
                "{} image(s) for {} texture(s): name each image's texture, --export NAME --in FILE, one pair per texture",
                args.input.len(),
                args.export.len()
            ));
        }
        let names: Vec<&str> = args.export.iter().map(String::as_str).collect();
        let images: Vec<(Option<u32>, &Path)> = asset::export_indices(&request, &names)?
            .into_iter()
            .map(Some)
            .zip(args.input.iter().map(Path::new))
            .collect();
        let batch = asset::import_texture_images(
            &request,
            &mod_name,
            args.write.save.overwrite,
            &images,
            quality,
            dry_run,
        )?;
        return emit(cli, &batch, || {
            for report in &batch.textures {
                outln!("{}", imported(report, batch.written.is_some()));
            }
            if let Some(written) = &batch.written {
                outln!("{written}");
            }
            for warning in batch
                .textures
                .iter()
                .flat_map(|report| &report.warnings)
                .chain(&batch.warnings)
            {
                outln!("warning: {warning}");
            }
            for brush in &batch.brushes {
                outln!("  drawn at a size its brush stores: {brush}");
            }
        });
    }
    if args.export.len() > 1 {
        return Err(
            "a texture's details and --out are for one texture: name one --export, or give each \
             --export an --in"
                .into(),
        );
    }
    let request = asset_request(cli, app, &args.asset, &root)?;
    let export = export_opt(&request, args.export.first().map(String::as_str))?;
    if let Some(out) = &args.out {
        let saved = asset::save_texture_image(&request, export, args.mip, Path::new(out))?;
        return emit(cli, &saved, || {
            outln!(
                "saved mip {} of export {}, {} at {}x{}, to {}",
                saved.mip,
                saved.export,
                saved.format,
                saved.width,
                saved.height,
                saved.path
            )
        });
    }
    let info = asset::texture_info(&request, export)?;
    emit(cli, &info, || {
        asset::print_texture(&info, &mut |line| outln!("{line}"))
    })
}

/// The line `asset texture --in` prints for a texture given an image.
fn imported(report: &rivals_core::texture::ImportReport, written: bool) -> String {
    let verb = if written { "gave" } else { "would give" };
    let layers = if report.layers > 1 {
        format!(", {} layers each", report.layers)
    } else {
        String::new()
    };
    format!(
        "{verb} {} ({}) the image's pixels: {}x{} to {}x{}{layers}, {} mip(s)",
        report.name,
        report.format,
        report.from.0,
        report.from.1,
        report.to.0,
        report.to.1,
        report.mips
    )
}

fn asset_imports_table(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &ImportsArgs,
) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let report = asset::imports(&asset_request(cli, app, &args.asset, &root)?, args.unused)?;
    emit(cli, &report, || {
        asset::print_imports(&report, &mut |line| outln!("{line}"))
    })
}

fn asset_widen_script(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &WidenScriptArgs,
) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let (request, mod_name) = write_request(cli, app, &args.asset, &args.write, &root)?;
    let export = if args.all {
        None
    } else {
        export_opt(&request, args.export.as_deref())?
    };
    let overwrite = args.write.save.overwrite;
    if args.write.save.dry_run {
        let applied = asset::preview_script_widen(&request, export, &mod_name, overwrite)?;
        return emit(cli, &applied, || {
            for done in &applied {
                outln!("would set {}: {} -> {}", done.name, done.before, done.after);
            }
        });
    }
    block_for_game(cli, false)?;
    let message = asset::script_widen(&request, export, &mod_name, overwrite)?;
    emit(cli, &message, || outln!("{message}"))
}

fn asset_assemble_script(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &AssembleScriptArgs,
) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let (request, mod_name) = write_request(cli, app, &args.asset, &args.write, &root)?;
    let bytes = std::fs::read(&args.text_file)
        .map_err(|e| format!("read {}: {e}", args.text_file.display()))?;
    let text = rivals_core::asset_edit::json::decode_text(&bytes)
        .map_err(|e| format!("{}: {e}", args.text_file.display()))?;
    let edit = match (&args.new_function, &args.export) {
        (Some(name), _) => rivals_uasset::PackageEdits {
            new_functions: vec![rivals_uasset::NewFunctionEdit {
                class: export_opt(&request, args.class.as_deref())?,
                name: name.clone(),
                signature: args.signature.clone().unwrap_or_default(),
                text,
            }],
            ..Default::default()
        },
        (None, Some(export)) => rivals_uasset::PackageEdits {
            script_texts: vec![rivals_uasset::ScriptTextEdit {
                export: export_of(&request, export)?,
                text,
                was: None,
                object: None,
            }],
            ..Default::default()
        },
        (None, None) => return Err("give --export, or --new-function to add one".into()),
    };
    save_or_preview(cli, &request, &mod_name, &args.write.save, edit)
}

fn asset_add_field(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &AddFieldArgs,
) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let (request, mod_name) = write_request(cli, app, &args.asset, &args.write, &root)?;
    let edit = rivals_uasset::PackageEdits {
        add_fields: vec![rivals_uasset::AddField {
            strukt: export_opt(&request, args.strukt.as_deref())?,
            name: args.name.clone(),
            ty: args.ty.clone(),
        }],
        ..Default::default()
    };
    save_or_preview(cli, &request, &mod_name, &args.write.save, edit)
}

fn asset_add_variable(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &AddVariableArgs,
) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let (request, mod_name) = write_request(cli, app, &args.asset, &args.write, &root)?;
    let edit = rivals_uasset::PackageEdits {
        add_variables: vec![rivals_uasset::AddVariable {
            class: export_opt(&request, args.class.as_deref())?,
            name: args.name.clone(),
            ty: args.ty.clone(),
        }],
        ..Default::default()
    };
    save_or_preview(cli, &request, &mod_name, &args.write.save, edit)
}

fn asset_script(cli: &Cli, app: &settings::AppSettings, args: &ScriptArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let request = asset_request(cli, app, &args.asset, &root)?;
    let report = asset::script(&request, export_of(&request, &args.export)?)?;
    if args.as_text {
        // Exactly the text, so `> fn.txt` saves what `assemble-script` reads.
        return emit(cli, &report, || out!("{}", report.text));
    }
    emit(cli, &report, || {
        asset::print_script(&report, args.expressions, &mut |line| outln!("{line}"))
    })
}

fn asset_vm(cli: &Cli, app: &settings::AppSettings, args: &VmArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let request = asset_request(cli, app, &args.asset, &root)?;
    let report = asset::vm(&request, export_opt(&request, args.export.as_deref())?)?;
    emit(cli, &report, || {
        asset::print_vm(
            &report,
            args.constants,
            args.trace.as_deref(),
            &mut |line| outln!("{line}"),
        )
    })
}

fn asset_edit_vm(cli: &Cli, app: &settings::AppSettings, args: &EditVmArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let (request, mod_name) = write_request(cli, app, &args.asset, &args.write, &root)?;
    let at = u32::try_from(args.at)
        .map_err(|_| format!("{:#X} is not an offset a script can hold", args.at))?;
    let edits = rivals_uasset::PackageEdits {
        vm_consts: vec![rivals_uasset::VmConstEdit {
            export: args.export.clone(),
            at,
            operand: args.operand,
            value: args.value.clone(),
            was: None,
            retarget: args.retarget,
        }],
        ..Default::default()
    };
    save_or_preview(cli, &request, &mod_name, &args.write.save, edits)
}

fn asset_edit_script(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &EditScriptArgs,
) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let (request, mod_name) = write_request(cli, app, &args.asset, &args.write, &root)?;
    let export = export_of(&request, &args.export)?;
    let statement = u32::try_from(args.statement).map_err(|_| {
        format!(
            "{:#X} is not a statement offset a script can hold",
            args.statement
        )
    })?;
    let by_kind = [
        (args.object, rivals_uasset::SlotKind::Object),
        (args.text, rivals_uasset::SlotKind::Text),
        (args.call, rivals_uasset::SlotKind::Call),
    ]
    .into_iter()
    .find_map(|(nth, kind)| nth.map(|nth| (kind, nth)));
    let at = match (args.at, by_kind, args.condition) {
        (Some(at), _, _) => Some(
            u32::try_from(at).map_err(|_| format!("{at:#X} is not an offset a script can hold"))?,
        ),
        (None, Some((kind, nth)), _) => {
            let report = asset::script(&request, export)?;
            Some(rivals_uasset::expression_at(
                &report.script,
                statement,
                kind,
                nth,
            )?)
        }
        (None, None, true) => Some(statement),
        (None, None, false) => None,
    };
    let edit = rivals_uasset::ScriptConstEdit {
        export,
        statement,
        constant: args.constant,
        at,
        value: args.value.clone(),
        was: None,
        ..Default::default()
    };
    let overwrite = args.write.save.overwrite;
    if args.write.save.dry_run {
        let applied = asset::preview_script_set(&request, edit, &mod_name, overwrite)?;
        return emit(cli, &applied, || {
            for done in &applied {
                outln!(
                    "would set {} (file {:#X}): {} -> {}",
                    done.name,
                    done.offset,
                    done.before,
                    done.after
                );
            }
        });
    }
    let edits = rivals_uasset::PackageEdits {
        scripts: vec![edit],
        ..Default::default()
    };
    save_or_preview(cli, &request, &mod_name, &args.write.save, edits)
}

/// Where asset edits go when neither the flag nor the desktop app has chosen a mod.
const DEFAULT_MOD_NAME: &str = "AssetEdits";

#[derive(Args)]
struct AssetApplyArgs {
    /// Edit files, or manifests naming several or holding them inline, told apart by what they
    /// hold. Each item is applied on its own.
    #[arg(value_name = "FILE", required = true)]
    files: Vec<String>,

    /// Apply an edit file even where the package no longer matches what it was written against.
    #[arg(long)]
    allow_drift: bool,

    /// When an edit renames or moves an export, leave the mod's other packages naming the old path
    /// instead of pointing them at the new one.
    #[arg(long)]
    keep_referencers: bool,

    #[command(flatten)]
    save: SaveArgs,
}

#[derive(Args)]
struct CopyExportArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// The package to copy out of: a package name, its path inside a container, or an extracted
    /// file on disk.
    #[arg(long, value_name = "ASSET")]
    source: String,

    /// Where that package is read, as `--from` takes it. Without it, the package is read as a save
    /// reads its asset: the mod's own copy when it holds one, else the game's, and asked for when
    /// another switched-on mod overrides it, since what is copied lands in the mod.
    #[arg(long, value_name = "SOURCE")]
    source_from: Option<String>,

    /// The export to copy out of that package: its index, as its `asset info` prints it, or its
    /// name or path.
    #[arg(long, value_name = "EXPORT")]
    source_export: String,

    /// The export the copy sits under here. Omit for the package root.
    #[arg(long, value_name = "EXPORT")]
    outer: Option<String>,

    /// List the copy in this Level export's actors, so the game spawns it. A copied actor the
    /// level does not name loads with the package and never appears.
    #[arg(long, value_name = "EXPORT")]
    level: Option<String>,

    /// The copy's object name. Omit to keep the source's.
    #[arg(long, value_name = "NAME")]
    name: Option<String>,

    #[command(flatten)]
    write: AssetWriteArgs,
}

fn asset_copy_export(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &CopyExportArgs,
) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let (request, mod_name) = write_request(cli, app, &args.asset, &args.write, &root)?;
    let source = resolve::save_source(
        &args.source,
        args.source_from.as_deref(),
        &root,
        &rivals_core::asset_edit::mod_pak(&root, &mod_name)?,
        false,
        request.target,
        "--source-from",
    )?
    .source;
    if source.container.is_empty() {
        return Err(format!(
            "{} is a file on disk; copy-export reads its source from a container, which \
             --source-from names",
            args.source
        ));
    }
    let source_export = export_of(
        &asset::Request::read(
            &root,
            source.container.clone(),
            source.entry.clone(),
            cli.usmap.as_deref(),
            app.usmap_path.as_deref(),
        ),
        &args.source_export,
    )?;
    let copy = asset::CopyArgs {
        from_container: &source.container,
        from_entry: &source.entry,
        export: source_export,
        into_outer: export_opt(&request, args.outer.as_deref())?,
        name: args.name.as_deref().unwrap_or_default(),
        into_level: export_opt(&request, args.level.as_deref())?,
    };
    let plan = asset::plan_copy(&request, &copy)?;
    if !plan.blockers.is_empty() {
        emit(cli, &plan, || {
            asset::print_copy_plan(&plan, &mut |line| outln!("{line}"))
        })?;
        return Err("the copy cannot be made".to_string());
    }
    if args.write.save.dry_run {
        let preview =
            asset::preview_copy_export(&request, &copy, &mod_name, args.write.save.overwrite);
        return preview_planned(cli, preview, &plan, || {
            asset::print_copy_plan(&plan, &mut |line| outln!("{line}"))
        });
    }
    block_for_game(cli, false)?;
    let message = asset::copy_export(&request, &copy, &mod_name, args.write.save.overwrite)?;
    emit(cli, &message, || outln!("{message}"))
}

#[derive(Args)]
#[command(allow_negative_numbers = true)]
struct DepsArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// The export whose runs to show or replace: its index, or its name or path.
    #[arg(long, value_name = "EXPORT")]
    export: String,

    /// Objects that must be fully read before this one is. Package indices, comma separated: an
    /// export's position plus one, or minus an import's position plus one. `--sbs=` empties it.
    #[arg(long, value_name = "LIST", value_parser = parse_run, num_args = 0..=1, default_missing_value = "", allow_hyphen_values = true)]
    sbs: Option<RunList>,

    /// Objects that must exist before this one is read, which is what a reference needs.
    #[arg(long, value_name = "LIST", value_parser = parse_run, num_args = 0..=1, default_missing_value = "", allow_hyphen_values = true)]
    cbs: Option<RunList>,

    /// Objects that must be fully read before this one is built.
    #[arg(long, value_name = "LIST", value_parser = parse_run, num_args = 0..=1, default_missing_value = "", allow_hyphen_values = true)]
    sbc: Option<RunList>,

    /// Objects that must exist before this one is built.
    #[arg(long, value_name = "LIST", value_parser = parse_run, num_args = 0..=1, default_missing_value = "", allow_hyphen_values = true)]
    cbc: Option<RunList>,

    #[command(flatten)]
    write: AssetWriteArgs,
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

    /// The mod the edit file saves into, for `asset apply`, named as a save's `--into` is; one
    /// named by a path is written as its whole path. Defaults to the one the desktop app last saved
    /// into, then to `AssetEdits`.
    #[arg(long, value_name = "MOD")]
    into: Option<String>,
}

impl From<FormatArg> for rivals_core::asset_edit::SaveTarget {
    fn from(value: FormatArg) -> Self {
        match value {
            FormatArg::Iostore => Self::IoStore,
            FormatArg::Pak => Self::Pak,
        }
    }
}

fn asset_deps(cli: &Cli, app: &settings::AppSettings, args: &DepsArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    // With no run given the command reports rather than edits, so a run can be read before it is
    // replaced. An explicitly empty run is still an edit.
    let given = [&args.sbs, &args.cbs, &args.sbc, &args.cbc];
    if given.iter().all(|run| run.is_none()) {
        let request = asset_request(cli, app, &args.asset, &root)?;
        let report = asset::dependencies(&request, export_of(&request, &args.export)?)?;
        return emit(cli, &report, || {
            asset::print_dependencies(&report, &mut |line| outln!("{line}"))
        });
    }
    let (request, mod_name) = write_request(cli, app, &args.asset, &args.write, &root)?;
    let export = export_of(&request, &args.export)?;
    let held = asset::dependencies(&request, export)?.runs;
    let given =
        |run: &Option<RunList>, held: Vec<i32>| run.as_ref().map_or(held, |run| run.0.clone());
    let runs = rivals_uasset::Runs {
        serialize_before_serialize: given(&args.sbs, held.serialize_before_serialize),
        create_before_serialize: given(&args.cbs, held.create_before_serialize),
        serialize_before_create: given(&args.sbc, held.serialize_before_create),
        create_before_create: given(&args.cbc, held.create_before_create),
    };
    if args.write.save.dry_run {
        let plan = asset::plan_dependencies(&request, export, runs)?;
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
    block_for_game(cli, false)?;
    let message =
        asset::set_dependencies(&request, export, runs, &mod_name, args.write.save.overwrite)?;
    emit(cli, &message, || outln!("{message}"))
}

fn asset_diff(cli: &Cli, app: &settings::AppSettings, args: &AssetDiffArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    // The edits are addressed into the package as a save into the mod would read it.
    let mod_name = into_mod(app, args.into.as_deref(), &root, true)?;
    let target = app.asset_save_target.unwrap_or_default();
    let read = resolve::save_source(
        &args.asset.asset,
        args.asset.from.as_deref(),
        &root,
        &rivals_core::asset_edit::mod_pak(&root, &mod_name)?,
        false,
        target,
        "--from",
    )?;
    let mut request = asset::Request::read(
        &root,
        read.source.container,
        read.source.entry,
        cli.usmap.as_deref(),
        app.usmap_path.as_deref(),
    );
    request.target = target;
    let file = asset::diff(
        &request,
        std::path::Path::new(&args.edited),
        &args.asset.asset,
        args.asset.from.as_deref(),
        // As the save resolves it, so a mod named by a path is written whole and found from
        // anywhere the edit file is applied.
        args.into
            .as_deref()
            .map(|_| mod_name.as_str())
            .or(app.asset_mod_name.as_deref()),
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
    let save = &args.save;
    block_for_game(cli, save.dry_run)?;
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let mut items = Vec::new();
    for path in &args.files {
        items.extend(rivals_core::asset_edit::json::read_edits(Path::new(path))?);
    }
    let report = asset::apply(
        &root,
        &items,
        &asset::ApplyOverrides {
            into: save.into.as_deref().or(app.asset_mod_name.as_deref()),
            overwrite: save.overwrite,
            keep_referencers: args.keep_referencers,
            allow_drift: args.allow_drift,
            allow_missing: save.allow_missing,
            allow_unchecked: save.allow_unchecked,
            dry_run: save.dry_run,
            target: save.format.map(Into::into).or(app.asset_save_target),
            drop_orphaned_imports: !save.keep_orphaned_imports,
            drop_unused_locals: !save.keep_unused_locals,
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
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let (request, mod_name) = write_request(cli, app, &args.asset, &args.write, &root)?;
    save_or_preview(cli, &request, &mod_name, &args.write.save, set_edits(args)?)
}

/// The edit `asset set` makes: by path, into a struct by `--field`, or at an offset.
fn set_edits(args: &AssetSetArgs) -> Result<rivals_uasset::PackageEdits, String> {
    if let Some(path) = &args.path {
        return Ok(rivals_uasset::PackageEdits {
            paths: vec![path_edit(args, path)?],
            ..Default::default()
        });
    }
    // Present unless `--path` is, which clap holds to.
    let (Some(offset), Some(name), Some(kind)) = (args.offset, &args.name, &args.kind) else {
        return Err(
            "name the value by --offset, --expect-name and --expect-kind, or by --export and --path"
                .into(),
        );
    };
    Ok(match &args.field {
        Some(field) => rivals_uasset::PackageEdits {
            field_sets: vec![rivals_uasset::FieldSet {
                offset,
                expect_name: name.clone(),
                expect_element: args.element,
                path: field.split('.').map(str::to_string).collect(),
                text: args.value.clone(),
            }],
            ..Default::default()
        },
        None => rivals_uasset::PackageEdits {
            values: vec![rivals_uasset::ValueEdit {
                offset,
                expect_name: name.clone(),
                expect_element: args.element,
                expect_kind: kind.clone(),
                op: value_op(args)?,
            }],
            ..Default::default()
        },
    })
}

/// The edit `--op` makes on what `--path` names. An `--index` names an element of it, except for
/// an insert, where it is the new element's position.
fn path_edit(args: &AssetSetArgs, path: &str) -> Result<rivals_uasset::PathEdit, String> {
    use rivals_uasset::PathOp;
    let op = match args.op {
        SetOpArg::Set | SetOpArg::SetElement => PathOp::Set {
            text: args.value.clone(),
        },
        SetOpArg::Clear => PathOp::Clear,
        SetOpArg::Store => PathOp::Store,
        SetOpArg::Unset => PathOp::Unset,
        SetOpArg::Insert => PathOp::Insert {
            index: args.index,
            key: args
                .key
                .clone()
                .or_else(|| (!args.value.is_empty()).then(|| args.value.clone())),
        },
        SetOpArg::Remove => PathOp::Remove,
        SetOpArg::SetKey => PathOp::SetKey {
            text: args.key.clone().ok_or("--op set-key needs --key")?,
        },
        SetOpArg::Reorder => PathOp::Reorder {
            order: args.order.clone().ok_or("--op reorder needs --order")?,
        },
        SetOpArg::SetRaw => PathOp::SetRaw {
            hex: args.hex.clone().ok_or("--op set-raw needs --hex")?,
        },
    };
    let path = match args.index {
        Some(at) if !matches!(op, PathOp::Insert { .. }) => format!("{path}[{at}]"),
        _ => path.to_string(),
    };
    rivals_uasset::parse_path(&path)?;
    Ok(rivals_uasset::PathEdit {
        export: args.export.clone().ok_or("--path needs --export")?,
        row: args.row.clone(),
        defaults: args.defaults,
        path,
        op,
        was: None,
        becomes: None,
    })
}

/// The edit `--op` names. One missing the element it acts on, or a whole-value op given one, is
/// refused rather than guessed at.
fn value_op(args: &AssetSetArgs) -> Result<rivals_uasset::EditOp, String> {
    use rivals_uasset::EditOp;
    let op = choice(&args.op);
    let index = || args.index.ok_or_else(|| format!("--op {op} needs --index"));
    let whole = |edit: EditOp| match args.index {
        Some(_) => Err(format!(
            "--op {op} acts on the whole value and takes no --index; set-element sets one element"
        )),
        None => Ok(edit),
    };
    match args.op {
        SetOpArg::Set => whole(EditOp::Set {
            text: args.value.clone(),
        }),
        SetOpArg::Clear => whole(EditOp::Clear),
        SetOpArg::Store => whole(EditOp::Store),
        SetOpArg::Unset => whole(EditOp::Unset),
        SetOpArg::SetElement => Ok(EditOp::SetElement {
            index: index()?,
            text: args.value.clone(),
        }),
        SetOpArg::Insert => Ok(EditOp::Insert {
            index: index()?,
            key: args
                .key
                .clone()
                .or_else(|| (!args.value.is_empty()).then(|| args.value.clone())),
        }),
        SetOpArg::Remove => Ok(EditOp::Remove { index: index()? }),
        SetOpArg::SetKey => Ok(EditOp::SetKey {
            index: index()?,
            text: args.key.clone().ok_or("--op set-key needs --key")?,
        }),
        SetOpArg::Reorder => whole(EditOp::Reorder {
            order: args.order.clone().ok_or("--op reorder needs --order")?,
        }),
        SetOpArg::SetRaw => whole(EditOp::SetRaw {
            hex: args.hex.clone().ok_or("--op set-raw needs --hex")?,
        }),
    }
}

/// A dry run's report: every change the save would make, then its notes.
fn print_edit_preview(preview: &asset::EditPreview) {
    for done in &preview.applied {
        outln!("would set {}: {} -> {}", done.name, done.before, done.after);
    }
    for note in &preview.notes {
        outln!("note: {note}");
    }
}

fn asset_sweep(cli: &Cli, app: &settings::AppSettings, args: &SweepArgs) -> Result<(), String> {
    block_for_game(cli, args.dry_run)?;
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
        args.format
            .map(Into::into)
            .or(app.asset_save_target)
            .unwrap_or_default(),
        &rivals_core::asset_edit::sweep::SweepRequest {
            game_root: &root,
            container: &resolve::source(&args.source, &root)?,
            filter: args.filter.as_deref(),
            class: args.class.as_deref(),
            mod_name: &into_mod(app, args.into.as_deref(), &root, true)?,
            sets,
            limit: args.limit,
            layer: !args.overwrite,
            whole_game: resolve::is_game(&args.source),
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

fn asset_edit_imports(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &EditImportsArgs,
) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let (request, mod_name) = write_request(cli, app, &args.asset, &args.write, &root)?;
    let save = &args.write.save;
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
        if !plan.blockers.is_empty() {
            emit(cli, &plan, || {
                asset::print_import_plan(&plan, &mut |line| outln!("{line}"))
            })?;
            return Err("the imports named cannot be removed".to_string());
        }
        if save.dry_run {
            let changes = rivals_uasset::PackageEdits {
                imports: positions
                    .iter()
                    .map(|&import| rivals_uasset::ImportEdit::Remove { import, from: None })
                    .collect(),
                ..Default::default()
            };
            let preview = asset::preview_edits(&request, &mod_name, save.overwrite, changes);
            return preview_planned(cli, preview, &plan, || {
                asset::print_import_plan(&plan, &mut |line| outln!("{line}"))
            });
        }
        block_for_game(cli, false)?;
        let message = asset::import_remove(&request, &positions, &mod_name, save.overwrite)?;
        return emit(cli, &message, || outln!("{message}"));
    }
    let path = args
        .path
        .clone()
        .ok_or("pass --path to retarget or add an import, or --remove to drop one")?;
    let class = args
        .class
        .as_deref()
        .map(|path| {
            path.rsplit_once('.')
                .filter(|(package, name)| package.starts_with('/') && !name.is_empty())
                .map(|(package, name)| (package.to_string(), name.to_string()))
                .ok_or_else(|| format!("--class takes a class's path, such as /Script/Engine.StaticMesh, not {path}"))
        })
        .transpose()?;
    let edit = match args.import {
        Some(index) if index < 0 => rivals_uasset::ImportEdit::Retarget {
            import: (-index - 1) as u32,
            path: path.clone(),
            class,
            from: None,
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
    let edits = rivals_uasset::PackageEdits {
        imports: vec![edit],
        ..Default::default()
    };
    save_or_preview(cli, &request, &mod_name, save, edits)
}

/// The mod a save goes into, as `--into` names it or else the one the desktop app last saved into,
/// by the name the save takes it by. A mod the save makes is said so; with `make` false, a name no
/// installed mod has is refused.
fn into_mod(
    app: &settings::AppSettings,
    given: Option<&str>,
    game_root: &str,
    make: bool,
) -> Result<String, String> {
    let default = app.asset_mod_name.as_deref().unwrap_or(DEFAULT_MOD_NAME);
    let into = resolve::into_mod(given, default, game_root, make)?;
    if let Some(note) = &into.note {
        eprintln!("note: {note}");
    }
    Ok(into.name)
}

fn asset_revert(cli: &Cli, app: &settings::AppSettings, args: &RevertArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let mod_name = into_mod(app, args.from.as_deref(), &root, false)?;
    let entry = rivals_core::asset_edit::save_entry(&rivals_core::asset_edit::AssetEditRequest {
        game_root: &root,
        container: "",
        entry: &args.asset,
        kind: rivals_core::asset::AssetSource::Utoc,
        mod_name: &mod_name,
        changes: Default::default(),
    })?;
    let pak = rivals_core::asset_edit::mod_pak(&root, &mod_name)?;
    let name = pak.file_name().unwrap_or_default().to_string_lossy();
    if args.dry_run {
        let message = match rivals_core::asset_edit::plan_revert(&pak, &entry)? {
            rivals_core::asset_edit::RevertOutcome::Reverted => {
                format!("would take {entry} out of {name}")
            }
            rivals_core::asset_edit::RevertOutcome::RemovedMod => format!(
                "would take {entry} out of {name}, which then holds nothing, so the mod would be \
                 deleted"
            ),
            rivals_core::asset_edit::RevertOutcome::NotHeld => {
                return Err(format!("{name} does not carry {entry}"));
            }
        };
        return emit(
            cli,
            &serde_json::json!({ "message": message, "entry": entry, "mod": name }),
            || outln!("{message}"),
        );
    }
    block_for_game(cli, false)?;
    let outcome = rivals_core::asset_edit::revert_asset(&pak, &entry, &Default::default())?;
    let message = match outcome {
        rivals_core::asset_edit::RevertOutcome::Reverted => {
            format!("{name} no longer carries {entry}")
        }
        rivals_core::asset_edit::RevertOutcome::RemovedMod => {
            format!("{entry} was all {name} carried, so the mod is deleted")
        }
        rivals_core::asset_edit::RevertOutcome::NotHeld => {
            return Err(format!("{name} does not carry {entry}"));
        }
    };
    emit(
        cli,
        &serde_json::json!({ "outcome": outcome, "entry": entry, "mod": name }),
        || outln!("{message}"),
    )
}

fn asset_remove_export(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &RemoveExportArgs,
) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let (request, mod_name) = write_request(cli, app, &args.asset, &args.write, &root)?;
    let given: Vec<&str> = args.exports.iter().map(String::as_str).collect();
    let exports = asset::export_indices(&request, &given)?;
    let plan = asset::plan_removal(&request, &exports)?;
    if args.write.save.dry_run && !plan.blockers.is_empty() {
        return emit(cli, &plan, || {
            asset::print_plan(&plan, &mut |line| outln!("{line}"));
        });
    }
    if args.write.save.dry_run {
        let changes = rivals_uasset::PackageEdits {
            remove_exports: exports,
            ..Default::default()
        };
        let preview = asset::preview_edits(&request, &mod_name, args.write.save.overwrite, changes);
        return preview_planned(cli, preview, &plan, || {
            asset::print_plan(&plan, &mut |line| outln!("{line}"))
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
    let edits = rivals_uasset::PackageEdits {
        remove_exports: exports,
        ..Default::default()
    };
    save_or_preview(cli, &request, &mod_name, &args.write.save, edits)
}

fn asset_reset_export(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &ResetExportArgs,
) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let (request, mod_name) = write_request(cli, app, &args.asset, &args.write, &root)?;
    let edits = rivals_uasset::PackageEdits {
        reset_exports: vec![export_of(&request, &args.export)?],
        ..Default::default()
    };
    save_or_preview(cli, &request, &mod_name, &args.write.save, edits)
}

fn asset_add_export(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &AddExportArgs,
) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let (request, mod_name) = write_request(cli, app, &args.asset, &args.write, &root)?;
    let edits = rivals_uasset::PackageEdits {
        add_exports: vec![rivals_uasset::AddExport {
            class: args.class.clone(),
            outer: export_opt(&request, args.outer.as_deref())?,
            name: args.name.clone(),
            layout: None,
        }],
        ..Default::default()
    };
    save_or_preview(cli, &request, &mod_name, &args.write.save, edits)
}

fn asset_add_component(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &AddComponentArgs,
) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let (request, mod_name) = write_request(cli, app, &args.asset, &args.write, &root)?;
    let edits = rivals_uasset::PackageEdits {
        add_components: vec![rivals_uasset::AddComponent {
            node: export_opt(&request, args.node.as_deref())?.unwrap_or_default(),
            name: args.name.clone(),
            with_children: args.with_children,
            from_parent: args.from_parent.clone(),
        }],
        ..Default::default()
    };
    save_or_preview(cli, &request, &mod_name, &args.write.save, edits)
}

fn asset_remove_component(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &RemoveComponentArgs,
) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let (request, mod_name) = write_request(cli, app, &args.asset, &args.write, &root)?;
    let edits = rivals_uasset::PackageEdits {
        remove_components: vec![rivals_uasset::RemoveComponent {
            node: export_of(&request, &args.node)?,
            with_children: args.with_children,
        }],
        ..Default::default()
    };
    save_or_preview(cli, &request, &mod_name, &args.write.save, edits)
}

fn asset_save_as(cli: &Cli, app: &settings::AppSettings, args: &SaveAsArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let (mut request, mod_name) = save_request(cli, app, &args.asset, &args.save, &root)?;
    request.save_as = Some(&args.to);
    request.keep_object_names = args.keep_object_names;
    save_or_preview(
        cli,
        &request,
        &mod_name,
        &args.save,
        rivals_uasset::PackageEdits::default(),
    )
}

fn asset_rename_package(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &RenamePackageArgs,
) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let mod_name = into_mod(app, args.into.as_deref(), &root, false)?;
    let utoc = rivals_core::asset_edit::mod_pak(&root, &mod_name)?.with_extension("utoc");
    let name = utoc.file_stem().unwrap_or_default().to_string_lossy();
    if !utoc.is_file() || !rivals_core::asset::container_holds(&utoc, &args.asset)? {
        return Err(format!("{name} does not carry {}", args.asset));
    }
    let package = if args.asset.starts_with('/') {
        Some(args.asset.clone())
    } else {
        rivals_core::asset::package_name_of_entry(&args.asset)
    };
    if package.is_some_and(|package| rivals_core::asset::base_game_ships(&root, &package)) {
        return Err(format!(
            "the game ships {}, which {name} only overrides, so it cannot be moved there; save it \
             as a new asset with `asset save-as` instead",
            args.asset
        ));
    }
    if rivals_core::asset::container_holds(&utoc, &args.to)? {
        return Err(format!("{name} already holds {}", args.to));
    }
    if args.dry_run {
        let message = format!("would move {} to {} in {name}", args.asset, args.to);
        return emit(
            cli,
            &serde_json::json!({ "from": args.asset, "to": args.to, "mod": name }),
            || outln!("{message}"),
        );
    }
    block_for_game(cli, false)?;
    let mut request = asset::Request::read(
        &root,
        utoc.to_string_lossy().into_owned(),
        args.asset.clone(),
        cli.usmap.as_deref(),
        app.usmap_path.as_deref(),
    );
    request.keep_referencers = args.keep_referencers;
    request.allow_unchecked = args.allow_unchecked;
    let message = asset::rename_package(&request, &args.to)?;
    emit(cli, &message, || outln!("{message}"))
}

fn asset_edit_row(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &EditRowArgs,
) -> Result<(), String> {
    let new_name = || {
        args.name
            .clone()
            .ok_or_else(|| format!("--op {} needs --name", choice(&args.op)))
    };
    let op = match args.op {
        RowOpArg::Add => rivals_uasset::RowOp::Add {
            name: args.row.clone(),
            at: args.position,
        },
        RowOpArg::Duplicate => rivals_uasset::RowOp::Duplicate {
            source: args.row.clone(),
            name: new_name()?,
            at: args.position,
        },
        RowOpArg::Remove => rivals_uasset::RowOp::Remove {
            name: args.row.clone(),
        },
        RowOpArg::Rename => rivals_uasset::RowOp::Rename {
            name: args.row.clone(),
            to: new_name()?,
        },
    };
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let (request, mod_name) = write_request(cli, app, &args.asset, &args.write, &root)?;
    let edits = rivals_uasset::PackageEdits {
        rows: vec![rivals_uasset::RowEdit {
            export: export_of(&request, &args.export)?,
            op,
            object: None,
        }],
        ..Default::default()
    };
    save_or_preview(cli, &request, &mod_name, &args.write.save, edits)
}

fn asset_duplicate_export(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &DuplicateExportArgs,
) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let (request, mod_name) = write_request(cli, app, &args.asset, &args.write, &root)?;
    let edits = rivals_uasset::PackageEdits {
        duplicate_exports: vec![rivals_uasset::DuplicateExport {
            export: export_of(&request, &args.export)?,
            name: args.name.clone(),
            into_level: export_opt(&request, args.level.as_deref())?,
        }],
        ..Default::default()
    };
    save_or_preview(cli, &request, &mod_name, &args.write.save, edits)
}

fn asset_payload(cli: &Cli, app: &settings::AppSettings, args: &PayloadArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    match (&args.out, &args.input) {
        (Some(out), None) => {
            let request = asset_request(cli, app, &args.asset, &root)?;
            let bytes = asset::payload_bytes(&request, export_of(&request, &args.export)?)?;
            std::fs::write(out, &bytes).map_err(|e| format!("could not write {out}: {e}"))?;
            let message = format!("Wrote {} bytes to {out}", bytes.len());
            emit(cli, &message, || outln!("{message}"))
        }
        (None, Some(input)) => {
            let (request, mod_name) = write_request(cli, app, &args.asset, &args.write, &root)?;
            let bytes = std::fs::read(input).map_err(|e| format!("could not read {input}: {e}"))?;
            let edits = rivals_uasset::PackageEdits {
                payloads: vec![rivals_uasset::PayloadEdit {
                    export: export_of(&request, &args.export)?,
                    bytes,
                    object: None,
                }],
                ..Default::default()
            };
            save_or_preview(cli, &request, &mod_name, &args.write.save, edits)
        }
        _ => Err("pass --out PATH to save the payload or --in PATH to replace it".into()),
    }
}

fn asset_bulk(cli: &Cli, app: &settings::AppSettings, args: &BulkArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let resource = || {
        args.resource
            .ok_or("--out and --in need --resource to say which resource")
    };
    if let Some(out) = &args.out {
        let bytes = asset::bulk_bytes(&asset_request(cli, app, &args.asset, &root)?, resource()?)?;
        std::fs::write(out, &bytes).map_err(|e| format!("could not write {out}: {e}"))?;
        let message = format!("Wrote {} bytes to {out}", bytes.len());
        return emit(cli, &message, || outln!("{message}"));
    }
    if let Some(input) = &args.input {
        let bytes = std::fs::read(input).map_err(|e| format!("could not read {input}: {e}"))?;
        let (request, mod_name) = write_request(cli, app, &args.asset, &args.write, &root)?;
        let edits = rivals_uasset::PackageEdits {
            bulk: vec![rivals_uasset::BulkEdit {
                resource: resource()?,
                bytes,
            }],
            ..Default::default()
        };
        return save_or_preview(cli, &request, &mod_name, &args.write.save, edits);
    }
    let resources = asset::bulk_list(&asset_request(cli, app, &args.asset, &root)?)?;
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

fn asset_edit_keys(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &EditKeysArgs,
) -> Result<(), String> {
    let op = choice(&args.op);
    let index = || args.index.ok_or_else(|| format!("--op {op} needs --index"));
    let time = || args.time.ok_or_else(|| format!("--op {op} needs --time"));
    let op = match args.op {
        KeyOpArg::Add => rivals_uasset::KeyOp::Add {
            time: time()?,
            value: args.value.ok_or("--op add needs --value")?,
        },
        KeyOpArg::Duplicate => rivals_uasset::KeyOp::Duplicate {
            index: index()?,
            time: time()?,
        },
        KeyOpArg::Move => rivals_uasset::KeyOp::Move {
            index: index()?,
            time: time()?,
        },
        KeyOpArg::Remove => rivals_uasset::KeyOp::Remove { index: index()? },
    };
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let (request, mod_name) = write_request(cli, app, &args.asset, &args.write, &root)?;
    let edits = rivals_uasset::PackageEdits {
        keys: vec![rivals_uasset::KeyEdit {
            offset: args.offset,
            expect_name: args.name.clone(),
            expect_element: args.element,
            op,
        }],
        ..Default::default()
    };
    save_or_preview(cli, &request, &mod_name, &args.write.save, edits)
}

fn asset_edit_strings(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &EditStringsArgs,
) -> Result<(), String> {
    let op = choice(&args.op);
    let index = || args.index.ok_or_else(|| format!("--op {op} needs --index"));
    let text = || {
        args.text
            .clone()
            .ok_or_else(|| format!("--op {op} needs --text"))
    };
    let id = || {
        args.id
            .clone()
            .ok_or_else(|| format!("--op {op} needs --id"))
    };
    let op = match args.op {
        StringOpArg::SetKey => rivals_uasset::StringOp::SetKey {
            index: index()?,
            key: args.key.clone(),
            to: text()?,
        },
        StringOpArg::SetTag => rivals_uasset::StringOp::SetTag {
            index: index()?,
            key: args.key.clone(),
            to: text()?,
        },
        StringOpArg::MetaSet => rivals_uasset::StringOp::SetMetaData {
            index: index()?,
            key: args.key.clone(),
            id: id()?,
            to: text()?,
        },
        StringOpArg::MetaRemove => rivals_uasset::StringOp::RemoveMetaData {
            index: index()?,
            key: args.key.clone(),
            id: id()?,
        },
        StringOpArg::SetSource => rivals_uasset::StringOp::SetSource {
            index: index()?,
            key: args.key.clone(),
            to: text()?,
        },
        StringOpArg::Add => rivals_uasset::StringOp::Add {
            key: args.key.clone(),
            source: text()?,
        },
        StringOpArg::Remove => rivals_uasset::StringOp::Remove {
            index: index()?,
            key: args.key.clone(),
        },
    };
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let (request, mod_name) = write_request(cli, app, &args.asset, &args.write, &root)?;
    let edits = rivals_uasset::PackageEdits {
        strings: vec![rivals_uasset::StringEdit {
            export: export_of(&request, &args.export)?,
            op,
            object: None,
        }],
        ..Default::default()
    };
    save_or_preview(cli, &request, &mod_name, &args.write.save, edits)
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

fn asset_search(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &AssetSearchArgs,
) -> Result<(), String> {
    use rivals_core::game_search::{GameSearch, SearchPhase, game_search};
    use std::io::IsTerminal;
    use std::sync::atomic::AtomicBool;

    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let mappings = game_mappings(cli, app)?;
    let values = args.values
        || args
            .kind
            .iter()
            .any(|kind| matches!(kind, SearchKindArg::Value | SearchKindArg::Field));
    let query = rivals_core::mod_search::Query::new(
        &args.query,
        args.kind.iter().map(|kind| (*kind).into()).collect(),
        values,
    )?
    .whole_word(args.word)
    .fields(values)
    .skip_self(args.no_self);
    // Ctrl+C ends the process, so nothing ever cancels a CLI search.
    static NEVER: AtomicBool = AtomicBool::new(false);
    let shown = std::io::stderr().is_terminal();
    let last = std::sync::Mutex::new(None);
    let progress = |phase: SearchPhase, current: usize, total: usize| {
        if !shown {
            return;
        }
        let Ok(mut last) = last.lock() else { return };
        if last.is_some_and(|held| held != phase) {
            eprintln!();
        }
        *last = Some(phase);
        let name = match phase {
            SearchPhase::Listing => "listing",
            SearchPhase::Headers => "headers",
            SearchPhase::Scripts => "scripts",
            SearchPhase::Packages => "packages",
        };
        eprint!("\r  {name} {current}/{total}");
    };
    let result = game_search(
        &root,
        &mappings,
        &GameSearch {
            query: &query,
            filter: &args.filter,
            mods: !args.no_mods,
            max_hits: (args.limit > 0).then_some(args.limit),
        },
        &NEVER,
        &progress,
    )?;
    if shown {
        eprintln!();
    }
    emit(cli, &result, || {
        match args.by {
            None => print_hits(&result.hits.iter().collect::<Vec<_>>(), ""),
            Some(by) => {
                for (title, hits) in hits_by_place(&result.hits, by) {
                    outln!("{title}");
                    print_hits(&hits, "  ");
                }
            }
        }
        let what = if values {
            "every package"
        } else {
            "only those holding functions"
        };
        outln!(
            "\n{} hit(s) in {} package(s); searched {} of {} packages ({what})",
            result.found,
            result.packages,
            result.searched,
            result.listed
        );
        if result.truncated {
            outln!(
                "showing the first {} by path: raise --limit, or narrow the search with --filter \
                 or --kind",
                result.hits.len()
            );
        }
        if !result.unreadable.is_empty() {
            eprintln!("{} package(s) could not be read:", result.unreadable.len());
            for (path, reason) in result.unreadable.iter().take(20) {
                eprintln!("  {path}: {reason}");
            }
        }
    })
}

/// The mappings a read of the game's own packages needs.
fn game_mappings(
    cli: &Cli,
    app: &settings::AppSettings,
) -> Result<std::sync::Arc<rivals_uasset::Mappings>, String> {
    rivals_core::mappings::resolve(cli.usmap.as_deref(), app.usmap_path.as_deref())
        .and_then(|path| rivals_core::mappings::load(&path))
        .map_err(|e| format!("reading the game's packages needs a .usmap mappings file: {e}"))
}

/// Prints search hits under their packages, each line indented by `indent` more.
fn print_hits(hits: &[&rivals_core::mod_search::SearchHit], indent: &str) {
    let mut package = "";
    for hit in hits {
        if hit.package != package {
            package = &hit.package;
            match &hit.in_mod {
                Some(name) => outln!("{indent}{package}  (in {name})"),
                None => outln!("{indent}{package}"),
            }
        }
        let at = hit
            .offset
            .map_or_else(String::new, |o| format!(" 0x{o:04X}"));
        let id = hit
            .id
            .as_ref()
            .map_or_else(String::new, |id| format!(" [{id}]"));
        let text = hit.call.as_deref().unwrap_or(&hit.line);
        outln!(
            "{indent}  {:<9} {}{at}{id}  {text}",
            hit.kind.name(),
            hit.export
        );
    }
}

/// Search hits under a heading for each hero or ability, in id order: an ability's own version
/// first, then each mode's and each event's flagged, and hits belonging to no hero last.
fn hits_by_place(
    hits: &[rivals_core::mod_search::SearchHit],
    by: SearchGroupArg,
) -> Vec<(String, Vec<&rivals_core::mod_search::SearchHit>)> {
    use rivals_core::ability_ids::Variant;
    let mut groups =
        std::collections::BTreeMap::<_, (String, Vec<&rivals_core::mod_search::SearchHit>)>::new();
    for hit in hits {
        let (key, title) = match hit
            .place
            .as_ref()
            .and_then(|place| Some((place.hero?, place)))
        {
            None => ((u32::MAX, 0, Variant::Base), "other".to_string()),
            Some((hero, place)) => match place.ability.filter(|_| by == SearchGroupArg::Ability) {
                None => ((hero, 0, Variant::Base), hero.to_string()),
                Some(ability) => {
                    let title = format!("{hero} / {ability}{}", variant_flag(place.variant));
                    ((hero, ability, place.variant), title)
                }
            },
        };
        groups
            .entry(key)
            .or_insert_with(|| (title, Vec::new()))
            .1
            .push(hit);
    }
    groups.into_values().collect()
}

/// A mode's or an event's version flagged after what it names; nothing for the game's own.
fn variant_flag(variant: rivals_core::ability_ids::Variant) -> String {
    match variant {
        rivals_core::ability_ids::Variant::Base => String::new(),
        variant => format!("  [{variant}]"),
    }
}

fn asset_ability(cli: &Cli, app: &settings::AppSettings, args: &AbilityArgs) -> Result<(), String> {
    use rivals_core::ability_summary::{AbilityRow, ability_summary, hero_abilities};

    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let mappings = game_mappings(cli, app)?;
    let culture = culture(cli, app);
    let row_line = |row: &AbilityRow| {
        let mut line = format!("{}  {}  [{}]", row.id, row.name, row.key);
        if let Some(cooldown) = &row.cooldown {
            line.push_str(&format!("  cooldown {cooldown}"));
        }
        if let Some(charges) = &row.charges {
            line.push_str(&format!("  {charges} charges"));
        }
        if !row.flags.is_empty() {
            line.push_str(&format!("  ({})", row.flags.join(", ")));
        }
        line
    };
    if args.id < 100_000 {
        let hero = hero_abilities(&root, &mappings, Some(&culture.name), args.id)?;
        return emit(cli, &hero, || {
            for row in &hero.rows {
                outln!("{}", row_line(row));
            }
        });
    }
    let summary = ability_summary(&root, &mappings, Some(&culture.name), args.id, args.calls)?;
    emit(cli, &summary, || {
        match &summary.row {
            Some(row) => {
                outln!("{}", row_line(row));
                for line in row.description.lines() {
                    outln!("  {line}");
                }
                if let Some(asset) = &row.asset {
                    outln!("  item {asset}");
                }
            }
            None => outln!(
                "hero {}'s ability table has no row {}",
                summary.hero,
                summary.id
            ),
        }
        if let Some(item) = &summary.item {
            outln!("\ninputs");
            for input in &item.inputs {
                outln!("  {} -> {} ({})", input.method, input.key, input.mode);
            }
            for action in &item.actions {
                outln!("  action {action}");
            }
            if let Some(class) = &item.class {
                outln!("  runs as {class}");
            }
            if let Some(config) = &item.config {
                outln!("  configured by {config}");
            }
        }
        if !summary.python.is_empty() {
            outln!("\npython classes");
            for python in &summary.python {
                outln!("  {}{}", python.class, variant_flag(python.variant));
            }
        }
        outln!("\npackages");
        for held in &summary.packages {
            let summon = if summary.summons.contains(&held.path) {
                "  (summon)"
            } else {
                ""
            };
            let in_mod = held
                .in_mod
                .as_ref()
                .map_or_else(String::new, |name| format!("  (in {name})"));
            outln!(
                "  {}{}{summon}{in_mod}",
                held.path,
                variant_flag(held.variant)
            );
        }
        if !summary.tags.is_empty() {
            outln!("\ntags");
            for tag in &summary.tags {
                outln!("  {tag}");
            }
        }
        outln!("\ncalls");
        for call in &summary.calls {
            outln!("  {}", call.text);
            outln!(
                "      {} {} 0x{:04X}",
                call.package,
                call.export,
                call.offset
            );
        }
        if summary.other_calls > 0 {
            outln!(
                "  and {} other call(s); --calls lists them",
                summary.other_calls
            );
        } else if summary.calls.is_empty() && !summary.python.is_empty() {
            outln!("  none in Blueprint scripts; its logic is in the Python classes above");
        } else if summary.calls.is_empty() {
            outln!("  none: no package carrying the id holds a script making one");
        }
        if !summary.unreadable.is_empty() {
            eprintln!("{} package(s) could not be read:", summary.unreadable.len());
            for (path, reason) in &summary.unreadable {
                eprintln!("  {path}: {reason}");
            }
        }
    })
}

fn asset_names(cli: &Cli, app: &settings::AppSettings, args: &NamesArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    if args.compact {
        let (request, mod_name) = write_request(cli, app, &args.asset, &args.write, &root)?;
        let unused = asset::unused_names(&request, &mod_name)?;
        if args.write.save.dry_run {
            return emit(cli, &unused, || {
                for name in &unused {
                    outln!("{name}");
                }
                outln!("would drop {} unused name(s)", unused.len());
            });
        }
        if unused.is_empty() {
            return Err("every name in the package is in use, so there is nothing to drop".into());
        }
        block_for_game(cli, false)?;
        let edits = rivals_uasset::PackageEdits {
            compact_names: true,
            ..Default::default()
        };
        let message = asset::save_changes(&request, edits, &mod_name, args.write.save.overwrite)?;
        let message = format!("{message}\ndropped {} unused name(s)", unused.len());
        return emit(cli, &message, || outln!("{message}"));
    }
    let request = asset_request(cli, app, &args.asset, &root)?;
    if args.unused {
        let unused = asset::unused_names(&request, "")?;
        return emit(cli, &unused, || {
            for name in &unused {
                outln!("{name}");
            }
            outln!("{} unused name(s)", unused.len());
        });
    }
    let names = asset::names(&request)?;
    emit(cli, &names, || {
        for (index, name) in names.iter().enumerate() {
            outln!("{index:4} {name}");
        }
    })
}

fn asset_diagnose(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &DiagnoseArgs,
) -> Result<(), String> {
    let audit = &args.audit;
    if audit.source.is_none() && audit.dir.is_none() && !audit.all {
        return Err("name a SOURCE, or pass --all or --dir".into());
    }
    let show_progress = audit.progress;
    let tick = |stage: &str, current: usize, total: usize| {
        if show_progress && (current.is_multiple_of(50) || current == total) {
            eprint!("\r  {stage}: {current}/{total}          ");
        }
    };
    let root = resolve::game_root(cli.game_root.as_deref(), app).unwrap_or_default();
    let container = match &audit.source {
        Some(given) if !resolve::is_game(given) => {
            Some(resolve::container(given, &root)?.into_owned())
        }
        _ => None,
    };
    let report = asset::diagnose(
        &asset::DiagnoseRequest {
            dir: audit.dir.as_deref(),
            game_root: &root,
            container: container.as_deref(),
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

fn asset_check_structs(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &CheckStructsArgs,
) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let container = if resolve::is_game(&args.source) {
        None
    } else {
        Some(resolve::container(&args.source, &root)?.into_owned())
    };
    let show_progress = args.progress;
    let tick = |current: usize, total: usize| {
        if show_progress && (current.is_multiple_of(200) || current == total) {
            eprint!("\r  {current}/{total} packages");
        }
    };
    let report = asset::synth_check(
        &root,
        container.as_deref(),
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
    let root = resolve::game_root(cli.game_root.as_deref(), app);
    let whole_game = args.all || args.source.as_deref().is_some_and(resolve::is_game);
    let newest = match (&root, whole_game) {
        (Ok(root), true) => asset::newest_patch(root).ok(),
        (Err(e), true) => return Err(e.clone()),
        _ => None,
    };
    let given = args.source.as_ref().filter(|_| !whole_game);
    let report = match (newest.as_ref().or(given), &args.dir) {
        (_, Some(dir)) => asset::audit_dir(
            dir,
            args.limit,
            args.filter.as_deref(),
            cli.usmap.as_deref(),
            app.usmap_path.as_deref(),
            args.skip_blueprint,
            tick,
        )?,
        (container, None) if container.is_some() || whole_game => asset::audit(
            &resolve::game_root(cli.game_root.as_deref(), app)?,
            match (container, &root) {
                (Some(given), Ok(root)) => Some(resolve::container(given, root)?.into_owned()),
                (given, _) => given.cloned(),
            }
            .as_deref(),
            args.limit,
            args.filter.as_deref(),
            cli.usmap.as_deref(),
            app.usmap_path.as_deref(),
            args.skip_blueprint,
            args.relocation_check,
            args.text_check,
            tick,
        )?,
        _ => return Err("name a SOURCE, or pass --all or --dir".into()),
    };
    if show_progress {
        eprintln!();
    }
    emit(cli, &report, || {
        asset::print_audit(&report, &mut |line| outln!("{line}"));
    })
}

/// Resolving a preset against the catalogue. The desktop app does the same thing in TypeScript,
/// so the two have to agree about what a preset asks for or the CLI cannot be used to reproduce
/// what a user saw.
#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod preset_tests {
    use super::*;
    use rivals_core::tweaks::catalogue::{TweakKind, tweak_catalogue};

    fn setting(id: &str, enabled: bool, value: Option<&str>) -> TweakSetting {
        TweakSetting {
            id: id.to_string(),
            enabled,
            value: value.map(str::to_string),
        }
    }

    fn app_with(settings: Vec<TweakSetting>) -> settings::AppSettings {
        settings::AppSettings {
            tweak_profiles: vec![settings::TweakProfile {
                name: "QOL".into(),
                settings,
            }],
            ..Default::default()
        }
    }

    /// Any id the catalogue happens to have, so the tests do not pin a tweak that may be renamed.
    fn some_toggle() -> String {
        tweak_catalogue()
            .into_iter()
            .find(|d| matches!(d.kind, TweakKind::Toggle { .. }))
            .expect("the catalogue has a toggle")
            .id
    }

    fn a_remove_only() -> Option<String> {
        tweak_catalogue()
            .into_iter()
            .find(|d| {
                matches!(
                    d.kind,
                    TweakKind::RemoveLines {
                        remove_only: true,
                        ..
                    }
                )
            })
            .map(|d| d.id)
    }

    #[test]
    fn a_preset_is_found_whatever_case_it_is_asked_for_in() {
        let app = app_with(vec![setting(&some_toggle(), true, None)]);
        for name in ["QOL", "qol", "QoL"] {
            let (kept, _) = preset_settings(&app, name, true).expect(name);
            assert_eq!(kept.len(), 1);
        }
    }

    #[test]
    fn an_unknown_preset_name_lists_what_is_saved() {
        let app = app_with(vec![setting(&some_toggle(), true, None)]);
        let err = preset_settings(&app, "nope", true).expect_err("should fail");
        assert!(err.contains("QOL"), "{err}");
    }

    #[test]
    fn an_empty_settings_file_says_so_rather_than_listing_nothing() {
        let app = settings::AppSettings::default();
        let err = preset_settings(&app, "QOL", true).expect_err("should fail");
        assert!(err.contains("none saved"), "{err}");
    }

    /// The failure this exists for: a preset naming a tweak this build does not have used to fail
    /// the whole apply here while the desktop app applied everything else.
    #[test]
    fn an_id_this_build_does_not_know_is_dropped_not_fatal() {
        let known = some_toggle();
        let app = app_with(vec![
            setting("a_tweak_from_the_future", true, None),
            setting(&known, true, None),
        ]);
        let (kept, dropped) = preset_settings(&app, "QOL", true).expect("should resolve");
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].id, known);
        assert_eq!(dropped.len(), 1);
        assert!(
            dropped[0].contains("a_tweak_from_the_future"),
            "{dropped:?}"
        );
        // What survives has to translate, or dropping it bought nothing.
        rivals_core::pak_tweaks::edits_for_settings(&kept).expect("kept settings translate");
    }

    #[test]
    fn a_remove_only_tweak_held_off_is_dropped_not_fatal() {
        let Some(id) = a_remove_only() else {
            return;
        };
        let app = app_with(vec![
            setting(&id, false, None),
            setting(&some_toggle(), true, None),
        ]);
        let (kept, dropped) = preset_settings(&app, "QOL", true).expect("should resolve");
        assert!(!kept.iter().any(|s| s.id == id), "{kept:?}");
        assert!(dropped.iter().any(|d| d.contains(&id)), "{dropped:?}");
        rivals_core::pak_tweaks::edits_for_settings(&kept).expect("kept settings translate");
    }

    /// The same tweak held on is asked for, not dropped: only the off direction is impossible.
    #[test]
    fn a_remove_only_tweak_held_on_survives() {
        let Some(id) = a_remove_only() else {
            return;
        };
        let app = app_with(vec![setting(&id, true, None)]);
        let (kept, dropped) = preset_settings(&app, "QOL", true).expect("should resolve");
        assert_eq!(kept.len(), 1);
        assert!(dropped.is_empty(), "{dropped:?}");
    }

    /// A pak shipping only device profile files cannot hold an engine-section setting, so asking
    /// for one writes nothing. The desktop app drops these and says how many; without the same
    /// rule here the CLI reported them as applied.
    #[test]
    fn an_engine_section_tweak_is_dropped_when_the_pak_ships_no_engine_ini() {
        let engine_only = tweak_catalogue()
            .into_iter()
            .find(rivals_core::pak_tweaks::needs_engine_ini)
            .expect("the catalogue has an engine-section tweak");
        let plain = tweak_catalogue()
            .into_iter()
            .find(|d| !rivals_core::pak_tweaks::needs_engine_ini(d))
            .expect("and one that is not");

        let app = app_with(vec![
            setting(&engine_only.id, true, slider_value(&engine_only)),
            setting(&plain.id, true, slider_value(&plain)),
        ]);

        let (with_engine, dropped) = preset_settings(&app, "QOL", true).expect("resolves");
        assert_eq!(with_engine.len(), 2, "both apply to a pak that has one");
        assert!(dropped.is_empty(), "{dropped:?}");

        let (without, dropped) = preset_settings(&app, "QOL", false).expect("resolves");
        assert_eq!(without.len(), 1);
        assert_eq!(without[0].id, plain.id);
        assert!(
            dropped.iter().any(|d| d.contains(&engine_only.id)),
            "{dropped:?}"
        );
    }

    /// A value the catalogue will not take fails the whole preset rather than part of it.
    ///
    /// Detection reports what a pak actually says, unclamped, so a preset saved off a pak whose
    /// slider sits outside the catalogue range captures that value and can never be applied. No
    /// pak seen so far does this, and a partial apply would be worse than a loud refusal, so the
    /// behaviour is pinned rather than changed: the error has to name the entry to be fixable.
    #[test]
    fn an_out_of_range_slider_fails_the_preset_and_names_it() {
        let slider = tweak_catalogue()
            .into_iter()
            .find_map(|d| match d.kind {
                TweakKind::Slider { max, .. } => Some((d.id, max)),
                _ => None,
            })
            .expect("the catalogue has a slider");
        let (id, max) = slider;

        let app = app_with(vec![
            setting(&id, true, Some(&format!("{}", max + 1.0))),
            setting(&some_toggle(), true, None),
        ]);
        let (kept, dropped) = preset_settings(&app, "QOL", true).expect("resolves");
        assert!(
            dropped.is_empty(),
            "the value is in range as far as this knows"
        );

        let err = rivals_core::pak_tweaks::edits_for_settings(&kept).expect_err("should fail");
        assert!(err.contains("Nothing was applied"), "{err}");
        assert!(
            err.contains(&format!("{}", max)),
            "the range is named: {err}"
        );
    }

    /// Sliders take their value from the preset; everything else ignores it.
    fn slider_value(def: &rivals_core::tweaks::TweakDefinition) -> Option<&'static str> {
        match def.kind {
            TweakKind::Slider { .. } => Some("1"),
            _ => None,
        }
    }

    #[test]
    fn a_preset_with_no_settings_resolves_to_nothing() {
        let app = app_with(vec![]);
        let (kept, dropped) = preset_settings(&app, "QOL", true).expect("should resolve");
        assert!(kept.is_empty() && dropped.is_empty());
    }

    /// Every tweak in the catalogue, both directions, is what a saved preset actually holds: the
    /// app writes an entry per definition rather than only the ones that are on.
    #[test]
    fn a_preset_covering_the_whole_catalogue_resolves_and_translates() {
        for enabled in [true, false] {
            let all: Vec<TweakSetting> = tweak_catalogue()
                .into_iter()
                .map(|d| {
                    // A slider only takes a value inside its range; the app stores what it read.
                    let value = match &d.kind {
                        TweakKind::Slider { default_value, .. } => Some(format!("{default_value}")),
                        _ => None,
                    };
                    TweakSetting {
                        id: d.id,
                        enabled,
                        value,
                    }
                })
                .collect();
            let app = app_with(all);
            let (kept, dropped) = preset_settings(&app, "QOL", true).expect("should resolve");
            assert!(
                dropped.iter().all(|d| d.contains("only removes lines")),
                "nothing should drop for being unknown: {dropped:?}"
            );
            rivals_core::pak_tweaks::edits_for_settings(&kept)
                .unwrap_or_else(|e| panic!("enabled={enabled}: {e}"));
        }
    }
}

/// Arguments that start with a minus sign: import indices, negative numbers and comma lists of
/// them have to reach their option rather than read as flags.
#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod parse_tests {
    use super::*;
    use clap::CommandFactory;

    fn asset(args: &[&str]) -> AssetCmd {
        let argv = [&["rivals-cli", "asset"][..], args].concat();
        match Cli::try_parse_from(argv).expect("should parse").command {
            Command::Asset(cmd) => cmd,
            _ => panic!("not an asset command"),
        }
    }

    const PACKAGE: [&str; 3] = ["/Game/Thing", "--from", "c.utoc"];

    fn parses(args: &[&str]) -> bool {
        let argv = [&["rivals-cli", "asset"][..], args].concat();
        Cli::try_parse_from(argv).is_ok()
    }

    /// A save drops what it orphaned unless told to keep it, and the flags that used to ask for the
    /// drop are gone.
    #[test]
    fn saves_drop_what_they_orphan_unless_told_to_keep_it() {
        let reset = |extra: &[&str]| match asset(
            &[&["reset-export"][..], &PACKAGE, &["--export", "1"], extra].concat(),
        ) {
            AssetCmd::ResetExport(args) => args.write.save,
            _ => panic!("not reset-export"),
        };
        let plain = reset(&[]);
        assert!(!plain.keep_orphaned_imports && !plain.keep_unused_locals);
        let kept = reset(&["--keep-orphaned-imports", "--keep-unused-locals"]);
        assert!(kept.keep_orphaned_imports && kept.keep_unused_locals);
        assert!(!parses(&[
            "reset-export",
            "/Game/T",
            "--export",
            "1",
            "--drop-unused-locals"
        ]));
    }

    /// The asset comes first, read from `--from` when given, and the old ways of naming it are
    /// gone.
    #[test]
    fn the_asset_comes_first_and_its_source_is_optional() {
        match asset(&["info", "/Game/Thing"]) {
            AssetCmd::Info(args) => {
                assert_eq!(args.asset.asset, "/Game/Thing");
                assert!(args.asset.from.is_none());
            }
            _ => panic!("not info"),
        }
        match asset(&["dump", "/Game/Thing", "--from", "game", "--export", "Table"]) {
            AssetCmd::Dump(args) => {
                assert_eq!(args.asset.from.as_deref(), Some("game"));
                assert_eq!(args.export.as_deref(), Some("Table"));
            }
            _ => panic!("not dump"),
        }
        assert!(!parses(&["info"]), "no asset");
        for gone in [
            &["info", "--container", "c.utoc", "--entry", "e.uasset"][..],
            &["info", "--file", "x.uasset"],
            &["info", "/Game/T", "--layer"],
            &["info", "/Game/T", "--target", "pak"],
            &[
                "set",
                "/Game/T",
                "--export",
                "T",
                "--path",
                "A",
                "--mod-name",
                "M",
            ],
            &[
                "set",
                "/Game/T",
                "--export",
                "T",
                "--path",
                "A",
                "--replace",
            ],
        ] {
            assert!(!parses(gone), "{gone:?} is gone");
        }
    }

    /// The save flags are a save's own: a command that only reads takes none of them.
    #[test]
    fn only_a_save_takes_the_save_flags() {
        match asset(&[
            "set",
            "/Game/T",
            "--export",
            "T",
            "--path",
            "A",
            "--value",
            "1",
            "--into",
            "M",
            "--overwrite",
            "--format",
            "pak",
            "--as",
            "/Game/New",
            "--allow-missing",
        ]) {
            AssetCmd::Set(args) => {
                let write = &args.write;
                assert_eq!(write.save.into.as_deref(), Some("M"));
                assert!(write.save.overwrite && write.save.allow_missing);
                assert!(matches!(write.save.format, Some(FormatArg::Pak)));
                assert_eq!(write.save_as.as_deref(), Some("/Game/New"));
            }
            _ => panic!("not set"),
        }
        for read in ["info", "dump", "table", "imports"] {
            assert!(
                !parses(&[read, "/Game/T", "--into", "M"]),
                "{read} saves nothing"
            );
            assert!(
                !parses(&[read, "/Game/T", "--allow-unchecked"]),
                "{read} saves nothing"
            );
        }
        assert!(
            !parses(&[
                "set",
                "/Game/T",
                "--export",
                "T",
                "--path",
                "A",
                "--keep-object-names"
            ]),
            "--keep-object-names goes with --as"
        );
    }

    /// Exports are named by index or by name, and the offset `hex` starts at is `--offset`.
    #[test]
    fn exports_are_named_by_index_or_name() {
        match asset(&[
            "hex",
            "/Game/T",
            "--export",
            "Default__T_C",
            "--offset",
            "0x40",
        ]) {
            AssetCmd::Hex(args) => {
                assert_eq!(args.export, "Default__T_C");
                assert_eq!(args.offset, Some(0x40));
            }
            _ => panic!("not hex"),
        }
        match asset(&[
            "remove-export",
            "/Game/T",
            "--export",
            "3",
            "--export",
            "Widget",
        ]) {
            AssetCmd::RemoveExport(args) => assert_eq!(args.exports, ["3", "Widget"]),
            _ => panic!("not remove-export"),
        }
    }

    /// The commands that only change something start with a verb, their guards say what they
    /// guard, and every fixed set of words is a choice `--help` lists.
    #[test]
    fn edits_start_with_their_verb_and_take_typed_choices() {
        for (old, new) in [
            ("script-set", "edit-script"),
            ("script-widen", "widen-script"),
            ("script-assemble", "assemble-script"),
            ("import", "edit-imports"),
            ("row", "edit-row"),
            ("strings", "edit-strings"),
            ("keys", "edit-keys"),
            ("export-edit", "edit-export"),
            ("synth-check", "check-structs"),
        ] {
            let help = |name: &str| {
                parses(&[name, "--help"]) || {
                    let argv = ["rivals-cli", "asset", name, "--help"];
                    matches!(
                        Cli::try_parse_from(argv).map_err(|e| e.kind()),
                        Err(clap::error::ErrorKind::DisplayHelp)
                    )
                }
            };
            assert!(help(new), "{new} is there");
            assert!(!help(old), "{old} is gone");
        }
        match asset(&[
            "edit-row",
            "/Game/T",
            "--export",
            "Table",
            "--op",
            "duplicate",
            "--row",
            "A",
            "--name",
            "B",
            "--position",
            "0",
        ]) {
            AssetCmd::EditRow(args) => {
                assert!(args.op == RowOpArg::Duplicate && args.position == Some(0));
            }
            _ => panic!("not edit-row"),
        }
        assert!(!parses(&[
            "edit-row", "/Game/T", "--export", "Table", "--op", "copy", "--row", "A"
        ]));
        assert!(!parses(&[
            "edit-row", "/Game/T", "--export", "Table", "--op", "add", "--row", "A", "--at", "0"
        ]));
        match asset(&[
            "edit-imports",
            "/Game/T",
            "--path",
            "/Game/X.X",
            "--class",
            "/Script/Engine.StaticMesh",
        ]) {
            AssetCmd::EditImports(args) => {
                assert_eq!(args.class.as_deref(), Some("/Script/Engine.StaticMesh"));
            }
            _ => panic!("not edit-imports"),
        }
        assert!(!parses(&[
            "edit-imports",
            "/Game/T",
            "--path",
            "/Game/X.X",
            "--class-package",
            "/Script/Engine",
            "--class-name",
            "StaticMesh"
        ]));
        match asset(&[
            "edit-export",
            "/Game/T",
            "--export",
            "W",
            "--to",
            "W2",
            "--set-flag",
            "Public",
            "--set-flag",
            "Standalone",
        ]) {
            AssetCmd::EditExport(args) => {
                assert_eq!(args.rename.as_deref(), Some("W2"));
                assert_eq!(args.set_flag, ["Public", "Standalone"]);
            }
            _ => panic!("not edit-export"),
        }
        assert!(!parses(&[
            "edit-export",
            "/Game/T",
            "--export",
            "W",
            "--rename",
            "W2"
        ]));
        assert!(matches!(
            asset(&[
                "rename-member", "/Game/T", "--member", "A", "--to", "B", "--possible", "follow"
            ]),
            AssetCmd::RenameMember(args) if args.possible == PossibleArg::Follow
        ));
        assert!(!parses(&[
            "rename-member",
            "/Game/T",
            "--member",
            "A",
            "--to",
            "B",
            "--possible",
            "maybe"
        ]));
        assert!(matches!(
            asset(&["script", "/Game/T", "--export", "F", "--as-text"]),
            AssetCmd::Script(args) if args.as_text
        ));
        assert!(!parses(&["script", "/Game/T", "--export", "F", "--text"]));
        assert!(matches!(
            asset(&["vm", "/Game/NS_T", "--export", "SpawnScript", "--constants"]),
            AssetCmd::Vm(args) if args.export.as_deref() == Some("SpawnScript") && args.constants
        ));
        assert!(matches!(
            asset(&["vm", "/Game/NS_T"]),
            AssetCmd::Vm(args) if args.export.is_none() && args.trace.is_none()
        ));
        assert!(matches!(
            asset(&["vm", "/Game/NS_T", "--export", "1", "--trace", "Particles.Color"]),
            AssetCmd::Vm(args) if args.trace.as_deref() == Some("Particles.Color")
        ));
        assert!(matches!(
            asset(&["vm", "/Game/NS_T", "--export", "1", "--trace"]),
            AssetCmd::Vm(args) if args.trace.as_deref() == Some("")
        ));
        assert!(matches!(
            asset(&[
                "edit-vm", "/Game/NS_T", "--export", "SpawnScript_1", "--at", "0x00BD",
                "--operand", "0", "--value", "-0.5", "--into", "M", "--dry-run"
            ]),
            AssetCmd::EditVm(args) if args.at == 0xBD && args.value == "-0.5" && !args.retarget
        ));
        match asset(&[
            "edit-script",
            "/Game/T",
            "--export",
            "F",
            "--statement",
            "0x10",
            "--object-const",
            "1",
            "--value",
            "None",
        ]) {
            AssetCmd::EditScript(args) => assert_eq!(args.object, Some(1)),
            _ => panic!("not edit-script"),
        }
        assert!(matches!(
            asset(&["duplicate-export", "/Game/T", "--export", "A", "--name", "B", "--level", "L"]),
            AssetCmd::DuplicateExport(args) if args.level.as_deref() == Some("L")
        ));
        assert!(matches!(
            asset(&["trace", "/Game/T", "--export", "A"]),
            AssetCmd::Trace(_)
        ));
        assert!(!parses(&["trace", "/Game/T", "--inherited"]));
        assert!(matches!(
            asset(&["check-structs", "M", "--filter", "UI"]),
            AssetCmd::CheckStructs(args) if args.source == "M"
        ));
        assert!(!parses(&["check-structs", "M", "--text-check"]));
        match asset(&["search", "Delay", "--filter", "UI", "--filter", "Data"]) {
            AssetCmd::Search(args) => assert_eq!(args.filter, ["UI", "Data"]),
            _ => panic!("not search"),
        }
    }

    /// Bulk data is listed with neither `--out` nor `--in`, saved with `--out` and replaced with
    /// `--in`, never both.
    #[test]
    fn bulk_data_is_listed_saved_or_replaced_by_its_flags() {
        assert!(matches!(
            asset(&["bulk", "/Game/T"]),
            AssetCmd::Bulk(args) if args.out.is_none() && args.input.is_none()
        ));
        assert!(parses(&[
            "bulk",
            "/Game/T",
            "--resource",
            "0",
            "--out",
            "a.bin"
        ]));
        assert!(parses(&[
            "bulk",
            "/Game/T",
            "--resource",
            "0",
            "--in",
            "a.bin",
            "--dry-run"
        ]));
        assert!(!parses(&[
            "bulk",
            "/Game/T",
            "--resource",
            "0",
            "--out",
            "a",
            "--in",
            "b"
        ]));
        assert!(
            !parses(&["bulk", "/Game/T", "--resource", "0"]),
            "a resource for nothing"
        );
        assert!(!parses(&["bulk", "/Game/T", "--op", "list"]));
    }

    /// A listing, a sweep and an audit name their source first; apply takes its files.
    #[test]
    fn sources_and_files_come_first() {
        assert!(matches!(asset(&["list", "game"]), AssetCmd::List(args) if args.source == "game"));
        match asset(&[
            "sweep",
            "pakchunk0-Windows",
            "--set",
            "A=1",
            "--into",
            "M",
            "--overwrite",
        ]) {
            AssetCmd::Sweep(args) => {
                assert_eq!(args.source, "pakchunk0-Windows");
                assert!(args.overwrite);
            }
            _ => panic!("not sweep"),
        }
        assert!(matches!(asset(&["audit", "M"]), AssetCmd::Audit(args) if args.source.is_some()));
        assert!(parses(&["audit", "game", "--text-check"]));
        assert!(parses(&["sweep", "game", "--set", "A=1"]));
        assert!(parses(&["check-structs", "game"]));
        assert!(!parses(&["audit", "M", "--all"]));
        match asset(&["apply", "a.json", "b.json", "--into", "M", "--dry-run"]) {
            AssetCmd::Apply(args) => {
                assert_eq!(args.files, ["a.json", "b.json"]);
                assert!(args.save.dry_run);
            }
            _ => panic!("not apply"),
        }
        assert!(!parses(&["apply", "--edits", "a.json"]));
        assert!(matches!(
            asset(&["revert", "/Game/T", "--from", "M", "--dry-run"]),
            AssetCmd::Revert(args) if args.dry_run && args.from.as_deref() == Some("M")
        ));
        assert!(matches!(
            asset(&[
                "copy-export",
                "/Game/T",
                "--source",
                "/Game/S",
                "--source-export",
                "Thing"
            ]),
            AssetCmd::CopyExport(args) if args.source_export == "Thing"
        ));
        assert!(matches!(
            asset(&["importers", "/Game/T"]),
            AssetCmd::Importers(_)
        ));
    }

    fn deps(extra: &[&str]) -> DepsArgs {
        let args = [&["deps"][..], &PACKAGE, &["--export", "19"], extra].concat();
        match asset(&args) {
            AssetCmd::Deps(args) => args,
            _ => panic!("not deps"),
        }
    }

    fn run(list: &Option<RunList>) -> Option<Vec<i32>> {
        list.as_ref().map(|run| run.0.clone())
    }

    #[test]
    fn the_command_line_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn a_dependency_run_takes_negative_indices_in_one_list() {
        let args = deps(&["--sbc", "-41,-11", "--cbs", "6,-39", "--dry-run"]);
        assert_eq!(run(&args.sbc), Some(vec![-41, -11]));
        assert_eq!(run(&args.cbs), Some(vec![6, -39]));
        assert!(
            args.write.save.dry_run,
            "the flag after a list stays a flag"
        );
        assert_eq!(run(&args.sbs), None);
    }

    #[test]
    fn an_empty_dependency_run_is_given_with_an_equals_sign_or_at_the_end() {
        assert_eq!(run(&deps(&["--cbc="]).cbc), Some(vec![]));
        assert_eq!(run(&deps(&["--dry-run", "--cbc"]).cbc), Some(vec![]));
        let bad = [
            &["rivals-cli", "asset", "deps"][..],
            &PACKAGE,
            &["--export", "1", "--sbs", "a,-2"],
        ];
        assert!(Cli::try_parse_from(bad.concat()).is_err());
    }

    /// A dry run is there to be asked for on a value edit and on a retarget, the two edits people
    /// most want to see before they land, and a value starting with a minus does not swallow it.
    #[test]
    fn value_edits_and_retargets_take_a_dry_run() {
        let set = [
            &["set"][..],
            &PACKAGE,
            &[
                "--offset",
                "8",
                "--expect-kind",
                "int",
                "--expect-name",
                "N",
                "--value",
                "-5",
                "--dry-run",
            ],
        ];
        match asset(&set.concat()) {
            AssetCmd::Set(args) => {
                assert_eq!(args.value, "-5");
                assert!(args.write.save.dry_run);
            }
            _ => panic!("not set"),
        }
        let retarget = [
            &["edit-imports"][..],
            &PACKAGE,
            &[
                "--import",
                "-40",
                "--path",
                "/Engine/BasicShapes/Cone.Cone",
                "--dry-run",
            ],
        ];
        match asset(&retarget.concat()) {
            AssetCmd::EditImports(args) => {
                assert_eq!(args.import, Some(-40));
                assert!(args.write.save.dry_run);
            }
            _ => panic!("not edit-imports"),
        }
    }

    fn value_set(extra: &[&str]) -> AssetSetArgs {
        let args = [
            &["set"][..],
            &PACKAGE,
            &[
                "--offset",
                "8",
                "--expect-kind",
                "array",
                "--expect-name",
                "N",
            ],
            extra,
        ]
        .concat();
        match asset(&args) {
            AssetCmd::Set(args) => *args,
            _ => panic!("not set"),
        }
    }

    /// An op `asset set` does not know does not parse, and an element op without the element or a
    /// whole-value op given one is refused.
    #[test]
    fn a_value_op_is_one_it_knows_with_the_index_it_needs() {
        use rivals_uasset::EditOp;
        let op = |extra: &[&str]| value_op(&value_set(extra));
        assert!(matches!(
            op(&["--op", "remove", "--index", "2"]),
            Ok(EditOp::Remove { index: 2 })
        ));
        assert!(matches!(op(&["--value", "4"]), Ok(EditOp::Set { text }) if text == "4"));
        let refused = |extra: &[&str], says: &str| {
            let error = op(extra).expect_err("refused");
            assert!(error.contains(says), "{error}");
        };
        assert!(!parses(&[
            "set",
            "/Game/T",
            "--offset",
            "8",
            "--expect-kind",
            "array",
            "--expect-name",
            "N",
            "--op",
            "delete"
        ]));
        refused(&["--op", "set-element", "--value", "4"], "needs --index");
        refused(&["--op", "remove"], "needs --index");
        refused(
            &["--op", "set", "--index", "2", "--value", "4"],
            "set-element",
        );
        refused(&["--op", "set-key", "--index", "2"], "needs --key");
        refused(&["--op", "reorder"], "needs --order");
        refused(&["--op", "set-raw"], "needs --hex");
        assert!(matches!(
            op(&["--op", "set-raw", "--hex", "0a 00 00 00"]),
            Ok(EditOp::SetRaw { hex }) if hex == "0a 00 00 00"
        ));
        assert!(matches!(
            op(&["--op", "set-key", "--index", "1", "--key", "-k"]),
            Ok(EditOp::SetKey { index: 1, text }) if text == "-k"
        ));
        assert!(matches!(
            op(&["--op", "reorder", "--order", "2,0,1"]),
            Ok(EditOp::Reorder { order }) if order == [2, 0, 1]
        ));
        assert!(matches!(
            op(&["--op", "insert", "--index", "0", "--key", "K"]),
            Ok(EditOp::Insert { index: 0, key: Some(key) }) if key == "K"
        ));
    }

    fn path_set(extra: &[&str]) -> Result<rivals_uasset::PathEdit, String> {
        let args = [&["set"][..], &PACKAGE, &["--export", "Table"], extra].concat();
        match asset(&args) {
            AssetCmd::Set(args) => path_edit(&args, args.path.as_deref().expect("a path")),
            _ => panic!("not set"),
        }
    }

    /// A value named by path needs no offset, kind or name, takes an element's index onto its
    /// path, and appends where an insert names no position.
    #[test]
    fn a_value_edit_is_named_by_path() {
        use rivals_uasset::PathOp;
        let edit = path_set(&["--row", "Row_A", "--path", "Damage", "--value", "5"]).expect("set");
        assert_eq!(edit.export, "Table");
        assert_eq!(edit.row.as_deref(), Some("Row_A"));
        assert_eq!(edit.op, PathOp::Set { text: "5".into() });
        let remove =
            path_set(&["--path", "Items", "--op", "remove", "--index", "3"]).expect("remove");
        assert_eq!(remove.path, "Items[3]");
        assert_eq!(remove.op, PathOp::Remove);
        let insert = path_set(&["--path", "Items", "--op", "insert"]).expect("insert");
        assert_eq!(insert.path, "Items");
        assert_eq!(
            insert.op,
            PathOp::Insert {
                index: None,
                key: None
            }
        );
        let keyed =
            path_set(&["--path", "Scores{A}", "--op", "set-key", "--key", "B"]).expect("key");
        assert_eq!(keyed.op, PathOp::SetKey { text: "B".into() });
        assert!(path_set(&["--path", "A..B"]).is_err());
        assert!(!parses(&[
            "set", "/Game/T", "--export", "Table", "--path", "A", "--op", "nope"
        ]));

        let conflicting = [
            &["rivals-cli", "asset", "set"][..],
            &PACKAGE,
            &["--export", "Table", "--path", "A", "--offset", "8"],
        ];
        assert!(Cli::try_parse_from(conflicting.concat()).is_err());
        let unplaced = [
            &["rivals-cli", "asset", "set"][..],
            &PACKAGE,
            &["--path", "A"],
        ];
        assert!(
            Cli::try_parse_from(unplaced.concat()).is_err(),
            "--path needs --export"
        );
        let neither = [
            &["rivals-cli", "asset", "set"][..],
            &PACKAGE,
            &["--value", "1"],
        ];
        assert!(
            Cli::try_parse_from(neither.concat()).is_err(),
            "an offset or a path"
        );
    }

    #[test]
    fn values_may_start_with_a_minus_sign() {
        let set = [
            &["set"][..],
            &PACKAGE,
            &[
                "--offset",
                "8",
                "--expect-kind",
                "int",
                "--expect-name",
                "N",
                "--value",
                "-5",
            ],
        ];
        match asset(&set.concat()) {
            AssetCmd::Set(args) => assert_eq!(args.value, "-5"),
            _ => panic!("not set"),
        }
        let keys = [
            &["edit-keys"][..],
            &PACKAGE,
            &[
                "--offset",
                "8",
                "--expect-name",
                "C",
                "--op",
                "add",
                "--time",
                "-600",
                "--value",
                "-0.5",
            ],
        ];
        match asset(&keys.concat()) {
            AssetCmd::EditKeys(args) => {
                assert_eq!(args.time, Some(-600));
                assert_eq!(args.value, Some(-0.5));
            }
            _ => panic!("not edit-keys"),
        }
        let script = [
            &["edit-script"][..],
            &PACKAGE,
            &["--export", "5", "--statement", "0x10", "--value", "-1,0,0"],
        ];
        match asset(&script.concat()) {
            AssetCmd::EditScript(args) => assert_eq!(args.value, "-1,0,0"),
            _ => panic!("not edit-script"),
        }
        let strings = [
            &["edit-strings"][..],
            &PACKAGE,
            &[
                "--export", "0", "--op", "add", "--key", "-k", "--text", "-dash",
            ],
        ];
        match asset(&strings.concat()) {
            AssetCmd::EditStrings(args) => {
                assert_eq!(args.key, "-k");
                assert_eq!(args.text.as_deref(), Some("-dash"));
            }
            _ => panic!("not edit-strings"),
        }
    }
}

/// Every `rivals-cli` line in the README's shell examples parses as the command line takes it, so an
/// example cannot fall behind a rename.
#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod readme_tests {
    use super::*;

    /// A line's words as a shell splits them, up to a comment or a redirect.
    fn words(line: &str) -> Vec<String> {
        let mut words = Vec::new();
        let mut word = String::new();
        let (mut quoted, mut started) = (false, false);
        for c in line.chars() {
            match c {
                '"' => {
                    quoted = !quoted;
                    started = true;
                }
                '#' | '>' if !quoted && !started => break,
                c if c.is_whitespace() && !quoted => {
                    if started {
                        words.push(std::mem::take(&mut word));
                        started = false;
                    }
                }
                c => {
                    word.push(c);
                    started = true;
                }
            }
        }
        if started {
            words.push(word);
        }
        words
    }

    #[test]
    fn every_readme_example_parses() {
        let readme = include_str!("../../../README.md");
        let mut fence: Option<&str> = None;
        let mut checked = 0;
        for line in readme.lines() {
            if let Some(lang) = line.strip_prefix("```") {
                fence = match fence {
                    Some(_) => None,
                    None => Some(lang.trim()),
                };
                continue;
            }
            if fence != Some("bash") || !line.starts_with("rivals-cli ") {
                continue;
            }
            if let Err(error) = Cli::try_parse_from(words(line)) {
                panic!(
                    "the README's `{line}` does not parse:
{error}"
                );
            }
            checked += 1;
        }
        assert!(checked > 60, "only {checked} examples found");
    }
}

/// What v0.14.1 shipped parses as it did: `tweaks`, `ini` and the global flags.
#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod shipped_parse_tests {
    use super::*;

    fn cli(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from([&["rivals-cli"][..], args].concat())
    }

    #[test]
    fn tweaks_parse_as_shipped() {
        assert!(matches!(
            cli(&["tweaks", "list"]).unwrap().command,
            Command::Tweaks(TweaksCmd::List)
        ));
        match cli(&["tweaks", "status", "--pak", "M"]).unwrap().command {
            Command::Tweaks(TweaksCmd::Status(args)) => assert_eq!(args.pak, "M"),
            _ => panic!("not tweaks status"),
        }
        match cli(&[
            "tweaks",
            "apply",
            "--pak",
            "M",
            "--on",
            "a",
            "--off",
            "b",
            "--set",
            "c=2",
            "--dry-run",
        ])
        .unwrap()
        .command
        {
            Command::Tweaks(TweaksCmd::Apply(args)) => {
                assert_eq!(args.pak.pak, "M");
                assert_eq!((args.on.len(), args.off.len(), args.set.len()), (1, 1, 1));
                assert!(args.dry_run);
            }
            _ => panic!("not tweaks apply"),
        }
    }

    #[test]
    fn ini_parses_as_shipped() {
        assert!(matches!(
            cli(&["ini", "list", "--pak", "M"]).unwrap().command,
            Command::Ini(IniCmd::List(_))
        ));
        match cli(&["ini", "get", "--pak", "M", "--key", "r.Foo"])
            .unwrap()
            .command
        {
            Command::Ini(IniCmd::Get(args)) => assert_eq!(args.key.as_deref(), Some("r.Foo")),
            _ => panic!("not ini get"),
        }
        match cli(&["ini", "set", "--pak", "M", "r.Foo=1", "r.Bar=2"])
            .unwrap()
            .command
        {
            Command::Ini(IniCmd::Set(args)) => assert_eq!(args.assignments.len(), 2),
            _ => panic!("not ini set"),
        }
        match cli(&["ini", "unset", "--pak", "M", "r.Foo", "--dry-run"])
            .unwrap()
            .command
        {
            Command::Ini(IniCmd::Unset(args)) => assert!(args.dry_run),
            _ => panic!("not ini unset"),
        }
    }

    #[test]
    fn the_shipped_globals_go_before_or_after_the_command() {
        for argv in [
            &["--json", "--game-root", "G", "--force", "mods", "list"][..],
            &["mods", "list", "--json", "--game-root", "G", "--force"],
        ] {
            let parsed = cli(argv).unwrap();
            assert!(parsed.json && parsed.force);
            assert_eq!(parsed.game_root.as_deref(), Some("G"));
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod mods_parse_tests {
    use super::*;
    use rivals_core::pak::profile::{OodleLevel, PackCompression};

    fn mods(args: &[&str]) -> Result<ModsCmd, clap::Error> {
        let argv = [&["rivals-cli", "mods"][..], args].concat();
        Cli::try_parse_from(argv).map(|cli| match cli.command {
            Command::Mods(cmd) => cmd,
            _ => panic!("not a mods command"),
        })
    }

    /// `mods diff` takes the two mods as it takes any, and one alone is refused.
    #[test]
    fn a_diff_takes_two_mods() {
        let Ok(ModsCmd::Diff(args)) = mods(&[
            "diff",
            "A_9999999_P",
            "D:/Backup/A_9999999_P.utoc",
            "--brief",
        ]) else {
            panic!("not a diff");
        };
        assert_eq!(
            (args.a.as_str(), args.b.as_str(), args.brief),
            ("A_9999999_P", "D:/Backup/A_9999999_P.utoc", true)
        );
        assert!(mods(&["diff", "A_9999999_P"]).is_err());
    }

    fn pack(extra: &[&str]) -> Result<ModsPackArgs, clap::Error> {
        let args = [&["pack", "D", "--to", "M_9999999_P"][..], extra].concat();
        mods(&args).map(|cmd| match cmd {
            ModsCmd::Pack(args) => args,
            _ => panic!("not pack"),
        })
    }

    /// The level a write uses is the one asked for, else the app's, else normal; the compression
    /// is Oodle unless zlib is asked for.
    #[test]
    fn a_write_takes_its_level_from_the_flag_then_the_app() {
        let app = settings::AppSettings {
            mod_compression_level: Some(OodleLevel::Optimal3),
            ..Default::default()
        };
        let plain = pack(&[]).unwrap();
        assert_eq!(plain.compress.level(&app), OodleLevel::Optimal3);
        assert_eq!(
            plain.compress.level(&settings::AppSettings::default()),
            OodleLevel::Normal
        );
        assert_eq!(plain.compress.compression(), PackCompression::Oodle);
        let asked = pack(&["--oodle-level", "Fast", "--compression", "zlib"]).unwrap();
        assert_eq!(asked.compress.level(&app), OodleLevel::Fast);
        assert_eq!(
            asked.compress.io(&app, None).compression,
            PackCompression::Zlib
        );
        assert!(pack(&["--oodle-level", "hyper"]).is_err());
    }

    /// A pack names its folder first and says what format the mod is, and every form can be tried
    /// first.
    #[test]
    fn a_pack_names_its_folder_and_its_format() {
        let iostore = pack(&[]).unwrap();
        assert_eq!(iostore.dir, "D");
        assert!(iostore.format.is_none() && !iostore.dry_run);
        assert!(matches!(
            pack(&["--format", "pak", "--dry-run"]).unwrap(),
            ModsPackArgs {
                format: Some(FormatArg::Pak),
                dry_run: true,
                ..
            }
        ));
        assert!(pack(&["--overwrite"]).unwrap().overwrite);
        for gone in [&["--plain"][..], &["--replace"], &["--dir", "E"]] {
            assert!(pack(gone).is_err(), "{gone:?} is gone");
        }
    }

    /// The INI listing is still there under `--ini` and `--all-ini`, and every listing of `~mods`
    /// takes the same scan flags.
    #[test]
    fn the_listings_keep_their_flags() {
        match mods(&["list", "--all-ini", "--no-recursive"]).unwrap() {
            ModsCmd::List(args) => assert!(args.all_ini && !args.ini && args.scan.no_recursive),
            _ => panic!("not list"),
        }
        assert!(matches!(mods(&["list", "--ini"]).unwrap(), ModsCmd::List(args) if args.ini));
        match mods(&["conflicts", "--assets", "--recursive"]).unwrap() {
            ModsCmd::Conflicts(args) => assert!(args.assets && args.scan.recursive),
            _ => panic!("not conflicts"),
        }
        assert!(mods(&["list", "--recursive", "--no-recursive"]).is_err());
        let app = settings::AppSettings {
            recursive_mod_scan: true,
            ..Default::default()
        };
        assert!(RecursiveArgs::default().resolve(&app));
        let top = RecursiveArgs {
            no_recursive: true,
            ..Default::default()
        };
        assert!(!top.resolve(&app));
    }

    /// An install takes several files or folders, writes over a mod only when asked, can be tried
    /// first, and can go into a folder of its own, packing what it packs as asked.
    #[test]
    fn an_install_takes_files_and_overwrites_only_when_asked() {
        match mods(&["install", "a.zip", "b.pak"]).unwrap() {
            ModsCmd::Install(args) => {
                assert_eq!(args.files.len(), 2);
                assert!(!args.overwrite && !args.dry_run);
            }
            _ => panic!("not install"),
        }
        assert!(matches!(
            mods(&["install", "a.pak", "--overwrite", "--dry-run"]).unwrap(),
            ModsCmd::Install(args) if args.overwrite && args.dry_run
        ));
        assert!(mods(&["install"]).is_err(), "nothing to install");
        assert!(mods(&["install", "a.pak", "--replace"]).is_err());
        match mods(&[
            "install",
            "Downloads",
            "--to",
            "Fixed",
            "--compression",
            "zlib",
            "--oodle-level",
            "fast",
        ])
        .unwrap()
        {
            ModsCmd::Install(args) => {
                assert_eq!(args.to.as_deref(), Some(Path::new("Fixed")));
                assert!(matches!(
                    args.compress.compression,
                    Some(CompressionArg::Zlib)
                ));
                assert!(args.compress.oodle_level.is_some());
            }
            _ => panic!("not install"),
        }
    }

    /// A tidy names its mod first and can list what would go without writing.
    #[test]
    fn a_tidy_names_its_mod_and_can_only_list() {
        match mods(&["tidy", "M", "--dry-run"]).unwrap() {
            ModsCmd::Tidy(args) => {
                assert_eq!(args.target_mod.as_deref(), Some("M"));
                assert!(args.dry_run && !args.pak_only && !args.all);
            }
            _ => panic!("not tidy"),
        }
        assert!(mods(&["tidy"]).is_err(), "no mod named");
        // Every installed mod is taken only for the pak alone, and instead of a mod named.
        match mods(&["tidy", "--pak-only", "--all", "--dry-run"]).unwrap() {
            ModsCmd::Tidy(args) => {
                assert!(args.pak_only && args.all && args.target_mod.is_none());
            }
            _ => panic!("not tidy"),
        }
        assert!(mods(&["tidy", "M", "--pak-only"]).is_ok());
        assert!(
            mods(&["tidy", "--all"]).is_err(),
            "a whole tidy of every mod"
        );
        assert!(mods(&["tidy", "M", "--pak-only", "--all"]).is_err());
    }

    /// A command on one mod takes it first and only there: the old `--pak` and `--container` are
    /// gone.
    #[test]
    fn a_mod_is_named_first() {
        match mods(&["check", "M"]).unwrap() {
            ModsCmd::Check(args) => assert_eq!(args.target.get(), "M"),
            _ => panic!("not check"),
        }
        assert!(mods(&["check"]).is_err(), "no mod named");
        assert!(mods(&["check", "M", "N"]).is_err(), "two mods");
        for gone in ["--pak", "--container"] {
            assert!(mods(&["check", gone, "M"]).is_err(), "{gone} is gone");
            assert!(mods(&["extract", gone, "M", "--out", "O"]).is_err());
        }
        match mods(&["disable", "A", "B", "C"]).unwrap() {
            ModsCmd::Disable(args) => assert_eq!(args.mods.all(), ["A", "B", "C"]),
            _ => panic!("not disable"),
        }
        assert!(mods(&["disable", "A", "--pak", "B"]).is_err());
        assert!(mods(&["delete"]).is_err(), "no mod named");
        assert!(mods(&["enable", "A", "--all"]).is_err());
    }

    /// The shipped `paks list` still parses, as `mods list`.
    #[test]
    fn paks_is_still_the_mods_family() {
        let cli = Cli::try_parse_from(["rivals-cli", "paks", "list", "--ini"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Mods(ModsCmd::List(ModsListArgs { ini: true, .. }))
        ));
    }

    /// Switching, deleting and exporting take several mods, or all of them where that is safe,
    /// and each can be tried first.
    #[test]
    fn the_folder_commands_take_several_mods() {
        assert!(matches!(mods(&["enable", "--all"]).unwrap(), ModsCmd::Enable(args) if args.all));
        assert!(matches!(
            mods(&["disable", "A", "--dry-run"]).unwrap(),
            ModsCmd::Disable(args) if args.dry_run
        ));
        assert!(mods(&["enable"]).is_err(), "no mod named");
        assert!(
            mods(&["delete", "--all"]).is_err(),
            "deleting every mod is not offered"
        );
        match mods(&["delete", "A", "--dry-run"]).unwrap() {
            ModsCmd::Delete(args) => assert!(args.dry_run),
            _ => panic!("not delete"),
        }
        match mods(&["export-archive", "mods.7z", "--overwrite", "--dry-run"]).unwrap() {
            ModsCmd::ExportArchive(args) => {
                assert_eq!(args.out, Path::new("mods.7z"));
                assert!(args.overwrite && args.dry_run);
            }
            _ => panic!("not export-archive"),
        }
        assert!(mods(&["export-archive", "--out", "mods.7z"]).is_err());
    }

    /// A conversion names its mods or asks for all of them, and says what format they become.
    #[test]
    fn a_conversion_names_its_mods_and_their_format() {
        match mods(&["convert", "A", "B", "--format", "iostore", "--dry-run"]).unwrap() {
            ModsCmd::Convert(args) => {
                assert_eq!(args.mods.all(), ["A", "B"]);
                assert!(matches!(args.format, FormatArg::Iostore));
                assert!(args.dry_run);
            }
            _ => panic!("not convert"),
        }
        assert!(mods(&["convert", "--all", "--format", "pak"]).is_ok());
        assert!(
            mods(&["convert", "--format", "pak"]).is_err(),
            "no mod named"
        );
        assert!(mods(&["convert", "A"]).is_err(), "no format");
        assert!(
            mods(&["convert", "A", "--to", "pak"]).is_err(),
            "--to is gone"
        );
        assert!(mods(&["convert", "--all", "A", "--format", "pak"]).is_err());
        assert!(
            mods(&[
                "convert",
                "A",
                "--format",
                "iostore",
                "--obfuscate",
                "--no-obfuscate"
            ])
            .is_err()
        );
    }

    /// `mods extract` and `mods files` take the mod first and repeatable filters.
    #[test]
    fn extract_and_files_name_the_mod_and_their_filters() {
        match mods(&["extract", "M", "--out", "O", "--filter", "UI"]).unwrap() {
            ModsCmd::Extract(args) => {
                assert_eq!(args.from.get(), "M");
                assert_eq!(args.filter, ["UI"]);
            }
            _ => panic!("not extract"),
        }
        match mods(&["files", "M", "--filter", "Config", "--filter", "Movies"]).unwrap() {
            ModsCmd::Files(args) => assert_eq!(args.filter, ["Config", "Movies"]),
            _ => panic!("not files"),
        }
    }

    /// A copy goes to a new mod or into one already there, never both, and a folder packs into
    /// one already there without a name of its own to write over.
    #[test]
    fn a_copy_or_a_pack_goes_to_a_new_mod_or_into_one() {
        assert!(mods(&["copy", "A", "--to", "B", "--overwrite"]).is_ok());
        match mods(&[
            "copy", "A", "--into", "B", "--filter", "Hub", "--filter", "UI",
        ])
        .unwrap()
        {
            ModsCmd::Copy(args) => assert_eq!(args.filters, ["Hub", "UI"]),
            _ => panic!("not copy"),
        }
        assert!(mods(&["copy", "A", "--to", "B", "--into", "C"]).is_err());
        assert!(mods(&["copy", "A", "--into", "C", "--overwrite"]).is_err());
        assert!(
            mods(&["copy", "A", "--to", "B", "--only", "Hub"]).is_err(),
            "--only is gone"
        );
        assert!(mods(&["copy", "A"]).is_err(), "neither");
        assert!(mods(&["pack", "D", "--into", "M", "--dry-run"]).is_ok());
        // A pack written anew has nothing already in it to compress again.
        assert!(mods(&["pack", "D", "--into", "M", "--recompress"]).is_ok());
        assert!(mods(&["pack", "D", "--to", "M", "--recompress"]).is_err());
        assert!(mods(&["pack", "D", "--into", "M", "--format", "pak"]).is_err());
        assert!(mods(&["pack", "D", "--into", "M", "--overwrite"]).is_err());
    }

    /// Renaming and taking packages out name the mod first and can be tried first.
    #[test]
    fn a_rename_or_a_removal_can_be_tried() {
        assert!(matches!(
            mods(&["rename", "A", "--to", "B", "--dry-run"]).unwrap(),
            ModsCmd::Rename(args) if args.dry_run && args.to == "B"
        ));
        match mods(&["remove", "M", "--package", "/Game/A", "--filter", "UI"]).unwrap() {
            ModsCmd::Remove(args) => {
                assert_eq!(args.entries, ["/Game/A"]);
                assert_eq!(args.filters, ["UI"]);
            }
            _ => panic!("not remove"),
        }
        assert!(
            mods(&["remove", "M", "--entry", "/Game/A"]).is_err(),
            "--entry is gone"
        );
    }

    /// A search names the mod first and the text by `--find`; a replacement says what the text
    /// becomes by `--with`.
    #[test]
    fn a_search_or_a_replacement_names_its_text() {
        match mods(&["search", "M", "--find", "Hub", "--names"]).unwrap() {
            ModsCmd::Search(args) => {
                assert_eq!(args.target.get(), "M");
                assert_eq!(args.find, "Hub");
                assert!(args.names);
            }
            _ => panic!("not search"),
        }
        assert!(mods(&["search", "--pak", "M", "Hub"]).is_err());
        match mods(&[
            "replace", "M", "--find", "Old", "--with", "New", "--word", "--filter", "A",
            "--filter", "B",
        ])
        .unwrap()
        {
            ModsCmd::Replace(args) => {
                assert_eq!((args.find.as_str(), args.with.as_str()), ("Old", "New"));
                assert!(args.word);
                assert_eq!(args.filters, ["A", "B"]);
            }
            _ => panic!("not replace"),
        }
        assert!(mods(&["replace", "M", "--find", "Old", "--to", "New"]).is_err());
        assert!(mods(&["replace", "M", "--find", "O", "--with", "N", "--whole-word"]).is_err());
    }

    /// A move takes folders and single packages, and writes as every other mod command does.
    #[test]
    fn a_move_takes_packages_and_write_options() {
        match mods(&[
            "move",
            "M",
            "--folder",
            "/Game/A=/Game/B",
            "--package",
            "/Game/A/X=/Game/B/Y",
            "--compression",
            "zlib",
        ])
        .unwrap()
        {
            ModsCmd::Move(args) => {
                assert_eq!(args.maps, ["/Game/A/X=/Game/B/Y"]);
                assert!(matches!(
                    args.write.compress.compression,
                    Some(CompressionArg::Zlib)
                ));
            }
            _ => panic!("not move"),
        }
        assert!(mods(&["move", "M", "--map", "/Game/A/X=/Game/B/Y"]).is_err());
        assert!(matches!(
            mods(&["move", "M", "--keep-referencers", "--keep-object-names", "--allow-unchecked"])
                .unwrap(),
            ModsCmd::Move(args) if args.keep_referencers && args.keep_object_names && args.allow_unchecked
        ));
        assert!(matches!(
            mods(&["copy", "A", "--to", "B", "--allow-missing"]).unwrap(),
            ModsCmd::Copy(args) if args.allow_missing
        ));
    }

    /// `mods check` takes several forbidden texts, and the audit can be left out.
    #[test]
    fn a_check_takes_its_forbidden_texts() {
        match mods(&[
            "check",
            "M",
            "--forbid",
            "a",
            "--forbid",
            "b",
            "--skip-audit",
        ])
        .unwrap()
        {
            ModsCmd::Check(args) => {
                assert_eq!(args.forbid, vec!["a".to_string(), "b".to_string()]);
                assert!(args.skip_audit);
            }
            _ => panic!("not check"),
        }
        assert!(mods(&["check"]).is_err(), "no mod named");
    }
}
