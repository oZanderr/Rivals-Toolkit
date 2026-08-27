//! Marvel Rivals pak and config-tweak engine, free of any UI framework.
//!
//! The Tauri app and the CLI are both consumers of this crate. Nothing here may depend on tauri:
//! that is what lets the CLI build without the desktop app's frontend or build script.

#![deny(clippy::unwrap_used, clippy::expect_used)]

pub mod game_status;
pub mod mods;
pub mod pak;
pub mod pak_tweaks;
pub mod paths;
pub mod tweaks;
