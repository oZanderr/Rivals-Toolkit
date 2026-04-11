//! Hitsound mod builder backed by rebnk for BNK parsing/repacking.

use std::collections::HashMap;
use std::fs;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::pak;
use crate::pak::crypto::open_pak;
use crate::wav_to_wem;

static BNK_CACHE: OnceLock<Mutex<HashMap<String, Vec<u8>>>> = OnceLock::new();

const BNK_ENTRY_NAME: &str = "Marvel/Content/WwiseAudio/bnk_ui_battle.bnk";
const BNK_MATCH: &str = "bnk_ui_battle.bnk";

/// All supported hitsound slots: (WEM ID, key used in commands, human label).
const SOUND_SLOTS: &[(u32, &str, &str)] = &[
    (975983943, "body_hit", "bodyshot hit"),
    (681577199, "head_hit", "headshot hit"),
    (1066162905, "body_kill", "bodyshot kill"),
    (1011085352, "head_kill", "headshot kill"),
];

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

fn get_or_extract_bnk(game_root: &str) -> Result<Vec<u8>, String> {
    let cache = BNK_CACHE.get_or_init(|| Mutex::new(HashMap::new()));

    if let Some(bytes) = cache
        .lock()
        .map_err(|e| format!("BNK cache lock poisoned: {e}"))?
        .get(game_root)
        .cloned()
    {
        return Ok(bytes);
    }

    let (source_pak, entry) = find_source_bnk(game_root)?;
    let bytes = read_bnk_from_pak(&source_pak, &entry)?;

    cache
        .lock()
        .map_err(|e| format!("BNK cache lock poisoned: {e}"))?
        .insert(game_root.to_string(), bytes.clone());

    Ok(bytes)
}

pub(crate) fn build_hitsound_mod(
    game_root: &str,
    wavs: &HashMap<String, String>,
    output_pak: &str,
) -> Result<String, String> {
    if wavs.is_empty() {
        return Err("At least one hitsound must be provided".into());
    }

    let _temp_guard = TempDirGuard::create("oinkers_hitsounds")?;

    let bnk_bytes = get_or_extract_bnk(game_root)?;

    if bnk_bytes.len() < 8 || &bnk_bytes[0..4] != b"BKHD" {
        return Err("Extracted BNK appears corrupt or has unexpected header".to_string());
    }

    let bnk = rebnk::parse_bnk_from_bytes(&bnk_bytes, Path::new(BNK_ENTRY_NAME))
        .map_err(|e| format!("Failed to parse BNK: {e}"))?;

    let mut replacements: HashMap<u32, Vec<u8>> = HashMap::new();
    let mut summary_parts: Vec<String> = Vec::new();

    for &(wem_id, key, label) in SOUND_SLOTS {
        if let Some(wav_path) = wavs.get(key) {
            let (wem_bytes, _) = wav_to_wem::convert_to_bytes(Path::new(wav_path))
                .map_err(|e| format!("{label} WAV conversion failed: {e}"))?;

            if !bnk.wems.iter().any(|w| w.id == wem_id) {
                return Err(format!(
                    "WEM ID {wem_id} ({label}) not found in BNK; the game may have been updated"
                ));
            }

            replacements.insert(wem_id, wem_bytes);
            summary_parts.push(label.to_string());
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

pub(crate) fn extract_hitsound_wavs(
    game_root: &str,
    pak_path: &str,
    output_dir: &str,
) -> Result<String, String> {
    let pak_path = Path::new(pak_path);
    let pak = open_pak(pak_path)?;
    let files = pak.files();

    let entry = files
        .iter()
        .find(|f| f.eq_ignore_ascii_case(BNK_ENTRY_NAME))
        .or_else(|| {
            files
                .iter()
                .find(|f| f.to_ascii_lowercase().ends_with(BNK_MATCH))
        })
        .cloned()
        .ok_or_else(|| {
            format!(
                "{BNK_MATCH} not found in {}; this may not be a hitsound mod",
                pak_path.display()
            )
        })?;

    let bnk_bytes = read_bnk_from_pak(pak_path, &entry)?;
    let bnk = rebnk::parse_bnk_from_bytes(&bnk_bytes, Path::new(BNK_ENTRY_NAME))
        .map_err(|e| format!("Failed to parse BNK: {e}"))?;

    // Load original game BNK for comparison
    let original_bnk = get_or_extract_bnk(game_root)
        .ok()
        .and_then(|bytes| rebnk::parse_bnk_from_bytes(&bytes, Path::new(BNK_ENTRY_NAME)).ok());

    // Derive subfolder name from pak filename, stripping version suffix
    let pak_stem = pak_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("hitsound_mod");
    let folder_name = pak_stem
        .strip_suffix("_P")
        .and_then(|s| s.rsplit_once('_').map(|(base, _)| base))
        .unwrap_or(pak_stem);

    let out_dir = Path::new(output_dir).join(folder_name);
    fs::create_dir_all(&out_dir).map_err(|e| format!("Failed to create output directory: {e}"))?;

    let mut extracted: Vec<String> = Vec::new();

    for &(wem_id, key, label) in SOUND_SLOTS {
        let Some(wem) = bnk.wems.iter().find(|w| w.id == wem_id) else {
            continue;
        };

        // Skip WEMs that match the original game data
        if let Some(ref orig) = original_bnk
            && let Some(orig_wem) = orig.wems.iter().find(|w| w.id == wem_id)
            && wem.data == orig_wem.data
        {
            continue;
        }

        let wav_bytes = crate::wav_to_wem::wem_to_wav(&wem.data)
            .map_err(|e| format!("Failed to convert {label} WEM to WAV: {e}"))?;
        let out_path = out_dir.join(format!("{key}.wav"));
        fs::write(&out_path, wav_bytes)
            .map_err(|e| format!("Failed to write {}: {e}", out_path.display()))?;
        extracted.push(label.to_string());
    }

    if extracted.is_empty() {
        return Err("No modified hitsounds found in this mod".to_string());
    }

    let summary = extracted.join(" + ");
    Ok(format!(
        "Extracted {summary} hitsound(s) to {}",
        out_dir.display()
    ))
}

pub(crate) fn build_hitsound_mod_to_dir(
    game_root: &str,
    wavs: &HashMap<String, String>,
    mod_name: &str,
    output_dir: &str,
) -> Result<String, String> {
    let output_path = Path::new(output_dir).join(format!("{mod_name}_9999999_P.pak"));
    let result = build_hitsound_mod(game_root, wavs, &output_path.to_string_lossy())?;
    Ok(format!("{result} -> {}", output_path.display()))
}
