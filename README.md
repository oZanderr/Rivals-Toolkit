# Rivals Toolkit

Rivals Toolkit is a desktop app for working with Marvel Rivals configuration and mod files.
It combines a React frontend with a Tauri/Rust backend to provide:

- game install detection across launchers
- pak browsing, extraction, and repacking
- mod management (status, toggle, export, delete)
- pak-based INI and game settings tweak tooling
- shader cache cleanup and game launch helpers

It also ships `rivals-cli`, a command-line tool over the same engine for scripting config tweaks and
pak INI edits. See [Command Line](#command-line).

Current platform support: Windows only.

## Tech Stack

- Frontend: React, TypeScript, Vite
- Backend: Tauri 2, Rust
- Layout: Cargo workspace. `crates/rivals-core` holds the pak and tweak engine and never depends on
  Tauri, which is what lets the CLI build without the frontend.
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
- `crates/rivals-core/`: pak and config-tweak engine shared by the app and the CLI
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
