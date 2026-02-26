use std::{
    fs,
    io::BufReader,
    path::{Path, PathBuf},
};

use walkdir::WalkDir;

use super::crypto::open_pak;

pub(super) fn paks_dir(game_root: &str) -> PathBuf {
    PathBuf::from(game_root).join("MarvelGame\\Marvel\\Content\\Paks")
}

/// Walks the game's Paks directory and returns the absolute paths of every pak file found
pub(super) fn list_pak_files(game_root: &str) -> Result<Vec<String>, String> {
    let dir = paks_dir(game_root);
    if !dir.is_dir() {
        return Err(format!("Paks directory not found: {}", dir.display()));
    }

    let mut paks: Vec<String> = WalkDir::new(&dir)
        .max_depth(2)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("pak"))
        .filter(|e| {
            // Exclude files nested under any ~-prefixed subdirectory (e.g. ~mods)
            let Ok(rel) = e.path().strip_prefix(&dir) else { return false };
            !rel.parent()
                .and_then(|p| p.iter().next())
                .is_some_and(|segment| segment.to_string_lossy().starts_with('~'))
        })
        .map(|e| e.path().to_string_lossy().into_owned())
        .collect();

    paks.sort();
    Ok(paks)
}

pub(super) fn list_pak_contents(pak_path: &str) -> Result<Vec<String>, String> {
    Ok(open_pak(Path::new(pak_path))?.files())
}

pub(super) fn unpack_pak(pak_path: &str, output_dir: &str) -> Result<Vec<String>, String> {
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
    let pak = open_pak(Path::new(pak_path))?;
    if let Some(parent) = Path::new(output_path).parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut reader = BufReader::new(fs::File::open(pak_path).map_err(|e| e.to_string())?);
    let mut out = fs::File::create(output_path).map_err(|e| e.to_string())?;
    pak.read_file(file_name, &mut reader, &mut out)
        .map_err(|e| e.to_string())
}
