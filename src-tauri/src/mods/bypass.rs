//! Installs and removes the signature bypass that allows unsigned pak mods to load. Installs oxiloader as dsound.dll plus the bundled UTOC bypass payload as a plugins/*.asi, and clears the version.dll loader and in-house payload left behind by earlier releases.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::paths::{binaries_dir, mods_dir};

use super::{BYPASS_ASI_LOADER, BYPASS_PAYLOAD_ASI};

const LOADER_DLL_FILENAME: &str = "dsound.dll";
const PAYLOAD_ASI_FILENAME: &str = "MarvelRivalsUTOCSignatureBypass.asi";
const SUPERSEDED_DLL_FILENAME: &str = "version.dll";
const SUPERSEDED_ASI_FILENAME: &str = "RivalsSigBypass.asi";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum BypassKind {
    None,
    Outdated,
    Installed,
}

struct BypassPaths {
    loader_dll: PathBuf,
    payload_asi: PathBuf,
    superseded_dll: PathBuf,
    superseded_asi: PathBuf,
}

fn bypass_paths(game_root: &str) -> BypassPaths {
    let bin_dir = binaries_dir(game_root);
    let plugins = bin_dir.join("plugins");
    BypassPaths {
        loader_dll: bin_dir.join(LOADER_DLL_FILENAME),
        payload_asi: plugins.join(PAYLOAD_ASI_FILENAME),
        superseded_dll: bin_dir.join(SUPERSEDED_DLL_FILENAME),
        superseded_asi: plugins.join(SUPERSEDED_ASI_FILENAME),
    }
}

/// Installed = a `dsound.dll` loader plus the payload `.asi` in `plugins`. A third-party
/// `dsound.dll` counts, since it loads the same payload and there is nothing to fix.
/// Leftovers from the older `version.dll` scheme report `Outdated` even when that pair is
/// already in place, so Install gets a chance to clear them.
pub(crate) fn bypass_install_kind(game_root: &str) -> BypassKind {
    let paths = bypass_paths(game_root);
    if paths.superseded_dll.exists() || paths.superseded_asi.exists() {
        return BypassKind::Outdated;
    }
    if paths.loader_dll.exists() && paths.payload_asi.exists() {
        return BypassKind::Installed;
    }
    BypassKind::None
}

pub(crate) fn is_signature_bypass_installed(game_root: &str) -> bool {
    bypass_install_kind(game_root) != BypassKind::None
}

pub(crate) fn install_signature_bypass(game_root: &str) -> Result<String, String> {
    if !BYPASS_ASI_LOADER.starts_with(b"MZ") || !BYPASS_PAYLOAD_ASI.starts_with(b"MZ") {
        return Err(
            "Bundled bypass binaries are placeholders. Put oxiloader's dsound.dll \
             (from the oZanderr/oxiloader release) and the original \
             MarvelRivalsUTOCSignatureBypass.asi into src-tauri/resources/bypass/, \
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

    let kind = bypass_install_kind(game_root);
    if kind == BypassKind::Installed {
        return Ok("Signature bypass already installed.".to_string());
    }

    let paths = bypass_paths(game_root);

    // Cleared before anything is written, so a locked file leaves the existing install intact.
    // The old payload is the build the game flags, and its proxy has nothing left to load.
    for stale in [&paths.superseded_asi, &paths.superseded_dll] {
        if stale.exists() {
            fs::remove_file(stale).map_err(|e| {
                format!(
                    "remove {}: {e}\nClose the game and try again.",
                    stale.display()
                )
            })?;
        }
    }

    if let Some(plugins) = paths.payload_asi.parent() {
        fs::create_dir_all(plugins).map_err(|e| format!("create plugins dir: {e}"))?;
    }
    fs::write(&paths.loader_dll, BYPASS_ASI_LOADER)
        .map_err(|e| format!("write {LOADER_DLL_FILENAME}: {e}"))?;
    fs::write(&paths.payload_asi, BYPASS_PAYLOAD_ASI)
        .map_err(|e| format!("write {PAYLOAD_ASI_FILENAME}: {e}"))?;

    if !mods_dir(game_root).exists() {
        fs::create_dir_all(mods_dir(game_root)).map_err(|e| e.to_string())?;
    }

    if kind == BypassKind::Outdated {
        Ok("Bypass updated to the current loader and payload.".to_string())
    } else {
        Ok("Bypass installed successfully!".to_string())
    }
}

pub(crate) fn remove_signature_bypass(game_root: &str) -> Result<String, String> {
    let paths = bypass_paths(game_root);

    let mut removed = 0usize;
    for path in &[
        &paths.loader_dll,
        &paths.payload_asi,
        &paths.superseded_dll,
        &paths.superseded_asi,
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
    fn detects_installed_when_loader_and_payload_present() {
        let root = scratch_game_root();
        let gr = root.to_str().unwrap();
        let paths = bypass_paths(gr);
        touch(&paths.loader_dll);
        touch(&paths.payload_asi);
        assert_eq!(bypass_install_kind(gr), BypassKind::Installed);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn loader_without_payload_reads_as_none() {
        let root = scratch_game_root();
        let gr = root.to_str().unwrap();
        let paths = bypass_paths(gr);
        touch(&paths.loader_dll);
        assert_eq!(bypass_install_kind(gr), BypassKind::None);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn version_dll_leftover_reads_as_outdated() {
        let root = scratch_game_root();
        let gr = root.to_str().unwrap();
        let paths = bypass_paths(gr);
        touch(&paths.superseded_dll);
        touch(&paths.payload_asi);
        assert_eq!(bypass_install_kind(gr), BypassKind::Outdated);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn superseded_payload_reads_as_outdated() {
        let root = scratch_game_root();
        let gr = root.to_str().unwrap();
        let paths = bypass_paths(gr);
        touch(&paths.loader_dll);
        touch(&paths.superseded_asi);
        assert_eq!(bypass_install_kind(gr), BypassKind::Outdated);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn install_clears_the_version_dll_scheme() {
        let root = scratch_game_root();
        let gr = root.to_str().unwrap();
        let paths = bypass_paths(gr);
        touch(&paths.superseded_dll);
        touch(&paths.superseded_asi);

        let msg = install_signature_bypass(gr).expect("install");

        assert_eq!(msg, "Bypass updated to the current loader and payload.");
        assert!(!paths.superseded_dll.exists());
        assert!(!paths.superseded_asi.exists());
        assert_eq!(
            fs::read(&paths.loader_dll).expect("loader"),
            BYPASS_ASI_LOADER
        );
        assert_eq!(
            fs::read(&paths.payload_asi).expect("payload"),
            BYPASS_PAYLOAD_ASI
        );
        assert_eq!(bypass_install_kind(gr), BypassKind::Installed);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn install_leaves_a_third_party_loader_alone() {
        let root = scratch_game_root();
        let gr = root.to_str().unwrap();
        let paths = bypass_paths(gr);
        touch(&paths.loader_dll);
        touch(&paths.payload_asi);

        let msg = install_signature_bypass(gr).expect("install");

        assert_eq!(msg, "Signature bypass already installed.");
        assert_eq!(fs::read(&paths.loader_dll).expect("loader"), b"MZ");
        let _ = fs::remove_dir_all(&root);
    }
}
