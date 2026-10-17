//! Converts an installed mod in place between a plain pak and IoStore. The extract and the rebuild
//! are staged in a temporary folder, the result is checked to hold everything the mod held, and
//! only then is it swapped over the original, which comes back should the swap fail part way.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use serde::{Deserialize, Serialize};

use crate::mod_ops::ModFiles;
use crate::pak::containers::{self, list_utoc_paths, open_utoc, utoc_is_obfuscated};
use crate::pak::iostore_out::{self, HeaderCarry, IoStoreOptions};
use crate::pak::pack::{COMPANION_EXTS, Take, pack_folder};
use crate::pak::{plain, shader_library};
use crate::paths::mods_dir;

/// Every file a converted mod may be made of.
const PARTS: [&str; 3] = ["pak", "utoc", "ucas"];

static TEMP_COUNTER: AtomicU32 = AtomicU32::new(0);

/// What a mod is converted to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModFormat {
    /// A plain pak, which delivers loose files only.
    Pak,
    /// A `.utoc` and `.ucas`, which the game loads packages from, with a pak beside them.
    Iostore,
}

impl ModFormat {
    pub fn name(self) -> &'static str {
        match self {
            Self::Pak => "pak",
            Self::Iostore => "iostore",
        }
    }
}

/// Which half of a conversion the progress is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvertPhase {
    /// The container's packages turned back into loose files.
    Extracting,
    /// The loose files packed into a container.
    Repacking,
}

#[derive(Debug, Serialize)]
pub struct ConvertReport {
    pub container_name: String,
    /// `"pak"` or `"iostore"`, the format written.
    pub format: &'static str,
    pub obfuscated: bool,
    pub assets_in: usize,
    pub assets_out: usize,
    /// Loose files moved into the pak beside the container, such as INIs.
    pub carried_pak_entries: usize,
    /// Shader archives written into the container as libraries.
    pub shader_libraries: Vec<String>,
    pub warnings: Vec<String>,
    /// The container's blocks were copied as they were stored, rather than every package going
    /// out to loose files and back.
    pub direct: bool,
    /// Blocks copied as they were stored.
    pub copied_blocks: usize,
    /// Blocks compressed again.
    pub recompressed_blocks: usize,
}

/// Deletes the staging tree on every way out.
struct TempGuard(PathBuf);

impl Drop for TempGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Mods only. Converting a game container in place would overwrite the game's files.
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

/// One comparable identity per piece of content. A package is named by its stem, so the same asset
/// compares equal whether it was listed from a container index (`X.uasset` only) or from a pak
/// (`X.uasset` and `X.uexp`), which is what makes a conversion checkable. A shader archive and its
/// asset info, two files in a pak, are one library in a container, and so one item.
fn content_key(path: &str) -> String {
    let normalized = path
        .replace('\\', "/")
        .trim_start_matches('/')
        .to_lowercase();
    if let Some(library) = shader_library::library_of(&normalized) {
        let folder = normalized.rsplit_once('/').map_or("", |(folder, _)| folder);
        return format!("{folder}/shader library {library}");
    }
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

/// Everything a mod carries: the container's packages and whatever rides in its pak.
fn mod_contents(pak: &Path, is_iostore: bool) -> Result<HashSet<String>, String> {
    let mut contents = HashSet::new();
    if is_iostore {
        for path in list_utoc_paths(&pak.with_extension("utoc"))? {
            contents.insert(content_key(&path));
        }
    }
    for path in plain::list_files(pak)? {
        let key = content_key(&crate::pak::profile::strip_mount_prefix(&path));
        if !plain::is_unsupported_entry(&key) {
            contents.insert(key);
        }
    }
    Ok(contents)
}

/// Whether any file lies under `dir`.
fn holds_files(dir: &Path) -> bool {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(Result::ok)
        .any(|e| e.file_type().is_file())
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    for entry in walkdir::WalkDir::new(from)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
    {
        let rel = entry.path().strip_prefix(from).map_err(|e| e.to_string())?;
        let dest = to.join(rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        fs::copy(entry.path(), &dest).map_err(|e| format!("copy {}: {e}", rel.display()))?;
    }
    Ok(())
}

/// Every file the converted mod is made of is staged, so the swap cannot leave it short: the
/// swap sets aside what it has no staged file for.
fn check_staged(staged: &Path, stem: &str, to: ModFormat) -> Result<(), String> {
    let wanted: &[&str] = match to {
        ModFormat::Iostore => &PARTS,
        ModFormat::Pak => &["pak"],
    };
    for extension in wanted {
        let file = staged.join(format!("{stem}.{extension}"));
        if !file.is_file() {
            return Err(format!(
                "Not repacking: the rebuild left no {}, so the mod stays as it was",
                file.display()
            ));
        }
    }
    Ok(())
}

/// Converts the mod at `files` to `to` where it is. `io.obfuscate` of `None` keeps the container
/// obfuscated when it was. The packages extract on `pool`, or the global pool, and repack on as
/// many threads; `cancel` stops the work before anything is swapped, and `progress` hears how far
/// each half got.
#[allow(clippy::too_many_arguments)]
pub fn convert_mod(
    game_root: &str,
    files: &ModFiles,
    to: ModFormat,
    io: &IoStoreOptions,
    dry_run: bool,
    pool: Option<&rayon::ThreadPool>,
    cancel: &AtomicBool,
    progress: &(dyn Fn(ConvertPhase, usize, usize) + Sync),
) -> Result<ConvertReport, String> {
    if files.disabled {
        return Err(format!(
            "{} is switched off; switch it on to convert it",
            files.stem
        ));
    }
    let live_pak = files.part("pak");
    validate_target(game_root, &live_pak)?;
    let stem = files.stem.clone();
    let live_utoc = files.part("utoc");
    let from_iostore = live_utoc.is_file() && files.part("ucas").is_file();
    let to_iostore = to == ModFormat::Iostore;
    let obfuscate = io
        .obfuscate
        .unwrap_or_else(|| from_iostore && utoc_is_obfuscated(&live_utoc));

    let contents_in = mod_contents(&live_pak, from_iostore)?;
    // Shader maps, localized packages and redirects are in the container header, which the
    // extracted files cannot say anything about.
    let carry = if from_iostore {
        HeaderCarry::from_store(&*open_utoc(&live_utoc.to_string_lossy())?)
    } else {
        HeaderCarry::default()
    };

    let temp = std::env::temp_dir().join(format!(
        "rivals_repack_{}_{}",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&temp);
    fs::create_dir_all(&temp).map_err(|e| format!("create temp dir: {e}"))?;
    let _guard = TempGuard(temp.clone());
    let assets_dir = temp.join("assets");
    let carried_dir = temp.join("carried");
    let staged = temp.join("out");
    fs::create_dir_all(&staged).map_err(|e| format!("create staging dir: {e}"))?;

    // A container that stays a container keeps its packages as they are stored: its blocks are
    // copied, encrypted again as the obfuscation asks, rather than every package going out to loose
    // files and back. It keeps its block size, as a save into it does, and one written plain is
    // compressed, as a rebuild would.
    if to_iostore
        && from_iostore
        && let Some(direct) = copies_directly(game_root, &live_utoc)?
    {
        let copied = iostore_out::copy_container(
            &live_utoc,
            &staged.join(format!("{stem}.utoc")),
            &HashSet::new(),
            &IoStoreOptions {
                obfuscate: Some(obfuscate),
                reencode: io.reencode || !direct.compressed,
                ..*io
            },
        )?;
        progress(ConvertPhase::Repacking, 1, 1);
        if cancel.load(Ordering::Relaxed) {
            return Err("Repack cancelled".to_string());
        }
        let staged_pak = staged.join(format!("{stem}.pak"));
        let contents_out = swap_checked(files, &stem, &staged, to, &contents_in, &[], dry_run)?;
        return Ok(ConvertReport {
            container_name: stem,
            format: to.name(),
            obfuscated: obfuscate,
            assets_in: contents_in.len(),
            assets_out: contents_out.len(),
            carried_pak_entries: plain::list_files(&staged_pak).map_or(0, |files| files.len()),
            shader_libraries: direct.libraries,
            warnings: Vec::new(),
            direct: true,
            copied_blocks: copied.copied_blocks,
            recompressed_blocks: copied.recompressed_blocks,
        });
    }

    if from_iostore {
        let extract = || {
            crate::pak::extract::extract_legacy(
                &live_utoc,
                game_root,
                &assets_dir,
                &[],
                cancel,
                &|done, total| progress(ConvertPhase::Extracting, done, total),
            )
        };
        let extracted = match pool {
            Some(pool) => pool.install(extract),
            None => extract(),
        }?;
        if cancel.load(Ordering::Relaxed) {
            return Err("Repack cancelled".to_string());
        }
        // A package that did not convert would leave the mod a subset of itself.
        if !extracted.errors.is_empty() {
            let sample: Vec<String> = extracted
                .errors
                .iter()
                .take(5)
                .map(|e| format!("  - {e}"))
                .collect();
            let more = match extracted.errors.len() {
                n if n > 5 => format!("\n  ...and {} more", n - 5),
                _ => String::new(),
            };
            return Err(format!(
                "Not repacking: {} asset(s) failed to convert, so the rebuilt mod would be \
                 missing content.\n{}{more}",
                extracted.errors.len(),
                sample.join("\n")
            ));
        }
        // The legacy tree holds packages only, so the pak's own entries, INIs and the like, are
        // set aside on their own.
        plain::extract(&live_pak, &carried_dir, &|name| {
            !plain::is_unsupported_entry(name)
        })?;
    } else {
        plain::extract(&live_pak, &assets_dir, &|_| true)?;
    }

    let mut carried_pak_entries = walkdir::WalkDir::new(&carried_dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .count();

    let staged_pak = staged.join(format!("{stem}.pak"));
    let mut shader_libraries = Vec::new();
    let mut left_out = Vec::new();
    let mut warnings = Vec::new();
    for folder in [&assets_dir, &carried_dir] {
        fs::create_dir_all(folder).map_err(|e| format!("create {}: {e}", folder.display()))?;
    }
    // A container holding no package and no shader library leaves its pak's files as all the mod
    // has, so they are what is packed, into a container holding nothing again.
    let only_carried = from_iostore && !holds_files(&assets_dir);
    if to_iostore {
        let packed = pack_folder(
            if only_carried {
                &carried_dir
            } else {
                &assets_dir
            },
            Take::Everything,
            &staged.join(format!("{stem}.utoc")),
            &IoStoreOptions {
                obfuscate: Some(obfuscate),
                ..*io
            },
            &carry,
            &|| shader_library::game_library_names(&crate::paths::paks_dir(game_root)),
            pool.map_or_else(
                rayon::current_num_threads,
                rayon::ThreadPool::current_num_threads,
            ),
            cancel,
            &|done, total| progress(ConvertPhase::Repacking, done, total),
        )?;
        if !from_iostore {
            // Packing to IoStore puts anything that is not a package bundle into the pak beside it.
            carried_pak_entries += packed.loose.len();
        }
        shader_libraries = packed.shader_libraries;
        left_out = packed.left_out;
        warnings = packed.warnings;
        if from_iostore && !only_carried && carried_pak_entries > 0 {
            // Fold the carried entries in beside the listing just written.
            let beside = temp.join("beside");
            plain::extract(&staged_pak, &beside, &|_| true)?;
            copy_tree(&carried_dir, &beside)?;
            plain::pack_dir(&beside, &staged_pak, &io.pak())?;
        }
    } else {
        if from_iostore && carried_pak_entries > 0 {
            // A plain pak holds everything, the files the container's pak carried beside them.
            copy_tree(&carried_dir, &assets_dir)?;
        }
        plain::pack_dir(&assets_dir, &staged_pak, &io.pak())?;
    }
    if cancel.load(Ordering::Relaxed) {
        return Err("Repack cancelled".to_string());
    }

    let contents_out = swap_checked(files, &stem, &staged, to, &contents_in, &left_out, dry_run)?;

    Ok(ConvertReport {
        container_name: stem,
        format: to.name(),
        obfuscated: to_iostore && obfuscate,
        assets_in: contents_in.len(),
        assets_out: contents_out.len(),
        carried_pak_entries,
        shader_libraries,
        warnings,
        direct: false,
        copied_blocks: 0,
        recompressed_blocks: 0,
    })
}

/// What a container repacked as a container keeps of how it is stored, when its blocks can be
/// copied: whether it compresses them at all, and the shader libraries it holds. `None` sends it
/// the long way, out to loose files and back: one this editor cannot read or would write
/// differently, and one holding a shader library named like the game's, which the long way leaves
/// out with a warning.
struct Direct {
    compressed: bool,
    libraries: Vec<String>,
}

fn copies_directly(game_root: &str, utoc: &Path) -> Result<Option<Direct>, String> {
    if !containers::utoc_is_decryptable(utoc) {
        return Ok(None);
    }
    let form = iostore_out::stored_form(utoc)?;
    if form.why_not.is_some() {
        return Ok(None);
    }
    let libraries: Vec<String> = list_utoc_paths(utoc)?
        .iter()
        .filter_map(|path| shader_library::library_name(path))
        .collect();
    if !libraries.is_empty() {
        let game = shader_library::game_library_names(&crate::paths::paks_dir(game_root))?;
        if libraries
            .iter()
            .any(|name| game.contains(&name.to_ascii_lowercase()))
        {
            return Ok(None);
        }
    }
    Ok(Some(Direct {
        compressed: form.compressed,
        libraries,
    }))
}

/// Holds what was staged to everything the mod held, but the shader archives the pack left out and
/// warned about, then swaps it in unless `dry_run`. Gives what the staged mod holds.
fn swap_checked(
    files: &ModFiles,
    stem: &str,
    staged: &Path,
    to: ModFormat,
    contents_in: &HashSet<String>,
    left_out: &[String],
    dry_run: bool,
) -> Result<HashSet<String>, String> {
    let staged_pak = staged.join(format!("{stem}.pak"));
    let contents_out = mod_contents(&staged_pak, to == ModFormat::Iostore)?;
    let dropped: HashSet<String> = left_out.iter().map(|path| content_key(path)).collect();
    let missing: Vec<&String> = contents_in
        .difference(&contents_out)
        .filter(|key| !dropped.contains(*key))
        .collect();
    if !missing.is_empty() {
        let sample: Vec<&str> = missing.iter().take(5).map(|s| s.as_str()).collect();
        return Err(format!(
            "Not repacking: the rebuilt mod is missing {} of {} items, for example:\n{}",
            missing.len(),
            contents_in.len(),
            sample.join("\n")
        ));
    }

    check_staged(staged, stem, to)?;
    if !dry_run {
        // The shared store may still hold the live container open, which Windows will not let the
        // swap move.
        containers::drop_cached_store();
        iostore_out::swap_into_place(&files.dir, stem, staged, &PARTS)?;
    }
    Ok(contents_out)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::pak::pack::non_package_files;
    use crate::pak::plain::PakOptions;
    use crate::pak::profile::PackCompression;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rivals-convert-{}-{}-{}",
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

    const ZLIB: IoStoreOptions = IoStoreOptions {
        compression: PackCompression::Zlib,
        oodle_level: None,
        obfuscate: None,
        reencode: false,
    };

    /// A game root whose `~mods` holds one plain pak with an INI in it.
    fn ini_mod(name: &str) -> (PathBuf, ModFiles) {
        let root = scratch(name);
        let pak = mods_dir(root.to_str().unwrap()).join("Ini_9999999_P.pak");
        plain::write_files(
            &pak,
            vec![(
                "Marvel/Config/DefaultEngine.ini".into(),
                b"[Core]\nA=1\n".to_vec(),
            )],
            &PakOptions {
                compression: PackCompression::Zlib,
                oodle_level: None,
            },
        )
        .expect("the mod");
        let files = ModFiles::of(&pak).expect("mod files");
        (root, files)
    }

    fn convert(root: &Path, files: &ModFiles, to: ModFormat) -> Result<ConvertReport, String> {
        convert_mod(
            root.to_str().unwrap(),
            files,
            to,
            &ZLIB,
            false,
            None,
            &AtomicBool::new(false),
            &|_, _, _| {},
        )
    }

    #[test]
    fn content_key_collapses_a_package_and_its_companions() {
        let base = content_key("Marvel/Content/Char/T_Body.uasset");
        for path in [
            "Marvel/Content/Char/T_Body.uexp",
            "Marvel/Content/Char/T_Body.ubulk",
            "Marvel/Content/Char/T_Body.uptnl",
            "Marvel/Content/Char/T_Body.m.ubulk",
        ] {
            assert_eq!(content_key(path), base, "{path}");
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
        // No .uexp beside it, so it is no package, and no file for the pak either: packing
        // refuses it.
        assert!(!loose.contains("Marvel/Content/Orphan.uasset"));
        assert!(!loose.contains("Marvel/Content/A.uasset"));
        assert!(!loose.contains("Marvel/Content/A.uexp"));
        assert!(!loose.contains("Marvel/Content/A.ubulk"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn only_a_mod_inside_the_mods_folder_is_converted() {
        let root = scratch("outside");
        let stray = root.join("stray.pak");
        write(&stray, "x");
        let err = validate_target(root.to_str().unwrap(), &stray).unwrap_err();
        assert!(err.contains("~mods"), "{err}");
        let inside = mods_dir(root.to_str().unwrap()).join("Some_9999999_P.pak");
        write(&inside, "x");
        assert!(validate_target(root.to_str().unwrap(), &inside).is_ok());
        let _ = fs::remove_dir_all(&root);
    }

    /// A switched-off mod is left alone, as the game would not load what it became anyway.
    #[test]
    fn a_switched_off_mod_is_refused() {
        let (root, files) = ini_mod("off");
        let (off, _) = crate::mod_ops::set_enabled(&files, false).expect("switch off");
        let err = convert(&root, &off, ModFormat::Pak).unwrap_err();
        assert!(err.contains("switched off"), "{err}");
        assert!(off.part("pak").is_file());
        let _ = fs::remove_dir_all(&root);
    }

    /// A rebuild that left out one of the files the format needs stops before the swap.
    #[test]
    fn a_missing_staged_file_stops_the_swap() {
        let staged = scratch("staged");
        write(&staged.join("M.pak"), "p");
        write(&staged.join("M.utoc"), "t");
        let err = check_staged(&staged, "M", ModFormat::Iostore).unwrap_err();
        assert!(err.contains("M.ucas"), "{err}");
        assert!(check_staged(&staged, "M", ModFormat::Pak).is_ok());
        let _ = fs::remove_dir_all(&staged);
    }

    /// A pak carrying a shader archive becomes a container holding its library, the archive's
    /// asset info taken in with it and the INI beside them in the pak, and comes back out as the
    /// archive and asset info a pak carries.
    #[test]
    fn a_shader_archive_survives_conversion_both_ways() {
        use crate::pak::shader_library::fixture::{MAP_HASH, cooked_archive, read_archive};
        let root = scratch("shaders");
        let archive = "Marvel/Content/ShaderArchive-ToolkitTest-PCD3D_SM6.ushaderbytecode";
        let info = "Marvel/Content/ShaderAssetInfo-ToolkitTest-PCD3D_SM6.assetinfo.json";
        let ini = "Marvel/Config/DefaultEngine.ini";
        let code: Vec<u8> = (0..64).collect();
        let pak = mods_dir(root.to_str().unwrap()).join("Shaders_9999999_P.pak");
        plain::write_files(
            &pak,
            vec![
                (archive.into(), cooked_archive(2, &code)),
                (info.into(), br#"{"ShaderCodeToAssets":[]}"#.to_vec()),
                (ini.into(), b"[Core]\nA=1\n".to_vec()),
            ],
            &PakOptions {
                compression: PackCompression::Zlib,
                oodle_level: None,
            },
        )
        .expect("the mod");
        let files = ModFiles::of(&pak).expect("mod files");

        let report = convert(&root, &files, ModFormat::Iostore).expect("to IoStore");
        assert_eq!(report.shader_libraries, [archive]);
        assert_eq!(list_utoc_paths(&files.part("utoc")).unwrap(), [archive]);
        assert_eq!(plain::list_files(&files.part("pak")).unwrap(), [ini]);

        convert(&root, &files, ModFormat::Pak).expect("back to a pak");
        assert!(!files.part("utoc").exists());
        let read: std::collections::HashMap<String, Vec<u8>> =
            plain::read_files(&files.part("pak"), &|_| true)
                .unwrap()
                .into_iter()
                .collect();
        let (maps, _, back) = read_archive(&read[archive]);
        assert_eq!((maps, back), (vec![MAP_HASH], code));
        assert!(
            read.contains_key(info) && read.contains_key(ini),
            "{read:?}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// An INI-only pak repacked as a pak keeps its contents. Converted to IoStore it becomes a
    /// container holding no package, as the game's own translations container is, with the INI in
    /// the pak beside it, and converts back.
    #[test]
    fn an_ini_pak_converts_both_ways() {
        let (root, files) = ini_mod("ini");
        let report = convert(&root, &files, ModFormat::Pak).expect("repack as a pak");
        assert_eq!(
            (report.format, report.assets_in, report.assets_out),
            ("pak", 1, 1)
        );
        let read = plain::read_files(&files.part("pak"), &|_| true).expect("read back");
        assert_eq!(
            read,
            vec![(
                "Marvel/Config/DefaultEngine.ini".to_string(),
                b"[Core]\nA=1\n".to_vec()
            )]
        );

        let report = convert(&root, &files, ModFormat::Iostore).expect("to IoStore");
        assert_eq!(
            (report.format, report.assets_in, report.carried_pak_entries),
            ("iostore", 1, 1)
        );
        let container = open_utoc(&files.part("utoc").to_string_lossy()).expect("open");
        assert_eq!(container.packages().count(), 0);
        drop(container);
        assert_eq!(
            plain::list_files(&files.part("pak")).unwrap(),
            ["Marvel/Config/DefaultEngine.ini"]
        );

        // Repacked again, now obfuscated: the container holds nothing still, and the INI stays.
        let obfuscated = convert_mod(
            root.to_str().unwrap(),
            &files,
            ModFormat::Iostore,
            &IoStoreOptions {
                obfuscate: Some(true),
                ..ZLIB
            },
            false,
            None,
            &AtomicBool::new(false),
            &|_, _, _| {},
        )
        .expect("obfuscated");
        assert!(obfuscated.obfuscated);
        assert!(utoc_is_obfuscated(&files.part("utoc")));
        assert_eq!(
            plain::list_files(&files.part("pak")).unwrap(),
            ["Marvel/Config/DefaultEngine.ini"]
        );

        convert(&root, &files, ModFormat::Pak).expect("back to a pak");
        assert!(!files.part("utoc").exists());
        assert_eq!(
            plain::read_files(&files.part("pak"), &|_| true).unwrap(),
            read
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// An IoStore mod repacked as IoStore keeps what it holds as it is stored: obfuscated, then
    /// plain, then obfuscated again, its blocks are copied each time, its chunks read back the
    /// same, its .ucas stays the size it was but for the padding encryption adds, and the files in
    /// its pak stay.
    #[test]
    fn a_container_repacked_as_a_container_copies_its_blocks() {
        use retoc::compression::CompressionMethod;
        use retoc::iostore_writer::IoStoreWriter;
        use retoc::{EIoChunkType, FIoChunkId, UEPath};

        let root = scratch("direct");
        let mods = mods_dir(root.to_str().unwrap());
        fs::create_dir_all(&mods).expect("mods");
        let utoc = mods.join("Direct_9999999_P.utoc");
        let engine = retoc::version::EngineVersion::UE5_3;
        let mut writer = IoStoreWriter::new(
            &utoc,
            engine.toc_version(),
            Some(engine.container_header_version()),
            "../../../".into(),
            Some(CompressionMethod::Zlib),
        )
        .expect("writer")
        .with_compression_block_size(crate::pak::profile::RIVALS_BLOCK_SIZE);
        let payloads: Vec<Vec<u8>> = (0..3u32)
            .map(|n| {
                (0..150_000 + n * 7_001)
                    .map(|i| (i % (200 + n)) as u8)
                    .collect()
            })
            .collect();
        let id = |n: usize| FIoChunkId::create(n as u64 + 11, 0, EIoChunkType::BulkData);
        for (n, data) in payloads.iter().enumerate() {
            let path = format!("../../../Marvel/Content/Direct/Thing{n}.ubulk");
            writer
                .write_chunk(id(n), Some(UEPath::new(&path)), data)
                .expect("chunk");
        }
        writer.finalize().expect("finalize");
        let pak = mods.join("Direct_9999999_P.pak");
        plain::write_files(
            &pak,
            vec![(
                "Marvel/Config/DefaultEngine.ini".into(),
                b"[Core]\nA=1\n".to_vec(),
            )],
            &PakOptions {
                compression: PackCompression::Zlib,
                oodle_level: None,
            },
        )
        .expect("the pak");
        let files = ModFiles::of(&pak).expect("mod files");
        let plain_size = fs::metadata(files.part("ucas")).expect("ucas").len();

        for obfuscate in [true, false, true] {
            let report = convert_mod(
                root.to_str().unwrap(),
                &files,
                ModFormat::Iostore,
                &IoStoreOptions {
                    obfuscate: Some(obfuscate),
                    ..ZLIB
                },
                false,
                None,
                &AtomicBool::new(false),
                &|_, _, _| {},
            )
            .unwrap_or_else(|e| panic!("obfuscate {obfuscate}: {e}"));
            assert!(report.direct, "{report:?}");
            assert_eq!(report.recompressed_blocks, 0, "{report:?}");
            assert!(report.copied_blocks >= 9, "{report:?}");
            assert_eq!(
                (report.assets_in, report.assets_out),
                (4, 4),
                "three chunks and the INI"
            );
            assert_eq!(utoc_is_obfuscated(&files.part("utoc")), obfuscate);
            let store = open_utoc(&files.part("utoc").to_string_lossy()).expect("open");
            for (n, data) in payloads.iter().enumerate() {
                assert!(&store.read(id(n)).expect("read") == data, "chunk {n}");
            }
            drop(store);
            let size = fs::metadata(files.part("ucas")).expect("ucas").len();
            let padding = if obfuscate { 16 * 9 } else { 0 };
            assert!(
                size >= plain_size && size <= plain_size + padding,
                "{size} against {plain_size}"
            );
            assert_eq!(
                plain::list_files(&files.part("pak")).unwrap(),
                ["Marvel/Config/DefaultEngine.ini"]
            );
        }
        let _ = fs::remove_dir_all(&root);
    }
}
