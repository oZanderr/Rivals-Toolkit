//! Installs and removes the signature bypass that allows unsigned pak mods to load. Installs Minimal ASI Loader as dsound.dll plus the bundled UTOC bypass payload as a plugins/*.asi, clears the version.dll loader and in-house payload left behind by earlier releases, and keeps the loader on removal while other plugins need it.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::paths::{binaries_dir, mods_dir, plugins_dir};

use super::{BYPASS_ASI_LOADER, BYPASS_PAYLOAD_ASI};

const LOADER_DLL_FILENAME: &str = "dsound.dll";
const PAYLOAD_ASI_FILENAME: &str = "MarvelRivalsUTOCSignatureBypass.asi";
const SUPERSEDED_DLL_FILENAME: &str = "version.dll";
const SUPERSEDED_ASI_FILENAME: &str = "RivalsSigBypass.asi";

/// SHA-256 of `dsound.dll` builds this toolkit shipped and has since replaced.
///
/// The loader keeps its filename across releases, so a bad build can only be spotted by content.
/// Only builds we shipped are listed: an unrecognized `dsound.dll` is someone's own choice of
/// loader and is left alone. Add the outgoing hash here whenever the bundled loader changes.
const SUPERSEDED_LOADER_SHA256: &[&str] = &[
    // Minimal ASI Loader v0.2.1, released as oxiloader, which crashes the game.
    "2ec12163ddcba1182a6008848c66f42d0f48647c82ff107789479cc0e9dcbf2c",
    // The self-contained proxy shipped before the loader/payload split, same filename.
    "bb8767f918c52a2ad055d2de9baffd2478598643b9894f09abd20d1f1ffd170c",
];

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().iter().fold(String::new(), |mut acc, b| {
        use std::fmt::Write;
        let _ = write!(acc, "{b:02x}");
        acc
    })
}

/// True when the installed loader is a build we shipped and have since replaced. An unreadable
/// file is not treated as stale, so a locked DLL never triggers a rewrite that would fail anyway.
fn loader_is_superseded(loader_dll: &std::path::Path) -> bool {
    loader_matches_any(loader_dll, SUPERSEDED_LOADER_SHA256)
}

fn loader_matches_any(loader_dll: &std::path::Path, hashes: &[&str]) -> bool {
    let Ok(bytes) = fs::read(loader_dll) else {
        return false;
    };
    let digest = sha256_hex(&bytes);
    hashes.contains(&digest.as_str())
}

/// The part a file beside the game or in `plugins` plays in the bypass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BypassRole {
    Loader,
    /// A loader build this toolkit shipped and has since replaced.
    OutdatedLoader,
    Payload,
}

fn switched_off(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".disabled");
    PathBuf::from(name)
}

/// The payload is known by its name, the loader by its content, so a `dsound.dll` someone else put
/// there is never claimed. Either may be switched off.
pub(crate) fn bypass_role(path: &Path) -> Option<BypassRole> {
    let name = path.file_name()?.to_string_lossy();
    let base = name.strip_suffix(".disabled").unwrap_or(&name);
    if base.eq_ignore_ascii_case(PAYLOAD_ASI_FILENAME) {
        return Some(BypassRole::Payload);
    }
    if !base.eq_ignore_ascii_case(LOADER_DLL_FILENAME) {
        return None;
    }
    let digest = sha256_hex(&fs::read(path).ok()?);
    if digest == sha256_hex(BYPASS_ASI_LOADER) {
        Some(BypassRole::Loader)
    } else if SUPERSEDED_LOADER_SHA256.contains(&digest.as_str()) {
        Some(BypassRole::OutdatedLoader)
    } else {
        None
    }
}

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
    let plugins = plugins_dir(game_root);
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
/// already in place, so Install gets a chance to clear them. A loader matching a build we shipped
/// and have since replaced is `Outdated` too, which is the only way a bad loader reaches users who
/// already installed one, since the filename never changes.
pub(crate) fn bypass_install_kind(game_root: &str) -> BypassKind {
    let paths = bypass_paths(game_root);
    if paths.superseded_dll.exists() || paths.superseded_asi.exists() {
        return BypassKind::Outdated;
    }
    if paths.loader_dll.exists() && paths.payload_asi.exists() {
        if loader_is_superseded(&paths.loader_dll) {
            return BypassKind::Outdated;
        }
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
            "Bundled bypass binaries are placeholders. Put Minimal ASI Loader's dsound.dll \
             (from the oZanderr/minimal-asi-loader release) and the original \
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
    // Switched-off copies of ours go too, so a fresh copy never lands beside one.
    let mut leftovers = vec![
        paths.superseded_asi.clone(),
        paths.superseded_dll.clone(),
        switched_off(&paths.payload_asi),
    ];
    let loader_off = switched_off(&paths.loader_dll);
    if bypass_role(&loader_off).is_some() {
        leftovers.push(loader_off);
    }
    for stale in &leftovers {
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
        &paths.payload_asi,
        &switched_off(&paths.payload_asi),
        &paths.superseded_dll,
        &paths.superseded_asi,
    ] {
        if path.exists() {
            fs::remove_file(path).map_err(|e| e.to_string())?;
            removed += 1;
        }
    }

    // The loader loads every other plugin too, so it stays while any of them is switched on.
    let others = rivals_core::plugins::scan(game_root)
        .plugins
        .iter()
        .filter(|plugin| plugin.enabled)
        .count();
    let kept_loader = paths.loader_dll.exists() && others > 0;
    if paths.loader_dll.exists() && !kept_loader {
        fs::remove_file(&paths.loader_dll).map_err(|e| e.to_string())?;
        removed += 1;
    }

    // Drop the plugins dir if our removals left it empty.
    let plugins = plugins_dir(game_root);
    if plugins.is_dir()
        && plugins
            .read_dir()
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(false)
    {
        let _ = fs::remove_dir(&plugins);
    }

    if kept_loader {
        Ok(format!(
            "Removed {removed} bypass file(s) and kept {LOADER_DLL_FILENAME}, which {others} other \
             plugin(s) still need"
        ))
    } else if removed == 0 {
        Ok("Bypass files were not present!".to_string())
    } else {
        Ok(format!("Removed {removed} bypass file(s)"))
    }
}

/// Writes the bundled loader alone, for plugins with nothing to load them, or over an outdated
/// build of ours. A `dsound.dll` of anyone else's is left alone, and so is another loader already
/// switched on, which would load the same plugins.
pub(crate) fn install_asi_loader(game_root: &str) -> Result<String, String> {
    if !BYPASS_ASI_LOADER.starts_with(b"MZ") {
        return Err(
            "The bundled loader is a placeholder. Put Minimal ASI Loader's dsound.dll into \
             src-tauri/resources/bypass/, then rebuild the app."
                .to_string(),
        );
    }
    let bin_dir = binaries_dir(game_root);
    if !bin_dir.is_dir() {
        return Err(format!(
            "Binaries directory not found: {}\nMake sure the game root path is correct.",
            bin_dir.display()
        ));
    }
    let paths = bypass_paths(game_root);
    let updating = paths.loader_dll.exists();
    if updating {
        match bypass_role(&paths.loader_dll) {
            Some(BypassRole::Loader) => {
                return Ok(format!(
                    "{LOADER_DLL_FILENAME} is already the current loader."
                ));
            }
            Some(BypassRole::OutdatedLoader) => {}
            _ => {
                return Err(format!(
                    "{LOADER_DLL_FILENAME} is already there and is not one the toolkit \
                     installed, so it is left alone."
                ));
            }
        }
    }
    let status = rivals_core::plugins::scan(game_root);
    if let Some(other) = status.loaders.iter().find(|loader| {
        loader.enabled
            && loader.kind != rivals_core::plugins::LoaderKind::Unknown
            && Path::new(&loader.path) != paths.loader_dll
    }) {
        return Err(format!(
            "{} is switched on and already loads plugins.",
            other.file_name
        ));
    }
    let loader_off = switched_off(&paths.loader_dll);
    if bypass_role(&loader_off).is_some() {
        fs::remove_file(&loader_off)
            .map_err(|e| format!("remove {}: {e}", loader_off.display()))?;
    }
    fs::write(&paths.loader_dll, BYPASS_ASI_LOADER)
        .map_err(|e| format!("write {LOADER_DLL_FILENAME}: {e}"))?;
    Ok(if updating {
        format!("Updated {LOADER_DLL_FILENAME} to the current Minimal ASI Loader.")
    } else {
        format!("Installed Minimal ASI Loader as {LOADER_DLL_FILENAME}.")
    })
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
    fn sha256_hex_matches_a_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn a_loader_matching_a_superseded_hash_reads_as_outdated() {
        let root = scratch_game_root();
        let gr = root.to_str().unwrap();
        let paths = bypass_paths(gr);
        touch(&paths.loader_dll);
        touch(&paths.payload_asi);

        let digest = sha256_hex(&fs::read(&paths.loader_dll).expect("loader"));
        assert!(loader_matches_any(&paths.loader_dll, &[&digest]));
        assert!(!loader_matches_any(&paths.loader_dll, &[]));
        let _ = fs::remove_dir_all(&root);
    }

    /// A loader we do not recognize belongs to whoever put it there, so it must keep reading as
    /// installed rather than being replaced.
    #[test]
    fn an_unrecognized_loader_is_left_alone() {
        let root = scratch_game_root();
        let gr = root.to_str().unwrap();
        let paths = bypass_paths(gr);
        touch(&paths.loader_dll);
        touch(&paths.payload_asi);

        assert!(!loader_is_superseded(&paths.loader_dll));
        assert_eq!(bypass_install_kind(gr), BypassKind::Installed);
        let _ = fs::remove_dir_all(&root);
    }

    /// Listing the shipped build as superseded would make every install report Outdated forever.
    #[test]
    fn the_bundled_loader_is_not_itself_superseded() {
        let digest = sha256_hex(BYPASS_ASI_LOADER);
        assert!(
            !SUPERSEDED_LOADER_SHA256.contains(&digest.as_str()),
            "the bundled loader ({digest}) is listed as superseded"
        );
    }

    #[test]
    fn remove_keeps_the_loader_while_other_plugins_need_it() {
        let root = scratch_game_root();
        let gr = root.to_str().unwrap();
        let paths = bypass_paths(gr);
        touch(&paths.loader_dll);
        touch(&paths.payload_asi);
        touch(&plugins_dir(gr).join("Other.asi"));

        let msg = remove_signature_bypass(gr).expect("remove");

        assert!(msg.contains("kept dsound.dll"), "{msg}");
        assert!(!paths.payload_asi.exists());
        assert!(paths.loader_dll.exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn remove_takes_the_loader_when_nothing_else_is_switched_on() {
        let root = scratch_game_root();
        let gr = root.to_str().unwrap();
        let paths = bypass_paths(gr);
        touch(&paths.loader_dll);
        touch(&paths.payload_asi);
        touch(&plugins_dir(gr).join("Other.asi.disabled"));

        remove_signature_bypass(gr).expect("remove");

        assert!(!paths.loader_dll.exists());
        assert!(plugins_dir(gr).join("Other.asi.disabled").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn install_clears_a_switched_off_payload() {
        let root = scratch_game_root();
        let gr = root.to_str().unwrap();
        let paths = bypass_paths(gr);
        touch(&switched_off(&paths.payload_asi));

        install_signature_bypass(gr).expect("install");

        assert!(paths.payload_asi.exists());
        assert!(!switched_off(&paths.payload_asi).exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn roles_go_by_name_for_the_payload_and_by_content_for_the_loader() {
        let root = scratch_game_root();
        let gr = root.to_str().unwrap();
        let paths = bypass_paths(gr);
        touch(&paths.loader_dll);
        assert_eq!(
            bypass_role(&paths.loader_dll),
            None,
            "a dsound.dll of another's"
        );
        fs::write(&paths.loader_dll, BYPASS_ASI_LOADER).expect("write loader");
        assert_eq!(bypass_role(&paths.loader_dll), Some(BypassRole::Loader));
        assert_eq!(
            bypass_role(&switched_off(&paths.payload_asi)),
            Some(BypassRole::Payload)
        );
        assert_eq!(bypass_role(&plugins_dir(gr).join("Other.asi")), None);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_bundled_loader_reads_as_minimal_asi_loader() {
        if !BYPASS_ASI_LOADER.starts_with(b"MZ") {
            return;
        }
        let (kind, _) = rivals_core::plugins::identify(BYPASS_ASI_LOADER);
        assert_eq!(kind, rivals_core::plugins::LoaderKind::MinimalAsiLoader);
    }

    #[test]
    fn the_loader_alone_installs_beside_plugins_and_leaves_another_dsound_alone() {
        let root = scratch_game_root();
        let gr = root.to_str().unwrap();
        let paths = bypass_paths(gr);
        touch(&plugins_dir(gr).join("Other.asi"));

        install_asi_loader(gr).expect("install loader");
        assert_eq!(
            fs::read(&paths.loader_dll).expect("loader"),
            BYPASS_ASI_LOADER
        );
        assert!(!paths.payload_asi.exists());

        fs::write(&paths.loader_dll, b"MZ").expect("swap");
        assert!(install_asi_loader(gr).is_err());
        assert_eq!(fs::read(&paths.loader_dll).expect("loader"), b"MZ");
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
