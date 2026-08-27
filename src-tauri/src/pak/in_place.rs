//! Repacks an installed mod in place: stages the extract and rebuild in a temp dir, verifies nothing was dropped, then swaps the result over the original with rollback on failure.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use serde::Serialize;
use tauri::AppHandle;

use crate::paths::mods_dir;

use super::{iostore, reader, writer};

/// Sidecar entry every IoStore mod carries. `repack_iostore` regenerates it, so it is never
/// carried over from the source pak.
const CHUNKNAMES: &str = "chunknames";

/// Extensions that belong to the package they sit beside rather than being content of their own.
const COMPANION_EXTS: [&str; 4] = [".uexp", ".m.ubulk", ".ubulk", ".uptnl"];

static TEMP_COUNTER: AtomicU32 = AtomicU32::new(0);

#[derive(Serialize)]
pub(crate) struct InPlaceReport {
    pub container_name: String,
    /// `"pak"` or `"iostore"`, the format actually written.
    pub format: &'static str,
    pub obfuscated: bool,
    pub assets_in: usize,
    pub assets_out: usize,
    /// Loose files moved into the sidecar pak, such as INIs the pak editor manages.
    pub carried_pak_entries: usize,
}

/// Deletes the staging tree on every exit path, including the early returns.
struct TempGuard(PathBuf);

impl Drop for TempGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn sibling(pak: &Path, ext: &str) -> PathBuf {
    pak.with_extension(ext)
}

/// Mods only. Repacking a vanilla container in place would overwrite game files, and the vanilla
/// extract/rebuild pair already covers that case with a manifest and an explicit output folder.
fn validate_target(game_root: &str, mod_pak: &Path) -> Result<(), String> {
    if mod_pak.extension().and_then(|e| e.to_str()) != Some("pak") {
        return Err("Select the mod's .pak file.".to_string());
    }
    if !mod_pak.is_file() {
        return Err(format!("Mod not found: {}", mod_pak.display()));
    }
    let Ok(target) = mod_pak.canonicalize() else {
        return Err(format!("Could not resolve {}", mod_pak.display()));
    };
    // A missing ~mods folder means the target cannot be inside it, so it takes the same answer
    // rather than a separate "could not resolve" that would read as a different problem.
    let inside = mods_dir(game_root)
        .canonicalize()
        .map(|mods| target.starts_with(&mods))
        .unwrap_or(false);
    if !inside {
        return Err("In-place repack only works on mods inside the ~mods folder.".to_string());
    }
    Ok(())
}

/// One comparable identity per piece of content. A package is named by its stem so the same asset
/// compares equal whether it was listed from a container index (`X.uasset` only) or from a pak
/// (`X.uasset` plus `X.uexp`), which is what makes a format conversion checkable.
fn content_key(path: &str) -> String {
    let normalized = path
        .replace('\\', "/")
        .trim_start_matches('/')
        .to_lowercase();
    for ext in COMPANION_EXTS {
        if let Some(stem) = normalized.strip_suffix(ext) {
            return stem.to_string();
        }
    }
    normalized
        .strip_suffix(".uasset")
        .or_else(|| normalized.strip_suffix(".umap"))
        .map(str::to_string)
        .unwrap_or(normalized)
}

/// Everything a mod carries: the container's packages plus whatever rides in its sidecar pak.
fn mod_contents(pak: &Path, is_iostore: bool) -> Result<HashSet<String>, String> {
    let mut contents = HashSet::new();
    if is_iostore {
        let utoc = sibling(pak, "utoc");
        for path in iostore::list_utoc_contents(&utoc.to_string_lossy())? {
            contents.insert(content_key(&path));
        }
    }
    for path in reader::list_pak_contents(&pak.to_string_lossy())? {
        let key = content_key(&path);
        if key != CHUNKNAMES {
            contents.insert(key);
        }
    }
    Ok(contents)
}

/// Relative paths under `dir` that `repack_iostore` would ignore, meaning everything that is not
/// part of a `.uasset`/`.umap` bundle. They have to ride in the sidecar pak or they are lost.
fn non_package_files(dir: &Path) -> Vec<PathBuf> {
    let files: Vec<PathBuf> = walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.path().strip_prefix(dir).ok().map(Path::to_path_buf))
        .collect();

    let lowered: HashSet<String> = files
        .iter()
        .map(|p| p.to_string_lossy().replace('\\', "/").to_lowercase())
        .collect();

    let owned_by_package = |rel: &Path| -> bool {
        let lower = rel.to_string_lossy().replace('\\', "/").to_lowercase();
        let stem = match COMPANION_EXTS.iter().find_map(|e| lower.strip_suffix(e)) {
            Some(stem) => stem.to_string(),
            None => match lower
                .strip_suffix(".uasset")
                .or_else(|| lower.strip_suffix(".umap"))
            {
                Some(stem) => stem.to_string(),
                None => return false,
            },
        };
        lowered.contains(&format!("{stem}.uexp"))
    };

    files
        .into_iter()
        .filter(|rel| !owned_by_package(rel))
        .collect()
}

/// Copies the given relative paths from one tree into another, creating parents as needed.
fn copy_files(from: &Path, to: &Path, rels: &[PathBuf]) -> Result<(), String> {
    for rel in rels {
        let dest = to.join(rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        fs::copy(from.join(rel), &dest).map_err(|e| format!("copy {}: {e}", rel.display()))?;
    }
    Ok(())
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    let rels: Vec<PathBuf> = walkdir::WalkDir::new(from)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.path().strip_prefix(from).ok().map(Path::to_path_buf))
        .collect();
    copy_files(from, to, &rels)
}

pub(crate) fn repack_mod_in_place(
    game_root: &str,
    mod_pak: &str,
    to_iostore: bool,
    obfuscate: bool,
    oodle_level: Option<retoc::OodleCompressionLevel>,
    compression: super::profile::PackCompression,
    app: AppHandle,
) -> Result<InPlaceReport, String> {
    let live_pak = PathBuf::from(mod_pak);
    validate_target(game_root, &live_pak)?;

    let stem = live_pak
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("Invalid mod file name")?
        .to_string();
    let live_utoc = sibling(&live_pak, "utoc");
    let live_ucas = sibling(&live_pak, "ucas");
    let from_iostore = live_utoc.is_file() && live_ucas.is_file();

    let contents_in = mod_contents(&live_pak, from_iostore)?;

    let temp = std::env::temp_dir().join(format!(
        "rivals_repack_{}_{}",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&temp);
    fs::create_dir_all(&temp).map_err(|e| format!("create temp dir: {e}"))?;
    let _temp_guard = TempGuard(temp.clone());

    let assets_dir = temp.join("assets");
    let carried_dir = temp.join("carried");
    let staged = temp.join("out");
    fs::create_dir_all(&staged).map_err(|e| format!("create staging dir: {e}"))?;

    if from_iostore {
        let extracted = iostore::extract_utoc_legacy(
            &live_utoc.to_string_lossy(),
            game_root,
            &assets_dir.to_string_lossy(),
            &[],
            app.clone(),
        )?;
        // Conversion reports per-asset failures as a warning entry rather than an error. Fine when
        // extracting to a folder, but here it would mean overwriting the mod with a subset of
        // itself.
        if let Some(warning) = extracted.iter().find(|f| f.starts_with("__warnings__")) {
            return Err(format!(
                "Not repacking: some assets failed to convert, so the rebuilt mod would be \
                 missing content.\n{}",
                warning.trim_start_matches("__warnings__:").trim()
            ));
        }
        // The legacy tree holds packages only, so the sidecar pak's own entries (INIs and the
        // like) are pulled aside separately.
        reader::unpack_pak(
            &live_pak.to_string_lossy(),
            &carried_dir.to_string_lossy(),
            &[CHUNKNAMES],
        )?;
    } else {
        reader::unpack_pak(
            &live_pak.to_string_lossy(),
            &assets_dir.to_string_lossy(),
            &[],
        )?;
        if to_iostore {
            // Packing to IoStore keeps only package bundles, so anything else has to be moved
            // into the sidecar pak rather than dropped.
            let loose = non_package_files(&assets_dir);
            copy_files(&assets_dir, &carried_dir, &loose)?;
        }
    }

    let carried_pak_entries = walkdir::WalkDir::new(&carried_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .count();

    let staged_pak = staged.join(format!("{stem}.pak"));
    let staged_utoc = staged.join(format!("{stem}.utoc"));

    if to_iostore {
        iostore::repack_iostore(
            &assets_dir.to_string_lossy(),
            &staged_utoc.to_string_lossy(),
            oodle_level,
            obfuscate,
            compression,
            app,
        )?;
        if carried_pak_entries > 0 {
            // Fold the carried entries in beside the regenerated `chunknames`.
            let sidecar = temp.join("sidecar");
            reader::unpack_pak(
                &staged_pak.to_string_lossy(),
                &sidecar.to_string_lossy(),
                &[],
            )?;
            copy_tree(&carried_dir, &sidecar)?;
            writer::repack_pak(
                &sidecar.to_string_lossy(),
                &staged_pak.to_string_lossy(),
                oodle_level,
                compression,
            )?;
        }
    } else {
        writer::repack_pak(
            &assets_dir.to_string_lossy(),
            &staged_pak.to_string_lossy(),
            oodle_level,
            compression,
        )?;
    }

    let contents_out = mod_contents(&staged_pak, to_iostore)?;
    let missing: Vec<&String> = contents_in.difference(&contents_out).collect();
    if !missing.is_empty() {
        let sample: Vec<&str> = missing.iter().take(5).map(|s| s.as_str()).collect();
        return Err(format!(
            "Not repacking: the rebuilt mod is missing {} of {} items, for example:\n{}",
            missing.len(),
            contents_in.len(),
            sample.join("\n")
        ));
    }

    swap_into_place(&live_pak, &staged, &stem, to_iostore)?;
    super::invalidate_list_caches(&[&live_pak, &live_utoc]);

    Ok(InPlaceReport {
        container_name: stem,
        format: if to_iostore { "iostore" } else { "pak" },
        obfuscated: to_iostore && obfuscate,
        assets_in: contents_in.len(),
        assets_out: contents_out.len(),
        carried_pak_entries,
    })
}

/// Moves the staged files over the live ones. The live set is renamed aside first, so a failure
/// part way through can put the original mod back.
fn swap_into_place(
    live_pak: &Path,
    staged: &Path,
    stem: &str,
    to_iostore: bool,
) -> Result<(), String> {
    let dir = live_pak.parent().ok_or("Mod has no parent folder")?;

    let mut backups: Vec<(PathBuf, PathBuf)> = Vec::new();
    for ext in ["pak", "utoc", "ucas"] {
        let live = dir.join(format!("{stem}.{ext}"));
        if !live.is_file() {
            continue;
        }
        let backup = dir.join(format!("{stem}.{ext}.bak"));
        let _ = fs::remove_file(&backup);
        match fs::rename(&live, &backup) {
            Ok(()) => backups.push((live, backup)),
            Err(e) => {
                restore(&backups);
                return Err(format!("back up {}: {e}", live.display()));
            }
        }
    }

    let wanted: &[&str] = if to_iostore {
        &["pak", "utoc", "ucas"]
    } else {
        &["pak"]
    };
    for ext in wanted {
        let from = staged.join(format!("{stem}.{ext}"));
        let to = dir.join(format!("{stem}.{ext}"));
        // Temp is often on another volume, so this copies rather than renames.
        if let Err(e) = fs::copy(&from, &to) {
            for (live, _) in &backups {
                let _ = fs::remove_file(live);
            }
            restore(&backups);
            return Err(format!("install {}: {e}", to.display()));
        }
    }

    // Anything the new format does not use was backed up and is simply not restored, which is how
    // an IoStore mod repacked to a plain pak loses its stale .utoc/.ucas.
    for (_, backup) in &backups {
        let _ = fs::remove_file(backup);
    }
    Ok(())
}

fn restore(backups: &[(PathBuf, PathBuf)]) {
    for (live, backup) in backups {
        let _ = fs::rename(backup, live);
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rivals-inplace-{}-{}-{}",
            std::process::id(),
            name,
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create scratch");
        dir
    }

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, body).expect("write");
    }

    #[test]
    fn content_key_collapses_a_package_and_its_companions() {
        let base = content_key("Marvel/Content/Char/T_Body.uasset");
        for path in [
            "Marvel/Content/Char/T_Body.uexp",
            "Marvel/Content/Char/T_Body.ubulk",
            "Marvel/Content/Char/T_Body.uptnl",
            "Marvel/Content/Char/T_Body.m.ubulk",
            "../../../Marvel/Content/Char/T_Body.uasset",
        ] {
            assert_eq!(
                content_key(path.trim_start_matches("../../..")),
                base,
                "{path}"
            );
        }
        assert_eq!(
            content_key("Marvel/Config/mod.ini"),
            "marvel/config/mod.ini"
        );
    }

    #[test]
    fn non_package_files_keeps_loose_content_and_skips_bundles() {
        let root = scratch("loose");
        write(&root.join("Marvel/Content/A.uasset"), "a");
        write(&root.join("Marvel/Content/A.uexp"), "a");
        write(&root.join("Marvel/Content/A.ubulk"), "a");
        write(&root.join("Marvel/Config/mod.ini"), "i");
        write(&root.join("Marvel/Content/Orphan.uasset"), "o");

        let loose: HashSet<String> = non_package_files(&root)
            .into_iter()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .collect();

        assert!(loose.contains("Marvel/Config/mod.ini"));
        // No .uexp beside it, so repack_iostore would skip it too.
        assert!(loose.contains("Marvel/Content/Orphan.uasset"));
        assert!(!loose.contains("Marvel/Content/A.uasset"));
        assert!(!loose.contains("Marvel/Content/A.uexp"));
        assert!(!loose.contains("Marvel/Content/A.ubulk"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_a_pak_outside_the_mods_folder() {
        let root = scratch("outside");
        let stray = root.join("stray.pak");
        write(&stray, "x");
        let err = validate_target(root.to_str().unwrap(), &stray).unwrap_err();
        assert!(err.contains("~mods"), "{err}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn accepts_a_pak_inside_the_mods_folder() {
        let root = scratch("inside");
        let mod_pak = mods_dir(root.to_str().unwrap()).join("Some_9999999_P.pak");
        write(&mod_pak, "x");
        assert!(validate_target(root.to_str().unwrap(), &mod_pak).is_ok());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn swap_replaces_the_live_files_and_clears_backups() {
        let root = scratch("swap");
        let dir = root.join("~mods");
        let staged = root.join("out");
        for ext in ["pak", "utoc", "ucas"] {
            write(&dir.join(format!("m.{ext}")), "old");
            write(&staged.join(format!("m.{ext}")), "new");
        }

        swap_into_place(&dir.join("m.pak"), &staged, "m", true).expect("swap");

        for ext in ["pak", "utoc", "ucas"] {
            assert_eq!(
                fs::read_to_string(dir.join(format!("m.{ext}"))).unwrap(),
                "new"
            );
            assert!(!dir.join(format!("m.{ext}.bak")).exists());
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn swap_restores_the_original_when_a_staged_file_is_missing() {
        let root = scratch("rollback");
        let dir = root.join("~mods");
        let staged = root.join("out");
        for ext in ["pak", "utoc", "ucas"] {
            write(&dir.join(format!("m.{ext}")), "old");
        }
        // Only the pak was staged, so installing the utoc fails part way through.
        write(&staged.join("m.pak"), "new");

        let err = swap_into_place(&dir.join("m.pak"), &staged, "m", true).unwrap_err();

        assert!(err.contains("install"), "{err}");
        for ext in ["pak", "utoc", "ucas"] {
            assert_eq!(
                fs::read_to_string(dir.join(format!("m.{ext}"))).unwrap(),
                "old"
            );
            assert!(!dir.join(format!("m.{ext}.bak")).exists());
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn converting_to_pak_leaves_no_stale_iostore_siblings() {
        let root = scratch("to-pak");
        let dir = root.join("~mods");
        let staged = root.join("out");
        for ext in ["pak", "utoc", "ucas"] {
            write(&dir.join(format!("m.{ext}")), "old");
        }
        write(&staged.join("m.pak"), "new");

        swap_into_place(&dir.join("m.pak"), &staged, "m", false).expect("swap");

        assert_eq!(fs::read_to_string(dir.join("m.pak")).unwrap(), "new");
        assert!(!dir.join("m.utoc").exists(), "stale utoc left behind");
        assert!(!dir.join("m.ucas").exists(), "stale ucas left behind");
        let _ = fs::remove_dir_all(&root);
    }
}
