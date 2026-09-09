//! Marvel Rivals pak and config-tweak engine, free of any UI framework.
//!
//! The Tauri app and the CLI are both consumers of this crate. Nothing here may depend on tauri:
//! that is what lets the CLI build without the desktop app's frontend or build script.

#![deny(clippy::unwrap_used, clippy::expect_used)]

pub mod asset;
pub mod asset_edit;
pub mod game_status;
pub mod import_index;
pub mod mappings;
pub mod mods;
pub mod pak;
pub mod pak_tweaks;
pub mod paths;
pub mod schema_synth;
pub mod tweaks;
