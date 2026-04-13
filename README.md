# Rivals Toolkit

Rivals Toolkit is a desktop app for working with Marvel Rivals configuration and mod files.
It combines a React frontend with a Tauri/Rust backend to provide:

- game install detection across launchers
- pak browsing, extraction, and repacking
- mod management (status, toggle, export, delete)
- scalability and pak-based INI tweak tooling
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

## License

This project is dual-licensed under either of the following, at your option:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))
