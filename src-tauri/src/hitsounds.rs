//! Hitsound mod builder backed by rebnk for BNK parsing/repacking.

use std::collections::HashMap;
use std::fs;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use crate::pak;
use crate::pak::crypto::open_pak;
use crate::wav_to_wem;

const HEAD_WEM_ID: u32 = 681577199;
const BODY_WEM_ID: u32 = 975983943;
const BNK_ENTRY_NAME: &str = "Marvel/Content/WwiseAudio/bnk_ui_battle.bnk";
const BNK_MATCH: &str = "bnk_ui_battle.bnk";

struct TempDirGuard {
    path: PathBuf,
}

impl TempDirGuard {
    fn create(prefix: &str) -> Result<Self, String> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_millis();
        let path = std::env::temp_dir().join(format!("{prefix}_{now}"));
        fs::create_dir_all(&path).map_err(|e| format!("Failed to create temp dir: {e}"))?;
        Ok(Self { path })
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn find_source_bnk(game_root: &str) -> Result<(PathBuf, String), String> {
    let paks_dir = crate::paths::paks_dir(game_root);
    if !paks_dir.is_dir() {
        return Err(format!("Paks directory not found: {}", paks_dir.display()));
    }

    let mut pak_candidates: Vec<PathBuf> = walkdir::WalkDir::new(&paks_dir)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("pak"))
        .filter(|e| {
            let Ok(rel) = e.path().strip_prefix(&paks_dir) else {
                return false;
            };
            !rel.parent()
                .and_then(|p| p.iter().next())
                .is_some_and(|segment| segment.to_string_lossy().starts_with('~'))
        })
        .map(|e| e.path().to_path_buf())
        .collect();

    pak_candidates.sort_by(|a, b| {
        let a_is_patch = a
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.to_ascii_lowercase().starts_with("patch_"));
        let b_is_patch = b
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.to_ascii_lowercase().starts_with("patch_"));
        a_is_patch.cmp(&b_is_patch).then_with(|| a.cmp(b))
    });

    let mut found: Option<(PathBuf, String)> = None;
    for pak_path in &pak_candidates {
        let pak = match open_pak(pak_path) {
            Ok(p) => p,
            Err(_) => continue,
        };

        let files = pak.files();
        let exact = files
            .iter()
            .find(|f| f.eq_ignore_ascii_case(BNK_ENTRY_NAME))
            .cloned();
        let fallback = files
            .iter()
            .find(|f| f.to_ascii_lowercase().ends_with(BNK_MATCH))
            .cloned();

        if let Some(entry) = exact.or(fallback) {
            found = Some((pak_path.clone(), entry));
        }
    }

    found.ok_or_else(|| {
        format!(
            "{BNK_MATCH} not found in any game pak under {}",
            paks_dir.display()
        )
    })
}

fn read_bnk_from_pak(pak_path: &Path, entry: &str) -> Result<Vec<u8>, String> {
    let pak = open_pak(pak_path)?;
    let mut reader = BufReader::new(fs::File::open(pak_path).map_err(|e| e.to_string())?);
    let mut out = Vec::new();
    pak.read_file(entry, &mut reader, &mut out)
        .map_err(|e| format!("Failed to extract BNK from {}: {e}", pak_path.display()))?;
    Ok(out)
}

pub(crate) fn build_hitsound_mod(
    game_root: &str,
    head_wav: Option<&str>,
    body_wav: Option<&str>,
    output_pak: &str,
) -> Result<String, String> {
    if head_wav.is_none() && body_wav.is_none() {
        return Err("At least one hitsound (head or body) must be provided".into());
    }

    let _temp_guard = TempDirGuard::create("oinkers_hitsounds")?;

    let (source_pak, bnk_entry) = find_source_bnk(game_root)?;
    let bnk_bytes = read_bnk_from_pak(&source_pak, &bnk_entry)?;

    if bnk_bytes.len() < 8 || &bnk_bytes[0..4] != b"BKHD" {
        return Err("Extracted BNK appears corrupt or has unexpected header".to_string());
    }

    let bnk = rebnk::parse_bnk_from_bytes(&bnk_bytes, Path::new(BNK_ENTRY_NAME))
        .map_err(|e| format!("Failed to parse BNK: {e}"))?;

    let mut replacements: HashMap<u32, Vec<u8>> = HashMap::new();
    let mut summary_parts: Vec<String> = Vec::new();

    if let Some(wav_path) = head_wav {
        let (wem_bytes, _) = wav_to_wem::convert_to_bytes(Path::new(wav_path))
            .map_err(|e| format!("Head WAV conversion failed: {e}"))?;
        replacements.insert(HEAD_WEM_ID, wem_bytes);
        summary_parts.push("head".to_string());
    }

    if let Some(wav_path) = body_wav {
        let (wem_bytes, _) = wav_to_wem::convert_to_bytes(Path::new(wav_path))
            .map_err(|e| format!("Body WAV conversion failed: {e}"))?;
        replacements.insert(BODY_WEM_ID, wem_bytes);
        summary_parts.push("body".to_string());
    }

    for (id, label) in [(HEAD_WEM_ID, "head"), (BODY_WEM_ID, "body")] {
        if replacements.contains_key(&id) && !bnk.wems.iter().any(|w| w.id == id) {
            return Err(format!(
                "WEM ID {id} ({label} hitsound) not found in BNK; the game may have been updated"
            ));
        }
    }

    let patched_bnk_path = _temp_guard.path.join("bnk_ui_battle_patched.bnk");
    rebnk::pack(&bnk, &replacements, &patched_bnk_path)
        .map_err(|e| format!("Failed to repack BNK: {e}"))?;

    let patched_bnk =
        fs::read(&patched_bnk_path).map_err(|e| format!("Failed to read rebuilt BNK: {e}"))?;

    pak::write_pak_bytes(output_pak, vec![(BNK_ENTRY_NAME.to_string(), patched_bnk)])?;

    let summary = summary_parts.join(" + ");
    Ok(format!("Hitsound mod created with {summary} sound(s)"))
}

pub(crate) fn build_hitsound_mod_to_dir(
    game_root: &str,
    head_wav: Option<&str>,
    body_wav: Option<&str>,
    mod_name: &str,
    output_dir: &str,
) -> Result<String, String> {
    let output_path = Path::new(output_dir).join(format!("{mod_name}_9999999_P.pak"));
    let result = build_hitsound_mod(
        game_root,
        head_wav,
        body_wav,
        &output_path.to_string_lossy(),
    )?;
    Ok(format!("{result} -> {}", output_path.display()))
}
