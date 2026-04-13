use std::io;
use std::path::Path;

use serde::Serialize;

use crate::paths::mods_dir;

#[derive(Serialize)]
pub(crate) struct InstallResult {
    pub file_name: String,
    pub replaced_disabled: bool,
    pub replaced_enabled: bool,
}

pub(crate) fn open_mods_folder(game_root: &str) -> Result<(), String> {
    let mods = mods_dir(game_root);
    if !mods.exists() {
        return Err("Mods folder does not exist, install the bypass first!".to_string());
    }
    #[cfg(target_os = "windows")]
    std::process::Command::new("explorer")
        .arg(mods.to_string_lossy().as_ref())
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

const COMPANION_EXTS: &[&str] = &["ucas", "utoc"];

/// Enable or disable a mod and its companion `.ucas`/`.utoc` files.
pub(crate) fn toggle_mod_enabled(
    mods_folder: &str,
    full_name: &str,
    enabled: bool,
) -> Result<(), String> {
    let dir = Path::new(mods_folder);
    let from = dir.join(full_name);
    let to = if enabled {
        let base = full_name
            .strip_suffix(".disabled")
            .ok_or_else(|| format!("expected .disabled suffix on: {full_name}"))?;
        dir.join(base)
    } else {
        dir.join(format!("{full_name}.disabled"))
    };
    std::fs::rename(&from, &to).map_err(|e| e.to_string())?;

    let stem = if enabled {
        full_name
            .strip_suffix(".pak.disabled")
            .ok_or_else(|| format!("expected .pak.disabled: {full_name}"))?
    } else {
        full_name
            .strip_suffix(".pak")
            .ok_or_else(|| format!("expected .pak: {full_name}"))?
    };
    for ext in COMPANION_EXTS {
        let (c_from, c_to) = if enabled {
            (
                dir.join(format!("{stem}.{ext}.disabled")),
                dir.join(format!("{stem}.{ext}")),
            )
        } else {
            (
                dir.join(format!("{stem}.{ext}")),
                dir.join(format!("{stem}.{ext}.disabled")),
            )
        };
        if c_from.exists() {
            std::fs::rename(&c_from, &c_to).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Delete a mod and its companion `.ucas`/`.utoc` files.
pub(crate) fn delete_mod(mods_folder: &str, full_name: &str) -> Result<(), String> {
    let dir = Path::new(mods_folder);

    let stem = if let Some(s) = full_name.strip_suffix(".pak.disabled") {
        s
    } else if let Some(s) = full_name.strip_suffix(".pak") {
        s
    } else {
        return Err(format!("Unexpected mod filename: {full_name}"));
    };

    let candidates = [
        full_name.to_string(),
        format!("{stem}.ucas"),
        format!("{stem}.utoc"),
        format!("{stem}.ucas.disabled"),
        format!("{stem}.utoc.disabled"),
    ];

    for name in &candidates {
        let path = dir.join(name);
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| format!("Failed to delete {name}: {e}"))?;
        }
    }

    Ok(())
}

/// Install a mod pak from an external path into the mods folder.
/// Also copies any adjacent companion files (.ucas/.utoc) with the same stem.
/// If a `.disabled` version with the same name already exists it is removed first.
pub(crate) fn install_mod(mods_folder: &str, source_path: &str) -> Result<InstallResult, String> {
    let src = Path::new(source_path);
    let file_name = src
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("Invalid source path: {source_path}"))?
        .to_string();

    if !file_name.ends_with(".pak") {
        return Err(format!("Not a pak file: {file_name}"));
    }

    let dir = Path::new(mods_folder);
    let stem = &file_name[..file_name.len() - 4];

    if !dir.exists() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("Failed to create mods folder {}: {e}", dir.display()))?;
    }

    // Remove stale disabled version if present.
    let disabled_name = format!("{file_name}.disabled");
    let replaced_disabled = dir.join(&disabled_name).exists();
    if replaced_disabled {
        delete_mod(mods_folder, &disabled_name)?;
    }

    let replaced_enabled = dir.join(&file_name).exists();
    std::fs::copy(src, dir.join(&file_name))
        .map_err(|e| format!("Failed to copy {file_name}: {e}"))?;

    // Sync companion files: remove old ones, copy new ones from the source directory.
    if let Some(src_dir) = src.parent() {
        for ext in COMPANION_EXTS {
            let dest_companion = dir.join(format!("{stem}.{ext}"));
            let src_companion = src_dir.join(format!("{stem}.{ext}"));
            if src_companion.exists() {
                let _ = std::fs::copy(&src_companion, &dest_companion);
            } else if dest_companion.exists() {
                let _ = std::fs::remove_file(&dest_companion);
            }
        }
    }

    Ok(InstallResult {
        file_name,
        replaced_disabled,
        replaced_enabled,
    })
}

fn is_mod_ext(name: &str) -> bool {
    matches!(
        Path::new(name).extension().and_then(|x| x.to_str()),
        Some("pak" | "ucas" | "utoc")
    )
}

fn archive_format(path: &str) -> Result<ArchiveFormat, String> {
    match Path::new(path)
        .extension()
        .and_then(|x| x.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("zip") => Ok(ArchiveFormat::Zip),
        Some("7z") => Ok(ArchiveFormat::SevenZ),
        _ => Err(format!("Unsupported archive format: {path}")),
    }
}

enum ArchiveFormat {
    Zip,
    SevenZ,
}

pub(crate) fn install_from_archive(
    mods_folder: &str,
    archive_path: &str,
) -> Result<Vec<InstallResult>, String> {
    let format = archive_format(archive_path)?;

    let temp_dir = std::env::temp_dir().join(format!("rivals_archive_{}", std::process::id()));
    if temp_dir.exists() {
        let _ = std::fs::remove_dir_all(&temp_dir);
    }
    std::fs::create_dir_all(&temp_dir).map_err(|e| format!("Failed to create temp dir: {e}"))?;

    let extract_result = match format {
        ArchiveFormat::Zip => extract_zip(archive_path, &temp_dir),
        ArchiveFormat::SevenZ => extract_7z(archive_path, &temp_dir),
    };
    if let Err(e) = extract_result {
        let _ = std::fs::remove_dir_all(&temp_dir);
        return Err(e);
    }

    let mut results = Vec::new();
    for entry in std::fs::read_dir(&temp_dir)
        .map_err(|e| e.to_string())?
        .flatten()
    {
        let path = entry.path();
        if path.extension().and_then(|x| x.to_str()) == Some("pak") {
            results.push(install_mod(mods_folder, &path.to_string_lossy())?);
        }
    }

    let _ = std::fs::remove_dir_all(&temp_dir);

    if results.is_empty() {
        return Err("No .pak files found in the archive".to_string());
    }
    Ok(results)
}

fn extract_zip(archive_path: &str, temp_dir: &Path) -> Result<(), String> {
    let file = std::fs::File::open(archive_path).map_err(|e| format!("Failed to open zip: {e}"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("Invalid zip file: {e}"))?;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        let Some(name) = entry
            .enclosed_name()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        else {
            continue;
        };
        if !is_mod_ext(&name) {
            continue;
        }
        let dest = temp_dir.join(&name);
        let mut out =
            std::fs::File::create(&dest).map_err(|e| format!("Failed to create {name}: {e}"))?;
        io::copy(&mut entry, &mut out).map_err(|e| format!("Failed to extract {name}: {e}"))?;
    }
    Ok(())
}

fn extract_7z(archive_path: &str, temp_dir: &Path) -> Result<(), String> {
    let file = std::fs::File::open(archive_path).map_err(|e| format!("Failed to open 7z: {e}"))?;

    sevenz_rust2::decompress_with_extract_fn(file, temp_dir, |entry, reader, _dest| {
        if entry.is_directory() {
            return Ok(true);
        }
        let file_name = Path::new(entry.name())
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string());
        let Some(file_name) = file_name else {
            return Ok(true);
        };
        if !is_mod_ext(&file_name) {
            return Ok(true);
        }
        let out_path = temp_dir.join(&file_name);
        let mut out = std::fs::File::create(&out_path).map_err(|e| {
            sevenz_rust2::Error::Other(format!("Failed to create {file_name}: {e}").into())
        })?;
        io::copy(reader, &mut out).map_err(|e| {
            sevenz_rust2::Error::Other(format!("Failed to extract {file_name}: {e}").into())
        })?;
        Ok(true)
    })
    .map_err(|e| format!("Failed to read 7z archive: {e}"))?;

    Ok(())
}

fn collect_export_files(dir: &Path) -> (Vec<(std::path::PathBuf, String)>, u32) {
    let mut files = Vec::new();
    let mut pak_count = 0u32;
    for rel_path in super::walk_mod_files(dir) {
        let rel_str = rel_path.to_string_lossy().replace('\\', "/");
        if rel_str.ends_with(".disabled") {
            continue;
        }
        let ext = rel_path.extension().and_then(|x| x.to_str()).unwrap_or("");
        if !matches!(ext, "pak" | "ucas" | "utoc") {
            continue;
        }
        if ext == "pak" {
            pak_count += 1;
        }
        let full_path = dir.join(&rel_path);
        files.push((full_path, rel_str));
    }
    (files, pak_count)
}

pub(crate) fn export_mods_archive(mods_folder: &str, dest_path: &str) -> Result<String, String> {
    let format = archive_format(dest_path)?;
    let dir = Path::new(mods_folder);
    let (files, pak_count) = collect_export_files(dir);

    match format {
        ArchiveFormat::Zip => export_zip(dest_path, &files)?,
        ArchiveFormat::SevenZ => export_7z(dest_path, &files)?,
    }

    Ok(format!(
        "Exported {pak_count} mod{}",
        if pak_count == 1 { "" } else { "s" }
    ))
}

fn export_zip(dest_path: &str, files: &[(std::path::PathBuf, String)]) -> Result<(), String> {
    let file = std::fs::File::create(dest_path).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipWriter::new(file);
    // Level 3: pak/ucas payloads are largely pre-compressed, so diminishing returns above this.
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(Some(3));

    for (full_path, rel_str) in files {
        zip.start_file(rel_str, options)
            .map_err(|e| e.to_string())?;
        let mut src = std::fs::File::open(full_path).map_err(|e| e.to_string())?;
        io::copy(&mut src, &mut zip).map_err(|e| e.to_string())?;
    }
    zip.finish().map_err(|e| e.to_string())?;
    Ok(())
}

fn export_7z(dest_path: &str, files: &[(std::path::PathBuf, String)]) -> Result<(), String> {
    let mut writer = sevenz_rust2::ArchiveWriter::create(dest_path)
        .map_err(|e| format!("Failed to create 7z: {e}"))?;
    // Level 3: pak/ucas payloads are largely pre-compressed, so diminishing returns above this.
    // Half the cores to keep hyperthreaded / weaker hardware responsive.
    let threads = std::thread::available_parallelism()
        .map(|n| (n.get() / 2).max(1))
        .unwrap_or(2) as u32;
    writer.set_content_methods(vec![
        sevenz_rust2::encoder_options::Lzma2Options::from_level_mt(3, threads, 4 * 1024 * 1024)
            .into(),
    ]);

    for (full_path, rel_str) in files {
        let entry = sevenz_rust2::ArchiveEntry::from_path(full_path, rel_str.clone());
        let src = std::fs::File::open(full_path).map_err(|e| e.to_string())?;
        writer
            .push_archive_entry(entry, Some(src))
            .map_err(|e| format!("Failed to add {rel_str}: {e}"))?;
    }
    writer
        .finish()
        .map_err(|e| format!("Failed to finalize 7z: {e}"))?;
    Ok(())
}
