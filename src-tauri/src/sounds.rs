//! Sound mod builder backed by rebnk for BNK parsing/repacking.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use rayon::prelude::*;

use crate::audio;
use crate::concurrency;
use crate::pak;
use crate::pak::crypto::open_pak;

const ILLEGAL_NAME_CHARS: &[char] = &['/', '\\', ':', '*', '?', '"', '<', '>', '|'];

/// Reject mod names that could escape the chosen output directory or contain
/// characters disallowed on Windows file systems.
fn sanitize_mod_name(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Mod name cannot be empty".into());
    }
    if trimmed == "." || trimmed == ".." || trimmed.contains("..") {
        return Err("Mod name cannot contain '..'".into());
    }
    if trimmed.contains(ILLEGAL_NAME_CHARS) {
        return Err("Mod name contains illegal characters".into());
    }
    if trimmed.chars().any(|c| c.is_control()) {
        return Err("Mod name contains control characters".into());
    }
    Ok(trimmed.to_string())
}

static BNK_CACHE: OnceLock<Mutex<HashMap<String, Vec<u8>>>> = OnceLock::new();

const BNK_ENTRY_NAME: &str = "Marvel/Content/WwiseAudio/bnk_ui_battle.bnk";
const BNK_MATCH: &str = "bnk_ui_battle.bnk";

struct SoundSlot {
    wem_id: u32,
    key: &'static str,
    label: &'static str,
}

/// WEM IDs silenced when their trigger slot is replaced (e.g. avoid overlap of duplicate SFX).
const SILENCE_COMPANIONS: &[(u32, &str)] = &[(1071347262, "bodyshot_kill")];

const SOUND_SLOTS: &[SoundSlot] = &[
    SoundSlot {
        wem_id: 975983943,
        key: "bodyshot_hit",
        label: "bodyshot hit",
    },
    SoundSlot {
        wem_id: 681577199,
        key: "headshot_hit",
        label: "headshot hit",
    },
    SoundSlot {
        wem_id: 1066162905,
        key: "bodyshot_kill",
        label: "bodyshot kill",
    },
    SoundSlot {
        wem_id: 1011085352,
        key: "headshot_kill",
        label: "headshot kill",
    },
    SoundSlot {
        wem_id: 201900974,
        key: "killstreak_2k",
        label: "double kill",
    },
    SoundSlot {
        wem_id: 863035871,
        key: "killstreak_3k",
        label: "triple kill",
    },
    SoundSlot {
        wem_id: 741756168,
        key: "killstreak_4k",
        label: "quad kill",
    },
    SoundSlot {
        wem_id: 403900834,
        key: "killstreak_5k",
        label: "penta kill",
    },
    SoundSlot {
        wem_id: 623489461,
        key: "killstreak_6k",
        label: "hexa kill",
    },
    SoundSlot {
        wem_id: 47027831,
        key: "killstreak_7k",
        label: "septa kill",
    },
    SoundSlot {
        wem_id: 267915878,
        key: "heal_direct",
        label: "heal tick",
    },
    SoundSlot {
        wem_id: 516301180,
        key: "heal_pack_pickup",
        label: "health pack",
    },
    SoundSlot {
        wem_id: 775556792,
        key: "kf_assist",
        label: "kill assist",
    },
    SoundSlot {
        wem_id: 1033171184,
        key: "kf_heal_to_kill",
        label: "healed teammate killed enemy",
    },
    SoundSlot {
        wem_id: 379333292,
        key: "kf_teammate_kill",
        label: "teammate kill",
    },
    SoundSlot {
        wem_id: 56073220,
        key: "kf_teammate_died",
        label: "teammate killed",
    },
];

const SILENCE_SAMPLE_RATE: u32 = 48000;
/// 10ms of silence at 48kHz.
const SILENCE_FRAMES: u32 = SILENCE_SAMPLE_RATE / 100;
/// Stereo 16-bit: 2 channels * 2 bytes per sample.
const STEREO_BYTES_PER_FRAME: u32 = 4;
const SILENCE_PCM_BYTES: u32 = SILENCE_FRAMES * STEREO_BYTES_PER_FRAME;

fn silence_wem() -> Vec<u8> {
    let mut wem = audio::build_wem_header(SILENCE_PCM_BYTES, SILENCE_SAMPLE_RATE);
    wem.resize(wem.len() + SILENCE_PCM_BYTES as usize, 0);
    wem
}

fn find_bnk_entry(files: &[String]) -> Option<String> {
    files
        .iter()
        .find(|f| f.eq_ignore_ascii_case(BNK_ENTRY_NAME))
        .or_else(|| {
            files
                .iter()
                .find(|f| f.to_ascii_lowercase().ends_with(BNK_MATCH))
        })
        .cloned()
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

    // Sort puts patch paks last; last hit wins so patch overrides base content.
    let mut found: Option<(PathBuf, String)> = None;
    for pak_path in &pak_candidates {
        let pak = match open_pak(pak_path) {
            Ok(p) => p,
            Err(_) => continue,
        };
        if let Some(entry) = find_bnk_entry(&pak.files()) {
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

/// Per-slot sound input: source audio path and optional gain in decibels.
#[derive(serde::Deserialize)]
pub(crate) struct SoundInput {
    path: String,
    #[serde(default)]
    gain_db: f32,
}

fn build_sound_pak(
    game_root: &str,
    wavs: &HashMap<String, SoundInput>,
    output_pak: &str,
) -> Result<String, String> {
    if wavs.is_empty() {
        return Err("At least one sound must be provided".into());
    }

    let bnk_bytes = get_or_extract_bnk(game_root)?;

    if bnk_bytes.len() < 8 || &bnk_bytes[0..4] != b"BKHD" {
        return Err("Extracted BNK appears corrupt or has unexpected header".to_string());
    }

    let bnk = rebnk::parse_bnk_from_bytes(&bnk_bytes, Path::new(BNK_ENTRY_NAME))
        .map_err(|e| format!("Failed to parse BNK: {e}"))?;

    let bnk_ids: HashSet<u32> = bnk.wems.iter().map(|w| w.id).collect();

    // Verify all requested slots exist in BNK before doing expensive WAV conversion.
    for slot in SOUND_SLOTS {
        if wavs.contains_key(slot.key) && !bnk_ids.contains(&slot.wem_id) {
            return Err(format!(
                "WEM ID {} ({}) not found in BNK; the game may have been updated",
                slot.wem_id, slot.label
            ));
        }
    }

    let pending: Vec<(&'static SoundSlot, &SoundInput)> = SOUND_SLOTS
        .iter()
        .filter_map(|slot| wavs.get(slot.key).map(|input| (slot, input)))
        .collect();

    let converted: Vec<(u32, &'static str, Vec<u8>)> = concurrency::POOL.install(|| {
        pending
            .par_iter()
            .map(|(slot, input)| {
                let (wem_bytes, _) =
                    audio::convert_to_bytes_with_gain(Path::new(&input.path), input.gain_db)
                        .map_err(|e| format!("{} WAV conversion failed: {e}", slot.label))?;
                Ok((slot.wem_id, slot.label, wem_bytes))
            })
            .collect::<Result<Vec<_>, String>>()
    })?;

    let mut replacements: HashMap<u32, Vec<u8>> = HashMap::new();
    let mut summary_parts: Vec<String> = Vec::new();
    for (wem_id, label, bytes) in converted {
        replacements.insert(wem_id, bytes);
        summary_parts.push(label.to_string());
    }

    for &(companion_id, trigger_key) in SILENCE_COMPANIONS {
        if wavs.contains_key(trigger_key)
            && !replacements.contains_key(&companion_id)
            && bnk_ids.contains(&companion_id)
        {
            replacements.insert(companion_id, silence_wem());
        }
    }

    let patched_bnk = rebnk::pack_to_bytes(&bnk, &replacements)
        .map_err(|e| format!("Failed to repack BNK: {e}"))?;

    pak::write_pak_bytes(output_pak, vec![(BNK_ENTRY_NAME.to_string(), patched_bnk)])?;

    let summary = summary_parts.join(" + ");
    Ok(format!("Sound mod created with {summary} sound(s)"))
}

struct ExtractedSoundMod {
    out_dir: PathBuf,
    slot_paths: HashMap<String, PathBuf>,
    extracted_labels: Vec<String>,
    baseline_warning: Option<String>,
}

fn derive_mod_name_from_pak(pak_path: &Path) -> String {
    let stem = pak_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("sound_mod");
    stem.strip_suffix("_P")
        .and_then(|s| s.rsplit_once('_').map(|(base, _)| base))
        .unwrap_or(stem)
        .to_string()
}

fn extract_sound_pak_core(
    game_root: &str,
    pak_path: &Path,
    output_dir: &Path,
) -> Result<ExtractedSoundMod, String> {
    let pak = open_pak(pak_path)?;

    let entry = find_bnk_entry(&pak.files()).ok_or_else(|| {
        format!(
            "{BNK_MATCH} not found in {}; this may not be a sound mod",
            pak_path.display()
        )
    })?;

    let bnk_bytes = read_bnk_from_pak(pak_path, &entry)?;
    let bnk = rebnk::parse_bnk_from_bytes(&bnk_bytes, Path::new(BNK_ENTRY_NAME))
        .map_err(|e| format!("Failed to parse BNK: {e}"))?;

    // Load original game BNK for comparison. If unavailable, every slot present
    // in the mod's BNK is extracted (no diff filter); the caller is told so.
    let (original_bnk, baseline_warning) = match get_or_extract_bnk(game_root) {
        Ok(bytes) => match rebnk::parse_bnk_from_bytes(&bytes, Path::new(BNK_ENTRY_NAME)) {
            Ok(parsed) => (Some(parsed), None),
            Err(e) => {
                eprintln!("rivals-toolkit: failed to parse game BNK for baseline diff: {e}");
                (None, Some(format!("could not parse game BNK ({e})")))
            }
        },
        Err(e) => {
            eprintln!("rivals-toolkit: failed to load game BNK for baseline diff: {e}");
            (None, Some(format!("could not load game BNK ({e})")))
        }
    };

    let folder_name = derive_mod_name_from_pak(pak_path);
    let out_dir = output_dir.join(&folder_name);
    fs::create_dir_all(&out_dir).map_err(|e| format!("Failed to create output directory: {e}"))?;

    let mut extracted: Vec<String> = Vec::new();
    let mut slot_paths: HashMap<String, PathBuf> = HashMap::new();

    for slot in SOUND_SLOTS {
        let Some(wem) = bnk.wems.iter().find(|w| w.id == slot.wem_id) else {
            continue;
        };

        // Skip WEMs that match the original game data
        if let Some(ref orig) = original_bnk
            && let Some(orig_wem) = orig.wems.iter().find(|w| w.id == slot.wem_id)
            && wem.data == orig_wem.data
        {
            continue;
        }

        let wav_bytes = crate::audio::wem_to_wav(&wem.data)
            .map_err(|e| format!("Failed to convert {} WEM to WAV: {e}", slot.label))?;
        let out_path = out_dir.join(format!("{}.wav", slot.key));
        fs::write(&out_path, wav_bytes)
            .map_err(|e| format!("Failed to write {}: {e}", out_path.display()))?;
        extracted.push(slot.label.to_string());
        slot_paths.insert(slot.key.to_string(), out_path);
    }

    Ok(ExtractedSoundMod {
        out_dir,
        slot_paths,
        extracted_labels: extracted,
        baseline_warning,
    })
}

fn extract_sound_pak(game_root: &str, pak_path: &str, output_dir: &str) -> Result<String, String> {
    let result = extract_sound_pak_core(game_root, Path::new(pak_path), Path::new(output_dir))?;
    if result.extracted_labels.is_empty() {
        return Err(match result.baseline_warning {
            Some(w) => format!("No sounds extracted ({w})"),
            None => "No modified sounds found in this mod".to_string(),
        });
    }
    let summary = result.extracted_labels.join(" + ");
    let mut msg = format!(
        "Extracted {summary} sound(s) to {}",
        result.out_dir.display()
    );
    if let Some(w) = result.baseline_warning {
        msg.push_str(&format!(" (note: {w}; all slot sounds extracted)"));
    }
    Ok(msg)
}

fn hitsound_edit_cache_dir() -> Result<PathBuf, String> {
    dirs::cache_dir()
        .map(|d| d.join("rivals-toolkit").join("hitsound-edit"))
        .ok_or_else(|| "Cache directory unavailable".to_string())
}

#[derive(serde::Serialize)]
pub(crate) struct LoadedSoundMod {
    mod_name: String,
    slots: HashMap<String, String>,
    missing_baseline: Option<String>,
}

fn load_sound_mod_for_edit_impl(game_root: &str, pak_path: &str) -> Result<LoadedSoundMod, String> {
    let cache_root = hitsound_edit_cache_dir()?;
    if cache_root.exists() {
        fs::remove_dir_all(&cache_root).map_err(|e| format!("clear hitsound-edit cache: {e}"))?;
    }
    fs::create_dir_all(&cache_root).map_err(|e| format!("create hitsound-edit cache: {e}"))?;

    let extracted = extract_sound_pak_core(game_root, Path::new(pak_path), &cache_root)?;
    if extracted.slot_paths.is_empty() {
        return Err(match extracted.baseline_warning {
            Some(w) => format!("No modified sounds found in this mod ({w})"),
            None => "No modified sounds found in this mod".to_string(),
        });
    }

    let mod_name = derive_mod_name_from_pak(Path::new(pak_path));
    let slots: HashMap<String, String> = extracted
        .slot_paths
        .into_iter()
        .map(|(k, p)| (k, p.to_string_lossy().into_owned()))
        .collect();

    Ok(LoadedSoundMod {
        mod_name,
        slots,
        missing_baseline: extracted.baseline_warning,
    })
}

#[tauri::command]
pub(crate) async fn build_sound_mod(
    game_root: String,
    wavs: HashMap<String, SoundInput>,
    mod_name: String,
    output_dir: String,
) -> Result<String, String> {
    let safe_mod_name = sanitize_mod_name(&mod_name)?;
    tauri::async_runtime::spawn_blocking(move || {
        let output_path = Path::new(&output_dir).join(format!("{safe_mod_name}_9999999_P.pak"));
        let result = build_sound_pak(&game_root, &wavs, &output_path.to_string_lossy())?;
        Ok::<_, String>(format!("{result} -> {}", output_path.display()))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn extract_sound_wavs(
    game_root: String,
    pak_path: String,
    output_dir: String,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        extract_sound_pak(&game_root, &pak_path, &output_dir)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn load_sound_mod_for_edit(
    game_root: String,
    pak_path: String,
) -> Result<LoadedSoundMod, String> {
    tauri::async_runtime::spawn_blocking(move || {
        load_sound_mod_for_edit_impl(&game_root, &pak_path)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::sanitize_mod_name;

    #[test]
    fn accepts_plain_name() {
        assert_eq!(sanitize_mod_name("my_sounds").as_deref(), Ok("my_sounds"));
    }

    #[test]
    fn trims_whitespace() {
        assert_eq!(sanitize_mod_name("  hits  ").as_deref(), Ok("hits"));
    }

    #[test]
    fn rejects_empty() {
        assert!(sanitize_mod_name("").is_err());
        assert!(sanitize_mod_name("   ").is_err());
    }

    #[test]
    fn rejects_path_separators() {
        assert!(sanitize_mod_name("evil/name").is_err());
        assert!(sanitize_mod_name("evil\\name").is_err());
    }

    #[test]
    fn rejects_parent_traversal() {
        assert!(sanitize_mod_name("..").is_err());
        assert!(sanitize_mod_name("../etc").is_err());
        assert!(sanitize_mod_name("foo..bar").is_err());
    }

    #[test]
    fn rejects_windows_reserved_chars() {
        for c in [':', '*', '?', '"', '<', '>', '|'] {
            let name = format!("foo{c}bar");
            assert!(sanitize_mod_name(&name).is_err(), "should reject {c}");
        }
    }

    #[test]
    fn rejects_control_chars() {
        assert!(sanitize_mod_name("foo\nbar").is_err());
        assert!(sanitize_mod_name("foo\0bar").is_err());
    }
}
