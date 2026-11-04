//! Commands that move mods between the folders below `~mods` and make, rename and delete folders,
//! the profiles and hero cache following the mods to their new names.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use rivals_core::mod_library::{self, BulkOpResult, FolderDeleteReport};
use tauri::State;

use crate::game_status;
use crate::mods::hero_cache::{HeroCache, HeroCacheState};
use crate::settings::{ModProfile, SettingsState};

fn refuse_while_running() -> Result<(), String> {
    if game_status::should_block_for_game() {
        return Err(game_status::game_running_error());
    }
    Ok(())
}

/// A mod's name as profiles and the hero cache know it: its path without `.disabled`.
fn display_name(full_name: &str) -> &str {
    full_name.strip_suffix(".disabled").unwrap_or(full_name)
}

#[tauri::command]
pub(crate) fn move_mods(
    settings: State<'_, SettingsState>,
    cache: State<'_, HeroCacheState>,
    mods_folder: String,
    full_names: Vec<String>,
    to: String,
) -> Result<BulkOpResult, String> {
    refuse_while_running()?;
    let to = mod_library::folder_path(&to)?;
    let result = mod_library::move_mods(Path::new(&mods_folder), &full_names, &to)?;
    let failed: HashSet<&str> = result
        .failures
        .iter()
        .map(|f| f.full_name.as_str())
        .collect();
    let renamed: HashMap<String, String> = full_names
        .iter()
        .filter(|name| !failed.contains(name.as_str()))
        .map(|name| {
            let moved = mod_library::moved_name(name, &to);
            (
                display_name(name).to_string(),
                display_name(&moved).to_string(),
            )
        })
        .collect();
    follow_renames(&settings, &cache, |name| renamed.get(name).cloned());
    Ok(result)
}

#[tauri::command]
pub(crate) fn create_mod_folder(mods_folder: String, path: String) -> Result<String, String> {
    mod_library::create_folder(Path::new(&mods_folder), &path)
}

#[tauri::command]
pub(crate) fn rename_mod_folder(
    settings: State<'_, SettingsState>,
    cache: State<'_, HeroCacheState>,
    mods_folder: String,
    path: String,
    new_name: String,
) -> Result<String, String> {
    refuse_while_running()?;
    let from = mod_library::folder_path(&path)?;
    let to = mod_library::rename_folder(Path::new(&mods_folder), &from, &new_name)?;
    follow_renames(&settings, &cache, |name| {
        mod_library::renamed_under(name, &from, &to)
    });
    Ok(to)
}

#[tauri::command]
pub(crate) fn delete_mod_folder(
    mods_folder: String,
    path: String,
    with_mods: bool,
) -> Result<FolderDeleteReport, String> {
    refuse_while_running()?;
    mod_library::delete_folder(Path::new(&mods_folder), &path, with_mods)
}

/// Puts the profiles and the hero cache on the names `renamed` gives mods that moved, saving each
/// only when it changed. The files have moved already, so a save that fails is logged, not
/// returned.
pub(crate) fn follow_renames(
    settings: &SettingsState,
    cache: &HeroCacheState,
    renamed: impl Fn(&str) -> Option<String>,
) {
    if let Ok(mut guard) = settings.lock()
        && rekey_profiles(&mut guard.mod_profiles, &renamed)
        && let Err(e) = guard.save()
    {
        eprintln!("Could not save the profiles after mods moved: {e}");
    }
    if let Ok(mut guard) = cache.lock()
        && rekey_hero_cache(&mut guard, &renamed)
        && let Err(e) = guard.save()
    {
        eprintln!("Could not save the hero cache after mods moved: {e}");
    }
}

fn rekey_profiles(profiles: &mut [ModProfile], renamed: &impl Fn(&str) -> Option<String>) -> bool {
    let mut changed = false;
    for name in profiles.iter_mut().flat_map(|p| p.enabled_mods.iter_mut()) {
        if let Some(new) = renamed(name) {
            *name = new;
            changed = true;
        }
    }
    changed
}

fn rekey_hero_cache(cache: &mut HeroCache, renamed: &impl Fn(&str) -> Option<String>) -> bool {
    let moved: Vec<(String, String)> = cache
        .entries
        .keys()
        .filter_map(|name| Some((name.clone(), renamed(name)?)))
        .collect();
    // Every entry comes out before any goes back, so one name taking another's cannot drop it.
    let entries: Vec<_> = moved
        .into_iter()
        .filter_map(|(from, to)| Some((to, cache.entries.remove(&from)?)))
        .collect();
    let changed = !entries.is_empty();
    cache.entries.extend(entries);
    changed
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::mods::hero_cache::ModHeroCacheEntry;

    fn profile(mods: &[&str]) -> ModProfile {
        ModProfile {
            name: "p".to_string(),
            enabled_mods: mods.iter().map(|m| m.to_string()).collect(),
            created_at: 0,
            modified_at: 0,
        }
    }

    fn entry(size_bytes: u64) -> ModHeroCacheEntry {
        ModHeroCacheEntry {
            size_bytes,
            catalogue_stamp: 0,
            heroes: Vec::new(),
        }
    }

    #[test]
    fn profiles_follow_a_renamed_folder() {
        let mut profiles = [profile(&["Skins/A.pak", "Skinsy/B.pak", "C.pak"])];
        let renamed = |name: &str| mod_library::renamed_under(name, "Skins", "Art");
        assert!(rekey_profiles(&mut profiles, &renamed));
        assert_eq!(
            profiles[0].enabled_mods,
            ["Art/A.pak", "Skinsy/B.pak", "C.pak"]
        );
        assert!(
            !rekey_profiles(&mut profiles, &renamed),
            "nothing left to move"
        );
    }

    #[test]
    fn the_hero_cache_follows_moved_mods_even_onto_each_other_s_names() {
        let mut cache = HeroCache::default();
        cache.entries.insert("A.pak".to_string(), entry(1));
        cache.entries.insert("Skins/A.pak".to_string(), entry(2));
        cache.entries.insert("C.pak".to_string(), entry(3));
        let renamed = |name: &str| match name {
            "A.pak" => Some("Skins/A.pak".to_string()),
            "Skins/A.pak" => Some("Other/A.pak".to_string()),
            _ => None,
        };
        assert!(rekey_hero_cache(&mut cache, &renamed));
        assert_eq!(cache.entries["Skins/A.pak"].size_bytes, 1);
        assert_eq!(cache.entries["Other/A.pak"].size_bytes, 2);
        assert!(!cache.entries.contains_key("A.pak"));
        assert_eq!(cache.entries.len(), 3);
    }
}
