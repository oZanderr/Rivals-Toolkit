use std::io;
use std::path::Path;

use crate::paths::mods_dir;

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

/// Rename a mod file to enable or disable, including any companion .ucas/.utoc files.
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

/// Zip all enabled `.pak` files (and their `.ucas`/`.utoc` companions) into `dest_path`.
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
        // Skip anything that has been disabled
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
