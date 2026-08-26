# Rivals Toolkit

Rivals Toolkit is a desktop app for working with Marvel Rivals configuration and mod files.
It combines a React frontend with a Tauri/Rust backend to provide:

- game install detection across launchers
- pak browsing, extraction, and repacking
- mod management (status, toggle, export, delete)
- pak-based INI and game settings tweak tooling
- shader cache cleanup and game launch helpers

Current platform support: Windows only.

## Tech Stack

- Frontend: React, TypeScript, Vite
- Backend: Tauri 2, Rust
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

## Project Layout

- `src/`: React frontend
- `src-tauri/src/`: Rust backend and Tauri commands
- `src-tauri/resources/`: bundled runtime resources (for example bypass files)

## Signature Bypass

The toolkit installs a signature bypass so the game will load modified pak containers. It drops [oxiloader](https://github.com/oZanderr/oxiloader) into the game's `Binaries/Win64` as `dsound.dll`, which loads the bypass payload `plugins/MarvelRivalsUTOCSignatureBypass.asi` (the original community build, redistributed unmodified). A third-party `dsound.dll` counts as installed too, since it loads the same payload. Anything left from the older `version.dll` scheme, the proxy itself or the superseded `RivalsSigBypass.asi` payload, reports as out of date, and Install clears it before writing the current pair.

## License

This project is dual-licensed under either of the following, at your option:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))

### Bundled third-party components

The installed signature bypass includes [oxiloader](https://github.com/oZanderr/oxiloader) (shipped as `dsound.dll`), licensed under the MIT License, Copyright (c) 2026 oZanderr. Its license text is bundled at [src-tauri/resources/bypass/oxiloader-LICENSE.txt](src-tauri/resources/bypass/oxiloader-LICENSE.txt). The bypass payload `MarvelRivalsUTOCSignatureBypass.asi` is a community binary redistributed unmodified, with no license text accompanying it. Provenance and hashes for both files are recorded in [src-tauri/resources/bypass/NOTICE.md](src-tauri/resources/bypass/NOTICE.md).
