//! Switching mods on and off and deleting them, several at a time, and packing the switched-on
//! ones into an archive to share.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::mod_ops::{ModFiles, set_enabled};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BulkFailure {
    pub full_name: String,
    pub error: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BulkOpResult {
    pub successes: u32,
    pub failures: Vec<BulkFailure>,
}

/// Switches the mod whose pak is `full_name` below `mods` on or off, all its files as one step.
/// Says whether anything changed; a file already at a name it would take is never written over.
pub fn toggle(mods: &Path, full_name: &str, enabled: bool) -> Result<bool, String> {
    let files = ModFiles::of(&mods.join(full_name))?;
    set_enabled(&files, enabled).map(|(_, changed)| changed)
}

/// [`toggle`] for each mod, the failures collected rather than stopping at the first.
pub fn toggle_many(mods: &Path, names: &[String], enabled: bool) -> BulkOpResult {
    each(names, |name| toggle(mods, name, enabled).map(|_| ()))
}

/// Deletes each mod whose pak is named, its `.utoc` and `.ucas` with it.
pub fn delete_many(mods: &Path, names: &[String]) -> BulkOpResult {
    each(names, |name| crate::mods::delete_mod(mods, name))
}

fn each(names: &[String], mut act: impl FnMut(&str) -> Result<(), String>) -> BulkOpResult {
    let mut result = BulkOpResult::default();
    for name in names {
        match act(name) {
            Ok(()) => result.successes += 1,
            Err(error) => result.failures.push(BulkFailure {
                full_name: name.clone(),
                error,
            }),
        }
    }
    result
}

/// The switched-on mods' files below `mods`, each with its path in the archive, and how many
/// mods they make.
#[cfg(feature = "archives")]
fn switched_on_files(mods: &Path, recursive: bool) -> (Vec<(std::path::PathBuf, String)>, usize) {
    let mut files = Vec::new();
    let mut paks = 0;
    for relative in crate::mods::walk_mod_files(mods, recursive) {
        let name = relative.to_string_lossy().replace('\\', "/");
        let extension = relative.extension().and_then(|x| x.to_str()).unwrap_or("");
        if !matches!(extension, "pak" | "ucas" | "utoc") {
            continue;
        }
        paks += usize::from(extension == "pak");
        files.push((mods.join(&relative), name));
    }
    files.sort_by(|a, b| a.1.cmp(&b.1));
    (files, paks)
}

/// How many switched-on mods below `mods` an export would pack, refused as the export would be.
#[cfg(feature = "archives")]
pub fn archived_mod_count(mods: &Path, dest: &Path, recursive: bool) -> Result<usize, String> {
    super::archive_kind(dest)?;
    let (_, paks) = switched_on_files(mods, recursive);
    if paks == 0 {
        return Err(format!("{} holds no switched-on mod", mods.display()));
    }
    Ok(paks)
}

/// Packs every switched-on mod below `mods` into a `.zip` or `.7z` at `dest`, under the paths they
/// have there. Returns how many mods went in.
#[cfg(feature = "archives")]
pub fn export_archive(mods: &Path, dest: &Path, recursive: bool) -> Result<usize, String> {
    let kind = super::archive_kind(dest)?;
    let (files, paks) = switched_on_files(mods, recursive);
    if paks == 0 {
        return Err(format!("{} holds no switched-on mod", mods.display()));
    }
    match kind {
        super::ArchiveKind::Zip => export_zip(dest, &files)?,
        super::ArchiveKind::SevenZ => export_7z(dest, &files)?,
        // UnRAR extracts only, and its licence forbids building a RAR-compatible archiver.
        super::ArchiveKind::Rar => {
            return Err("Exporting to .rar is not supported. Use .zip or .7z.".to_string());
        }
    }
    Ok(paks)
}

#[cfg(feature = "archives")]
fn export_zip(dest: &Path, files: &[(std::path::PathBuf, String)]) -> Result<(), String> {
    let file = std::fs::File::create(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    // Level 3: pak and cas payloads are largely compressed already, so more does little.
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(Some(3));
    for (path, name) in files {
        zip.start_file(name, options).map_err(|e| e.to_string())?;
        let mut source = std::fs::File::open(path).map_err(|e| format!("{name}: {e}"))?;
        std::io::copy(&mut source, &mut zip).map_err(|e| format!("{name}: {e}"))?;
    }
    zip.finish().map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(feature = "archives")]
fn export_7z(dest: &Path, files: &[(std::path::PathBuf, String)]) -> Result<(), String> {
    let mut writer = sevenz_rust2::ArchiveWriter::create(dest)
        .map_err(|e| format!("Failed to create 7z: {e}"))?;
    // Level 3, as for zip, on half the cores so a weaker machine stays responsive.
    let threads = std::thread::available_parallelism()
        .map(|n| (n.get() / 2).max(1))
        .unwrap_or(2) as u32;
    writer.set_content_methods(vec![
        sevenz_rust2::encoder_options::Lzma2Options::from_level_mt(3, threads, 4 * 1024 * 1024)
            .into(),
    ]);
    for (path, name) in files {
        let entry = sevenz_rust2::ArchiveEntry::from_path(path, name.clone());
        let source = std::fs::File::open(path).map_err(|e| format!("{name}: {e}"))?;
        writer
            .push_archive_entry(entry, Some(source))
            .map_err(|e| format!("Failed to add {name}: {e}"))?;
    }
    writer
        .finish()
        .map_err(|e| format!("Failed to finalize 7z: {e}"))?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rivals-manage-{tag}-{stamp}"));
        std::fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    fn trio(dir: &Path, stem: &str, suffix: &str) {
        for ext in ["pak", "ucas", "utoc"] {
            std::fs::write(dir.join(format!("{stem}.{ext}{suffix}")), ext).expect("write");
        }
    }

    /// A mod switches off and on as a whole, and a mod already in that state is left alone.
    #[test]
    fn a_toggle_moves_the_whole_trio() {
        let dir = scratch("toggle");
        trio(&dir, "Mod", "");
        assert!(toggle(&dir, "Mod.pak", false).expect("disable"));
        for ext in ["pak", "ucas", "utoc"] {
            assert!(dir.join(format!("Mod.{ext}.disabled")).is_file());
            assert!(!dir.join(format!("Mod.{ext}")).exists());
        }
        assert!(!toggle(&dir, "Mod.pak.disabled", false).expect("already off"));
        assert!(toggle(&dir, "Mod.pak.disabled", true).expect("enable"));
        for ext in ["pak", "ucas", "utoc"] {
            assert!(dir.join(format!("Mod.{ext}")).is_file());
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A mod switched off by its pak and on by its container still mounts, so a toggle that
    /// would write over what is at a name it takes leaves the whole mod as it was.
    #[test]
    fn a_toggle_onto_a_taken_name_leaves_the_mod_as_it_was() {
        let dir = scratch("toggle-blocked");
        trio(&dir, "Mod", "");
        std::fs::create_dir_all(dir.join("Mod.utoc.disabled")).expect("blocker");
        let error = toggle(&dir, "Mod.pak", false).expect_err("the utoc cannot move");
        assert!(error.contains("Mod.utoc"), "{error}");
        for ext in ["pak", "ucas", "utoc"] {
            assert!(
                dir.join(format!("Mod.{ext}")).is_file(),
                "Mod.{ext} is still on"
            );
        }
        assert!(!dir.join("Mod.pak.disabled").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Several at once: each goes or fails on its own, and a failure is named.
    #[test]
    fn several_mods_switch_and_delete_each_on_their_own() {
        let dir = scratch("bulk");
        trio(&dir, "A", "");
        trio(&dir, "B", "");
        let names = vec![
            "A.pak".to_string(),
            "Missing.pak".to_string(),
            "B.pak".to_string(),
        ];
        let off = toggle_many(&dir, &names, false);
        assert_eq!(off.successes, 2);
        assert_eq!(off.failures.len(), 1);
        assert_eq!(off.failures[0].full_name, "Missing.pak");
        let gone = delete_many(&dir, &["A.pak.disabled".to_string()]);
        assert_eq!(gone.successes, 1);
        assert!(
            std::fs::read_dir(&dir)
                .unwrap()
                .filter_map(Result::ok)
                .all(|entry| entry.file_name().to_string_lossy().starts_with('B'))
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The switched-on mods export to a 7z, and installing that into another mods folder brings
    /// them back as they were.
    #[cfg(feature = "archives")]
    #[test]
    fn an_export_installs_back() {
        let dir = scratch("export");
        trio(&dir, "On_9999999_P", "");
        trio(&dir, "Off_9999999_P", ".disabled");
        let archive = dir.join("mods.7z");
        assert_eq!(export_archive(&dir, &archive, false).expect("export"), 1);
        let elsewhere = scratch("export-into");
        let installed = super::super::install(
            &elsewhere,
            &archive,
            &super::super::InstallOptions::default(),
        )
        .expect("install");
        assert_eq!(installed.len(), 1);
        for ext in ["pak", "ucas", "utoc"] {
            assert_eq!(
                std::fs::read_to_string(elsewhere.join(format!("On_9999999_P.{ext}"))).unwrap(),
                ext
            );
        }
        assert!(!elsewhere.join("Off_9999999_P.pak").exists());
        assert!(export_archive(&dir, &dir.join("mods.rar"), false).is_err());
        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&elsewhere).ok();
    }
}
