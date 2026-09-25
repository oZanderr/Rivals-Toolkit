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

There is no JavaScript test framework. `pnpm lint` and `pnpm exec tsc --noEmit` cover the frontend.

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

`asset script-set` changes one literal constant inside a function's bytecode, addressed by the
statement offset `asset script` prints and which literal in that statement, and only at the width
the old value took: an `IntConst` can become another integer, a string another string of the same
length, but a one-byte `IntZero` cannot grow into a four-byte constant without moving every jump
after it, so that is refused. `--dry-run` shows the change without writing.


```bash
rivals-cli asset list  --container pakchunk0-Windows.utoc --filter DataTable
rivals-cli asset info  --container pakchunk0-Windows.utoc --entry Marvel/Content/.../DT_Thing.uasset
rivals-cli asset dump  --container ... --entry ... --declared      # the decoded property tree
rivals-cli asset table --container ... --entry ...                 # a DataTable as rows
rivals-cli asset trace --container ... --entry ... --export 0      # the bytes each property took

rivals-cli asset set --container ... --entry ...   --offset 0xBE6 --kind float --name Damage --value 42.5 --mod-name MyMod
rivals-cli asset row     --container ... --entry ... --export 0 --op add --row NewRow
rivals-cli asset strings --container ... --entry ... --export 0 --op set-source --index 3 --to "Hello"

rivals-cli asset export-edit --container ... --entry ... --export 3 --rename NewName   # header rows
rivals-cli asset export-edit --container ... --entry ... --export 3 --class -32         # retype
rivals-cli asset imports --container ... --entry ... --unused                           # tidy the table
rivals-cli asset deps    --container ... --entry ... --export 3                         # load order
rivals-cli asset script  --container ... --entry ... --export 26                        # disassembly
rivals-cli asset script-set --container ... --entry ... --export 26 --statement 0x0664 --const 0 --value 1000 --mod-name MyMod
rivals-cli asset copy-export --container ... --entry ... --from-container ... --from-entry ... --export 274 --name MyLight

rivals-cli asset audit --container pakchunk0-Windows.utoc --filter Data/DataTable
rivals-cli asset audit --container pakchunk0-Windows.utoc --skip-blueprint   # native classes only
```

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

What the diff cannot express it writes as a note beside the edits rather than guessing. The edits
beside a note still apply. The limits worth knowing:

- **Offsets are one-shot.** An edit file addresses the package state its dump came from. Applying it
  twice, or applying two files to one entry expecting them to stack, does not work: dump the saved
  copy and diff again from there.
- **Adding needs a second pass.** A new container element, table row or stored struct is created
  with its default; dump the result and diff again to give it a value.
- **Some changes are not value edits.** Retyping a property, renaming an export, reordering a
  container, changing a map key, and editing a delegate or a field path are reported, not written.
- **Bytes are not in the JSON.** Payload and bulk data are named by file in an edit list, never
  dumped inline.

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
