use std::{fs, io::BufWriter, path::Path};

use walkdir::WalkDir;

use super::crypto::make_aes_key;

pub(super) fn repack_pak(input_dir: &str, output_pak: &str) -> Result<(), String> {
    let input = Path::new(input_dir);
    if !input.exists() {
        return Err(format!("Input directory does not exist: {input_dir}"));
    }
    if let Some(parent) = Path::new(output_pak).parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let out_file = fs::File::create(output_pak).map_err(|e| e.to_string())?;
    // Canonicalize after creation so we can skip it during the directory walk
    // (relevant when the output pak is saved inside the input directory).
    let output_canonical = Path::new(output_pak).canonicalize().ok();
    let mut pak_writer = repak::PakBuilder::new()
        .key(make_aes_key()?)
        .compression([repak::Compression::Oodle])
        .writer(
            BufWriter::new(out_file),
            repak::Version::V11,
            "../../../".to_string(),
            None,
        );

    let mut files_written = 0usize;
    for entry in WalkDir::new(input).into_iter().flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        // Skip the output pak itself if it lives inside the input directory.
        if let Some(ref canon_out) = output_canonical {
            if path.canonicalize().ok().as_ref() == Some(canon_out) {
                continue;
            }
        }
        let rel = path
            .strip_prefix(input)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        pak_writer
            .write_file(&rel, true, fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        files_written += 1;
    }

    if files_written == 0 {
        return Err("No files found in the input directory.".to_string());
    }
    pak_writer.write_index().map_err(|e| e.to_string())?;
    Ok(())
}
