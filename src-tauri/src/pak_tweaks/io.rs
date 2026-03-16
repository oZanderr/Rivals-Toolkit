use std::{fs, io::BufReader, path::Path};
use walkdir::WalkDir;

use crate::pak::crypto::{make_aes_key, open_pak};
use crate::pak::profile::{RIVALS_PROFILE, strip_mount_prefix};

use super::PakIniInfo;

/// Inspect a pak for tweakable INI entries.
pub(super) fn inspect_pak_for_ini(pak_path: &Path) -> Result<Option<PakIniInfo>, String> {
    let pak = open_pak(pak_path)?;
    let files = pak.files();

    let mut device_profiles_entry = None;
    let mut engine_ini_entry = None;

    for f in &files {
        let lower = f.to_ascii_lowercase();
        if lower.ends_with("defaultdeviceprofiles.ini") {
            device_profiles_entry = Some(f.clone());
        } else if lower.ends_with("defaultengine.ini") {
            engine_ini_entry = Some(f.clone());
        }
    }

    if device_profiles_entry.is_none() && engine_ini_entry.is_none() {
        return Ok(None);
    }

    let pak_name = pak_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();

    Ok(Some(PakIniInfo {
        pak_name,
        pak_path: pak_path.to_string_lossy().into_owned(),
        has_device_profiles: device_profiles_entry.is_some(),
        has_engine_ini: engine_ini_entry.is_some(),
        device_profiles_entry,
        engine_ini_entry,
    }))
}

/// Extract one pak entry to a UTF-8 string.
pub(super) fn extract_file_to_string(pak_path: &Path, entry: &str) -> Result<String, String> {
    let pak = open_pak(pak_path)?;
    let mut reader = BufReader::new(fs::File::open(pak_path).map_err(|e| e.to_string())?);
    let mut buf = Vec::new();
    pak.read_file(entry, &mut reader, &mut buf)
        .map_err(|e| e.to_string())?;
    String::from_utf8(buf).map_err(|e| format!("INI file is not valid UTF-8: {}", e))
}

/// Extract all pak entries to a directory.
pub(super) fn unpack_to_dir(pak_path: &Path, output_dir: &Path) -> Result<(), String> {
    fs::create_dir_all(output_dir).map_err(|e| e.to_string())?;

    let pak = open_pak(pak_path)?;
    let files = pak.files();

    for name in &files {
        let stripped = strip_mount_prefix(name);
        let dest = output_dir.join(stripped);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut reader = BufReader::new(fs::File::open(pak_path).map_err(|e| e.to_string())?);
        let mut out = fs::File::create(&dest).map_err(|e| e.to_string())?;
        pak.read_file(name, &mut reader, &mut out)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Repack a directory into a pak file.
pub(super) fn repack_dir_to_pak(input_dir: &Path, output_pak: &Path) -> Result<(), String> {
    use std::io::BufWriter;

    if let Some(parent) = output_pak.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let out_file = fs::File::create(output_pak).map_err(|e| e.to_string())?;
    // Canonicalize output to avoid writing the output file back into itself.
    let output_canonical = output_pak.canonicalize().ok();
    let mut pak_writer = repak::PakBuilder::new()
        .profile(RIVALS_PROFILE.repak_profile())
        .key(make_aes_key()?)
        .compression(RIVALS_PROFILE.compression())
        .writer(
            BufWriter::new(out_file),
            RIVALS_PROFILE.pak_version(),
            RIVALS_PROFILE.mount_point().to_string(),
            None,
        );

    for entry in WalkDir::new(input_dir).into_iter().flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if let Some(ref canon_out) = output_canonical
            && path.canonicalize().ok().as_ref() == Some(canon_out)
        {
            continue;
        }
        let rel = path
            .strip_prefix(input_dir)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        pak_writer
            .write_file(&rel, true, fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    }

    pak_writer.write_index().map_err(|e| e.to_string())?;
    Ok(())
}
