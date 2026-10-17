# Rivals Toolkit

Rivals Toolkit is a desktop app for working with Marvel Rivals configuration and mod files.
It combines a React frontend with a Tauri/Rust backend to provide:

- game install detection across launchers
- pak browsing, extraction, and repacking
- mod management (status, toggle, export, delete)
- ASI plugin management, and a view of the proxy DLL that loads them
- pak-based INI and game settings tweak tooling
- an INI editor for the configs inside paks and loose ones such as plugin settings
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
rivals-cli mods list                                  # every mod in ~mods, its state and load order
rivals-cli mods conflicts --assets                    # files more than one switched-on mod carries
rivals-cli mods list --ini                            # paks in ~mods with editable INIs
rivals-cli mods list --all-ini --no-recursive         # any INI, top level of ~mods only

rivals-cli logs list --crashes                        # game sessions that crashed, the newest first
rivals-cli logs analyze latest-crash                  # the last crash, and the mods its errors name

rivals-cli tweaks list                                # the tweak catalogue
rivals-cli tweaks status --pak MyMod                  # which tweaks a pak has on
rivals-cli tweaks apply  --pak MyMod --on fix_dark_maps --off cas_sharpening
rivals-cli tweaks apply  --pak MyMod --set brightness=2.8 --dry-run
rivals-cli tweaks presets                             # the presets the desktop app saved
rivals-cli tweaks apply  --pak MyMod --preset QOL --off cas_sharpening

rivals-cli ini list  --pak MyMod                      # INI files the pak ships
rivals-cli ini get   --pak MyMod --key r.TonemapperGamma
rivals-cli ini get   --pak MyMod --entry Marvel/Config/DefaultEngine.ini
rivals-cli ini set   --pak MyMod r.Foo=1 r.Bar=2
rivals-cli ini set   --pak MyMod --file Marvel/Config/DefaultEngine.ini=./Engine.ini
rivals-cli ini unset --pak MyMod r.Foo
```

A mod is named the same way everywhere: by a path to its `.pak` or `.utoc`, or by its whole name in
`~mods`, such as `MyMod_9999999_P`, with or without the extension and in any case, switched on or
off. Nothing short of the whole name is taken: a part of one is refused, with the installed names it
may have meant. The `mods` commands take the mod as their first argument, `mods check
MyMod_9999999_P`, and those working on several take each in turn, `mods disable A B`; `paks`, the
family's old name, still works, and `ini` and `tweaks` take the mod with `--pak`.

The `asset` commands take the asset first: a package name such as `/Game/Marvel/UI/WBP_Thing`, its
path inside a container, or an extracted `.uasset` on disk. `--from` says where it is read: `game`
for the game's own copy, a mod named as above, a game container such as `pakchunk0-Windows`, or a
plain `.pak`; an IoStore mod's `.pak` names its container, which holds the mod's packages, wherever
the mod is. Without it a read takes the copy the game loads: the first switched-on mod holding it
in load order, else the game's own. The commands that work through a whole source, `list`,
`sweep`, `audit`, `diagnose` and `check-structs`, take it first, and there `game` is every package
the game ships, each from the container that wins it. An export is named by its index, as
`asset info` prints it, or by its name or its path below the package.

A save goes `--into` a mod named as above. `--into MyMod` also finds `MyMod_9999999_P`, as the
desktop app names the mods it saves into, and a name no installed mod has makes a new mod: the save
says so, and names any installed mod the name is close to. Without `--into` a save goes into the
mod the desktop app last saved into, then `AssetEdits`. A save goes only into a switched-on mod
whose name ends `_9999999_P`, and says why of any other: one in `~mods` by its name or a path, or an
IoStore mod anywhere else, such as a build kept in its own folder, by a path to one of its files.
That one is written where it is and never made, and the game loads nothing from it, so a mod of its
name in `~mods` is another mod, which the save neither reads nor writes. A save into a mod
already holding the asset builds on the mod's copy; with `--from` naming somewhere else it is
refused unless `--overwrite`, which starts again from there. A save naming no source into a mod
holding no copy, or with `--overwrite` alone, starts from the game's copy, and is refused when
another switched-on mod overrides the asset: starting from the game's copy and building on that
mod's both make sense and make different mods, so `--from game` or `--from` that mod says which, and
the refusal names the mod. Every save takes `--dry-run`, which patches and checks everything, says
which mod it would save into and which copy it would read, and writes nothing. A mod is read where
it is whether it is switched on or off, and `mods replace` changes a switched-off one where it is;
`mods move` asks for its mod switched on.

Reading and editing packages needs a `.usmap` mappings file for the game's build, since Marvel
Rivals ships unversioned properties. Point `--usmap` at the file or at a folder holding one, or set
it once in the app's settings. A mod saved from the editor with tagged properties names what it
stores, and every edit works on it as on the game's own packages. Its instanced structs and the
reflected parts of native structs are tagged blocks too, read as such, and when a value does not
read, the package is read again with its own classes and the Blueprint ones it imports, which name
what its sets and maps hold. A tagged value of a type nothing here reads keeps its bytes, and is
counted as not decoded rather than read past.

Imports of native objects the game registers only at runtime (its `NePatchUtility` hot-patch plugin,
which Blueprint mod loaders call) are not in the game's script objects table, so a bundled list names
them; `--script-objects PATH` adds a text file of further object paths, one per line.

A save that adds an import, retargets one or points an object property at a new path first looks the
path up in the game and the enabled IoStore mods. One that points at nothing is refused, since the
game would load it as nothing; pass `--allow-missing` when a mod loaded alongside provides it.
`asset edit-export` likewise refuses a rename or a move while other packages or soft references may
still name the old path, until `--accept-warnings`.

A package's identity is its stored name, so any writing command can save under another one with
`--as /Game/Mods/MyThing/DA_Copy`: a new asset, or a replacement for the asset at that path. The
paths inside the package that name itself follow, and so do the objects named after it (the asset,
and a Blueprint's class and default object) unless `--keep-object-names` says otherwise. Functions
keep their names, since bytecode calls them by name. Paths in map values and bytecode strings
follow too; a map key, a bytecode string that would change length, and a payload the reader does
not follow keep the old path, and the save says which. `asset save-as` writes a copy with no other
change, and `asset rename-package` moves a package a mod added to another path inside that mod. A
level saves under another path as any package does and stays a `.umap`. The game opens a level only
by a path it is given (a mode's `LevelPath`, a select-hero sublevel row, another map's list), so a
copy loads once one of those names it, and only where the game opens the level itself, as lobby and
select-hero scenes do: in a match the server chooses the map. Its sublevels and its lighting (its
`_BuiltData`) stay the original's, and the save names them.

A world's streaming levels, and a level's model and script actor, are written by the engine after
the properties. The property a class declares for each is transient or read over, so those lists are
what the game loads, and a path names them as it names a property: `asset set <map> --export <world>
--path StreamingLevels --op remove --index 0` stops the world streaming that sublevel in. The
`LevelStreaming` object it named stays in the package, named by nothing.

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
or derives from, itself or through one of its package's own classes below it, is refused, naming
those packages: the import index finds them, and is built first when it has not been. A type is written as a `local` line takes it (see below), a Blueprint one
loading after its type as for a struct's field, and `--class` picks the class when the package
holds more than one. A package whose classes its own records describe otherwise than the mappings
file does, as after this, is read with its own records. The app's script view has New variable, for
the class of the function shown. A function can be added to a Blueprint class, and an existing one
rewritten whole, as text: see below.

`asset add-field --name X --type T` adds a field to a Blueprint struct after its own, named as the
editor names a member: `X_<n>_<GUID>`, the guid the same each time the same field is added. Each
stored value of a struct says which of its fields it holds, so table rows, values and the struct's
own defaults read as before in every package, the new field unset until a value edit sets it. A
package read for a save into the mod holding the struct reads it with the mod's fields, so a row of
a table in the game can be given the new field in that mod. A field the struct has already, by name
and type, is left out, so a file adding one applies again. The type is written as a `local` line
takes it (see below), containers included. A field of a Blueprint type loads after its type, as the
cook orders it: a Blueprint struct or enum is read before the struct, and a Blueprint class a
reference may hold is created before it. The type must be what it says, read through the save's load
order, and a Blueprint struct that needs the struct being edited loaded first, directly or through
others, is refused, since neither could load before the other. A container of containers is refused,
as is a set's element or a map's key that cannot be hashed. A struct key hashes only where its code
says so: an engine struct that does, such as a `GameplayTag`, a `Guid` or an `IntPoint`, is let
through, and any other only with `--allow-unchecked`. A struct deriving from another is refused,
which the editor never makes. The flags the compiler sets from what the members are (zero
constructed, plain old data, no destructor) are cleared, which suits any member. A struct another
package lays out by position is refused, naming the package: a value of it sent over the network, a
Niagara object using it as a type (or holding data the reader does not follow), or a map or set
keyed by it when the new field cannot be hashed. A Blueprint struct holding it as a field, at any
depth, carries its fields, so the same goes for that struct's uses. A script building it as a
constant lists a value for every field, so the save gives each such constant a default for the new
fields, zero, false, none or empty, and writes that package into the mod with the struct; a script
that cannot change size, a field type whose constant is not written here (a struct or a
container), or a package another mod holds is refused instead. `--struct` picks the struct when the
package holds more than one. In the app, a
Blueprint struct's export actions offer Add field.

`asset edit-script` changes one thing inside a function's bytecode, addressed by the statement
offset `asset script` prints: a literal by its place in the statement (`--const`) or by where it
starts (`--at`, as `asset script --expressions` lists), an object constant (`--object-const`), a
text (`--text-const`), a call's function (`--call`) or a branch's condition (`--condition`). A value may take another
width: the code after it moves, and so does everything pointing into the function, the event stubs
entering an event graph and the latent actions resuming in it. A stub holding its entry in a literal
too small for where the event enters now takes an `IntConst` instead, and grows by it. A function
something points into in a way that cannot be followed keeps its size and takes only a change of the
same width. A call pointed at a Blueprint function is held to its parameters; a native function's
are recorded nowhere the toolkit reads, so one is refused until `--allow-unchecked`. `asset
widen-script` writes every literal in its widest form, which moves code without changing what it
does. `--dry-run` shows the change without writing.


```bash
rivals-cli asset list  game --filter DataTable                 # every package the game ships, by path
rivals-cli asset list  MyMod_9999999_P                         # every package a mod holds
rivals-cli asset info  /Game/Data/DT_Thing                     # the copy the game loads
rivals-cli asset info  /Game/Data/DT_Thing --from game         # the game's own copy
rivals-cli asset dump  /Game/Data/DT_Thing --declared          # the decoded property tree
rivals-cli asset table /Game/Data/DT_Thing                     # a DataTable as rows
rivals-cli asset table /Game/Data/DT_Thing --row Hulk          # one row in full, nested values and all
rivals-cli asset trace /Game/Data/DT_Thing --export 0          # the bytes each property took
rivals-cli asset search SetScalarParameterValue                # every script in the game naming it
rivals-cli asset search Hulk --values --filter Data/DataTable  # stored values too, narrowed by path
rivals-cli asset search 10146101 --values --no-self --by ability  # who names it, under each ability
rivals-cli asset ability 101461                                # what an ability is made of
rivals-cli asset ability 1014                                  # a hero's abilities

rivals-cli asset set /Game/Data/DT_Thing --export DT_Thing --row Hulk --path "Scores{Ranked}" --value 7 --into MyMod
rivals-cli asset set /Game/Data/DT_Thing --offset 0xBE6 --expect-kind float --expect-name Damage --value 42.5 --into MyMod
rivals-cli asset set /Game/Data/DT_Thing --offset 0x3A0 --expect-kind map --expect-name Scores --op set-key --index 1 --key Hulk --into MyMod
rivals-cli asset set /Game/Data/DT_Thing --offset 0x3A0 --expect-kind array --expect-name Tags --op reorder --order 2,0,1 --into MyMod
rivals-cli asset set /Game/Data/DT_Thing --offset 0x4C0 --expect-kind struct --expect-name Payload --op set-raw --hex "0a 00 00 00" --into MyMod
rivals-cli asset set /Game/Data/DT_Thing --export DT_Thing --row Hulk --path Damage --value 50 --from game --overwrite --into MyMod
rivals-cli asset edit-row     /Game/Data/DT_Thing --export DT_Thing --op add --row NewRow --into MyMod
rivals-cli asset edit-strings /Game/UI/ST_Menu --export ST_Menu --op set-source --index 3 --key Play --text "Start" --into MyMod

rivals-cli asset edit-export /Game/Data/DA_Thing --export 3 --to NewName --into MyMod   # header rows
rivals-cli asset edit-export /Game/Data/DA_Thing --export 3 --class -32 --into MyMod    # retype
rivals-cli asset imports /Game/Data/DA_Thing --unused                                   # tidy the table
rivals-cli asset names   /Game/Data/DA_Thing --compact --into MyMod                     # drop unused names
rivals-cli asset add-export /Game/Data/DA_Thing --class /Script/Module.Class --outer 0 --name MyThing --into MyMod
rivals-cli asset add-component /Game/Blueprints/BP_Thing --node 5 --name StaticMesh2 --into MyMod
rivals-cli asset remove-component /Game/Blueprints/BP_Thing --node 18 --into MyMod
rivals-cli asset add-component /Game/Blueprints/BP_Thing --from-parent StaticMesh --name StaticMeshCopy --into MyMod
rivals-cli asset add-field /Game/Data/S_Thing --name Rarity --type Int --into MyMod
rivals-cli asset save-as /Game/Data/DA_Thing --to /Game/Mods/MyThing/DA_Copy --into MyMod
rivals-cli asset rename-package /Game/Mods/MyThing/DA_Copy --to /Game/Mods/MyThing/DA_New --into MyMod
rivals-cli asset deps    /Game/Data/DA_Thing --export 3                                 # load order
rivals-cli asset script  /Game/Blueprints/BP_Thing --export ExecuteUbergraph_BP_Thing   # disassembly
rivals-cli asset script  /Game/Blueprints/BP_Thing --export 26 --as-text > fn.txt       # assembler text
rivals-cli asset vm      /Game/VFX/NS_Thing                                             # its Niagara scripts
rivals-cli asset vm      /Game/VFX/NS_Thing --export SpawnScript --constants            # VM disassembly
rivals-cli asset edit-vm /Game/VFX/NS_Thing --export SpawnScript --at 0x00BD --operand 0 --value 0.5 --into MyMod
rivals-cli asset edit-script /Game/Blueprints/BP_Thing --export 26 --statement 0x0664 --const 0 --value 1000 --into MyMod
rivals-cli asset assemble-script /Game/Blueprints/BP_Thing --export 26 --text-file fn.txt --into MyMod
rivals-cli asset copy-export /Game/Maps/MyMap --source /Game/Maps/Other --source-export PointLight_2 --name MyLight --into MyMod

rivals-cli asset audit pakchunk0-Windows --filter Data/DataTable
rivals-cli asset audit pakchunk0-Windows --skip-blueprint   # native classes only
rivals-cli asset audit game --text-check                   # every script back byte for byte from its own text
```

`asset set` names a value two ways. `--export` and `--path` find it by the fields leading to it,
`A.B[2]` or `Scores{Ranked}`, from a DataTable row with `--row` or a Blueprint struct's defaults with
`--defaults`, and leave a value already as asked alone. A value the object leaves unset is set
from its class's layout: the mappings file's, or for a Blueprint class the file has no entry for, as
one a mod adds, the class's own records over its native parent's entry, so a mod's widget can be
given a visibility it inherits. `--offset`, as `asset dump` or `asset trace`
prints it, names it by where it sits, held by `--expect-kind` and `--expect-name` to what was read
there so a stale offset is refused; `--element` picks a static array's slot, and `--field` a value
inside the struct or container there. `--op` says what to do, `set` unless told otherwise, and
`--help` lists the rest. A struct the engine writes whole, such as a vector, a rotator or a colour,
takes the fields left untyped from what the object inherits; when that comes from native code, which
no file records, the save is refused naming them, and `--allow-unchecked` writes them as zero. An
array's new element starts as a copy of the one at `--index`, or of the last when it goes last, and
the save says which; an edit at that index after it gives it a value of its own. `asset edit-row`, `asset edit-strings` and `asset edit-keys` change a
table's rows, a string table's entries and a sequence channel's keys the same way: `--op` names the
change, `--position` where an added or copied row goes, `--index` the entry or key, `--id` an
entry's metadata item, and `--time` a key's frame.

`asset edit-imports` points an import at another object with `--import INDEX --path PATH`, adds one
with `--path` alone, its class given as `--class /Script/Engine.StaticMesh`, or drops imports with
`--remove INDEX`. `asset edit-export` renames an export with `--to`, moves it with `--outer` or
`--outer-root`, points it at another archetype with `--template`, retypes it with `--class` or
reparents it with `--parent`, each a package index as `asset info` prints them, and turns the flags
the loader reads on and off with `--set-flag` and `--clear-flag`, one flag each, or
`--public-hash true|false`. `asset deps` shows an export's preload dependency runs, and with
`--sbs`, `--cbs`, `--sbc` or `--cbc` replaces one with a comma list of package indices, `--cbs=`
for none. `asset copy-export` copies an export and what is under it out of the package `--source`
names, read as a save reads its asset unless `--source-from` says where, under `--outer` here, and
lists it in a level's actors with `--level`. `asset revert` takes one asset back out of a mod, so the
game's own copy loads again; a mod left holding nothing is deleted, which `--dry-run` says first.

```bash
rivals-cli asset hex       /Game/Data/DA_Thing --export 3 --offset 0x40      # an export's bytes, from an offset
rivals-cli asset fields    /Game/Data/S_Thing --export S_Thing               # the fields a struct or class declares
rivals-cli asset dump      /Game/Data/DA_Thing --export 3 --inherited        # unset values as its archetypes hold them
rivals-cli asset importers /Game/Data/DA_Thing --build                       # every package importing it, from the import index
rivals-cli asset edit-imports /Game/Data/DA_Thing --import -12 --path /Game/Data/DA_Other.DA_Other --into MyMod
rivals-cli asset edit-export  /Game/Data/DA_Thing --export 3 --set-flag Public --clear-flag Transactional --into MyMod
rivals-cli asset deps         /Game/Data/DA_Thing --export 3 --cbs 6,-39 --into MyMod --dry-run
rivals-cli asset remove-export /Game/Data/DA_Thing --export 7 --into MyMod --dry-run   # an export and its subobjects
rivals-cli asset reset-export  /Game/Data/DA_Thing --export 3 --into MyMod             # drop what it stores
rivals-cli asset duplicate-export /Game/Maps/MyMap --export PointLight_2 --name PointLight_3 --level PersistentLevel --into MyMod
rivals-cli asset edit-keys /Game/Cinematics/LS_Intro --offset 0x1F40 --expect-name Channel --op add --time 120 --value 0.5 --into MyMod
rivals-cli asset payload /Game/Data/DA_Thing --export 3 --out payload.bin    # the bytes after its properties
rivals-cli asset payload /Game/Data/DA_Thing --export 3 --in payload.bin --into MyMod
rivals-cli asset bulk    /Game/Textures/T_Logo                               # the bulk data table
rivals-cli asset bulk    /Game/Textures/T_Logo --resource 0 --out mip0.bin
rivals-cli asset revert  /Game/Data/DA_Thing --from MyMod_9999999_P --dry-run  # take it back out of the mod
```

`asset audit` reads every package of a source, or of an extracted folder with `--dir`, and counts
how much of each decodes; `--limit` stops after that many packages, `--progress` reports as it goes,
and `--relocation-check` widens every literal in every script in memory and holds each offset into
the script to the code it named. It also reads every Niagara script's VM program, as `asset vm`
does, and lists any whose bytecode does not decode to its end and back, or whose constants or
outputs fall outside the tables and attributes the script has, on `bad:` lines that should count
nothing. `asset diagnose` reads failing packages again with one schema slot
left out at a time, `--struct` keeping to one struct and `--depth` leaving out several at once, to
find mappings entries the build does not serialize. `asset check-structs` reads the Blueprint
structs a source's packages define and compares them with the mappings. `asset search --limit` and
`asset sweep --limit` stop after that many places or packages.

```bash
rivals-cli asset diagnose pakchunk0-Windows --filter Data/DataTable --struct FThing --depth 2 --progress
rivals-cli asset check-structs game --filter Struct_ --progress
rivals-cli asset audit --dir D:\Mods\MyMod --limit 200
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
nothing in the bytecode says which arguments other calls change. A call shows with its arguments,
and `--kind bind` and `--kind broadcast` tell where a delegate is bound or unbound from where it is
fired (`delegate` is both). With `--values` a field is found by its name too, with what it holds,
and a table's value names its row and the row's id, as `[GamePlayModeId 600]`; `--kind value` and
`--kind field` keep one or the other. `--no-self` leaves out a package's paths to its own objects
where its own name is what matched, so an id finds who refers to it. `--by hero` and `--by ability`
list the places under the hero and ability their path names, a mode's or an event's version of an
ability flagged and listed after the game's own.

`asset members` lists a Blueprint class's variables and functions, or with `--function` a
function's parameters and locals, with what uses each: a field path or a call in a script, a call
by the function's object, a binding object or a construction script node naming it, a latent action
resuming into it, a value its objects store under it, and a literal an engine function takes a
member's name in (`Set*PropertyByName`, the `K2_*Timer` functions). A call or a literal acting on an
object the package does not say is of the class, such as an `Actor` a spawn returned, counts as a
possible use; one on an object of an unrelated class is not counted. A class extending a Python
class gives each of its variables and functions a possible use too, since the Python code reaches
them by name. A rename following possible uses leaves that one, which is code outside the packages.
When the package is in a mod,
in `~mods` or a copy anywhere else, the mod's other packages are read for uses too. `--member NAME` lists every use of one member, and
`--unused` keeps those nothing runs or reads, which leaves out overrides and widget-bound
variables, since the engine reaches those itself, and the event graph while an event enters it.
Code outside the packages, native or
Python, can still call a function by its name. A variable or local an assignment statement only
stores into, which nothing ever reads, is marked "written, never read" and listed by `--unused`
too. A store of a call's result is a "call result" instead and keeps its variable: the call writes
its value there, which is how a result nothing reads is compiled. A store through a context, a
struct member or an array element counts as a read, since what holds the variable may still see
it.

```bash
rivals-cli asset members /Game/Mods/MyMod/BP_Loader --from MyMod_9999999_P --unused
rivals-cli asset members /Game/Mods/MyMod/BP_Loader --member ShowMessage
```

`asset rename-member --member OLD --to NEW` renames a variable, a function, an event or a widget
of a Blueprint class, or with `--function` a parameter or local of that function, and every use
`asset members` finds, in the package and in every other package of the mod it is saved into, all
in one write. Almost every use is a name, rewritten in place, so nothing moves: the field record,
the function map, field paths and calls by name, a latent action's resume, the tags the class's
objects store values under and the binding objects naming it. A function's or a widget's export
takes the name too, so UMG binds the renamed widget to its renamed variable, and another package's
import of the function follows. A timer's function name and a widget binding's object name are
strings, rewritten at their new length. The new name has to be free beside the member. An override
is refused, since the engine calls it by its parent's name, and so is a component, whose template
and construction script node go by its name. A use the rename cannot place is refused until
`--allow-unchecked`, which leaves it as it is. A possible use is refused until `--possible follow`
renames it or `--possible leave` keeps it. A package outside the mod that imports the class and uses
the member is refused outright, since the rename cannot reach it. `--dry-run` lists every use it
would change. `asset edit-export` sends a function's rename here.

`asset remove-member --member NAME` takes variables or functions out of a Blueprint class, or with
`--function` locals out of a function, once nothing runs or reads them; `--unused` takes every one
`asset members --unused` lists. A variable goes with the values its objects store for it, in the
package and the rest of the mod. A function goes with its export and its class's function list and
function map entries, which keep the rest of their entries in step. `--drop-bindings` also takes
out the binding entry naming a function, such as an input event's key binding, which otherwise
keeps it. A binding object left with no entries goes too, with its element of the class's
`DynamicBindingObjects`. An override may go, since the parent class's version then runs. An event goes with the
event graph code only it reached, the graph locals that leaves unused and the imports nothing names
any more, unless `--keep-orphaned-imports` keeps those; code another event still reaches
stays, and a graph that has to keep its layout, or whose text does not assemble back to its bytes,
keeps its code, with a warning. `--dry-run` lists every statement it would take. Anything still using a
member refuses it, and so does a possible or unplaced use until `--allow-unchecked`: code a
function's removal takes with it does not count. A variable or local written but never read goes
with the assignments storing into it, and each label on one moves to the line after it. An
assignment whose value calls something is refused rather than taken, since the call would go too. An
event graph no event enters any more goes whole, with its frame, the value the default object
stores for that and the class's reference to the graph; one an event enters stays, and so do a
widget-bound variable, a component and a parameter. In a class whose objects store values by
position, as cooked ones do, every object of it in the package, or of a class below it there, loses
the value it stored for the variable and has every slot after it moved down, so each value still
reads as itself. That is refused while a package elsewhere makes an object of the class, or of one
of the package's classes below it, or derives a class from either, naming the package, and for a
replicated variable, which the server numbers among the class's others. Locals have no stored
values and go from any package. Any save drops, in the same write, every import it left named by nothing,
such as one only a removed member or a rewritten script's old code named, along with the load-order
edges still holding it. `--keep-orphaned-imports` keeps them, each listed with its index for
`asset edit-imports --remove` to drop later. An import the cook itself left held by edges alone is
never one of them, since what it is loaded for may be something this reader does not follow, such
as a text's string table. In the same way a save takes out the locals of each function a script text
rewrites that the rewrite left unused, so `assemble-script` and an edit file's `script_texts` need no
`remove-member` after them; a local the function had no use for before is named and kept, an import
only such a local's type named goes too, and `--keep-unused-locals` keeps them all.

```bash
rivals-cli asset remove-member /Game/Mods/MyMod/BP_Loader --unused --into MyMod --dry-run
rivals-cli asset remove-member /Game/Mods/MyMod/BP_Loader --member Tick --member InpActEvt_Escape_K2Node_InputKeyEvent_0 --drop-bindings --into MyMod
rivals-cli asset remove-member /Game/Mods/MyMod/BP_Loader --function ShowMessage --unused --into MyMod
rivals-cli asset assemble-script /Game/Mods/MyMod/BP_Loader --export 5 --text-file graph.txt --into MyMod
```

```bash
rivals-cli asset rename-member /Game/Mods/MyMod/BP_Loader --member ShowMessage --to ShowNotice --into MyMod --dry-run
rivals-cli asset rename-member /Game/Mods/MyMod/BP_Loader --member "Msg UI" --to MessageWidget --possible follow --into MyMod
```

`asset texture` shows a texture's pixel format, size and mips, and where each mip's pixels are:
inline in the export, or in the package's `.ubulk` or `.uptnl`. The game does not install the
optional `.uptnl` data, so a texture keeping its largest mips there has only the smaller ones, and
a cook can leave out a texture's largest mips altogether. `--out FILE.png` saves the largest mip the
package has, or `--mip N`, decoded from 8-bit color or grey, DXT1, DXT3, DXT5, BC4, BC5 or BC7; a
cube's six faces and an array's or a volume's slices come out stacked top to bottom. A texture
holding high dynamic range values, half or float color or red, or BC6H, saves with `--out
FILE.exr` as OpenEXR instead, in halves or floats as the texture holds them, negative values and
values above 1 kept; a one-channel texture comes out grey.

`--in FILE` gives a texture an image's pixels at the image's size, encoded in the texture's own
format, and saves it into the mod: a PNG for an 8-bit texture, an OpenEXR file for a high dynamic
range one, which a one-channel texture takes the red of. The other kind is refused, naming the kind
the texture takes. A cube takes its six faces stacked top to bottom as `--out` writes them (+X, -X,
+Y, -Y, +Z, -Z), each face square, and an array or a volume takes its slices the same way, as many
as it has. Its mips are rebuilt by halving: one if it had one, else a chain as long as it had at
the old size, the smallest as many inline as before and the rest in its `.ubulk`, which the game
installs where it does not install the `.uptnl`. A cube's faces and an array's slices halve each on
its own; a volume's mips average two by two by two, so its depth halves too. A 2D texture's imported
size follows, and the brushes in the mod that draw it at a size they store are listed, since they
keep drawing at that size. A texture whose cook left out its largest mips takes the image as the
largest mip it stores, which is the one `--out` saves. Those mips stay left out, and the texture's
own size stays as many times larger than the image's, so an image of the stored size keeps the size
it had. `--quality fast|default|best` sets how hard BC7 and BC6H are worked; BC6H holds no negative
values and no alpha, so negatives come in as 0. `--dry-run` checks the image and the package as the
save does and says what it would write. A package can hold more than one texture, or a texture
beside other bulk data, as a map holds its landscape's height and weight maps; `--export` picks
the texture, and everything else's bulk data keeps its bytes, the `.ubulk` written again around the
texture's mips. Several textures of one package take their images in one save, `--export` and
`--in` given in pairs: the package is read once and the mod written once, the same bytes as saving
them one after another. A save of new pixels and nothing else reads only the textures' exports and
holds the rest of the package to the bytes it had, every other export, the header but for the
export and bulk data tables' entries the textures move, and each texture's properties but for its
imported size, so a map's textures save in about a second. A virtual texture is refused (the game
ships only the engine's placeholder ones).
`asset bulk --in` refuses a mip of another size, since the texture's platform data gives the size
too.

```bash
rivals-cli asset texture /Game/Mods/MyMod/T_Logo
rivals-cli asset texture /Game/Marvel/UI/Textures/T_Logo --from game --out logo.png --mip 0
rivals-cli asset texture /Game/Marvel/UI/Textures/T_Logo --in logo.png --into MyMod_9999999_P
rivals-cli asset texture Marvel/Content/Marvel/Maps/KrakoaC/KrakoaC01/KrakoaC01_Art.umap --export Weightmap_120 --in w120.png --export Heightmap_0 --in height.png --into MyMod_9999999_P
rivals-cli asset texture /Game/Marvel/Environment/Common/LightingRes/X20_Hdri/T_Lighthdr_002 --from game --out sky.exr
```

`asset ability` sums up an ability from its six-digit id: its row in the hero's ability table (name,
key, cooldown, and whether it is an ultimate, a passive or a team-up), the inputs its item binds, and
every package carrying the id, the game's own and each mode's and event's version, with the Python
classes, summons and gameplay tags they hold and the telling calls their scripts make: launching,
spawning, summoning, timers and delegates. `--calls` lists every call. A hero's four-digit id lists
its abilities, one line each.

`asset sweep` sets the same properties across every package a filter matches, by name at any depth,
and puts the whole batch in with one container rewrite. It changes only values a package actually
stores: a slot left to its archetype stays that way.

```bash
rivals-cli asset sweep game --filter CameraShake --set Amplitude=0 --set AnimScale=0 --into NoShake --dry-run
```

`--filter` matches the package path, which misses anything named differently. `--class` matches what
a package actually holds, at the cost of reading every package the path filter allowed through:

```bash
rivals-cli asset sweep game --class LegacyCameraShakePattern --set Amplitude=0 --set AnimScale=0 --into NoShake --dry-run
```

A sweep reads a package the mod already holds from the mod, so several sweeps build one mod.
`--overwrite` starts each such package again from SOURCE, dropping what earlier sweeps wrote to it,
and the sweep says when it did:

```bash
rivals-cli asset sweep game --filter CameraShake --set Amplitude=0 --into NoShake
rivals-cli asset sweep game --filter CameraShake --set AnimPlayRate=0.001 --into NoShake
```

Every writing command reads the vanilla asset, splices in the change, proves the result parses back
the same way, and only then writes it into the mod container `--into` names, which overrides the
original once it is in `~mods`.
Each property header a save writes is also the one the game's own header builder writes for the same
values: `asset audit` counts the shapes headers take, and across 87.6 million of the game's headers
none takes another, so a save that would write one is refused.
Nothing in the game's own containers is touched. `--into` picks the mod and `--overwrite` starts
again over a copy it already holds. The game loads packages only from IoStore, so writes go to a
`.utoc` trio by default; `--format pak` writes a plain pak for tooling that converts it onward
itself.

### Working on a whole mod

`mods search NAME --find TEXT` finds where a mod's packages name a text, ignoring case: in their
scripts and in the values their objects and tables store. A text is found by what it shows, and
otherwise by the namespace and key its translations are filed under, which no screen shows but
which still carry whatever name the text was first made under. `--names` looks in each package's table of
names as well, which still holds a name nothing in the package uses any more, such as an old
folder's path after a move, and says of each name it finds there whether it is unused, or why that
cannot be known.

```bash
rivals-cli mods search MyMod_9999999_P --find OldName --names
```

`mods replace NAME --find TEXT --with TEXT` replaces text across a mod in one write: the strings its scripts
hold, at their new length unless a script has to keep its size, which is listed instead; the strings
and texts its objects and tables store; and its string tables' entries. A text keeps its key and
namespace, and one showing a string table's entry is changed in the table. Names are left alone,
since they are members, rows and objects (rename those with `asset rename-member` or move them), and
so are strings that are paths, unless `--paths`. `--ignore-case` matches any case and writes the
replacement in the case of what it replaces, so `GALACTA` becomes `SIGEON` and `Galacta` becomes
`Sigeon`; `--word` matches only a word of its own; `--filter`, repeatable, keeps the packages whose
path or name holds a text; `--allow-unchecked` saves a change that cannot be confirmed, as any save
does; `--dry-run` lists every change.

```bash
rivals-cli mods replace MyMod --find "Old Name" --with "New Name" --ignore-case --dry-run
```

`mods move` moves packages a mod added to other paths inside it: every package under a folder with
`--folder /Game/Old=/Game/New`, one package with `--package /Game/Old/Thing=/Game/New/Thing`, and with `--name-prefix GAL_=SIG_` the moved
packages whose name starts with the one prefix take the other, their classes and default objects
with them. Each move points the mod's other packages at the new path, its imports, path strings and
map keys included, and a Blueprint whose class is renamed has its event graph renamed after it, the
latent actions resuming into it following. Strings still naming a moved folder follow last, as
paths. A package the game ships stays where it is, and a move onto something already there is
refused. `--keep-object-names` keeps the objects' names, `--keep-referencers` leaves the other
packages as they are, and `--allow-unchecked` saves a move whose changes cannot all be confirmed;
`--dry-run` lists every move.

```bash
rivals-cli mods move MyMod --folder /Game/Mods/Old=/Game/Mods/New --name-prefix OLD_=NEW_ --dry-run
```

`mods copy NAME --to NAME` copies a mod under a new name, rebuilt so it mounts as a container of its own:
a container's id comes from its file name, so a copy made on disk would clash with the original.
`--to` is a name in `~mods` or a path elsewhere; a folder that exists, or a path ending in a
separator, takes the copy under the mod's own name, as `cp` does. `--filter TEXT`, repeatable,
keeps only the packages whose path or name holds a text. A kept package that imports one left out
would fail to load, so the copy is refused unless `--allow-missing` says those imports will be
taken out; packages that name one by path are listed.
`--into MOD` copies the packages into a mod already there instead, switched on or off, in one
rewrite that keeps everything else it holds: each goes in as the first mod stores it, over the
second's own copy, and a package it imports that the second ships already counts as there.
`--overwrite` writes over a mod already at the copy's name. `mods rename NAME --to NAME` renames a
mod where it is, rebuilt the same way, and keeps it switched on or off. `mods remove` takes packages
out of a mod in one write, by `--package` or `--filter`, with the same check. Each takes a mod
switched off as readily as one switched on, and `--dry-run` says what would change.

```bash
rivals-cli mods copy    MyMod_9999999_P --to MyModLite_9999999_P --filter /Core/ --dry-run
rivals-cli mods copy    MyOldMod_9999999_P --filter /Settings/ --into MyMod_9999999_P
rivals-cli mods rename  MyModLite_9999999_P --to MyModCore_9999999_P
rivals-cli mods remove  MyModCore_9999999_P --filter /Unused/ --dry-run
```

`mods deps` shows what each package of a mod imports and names by path, what imports it, and how the
game comes to load it: a package the game ships is an override, loaded wherever the game would load
its own, and the rest load because something reached imports them, or only if something reached asks
for them by the path it names. An import neither the mod nor the game has is listed, and
`--unreached` lists only the packages nothing leads to.

```bash
rivals-cli mods deps MyMod_9999999_P --unreached
```

A mod keeps loading its own copy of a package the game ships after a game update changes the game's.
`mods overrides` checks each package a mod overrides against the game's current copy and lists what
the game's copy has that the mod's lacks or declares differently:

- the Blueprint variables and functions it declares, with their types and parameters;
- its parent class;
- the DataTable rows and columns it has, and its row struct;
- its StringTable keys;
- the objects it holds by name, meaning widgets, components and the like (a panel's slots and an
  animation's scene are left out, since an editor's save names those afresh);
- the objects it imports.

What only the mod's copy has is the mod's own intent, so it is not listed. A copy sharing fewer than
half the game's objects is called a replacement. It takes several mods, and `--all` checks every
switched-on IoStore mod. `mods report` gives each override the same one-line verdict.

`--values` also lists what each override holds differently from the game's copy, which is what the
mod changes: every field, row cell, element and map entry by its export, row and path, a Blueprint
struct's defaults among them, with both values, and every row the mod adds. Objects compare by path, so two packages numbering their
imports apart agree; a panel's slot is the same whatever number an editor's save gave it, and an
enumerator the same with its enum's name before it or without. A field one copy leaves at its
default and the other stores counts, even as zero, since the two hold different things.

```bash
rivals-cli mods overrides MyMod_9999999_P
rivals-cli mods overrides MyMod_9999999_P --values
rivals-cli mods overrides --all --json
```

`mods diff` compares two IoStore mods package by package, such as a build and a backup of it, or two
variants of one mod. Packages pair by name, and one whose bytes match in both is counted the same and
not read. It lists the packages only one mod holds, and the files their paks hold beside their
containers that only one holds or the two hold differently. For each package both hold differently,
it says which parts differ, the package itself or its bulk data, and then what differs in it:

- the objects, variables, functions, parameters, parent class, rows, columns, string keys and
  imports only one copy has, or both hold differently. An import is named by the outermost object
  missing, so a package the other copy no longer imports at all shows as the objects it held;
- each function whose script reads differently, as the lines of its `asset script --as-text`
  printing that differ, with two around each run. Labels are code offsets, so they are paired by
  the code they stand at: a statement put in shows as that one line rather than as every label after
  it renamed, while a jump landing on other code still shows;
- the functions that read the same and only sit elsewhere, such as an event whose graph grew before
  the code it enters at, named once;
- every value the two copies hold differently, by export, row and path, a Blueprint struct's
  defaults among them as `defaults.Field`;
- what an export holds beyond its values and scripts: a texture's format, size, mips and inline
  pixels, and the bytes a class keeps that the parse measures but does not decode. Bytes of one size
  holding other values may be no more than names or objects the two number otherwise, which it says
  when the two do not number them alike. Mips kept in bulk files show as the parts that differ.

`--brief` only pairs the packages, reading none of them, so it needs neither the game's containers
nor mappings. `--json` has everything the text shortens.

```bash
rivals-cli mods diff D:/Backup/MyMod_9999999_P.utoc MyMod_9999999_P
rivals-cli mods diff MyMod_9999999_P MyModLite_9999999_P --brief --json
```

`mods report` also holds each combined table a mod ships (a `CompositeDataTable`) to its parent
tables, which it reads where the game would, from the mod or from the game, merging them in order
with a later parent's row taking the place of an earlier one's. The game is expected to rebuild a
combined table from its parents when it loads, so the rows the table stores itself matter only
where they disagree: a row no parent has, or a cell a parent holds otherwise, is an edit that would
not show, and each is a warning saying to edit the parent. Storing only some of the parents' rows
is not one. It also counts, under each package, the imports and names nothing in it uses, and lists
each load-order problem described under `mods check` below.

`mods check NAME` checks a whole mod in one go and fails when it finds something that would
break:

- **audit**: every package reads whole and writes its header back as it was, and every function
  comes back byte for byte from its own text, as `asset audit --text-check` checks;
- **imports**: every package it imports is in the mod or the game, and something the game loads
  leads to each package, a warning otherwise;
- **overrides**: each package it overrides matches the game's current layout, as `mods overrides`
  checks;
- **unused**: no package holds an import or a name nothing uses;
- **unused members**: no Blueprint holds a local nothing in its function names, which `mods tidy`
  drops, or a variable or function nothing in the mod uses, as `asset members --unused` lists them.
  Those are left for `asset remove-member`, since the game's code or another mod may still reach
  them by name; a member of a class extending a Python class counts as possibly used, since that
  class's code reaches members by name;
- **load order**: every export waits for the objects its scripts and values name, so the loader
  creates them before it reads the export. An export that also waits for another export it never
  names is a warning: that is the edge a link saved against the wrong export leaves, which
  `mods tidy` drops. Every package the game ships passes both, apart from the edges the cook writes
  itself, to an object's own subobjects and to imports;
- **pak entries**: the pak beside the container holds neither `chunknames` nor `patched_files`.
  Older packers, this toolkit among them, wrote those there, but the game does not support them, and
  current packers no longer do. A container needs only a pak of its name beside it, which may be
  empty, as most of the game's own are. Nor does it hold a `desktop.ini`, `Thumbs.db` or
  `.DS_Store` in any folder: the files Windows and macOS leave in folders, which a mod packed from
  a folder picks up. They are no part of a mod, and a `desktop.ini` in a mod has been reported to
  get players kicked;
- **combined tables**: each agrees with its parents;
- **forbidden text**: with `--forbid TEXT`, repeatable, no package names it in its scripts, its values,
  its texts' namespaces and keys or its table of names, which is how a renamed mod shows nothing of
  its old name is left.

A failed audit, a missing import, an object named with no edge or a forbidden text fails the
check, and it exits non-zero; the rest warn. `--skip-audit` leaves out the audit, the slowest step,
and `--json` gives every step's status and details.

`mods tidy NAME` drops what the "unused", "load order" and "pak entries" steps warn about, and the
locals the "unused members" step finds, in one write of the mod, switched on or off. Every save
leaves those pak entries out of the pak it writes again, too. For each package it drops:
- the edges to another export nothing in the waiting export names;
- each function's locals nothing in it names, which only that function could;
- the imports nothing names, with the outers that leaves holding nothing;
- the names nothing uses, which by then includes those the dropped imports freed.

Each kind is a stage of its own, checked the way a save is. A package with nothing to drop is not
written again, `--dry-run` lists what would go, and what cannot go is named in a warning.

`--pak-only` takes out only the pak entries the "pak entries" step warns about, and reads and writes no
package, so it suits a mod someone else made, where a whole tidy would rewrite their packages. With
`--all` it goes through every IoStore mod in `~mods`, switched on or off, as `--recursive` and
`--no-recursive` say, the way repak-rivals offers to rewrite affected paks when it starts; plain pak
mods are passed over. `--all` goes only with `--pak-only`, so a whole tidy is always of one mod.

Every save holds itself to the same load order. A save is refused when it leaves:
- an export naming an object it does not wait for;
- an export waiting for another export it does not name;
- an export the package already had waiting for an import nothing in it names. An edge to a string
  table is the exception, since a text names its table by id.

None of these refusals fires where the package was already that way before the save. Only a toolkit
bug leaves one, so `--allow-unchecked` saves anyway, and a hand edit with `asset deps` is exempt.
A save drops, in the same write, each wait it left for an object the export named before and no
longer names, such as the texture a retype emptied out of an image, and then the import nothing
names any more.

```bash
rivals-cli mods check MyMod_9999999_P --forbid OldName --forbid "Old Name"
rivals-cli mods tidy  MyMod_9999999_P --dry-run
rivals-cli mods tidy  --pak-only --all --dry-run
```

### Plain paks and conversion

A plain pak delivers loose files, such as INIs and movies; the game loads packages only from an
IoStore container, the `.utoc` and `.ucas` beside a mod's pak.

`mods extract NAME --out DIR` converts an IoStore mod's packages back into loose files and
writes its pak's own files beside them, switched on or off. A plain pak is unpacked as it is.
Every pak's files are named from the game root, `Marvel/Content/...`, whatever its mount point:
UnrealPak mounts a pak at the folder its files share, and the toolkit reads, unpacks and edits
such a pak by the paths the game sees. `mods pack DIR --to NAME` packs such a folder, laid out
from `Marvel/Content`, into an IoStore mod, and anything that is not a package goes in its pak;
given a plain pak instead of a folder, switched on or off and anywhere, it unpacks that first, so
an old pak mod becomes an IoStore one in one step without being installed. The pak beside an
IoStore container is refused, since it holds only part of its mod. A package is a `.uasset` or `.umap` with
its `.uexp` beside it, and its bulk data with them; a part without the rest, such as a `.uasset`
missing its `.uexp` or a `.ubulk` with no package, is named and the pack refused, since the game
would never load it.

A shader archive the editor cooks for a material's new shaders,
`ShaderArchive-<Library>.ushaderbytecode`, becomes the container's shader library, and the
`ShaderAssetInfo-<Library>.assetinfo.json` beside it tells each package which of its shader maps
it uses; neither goes in the pak. An archive named like a library the game ships, such as the
editor's `Global` one, would replace the game's whole library, so it is left out with a warning,
and finding the game's names needs the game folder. `mods extract` writes a container's libraries
back out as those two files, so an extracted mod packs back the same.

`--to` is a name in `~mods` or a path
ending in the mod's name, so a folder alone is refused. `--format pak` packs every file into a
plain pak instead; the packages the folder holds are named, since the game would not load them from
it. `--overwrite` writes over a mod already there, and `--into MOD` packs the folder's packages into
an IoStore mod already there, keeping what else it holds; files that are not packages are named and
left out. `--dry-run` builds the mod and checks it, and writes nothing. `mods files` lists what a mod
holds, its container's packages and its pak's files, with `--filter`, repeatable.

`mods convert --format iostore|pak` converts mods where they are: a plain pak's packages go into a
container, its other files into the pak beside it, and a container's packages come back out into a
plain pak, the files its pak carried with them. A pak holding no package, such as an INI or sound
mod, becomes a container holding nothing but its header, as the game's own translations container
is, with its files in the pak beside it. The rebuild is staged aside, checked to hold everything
the mod held, and only then swapped in, so a mod that would lose something stays as it was, and
`--dry-run` stops there for every mod. It
takes several mods, and `--all` takes every switched-on mod not already in that format; each is
converted on its own, and the command fails if any one did. A container keeps its obfuscation
unless `--obfuscate` or `--no-obfuscate` says otherwise. A switched-off mod is refused. A
container converted to a container again, to obfuscate it or to take that away, has its blocks
copied as they are stored, encrypted again as asked, rather than its packages converted out and
back, so its packages and its size stay as they were and `--recompress` compresses it again. One
this editor cannot read, one built for another engine version, and one holding a shader library
named like the game's go the long way, out to loose files and back.

Every command that writes a mod (`pack`, `convert`, `copy`, `rename`, `remove`, `replace`, `move`,
`tidy`) takes
`--compression oodle|zlib`, Oodle by default as the game's own files are, and `--oodle-level
none|fast|normal|optimal1|optimal2|optimal3`, which defaults to the desktop app's mod setting and
then to `normal`. The pak beside a container is written the same way. Those apply to what the
command writes anew: a save or a rewrite keeps the chunks it does not change as they were stored,
in the compression and at the level the mod was packed with, so saving into a mod changes the size
of what the save changed and nothing else. Only a chunk stored in another method is compressed
again. `--recompress` compresses everything the mod holds again, at `--compression` and
`--oodle-level`; `mods pack` takes it only with `--into`, since a mod packed anew holds nothing
already. Every chunk carried over is read back from the rewrite before it replaces the mod.

```bash
rivals-cli mods extract MyMod_9999999_P --out D:\Mods\MyMod
rivals-cli mods pack    D:\Mods\MyMod --to MyMod_9999999_P --overwrite --oodle-level optimal2
rivals-cli mods pack    D:\Mods\MyConfig --to MyConfig_9999999_P --format pak
rivals-cli mods pack    D:\Mods\MyFix --into MyMod_9999999_P --dry-run
rivals-cli mods files   MyMod_9999999_P --filter Config
rivals-cli mods convert MyOldMod_9999999_P --format iostore
rivals-cli mods convert --all --format iostore --no-obfuscate
```

### Managing the mods folder

`mods list` lists every mod in `~mods` with its rank in the load order (1 is the one whose copy of a
shared file loads: the highest patch number, then the first by name), whether it is switched on, its
kind, patch number, size and obfuscation. A switched-off copy of a mod that is also switched on is
marked stale. `mods conflicts` names the switched-on mods that carry the same files, an IoStore
mod's container and the pak beside it alike, and `--assets` lists each file with the copy that loads
first and whether the game ships it too.

`mods install PATH...` installs mods: a `.pak` with the `.utoc` and `.ucas` beside it, either of
those for the `.pak` beside it, a `.zip`, `.7z` or `.rar`, or a folder, such as a downloads folder
holding any mix of these and mods' loose files. Folders and archives are looked through at any
depth, archives inside archives too, and each mod found installs on its own:

- A folder holding a `Marvel` or `Engine` folder is a mod's loose files, as a pak unpacks. It is
  packed into a container named after the folder, with `_9999999_P` added when the name has no patch
  number, so it mounts above the game's patches. Only what is under those two folders goes in, since
  the game reads nothing else: a readme or preview beside them stays out. An archive holding the two
  folders at its top is named after the archive.
- A plain pak holding packages is packed into a container under its own name, since the game loads
  no package from a plain pak. Its other files ride in the pak beside the container. A plain pak
  holding none goes in as it is.
- An IoStore mod goes in as it is, without the `chunknames` and `patched_files` an older packer left
  in its pak, which the game does not support.

Whatever goes in leaves out the `desktop.ini`, `Thumbs.db` and `.DS_Store` files Windows and macOS
leave in folders: packing never takes them, and a pak copied in loses them, keeping how each other
entry is stored. The install says what it took out.

Everything is looked through before anything is written. Two mods that would go in under one name
are both refused, each naming the other. Packages under no `Marvel` or `Engine` folder, whose place
in the game is unknown, are named and left out, as is a container with no pak beside it; the other
mods still go in. The mods folder itself, a folder in it or one holding it is refused. A mod named by
more than one of its files installs once. A mod already installed under that name, switched on or
off, is left alone unless `--overwrite`, which also takes away a container the new copy does not
have. `--to DIR` installs into another folder instead of `~mods`, to make fixed copies of mods to
share. What install packs is compressed as `--compression` and `--oodle-level` say, by default with
Oodle at the desktop app's mod level. The desktop app installs whatever is dropped on its mods list
the same way, files and folders alike.

`mods disable` and `mods enable` switch mods off and on, all of a mod's files as one step and never
over a file already at a name they take; each takes several mods, and `--all` takes every mod in the
other state. `mods delete NAME...` deletes mods, their pak, container and cas switched on or off.
`mods export-archive FILE.zip|FILE.7z` packs every switched-on mod into an archive, under the paths
they have in `~mods`, which `mods install` takes back, and `--overwrite` writes over an archive
already there. Each of these takes `--dry-run`, which says what it would do and changes nothing. A command working through several mods does each on its own, lists what became of each, and
fails if any one did. The listings, `--all` and the export read `~mods` subfolders as the desktop app
is set to, or as `--recursive` and `--no-recursive` say.

```bash
rivals-cli mods list
rivals-cli mods conflicts --assets
rivals-cli mods install D:\Downloads\SomeMod.zip D:\Downloads\Other_9999999_P.pak --overwrite
rivals-cli mods install "D:\Downloads\Rivals Mods" --dry-run
rivals-cli mods install "D:\Downloads\Rivals Mods" --to D:\Fixed
rivals-cli mods disable MyMod_9999999_P Other_9999999_P
rivals-cli mods enable  --all
rivals-cli mods delete  OldMod_9999999_P --dry-run
rivals-cli mods export-archive D:\Backups\mods.7z
```

### Game logs and crash reports

The game writes an encrypted log for every launch to `%LOCALAPPDATA%\Marvel\Saved\Logs`, and for
every crash a folder in `%LOCALAPPDATA%\Marvel\Saved\Crashes` holding the crash report and a copy
of that session's log. `logs list` lists the sessions, the newest first: how each ended (a clean
exit, a crash, a fatal error with no report left for it, or a log that stops with neither, as when
the game is killed), its build, and its crash, with how many crash reports share its message and top
stack frames. `--crashes` lists only the sessions that crashed. A session is named by its log's file
name without `.log`, such as `2026.10.14_02.13.57-8832`, by a path to the log or to the crash
folder, or as `latest` or `latest-crash`.

`logs decode LOG` prints the log's text as the game wrote it, or writes it to `--out FILE`, which
`--overwrite` writes over. `--verbosity`, `--category` and `--filter` keep only the lines they match,
and `--redact` takes out the player's Windows name, account and machine ids, tokens, IP addresses
and command-line values. `logs analyze LOG` summarises the session: the crash, with its faulting
module and stack, telling the game's code from Windows, the GPU driver and anything else loaded
into the game, such as ASI plugins; the fatal error; the errors and warnings, alike lines grouped;
and the mods the session mounted that ship the packages those lines name. Errors the game also
logs in its latest sessions on the same build with no mods mounted are marked so, and are not
blamed on mods. For a session that died, it names the cause when the crash says so outright (see
"Cause" under [Logs](#logs)) and which Python the game thread was running. `--share` prints a
redacted summary to paste into a bug report.

The logs are encrypted under a key built into the game, which the toolkit ships. If a patch
changes it, set the new key and IV in the desktop app's Settings, under Game Logs; `logs key` says
which key is in use and whether it opens the newest log.

```bash
rivals-cli logs list --limit 20
rivals-cli logs list --crashes
rivals-cli logs analyze latest-crash
rivals-cli logs analyze 2026.10.14_02.13.57-8832 --share
rivals-cli logs decode latest --verbosity error,warning
rivals-cli logs decode latest --filter ~mods --out D:\mounts.txt
rivals-cli logs decode latest-crash --redact --out D:\crash.txt --overwrite
rivals-cli logs key
```

### Editing a package as JSON

Rather than working out byte offsets, dump a package, edit the JSON, and let the tool work out the
edits:

```bash
rivals-cli asset dump /Game/Data/DT_Thing --declared --json > thing.json
#   edit thing.json in any editor
rivals-cli asset diff  /Game/Data/DT_Thing --edited thing.json --into MyMod --out thing.edits.json
rivals-cli asset apply thing.edits.json --dry-run    # what it would change
rivals-cli asset apply thing.edits.json              # write it
```

The diff reads the package where a save into the mod would, so its edits are addressed into the
copy they land on. `asset apply` takes edit files and manifests alike, told apart by what they hold:
a manifest names several edit files or holds them inline, so a set of packages goes into one mod in
one run.

The diff writes each value change as the object, row and property path leading to it, such as
`Mappings[3].Key` or `Options{Hero.Ability}.Value`, with what it read as there. A save finds the
value in whatever it reads, so an edit file still fits a copy that a game patch or an earlier save
has moved, and applied a second time it finds its changes made and writes nothing. A script can
write the same edits itself:

```json
{"asset": "/Game/Data/DT_Thing", "into": "MyMod",
 "edits": {"paths": [
   {"export": "DT_Thing", "row": "Hulk", "path": "Damage", "op": "set", "text": "42.5"},
   {"export": "DT_Thing", "row": "Hulk", "path": "Tags", "op": "insert", "key": "Hero.Tank"},
   {"export": "DT_Thing", "row": "Hulk", "path": "Scores{Ranked}", "op": "remove"}
 ]}}
```

An edit file names its asset and where it is read as the command line does: `asset`, and
optionally `from`, `into`, `overwrite` and `format`, so without `from` a save builds on the mod's
own copy when it holds one and otherwise reads the game's, refused when another mod overrides it.
`asset diff` writes `from` whenever it was given one. A `from` or an `asset` that
names a file beside the edit file is that file. `export` is the object's path below the package. `path` steps into a field with `.`, into a static
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

A texture takes an image the same way: `textures` names each texture by `object` (or `export`)
and its `image`, a file beside the edit file, with an optional `quality` of `fast`, `default` or
`best`. Each image is encoded in its texture's own format against the package as the save reads
it, so it fits whatever copy the save builds on, and the textures of one package go in one save,
from one edit file or from several a manifest merges, as `asset texture --in` saves them:

```json
{"textures": [{"object": "KrakoaC01_Art:PersistentLevel:Landscape_1:Weightmap_120", "image": "w120.png"},
              {"object": "Heightmap_0", "image": "height.png", "quality": "fast"}]}
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
  and an object given another class is refused: `asset edit-export --class` does that, emptying
  it. An object the dump renames is renamed once everything else has been made under its old name.
  The rename names the object by that path (`from`), so applied again it finds the object called
  so already, and every edit naming the old path finds it under the new one.
- **Bytes are not in the JSON.** Payload and bulk data are named by file in an edit list, never
  dumped inline, and neither is a payload that did not decode: replace it with `asset set --op
  set-raw`, or a `set_raw` edit, which takes any value's bytes as hex.

### Editing a function as text

`asset script --as-text` prints a function's bytecode as assembler text, one statement a line, and
`asset assemble-script` writes the function anew from such a text, so statements can be added,
dropped, reordered or changed:

```bash
rivals-cli asset script /Game/Blueprints/BP_Thing --export 10 --as-text > graph.txt
#   edit graph.txt
rivals-cli asset assemble-script /Game/Blueprints/BP_Thing --export 10 --text-file graph.txt --into MyMod --dry-run
rivals-cli asset assemble-script /Game/Blueprints/BP_Thing --export 10 --text-file graph.txt --into MyMod
rivals-cli asset assemble-script /Game/Blueprints/BP_Thing --new-function Glow --signature "(Strength: Float)" --text-file glow.txt --into MyMod
rivals-cli asset add-variable /Game/Blueprints/BP_Thing --name Charges --type Int --into MyMod
```

Before anything is written, the save reads the package back and holds each function it wrote from
text to that text, statement by statement: where each one starts and every value it holds. It then
says so for each, as `ExecuteUbergraph_BP_Thing: 165 statement(s), each checked against the text`,
so there is no need to print the function again and compare. The dry run's layout line says the
function's own jumps and resume points follow the text's labels, and counts only the entries and
resume points other functions hold into it.

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
`Class<path>`, `SoftObject<path>`, `SoftClass<path>`, `Interface<path>`, `Delegate<path>`,
`Struct<path>`, `Enum<path>`, `Array<T>`, `Set<T>` or `Map<K, V>`, with every class, struct and enum
named by its full path, as `Object</Script/Engine.Actor>`, and a delegate by the function its
signature comes from, as `Delegate</Script/Engine.TimerDynamicDelegate__DelegateSignature>`: the
local `BindDelegate` binds a function into before `AddMulticastDelegate` adds it to an event, or a
timer is set with it. A local of the event graph keeps its value between
events, one for each object, which is state a class variable would otherwise hold. A function's
parameters stay as they are, since callers anywhere in the game pass its arguments by position; a
new function takes whatever signature is wanted. A name or object the package does not have yet is
added to it, an object with the class its own package exports it with, read from the mod being
saved into and then the game; a text can write the class itself before the path, as
`/Script/Engine.BlueprintGeneratedClass'/Game/Mods/Loader.Loader_C'`, and one found nowhere is
noted for its class.
Where the game reads a value as a variable (a `SwitchValue`'s index, a `DoubleToFloat`
or `FloatToDouble` cast, a struct member's struct, an array element's array, a delegate) the text
has to name one, as the compiler always does: a call or literal there would be written through a
null pointer and crash the game, so a computed value goes in a `local` first. A struct literal can
leave out the size the compiler writes after its path, as
`StructConst</Script/CoreUObject.Vector2D>(1.0, 0.0)`: the game never reads it. What a `Let` stores,
and each field of a struct literal, is held to the type of where it goes wherever both are known:
the game copies a value's bytes as they are, so a Float is never a Double (`1.0f` and `1.0`) and a
Bool never an Int. A literal giving a struct a different number of fields than the mappings list
asks for `--allow-unchecked`, since a struct's transient fields are in its layout and not in a
literal. A changed or new call to a Blueprint function is held to its parameters, a call by name
made on an object of another Blueprint class included: that class's function, or a Blueprint
parent's, is read from its package, in the mod being saved into or the game. An argument the
function writes, an output or a `ref` input, takes a variable, any one; a literal or a computed
value there is refused, since the game would have nowhere to write. A call to a native function
the package never calls with as many arguments is refused until `--allow-unchecked`. An
edit file carries a function's text as `script_texts`, inline (`text`) or from a file beside it
(`file`).

`--new-function NAME --signature SIG` adds a function to the Blueprint class instead of rewriting
one, its script written from the text. A signature takes the inputs in parentheses, `ref` before one
passed by reference, and the outputs after `->`, several in parentheses: `(Strength: Float, ref
Seen: Array<Name>) -> (Hit: Bool, Count: Int)`, types as a `local` line takes them. The function
goes into the class's function map, which is where a call by name, `LocalVirtualFunction
Glow(1.5f)`, finds it, and a call to it is held to its signature; `--class` picks the class when
the package holds more than one. A name the class or a Blueprint parent already uses is refused; a
native parent's functions are listed nowhere the toolkit reads, so the save notes that it did not
look. In an edit file a `script_texts` entry with `new_function` and `signature` in place of
`export` does the same, and the app's script view has New function.

### Niagara scripts

A Niagara effect simulated on the CPU runs compiled VectorVM bytecode, which its `NiagaraScript`
exports carry. `asset vm` lists a package's scripts with their usage (spawn, update, event, system)
and disassembles one: every instruction with its operands, each constant named by what it reads (an
engine value such as `Engine.DeltaTime`, a parameter of the emitter such as `User.IsEnemy`, or a
literal with its value and how many instructions share it), and each output by the particle
attribute it writes, such as `Particles.Color.R`. `--constants` adds the literal table with what
reads each entry, and the script's parameter store. GPU scripts carry no bytecode of their own.
`--trace Particles.Color` follows each output whose name starts so back to what it is computed
from, as one expression and as its leaves: each constant with the instruction and operand that read
it, how many operands share it and every output its instruction reaches, and the particle values,
inputs and data interface calls it also takes. An output whose leaves are all literals is baked. In the
app, a `NiagaraScript` export's VM view shows the same listing beside its outputs: each opens to
what it is made of, a leaf shows the instruction reading it, and a constant copies the `asset
edit-vm` line that changes it.

```bash
rivals-cli asset vm /Game/VFX/NS_Thing                                       # the scripts
rivals-cli asset vm /Game/VFX/NS_Thing --export SpawnScript_1 --constants    # one, disassembled
rivals-cli asset vm /Game/VFX/NS_Thing --export SpawnScript_1 --trace Particles.Color
```

`asset edit-vm` gives one constant operand a new value, named by the instruction's offset and the
operand's number as the trace lists them. Literals are shared: the compiler keeps one of each value
and lets every instruction needing it read it, so an operand that is the only reader of its literal
has the literal rewritten, and one that shares it is pointed at a literal of its own, appended
after the others, leaving the rest reading the old value (`--retarget` asks for that even for a
sole reader). A value goes by the operand's type, a number for a float, or typed (`float:1`,
`int:-1`, `0x3F800000`) where the operand takes either. An engine value or a parameter can be
replaced the same way by a literal. In an edit file a `vm_consts` entry does the same, with `was`
holding what the operand read, as the trace keys it (`lit+48=0x3F400000`):

```json
{"edits": {"vm_consts": [{"export": "SpawnScript_1", "at": 189, "operand": 0, "value": "0.0", "was": "lit+44=0x41400000"}]}}
```

The save reads every script it changed back and holds it to the planned bytes. A script that
carries data for the experimental VM, which fixes where its constants are when it is compiled, or
whose bytecode is stored compressed, is refused; no shipped script has either.

`ini` and `tweaks` take `--pak`, a pak path or a mod's whole name in `~mods`. `--json` makes every
command emit machine-readable output, and failures exit non-zero. `--dry-run` reports what a write command
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

The toolkit installs a signature bypass so the game will load modified pak containers. It drops [Minimal ASI Loader](https://github.com/oZanderr/minimal-asi-loader) into the game's `Binaries/Win64` as `dsound.dll`, which loads the bypass payload `plugins/MarvelRivalsUTOCSignatureBypass.asi` (the original community build, redistributed unmodified). A third-party `dsound.dll` counts as installed too, since it loads the same payload. Anything left from the older `version.dll` scheme, the proxy itself or the superseded `RivalsSigBypass.asi` payload, reports as out of date, and Install clears it before writing the current pair. A `dsound.dll` matching a build the toolkit previously shipped reports out of date as well and is replaced, since the loader keeps its filename across releases and a stale one is otherwise invisible; loaders it does not recognize are left untouched. Remove deletes the payload, and deletes `dsound.dll` only when no other switched-on plugin needs it.

## Plugins

The Plugins tab manages the `.asi` plugins in `Binaries/Win64/plugins` and shows the proxy DLLs beside the game executable that load them.

- **Loaders.** A DLL named after one an ASI loader stands in for (`dsound.dll`, `version.dll`, `dxgi.dll`, `winmm.dll`, `dinput8.dll`, `d3d11.dll` and the like) is listed as a loader. Minimal ASI Loader (formerly oxiloader) is recognised by its export directory, Ultimate ASI Loader by its `IsUltimateASILoader` export, and anything else, ReShade for one, is shown with its version resource's product name. The tab warns when plugins are switched on with no loader to load them, when more than one loader is switched on, and when `dsound.dll` is a loader build the toolkit shipped and has since replaced, and offers to install or update the bundled loader on its own.
- **Where plugins are looked for.** Beside the game and in `plugins`, which is where Minimal ASI Loader looks, and with Ultimate ASI Loader also in `scripts` and one folder down in each. A plugin beside the game takes the place of one of the same name in `plugins`, which Minimal ASI Loader then skips.
- **Switching on and off.** A plugin or loader is switched off by adding `.disabled` to its name, which every loader skips. This works with the game open, since Windows lets a loaded DLL be renamed, and takes effect on the next launch.
- **Companion files.** Files beside a plugin that carry its name, such as `PigeonPass.ini` or `PigeonPass.log`, are shown with it. An `.ini` opens in the INI Editor, anything else in its default app. Deleting a plugin asks before deleting them, and keeps them unless asked, since they can hold settings or licence keys.
- **Installing.** Drop `.asi` files, folders, or zip, 7z and rar archives on the tab, or use Add. Each `.asi` goes into `plugins` with the files beside it that carry its name. A settings file already installed is kept rather than written over, a plugin already installed is replaced only once confirmed, and DLLs in what is dropped are skipped and reported, so a loader is never installed this way.
- **The signature bypass's files** (its payload, and the `dsound.dll` it installed) are tagged, and switching either off or deleting it first warns that mods in `~mods` stop loading without it.

## INI Editor

The INI Editor edits the INI files inside a mod's `.pak` and loose `.ini` files on disk alike. Its source list holds the `~mods` paks with INI files, a Plugins entry for the configs in `Binaries/Win64/plugins` (and a loader's own `.ini` beside the game), and any `.pak` or `.ini` opened with the browse button or dropped on the tab.

- **Paks** save by repacking, so they are held back while the game runs, and INIs can be added to or removed from them.
- **Loose files** are written where they are, through a temporary file so a failed write never truncates one. They save while the game runs, with a reminder that a plugin may rewrite its config when the game closes, and a file changed on disk since it was opened is refused until it is reloaded. Only `.ini` files are written, and read-only ones are refused.
- **Each file keeps its line endings,** whether CRLF or LF, and a loose file its encoding: UTF-8 with or without a byte order mark, or UTF-16.

## Logs

The Logs tab reads the game's engine logs and crash reports, which the game writes encrypted to `%LOCALAPPDATA%\Marvel\Saved`.

- **Sessions.** Every launch the game logged, the newest first, with how it ended: a clean exit, a crash, a fatal error with no crash report left for it, a log that stops with neither (the game was killed or the PC went down), or still running. A crash shows how many reports share its message and top stack frames.
- **Cause.** Shown only when the crash says so outright: the fatal error or the crash report names a package one of the session's mods ships, which names that mod, or the game crashed inside third-party code such as an injected DLL. The same card lists every other mod whose packages the session's errors and warnings name, with those errors; with no cause, they get a card of their own.
- **Summary.** The crash, with its message, the engine source line that raised it, its faulting module and its top stack frames, marking the game's own code apart from Windows, the GPU driver and third-party code such as ASI plugins and proxy DLLs, and listing every third-party module the game had loaded, with the Python the game thread was running when it died. A fatal error with no crash report gets a card of its own. Then every error and warning logged before the game went down, alike lines grouped. Errors the game also logs in its latest sessions with no mods mounted are folded away below the rest and never blamed on a mod. Clicking one opens the log at its first line.
- **Log.** The whole decoded log, filtered by verbosity, category and text, with jumps between errors. A log can decode to over 100 MB, so the app keeps it and the view reads only the lines on screen.
- **Copy log** puts the whole decoded log on the clipboard, and **Export** saves it as text. `rivals-cli logs` can redact it, or print a redacted summary, for sharing.
- **The key.** The logs are encrypted under a key built into the game, which the toolkit ships. If a patch changes it, Settings takes a new key and IV under Game Logs.

## License

This project is dual-licensed under either of the following, at your option:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))

### Bundled third-party components

The installed signature bypass includes [Minimal ASI Loader](https://github.com/oZanderr/minimal-asi-loader) (formerly oxiloader, shipped as `dsound.dll`), licensed under the MIT License, Copyright (c) 2026 oZanderr. Its license text is bundled at [src-tauri/resources/bypass/minimal-asi-loader-LICENSE.txt](src-tauri/resources/bypass/minimal-asi-loader-LICENSE.txt). The bypass payload `MarvelRivalsUTOCSignatureBypass.asi` is a community binary redistributed unmodified, with no license text accompanying it. Provenance and hashes for both files are recorded in [src-tauri/resources/bypass/NOTICE.md](src-tauri/resources/bypass/NOTICE.md).

Installing mods and plugins from `.rar` archives, in the desktop app and with
`rivals-cli mods install`, uses the [unrar](https://crates.io/crates/unrar) crate, which vendors
Alexander Roshal's UnRAR source. That source is used only to extract archives, never to create
them, and its license requires the following paragraph to be reproduced:

> UnRAR source code may be used in any software to handle RAR archives without limitations free of
> charge, but cannot be used to develop RAR (WinRAR) compatible archiver and to re-create RAR
> compression algorithm, which is proprietary. Distribution of modified UnRAR source code in
> separate form or as a part of other software is permitted, provided that full text of this
> paragraph, starting from "UnRAR source code" words, is included in license, or in documentation if
> license is not available, and in source code comments of resulting package.
