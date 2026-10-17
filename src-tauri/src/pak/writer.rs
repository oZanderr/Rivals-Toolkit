//! Pak file creation and repacking from directory trees.

use std::path::{Path, PathBuf};

use retoc::OodleCompressionLevel;
use rivals_core::pak::plain::{self, PakOptions};

use super::profile::PackCompression;

pub(super) fn write_empty_pak(
    output_pak: &str,
    oodle_level: Option<OodleCompressionLevel>,
) -> Result<(), String> {
    plain::write_files(
        Path::new(output_pak),
        Vec::new(),
        &PakOptions {
            compression: PackCompression::default(),
            oodle_level,
        },
    )
}

pub(super) fn write_pak_bytes(
    output_pak: &str,
    files: Vec<(String, Vec<u8>)>,
    oodle_level: Option<OodleCompressionLevel>,
    compression: PackCompression,
) -> Result<(), String> {
    if files.is_empty() {
        return Err("No files provided for pak build.".to_string());
    }
    plain::write_files(
        Path::new(output_pak),
        files,
        &PakOptions {
            compression,
            oodle_level,
        },
    )
}

/// Each entry is `(pak_path, source_file, compress)`. `compress = false` stores the entry
/// uncompressed, reproducing entries the source pak shipped raw.
pub(super) fn write_pak_streaming(
    output_pak: &str,
    entries: Vec<(String, PathBuf, bool)>,
    oodle_level: Option<OodleCompressionLevel>,
) -> Result<(), String> {
    if entries.is_empty() {
        return Err("No files provided for pak build.".to_string());
    }
    plain::write_streaming(
        Path::new(output_pak),
        entries,
        &PakOptions {
            compression: PackCompression::default(),
            oodle_level,
        },
    )
}

pub(super) fn repack_pak(
    input_dir: &str,
    output_pak: &str,
    oodle_level: Option<OodleCompressionLevel>,
    compression: PackCompression,
) -> Result<(), String> {
    plain::pack_dir(
        Path::new(input_dir),
        Path::new(output_pak),
        &PakOptions {
            compression,
            oodle_level,
        },
    )
    .map(|_| ())
}
