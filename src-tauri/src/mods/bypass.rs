//! Installs and removes the signature bypass that allows unsigned pak mods to load. Installs oxiloader as version.dll plus the bypass payload as a plugins/*.asi, and still detects and removes the legacy dsound.dll loader.

use std::fs;
use std::path::PathBuf;

use serde::Serialize;

use crate::paths::{binaries_dir, mods_dir};

use super::{BYPASS_ASI_LOADER, BYPASS_PAYLOAD_ASI};

const VERSION_DLL_FILENAME: &str = "version.dll";
const PAYLOAD_ASI_FILENAME: &str = "RivalsSigBypass.asi";
const LEGACY_ASI_FILENAME: &str = "MarvelRivalsUTOCSignatureBypass.asi";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum BypassKind {
    None,
    Legacy,
    Modern,
}

struct BypassPaths {
    dsound: PathBuf,
    asi: PathBuf,
    version_dll: PathBuf,
    payload_asi: PathBuf,
}

fn bypass_paths(game_root: &str) -> BypassPaths {
    let bin_dir = binaries_dir(game_root);
    let plugins = bin_dir.join("plugins");
    BypassPaths {
        dsound: bin_dir.join("dsound.dll"),
        asi: plugins.join(LEGACY_ASI_FILENAME),
        version_dll: bin_dir.join(VERSION_DLL_FILENAME),
        payload_asi: plugins.join(PAYLOAD_ASI_FILENAME),
    }
}

/// Modern = oxiloader `version.dll` plus our payload `.asi` in `plugins`.
/// Legacy pairs a generic `dsound.dll` with the specifically-named ASI loader so a stray
/// third-party `dsound.dll` isn't mistaken for ours. A leftover `version.dll` with no payload
/// (the old self-contained proxy) reports `None` so Install migrates it to the loader scheme.
pub(crate) fn bypass_install_kind(game_root: &str) -> BypassKind {
    let paths = bypass_paths(game_root);
    if paths.version_dll.exists() && paths.payload_asi.exists() {
        return BypassKind::Modern;
    }
    if paths.dsound.exists() && paths.asi.exists() {
        return BypassKind::Legacy;
    }
    BypassKind::None
}

pub(crate) fn is_signature_bypass_installed(game_root: &str) -> bool {
    bypass_install_kind(game_root) != BypassKind::None
}

pub(crate) fn install_signature_bypass(game_root: &str) -> Result<String, String> {
    if !BYPASS_ASI_LOADER.starts_with(b"MZ") || !BYPASS_PAYLOAD_ASI.starts_with(b"MZ") {
        return Err(
            "Bundled bypass binaries are placeholders. Put oxiloader's version.dll \
             (from the oZanderr/oxiloader release) and a RivalsSigBypass.asi (from the \
             oZanderr/rivals-sigbypass release) into src-tauri/resources/bypass/, \
             then rebuild the app."
                .to_string(),
        );
    }

    let bin_dir = binaries_dir(game_root);
    if !bin_dir.exists() {
        return Err(format!(
            "Binaries directory not found: {}\nMake sure the game root path is correct.",
            bin_dir.display()
        ));
    }

    match bypass_install_kind(game_root) {
        BypassKind::Modern => {
            return Ok("Signature bypass already installed.".to_string());
        }
        BypassKind::Legacy => {
            return Err(
                "Legacy bypass is installed. Remove it first to install the new bypass."
                    .to_string(),
            );
        }
        BypassKind::None => {}
    }

    let paths = bypass_paths(game_root);
    let migrating = paths.version_dll.exists();

    if let Some(plugins) = paths.payload_asi.parent() {
        fs::create_dir_all(plugins).map_err(|e| format!("create plugins dir: {e}"))?;
    }
    fs::write(&paths.version_dll, BYPASS_ASI_LOADER)
        .map_err(|e| format!("write version.dll: {e}"))?;
    fs::write(&paths.payload_asi, BYPASS_PAYLOAD_ASI)
        .map_err(|e| format!("write {PAYLOAD_ASI_FILENAME}: {e}"))?;

    if !mods_dir(game_root).exists() {
        fs::create_dir_all(mods_dir(game_root)).map_err(|e| e.to_string())?;
    }

    if migrating {
        Ok("Bypass upgraded to the ASI loader.".to_string())
    } else {
        Ok("Bypass installed successfully!".to_string())
    }
}

pub(crate) fn remove_signature_bypass(game_root: &str) -> Result<String, String> {
    let paths = bypass_paths(game_root);

    let mut removed = 0usize;
    for path in &[
        &paths.version_dll,
        &paths.dsound,
        &paths.asi,
        &paths.payload_asi,
    ] {
        if path.exists() {
            fs::remove_file(path).map_err(|e| e.to_string())?;
            removed += 1;
        }
    }

    // Drop the plugins dir if our removals left it empty.
    let plugins = binaries_dir(game_root).join("plugins");
    if plugins.is_dir()
        && plugins
            .read_dir()
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(false)
    {
        let _ = fs::remove_dir(&plugins);
    }

    if removed == 0 {
        Ok("Bypass files were not present!".to_string())
    } else {
        Ok(format!("Removed {removed} bypass file(s)"))
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// Fresh game root with an empty `Binaries/Win64`, unique per call so tests don't collide.
    fn scratch_game_root() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "rivals-bypass-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(binaries_dir(root.to_str().unwrap())).expect("create binaries dir");
        root
    }

    fn touch(path: &PathBuf) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, b"MZ").expect("write stub");
    }

    #[test]
    fn detects_none_when_empty() {
        let root = scratch_game_root();
        let gr = root.to_str().unwrap();
        assert_eq!(bypass_install_kind(gr), BypassKind::None);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn detects_modern_when_loader_and_payload_present() {
        let root = scratch_game_root();
        let gr = root.to_str().unwrap();
        let paths = bypass_paths(gr);
        touch(&paths.version_dll);
        touch(&paths.payload_asi);
        assert_eq!(bypass_install_kind(gr), BypassKind::Modern);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn detects_legacy_when_dsound_and_legacy_asi_present() {
        let root = scratch_game_root();
        let gr = root.to_str().unwrap();
        let paths = bypass_paths(gr);
        touch(&paths.dsound);
        touch(&paths.asi);
        assert_eq!(bypass_install_kind(gr), BypassKind::Legacy);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn version_dll_alone_reads_as_none_so_install_migrates() {
        let root = scratch_game_root();
        let gr = root.to_str().unwrap();
        let paths = bypass_paths(gr);
        touch(&paths.version_dll);
        assert_eq!(bypass_install_kind(gr), BypassKind::None);
        let _ = fs::remove_dir_all(&root);
    }
}
