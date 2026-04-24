//! Detect which heroes/skins a mod targets by scanning pak/utoc entries against the bundled character catalogue.

#![allow(clippy::redundant_pub_crate)]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use tauri::{AppHandle, Manager};

use crate::concurrency;
use crate::pak;
use crate::paths;
use crate::settings::{ModHeroCacheEntry, Settings, SettingsState};

use super::status::{ModEntry, ModsStatus};

const RAW_CATALOGUE: &str = include_str!("../../data/character_ids.json");

#[derive(Clone, Deserialize)]
pub(crate) struct RawCatalogue {
    #[serde(default)]
    pub(crate) generated_at: Option<String>,
    #[serde(default)]
    pub(crate) source: Option<String>,
    #[serde(default)]
    pub(crate) characters: HashMap<String, RawCharacter>,
}

#[derive(Clone, Deserialize)]
pub(crate) struct RawCharacter {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) skins: HashMap<String, String>,
}

#[derive(Clone, Debug)]
pub(crate) struct Character {
    pub name: String,
    pub skins: HashMap<u32, String>,
}

#[derive(Clone, Debug)]
pub(crate) struct CatalogueData {
    pub characters: HashMap<u32, Character>,
    pub generated_at: Option<String>,
    pub source: Option<String>,
    /// `"bundled"` or the disk path the data came from.
    pub origin: String,
}

static CATALOGUE: RwLock<Option<Arc<CatalogueData>>> = RwLock::new(None);

pub(crate) fn user_catalogue_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("rivals-toolkit").join("character_ids.json"))
}

/// Disk override takes precedence; bundled JSON is the fallback.
fn load_source() -> (String, String) {
    if let Some(path) = user_catalogue_path()
        && let Ok(s) = std::fs::read_to_string(&path)
        && serde_json::from_str::<RawCatalogue>(&s).is_ok()
    {
        return (s, path.display().to_string());
    }
    (RAW_CATALOGUE.to_string(), "bundled".to_string())
}

fn parse_catalogue(raw: &str, origin: String) -> CatalogueData {
    match serde_json::from_str::<RawCatalogue>(raw) {
        Ok(parsed) => {
            let characters = parsed
                .characters
                .into_iter()
                .filter_map(|(id_str, raw)| {
                    let id = id_str.parse::<u32>().ok()?;
                    let skins = raw
                        .skins
                        .into_iter()
                        .filter_map(|(sid, name)| sid.parse::<u32>().ok().map(|sid| (sid, name)))
                        .collect();
                    Some((
                        id,
                        Character {
                            name: raw.name,
                            skins,
                        },
                    ))
                })
                .collect();
            CatalogueData {
                characters,
                generated_at: parsed.generated_at,
                source: parsed.source,
                origin,
            }
        }
        Err(e) => {
            eprintln!("rivals-toolkit: failed to parse character_ids.json ({origin}): {e}");
            CatalogueData {
                characters: HashMap::new(),
                generated_at: None,
                source: None,
                origin,
            }
        }
    }
}

fn read_cached() -> Option<Arc<CatalogueData>> {
    CATALOGUE
        .read()
        .ok()
        .and_then(|guard| guard.as_ref().cloned())
}

pub(crate) fn catalogue_data() -> Arc<CatalogueData> {
    if let Some(cached) = read_cached() {
        return cached;
    }
    let (raw, origin) = load_source();
    let parsed = Arc::new(parse_catalogue(&raw, origin));
    if let Ok(mut guard) = CATALOGUE.write() {
        if let Some(existing) = guard.as_ref() {
            return existing.clone();
        }
        *guard = Some(parsed.clone());
    }
    parsed
}

pub(crate) fn reload_catalogue() {
    let (raw, origin) = load_source();
    let parsed = Arc::new(parse_catalogue(&raw, origin));
    if let Ok(mut guard) = CATALOGUE.write() {
        *guard = Some(parsed);
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct HeroMatch {
    pub character_id: u32,
    pub character_name: String,
    pub skin_ids: Vec<u32>,
    pub skin_names: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct CharacterSummary {
    pub id: u32,
    pub name: String,
}

pub(crate) fn list_known_characters() -> Vec<CharacterSummary> {
    let cat = catalogue_data();
    let mut out: Vec<CharacterSummary> = cat
        .characters
        .iter()
        .map(|(id, c)| CharacterSummary {
            id: *id,
            name: c.name.clone(),
        })
        .collect();
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

/// Two-rule detector applied per asset path:
/// 1. Anchor-then-id: `.../Characters/<HEROID>/[<SKINID>/...]`
///    or `.../AbilitySystem/<HEROID>/...`. HEROID = 4 digits, SKINID = 7 digits.
/// 2. Embedded 7-digit skin token: any segment contains a 7-digit run between
///    non-digit boundaries. Catches `bnk_vo_<SKINID>.bnk`, `<SKINID>` segments,
///    `Materials/<SKINID>/Mat.uasset`, etc. Char id = skin / 1000.
///
/// Skips paths under `/environment/` (map decoration referencing characters).
fn detect_in_path(path: &str) -> Vec<(u32, Option<u32>)> {
    let normalized = path.replace('\\', "/").to_ascii_lowercase();
    if normalized.contains("/environment/") {
        return Vec::new();
    }

    let segments: Vec<&str> = normalized.split('/').collect();
    let mut hits: Vec<(u32, Option<u32>)> = Vec::new();

    for (i, seg) in segments.iter().enumerate() {
        if (*seg == "characters" || *seg == "abilitysystem")
            && let Some(char_id) = segments.get(i + 1).and_then(|s| parse_digits(s, 4))
        {
            let skin_id = segments.get(i + 2).and_then(|s| parse_digits(s, 7));
            hits.push((char_id, skin_id));
        }
    }

    for seg in &segments {
        for tok in seg.split(|c: char| !c.is_ascii_digit()) {
            if let Some(skin_id) = parse_digits(tok, 7) {
                hits.push((skin_id / 1000, Some(skin_id)));
            }
        }
    }

    hits
}

fn parse_digits(s: &str, len: usize) -> Option<u32> {
    if s.len() == len && s.bytes().all(|b| b.is_ascii_digit()) {
        s.parse().ok()
    } else {
        None
    }
}

pub(crate) fn detect_heroes_from_paths<I, S>(paths: I) -> Vec<HeroMatch>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let cat = catalogue_data();
    let mut by_char: HashMap<u32, HashSet<u32>> = HashMap::new();
    for path in paths {
        for (char_id, skin_id) in detect_in_path(path.as_ref()) {
            // Drop hits whose char id isn't a known hero. Suppresses random
            // 7-digit tokens (UE asset hashes, etc.) that happen to look like skin ids.
            if !cat.characters.contains_key(&char_id) {
                continue;
            }
            let entry = by_char.entry(char_id).or_default();
            if let Some(sid) = skin_id {
                entry.insert(sid);
            }
        }
    }

    let mut out: Vec<HeroMatch> = by_char
        .into_iter()
        .filter_map(|(char_id, skins)| {
            let c = cat.characters.get(&char_id)?;
            let mut skin_ids: Vec<u32> = skins.into_iter().collect();
            skin_ids.sort_unstable();
            let skin_names = skin_ids
                .iter()
                .map(|sid| {
                    c.skins
                        .get(sid)
                        .cloned()
                        .unwrap_or_else(|| format!("Skin {sid}"))
                })
                .collect();
            Some(HeroMatch {
                character_id: char_id,
                character_name: c.name.clone(),
                skin_ids,
                skin_names,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        a.character_name
            .to_lowercase()
            .cmp(&b.character_name.to_lowercase())
    });
    out
}

fn strip_pak_suffix(name: &str) -> &str {
    name.strip_suffix(".pak.disabled")
        .or_else(|| name.strip_suffix(".pak"))
        .unwrap_or(name)
}

/// Outcome of attempting to detect heroes for a single mod. `Heroes` (including
/// an empty Vec for legitimate non-character mods) means every read succeeded
/// and the result is safe to cache. `Failed` means at least one container read
/// errored and the caller should not cache the result.
pub(crate) enum DetectionOutcome {
    Heroes(Vec<HeroMatch>),
    Failed,
}

/// Detect heroes for a single mod by scanning its pak (and utoc, if present).
/// Returns `Failed` when any present container could not be read so the caller
/// can skip caching and recompute on a later attempt.
pub(crate) fn detect_heroes_for_mod(
    mods_folder: &Path,
    full_name: &str,
) -> Result<DetectionOutcome, String> {
    let pak_path = mods_folder.join(full_name);
    if !pak_path.exists() {
        return Err(format!("Mod file not found: {}", pak_path.display()));
    }

    let mut paths: Vec<String> = Vec::new();
    let mut any_failed = false;

    match pak::list_pak_contents(&pak_path.to_string_lossy()) {
        Ok(files) => paths.extend(files),
        Err(e) => {
            eprintln!(
                "rivals-toolkit: pak read failed for {}: {e}",
                pak_path.display()
            );
            any_failed = true;
        }
    }

    let stem = strip_pak_suffix(full_name);
    let parent = Path::new(full_name)
        .parent()
        .unwrap_or_else(|| Path::new(""));
    let stem_file = Path::new(stem)
        .file_name()
        .map_or_else(|| stem.to_string(), |f| f.to_string_lossy().into_owned());
    let dir = mods_folder.join(parent);
    let utoc_enabled = dir.join(format!("{stem_file}.utoc"));
    let utoc_disabled = dir.join(format!("{stem_file}.utoc.disabled"));
    let utoc_path = if utoc_enabled.exists() {
        Some(utoc_enabled)
    } else if utoc_disabled.exists() {
        Some(utoc_disabled)
    } else {
        None
    };
    if let Some(p) = utoc_path {
        match pak::list_utoc_contents(&p.to_string_lossy()) {
            Ok(files) => paths.extend(files),
            Err(e) => {
                eprintln!("rivals-toolkit: utoc read failed for {}: {e}", p.display());
                any_failed = true;
            }
        }
    }

    if any_failed {
        Ok(DetectionOutcome::Failed)
    } else {
        Ok(DetectionOutcome::Heroes(detect_heroes_from_paths(paths)))
    }
}

/// Populate `status.mod_entries[*].heroes` using the cache, recomputing for any
/// mod whose cached size doesn't match the current size (or that's missing from
/// the cache). Persists the cache when changes occur.
pub(crate) fn enrich_status_with_heroes(state: &Mutex<Settings>, status: &mut ModsStatus) {
    if status.mod_entries.is_empty() {
        return;
    }
    let mods_folder = PathBuf::from(&status.mods_folder_path);

    // First pass: serve from cache where possible; collect entries needing recompute.
    // Snapshot sync timestamp so we can detect a sync that races with our pak I/O
    // and so cache hits re-detect when the catalogue has moved on.
    let (needs_compute, sync_stamp_at_start): (Vec<(usize, String, String, u64)>, u64) = {
        let Ok(guard) = state.lock() else {
            eprintln!("rivals-toolkit: settings lock poisoned, skipping hero enrichment");
            return;
        };
        let current_stamp = guard.last_character_data_sync;
        let mut work = Vec::new();
        for (idx, entry) in status.mod_entries.iter_mut().enumerate() {
            if let Some(cached) = guard.mod_hero_cache.get(&entry.display_name)
                && cached.size_bytes == entry.size_bytes
                && cached.catalogue_stamp == current_stamp
            {
                entry.heroes = cached.heroes.clone();
                continue;
            }
            work.push((
                idx,
                entry.full_name.clone(),
                entry.display_name.clone(),
                entry.size_bytes,
            ));
        }
        (work, current_stamp)
    };

    if needs_compute.is_empty() {
        prune_cache(state, &status.mod_entries);
        return;
    }

    // Second pass: parallel detect for misses, scoped to half the cores so the
    // background scan doesn't starve the rest of the app on app startup.
    let folder_ref = mods_folder.as_path();
    let computed: Vec<(usize, String, u64, DetectionOutcome)> = concurrency::POOL.install(|| {
        needs_compute
            .par_iter()
            .map(|(idx, full_name, display_name, size)| {
                let outcome = detect_heroes_for_mod(folder_ref, full_name)
                    .unwrap_or(DetectionOutcome::Failed);
                (*idx, display_name.clone(), *size, outcome)
            })
            .collect()
    });

    {
        let Ok(mut guard) = state.lock() else {
            eprintln!("rivals-toolkit: settings lock poisoned during hero enrichment write");
            return;
        };
        // If a sync committed between snapshot and now, our `computed` results were
        // produced against the old catalogue. Drop them; the post-sync refresh
        // will re-detect against the new catalogue.
        if guard.last_character_data_sync != sync_stamp_at_start {
            return;
        }
        for (idx, display_name, size, outcome) in computed {
            match outcome {
                DetectionOutcome::Heroes(heroes) => {
                    if let Some(entry) = status.mod_entries.get_mut(idx) {
                        entry.heroes.clone_from(&heroes);
                    }
                    guard.mod_hero_cache.insert(
                        display_name,
                        ModHeroCacheEntry {
                            size_bytes: size,
                            catalogue_stamp: sync_stamp_at_start,
                            heroes,
                        },
                    );
                }
                DetectionOutcome::Failed => {
                    if let Some(entry) = status.mod_entries.get_mut(idx) {
                        entry.heroes.clear();
                    }
                }
            }
        }
        let live: HashSet<&str> = status
            .mod_entries
            .iter()
            .map(|e| e.display_name.as_str())
            .collect();
        guard
            .mod_hero_cache
            .retain(|k, _| live.contains(k.as_str()));
        if let Err(e) = guard.save() {
            eprintln!("rivals-toolkit: failed to persist mod hero cache: {e}");
        }
    }
}

fn prune_cache(state: &Mutex<Settings>, entries: &[ModEntry]) {
    let Ok(mut guard) = state.lock() else {
        return;
    };
    let live: HashSet<&str> = entries.iter().map(|e| e.display_name.as_str()).collect();
    let before = guard.mod_hero_cache.len();
    guard
        .mod_hero_cache
        .retain(|k, _| live.contains(k.as_str()));
    if guard.mod_hero_cache.len() != before
        && let Err(e) = guard.save()
    {
        eprintln!("rivals-toolkit: failed to persist pruned hero cache: {e}");
    }
}

/// Force recomputation of heroes for a single mod, bypassing the cache.
/// Derives the total on-disk size (pak + companions) at scan time via the
/// shared helper so the cached entry matches what status enrichment computes.
pub(crate) fn rescan_heroes_for_mod(
    state: &Mutex<Settings>,
    mods_folder: &Path,
    full_name: &str,
    display_name: &str,
) -> Result<Vec<HeroMatch>, String> {
    let pak_path = mods_folder.join(full_name);
    if !pak_path.exists() {
        return Err(format!("Mod file not found: {}", pak_path.display()));
    }
    // Must match the combined size reported by status enrichment so a rescan
    // result stays valid on the next get_mods_status call.
    let size_bytes = super::mod_size_on_disk(mods_folder, full_name);

    match detect_heroes_for_mod(mods_folder, full_name)? {
        DetectionOutcome::Heroes(heroes) => {
            if let Ok(mut guard) = state.lock() {
                let catalogue_stamp = guard.last_character_data_sync;
                guard.mod_hero_cache.insert(
                    display_name.to_string(),
                    ModHeroCacheEntry {
                        size_bytes,
                        catalogue_stamp,
                        heroes: heroes.clone(),
                    },
                );
                if let Err(e) = guard.save() {
                    eprintln!("rivals-toolkit: failed to persist rescan result: {e}");
                }
            }
            Ok(heroes)
        }
        DetectionOutcome::Failed => {
            // Drop any stale cached entry so the next scan retries instead of
            // serving the bad data.
            if let Ok(mut guard) = state.lock() {
                guard.mod_hero_cache.remove(display_name);
                if let Err(e) = guard.save() {
                    eprintln!(
                        "rivals-toolkit: failed to persist cleared cache after failed rescan: {e}"
                    );
                }
            }
            Err(format!(
                "Failed to read mod containers for {display_name}; cache cleared, will retry"
            ))
        }
    }
}

#[tauri::command]
pub(crate) fn list_known_heroes() -> Vec<CharacterSummary> {
    list_known_characters()
}

#[tauri::command]
pub(crate) async fn rescan_mod_heroes(
    app: AppHandle,
    game_root: String,
    full_name: String,
    display_name: String,
) -> Result<Vec<HeroMatch>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<SettingsState>();
        let mods_folder = paths::mods_dir(&game_root);
        rescan_heroes_for_mod(&state, &mods_folder, &full_name, &display_name)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_characters_anchor_with_skin() {
        let hits = detect_in_path("Marvel/Content/Marvel/Characters/1054/1054001/Mesh.uasset");
        assert!(hits.contains(&(1054, Some(1_054_001))));
    }

    #[test]
    fn matches_characters_anchor_without_skin() {
        let hits = detect_in_path("Marvel/Content/Marvel/Characters/1011/Shared/foo.uasset");
        assert!(hits.contains(&(1011, None)));
    }

    #[test]
    fn matches_abilitysystem_anchor() {
        let hits = detect_in_path("Marvel/Content/Marvel/AbilitySystem/1033/Skills/Foo.uasset");
        assert!(hits.contains(&(1033, None)));
    }

    #[test]
    fn matches_embedded_skin_id_in_filename() {
        let hits = detect_in_path("WwiseAudio/Media/bnk_vo_1054001.bnk");
        assert!(hits.contains(&(1054, Some(1_054_001))));
    }

    #[test]
    fn matches_embedded_skin_id_in_split_path() {
        let hits = detect_in_path(
            "Marvel/Content/Marvel/VFX/Materials/Characters/1033/Materials/1033502/Mat.uasset",
        );
        assert!(hits.contains(&(1033, Some(1_033_502))));
    }

    #[test]
    fn rejects_environment_paths() {
        let hits = detect_in_path(
            "Marvel/Content/Marvel/Environment/IPAsset/Characters/1026/1026300/Mesh.uasset",
        );
        assert!(hits.is_empty());
    }

    #[test]
    fn handles_backslash_separators() {
        let hits = detect_in_path("Marvel\\Content\\Marvel\\Characters\\1011\\1011502\\foo.uasset");
        assert!(hits.contains(&(1011, Some(1_011_502))));
    }

    #[test]
    fn aggregates_skins_per_char() {
        let paths = vec![
            "Marvel/Content/Marvel/Characters/1054/1054001/A.uasset".to_string(),
            "Marvel/Content/Marvel/Characters/1054/1054300/B.uasset".to_string(),
        ];
        let matches = detect_heroes_from_paths(paths);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].character_id, 1054);
        assert_eq!(matches[0].skin_ids, vec![1_054_001, 1_054_300]);
    }

    #[test]
    fn unknown_char_id_dropped() {
        let paths = vec!["Marvel/Content/Marvel/Characters/9000/9000100/x.uasset".to_string()];
        assert!(detect_heroes_from_paths(paths).is_empty());
    }

    #[test]
    fn random_7digit_token_dropped_when_not_in_catalogue() {
        // 2103312 derives char 2103, not a real hero. Must not surface as a match.
        let paths = vec!["Marvel/Content/Marvel/Some/Asset/2103312_thing.uasset".to_string()];
        assert!(detect_heroes_from_paths(paths).is_empty());
    }
}
