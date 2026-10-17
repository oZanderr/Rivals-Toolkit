//! Repacks an installed mod in place, through the core conversion, with the app's progress events
//! and cancel buttons wired to it.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use rivals_core::mod_convert::{self, ConvertPhase, ModFormat};
use rivals_core::mod_ops::ModFiles;
use rivals_core::pak::iostore_out::IoStoreOptions;
use tauri::{AppHandle, Emitter};

use super::iostore::{LegacyExtractionProgress, RepackProgress};
use crate::concurrency;

pub(crate) type InPlaceReport = mod_convert::ConvertReport;

/// Set by either cancel button: the extraction's or the repack's, whichever half is running.
static CONVERT_CANCEL: AtomicBool = AtomicBool::new(false);

pub(super) fn cancel() {
    CONVERT_CANCEL.store(true, Ordering::Relaxed);
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn repack_mod_in_place(
    game_root: &str,
    mod_pak: &str,
    to_iostore: bool,
    obfuscate: bool,
    recompress: bool,
    oodle_level: Option<retoc::OodleCompressionLevel>,
    compression: super::profile::PackCompression,
    app: AppHandle,
) -> Result<InPlaceReport, String> {
    CONVERT_CANCEL.store(false, Ordering::Relaxed);
    let files = ModFiles::of(Path::new(mod_pak))?;
    let progress = |phase: ConvertPhase, current: usize, total: usize| {
        let _ = match phase {
            ConvertPhase::Extracting => app.emit(
                "legacy-extraction-progress",
                LegacyExtractionProgress { current, total },
            ),
            ConvertPhase::Repacking => app.emit(
                "repack-iostore-progress",
                RepackProgress {
                    phase: "repacking",
                    current,
                    total,
                },
            ),
        };
    };
    let report = mod_convert::convert_mod(
        game_root,
        &files,
        if to_iostore {
            ModFormat::Iostore
        } else {
            ModFormat::Pak
        },
        &IoStoreOptions {
            compression,
            oodle_level,
            obfuscate: Some(obfuscate),
            reencode: recompress,
        },
        false,
        Some(&concurrency::POOL),
        &CONVERT_CANCEL,
        &progress,
    )?;
    super::invalidate_list_caches(&[&files.part("pak"), &files.part("utoc")]);
    Ok(report)
}
