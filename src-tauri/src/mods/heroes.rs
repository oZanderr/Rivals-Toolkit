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

use super::hero_cache::{HeroCache, HeroCacheState, ModHeroCacheEntry};
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
    out.sort_by_key(|a| a.name.to_lowercase());
    out
}

/// Three-rule detector applied per asset path:
/// 1. Anchor-then-id: `.../Characters/<HEROID>/[<SKINID>/...]`
///    or `.../AbilitySystem/<HEROID>/...`. HEROID = 4 digits, SKINID = 7 digits.
/// 2. Embedded 7-digit skin token: any segment contains a 7-digit run between
///    non-digit boundaries. Catches `bnk_vo_<SKINID>.bnk`, `<SKINID>` segments,
///    `Materials/<SKINID>/Mat.uasset`, etc. Char id = skin / 1000.
/// 3. UI textures (`/UI/Textures/`): if any path segment is exactly a 4-digit
///    catalogue char id (e.g. `Mastery/Reduce/1011/...`), that segment pins
///    ownership and the filename is not scanned (mastery filenames reference
///    other heroes by id but belong to the folder's hero). Otherwise slide a
///    4-digit window through every digit token, taking the FIRST window that
///    matches the catalogue per token, so a single id like `1055103205` resolves
///    to one hero (1055) instead of also matching incidental substrings (1032 at
///    position 4). Multi-hero filenames use the literal `and` separator, which
///    splits the digit run into independent tokens (`10515040and10558000` →
///    `10515040` and `10558000` scanned separately).
///
/// Under `/UI/Textures/`, certain non-character subdirs (`Career`, `Depot`) embed
/// digit tokens that look like char/skin ids but aren't hero attribution
/// (achievement tiers, store themes). Both rules 2 and 3 are suppressed for
/// those paths.
///
/// Skips paths under `/environment/` (map decoration referencing characters).
fn detect_in_path(path: &str, known_chars: &HashSet<u32>) -> Vec<(u32, Option<u32>)> {
    let normalized = path.replace('\\', "/").to_ascii_lowercase();
    if normalized.contains("/environment/") {
        return Vec::new();
    }

    let segments: Vec<&str> = normalized.split('/').collect();
    let mut hits: Vec<(u32, Option<u32>)> = Vec::new();
    let mut anchored_chars: HashSet<u32> = HashSet::new();

    let in_ui_textures = normalized.contains("/ui/textures/");
    let ui_denylisted = in_ui_textures && segments.iter().any(|s| matches!(*s, "career" | "depot"));

    for (i, seg) in segments.iter().enumerate() {
        if (*seg == "characters" || *seg == "abilitysystem")
            && let Some(char_id) = segments.get(i + 1).and_then(|s| parse_digits(s, 4))
        {
            let skin_id = segments.get(i + 2).and_then(|s| parse_digits(s, 7));
            hits.push((char_id, skin_id));
            anchored_chars.insert(char_id);
        }
    }

    if !ui_denylisted {
        // When path is anchored to a character (Characters/<id>/ or
        // AbilitySystem/<id>/), only accept 7-digit skin tokens that belong to
        // that character. Filenames frequently embed unrelated character ids
        // (e.g. shared VFX `NS_1034300_*` referenced from char 1036's folder).
        // Multi-hero `and` separator filenames use 8-digit tokens and are
        // handled by the UI-textures window scan below, not this rule.
        for seg in &segments {
            for tok in seg.split(|c: char| !c.is_ascii_digit()) {
                if let Some(skin_id) = parse_digits(tok, 7) {
                    let char_id = skin_id / 1000;
                    if anchored_chars.is_empty() || anchored_chars.contains(&char_id) {
                        hits.push((char_id, Some(skin_id)));
                    }
                }
            }
        }
    }

    if in_ui_textures && !ui_denylisted {
        let mut segment_anchors: Vec<u32> = Vec::new();
        for seg in &segments {
            if let Some(c) = parse_digits(seg, 4)
                && known_chars.contains(&c)
            {
                segment_anchors.push(c);
            }
        }

        if !segment_anchors.is_empty() {
            for c in segment_anchors {
                hits.push((c, None));
            }
        } else {
            for seg in &segments {
                for tok in seg.split(|c: char| !c.is_ascii_digit()) {
                    if tok.len() < 4 {
                        continue;
                    }
                    for start in 0..=tok.len() - 4 {
                        if let Some(char_id) = parse_digits(&tok[start..start + 4], 4)
                            && known_chars.contains(&char_id)
                        {
                            hits.push((char_id, None));
                            break;
                        }
                    }
                }
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
    let known_chars: HashSet<u32> = cat.characters.keys().copied().collect();
    let mut by_char: HashMap<u32, HashSet<u32>> = HashMap::new();
    for path in paths {
        for (char_id, skin_id) in detect_in_path(path.as_ref(), &known_chars) {
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
pub(crate) fn enrich_status_with_heroes(cache: &Mutex<HeroCache>, status: &mut ModsStatus) {
    if status.mod_entries.is_empty() {
        return;
    }
    let mods_folder = PathBuf::from(&status.mods_folder_path);

    // First pass: serve from cache where possible; collect entries needing recompute.
    // Snapshot sync timestamp so we can detect a sync that races with our pak I/O
    // and so cache hits re-detect when the catalogue has moved on.
    let (needs_compute, sync_stamp_at_start): (Vec<(usize, String, String, u64)>, u64) = {
        let Ok(guard) = cache.lock() else {
            eprintln!("rivals-toolkit: hero cache lock poisoned, skipping hero enrichment");
            return;
        };
        let current_stamp = guard.last_character_data_sync;
        let mut work = Vec::new();
        for (idx, entry) in status.mod_entries.iter_mut().enumerate() {
            if let Some(cached) = guard.entries.get(&entry.display_name)
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
        prune_cache(cache, &status.mod_entries);
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
        let Ok(mut guard) = cache.lock() else {
            eprintln!("rivals-toolkit: hero cache lock poisoned during hero enrichment write");
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
                    guard.entries.insert(
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
        guard.entries.retain(|k, _| live.contains(k.as_str()));
        if let Err(e) = guard.save() {
            eprintln!("rivals-toolkit: failed to persist mod hero cache: {e}");
        }
    }
}

fn prune_cache(cache: &Mutex<HeroCache>, entries: &[ModEntry]) {
    let Ok(mut guard) = cache.lock() else {
        return;
    };
    let live: HashSet<&str> = entries.iter().map(|e| e.display_name.as_str()).collect();
    let before = guard.entries.len();
    guard.entries.retain(|k, _| live.contains(k.as_str()));
    if guard.entries.len() != before
        && let Err(e) = guard.save()
    {
        eprintln!("rivals-toolkit: failed to persist pruned hero cache: {e}");
    }
}

/// Force recomputation of heroes for a single mod, bypassing the cache.
/// Derives the total on-disk size (pak + companions) at scan time via the
/// shared helper so the cached entry matches what status enrichment computes.
pub(crate) fn rescan_heroes_for_mod(
    cache: &Mutex<HeroCache>,
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
            if let Ok(mut guard) = cache.lock() {
                let catalogue_stamp = guard.last_character_data_sync;
                guard.entries.insert(
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
            if let Ok(mut guard) = cache.lock() {
                guard.entries.remove(display_name);
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
        let cache = app.state::<HeroCacheState>();
        let mods_folder = paths::mods_dir(&game_root);
        rescan_heroes_for_mod(&cache, &mods_folder, &full_name, &display_name)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known() -> HashSet<u32> {
        [
            1011, 1015, 1016, 1026, 1032, 1033, 1047, 1051, 1054, 1055, 1056, 1065,
        ]
        .into_iter()
        .collect()
    }

    #[test]
    fn matches_characters_anchor_with_skin() {
        let hits = detect_in_path(
            "Marvel/Content/Marvel/Characters/1054/1054001/Mesh.uasset",
            &known(),
        );
        assert!(hits.contains(&(1054, Some(1_054_001))));
    }

    #[test]
    fn matches_characters_anchor_without_skin() {
        let hits = detect_in_path(
            "Marvel/Content/Marvel/Characters/1011/Shared/foo.uasset",
            &known(),
        );
        assert!(hits.contains(&(1011, None)));
    }

    #[test]
    fn matches_abilitysystem_anchor() {
        let hits = detect_in_path(
            "Marvel/Content/Marvel/AbilitySystem/1033/Skills/Foo.uasset",
            &known(),
        );
        assert!(hits.contains(&(1033, None)));
    }

    #[test]
    fn matches_embedded_skin_id_in_filename() {
        let hits = detect_in_path("WwiseAudio/Media/bnk_vo_1054001.bnk", &known());
        assert!(hits.contains(&(1054, Some(1_054_001))));
    }

    #[test]
    fn matches_embedded_skin_id_in_split_path() {
        let hits = detect_in_path(
            "Marvel/Content/Marvel/VFX/Materials/Characters/1033/Materials/1033502/Mat.uasset",
            &known(),
        );
        assert!(hits.contains(&(1033, Some(1_033_502))));
    }

    #[test]
    fn anchor_suppresses_unrelated_filename_skin_token() {
        // Shared VFX folder under char 1036 references char 1034's skin id in
        // the filename. Anchor pins ownership; filename token must not leak.
        let hits = detect_in_path(
            "Marvel/Content/Marvel/VFX/Particles/Characters/1036/MVP/1036300/NS_1034300_CameraParticle_01.uasset",
            &known(),
        );
        let chars: Vec<u32> = hits.iter().map(|(c, _)| *c).collect();
        assert!(chars.contains(&1036));
        assert!(!chars.contains(&1034));
    }

    #[test]
    fn rejects_environment_paths() {
        let hits = detect_in_path(
            "Marvel/Content/Marvel/Environment/IPAsset/Characters/1026/1026300/Mesh.uasset",
            &known(),
        );
        assert!(hits.is_empty());
    }

    #[test]
    fn handles_backslash_separators() {
        let hits = detect_in_path(
            "Marvel\\Content\\Marvel\\Characters\\1011\\1011502\\foo.uasset",
            &known(),
        );
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

    #[test]
    fn matches_square_hero_head_anchor() {
        let hits = detect_in_path(
            "Marvel/Content/Marvel/UI/Textures/HeroPortrait/SquareHeroHead/img_squarehead_10110010_avatar.uasset",
            &known(),
        );
        assert!(hits.contains(&(1011, None)));
    }

    #[test]
    fn matches_square_hero_head_proficiency() {
        let hits = detect_in_path(
            "Marvel/Content/Marvel/UI/Textures/HeroPortrait/SquareHeroHead/Proficiency/img_squarehead_21047020_avatar.uasset",
            &known(),
        );
        assert!(hits.contains(&(1047, None)));
    }

    #[test]
    fn square_head_aggregates_with_catalogue_filter() {
        let paths = vec![
            "Marvel/Content/Marvel/UI/Textures/HeroPortrait/SquareHeroHead/img_squarehead_10110010_avatar.uasset".to_string(),
            "Marvel/Content/Marvel/UI/Textures/HeroPortrait/SquareHeroHead/Proficiency/img_squarehead_21065020_avatar.uasset".to_string(),
        ];
        let matches = detect_heroes_from_paths(paths);
        let ids: Vec<u32> = matches.iter().map(|m| m.character_id).collect();
        assert!(ids.contains(&1011));
        assert!(ids.contains(&1065));
    }

    #[test]
    fn matches_ui_textures_charid_folder_segment() {
        let hits = detect_in_path(
            "Marvel/Content/Marvel/UI/Textures/Ability/1055/icon_105501.uasset",
            &known(),
        );
        assert!(hits.contains(&(1055, None)));
    }

    #[test]
    fn matches_ui_textures_short_filename_token() {
        let hits = detect_in_path(
            "Marvel/Content/Marvel/UI/Textures/HeroLogo/img_herologo_1055_logo.uasset",
            &known(),
        );
        assert!(hits.contains(&(1055, None)));
    }

    #[test]
    fn matches_ui_textures_8digit_with_item_prefix() {
        // 31055001 = item-type 3 + char 1055 + variant 001, char at pos 1.
        let hits = detect_in_path(
            "Marvel/Content/Marvel/UI/Textures/Show/Nameplate/img_nameplate_31055001.uasset",
            &known(),
        );
        assert!(hits.contains(&(1055, None)));
    }

    #[test]
    fn matches_ui_textures_pendant_with_3digit_prefix() {
        // 03810550001: char 1055 at window position 3.
        let hits = detect_in_path(
            "Marvel/Content/Marvel/UI/Textures/Item/Pendant/item_pandant_03810550001.uasset",
            &known(),
        );
        assert!(hits.contains(&(1055, None)));
    }

    #[test]
    fn matches_ui_textures_mall_multi_id() {
        // 10515040and10558000 splits into two digit runs; both decode to chars.
        let hits = detect_in_path(
            "Marvel/Content/Marvel/UI/Textures/Mall/SkinCard/img_mall_10515040and10558000_skin.uasset",
            &known(),
        );
        let ids: Vec<u32> = hits.iter().map(|(c, _)| *c).collect();
        assert!(ids.contains(&1051));
        assert!(ids.contains(&1055));
    }

    #[test]
    fn ui_segment_anchor_suppresses_filename_window() {
        // Mastery filename 21055022 references char 1055 (mastery target), but the
        // 1011/ folder pins ownership to Bruce Banner. Window scan must be skipped.
        let hits = detect_in_path(
            "Marvel/Content/Marvel/UI/Textures/Mastery/Reduce/1011/fb_mastery21055022_bg_03.uasset",
            &known(),
        );
        let ids: Vec<u32> = hits.iter().map(|(c, _)| *c).collect();
        assert!(ids.contains(&1011));
        assert!(!ids.contains(&1055));
    }

    #[test]
    fn ui_window_scan_takes_first_catalogue_match_per_token() {
        // 1055103205 contains char 1055 at pos 0 and char 1032 at pos 4. Only the
        // first (1055) should hit; 1032 is incidental substring overlap.
        let hits = detect_in_path(
            "Marvel/Content/Marvel/UI/Textures/Item/Emote/item_emote_1055103205.uasset",
            &known(),
        );
        let ids: Vec<u32> = hits.iter().map(|(c, _)| *c).collect();
        assert!(ids.contains(&1055));
        assert!(!ids.contains(&1032));
    }

    #[test]
    fn ui_window_scan_skipped_outside_ui_textures() {
        // Without /ui/textures/ anchor, the broad window scan must not run, so
        // 2103312 (which contains substring `1033`) stays a no-match.
        let hits = detect_in_path(
            "Marvel/Content/Marvel/Some/Asset/2103312_thing.uasset",
            &known(),
        );
        assert!(hits.iter().all(|(c, _)| *c != 1033));
    }

    #[test]
    fn ui_career_path_suppressed() {
        // 1011001 is Hulk's default skin id, but Career achievement badges aren't
        // hero-attributed; the copper tier id collides incidentally.
        let hits = detect_in_path(
            "Marvel/Content/Marvel/UI/Textures/Career/Career_Achievement/Career_Achvement_Badge/img_hero_copper_1011001.uasset",
            &known(),
        );
        assert!(hits.is_empty());
    }

    #[test]
    fn ui_depot_path_suppressed() {
        // 2001011 contains substring `1011` at window pos 3; Depot themes aren't
        // hero-attributed.
        let hits = detect_in_path(
            "Marvel/Content/Marvel/UI/Textures/Depot/MCNTheme/icon_mcntheme_2001011.uasset",
            &known(),
        );
        assert!(hits.is_empty());
    }
}
