# Rivals Toolkit

Rivals Toolkit is a desktop app for working with Marvel Rivals configuration and mod files.
It combines a React frontend with a Tauri/Rust backend to provide:

- game install detection across launchers
- pak browsing, extraction, and repacking
- mod management (status, toggle, export, delete)
- pak-based INI and game settings tweak tooling
- `.uasset` inspection and editing: property trees, data tables, string tables, animation keys
- shader cache cleanup and game launch helpers

It also ships `rivals-cli`, a command-line tool over the same engine for scripting config tweaks and
pak INI edits. See [Command Line](#command-line).

Current platform support: Windows only.

## Tech Stack

- Frontend: React, TypeScript, Vite
- Backend: Tauri 2, Rust
- Layout: Cargo workspace. `crates/rivals-core` holds the pak and tweak engine and never depends on
  Tauri, which is what lets the CLI build without the frontend. `crates/rivals-uasset` holds the UE5
  package reader and writer underneath it.
- Tooling: ESLint, Prettier, Clippy, rustfmt

## Prerequisites

Install the following before running the project:

- Node.js 20+
- pnpm 9+
- Rust toolchain (stable) via rustup
- Microsoft C++ Build Tools (Windows)
- Microsoft Edge WebView2 Runtime (Windows)

Reference: https://tauri.app/start/prerequisites/

## Getting Started

1. Install JavaScript dependencies:

```bash
pnpm install
```

2. Start the desktop app in development mode:

```bash
pnpm tauri dev
```

Notes:

- `pnpm dev` starts only the Vite frontend.
- `pnpm tauri dev` runs the full desktop app (frontend + Rust backend).

## Linting And Formatting

Run all lint checks:

```bash
pnpm lint
```

Run lint checks individually:

```bash
pnpm lint:web
pnpm lint:rust
pnpm lint:rust:strict
```

Format all code:

```bash
pnpm format
```

Check formatting without changing files:

```bash
pnpm format:check
```

Run format checks individually:

```bash
pnpm format:web:check
pnpm format:rust:check
```

## Build

Build the frontend bundle:

```bash
pnpm build
```

Build desktop binaries:

```bash
pnpm tauri build
```

Build the CLI:

```bash
cargo build --release -p rivals-cli
```

## Tests

```bash
cargo test --workspace
```

`pnpm test` runs the frontend's tests with Vitest; `pnpm lint` and `pnpm exec tsc --noEmit` check it.

## Command Line

`rivals-cli.exe` ships in the release zip next to the desktop app and scripts the same config-tweak
and pak INI engine. It reads the game path the app saved, so `--game-root` is only needed when that
is unset or you want a different install. In a dev checkout, run it with
`cargo run -p rivals-cli -- <args>`.

```bash
rivals-cli paks list                                  # paks in ~mods with editable INIs
rivals-cli paks list --all-ini --no-recursive         # any INI, top level of ~mods only

rivals-cli tweaks list                                # the tweak catalogue
rivals-cli tweaks status --pak MyMod                  # which tweaks a pak has on
rivals-cli tweaks apply  --pak MyMod --on fix_dark_maps --off cas_sharpening
rivals-cli tweaks apply  --pak MyMod --set brightness=2.8 --dry-run

rivals-cli ini list  --pak MyMod                      # INI files the pak ships
rivals-cli ini get   --pak MyMod --key r.TonemapperGamma
rivals-cli ini get   --pak MyMod --entry Marvel/Config/DefaultEngine.ini
rivals-cli ini set   --pak MyMod r.Foo=1 r.Bar=2
rivals-cli ini set   --pak MyMod --file Marvel/Config/DefaultEngine.ini=./Engine.ini
rivals-cli ini unset --pak MyMod r.Foo
```

Reading and editing packages needs a `.usmap` mappings file for the game's build, since Marvel
Rivals ships unversioned properties. Point `--usmap` at the file or at a folder holding one, or set
it once in the app's settings.

Imports of native objects the game registers only at runtime (its `NePatchUtility` hot-patch plugin,
which Blueprint mod loaders call) are not in the game's script objects table, so a bundled list names
them; `--script-objects PATH` adds a text file of further object paths, one per line.

A save that adds an import, retargets one or points an object property at a new path first looks the
path up in the game and the enabled IoStore mods. One that points at nothing is refused, since the
game would load it as nothing; pass `--allow-missing` when a mod loaded alongside provides it.
`asset export-edit` likewise refuses a rename or a move while other packages or soft references may
still name the old path, until `--accept-warnings`.

A package's identity is its stored name, so any writing command can save under another one with
`--as /Game/Mods/MyThing/DA_Copy`: a new asset, or a replacement for the asset at that path. The
paths inside the package that name itself follow, and so do the objects named after it (the asset,
and a Blueprint's class and default object) unless `--keep-object-names` says otherwise. Functions
keep their names, since bytecode calls them by name. Paths in map values and bytecode strings
follow too; a map key, a bytecode string that would change length, and a payload the reader does
not follow keep the old path, and the save says which. `asset save-as` writes a copy with no other
change, and `asset rename-package` moves a package a mod added to another path inside that mod. A
level cannot be saved under another path, since its package name is written into its world.

Renaming or moving a package or an export inside a mod also points the mod's other packages at the
new path: the imports that name it are renamed in place, so a class stays the class it was, and the
soft paths and strings follow. Packages outside the mod are only listed. `--keep-referencers`
leaves the mod's other packages as they are.

`asset add-export` adds an object of a class, under an export or at the top of the package, storing
nothing so it takes every value from its class; its values are set with `asset set` afterwards. Only
a class whose objects are a property block and nothing more can be added this way: an actor, a
component, a texture, a mesh or a table writes more after its properties, and is refused. Whether a
class is abstract is not recorded anywhere the toolkit reads, and the game will not create an
object of an abstract class, so pick a concrete one.

`asset add-component` adds a component to a Blueprint by copying one its construction script
already builds: `--node` is the `SCS_Node` export, and the copy gets its own template, variable name
and guid, and hangs beside the original; `--with-children` copies the components under it too, each
under the next free name after its own. `--from-parent VARIABLE` copies a component the parent
Blueprint adds instead, as one of this Blueprint's own attached where the original is, with the
parent's values rather than this Blueprint's overrides of them. It appears on actors the game spawns from the Blueprint;
actors already placed in a map keep the components they were saved with. The class gains no
variable for it, which UE only notes. Whenever a save sets a property on a component template, the
property joins its node's changed property list, since a cooked Blueprint copies only the listed
properties onto the components it spawns: a struct with the fields the save changed, an array with
every element it holds, and a set or map on its own, which is copied whole. A field changed inside
a struct the list already names joins that struct's entries. A component a Blueprint inherits and
overrides keeps the list in its override record in step the same way.

`asset remove-component --node N` takes a component out of a Blueprint with its template. The
components under it take its place, or go with it with `--with-children`; the scene root goes only
with the components hanging from it. The class keeps the component's variable, which reads None, so
the save names the functions that read it.

`asset add-variable --name X --type T` adds a variable to a Blueprint class whose objects all live
in its own package. The class declares it after its own, and since a class's own properties come
first in every object's layout, each object of the class there is renumbered to match, its values as
they were and the new one unset until a value edit sets it. A class another package makes objects of
or derives from is refused, naming those packages: the import index finds them, and is built first
when it has not been. A type is written as a `local` line takes it (see below), and `--class N`
picks the class when the package holds more than one. A package whose classes its own records
describe otherwise than the mappings file does, as after this, is read with its own records. The
app's script view has New variable, for the class of the function shown. A function can be added to
a Blueprint class, and an existing one rewritten whole, as text: see below.

`asset add-enum-entry --display "Text"` adds an entry to a Blueprint enum, shown as the text given.
It goes in before `_MAX`, named `NewEnumerator<n>` as the editor names one and taking the value
`_MAX` had, and `_MAX` moves up one. The entries before keep their values, so what other packages
store of the enum reads as before, and the display name goes into the enum's `DisplayNameMap` in
the same save. An entry shown as text the enum shows already is left out, so a file adding one
applies again. When the new values take more bits, an enum another package sends over the network,
as a replicated property or a parameter of a function called across it, is refused, naming the
package, since the game's server reads it at the old width. Blueprints built against the enum keep
what they knew of it: a Switch on it sends the new entry to its default, and a loop over its
entries stops before it. The mappings do not know the entry, so setting a value of the enum to it
in another package asks for `--allow-unchecked`. `--export N` picks the enum when the package holds
more than one.

`asset add-field --name X --type T` adds a field to a Blueprint struct after its own, named as the
editor names a member: `X_<n>_<GUID>`, the guid the same each time the same field is added. Each
stored value of a struct says which of its fields it holds, so table rows, values and the struct's
own defaults read as before in every package, the new field unset until a value edit sets it. A
package read for a save into the mod holding the struct reads it with the mod's fields, so a row of
a table in the game can be given the new field in that mod. A field the struct has already, by name
and type, is left out, so a file adding one applies again. The type is one value of a native type,
as a `local` line takes it (see below); a container or a Blueprint type is refused, as is a struct
deriving from another. The flags the compiler sets from what the members are (zero constructed,
plain old data, no destructor) are cleared, which suits any member. A struct another package lays
out by position is refused, naming the package: a script building it as a constant, a value of it
sent over the network, a Niagara asset, or a map or set keyed by it when the new field cannot be
hashed. `--struct N` picks the struct when the package holds more than one.

`asset script-set` changes one thing inside a function's bytecode, addressed by the statement offset
`asset script` prints: a literal by its place in the statement (`--const`) or by where it starts
(`--at`, as `asset script --expressions` lists), an object constant (`--object`), a text (`--text`),
a call's function (`--call`) or a branch's condition (`--condition`). A value may take another
width: the code after it moves, and so does everything pointing into the function, the event stubs
entering an event graph and the latent actions resuming in it. A stub holding its entry in a literal
too small for where the event enters now takes an `IntConst` instead, and grows by it. A function
something points into in a way that cannot be followed keeps its size and takes only a change of the
same width. A call pointed at a Blueprint function is held to its parameters; a native function's
are recorded nowhere the toolkit reads, so one is refused until `--allow-unchecked`. `asset
script-widen` writes every literal in its widest form, which moves code without changing what it
does. `--dry-run` shows the change without writing.


```bash
rivals-cli asset list  --container pakchunk0-Windows.utoc --filter DataTable
rivals-cli asset info  --container pakchunk0-Windows.utoc --entry Marvel/Content/.../DT_Thing.uasset
rivals-cli asset dump  --container ... --entry ... --declared      # the decoded property tree
rivals-cli asset table --container ... --entry ...                 # a DataTable as rows
rivals-cli asset table --container ... --entry ... --row NAME      # one row in full, nested values and all
rivals-cli asset trace --container ... --entry ... --export 0      # the bytes each property took
rivals-cli asset search SetScalarParameterValue                    # every script in the game naming it
rivals-cli asset search Hulk --values --filter Data/DataTable      # stored values too, narrowed by path

rivals-cli asset set --container ... --entry ...   --offset 0xBE6 --kind float --name Damage --value 42.5 --mod-name MyMod
rivals-cli asset set --container ... --entry ...   --offset 0x3A0 --kind map --name Scores --op set-key --index 1 --key Hulk --mod-name MyMod
rivals-cli asset set --container ... --entry ...   --offset 0x3A0 --kind array --name Tags --op reorder --order 2,0,1 --mod-name MyMod
rivals-cli asset set --container ... --entry ...   --offset 0x4C0 --kind struct --name Payload --op set-raw --hex "0a 00 00 00" --mod-name MyMod
rivals-cli asset set --container ... --entry ...   --export DT_Thing --row Hulk --path "Scores{Ranked}" --value 7 --mod-name MyMod
rivals-cli asset row     --container ... --entry ... --export 0 --op add --row NewRow
rivals-cli asset strings --container ... --entry ... --export 0 --op set-source --index 3 --to "Hello"

rivals-cli asset export-edit --container ... --entry ... --export 3 --rename NewName   # header rows
rivals-cli asset export-edit --container ... --entry ... --export 3 --class -32         # retype
rivals-cli asset imports --container ... --entry ... --unused                           # tidy the table
rivals-cli asset names   --container ... --entry ... --compact --mod-name MyMod         # drop unused names
rivals-cli asset add-export --container ... --entry ... --class /Script/Module.Class --outer 0 --name MyThing --mod-name MyMod
rivals-cli asset add-component --container ... --entry ... --node 5 --name StaticMesh2 --mod-name MyMod
rivals-cli asset remove-component --container ... --entry ... --node 18 --mod-name MyMod
rivals-cli asset add-component --container ... --entry ... --from-parent StaticMesh --name StaticMeshCopy --mod-name MyMod
rivals-cli asset add-enum-entry --container ... --entry ... --display "Cone" --mod-name MyMod
rivals-cli asset add-field --container ... --entry ... --name Rarity --type Int --mod-name MyMod
rivals-cli asset save-as --container ... --entry ... --to /Game/Mods/MyThing/DA_Copy --mod-name MyMod
rivals-cli asset rename-package --container ~mods/MyMod_9999999_P.utoc --entry ... --to /Game/Mods/MyThing/DA_New
rivals-cli asset deps    --container ... --entry ... --export 3                         # load order
rivals-cli asset script  --container ... --entry ... --export 26                        # disassembly
rivals-cli asset script  --container ... --entry ... --export 26 --text > fn.txt        # assembler text
rivals-cli asset script-set --container ... --entry ... --export 26 --statement 0x0664 --const 0 --value 1000 --mod-name MyMod
rivals-cli asset script-assemble --container ... --entry ... --export 26 --text-file fn.txt --mod-name MyMod
rivals-cli asset copy-export --container ... --entry ... --from-container ... --from-entry ... --export 274 --name MyLight

rivals-cli asset audit --container pakchunk0-Windows.utoc --filter Data/DataTable
rivals-cli asset audit --container pakchunk0-Windows.utoc --skip-blueprint   # native classes only
rivals-cli asset audit --all --text-check      # every script back byte for byte from its own text
```

`asset search` finds where the game's scripts name something: a call, a bound or broadcast delegate,
a string, a name constant, an object, or a variable read or written. It reads each package from the
copy the game loads, enabled mods included and labelled. A script search parses only the packages
holding functions, which takes under half a minute for the whole game; `--values` searches stored
values too, at any depth, which means reading every package and takes about a minute, or less with
`--filter`. `--kind` keeps one kind of term, `--word` matches whole words only, and `--no-mods`
reads the base game alone. `--kind write` lists only where a variable is assigned or changed in
place: a call changing a variable it is handed counts only for the engine functions known to, the
array, map and set functions, timer handles, random streams and gameplay tag containers, since
nothing in the bytecode says which arguments other calls change.

`asset sweep` sets the same properties across every package a filter matches, by name at any depth,
and puts the whole batch in with one container rewrite. It changes only values a package actually
stores: a slot left to its archetype stays that way.

```bash
rivals-cli asset sweep --container pakchunk0-Windows.utoc --filter CameraShake   --set Amplitude=0 --set AnimScale=0 --mod-name NoShake --dry-run
```

`--filter` matches the package path, which misses anything named differently. `--class` matches what
a package actually holds, at the cost of reading every package the path filter allowed through:

```bash
rivals-cli asset sweep --container pakchunk0-Windows.utoc --class LegacyCameraShakePattern   --set Amplitude=0 --set AnimScale=0 --mod-name NoShake --dry-run
```

A sweep reads from `--container` every time, so a second sweep over packages the mod already holds
replaces what the first one wrote rather than adding to it. It says so when that happens. Pass
`--layer` to read the mod's own copies where it has them, which is how several sweeps build one mod:

```bash
rivals-cli asset sweep --container pakchunk0-Windows.utoc --filter CameraShake   --set Amplitude=0 --mod-name NoShake
rivals-cli asset sweep --container pakchunk0-Windows.utoc --filter CameraShake   --set AnimPlayRate=0.001 --mod-name NoShake --layer
```

Every writing command reads the vanilla asset, splices in the change, proves the result parses back
the same way, and only then writes it into a mod container in `~mods` that overrides the original.
Each property header a save writes is also the one the game's own header builder writes for the same
values: `asset audit` counts the shapes headers take, and across 87.6 million of the game's headers
none takes another, so a save that would write one is refused.
Nothing in the game's own containers is touched. `--mod-name` picks the container and `--replace`
overwrites a copy it already holds. The game loads packages only from IoStore, so writes go to a
`.utoc` trio by default; `--target pak` writes a plain pak for tooling that converts it onward
itself.

### Editing a package as JSON

Rather than working out byte offsets, dump a package, edit the JSON, and let the tool work out the
edits:

```bash
rivals-cli asset dump --container ... --entry ... --declared --json > thing.json
#   edit thing.json in any editor
rivals-cli asset diff  --container ... --entry ... --edited thing.json --out thing.edits.json
rivals-cli asset apply --edits thing.edits.json --dry-run    # what it would change
rivals-cli asset apply --edits thing.edits.json              # write it
```

`asset apply` also takes `--manifest`, a file naming several edit files, or holding them inline, so
a set of packages goes into one mod in one run.

The diff writes each value change as the object, row and property path leading to it, such as
`Mappings[3].Key` or `Options{Hero.Ability}.Value`, with what it read as there. A save finds the
value in whatever it reads, so an edit file still fits a copy that a game patch or an earlier save
has moved, and applied a second time it finds its changes made and writes nothing. A script can
write the same edits itself:

```json
{"container": "pakchunk0-Windows.utoc", "entry": "Marvel/Content/.../DT_Thing.uasset",
 "edits": {"paths": [
   {"export": "DT_Thing", "row": "Hulk", "path": "Damage", "op": "set", "text": "42.5"},
   {"export": "DT_Thing", "row": "Hulk", "path": "Tags", "op": "insert", "key": "Hero.Tank"},
   {"export": "DT_Thing", "row": "Hulk", "path": "Scores{Ranked}", "op": "remove"}
 ]}}
```

`export` is the object's path below the package. `path` steps into a field with `.`, into a static
array's slot or a container's element with `[i]`, and into a map's pair or a set's element with
`{key}`. An edit given `was` is refused as drift once its value reads otherwise, which
`--allow-drift` overrides. An insert into an array, a removal from one or a reorder also records
what the array `becomes`, so applying it again finds it made rather than adding another, and an
array that reads as neither is refused as drift. An edit under one that adds, moves or replaces
what it names is made once that has landed, in the same save, so one apply gives a new row its
columns, an instanced struct given another type that type's fields, and a map whose pairs moved its
changed values.

Rows, string table entries, payloads, and a function's script text or script constants name their
object the same way, with `object` in place of `export`, and a string table entry is found by its
key. A script constant still names its literal by the statement's offset, and `was` holds it to
what it read there. An import retarget or removal names its import by the path it has now, with
`from`. One already made is left out, so these too can be applied again:

```json
{"rows": [{"object": "DT_Thing", "op": "add", "name": "NewRow"}],
 "strings": [{"object": "ST_Menu", "op": "set_source", "key": "Play", "to": "Start"}],
 "imports": [{"op": "retarget", "from": "/Game/A/M_Old.M_Old", "path": "/Game/A/M_New.M_New"}]}
```

What the diff cannot express it writes as a note beside the edits rather than guessing. The edits
beside a note still apply. The limits worth knowing:

- **Not everything has a path.** A field of a map's key keeps its byte offset, which fits only the
  package it came from. Export table edits other than a rename, and bulk data, still name their
  object by index, held by `expect` to what it was written against.
- **A few changes take a second pass.** Sets and maps gain and lose elements by key, and a key is
  typed as text: its own, or the one field of a struct holding one, as a gameplay tag is typed by
  its name. A key with no such text form, such as a struct of several fields, is a note, and so are
  values in a map whose pairs moved when a key has none. A map's new pairs go in after the ones it
  keeps; one the dump puts before them is a note. Dump the saved copy and diff again for those.
- **Some changes are not value edits.** A property given another type is reported, not written,
  and an object given another class is refused: `asset export-edit --class` does that, emptying
  it. An object the dump renames is renamed once everything else has been made under its old name.
  The rename names the object by that path (`from`), so applied again it finds the object called
  so already, and every edit naming the old path finds it under the new one.
- **Bytes are not in the JSON.** Payload and bulk data are named by file in an edit list, never
  dumped inline, and neither is a payload that did not decode: replace it with `asset set --op
  set-raw`, or a `set_raw` edit, which takes any value's bytes as hex.

### Editing a function as text

`asset script --text` prints a function's bytecode as assembler text, one statement a line, and
`asset script-assemble` writes the function anew from such a text, so statements can be added,
dropped, reordered or changed:

```bash
rivals-cli asset script --container ... --entry ... --export 10 --text > graph.txt
#   edit graph.txt
rivals-cli asset script-assemble --container ... --entry ... --export 10 --text-file graph.txt --dry-run
rivals-cli asset script-assemble --container ... --entry ... --export 10 --text-file graph.txt --mod-name MyMod
rivals-cli asset script-assemble --container ... --entry ... --new-function Glow --signature "(Strength: Float)" --text-file glow.txt --mod-name MyMod
rivals-cli asset add-variable --container ... --entry ... --name Charges --type Int --mod-name MyMod
```

Labels stand where offsets did, each named after the offset its statement started at (`@0045:`),
with a comment saying what enters there from outside the function. Whatever outside the function
points into it, an event's entry or a latent action's resume point, follows its label; a text that
drops a label something enters at is refused, naming what holds it. A label of your own is a word
(`@retry:`). A function something points into in a way that cannot be followed keeps its layout:
its statements can change but not move.

The text says everything the bytes do. Variables are named, with the object that owns one written
after `in` where it is not the function or class it plainly belongs to; objects are named by path;
numbers, strings and names take forms that say which instruction holds them (`5`, `Int64Const(5)`,
`1.5f`, `"one byte a character"`, `u"wide"`, `'Name'`); and `#` marks anything written exactly as
the bytes hold it. A text is assembled only for a function whose own text assembles back to its
exact bytes, which `asset audit --text-check` counts across the game.

A text can give its function locals of its own, one `local Name: Type` line each ahead of the first
statement; the function gains a field for each after the ones it has, so its parameters stay first.
A type is `Bool`, `Byte`, `Int`, `Int64`, `Float`, `Double`, `Name`, `Str`, `Text`, `Object<path>`,
`Class<path>`, `SoftObject<path>`, `SoftClass<path>`, `Interface<path>`, `Struct<path>`,
`Enum<path>`, `Array<T>`, `Set<T>` or `Map<K, V>`, with every class, struct and enum named by its
full path, as `Object</Script/Engine.Actor>`. A local of the event graph keeps its value between
events, one for each object, which is state a class variable would otherwise hold. A function's
parameters stay as they are, since callers anywhere in the game pass its arguments by position; a
new function takes whatever signature is wanted. A name or object the package does not have yet is
added to it. Where the game reads a value as a variable (a `SwitchValue`'s index, a `DoubleToFloat`
or `FloatToDouble` cast, a struct member's struct, an array element's array, a delegate) the text
has to name one, as the compiler always does: a call or literal there would be written through a
null pointer and crash the game, so a computed value goes in a `local` first. A struct literal can
leave out the size the compiler writes after its path, as
`StructConst</Script/CoreUObject.Vector2D>(1.0, 0.0)`: the game never reads it. What a `Let` stores,
and each field of a struct literal, is held to the type of where it goes wherever both are known:
the game copies a value's bytes as they are, so a Float is never a Double (`1.0f` and `1.0`) and a
Bool never an Int. A literal giving a struct a different number of fields than the mappings list
asks for `--allow-unchecked`, since a struct's transient fields are in its layout and not in a
literal. A changed or new call to a Blueprint function is held to its parameters; one to a native
function the package never calls with as many arguments is refused until `--allow-unchecked`. An
edit file carries a function's text as `script_texts`, inline (`text`) or from a file beside it
(`file`).

`--new-function NAME --signature SIG` adds a function to the Blueprint class instead of rewriting
one, its script written from the text. A signature takes the inputs in parentheses, `ref` before one
passed by reference, and the outputs after `->`, several in parentheses: `(Strength: Float, ref
Seen: Array<Name>) -> (Hit: Bool, Count: Int)`, types as a `local` line takes them. The function
goes into the class's function map, which is where a call by name, `LocalVirtualFunction
Glow(1.5f)`, finds it, and a call to it is held to its signature; `--class N` picks the class when
the package holds more than one. A name the class or a Blueprint parent already uses is refused; a
native parent's functions are listed nowhere the toolkit reads, so the save notes that it did not
look. In an edit file a `script_texts` entry with `new_function` and `signature` in place of
`export` does the same, and the app's script view has New function.

`--pak` takes a pak path or a bare mod name to look up in `~mods`. `--json` makes every command
emit machine-readable output, and failures exit non-zero. `--dry-run` reports what a write command
would change without touching the pak, and `--section` pins an Engine.ini edit to a given section
instead of letting the app resolve it. Mutating commands refuse to run while Marvel Rivals is open,
since the game holds the pak files; `--force` overrides that. `rivals-cli <command> --help` lists
every flag.

Keep `oo2core_9_win64.dll` beside the executable, as shipped, or Oodle-compressed paks will not read.

## Project Layout

- `src/`: React frontend
- `src-tauri/src/`: Rust backend and Tauri commands
- `src-tauri/resources/`: bundled runtime resources (for example bypass files)
- `crates/rivals-uasset/`: UE5 package reader and writer behind the asset tools
- `crates/rivals-core/`: pak, config-tweak and asset engine shared by the app and the CLI
- `crates/rivals-cli/`: the `rivals-cli` binary

## Signature Bypass

The toolkit installs a signature bypass so the game will load modified pak containers. It drops [oxiloader](https://github.com/oZanderr/oxiloader) into the game's `Binaries/Win64` as `dsound.dll`, which loads the bypass payload `plugins/MarvelRivalsUTOCSignatureBypass.asi` (the original community build, redistributed unmodified). A third-party `dsound.dll` counts as installed too, since it loads the same payload. Anything left from the older `version.dll` scheme, the proxy itself or the superseded `RivalsSigBypass.asi` payload, reports as out of date, and Install clears it before writing the current pair. A `dsound.dll` matching a build the toolkit previously shipped reports out of date as well and is replaced, since the loader keeps its filename across releases and a stale one is otherwise invisible; loaders it does not recognize are left untouched.

## License

This project is dual-licensed under either of the following, at your option:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))

### Bundled third-party components

The installed signature bypass includes [oxiloader](https://github.com/oZanderr/oxiloader) (shipped as `dsound.dll`), licensed under the MIT License, Copyright (c) 2026 oZanderr. Its license text is bundled at [src-tauri/resources/bypass/oxiloader-LICENSE.txt](src-tauri/resources/bypass/oxiloader-LICENSE.txt). The bypass payload `MarvelRivalsUTOCSignatureBypass.asi` is a community binary redistributed unmodified, with no license text accompanying it. Provenance and hashes for both files are recorded in [src-tauri/resources/bypass/NOTICE.md](src-tauri/resources/bypass/NOTICE.md).

Installing mods from `.rar` archives uses the [unrar](https://crates.io/crates/unrar) crate, which
vendors Alexander Roshal's UnRAR source. That source is used only to extract archives, never to
create them, and its license requires the following paragraph to be reproduced:

> UnRAR source code may be used in any software to handle RAR archives without limitations free of
> charge, but cannot be used to develop RAR (WinRAR) compatible archiver and to re-create RAR
> compression algorithm, which is proprietary. Distribution of modified UnRAR source code in
> separate form or as a part of other software is permitted, provided that full text of this
> paragraph, starting from "UnRAR source code" words, is included in license, or in documentation if
> license is not available, and in source code comments of resulting package.
