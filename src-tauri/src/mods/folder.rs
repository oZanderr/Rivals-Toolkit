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

/// Export enabled mod files to a zip archive.
pub(crate) fn export_mods_zip(mods_folder: &str, dest_path: &str) -> Result<String, String> {
    let dir = Path::new(mods_folder);
    let file = std::fs::File::create(dest_path).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipWriter::new(file);
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);

    let mut pak_count = 0u32;
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())?.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".disabled") {
            continue;
        }
        let ext = path.extension().and_then(|x| x.to_str()).unwrap_or("");
        if !matches!(ext, "pak" | "ucas" | "utoc") {
            continue;
        }
        if ext == "pak" {
            pak_count += 1;
        }
        zip.start_file(&name, options).map_err(|e| e.to_string())?;
        let mut src = std::fs::File::open(&path).map_err(|e| e.to_string())?;
        io::copy(&mut src, &mut zip).map_err(|e| e.to_string())?;
    }

    zip.finish().map_err(|e| e.to_string())?;
    Ok(format!(
        "Exported {pak_count} mod{} to zip",
        if pak_count == 1 { "" } else { "s" }
    ))
}
