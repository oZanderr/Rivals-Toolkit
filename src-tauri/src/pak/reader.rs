use std::collections::HashSet;
use std::{fs, io::BufReader, path::Path};

use serde::Serialize;
use walkdir::WalkDir;

use super::crypto::open_pak;
use super::profile::strip_mount_prefix;
use crate::paths::paks_dir;

#[derive(Serialize, Clone)]
pub(crate) struct PakFileInfo {
    pub path: String,
    pub has_utoc: bool,
    pub has_ucas: bool,
}

fn is_update_patch_pak_path(path: &str) -> bool {
    Path::new(path)
        .file_name()
        .and_then(|x| x.to_str())
        .is_some_and(|name| name.to_ascii_lowercase().starts_with("patch_"))
}

/// List pak files under game `Paks`, then append `~mods` pak files.
pub(super) fn list_pak_files(game_root: &str) -> Result<Vec<String>, String> {
    let dir = paks_dir(game_root);
    if !dir.is_dir() {
        return Err(format!("Paks directory not found: {}", dir.display()));
    }

    let mut game_paks: Vec<String> = WalkDir::new(&dir)
        .max_depth(2)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("pak"))
        .filter(|e| {
            // Skip "optional" paks – they are empty I/O Store placeholders.
            let name = e.file_name().to_string_lossy().to_ascii_lowercase();
            !name.contains("optional")
        })
        .filter(|e| {
            let Ok(rel) = e.path().strip_prefix(&dir) else {
                return false;
            };
            !rel.parent()
                .and_then(|p| p.iter().next())
                .is_some_and(|segment| segment.to_string_lossy().starts_with('~'))
        })
        .map(|e| e.path().to_string_lossy().into_owned())
        .collect();
    game_paks.sort_by(|a, b| {
        let a_key = (is_update_patch_pak_path(a), a.to_ascii_lowercase());
        let b_key = (is_update_patch_pak_path(b), b.to_ascii_lowercase());
        a_key.cmp(&b_key)
    });

    let mods_dir = dir.join("~mods");
    let mut mod_paks: Vec<String> = if mods_dir.is_dir() {
        crate::mods::walk_mod_files(&mods_dir)
            .into_iter()
            .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("pak"))
            .map(|p| mods_dir.join(p).to_string_lossy().into_owned())
            .collect()
    } else {
        vec![]
    };
    mod_paks.sort();

    game_paks.extend(mod_paks);
    Ok(game_paks)
}

pub(super) fn list_pak_contents(pak_path: &str) -> Result<Vec<String>, String> {
    Ok(open_pak(Path::new(pak_path))?.files())
}

pub(super) fn unpack_pak(
    pak_path: &str,
    output_dir: &str,
    skip: &[&str],
) -> Result<Vec<String>, String> {
    let output = Path::new(output_dir);
    fs::create_dir_all(output).map_err(|e| e.to_string())?;

    let pak = open_pak(Path::new(pak_path))?;
    let files: Vec<String> = pak
        .files()
        .into_iter()
        .filter(|f| !skip.contains(&f.as_str()))
        .collect();

    let mut reader = BufReader::new(fs::File::open(pak_path).map_err(|e| e.to_string())?);
    // Extract in file-offset order to read sequentially and avoid backward seeks.
    for name in pak.files_by_offset() {
        let stripped = strip_mount_prefix(name);
        if skip.contains(&stripped.as_str()) {
            continue;
        }
        let dest = output.join(&stripped);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut out = fs::File::create(&dest).map_err(|e| e.to_string())?;
        pak.read_file(name, &mut reader, &mut out)
            .map_err(|e| e.to_string())?;
    }
    Ok(files)
}

pub(super) fn extract_single_file(
    pak_path: &str,
    file_name: &str,
    output_path: &str,
) -> Result<(), String> {
    let pak = open_pak(Path::new(pak_path))?;
    if let Some(parent) = Path::new(output_path).parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut reader = BufReader::new(fs::File::open(pak_path).map_err(|e| e.to_string())?);
    let mut out = fs::File::create(output_path).map_err(|e| e.to_string())?;
    pak.read_file(file_name, &mut reader, &mut out)
        .map_err(|e| e.to_string())
}

/// Extract multiple files from a pak into an output directory.
pub(super) fn extract_pak_files(
    pak_path: &str,
    file_names: &[String],
    output_dir: &str,
) -> Result<Vec<String>, String> {
    let output = Path::new(output_dir);
    fs::create_dir_all(output).map_err(|e| e.to_string())?;

    let pak = open_pak(Path::new(pak_path))?;
    let wanted: HashSet<&str> = file_names.iter().map(|s| s.as_str()).collect();
    let mut reader = BufReader::new(fs::File::open(pak_path).map_err(|e| e.to_string())?);
    let mut extracted = Vec::new();

    // Iterate in file-offset order for sequential reads
    for name in pak.files_by_offset() {
        let stripped = strip_mount_prefix(name);
        if !wanted.contains(stripped.as_str()) {
            continue;
        }
        let dest = output.join(&stripped);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut out = fs::File::create(&dest).map_err(|e| e.to_string())?;
        pak.read_file(name, &mut reader, &mut out)
            .map_err(|e| e.to_string())?;
        extracted.push(stripped);
    }
    Ok(extracted)
}

/// List pak files with companion file info (utoc/ucas presence).
pub(super) fn list_pak_files_info(game_root: &str) -> Result<Vec<PakFileInfo>, String> {
    let paths = list_pak_files(game_root)?;
    Ok(paths
        .into_iter()
        .map(|p| {
            // String-based swap: Path::with_extension breaks on dotted filenames.
            let base = p
                .strip_suffix(".pak")
                .or_else(|| p.strip_suffix(".PAK"))
                .unwrap_or(&p);
            PakFileInfo {
                has_utoc: Path::new(&format!("{base}.utoc")).exists(),
                has_ucas: Path::new(&format!("{base}.ucas")).exists(),
                path: p,
            }
        })
        .collect())
}
