//! Pak file listing, inspection, and content extraction.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rivals_core::pak::plain;
use serde::Serialize;
use walkdir::WalkDir;

use crate::paths::paks_dir;

#[derive(Serialize, Clone)]
pub(crate) struct PakFileInfo {
    pub path: String,
    pub has_utoc: bool,
    pub has_ucas: bool,
    /// Container ships its payload encrypted. Vanilla containers always do, so only on a mod
    /// does this mean obfuscation; callers scope the distinction.
    pub obfuscated: bool,
    pub optional_pak: Option<String>,
    pub optional_has_utoc: bool,
    pub optional_has_ucas: bool,
}

fn is_optional_pak_name(name: &str) -> bool {
    name.to_ascii_lowercase().contains("optional")
}

/// Strip the `optional` token from a pak filename to derive the base sibling name.
/// Matches the first case-insensitive occurrence; returns `None` if not present.
fn strip_optional_token(name: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    let idx = lower.find("optional")?;
    let mut out = String::with_capacity(name.len() - "optional".len());
    out.push_str(&name[..idx]);
    out.push_str(&name[idx + "optional".len()..]);
    Some(out)
}

fn is_update_patch_pak_path(path: &str) -> bool {
    Path::new(path)
        .file_name()
        .and_then(|x| x.to_str())
        .is_some_and(|name| name.to_ascii_lowercase().starts_with("patch_"))
}

/// List pak files under game `Paks`, then append `~mods` pak files.
pub(super) fn list_pak_files(game_root: &str, recursive: bool) -> Result<Vec<String>, String> {
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
        .filter(|e| !is_optional_pak_name(&e.file_name().to_string_lossy()))
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
        crate::mods::walk_mod_files(&mods_dir, recursive)
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
    plain::list_files(Path::new(pak_path))
}

/// Mount-stripped relative paths of entries the source pak stored uncompressed
/// (`compression_slot == None`). Used to reproduce the original compression layout on rebuild.
pub(super) fn pak_uncompressed_entries(pak_path: &str) -> Result<Vec<String>, String> {
    plain::uncompressed_entries(Path::new(pak_path))
}

/// Every entry but those named in `skip`, by their mount-stripped names.
pub(super) fn unpack_pak(
    pak_path: &str,
    output_dir: &str,
    skip: &[&str],
) -> Result<Vec<String>, String> {
    plain::extract(Path::new(pak_path), Path::new(output_dir), &|name| {
        !skip.contains(&name)
    })
}

pub(super) fn extract_single_file(
    pak_path: &str,
    file_name: &str,
    output_path: &str,
) -> Result<(), String> {
    plain::extract_one(Path::new(pak_path), file_name, Path::new(output_path))
}

/// Extract multiple files, by their mount-stripped names, from a pak into an output directory.
pub(super) fn extract_pak_files(
    pak_path: &str,
    file_names: &[String],
    output_dir: &str,
) -> Result<Vec<String>, String> {
    let wanted: HashSet<&str> = file_names.iter().map(|s| s.as_str()).collect();
    plain::extract(Path::new(pak_path), Path::new(output_dir), &|name| {
        wanted.contains(name)
    })
}

/// List pak files with companion file info (utoc/ucas presence) and any
/// paired optional IoStore container that lives alongside the base pak.
pub(super) fn list_pak_files_info(
    game_root: &str,
    recursive: bool,
) -> Result<Vec<PakFileInfo>, String> {
    let paths = list_pak_files(game_root, recursive)?;
    let optional_by_base = collect_optional_paks(game_root);
    Ok(paths
        .into_iter()
        .map(|p| {
            // String-based swap: Path::with_extension breaks on dotted filenames.
            let base = p
                .strip_suffix(".pak")
                .or_else(|| p.strip_suffix(".PAK"))
                .unwrap_or(&p);

            let optional_pak = optional_by_base.get(&p.to_ascii_lowercase()).cloned();
            let (optional_has_utoc, optional_has_ucas) = match &optional_pak {
                Some(opt) => {
                    let opt_base = opt
                        .strip_suffix(".pak")
                        .or_else(|| opt.strip_suffix(".PAK"))
                        .unwrap_or(opt);
                    (
                        Path::new(&format!("{opt_base}.utoc")).exists(),
                        Path::new(&format!("{opt_base}.ucas")).exists(),
                    )
                }
                None => (false, false),
            };

            let utoc = PathBuf::from(format!("{base}.utoc"));
            let has_utoc = utoc.exists();
            PakFileInfo {
                has_utoc,
                has_ucas: Path::new(&format!("{base}.ucas")).exists(),
                obfuscated: has_utoc && super::iostore::utoc_is_obfuscated(&utoc),
                optional_pak,
                optional_has_utoc,
                optional_has_ucas,
                path: p,
            }
        })
        .collect())
}

/// Walk the game `Paks` dir (excluding `~mods` overlays) for optional paks,
/// returning a map keyed by the lowercased base sibling pak path.
fn collect_optional_paks(game_root: &str) -> HashMap<String, String> {
    let dir = paks_dir(game_root);
    if !dir.is_dir() {
        return HashMap::new();
    }
    let mut map = HashMap::new();
    for entry in WalkDir::new(&dir)
        .max_depth(2)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        if entry.path().extension().and_then(|x| x.to_str()) != Some("pak") {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(&dir) else {
            continue;
        };
        if rel
            .parent()
            .and_then(|p| p.iter().next())
            .is_some_and(|segment| segment.to_string_lossy().starts_with('~'))
        {
            continue;
        }
        let file_name = entry.file_name().to_string_lossy().into_owned();
        if !is_optional_pak_name(&file_name) {
            continue;
        }
        let Some(base_name) = strip_optional_token(&file_name) else {
            continue;
        };
        let Some(parent) = entry.path().parent() else {
            continue;
        };
        let base_path = parent.join(&base_name);
        let optional_path = entry.path().to_string_lossy().into_owned();
        map.insert(
            base_path.to_string_lossy().to_ascii_lowercase(),
            optional_path,
        );
    }
    map
}
