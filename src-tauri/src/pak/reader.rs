use std::{fs, io::BufReader, path::Path};

use walkdir::WalkDir;

use super::crypto::open_pak;
use crate::paths::paks_dir;

fn ensure_supported_pak(pak_path: &str) -> Result<(), String> {
    let name = Path::new(pak_path)
        .file_name()
        .and_then(|x| x.to_str())
        .unwrap_or_default();

    // Rivals update patch paks (e.g. Patch_-Windows_..._P.pak) are delta containers
    // and are merged by the game/launcher during update flow, not normal browseable paks.
    if name.starts_with("Patch_") {
        return Err(
            "Update patch pak (delta) is not browseable. Launch Marvel Rivals once after update."
                .to_string(),
        );
    }

    Ok(())
}

fn is_update_patch_pak_path(path: &str) -> bool {
    Path::new(path)
        .file_name()
        .and_then(|x| x.to_str())
        .is_some_and(|name| name.to_ascii_lowercase().starts_with("patch_"))
}

/// Walks the game's Paks directory and returns the absolute paths of every pak file found.
/// Mod paks from the ~mods subdirectory are included and sorted after game paks.
pub(super) fn list_pak_files(game_root: &str) -> Result<Vec<String>, String> {
    let dir = paks_dir(game_root);
    if !dir.is_dir() {
        return Err(format!("Paks directory not found: {}", dir.display()));
    }

    // Game paks (exclude ~-prefixed subdirs like ~mods)
    let mut game_paks: Vec<String> = WalkDir::new(&dir)
        .max_depth(2)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("pak"))
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

    // Mod paks from ~mods subdirectory
    let mods_dir = dir.join("~mods");
    let mut mod_paks: Vec<String> = if mods_dir.is_dir() {
        WalkDir::new(&mods_dir)
            .max_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("pak"))
            .map(|e| e.path().to_string_lossy().into_owned())
            .collect()
    } else {
        vec![]
    };
    mod_paks.sort();

    game_paks.extend(mod_paks);
    Ok(game_paks)
}

pub(super) fn list_pak_contents(pak_path: &str) -> Result<Vec<String>, String> {
    ensure_supported_pak(pak_path)?;
    Ok(open_pak(Path::new(pak_path))?.files())
}

pub(super) fn unpack_pak(pak_path: &str, output_dir: &str) -> Result<Vec<String>, String> {
    ensure_supported_pak(pak_path)?;
    let output = Path::new(output_dir);
    fs::create_dir_all(output).map_err(|e| e.to_string())?;

    let pak = open_pak(Path::new(pak_path))?;
    let files = pak.files();

    for name in &files {
        let stripped = name.trim_start_matches("../../../").trim_start_matches('/');
        let dest = output.join(stripped);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut reader = BufReader::new(fs::File::open(pak_path).map_err(|e| e.to_string())?);
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
    ensure_supported_pak(pak_path)?;
    let pak = open_pak(Path::new(pak_path))?;
    if let Some(parent) = Path::new(output_path).parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut reader = BufReader::new(fs::File::open(pak_path).map_err(|e| e.to_string())?);
    let mut out = fs::File::create(output_path).map_err(|e| e.to_string())?;
    pak.read_file(file_name, &mut reader, &mut out)
        .map_err(|e| e.to_string())
}
