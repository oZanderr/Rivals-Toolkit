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

/// The app's progress events for a conversion's two halves.
fn progress_to(app: &AppHandle) -> impl Fn(ConvertPhase, usize, usize) + Sync + '_ {
    move |phase, current, total| {
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
    }
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
    conform: bool,
    mappings: Option<std::sync::Arc<rivals_uasset::Mappings>>,
    app: AppHandle,
) -> Result<InPlaceReport, String> {
    CONVERT_CANCEL.store(false, Ordering::Relaxed);
    let files = ModFiles::of(Path::new(mod_pak))?;
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
            conform,
            mappings,
        },
        false,
        Some(&concurrency::POOL),
        &CONVERT_CANCEL,
        &progress_to(&app),
    )?;
    super::invalidate_list_caches(&[&files.part("pak"), &files.part("utoc")]);
    Ok(report)
}

/// Brings an installed IoStore mod a stock editor cooked to this game's layout where it is. A mod
/// with nothing to convert is left as it was.
pub(crate) fn conform_mod_in_place(
    game_root: &str,
    mod_pak: &str,
    oodle_level: Option<retoc::OodleCompressionLevel>,
    compression: super::profile::PackCompression,
    mappings: Option<std::sync::Arc<rivals_uasset::Mappings>>,
    app: AppHandle,
) -> Result<InPlaceReport, String> {
    CONVERT_CANCEL.store(false, Ordering::Relaxed);
    let files = ModFiles::of(Path::new(mod_pak))?;
    let report = mod_convert::conform_mod(
        game_root,
        &files,
        &IoStoreOptions {
            compression,
            oodle_level,
            mappings,
            ..Default::default()
        },
        false,
        &CONVERT_CANCEL,
        &progress_to(&app),
    )?;
    if report.written {
        super::invalidate_list_caches(&[&files.part("pak"), &files.part("utoc")]);
    }
    Ok(report)
}
