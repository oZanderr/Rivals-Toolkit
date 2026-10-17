//! Marvel Rivals pak and config-tweak engine, free of any UI framework.
//!
//! The Tauri app and the CLI are both consumers of this crate. Nothing here may depend on tauri:
//! that is what lets the CLI build without the desktop app's frontend or build script.

#![deny(clippy::unwrap_used, clippy::expect_used)]

pub mod ability_ids;
pub mod ability_summary;
pub mod asset;
pub mod asset_edit;
pub mod game_logs;
pub mod game_search;
pub mod game_status;
pub mod import_index;
pub mod inherit;
pub mod ini_file;
pub mod localization;
pub mod mappings;
pub mod members;
pub mod mod_convert;
pub mod mod_diff;
pub mod mod_library;
pub mod mod_move;
pub mod mod_ops;
pub mod mod_replace;
pub mod mod_report;
pub mod mod_search;
pub mod mod_tidy;
pub mod mods;
pub mod object_check;
pub mod override_check;
pub mod pak;
pub mod pak_tweaks;
pub mod parallel;
pub mod paths;
pub mod plugins;
pub mod schema_synth;
pub mod script_objects;
pub mod texture;
pub mod tweaks;
pub mod value_diff;
