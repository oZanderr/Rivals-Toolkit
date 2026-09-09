//! Decides which IoStore containers under a Paks folder can be opened, and opens them.

use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use retoc::iostore::IoStoreTrait;
use walkdir::WalkDir;

pub const MOUNT_POINT: &str = "../../../";
const TOC_MAGIC: &[u8; 16] = b"-==--==--==--==-";
const TOC_ENCRYPTED_FLAG: u8 = 0b0010;

/// The app holds the default (all-zero) GUID key plus the named keys in
/// [`super::profile::NAMED_AES_KEYS`]. A container encrypted under any other key GUID cannot be
/// decrypted; probing the TOC header for it lets callers skip such a container instead of
/// aborting a whole merged open with "missing encryption key" (a future game patch could ship a
/// container keyed to a GUID we do not have).
pub fn utoc_is_decryptable(utoc_path: &Path) -> bool {
    let Some(header) = read_toc_header_probe(utoc_path) else {
        return false;
    };
    if header[80] & TOC_ENCRYPTED_FLAG == 0 {
        return true;
    }
    let guid = &header[64..80];
    guid.iter().all(|&b| b == 0)
        || super::profile::NAMED_AES_KEYS
            .iter()
            .any(|(known, _)| known == guid)
}

/// Whether a container is obfuscated, meaning it carries the Encrypted flag. Mods use it with the
/// game's own default-GUID key, so this is a presentation detail rather than a barrier: it seeds
/// the repack option so an obfuscated mod does not come back out plain.
pub fn utoc_is_obfuscated(utoc_path: &Path) -> bool {
    read_toc_header_probe(utoc_path).is_some_and(|header| header[80] & TOC_ENCRYPTED_FLAG != 0)
}

/// The leading 81 bytes of a TOC header, covering the magic, the encryption-key GUID (offset 64)
/// and the container flags (offset 80). `None` when the file is unreadable or is not a TOC.
fn read_toc_header_probe(utoc_path: &Path) -> Option<[u8; 81]> {
    let mut file = std::fs::File::open(utoc_path).ok()?;
    let mut header = [0u8; 81];
    if file.read_exact(&mut header).is_err() || &header[0..16] != TOC_MAGIC {
        return None;
    }
    Some(header)
}

/// Container stems under `paks_dir` encrypted with a key GUID the app does not hold. These
/// poison a merged `open_filtered` with "missing encryption key", so callers exclude them
/// from the container filter.
pub fn undecryptable_container_stems(paks_dir: &Path) -> HashSet<String> {
    utoc_stems(paks_dir, |p| !utoc_is_decryptable(p))
}

/// Container stems that live under `~mods`. Identifying mods by folder rather than by the
/// `_9999999_` naming convention matters because a plainly named mod follows no convention, and
/// letting one into the base-game store makes it resolve as vanilla content.
pub fn mod_container_stems(paks_dir: &Path) -> HashSet<String> {
    utoc_stems(&paks_dir.join("~mods"), |_| true)
}

fn utoc_stems(root: &Path, keep: impl Fn(&Path) -> bool) -> HashSet<String> {
    utoc_paths(root)
        .filter(|p| keep(p))
        .filter_map(|p| p.file_stem().and_then(|s| s.to_str()).map(str::to_string))
        .collect()
}

/// Every `.utoc` under `root`, `~mods` included, which is the set the merged open considers.
fn utoc_paths(root: &Path) -> impl Iterator<Item = PathBuf> {
    WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
        .map(walkdir::DirEntry::into_path)
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("utoc"))
}

/// The admitted TOC files with their size and modification time: what the open store is made of,
/// so a game update, a saved mod or a container arriving or leaving shows as a different key.
type StoreKey = Vec<(PathBuf, u64, Option<SystemTime>)>;

static BASE_STORE: Mutex<Option<(StoreKey, Arc<dyn IoStoreTrait>)>> = Mutex::new(None);

/// Open the base game: every chunk and every patch, minus mods and any container this app cannot
/// decrypt. Patches sort above the chunks they supersede, so the latest revision of a package wins.
/// Opening parses every TOC, so the store is kept for the next call over the same files.
pub fn open_base_game_paks(
    paks_dir: &Path,
    target_container: &str,
) -> Result<Arc<dyn IoStoreTrait>, String> {
    let target = target_container.to_string();
    let undecryptable = undecryptable_container_stems(paks_dir);
    let mods = mod_container_stems(paks_dir);
    let admits = move |name: &str| admits_container(name, &target, &undecryptable, &mods);
    let key = store_key(paks_dir, &admits);
    if let Some((cached, store)) = BASE_STORE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        && *cached == key
    {
        return Ok(Arc::clone(store));
    }
    let store: Arc<dyn IoStoreTrait> = Arc::from(
        retoc::iostore::open_filtered(paks_dir, super::profile::make_config()?, admits)
            .map_err(|e| e.to_string())?,
    );
    *BASE_STORE.lock().unwrap_or_else(PoisonError::into_inner) = Some((key, Arc::clone(&store)));
    Ok(store)
}

fn store_key(paks_dir: &Path, admits: &dyn Fn(&str) -> bool) -> StoreKey {
    let mut key: StoreKey = utoc_paths(paks_dir)
        .filter(|p| p.file_stem().and_then(|s| s.to_str()).is_some_and(admits))
        .filter_map(|p| {
            let meta = std::fs::metadata(&p).ok()?;
            Some((p, meta.len(), meta.modified().ok()))
        })
        .collect();
    key.sort();
    key
}

/// Whether the base-game store takes `name`: the target always, otherwise anything readable that
/// is not a mod. Mods are known by folder first, then by the naming convention as a fallback for
/// ones dropped straight into Paks rather than `~mods`.
fn admits_container(
    name: &str,
    target: &str,
    undecryptable: &HashSet<String>,
    mods: &HashSet<String>,
) -> bool {
    if undecryptable.contains(name) {
        return false;
    }
    name == target || !(mods.contains(name) || name.contains("_9999999_"))
}

/// Open the target container in isolation (no base game, no other mods). `utoc_path` is where
/// the container actually lives, which is rarely `paks_dir` itself since mods sit under `~mods`.
pub fn open_target_only(
    paks_dir: &Path,
    utoc_path: &Path,
    target_container: &str,
) -> Result<Box<dyn IoStoreTrait>, String> {
    // Only a readable container under an unknown key GUID is an encryption failure. A missing or
    // unreadable file fails the same check, so let it reach the honest error further down.
    if utoc_path.is_file() && !utoc_is_decryptable(utoc_path) {
        return Err(format!(
            "{target_container} is encrypted with a key this app does not have and cannot be extracted."
        ));
    }
    let target = target_container.to_string();
    retoc::iostore::open_filtered(paks_dir, super::profile::make_config()?, move |name| {
        name == target
    })
    .map_err(|e| e.to_string())
}

/// Forgets the merged base store. Its key is the admitted containers' size and time, so it would
/// notice a rewrite on its own; this drops any file handle it holds first, which is what lets a
/// container be replaced underneath it.
pub fn drop_cached_store() {
    *BASE_STORE.lock().unwrap_or_else(PoisonError::into_inner) = None;
}

/// Open a single container file by path, with no sibling containers merged in.
pub fn open_utoc(utoc_path: &str) -> Result<Box<dyn IoStoreTrait>, String> {
    retoc::iostore::open(utoc_path, super::profile::make_config()?).map_err(|e| e.to_string())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Minimal TOC header: valid magic, encryption flag clear, so the decryptability probe
    /// passes and only the container's location is under test.
    fn write_stub_utoc(path: &Path) {
        let mut header = vec![0u8; 0x90];
        header[0..16].copy_from_slice(b"-==--==--==--==-");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("create dir");
        std::fs::write(path, &header).expect("write stub utoc");
    }

    /// Patches carry the game's revisions and sort above the chunks they supersede, so they belong
    /// in the store; mods do not, whichever way they are recognised.
    #[test]
    fn the_base_store_admits_patches_and_refuses_mods() {
        let undecryptable: HashSet<String> = ["Locked".to_string()].into();
        let mods: HashSet<String> = ["PlainName".to_string()].into();
        let admits =
            |name: &str, target: &str| admits_container(name, target, &undecryptable, &mods);
        assert!(admits("Patch_-Windows_1.1.3825464_P", "pakchunk0-Windows"));
        assert!(admits("pakchunkEnv-Windows", "pakchunk0-Windows"));
        assert!(!admits("PlainName", "pakchunk0-Windows"));
        assert!(!admits("Deep_9999999_P", "pakchunk0-Windows"));
        assert!(!admits("Locked", "pakchunk0-Windows"));
        assert!(
            admits("PlainName", "PlainName"),
            "the target is admitted even when it is a mod"
        );
        assert!(
            !admits("Locked", "Locked"),
            "an undecryptable target still fails"
        );
    }

    /// The kept store answers for the files it was opened from, so only those files may retire it:
    /// a mod changing under `~mods` must not, a base container changing size must.
    #[test]
    fn the_store_key_follows_the_admitted_files_only() {
        let paks_dir = std::env::temp_dir().join(format!("rivals-storekey-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&paks_dir);
        let base = paks_dir.join("pakchunk0-Windows.utoc");
        let cosmetic = paks_dir.join("~mods").join("Cosmetic_9999999_P.utoc");
        write_stub_utoc(&base);
        write_stub_utoc(&cosmetic);
        let admits = |name: &str| !name.contains("_9999999_");

        let before = store_key(&paks_dir, &admits);
        assert_eq!(before.len(), 1);
        std::fs::write(&cosmetic, b"rewritten").expect("rewrite the mod");
        assert_eq!(store_key(&paks_dir, &admits), before);
        std::fs::write(&base, vec![0u8; 0x91]).expect("grow the base container");
        assert_ne!(store_key(&paks_dir, &admits), before);
        let _ = std::fs::remove_dir_all(&paks_dir);
    }

    #[test]
    fn mod_containers_are_identified_by_folder_not_by_name() {
        let paks_dir = std::env::temp_dir().join(format!("rivals-modstems-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&paks_dir);
        write_stub_utoc(&paks_dir.join("~mods").join("PlainName.utoc"));
        write_stub_utoc(
            &paks_dir
                .join("~mods")
                .join("Nested")
                .join("Deep_9999999_P.utoc"),
        );
        write_stub_utoc(&paks_dir.join("pakchunk0-Windows.utoc"));

        let stems = mod_container_stems(&paks_dir);

        assert!(
            stems.contains("PlainName"),
            "a mod that skips the _9999999_P convention still has to be excluded from the base store"
        );
        assert!(stems.contains("Deep_9999999_P"));
        assert!(!stems.contains("pakchunk0-Windows"));
        let _ = std::fs::remove_dir_all(&paks_dir);
    }

    #[test]
    fn detects_the_obfuscation_flag() {
        let dir = std::env::temp_dir().join(format!("rivals-obfuscation-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create dir");

        let plain = dir.join("plain.utoc");
        let obfuscated = dir.join("obfuscated.utoc");
        write_stub_utoc(&plain);
        let mut header = vec![0u8; 0x90];
        header[0..16].copy_from_slice(TOC_MAGIC);
        header[80] = TOC_ENCRYPTED_FLAG;
        std::fs::write(&obfuscated, &header).expect("write stub");

        assert!(!utoc_is_obfuscated(&plain));
        assert!(utoc_is_obfuscated(&obfuscated));
        assert!(!utoc_is_obfuscated(&dir.join("missing.utoc")));
        // The flag alone is not a barrier: the payload is under the default-GUID key we hold.
        assert!(utoc_is_decryptable(&obfuscated));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn obfuscation_key_parses() {
        assert!(super::super::profile::obfuscation_key().is_ok());
    }

    /// Mods live under `~mods`, so a guard that rebuilt the path from `paks_dir` and the
    /// container name found nothing and reported every mod as encrypted.
    #[test]
    fn container_in_a_subfolder_is_not_reported_as_encrypted() {
        let paks_dir =
            std::env::temp_dir().join(format!("rivals-iostore-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&paks_dir);
        let utoc_path = paks_dir.join("~mods").join("SomeMod_9999999_P.utoc");
        write_stub_utoc(&utoc_path);

        let err = open_target_only(&paks_dir, &utoc_path, "SomeMod_9999999_P")
            .err()
            .unwrap_or_default();

        assert!(
            !err.contains("encrypted with a key"),
            "guard rejected a container it should have found: {err}"
        );
        let _ = std::fs::remove_dir_all(&paks_dir);
    }

    #[test]
    fn a_file_that_is_not_a_toc_is_never_treated_as_decryptable() {
        let root = std::env::temp_dir().join(format!("rivals-containers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create dir");
        let path = root.join("NotAToc.utoc");
        std::fs::write(&path, b"definitely not a toc header at all right").expect("write");

        assert!(!utoc_is_decryptable(&path));
        assert!(!utoc_is_obfuscated(&path));
        assert!(undecryptable_container_stems(&root).contains("NotAToc"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_folder_yields_no_stems_rather_than_an_error() {
        let missing = std::env::temp_dir().join("rivals-containers-does-not-exist");
        assert!(mod_container_stems(&missing).is_empty());
    }
}
