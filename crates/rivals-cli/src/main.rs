//! Command-line front end for scripting Marvel Rivals pak INI edits and config tweaks.

#![deny(clippy::unwrap_used, clippy::expect_used)]

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

    /// Build on the mod's own copy of an asset where it already has one, instead of reading the
    /// source again. Edits must then be made against that copy, as `asset dump` of the mod shows it.
    /// `--replace`, which starts again from the source, is refused alongside it unless the source
    /// is the mod itself.
    #[arg(long, global = true)]
    layer: bool,

    /// Save edits that point an import or an object reference at a path neither the game nor an
    /// enabled mod has, for an asset something loaded alongside provides.
    #[arg(long, global = true)]
    allow_missing: bool,

    /// Save script edits that point an object constant or a call at something whose kind cannot
    /// be confirmed to match what the script held there.
    #[arg(long, global = true)]
    allow_unchecked: bool,

    /// Save an asset write under this package name instead of the asset's own, such as
    /// `/Game/Mods/MyThing/DA_Sword`: a new asset, or a replacement for the asset at that path.
    #[arg(long = "as", global = true, value_name = "PACKAGE")]
    save_as: Option<String>,

    /// With `--as`, keep the names of the objects named after the package instead of renaming the
    /// asset, and a Blueprint's class and default object, along with it.
    #[arg(long, global = true)]
    keep_object_names: bool,

    /// When a rename or a move is saved into a mod, leave the mod's other packages naming the old
    /// paths instead of pointing them at the new ones.
    #[arg(long, global = true)]
    keep_referencers: bool,

    /// The language texts are shown in, as the game names it (`en`, `ja`, `zh-hans`, ...).
    /// Defaults to the app's setting, then English.
    #[arg(long, global = true, value_name = "CULTURE")]
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
    Table(TableArgs),
    /// Print the byte range every property consumed, to locate a desync.
    Trace(DumpArgs),
    /// Print one export's raw bytes at the offsets the trace reports.
    Hex(HexArgs),
    /// Print the field records a class or struct export declares
    Fields(FieldsArgs),
    /// Disassemble the bytecode a function or class export stores.
    Script(ScriptArgs),
    /// Change something inside a function's bytecode, a literal, an object constant or a branch's
    /// condition, and write the result into a mod pak.
    ScriptSet(ScriptSetArgs),
    /// Rewrite every literal in a function, or in every function of the package, in its widest
    /// form with the same meaning, and write the result into a mod pak. Nothing behaves
    /// differently, but every offset after each literal moves, which checks in game that a
    /// script can change size safely.
    ScriptWiden(ScriptWidenArgs),
    /// Write a function's whole script anew from assembler text, as `asset script --text` prints
    /// it, and write the result into a mod pak. Whatever outside the function points into it
    /// follows the labels the text keeps.
    ScriptAssemble(ScriptAssembleArgs),
    /// Change a stored value and write the result into a mod pak.
    Set(AssetSetArgs),
    /// Set the same properties by name across every package a filter matches, in one mod.
    Sweep(SweepArgs),
    /// Point an import at another object, or add one, and write the result into a mod pak.
    Import(ImportArgs),
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
    /// Find where the game's scripts name something: a call, a delegate, a string, a name, an
    /// object or a variable. Reads every package the game loads, the enabled mods' included.
    Search(AssetSearchArgs),
    /// Rename an export, move it, or change the flags the loader reads it by.
    ExportEdit(ExportEditArgs),
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
    SynthCheck(AuditArgs),
}

#[derive(Args)]
struct ListArgs {
    /// IoStore container to enumerate. A patch declares every package of the game in its header,
    /// so listing one lists them all.
    #[arg(long, value_name = "PATH")]
    container: String,

    /// Keep only paths containing this text, case insensitive: a piece of the container path (`Marvel/UI/Setting`) or of the package name (`/Game/Marvel/UI/Setting`), with either slash.
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

    /// Show the values an export does not store as its archetypes hold them, in place of "not
    /// stored". Reads each archetype's package; native classes keep theirs in code.
    #[arg(long)]
    inherited: bool,
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
struct ScriptWidenArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// Export index of the one function to widen, as `asset info` prints it.
    #[arg(
        long,
        value_name = "N",
        conflicts_with = "all",
        required_unless_present = "all"
    )]
    export: Option<u32>,

    /// Widen every function in the package.
    #[arg(long)]
    all: bool,

    /// Mod pak to write into, created in `~mods` if it does not exist. Defaults to the name the
    /// desktop app last saved into, then to `AssetEdits`.
    #[arg(long, value_name = "NAME")]
    mod_name: Option<String>,

    /// Overwrite an edited copy of this asset that the mod pak already holds.
    #[arg(long)]
    replace: bool,

    /// Patch and verify, report what would change, and write nothing.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args)]
struct ScriptAssembleArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// Export index of the function, as `asset info` prints it.
    #[arg(long, value_name = "N")]
    export: u32,

    /// The text to assemble, as `asset script --text` prints it: UTF-8, or UTF-16 behind a byte
    /// order mark.
    #[arg(long, value_name = "FILE")]
    text_file: std::path::PathBuf,

    /// Mod pak to write into, created in `~mods` if it does not exist. Defaults to the name the
    /// desktop app last saved into, then to `AssetEdits`.
    #[arg(long, value_name = "NAME")]
    mod_name: Option<String>,

    /// Overwrite an edited copy of this asset that the mod pak already holds.
    #[arg(long)]
    replace: bool,

    /// Assemble, patch and verify, report what would change, and write nothing.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args)]
struct ScriptArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// Export index of the function or class, as `asset info` prints it.
    #[arg(long, value_name = "N")]
    export: u32,

    /// Under each statement, list everything in it an edit can address, by the offset it starts
    /// at: literals, object constants, texts, calls and conditions.
    #[arg(long, conflicts_with = "text")]
    expressions: bool,

    /// Print only the assembler text, which `asset script-assemble` takes back: no offsets, no
    /// header, labels where code is jumped to.
    #[arg(long)]
    text: bool,
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

    /// Export index of the class or struct, as `asset info` prints it.
    #[arg(long, value_name = "N")]
    export: u32,
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
    /// IoStore container to sweep. Every package it declares is a candidate.
    #[arg(long, value_name = "PATH")]
    container: String,

    /// Keep only packages whose path contains this text, case insensitive: a piece of the container path (`Marvel/UI/Setting`) or of the package name (`/Game/Marvel/UI/Setting`), with either slash.
    #[arg(long, value_name = "TEXT")]
    filter: Option<String>,

    /// Keep only packages holding an export of this class, such as `LegacyCameraShakePattern`.
    /// Narrows `--filter` rather than replacing it, and reads every package the filter allows.
    #[arg(long, value_name = "CLASS")]
    class: Option<String>,

    /// `Name=Value`: every property with that name, at any depth, takes that value. Over an array,
    /// `Name=[a, b, c]` makes it hold exactly those elements (quote one that holds a comma) and
    /// `Name[2]=Value` sets one element. Repeatable.
    #[arg(long = "set", value_name = "NAME=VALUE", required = true)]
    sets: Vec<String>,

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

    /// A value inside the struct or container `--name` names, as `A.B.C` from the struct down, or
    /// `[2].X` for field `X` of element 2. Any struct on the way that stores nothing yet is stored
    /// to hold it, and an element counts the container once the save's own inserts and removals
    /// have landed, all in one save.
    #[arg(long, value_name = "PATH", conflicts_with_all = ["op", "index"])]
    field: Option<String>,

    /// `set` writes `--value`; `clear` flags the property as zero; `store` gives an unset struct,
    /// container or reference its empty form; `unset` drops it so the inherited value applies;
    /// `set-element`, `insert` and `remove` act on the container element at `--index`, and
    /// `set-key` gives the map pair there the key `--key`. In a set or a map, `insert` takes the
    /// new element's key from `--key`, or from `--value`. `reorder` puts the elements in the
    /// order `--order` gives. `set-raw` replaces the value's bytes with `--hex`.
    #[arg(long, value_name = "OP", default_value = "set")]
    op: String,

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

    /// Mod pak to write into, created in `~mods` if it does not exist. Defaults to the name the
    /// desktop app last saved into, then to `AssetEdits`.
    #[arg(long, value_name = "NAME")]
    mod_name: Option<String>,

    /// Overwrite an edited copy of this asset that the mod pak already holds. Without it the
    /// command refuses, because the copy's earlier edits would be lost.
    #[arg(long)]
    replace: bool,

    /// Report what the save would change, checked the way the save checks it, and write nothing.
    #[arg(long)]
    dry_run: bool,
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

    /// Report what the retarget, add or removal would do, and write nothing.
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
struct RevertArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// Mod to take the asset out of. Defaults to the name the desktop app last saved into, then
    /// to `AssetEdits`.
    #[arg(long, value_name = "NAME")]
    mod_name: Option<String>,
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
struct AssetSearchArgs {
    /// Text to find, anywhere in a term, case aside.
    query: String,

    /// Search stored values too: strings, names, texts, object and asset paths, enumerators and
    /// delegates, at any depth, DataTable rows and StringTable entries included. Reads every
    /// package rather than only the ones holding functions, which takes minutes; pair it with
    /// `--filter`.
    #[arg(long)]
    values: bool,

    /// Keep only packages whose path contains this text, case insensitive: a piece of the
    /// container path (`Marvel/UI/Setting`) or of the package name (`/Game/Marvel/UI/Setting`).
    #[arg(long, value_name = "TEXT")]
    filter: Option<String>,

    /// Only terms of this kind. Repeatable.
    #[arg(long, value_name = "KIND")]
    kind: Vec<SearchKindArg>,

    /// Match the text only as a whole word, not inside a longer name: `Delay` then finds
    /// `KismetSystemLibrary:Delay` but not `DelayUntilNextTick`.
    #[arg(long)]
    word: bool,

    /// Leave the enabled mods out and read the base game alone.
    #[arg(long)]
    no_mods: bool,

    /// Stop after this many places are found; 0 for no limit.
    #[arg(long, value_name = "N", default_value_t = 5000)]
    limit: usize,
}

/// The kinds of term a search can be narrowed to.
#[derive(Clone, Copy, ValueEnum)]
enum SearchKindArg {
    Call,
    Delegate,
    String,
    Name,
    Object,
    Variable,
}

impl From<SearchKindArg> for rivals_core::mod_search::HitKind {
    fn from(kind: SearchKindArg) -> Self {
        match kind {
            SearchKindArg::Call => Self::Call,
            SearchKindArg::Delegate => Self::Delegate,
            SearchKindArg::String => Self::String,
            SearchKindArg::Name => Self::Name,
            SearchKindArg::Object => Self::Object,
            SearchKindArg::Variable => Self::Variable,
        }
    }
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
struct AddExportArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// The class, as an object path: `/Script/Module.Class`, or `/Game/Path/BP_Thing.BP_Thing_C`
    /// for a Blueprint.
    #[arg(long, value_name = "PATH")]
    class: String,

    /// The export to put it under, as `asset info` prints it. Omit for an object at the top of the
    /// package.
    #[arg(long, value_name = "N")]
    outer: Option<u32>,

    /// Its name, unique among what shares its outer.
    #[arg(long, value_name = "NAME")]
    name: String,

    /// Mod pak to write into, created in `~mods` if it does not exist. Defaults to the name the
    /// desktop app last saved into, then to `AssetEdits`.
    #[arg(long, value_name = "NAME")]
    mod_name: Option<String>,

    /// Overwrite an edited copy of this asset that the mod pak already holds.
    #[arg(long)]
    replace: bool,
}

#[derive(Args)]
struct RemoveComponentArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// The construction script node that builds the component, as `asset info` prints its export
    /// index (an `SCS_Node`).
    #[arg(long, value_name = "N")]
    node: u32,

    /// Take the components under it too, rather than hang them where it was.
    #[arg(long)]
    with_children: bool,

    /// Mod pak to write into, created in `~mods` if it does not exist. Defaults to the name the
    /// desktop app last saved into, then to `AssetEdits`.
    #[arg(long, value_name = "NAME")]
    mod_name: Option<String>,

    /// Overwrite an edited copy of this asset that the mod pak already holds.
    #[arg(long)]
    replace: bool,
}

#[derive(Args)]
struct AddComponentArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// The construction script node that builds the component to copy, as `asset info` prints its
    /// export index (an `SCS_Node`).
    #[arg(long, value_name = "N", required_unless_present = "from_parent")]
    node: Option<u32>,

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

    /// Mod pak to write into, created in `~mods` if it does not exist. Defaults to the name the
    /// desktop app last saved into, then to `AssetEdits`.
    #[arg(long, value_name = "NAME")]
    mod_name: Option<String>,

    /// Overwrite an edited copy of this asset that the mod pak already holds.
    #[arg(long)]
    replace: bool,
}

#[derive(Args)]
struct SaveAsArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// The package name to save it as, such as `/Game/Mods/MyThing/DA_Sword`.
    #[arg(long, value_name = "PACKAGE")]
    to: String,

    /// Mod pak to write into, created in `~mods` if it does not exist. Defaults to the name the
    /// desktop app last saved into, then to `AssetEdits`.
    #[arg(long, value_name = "NAME")]
    mod_name: Option<String>,

    /// Overwrite a copy of the asset at that path that the mod pak already holds.
    #[arg(long)]
    replace: bool,
}

#[derive(Args)]
struct RenamePackageArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// The package name to move it to, such as `/Game/Mods/MyThing/DA_Sword`.
    #[arg(long, value_name = "PACKAGE")]
    to: String,
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

    /// With `--compact`, say what would be dropped and write nothing.
    #[arg(long)]
    dry_run: bool,

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
    #[arg(long, value_name = "NAME", allow_hyphen_values = true)]
    row: String,

    /// The new name a copy or a rename takes.
    #[arg(long, value_name = "NAME", allow_hyphen_values = true)]
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
    #[arg(long, value_name = "KEY", allow_hyphen_values = true)]
    key: String,

    /// The new key, source string or tag, an added entry's source string, or a metadata value.
    #[arg(long, value_name = "TEXT", allow_hyphen_values = true)]
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
    #[arg(long, value_name = "FRAME", allow_hyphen_values = true)]
    time: Option<i32>,

    /// The value of the added key.
    #[arg(long, value_name = "NUMBER", allow_hyphen_values = true)]
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
#[command(allow_negative_numbers = true)]
struct ScriptSetArgs {
    #[command(flatten)]
    asset: AssetArgs,

    /// Export index of the function, as `asset info` prints it.
    #[arg(long, value_name = "N")]
    export: u32,

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
    #[arg(long, value_name = "K", conflicts_with_all = ["text", "call", "condition"])]
    object: Option<u32>,

    /// The K-th text in the statement, counted from 0 left to right. It takes UE's text literal
    /// syntax, `INVTEXT("...")`, `NSLOCTEXT("ns", "key", "...")` or `LOCTABLE("/Game/...", "key")`,
    /// or a plain string: a new source for a text that has one, `Table:Key` to repoint a string
    /// table text, and otherwise the words to show in every language.
    #[arg(long, value_name = "K", conflicts_with_all = ["call", "condition"])]
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

    /// Mod pak to write into, created in `~mods` if it does not exist. Defaults to the name the
    /// desktop app last saved into, then to `AssetEdits`.
    #[arg(long, value_name = "NAME")]
    mod_name: Option<String>,

    /// Overwrite an edited copy of this asset that the mod pak already holds.
    #[arg(long)]
    replace: bool,

    /// Patch and verify, report what would change, and write nothing.
    #[arg(long)]
    dry_run: bool,
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

    /// Walk every package the game loads, each from the container that wins: the newest patch
    /// declares them all. Takes about an hour.
    #[arg(long, conflicts_with_all = ["container", "dir"])]
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
    /// Convert the packages an IoStore mod ships back into loose .uasset/.uexp files.
    Extract(PaksExtractArgs),
    /// Summarise an IoStore mod: overrides, natives it calls, files and save slots it touches.
    Report(PaksReportArgs),
    /// Find where an IoStore mod's scripts and values name a string, function, variable or object.
    Search(PaksSearchArgs),
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

#[derive(Args)]
struct PaksExtractArgs {
    /// The mod: a .utoc or .pak path, or a name in `~mods`.
    #[arg(long)]
    container: String,

    /// Folder the package tree is written under.
    #[arg(long, value_name = "DIR")]
    out: PathBuf,

    /// Only packages whose path contains this text, case insensitive: a piece of the container path (`Marvel/UI/Setting`) or of the package name (`/Game/Marvel/UI/Setting`), with either slash. Repeatable.
    #[arg(long)]
    filter: Vec<String>,
}

#[derive(Args)]
struct PaksSearchArgs {
    /// The mod: a .utoc or .pak path, or a name in `~mods`.
    #[arg(long)]
    pak: String,

    /// Text to look for, ignoring case.
    query: String,
}

#[derive(Args)]
struct PaksReportArgs {
    /// The mod: a .utoc or .pak path, or a name in `~mods`.
    #[arg(long)]
    pak: String,
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
        Command::Paks(PaksCmd::List(a)) => paks_list(cli, &app, a),
        Command::Paks(PaksCmd::Extract(a)) => paks_extract(cli, &app, a),
        Command::Paks(PaksCmd::Report(a)) => paks_report(cli, &app, a),
        Command::Paks(PaksCmd::Search(a)) => paks_search(cli, &app, a),
        Command::Asset(AssetCmd::List(a)) => asset_list(cli, &app, a),
        Command::Asset(AssetCmd::Info(a)) => asset_info(cli, &app, a),
        Command::Asset(AssetCmd::Dump(a)) => asset_dump(cli, &app, a),
        Command::Asset(AssetCmd::Table(a)) => asset_table(cli, &app, a),
        Command::Asset(AssetCmd::Trace(a)) => asset_trace(cli, &app, a),
        Command::Asset(AssetCmd::Hex(a)) => asset_hex(cli, &app, a),
        Command::Asset(AssetCmd::Fields(a)) => asset_fields(cli, &app, a),
        Command::Asset(AssetCmd::Script(a)) => asset_script(cli, &app, a),
        Command::Asset(AssetCmd::ScriptSet(a)) => asset_script_set(cli, &app, a),
        Command::Asset(AssetCmd::ScriptWiden(a)) => asset_script_widen(cli, &app, a),
        Command::Asset(AssetCmd::ScriptAssemble(a)) => asset_script_assemble(cli, &app, a),
        Command::Asset(AssetCmd::Set(a)) => asset_set(cli, &app, a),
        Command::Asset(AssetCmd::Sweep(a)) => asset_sweep(cli, &app, a),
        Command::Asset(AssetCmd::Import(a)) => asset_import(cli, &app, a),
        Command::Asset(AssetCmd::RemoveExport(a)) => asset_remove_export(cli, &app, a),
        Command::Asset(AssetCmd::Revert(a)) => asset_revert(cli, &app, a),
        Command::Asset(AssetCmd::ResetExport(a)) => asset_reset_export(cli, &app, a),
        Command::Asset(AssetCmd::SaveAs(a)) => asset_save_as(cli, &app, a),
        Command::Asset(AssetCmd::AddExport(a)) => asset_add_export(cli, &app, a),
        Command::Asset(AssetCmd::AddComponent(a)) => asset_add_component(cli, &app, a),
        Command::Asset(AssetCmd::RemoveComponent(a)) => asset_remove_component(cli, &app, a),
        Command::Asset(AssetCmd::RenamePackage(a)) => asset_rename_package(cli, &app, a),
        Command::Asset(AssetCmd::Row(a)) => asset_row(cli, &app, a),
        Command::Asset(AssetCmd::Strings(a)) => asset_strings(cli, &app, a),
        Command::Asset(AssetCmd::Keys(a)) => asset_keys(cli, &app, a),
        Command::Asset(AssetCmd::Payload(a)) => asset_payload(cli, &app, a),
        Command::Asset(AssetCmd::DuplicateExport(a)) => asset_duplicate_export(cli, &app, a),
        Command::Asset(AssetCmd::Bulk(a)) => asset_bulk(cli, &app, a),
        Command::Asset(AssetCmd::Importers(a)) => asset_importers(cli, &app, a),
        Command::Asset(AssetCmd::Search(a)) => asset_search(cli, &app, a),
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
    let path = resolve::pak(&args.pak.pak, cli.game_root.as_deref(), app)?;

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

#[derive(Serialize)]
struct ExtractResult {
    out: String,
    extracted: Vec<String>,
    errors: Vec<String>,
    /// Imports each package still could not name, keyed by package path.
    unresolved: BTreeMap<String, Vec<String>>,
}

fn paks_extract(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &PaksExtractArgs,
) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let utoc = Path::new(&resolve::pak(
        &args.container,
        cli.game_root.as_deref(),
        app,
    )?)
    .with_extension("utoc");
    if !utoc.is_file() {
        return Err(format!("{} is not an IoStore mod", utoc.display()));
    }
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
        errors: result.errors,
        unresolved,
    };
    emit(cli, &report, || {
        outln!(
            "{} package(s) written under {}",
            report.extracted.len(),
            report.out
        );
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

fn paks_search(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &PaksSearchArgs,
) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let utoc =
        Path::new(&resolve::pak(&args.pak, cli.game_root.as_deref(), app)?).with_extension("utoc");
    if !utoc.is_file() {
        return Err(format!("{} is not an IoStore mod", utoc.display()));
    }
    let schema = rivals_core::mappings::resolve(cli.usmap.as_deref(), app.usmap_path.as_deref())
        .and_then(|path| rivals_core::mappings::load(&path))
        .ok();
    let result = rivals_core::mod_search::mod_search(&root, &utoc, schema.as_deref(), &args.query)?;
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

fn paks_report(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &PaksReportArgs,
) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let utoc =
        Path::new(&resolve::pak(&args.pak, cli.game_root.as_deref(), app)?).with_extension("utoc");
    if !utoc.is_file() {
        return Err(format!("{} is not an IoStore mod", utoc.display()));
    }
    let schema = rivals_core::mappings::resolve(cli.usmap.as_deref(), app.usmap_path.as_deref())
        .and_then(|path| rivals_core::mappings::load(&path))
        .ok();
    let report = rivals_core::mod_report::mod_report(&root, &utoc, schema.as_deref())?;
    emit(cli, &report, || {
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
        layer: cli.layer,
        allow_missing: cli.allow_missing,
        allow_unchecked: cli.allow_unchecked,
        save_as: cli.save_as.as_deref(),
        keep_object_names: cli.keep_object_names,
        keep_referencers: cli.keep_referencers,
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
    let mut request = asset_request(cli, app, &args.asset, &root);
    // What an export leaves out is only listed by the parse that declares every slot.
    request.declared |= args.inherited;
    let parsed = asset::dump(&request, args.export, &culture(cli, app))?;
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
        &asset_request(cli, app, &args.asset, &root),
        &culture(cli, app),
        args.row.as_deref(),
    )?;
    emit(cli, &report, || {
        asset::print_table(&report, &mut |line| outln!("{line}"));
    })
}

fn asset_trace(cli: &Cli, app: &settings::AppSettings, args: &DumpArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let entries = asset::trace(&asset_request(cli, app, &args.asset, &root), args.export)?;
    emit(cli, &entries, || {
        asset::print_trace(&entries, &mut |line| outln!("{line}"));
    })
}

fn asset_hex(cli: &Cli, app: &settings::AppSettings, args: &HexArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let rows = asset::hex(
        &asset_request(cli, app, &args.asset, &root),
        args.export,
        args.from,
    )?;
    emit(cli, &rows, || {
        asset::print_hex(&rows, &mut |line| outln!("{line}"));
    })
}

fn asset_fields(cli: &Cli, app: &settings::AppSettings, args: &FieldsArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let report = asset::fields(&asset_request(cli, app, &args.asset, &root), args.export)?;
    emit(cli, &report, || {
        asset::print_fields(&report, &mut |line| outln!("{line}"));
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
    #[arg(long, value_name = "N", conflicts_with = "outer_root")]
    outer: Option<u32>,

    /// Move the object to the package root, where the package's own asset sits.
    #[arg(long)]
    outer_root: bool,

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

    /// Go ahead although the plan carries warnings, such as packages still naming the old path.
    #[arg(long)]
    accept_warnings: bool,

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
    if args.outer.is_some() || args.outer_root {
        edits.push(rivals_uasset::ExportEdit::SetOuter {
            export,
            outer: args.outer,
        });
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

fn asset_script_widen(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &ScriptWidenArgs,
) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let request = asset_request(cli, app, &args.asset, &root);
    let export = if args.all { None } else { args.export };
    if args.dry_run {
        let applied = asset::preview_script_widen(&request, export)?;
        return emit(cli, &applied, || {
            for done in &applied {
                outln!("would set {}: {} -> {}", done.name, done.before, done.after);
            }
        });
    }
    if !cli.force && rivals_core::game_status::should_block_for_game() {
        return Err(rivals_core::game_status::game_running_error());
    }
    let message = asset::script_widen(
        &request,
        export,
        mod_name_of(app, args.mod_name.as_deref()),
        args.replace,
    )?;
    emit(cli, &message, || outln!("{message}"))
}

fn asset_script_assemble(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &ScriptAssembleArgs,
) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let request = asset_request(cli, app, &args.asset, &root);
    let bytes = std::fs::read(&args.text_file)
        .map_err(|e| format!("read {}: {e}", args.text_file.display()))?;
    let text = rivals_core::asset_edit::json::decode_text(&bytes)
        .map_err(|e| format!("{}: {e}", args.text_file.display()))?;
    let edit = rivals_uasset::ScriptTextEdit {
        export: args.export,
        text,
        was: None,
    };
    if args.dry_run {
        let preview = asset::preview_script_assemble(&request, edit)?;
        return emit(cli, &preview, || {
            for done in &preview.applied {
                outln!("would set {}: {} -> {}", done.name, done.before, done.after);
            }
            for note in &preview.notes {
                outln!("note: {note}");
            }
        });
    }
    if !cli.force && rivals_core::game_status::should_block_for_game() {
        return Err(rivals_core::game_status::game_running_error());
    }
    let message = asset::script_assemble(
        &request,
        edit,
        mod_name_of(app, args.mod_name.as_deref()),
        args.replace,
    )?;
    emit(cli, &message, || outln!("{message}"))
}

fn asset_script(cli: &Cli, app: &settings::AppSettings, args: &ScriptArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let report = asset::script(&asset_request(cli, app, &args.asset, &root), args.export)?;
    if args.text {
        // Exactly the text, so `> fn.txt` saves what `script-assemble` reads.
        return emit(cli, &report, || out!("{}", report.text));
    }
    emit(cli, &report, || {
        asset::print_script(&report, args.expressions, &mut |line| outln!("{line}"))
    })
}

fn asset_script_set(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &ScriptSetArgs,
) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let request = asset_request(cli, app, &args.asset, &root);
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
            let report = asset::script(&request, args.export)?;
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
        export: args.export,
        statement,
        constant: args.constant,
        at,
        value: args.value.clone(),
        was: None,
        ..Default::default()
    };
    if args.dry_run {
        let applied = asset::preview_script_set(&request, edit)?;
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
    if !cli.force && rivals_core::game_status::should_block_for_game() {
        return Err(rivals_core::game_status::game_running_error());
    }
    let message = asset::script_set(
        &request,
        edit,
        mod_name_of(app, args.mod_name.as_deref()),
        args.replace,
    )?;
    emit(cli, &message, || outln!("{message}"))
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
    /// Apply an edit file even where the package no longer matches what it was written against.
    #[arg(long)]
    allow_drift: bool,
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
    #[arg(long, value_name = "N")]
    into_outer: Option<u32>,

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
    let given =
        |run: &Option<RunList>, held: Vec<i32>| run.as_ref().map_or(held, |run| run.0.clone());
    let runs = rivals_uasset::Runs {
        serialize_before_serialize: given(&args.sbs, held.serialize_before_serialize),
        create_before_serialize: given(&args.cbs, held.create_before_serialize),
        serialize_before_create: given(&args.sbc, held.serialize_before_create),
        create_before_create: given(&args.cbc, held.create_before_create),
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
        DEFAULT_MOD_NAME,
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
            layer: cli.layer,
            allow_drift: args.allow_drift,
            allow_missing: cli.allow_missing,
            allow_unchecked: cli.allow_unchecked,
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
    if !args.dry_run && !cli.force && rivals_core::game_status::should_block_for_game() {
        return Err(rivals_core::game_status::game_running_error());
    }
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let request = asset_request(cli, app, &args.asset, &root);
    let mod_name = mod_name_of(app, args.mod_name.as_deref());
    let changes = match &args.field {
        Some(field) => rivals_uasset::PackageEdits {
            field_sets: vec![rivals_uasset::FieldSet {
                offset: args.offset,
                expect_name: args.name.clone(),
                expect_element: args.element,
                path: field.split('.').map(str::to_string).collect(),
                text: args.value.clone(),
            }],
            ..Default::default()
        },
        None => rivals_uasset::PackageEdits {
            values: vec![rivals_uasset::ValueEdit {
                offset: args.offset,
                expect_name: args.name.clone(),
                expect_element: args.element,
                expect_kind: args.kind.clone(),
                op: value_op(args)?,
            }],
            ..Default::default()
        },
    };
    if args.dry_run {
        let preview = asset::preview_edits(&request, mod_name, args.replace, changes)?;
        return emit(cli, &preview, || print_edit_preview(&preview));
    }
    let message = asset::set(&request, changes, mod_name, args.replace)?;
    emit(cli, &message, || outln!("{message}"))
}

/// The edit `--op` names. One it does not know, or one missing the element it acts on, is refused
/// rather than taken for a `set`.
fn value_op(args: &AssetSetArgs) -> Result<rivals_uasset::EditOp, String> {
    use rivals_uasset::EditOp;
    let index = || {
        args.index
            .ok_or_else(|| format!("--op {} needs --index", args.op))
    };
    let whole = |op: EditOp| match args.index {
        Some(_) => Err(format!(
            "--op {} acts on the whole value and takes no --index; set-element sets one element",
            args.op
        )),
        None => Ok(op),
    };
    match args.op.as_str() {
        "set" => whole(EditOp::Set {
            text: args.value.clone(),
        }),
        "clear" => whole(EditOp::Clear),
        "store" => whole(EditOp::Store),
        "unset" => whole(EditOp::Unset),
        "set-element" => Ok(EditOp::SetElement {
            index: index()?,
            text: args.value.clone(),
        }),
        "insert" => Ok(EditOp::Insert {
            index: index()?,
            key: args
                .key
                .clone()
                .or_else(|| (!args.value.is_empty()).then(|| args.value.clone())),
        }),
        "remove" => Ok(EditOp::Remove { index: index()? }),
        "set-key" => Ok(EditOp::SetKey {
            index: index()?,
            text: args.key.clone().ok_or("--op set-key needs --key")?,
        }),
        "reorder" => whole(EditOp::Reorder {
            order: args.order.clone().ok_or("--op reorder needs --order")?,
        }),
        "set-raw" => whole(EditOp::SetRaw {
            hex: args.hex.clone().ok_or("--op set-raw needs --hex")?,
        }),
        other => Err(format!(
            "--op {other} is not an edit this command makes: use set, clear, store, unset, \
             set-element, insert, remove, set-key, reorder or set-raw"
        )),
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
            layer: cli.layer,
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
    if !args.dry_run && !cli.force && rivals_core::game_status::should_block_for_game() {
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
    let mod_name = mod_name_of(app, args.mod_name.as_deref());
    if args.dry_run {
        let preview = asset::preview_import_edit(&request, edit, mod_name, args.replace)?;
        return emit(cli, &preview, || print_edit_preview(&preview));
    }
    let message = asset::import_edit(&request, edit, mod_name, args.replace)?;
    emit(cli, &message, || outln!("{message}"))
}

/// The mod pak an edit goes into: the one asked for, else the one the desktop app last used.
fn mod_name_of<'a>(app: &'a settings::AppSettings, explicit: Option<&'a str>) -> &'a str {
    explicit
        .or(app.asset_mod_name.as_deref())
        .unwrap_or(DEFAULT_MOD_NAME)
}

fn asset_revert(cli: &Cli, app: &settings::AppSettings, args: &RevertArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let mod_name = args
        .mod_name
        .as_deref()
        .or(app.asset_mod_name.as_deref())
        .unwrap_or(DEFAULT_MOD_NAME);
    let request = asset_request(cli, app, &args.asset, &root);
    let entry = rivals_core::asset_edit::save_entry(&rivals_core::asset_edit::AssetEditRequest {
        game_root: &root,
        container: request.container,
        entry: request.entry,
        kind: if args.asset.file.is_some() {
            rivals_core::asset::AssetSource::Loose
        } else {
            rivals_core::asset::AssetSource::Utoc
        },
        mod_name,
        changes: Default::default(),
    })?;
    let pak = rivals_core::asset_edit::mod_pak(&root, mod_name)?;
    if !cli.force && rivals_core::game_status::should_block_for_game() {
        return Err(rivals_core::game_status::game_running_error());
    }
    let outcome = rivals_core::asset_edit::revert_asset(&pak, &entry, &Default::default())?;
    let name = pak.file_name().unwrap_or_default().to_string_lossy();
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

fn asset_add_export(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &AddExportArgs,
) -> Result<(), String> {
    if !cli.force && rivals_core::game_status::should_block_for_game() {
        return Err(rivals_core::game_status::game_running_error());
    }
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let message = asset::add_export(
        &asset_request(cli, app, &args.asset, &root),
        rivals_uasset::AddExport {
            class: args.class.clone(),
            outer: args.outer,
            name: args.name.clone(),
            layout: None,
        },
        mod_name_of(app, args.mod_name.as_deref()),
        args.replace,
    )?;
    emit(cli, &message, || outln!("{message}"))
}

fn asset_add_component(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &AddComponentArgs,
) -> Result<(), String> {
    if !cli.force && rivals_core::game_status::should_block_for_game() {
        return Err(rivals_core::game_status::game_running_error());
    }
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let message = asset::add_component(
        &asset_request(cli, app, &args.asset, &root),
        rivals_uasset::AddComponent {
            node: args.node.unwrap_or_default(),
            name: args.name.clone(),
            with_children: args.with_children,
            from_parent: args.from_parent.clone(),
        },
        mod_name_of(app, args.mod_name.as_deref()),
        args.replace,
    )?;
    emit(cli, &message, || outln!("{message}"))
}

fn asset_remove_component(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &RemoveComponentArgs,
) -> Result<(), String> {
    if !cli.force && rivals_core::game_status::should_block_for_game() {
        return Err(rivals_core::game_status::game_running_error());
    }
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let message = asset::remove_component(
        &asset_request(cli, app, &args.asset, &root),
        rivals_uasset::RemoveComponent {
            node: args.node,
            with_children: args.with_children,
        },
        mod_name_of(app, args.mod_name.as_deref()),
        args.replace,
    )?;
    emit(cli, &message, || outln!("{message}"))
}

fn asset_save_as(cli: &Cli, app: &settings::AppSettings, args: &SaveAsArgs) -> Result<(), String> {
    if !cli.force && rivals_core::game_status::should_block_for_game() {
        return Err(rivals_core::game_status::game_running_error());
    }
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let message = asset::save_as(
        &asset_request(cli, app, &args.asset, &root),
        &args.to,
        mod_name_of(app, args.mod_name.as_deref()),
        args.replace,
    )?;
    emit(cli, &message, || outln!("{message}"))
}

fn asset_rename_package(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &RenamePackageArgs,
) -> Result<(), String> {
    if !cli.force && rivals_core::game_status::should_block_for_game() {
        return Err(rivals_core::game_status::game_running_error());
    }
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let message = asset::rename_package(&asset_request(cli, app, &args.asset, &root), &args.to)?;
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

fn asset_search(
    cli: &Cli,
    app: &settings::AppSettings,
    args: &AssetSearchArgs,
) -> Result<(), String> {
    use rivals_core::game_search::{GameSearch, SearchPhase, game_search};
    use std::io::IsTerminal;
    use std::sync::atomic::AtomicBool;

    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let mappings = rivals_core::mappings::resolve(cli.usmap.as_deref(), app.usmap_path.as_deref())
        .and_then(|path| rivals_core::mappings::load(&path))
        .map_err(|e| format!("reading the game's packages needs a .usmap mappings file: {e}"))?;
    let query = rivals_core::mod_search::Query::new(
        &args.query,
        args.kind.iter().map(|kind| (*kind).into()).collect(),
        args.values,
    )?
    .whole_word(args.word);
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
            filter: args.filter.as_deref(),
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
        let mut package = "";
        let mut packages = 0;
        for hit in &result.hits {
            if hit.package != package {
                package = &hit.package;
                packages += 1;
                match &hit.in_mod {
                    Some(name) => outln!("{package}  (in {name})"),
                    None => outln!("{package}"),
                }
            }
            let at = hit
                .offset
                .map_or_else(String::new, |o| format!(" 0x{o:04X}"));
            outln!("  {:<8} {}{at}  {}", hit.kind.name(), hit.export, hit.line);
        }
        let what = if args.values {
            "every package"
        } else {
            "only those holding functions"
        };
        outln!(
            "\n{} hit(s) in {packages} package(s); searched {} of {} packages ({what})",
            result.hits.len(),
            result.searched,
            result.listed
        );
        if result.truncated {
            outln!(
                "stopped at {} hits: raise --limit, or narrow the search with --filter or --kind",
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

fn asset_names(cli: &Cli, app: &settings::AppSettings, args: &NamesArgs) -> Result<(), String> {
    let root = resolve::game_root(cli.game_root.as_deref(), app)?;
    let request = asset_request(cli, app, &args.asset, &root);
    let mod_name = mod_name_of(app, args.mod_name.as_deref());
    if args.unused || args.compact {
        let unused = asset::unused_names(&request, mod_name)?;
        if !args.compact || args.dry_run {
            return emit(cli, &unused, || {
                for name in &unused {
                    outln!("{name}");
                }
                outln!("{} unused name(s)", unused.len());
            });
        }
        if unused.is_empty() {
            return Err("every name in the package is in use, so there is nothing to drop".into());
        }
        if !cli.force && rivals_core::game_status::should_block_for_game() {
            return Err(rivals_core::game_status::game_running_error());
        }
        let message = asset::compact_names(&request, mod_name, args.replace)?;
        let message = format!("{message}\ndropped {} unused name(s)", unused.len());
        return emit(cli, &message, || outln!("{message}"));
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
    let root = resolve::game_root(cli.game_root.as_deref(), app);
    let newest = match (&root, args.all) {
        (Ok(root), true) => Some(asset::newest_patch(root)?),
        (Err(e), true) => return Err(e.clone()),
        _ => None,
    };
    let report = match (newest.as_ref().or(args.container.as_ref()), &args.dir) {
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
            args.relocation_check,
            args.text_check,
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

    const PACKAGE: [&str; 4] = ["--container", "c.utoc", "--entry", "e.uasset"];

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
        assert!(args.dry_run, "the flag after a list stays a flag");
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
                "--kind",
                "int",
                "--name",
                "N",
                "--value",
                "-5",
                "--dry-run",
            ],
        ];
        match asset(&set.concat()) {
            AssetCmd::Set(args) => {
                assert_eq!(args.value, "-5");
                assert!(args.dry_run);
            }
            _ => panic!("not set"),
        }
        let retarget = [
            &["import"][..],
            &PACKAGE,
            &[
                "--index",
                "-40",
                "--path",
                "/Engine/BasicShapes/Cone.Cone",
                "--dry-run",
            ],
        ];
        match asset(&retarget.concat()) {
            AssetCmd::Import(args) => {
                assert_eq!(args.index, Some(-40));
                assert!(args.dry_run);
            }
            _ => panic!("not import"),
        }
    }

    fn value_set(extra: &[&str]) -> AssetSetArgs {
        let args = [
            &["set"][..],
            &PACKAGE,
            &["--offset", "8", "--kind", "array", "--name", "N"],
            extra,
        ]
        .concat();
        match asset(&args) {
            AssetCmd::Set(args) => args,
            _ => panic!("not set"),
        }
    }

    /// An op `asset set` does not know is refused rather than taken for a `set`, and so is an
    /// element op without the element or a whole-value op given one.
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
        refused(&["--op", "delete", "--index", "2"], "not an edit");
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

    #[test]
    fn values_may_start_with_a_minus_sign() {
        let set = [
            &["set"][..],
            &PACKAGE,
            &[
                "--offset", "8", "--kind", "int", "--name", "N", "--value", "-5",
            ],
        ];
        match asset(&set.concat()) {
            AssetCmd::Set(args) => assert_eq!(args.value, "-5"),
            _ => panic!("not set"),
        }
        let keys = [
            &["keys"][..],
            &PACKAGE,
            &[
                "--offset", "8", "--name", "C", "--op", "add", "--time", "-600", "--value", "-0.5",
            ],
        ];
        match asset(&keys.concat()) {
            AssetCmd::Keys(args) => {
                assert_eq!(args.time, Some(-600));
                assert_eq!(args.value, Some(-0.5));
            }
            _ => panic!("not keys"),
        }
        let script = [
            &["script-set"][..],
            &PACKAGE,
            &["--export", "5", "--statement", "0x10", "--value", "-1,0,0"],
        ];
        match asset(&script.concat()) {
            AssetCmd::ScriptSet(args) => assert_eq!(args.value, "-1,0,0"),
            _ => panic!("not script-set"),
        }
        let strings = [
            &["strings"][..],
            &PACKAGE,
            &[
                "--export", "0", "--op", "add", "--key", "-k", "--text", "-dash",
            ],
        ];
        match asset(&strings.concat()) {
            AssetCmd::Strings(args) => {
                assert_eq!(args.key, "-k");
                assert_eq!(args.text.as_deref(), Some("-dash"));
            }
            _ => panic!("not strings"),
        }
    }
}
