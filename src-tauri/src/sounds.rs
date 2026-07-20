//! Sound mod builder backed by rebnk for BNK parsing/repacking.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use rayon::prelude::*;
use tauri::State;

use crate::audio;
use crate::concurrency;
use crate::pak;
use crate::pak::crypto::open_pak;
use crate::settings::SettingsState;

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
/// Per-language announcer bank (Marvel/Content/WwiseAudio/<Lang>/bnk_vo_system.bnk).
const VO_BANK_MATCH: &str = "bnk_vo_system.bnk";

/// Per-bank bus reroute used to bypass shared, always-on bus effects: a replaced sound that still
/// resolves to `fx_bus` is re-pointed to `clean_bus` (a verified effect-free ancestor). Ids come
/// from the current game's Init.bnk bus hierarchy; the reroute is skipped if a sound resolves
/// elsewhere, so a changed hierarchy is left untouched.
struct FilterBypass {
    fx_bus: u32,
    clean_bus: u32,
}

/// bnk_ui_battle hit/kill SFX: bus 1952531228 carries a bus effect; 2791637696 is its clean parent.
const UI_BATTLE_BYPASS: FilterBypass = FilterBypass {
    fx_bus: 1952531228,
    clean_bus: 2791637696,
};

/// Announcer callouts: bus 3919227308 carries the 4-effect "PA" chain; 812276737 is its clean parent.
const VO_SYSTEM_BYPASS: FilterBypass = FilterBypass {
    fx_bus: 3919227308,
    clean_bus: 812276737,
};

/// Strip every removable layer of game filtering from the replaced `ids` in `bnk`: baked LPF/HPF on
/// the sound, effects inherited from its parent actor-mixer chain, and the shared bus effect chain.
fn strip_filtering(bnk: &mut rebnk::BnkFile, ids: &HashSet<u32>, bypass: &FilterBypass) {
    rebnk::clear_sound_filters(bnk, ids);
    rebnk::override_parent_fx(bnk, ids);
    rebnk::reroute_sound_bus(bnk, ids, bypass.fx_bus, bypass.clean_bus);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SoundBank {
    /// Marvel/Content/WwiseAudio/bnk_ui_battle.bnk (a single bank).
    UiBattle,
    /// Marvel/Content/WwiseAudio/<Language>/bnk_vo_system.bnk (one per language; all patched).
    VoSystem,
}

struct SoundSlot {
    wem_id: u32,
    key: &'static str,
    label: &'static str,
    bank: SoundBank,
}

/// WEM IDs silenced when their trigger slot is replaced (e.g. avoid overlap of duplicate SFX).
const SILENCE_COMPANIONS: &[(u32, &str)] = &[(1071347262, "bodyshot_kill")];

const SOUND_SLOTS: &[SoundSlot] = &[
    // ── bnk_ui_battle.bnk: in-memory hit/kill SFX ──
    SoundSlot {
        wem_id: 975983943,
        key: "bodyshot_hit",
        label: "bodyshot hit",
        bank: SoundBank::UiBattle,
    },
    SoundSlot {
        wem_id: 681577199,
        key: "headshot_hit",
        label: "headshot hit",
        bank: SoundBank::UiBattle,
    },
    SoundSlot {
        wem_id: 1066162905,
        key: "bodyshot_kill",
        label: "bodyshot kill",
        bank: SoundBank::UiBattle,
    },
    SoundSlot {
        wem_id: 1011085352,
        key: "headshot_kill",
        label: "headshot kill",
        bank: SoundBank::UiBattle,
    },
    SoundSlot {
        wem_id: 267915878,
        key: "heal_direct",
        label: "heal tick",
        bank: SoundBank::UiBattle,
    },
    SoundSlot {
        wem_id: 516301180,
        key: "heal_pack_pickup",
        label: "health pack",
        bank: SoundBank::UiBattle,
    },
    SoundSlot {
        wem_id: 775556792,
        key: "kf_assist",
        label: "kill assist",
        bank: SoundBank::UiBattle,
    },
    SoundSlot {
        wem_id: 1033171184,
        key: "kf_heal_to_kill",
        label: "healed teammate killed enemy",
        bank: SoundBank::UiBattle,
    },
    SoundSlot {
        wem_id: 379333292,
        key: "kf_teammate_kill",
        label: "teammate kill",
        bank: SoundBank::UiBattle,
    },
    SoundSlot {
        wem_id: 56073220,
        key: "kf_teammate_died",
        label: "teammate killed",
        bank: SoundBank::UiBattle,
    },
    // ── <Lang>/bnk_vo_system.bnk: announcer multi-KO callouts ──
    SoundSlot {
        wem_id: 980924621,
        key: "ko_double",
        label: "double KO",
        bank: SoundBank::VoSystem,
    },
    SoundSlot {
        wem_id: 235211223,
        key: "ko_triple",
        label: "triple KO",
        bank: SoundBank::VoSystem,
    },
    SoundSlot {
        wem_id: 103468488,
        key: "ko_quad",
        label: "quad KO",
        bank: SoundBank::VoSystem,
    },
    SoundSlot {
        wem_id: 90513181,
        key: "ko_penta",
        label: "penta KO",
        bank: SoundBank::VoSystem,
    },
    SoundSlot {
        wem_id: 918169832,
        key: "ko_hexa",
        label: "hexa KO",
        bank: SoundBank::VoSystem,
    },
    SoundSlot {
        wem_id: 639270720,
        key: "ko_septa",
        label: "septa KO",
        bank: SoundBank::VoSystem,
    },
    SoundSlot {
        wem_id: 595297408,
        key: "ko_ace",
        label: "ace",
        bank: SoundBank::VoSystem,
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

/// Game pak files under `Paks`, excluding the `~mods` tree, sorted so `patch_*` paks come last
/// (the caller's last hit wins, letting patch content override base content).
fn game_pak_candidates(game_root: &str) -> Result<Vec<PathBuf>, String> {
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

    Ok(pak_candidates)
}

fn find_source_bnk(game_root: &str) -> Result<(PathBuf, String), String> {
    let candidates = game_pak_candidates(game_root)?;

    // Last hit wins so patch overrides base content.
    let mut found: Option<(PathBuf, String)> = None;
    for pak_path in &candidates {
        let pak = match open_pak(pak_path) {
            Ok(p) => p,
            Err(_) => continue,
        };
        if let Some(entry) = find_bnk_entry(&pak.files()) {
            found = Some((pak_path.clone(), entry));
        }
    }

    found.ok_or_else(|| format!("{BNK_MATCH} not found in any game pak"))
}

/// Compose the full game-relative path from a pak's mount point and a raw index key, dropping the
/// engine's leading `../../../`. `pak.files()` returns keys relative to the pak's own mount, which
/// for the VO paks sits at `WwiseAudio/`; a mod must store the full path for the loader to match.
fn full_game_path(mount_point: &str, key: &str) -> String {
    format!("{mount_point}{key}")
        .trim_start_matches("../")
        .to_string()
}

/// Every per-language `bnk_vo_system.bnk` (patch paks override base). Returns
/// `(pak_path, raw_key, full_path)`: the raw key reads from the source pak; the full path is what
/// the mod pak must store.
fn find_vo_banks(game_root: &str) -> Result<Vec<(PathBuf, String, String)>, String> {
    let candidates = game_pak_candidates(game_root)?;

    let mut by_full: HashMap<String, (PathBuf, String)> = HashMap::new();
    for pak_path in &candidates {
        let pak = match open_pak(pak_path) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let mount = pak.mount_point().to_string();
        for key in pak.files() {
            if key.to_ascii_lowercase().ends_with(VO_BANK_MATCH) {
                let full = full_game_path(&mount, &key);
                by_full.insert(full, (pak_path.clone(), key));
            }
        }
    }

    Ok(by_full
        .into_iter()
        .map(|(full, (pak, key))| (pak, key, full))
        .collect())
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

/// Per-slot sound input: source audio path, optional gain in decibels, and whether to strip the
/// sound's baked LPF/HPF so the custom audio plays unfiltered.
#[derive(serde::Deserialize)]
pub(crate) struct SoundInput {
    path: String,
    #[serde(default)]
    gain_db: f32,
    #[serde(default)]
    remove_filtering: bool,
}

/// Source ids of the slots in `bank` whose provided input requested baked-filter removal.
fn ids_to_unfilter(wavs: &HashMap<String, SoundInput>, bank: SoundBank) -> HashSet<u32> {
    SOUND_SLOTS
        .iter()
        .filter(|s| s.bank == bank)
        .filter(|s| wavs.get(s.key).is_some_and(|i| i.remove_filtering))
        .map(|s| s.wem_id)
        .collect()
}

fn build_sound_pak(
    game_root: &str,
    wavs: &HashMap<String, SoundInput>,
    output_pak: &str,
) -> Result<String, String> {
    if wavs.is_empty() {
        return Err("At least one sound must be provided".into());
    }

    // Encode each provided slot's WAV -> PCM WEM in parallel, keeping the slot for routing.
    let pending: Vec<(&'static SoundSlot, &SoundInput)> = SOUND_SLOTS
        .iter()
        .filter_map(|slot| wavs.get(slot.key).map(|input| (slot, input)))
        .collect();

    let converted: Vec<(&'static SoundSlot, Vec<u8>)> = concurrency::POOL.install(|| {
        pending
            .par_iter()
            .map(|(slot, input)| {
                let (wem_bytes, _) =
                    audio::convert_to_bytes_with_gain(Path::new(&input.path), input.gain_db)
                        .map_err(|e| format!("{} WAV conversion failed: {e}", slot.label))?;
                Ok((*slot, wem_bytes))
            })
            .collect::<Result<Vec<_>, String>>()
    })?;

    // Route each encoded WEM to its bank's replacement set.
    let mut ui_repl: HashMap<u32, Vec<u8>> = HashMap::new();
    let mut vo_repl: HashMap<u32, Vec<u8>> = HashMap::new();
    let mut summary_parts: Vec<String> = Vec::new();
    for (slot, bytes) in converted {
        match slot.bank {
            SoundBank::UiBattle => {
                ui_repl.insert(slot.wem_id, bytes);
            }
            SoundBank::VoSystem => {
                vo_repl.insert(slot.wem_id, bytes);
            }
        }
        summary_parts.push(slot.label.to_string());
    }

    let mut outputs: Vec<(String, Vec<u8>)> = Vec::new();
    if !ui_repl.is_empty() {
        outputs.push(patch_ui_battle(game_root, wavs, ui_repl)?);
    }
    if !vo_repl.is_empty() {
        outputs.extend(patch_vo_banks(game_root, wavs, &vo_repl)?);
    }
    if outputs.is_empty() {
        return Err("Nothing to build.".into());
    }

    pak::write_pak_bytes(output_pak, outputs)?;

    let summary = summary_parts.join(" + ");
    Ok(format!("Sound mod created with {summary} sound(s)"))
}

/// Patch the single `bnk_ui_battle.bnk` with the in-memory SFX replacements, returning the
/// `(entry_path, bytes)` to write into the mod pak.
fn patch_ui_battle(
    game_root: &str,
    wavs: &HashMap<String, SoundInput>,
    mut replacements: HashMap<u32, Vec<u8>>,
) -> Result<(String, Vec<u8>), String> {
    let bnk_bytes = get_or_extract_bnk(game_root)?;
    if bnk_bytes.len() < 8 || &bnk_bytes[0..4] != b"BKHD" {
        return Err("Extracted BNK appears corrupt or has unexpected header".to_string());
    }
    let mut bnk = rebnk::parse_bnk_from_bytes(&bnk_bytes, Path::new(BNK_ENTRY_NAME))
        .map_err(|e| format!("Failed to parse BNK: {e}"))?;
    let bnk_ids: HashSet<u32> = bnk.wems.iter().map(|w| w.id).collect();

    for slot in SOUND_SLOTS
        .iter()
        .filter(|s| matches!(s.bank, SoundBank::UiBattle))
    {
        if replacements.contains_key(&slot.wem_id) && !bnk_ids.contains(&slot.wem_id) {
            return Err(format!(
                "WEM ID {} ({}) not found in BNK; the game may have been updated",
                slot.wem_id, slot.label
            ));
        }
    }

    for &(companion_id, trigger_key) in SILENCE_COMPANIONS {
        if wavs.contains_key(trigger_key)
            && !replacements.contains_key(&companion_id)
            && bnk_ids.contains(&companion_id)
        {
            replacements.insert(companion_id, silence_wem());
        }
    }

    let unfilter = ids_to_unfilter(wavs, SoundBank::UiBattle);
    if !unfilter.is_empty() {
        strip_filtering(&mut bnk, &unfilter, &UI_BATTLE_BYPASS);
    }

    let patched = rebnk::pack_to_bytes(&bnk, &replacements)
        .map_err(|e| format!("Failed to repack BNK: {e}"))?;
    Ok((BNK_ENTRY_NAME.to_string(), patched))
}

/// Patch every per-language `bnk_vo_system.bnk` with the announcer replacements so the mod
/// works regardless of the player's game language. A language bank only gets the callout ids it
/// actually carries.
fn patch_vo_banks(
    game_root: &str,
    wavs: &HashMap<String, SoundInput>,
    replacements: &HashMap<u32, Vec<u8>>,
) -> Result<Vec<(String, Vec<u8>)>, String> {
    let banks = find_vo_banks(game_root)?;
    if banks.is_empty() {
        return Err(format!("No {VO_BANK_MATCH} found in the game paks."));
    }

    let unfilter = ids_to_unfilter(wavs, SoundBank::VoSystem);

    let mut outputs = Vec::new();
    for (pak_path, key, full) in &banks {
        let bytes = read_bnk_from_pak(pak_path, key)?;
        if bytes.len() < 8 || &bytes[0..4] != b"BKHD" {
            continue;
        }
        let mut bnk = rebnk::parse_bnk_from_bytes(&bytes, Path::new(key))
            .map_err(|e| format!("Failed to parse {full}: {e}"))?;
        let ids: HashSet<u32> = bnk.wems.iter().map(|w| w.id).collect();
        let repl: HashMap<u32, Vec<u8>> = replacements
            .iter()
            .filter(|(id, _)| ids.contains(id))
            .map(|(id, b)| (*id, b.clone()))
            .collect();
        if repl.is_empty() {
            continue;
        }
        if !unfilter.is_empty() {
            strip_filtering(&mut bnk, &unfilter, &VO_SYSTEM_BYPASS);
        }
        let patched = rebnk::pack_to_bytes(&bnk, &repl)
            .map_err(|e| format!("Failed to repack {full}: {e}"))?;
        outputs.push((full.clone(), patched));
    }

    if outputs.is_empty() {
        return Err(format!(
            "The announcer WEM IDs were not found in any {VO_BANK_MATCH}; the game may have been updated."
        ));
    }
    Ok(outputs)
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
    vgmstream: Option<&Path>,
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

        let wav_bytes = crate::audio::wem_to_wav(&wem.data, vgmstream)
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

fn extract_sound_pak(
    game_root: &str,
    pak_path: &str,
    output_dir: &str,
    vgmstream: Option<&Path>,
) -> Result<String, String> {
    let result = extract_sound_pak_core(
        game_root,
        Path::new(pak_path),
        Path::new(output_dir),
        vgmstream,
    )?;
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

fn load_sound_mod_for_edit_impl(
    game_root: &str,
    pak_path: &str,
    vgmstream: Option<&Path>,
) -> Result<LoadedSoundMod, String> {
    let cache_root = hitsound_edit_cache_dir()?;
    if cache_root.exists() {
        fs::remove_dir_all(&cache_root).map_err(|e| format!("clear hitsound-edit cache: {e}"))?;
    }
    fs::create_dir_all(&cache_root).map_err(|e| format!("create hitsound-edit cache: {e}"))?;

    let extracted = extract_sound_pak_core(game_root, Path::new(pak_path), &cache_root, vgmstream)?;
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
    state: State<'_, SettingsState>,
    game_root: String,
    pak_path: String,
    output_dir: String,
) -> Result<String, String> {
    let configured = state.lock().ok().and_then(|s| s.vgmstream_path.clone());
    tauri::async_runtime::spawn_blocking(move || {
        let vgm = crate::audio::vgmstream::resolve(configured.as_deref());
        extract_sound_pak(&game_root, &pak_path, &output_dir, vgm.as_deref())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn load_sound_mod_for_edit(
    state: State<'_, SettingsState>,
    game_root: String,
    pak_path: String,
) -> Result<LoadedSoundMod, String> {
    let configured = state.lock().ok().and_then(|s| s.vgmstream_path.clone());
    tauri::async_runtime::spawn_blocking(move || {
        let vgm = crate::audio::vgmstream::resolve(configured.as_deref());
        load_sound_mod_for_edit_impl(&game_root, &pak_path, vgm.as_deref())
    })
    .await
    .map_err(|e| e.to_string())?
}

static BNK_EXTRACT_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Wwise short id of a loose `<id>.wem` entry. `None` for non-`.wem` entries or names whose
/// stem is not a plain number, so descriptively-named files never match a bank WEM by accident.
fn parse_wem_id(entry: &str) -> Option<u32> {
    let path = Path::new(entry);
    if !path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("wem"))
    {
        return None;
    }
    path.file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| s.parse::<u32>().ok())
}

/// A loose streamed file replaces the bank's embedded copy only when it is genuinely longer,
/// i.e. the embedded copy was a prefetch stub (a truncated head of the full sound).
fn prefer_loose(loose_size: u64, embedded_size: usize) -> bool {
    loose_size > embedded_size as u64
}

/// Read a single entry from a pak/utoc into memory via a managed temp file.
fn read_container_entry(source: &str, entry: &str, is_utoc: bool) -> Result<Vec<u8>, String> {
    let tmp = std::env::temp_dir().join(format!(
        "rivals_entry_{}_{}.bin",
        std::process::id(),
        BNK_EXTRACT_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let tmp_str = tmp.to_string_lossy().into_owned();
    let extracted = if is_utoc {
        crate::pak::extract_utoc_file(source, entry, &tmp_str)
    } else {
        crate::pak::extract_single_file(source, entry, &tmp_str)
    };
    if let Err(e) = extracted {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    let bytes = fs::read(&tmp).map_err(|e| e.to_string());
    let _ = fs::remove_file(&tmp);
    bytes
}

/// For each `wanted` bank WEM id, find the loose `<id>.wem` in the same container (the full
/// streamed version) and extract it into `dest`, returning `id -> on-disk path`. Best-effort:
/// any failure yields an empty map so the caller transparently falls back to embedded bytes.
fn extract_full_loose_wems(
    source: &str,
    is_utoc: bool,
    wanted: &HashSet<u32>,
    dest: &Path,
) -> HashMap<u32, PathBuf> {
    let listing = if is_utoc {
        crate::pak::list_utoc_contents(source)
    } else {
        crate::pak::list_pak_contents(source)
    };
    let Ok(entries) = listing else {
        return HashMap::new();
    };

    let to_extract: Vec<String> = entries
        .into_iter()
        .filter(|e| parse_wem_id(e).is_some_and(|id| wanted.contains(&id)))
        .collect();
    if to_extract.is_empty() || fs::create_dir_all(dest).is_err() {
        return HashMap::new();
    }

    let dest_str = dest.to_string_lossy();
    let extracted = if is_utoc {
        crate::pak::extract_utoc_files(source, &to_extract, &dest_str)
    } else {
        crate::pak::extract_pak_files(source, &to_extract, &dest_str)
    };
    if extracted.is_err() {
        return HashMap::new();
    }

    // Map extracted files back to ids by numeric stem, robust to whatever folder structure the
    // extractor preserved under `dest`.
    let mut map = HashMap::new();
    for file in walkdir::WalkDir::new(dest)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        if let Some(id) = file.path().to_str().and_then(parse_wem_id)
            && wanted.contains(&id)
        {
            map.insert(id, file.path().to_path_buf());
        }
    }
    map
}

enum Outcome {
    Full,
    Embedded,
    Failed,
}

/// Decode every WEM a `.bnk` entry references to `<output_dir>/<bank>/<wem_id>.wav`.
///
/// Wwise embeds only a prefetch stub (truncated head) for streamed sounds; the full audio lives
/// in a loose `<id>.wem` beside the bank. For each embedded WEM, we prefer that loose full file
/// when it exists and is longer, falling back to the embedded bytes otherwise. Works for any
/// bank; banks whose audio is purely streamed carry no embedded media and report nothing.
fn extract_bnk_all(
    source: &str,
    entry: &str,
    is_utoc: bool,
    output_dir: &str,
    vgmstream: Option<&Path>,
) -> Result<String, String> {
    let bytes = read_container_entry(source, entry, is_utoc)?;
    if bytes.len() < 4 || &bytes[0..4] != b"BKHD" {
        return Err("That file is not a Wwise bank (missing BKHD header).".to_string());
    }

    let bnk = rebnk::parse_bnk_from_bytes(&bytes, Path::new(entry))
        .map_err(|e| format!("Failed to parse BNK: {e}"))?;
    if bnk.wems.is_empty() {
        return Err(
            "This bank has no embedded WEMs (its audio is likely streamed as loose .wem files)."
                .to_string(),
        );
    }

    let stem = Path::new(entry)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("bank");
    let out_dir = Path::new(output_dir).join(stem);
    fs::create_dir_all(&out_dir).map_err(|e| format!("Failed to create output directory: {e}"))?;

    let wanted: HashSet<u32> = bnk.wems.iter().map(|w| w.id).collect();
    let loose_dir = std::env::temp_dir().join(format!(
        "rivals_loose_{}_{}",
        std::process::id(),
        BNK_EXTRACT_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let id_to_loose = extract_full_loose_wems(source, is_utoc, &wanted, &loose_dir);

    let results: Vec<Outcome> = concurrency::POOL.install(|| {
        bnk.wems
            .par_iter()
            .map(|wem| {
                let loose = id_to_loose.get(&wem.id).filter(|p| {
                    fs::metadata(p)
                        .map(|m| prefer_loose(m.len(), wem.data.len()))
                        .unwrap_or(false)
                });
                let used_full = loose.is_some();
                let decoded = match loose {
                    Some(p) => fs::read(p).map_err(|e| e.to_string()).and_then(|b| {
                        crate::audio::wem_to_wav(&b, vgmstream).map_err(|e| e.to_string())
                    }),
                    None => {
                        crate::audio::wem_to_wav(&wem.data, vgmstream).map_err(|e| e.to_string())
                    }
                };
                let written = match decoded {
                    Ok(wav) => fs::write(out_dir.join(format!("{}.wav", wem.id)), wav).is_ok(),
                    Err(_) => false,
                };
                match (written, used_full) {
                    (false, _) => Outcome::Failed,
                    (true, true) => Outcome::Full,
                    (true, false) => Outcome::Embedded,
                }
            })
            .collect()
    });

    let _ = fs::remove_dir_all(&loose_dir);

    let full = results
        .iter()
        .filter(|o| matches!(o, Outcome::Full))
        .count();
    let embedded = results
        .iter()
        .filter(|o| matches!(o, Outcome::Embedded))
        .count();
    let failed = results
        .iter()
        .filter(|o| matches!(o, Outcome::Failed))
        .count();
    let ok = full + embedded;
    if ok == 0 {
        return Err(format!(
            "Could not decode any of the {} WEM(s); they may use a codec your vgmstream build does not support.",
            results.len()
        ));
    }

    let mut notes = Vec::new();
    if full > 0 {
        notes.push(format!("{full} full streamed"));
    }
    if embedded > 0 {
        notes.push(format!("{embedded} embedded"));
    }
    if failed > 0 {
        notes.push(format!("{failed} failed"));
    }
    let mut msg = format!("Extracted {ok} WEM(s) to {}", out_dir.display());
    if !notes.is_empty() {
        msg.push_str(&format!(" ({})", notes.join(", ")));
    }
    Ok(msg)
}

#[tauri::command]
pub(crate) async fn extract_bnk_wems_as_wav(
    state: State<'_, SettingsState>,
    source_path: String,
    entry: String,
    is_utoc: bool,
    output_dir: String,
) -> Result<String, String> {
    let configured = state.lock().ok().and_then(|s| s.vgmstream_path.clone());
    tauri::async_runtime::spawn_blocking(move || {
        let vgm = crate::audio::vgmstream::resolve(configured.as_deref());
        extract_bnk_all(&source_path, &entry, is_utoc, &output_dir, vgm.as_deref())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::{full_game_path, parse_wem_id, prefer_loose, sanitize_mod_name};

    #[test]
    fn full_game_path_composes_mount_and_key() {
        // Root-mounted pak: the key is already the full game-relative path.
        assert_eq!(
            full_game_path("../../../", "Marvel/Content/WwiseAudio/bnk_ui_battle.bnk"),
            "Marvel/Content/WwiseAudio/bnk_ui_battle.bnk"
        );
        // VO pak mounted at WwiseAudio: prepend the mount so the loader matches.
        assert_eq!(
            full_game_path(
                "../../../Marvel/Content/WwiseAudio/",
                "English (US)/bnk_vo_system.bnk"
            ),
            "Marvel/Content/WwiseAudio/English (US)/bnk_vo_system.bnk"
        );
    }

    #[test]
    fn parse_wem_id_reads_numeric_stem() {
        assert_eq!(
            parse_wem_id("WwiseAudio/Media/123456789.wem"),
            Some(123456789)
        );
        assert_eq!(parse_wem_id("123.WEM"), Some(123));
    }

    #[test]
    fn parse_wem_id_rejects_non_wem_and_non_numeric() {
        assert_eq!(parse_wem_id("WwiseAudio/Media/bnk_ui_battle.bnk"), None);
        assert_eq!(parse_wem_id("Footstep_Concrete.wem"), None);
        assert_eq!(parse_wem_id("123456789.wav"), None);
        assert_eq!(parse_wem_id("12_34.wem"), None);
    }

    #[test]
    fn prefer_loose_only_when_strictly_larger() {
        assert!(prefer_loose(2000, 500)); // loose full vs prefetch stub
        assert!(!prefer_loose(500, 500)); // same size: keep embedded
        assert!(!prefer_loose(400, 500)); // smaller: keep embedded
    }

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
