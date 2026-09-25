//! Detects asset conflicts between installed mod pak files.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::pak;
use crate::paths::mods_dir;

use super::walk_mod_files;

#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct AssetConflict {
    /// The asset path that multiple mods touch.
    pub asset: String,
    /// Mod display names that contain this asset, winner first: highest `_N_P` patch number,
    /// then alphabetical.
    pub mods: Vec<String>,
    /// The base game ships this asset too, so every mod here replaces a vanilla package and only
    /// the winner's copy loads.
    #[serde(default)]
    pub overrides_game: bool,
}

#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct ConflictGroup {
    /// The mod's full filename on disk.
    pub mod_name: String,
    /// Display name (stripped .pak suffix etc).
    pub display_name: String,
    /// Display names of other mods this one conflicts with.
    pub conflicts_with: Vec<String>,
    /// Number of conflicting assets.
    pub conflicting_asset_count: usize,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct ConflictReport {
    /// Per-mod conflict summaries.
    pub groups: Vec<ConflictGroup>,
    /// Full asset-level detail.
    pub asset_conflicts: Vec<AssetConflict>,
    /// Total number of enabled mods scanned.
    pub mods_scanned: usize,
}

/// UE maps any `<ProjectName>/Content/...` to `/Game/...` at runtime, so paks cooked
/// from different project names overlay the same virtual asset. Strip the project
/// prefix so cross-project conflicts collide in the same map bucket.
fn canonical_asset_key(path: &str) -> String {
    let lower = path.replace('\\', "/").to_lowercase();
    let trimmed = lower.trim_start_matches('/');
    if let Some(idx) = trimmed.find("/content/") {
        let prefix = &trimmed[..idx];
        if !prefix.is_empty() && !prefix.contains('/') {
            return trimmed[idx + "/content/".len()..].to_string();
        }
    }
    lower
}

/// Check all enabled mods for asset-level conflicts (same file path in multiple paks).
pub(crate) fn check_conflicts(game_root: &str, recursive: bool) -> Result<ConflictReport, String> {
    let mods_folder = mods_dir(game_root);
    if !mods_folder.exists() {
        return Ok(ConflictReport {
            groups: Vec::new(),
            asset_conflicts: Vec::new(),
            mods_scanned: 0,
        });
    }

    // Collect enabled mod pak/utoc files.
    let mod_files = walk_mod_files(&mods_folder, recursive);
    let enabled_paks: Vec<String> = mod_files
        .iter()
        .filter_map(|rel| {
            let name = rel.to_string_lossy();
            if name.ends_with(".pak") && !name.ends_with(".disabled") {
                Some(name.into_owned().replace('\\', "/"))
            } else {
                None
            }
        })
        .collect();

    // Map: asset path (lowercased) → list of mod display names that contain it.
    let mut asset_to_mods: HashMap<String, Vec<String>> = HashMap::new();

    for pak_rel in &enabled_paks {
        let pak_abs = mods_folder.join(pak_rel.replace('/', "\\"));
        let pak_abs_str = pak_abs.to_string_lossy().to_string();

        // Determine display name for this mod.
        let display = pak_rel.clone();

        // Try IoStore first (utoc), fall back to plain pak.
        let stem = pak_rel.strip_suffix(".pak").unwrap_or(pak_rel);
        let utoc_rel = format!("{stem}.utoc");
        let utoc_abs = mods_folder.join(utoc_rel.replace('/', "\\"));

        let assets: Vec<String> = if utoc_abs.exists() {
            pak::list_utoc_contents(&utoc_abs.to_string_lossy()).unwrap_or_default()
        } else {
            pak::list_pak_contents(&pak_abs_str).unwrap_or_default()
        };

        for asset in assets {
            let key = canonical_asset_key(&asset);
            // Skip patched_files entries, these are metadata not real asset conflicts.
            if key.contains("patched_files") {
                continue;
            }
            asset_to_mods.entry(key).or_default().push(display.clone());
        }
    }

    // Filter to assets with 2+ mods, sort each mod list alphabetically (winner first).
    let mut asset_conflicts: Vec<AssetConflict> = asset_to_mods
        .into_iter()
        .filter(|(_, mods)| mods.len() > 1)
        .map(|(asset, mut mods)| {
            mods.sort_by(|a, b| rivals_core::mods::winner_order(a, b));
            mods.dedup();
            AssetConflict {
                asset,
                mods,
                overrides_game: false,
            }
        })
        .filter(|c| c.mods.len() > 1)
        .collect();

    asset_conflicts.sort_by(|a, b| b.mods.len().cmp(&a.mods.len()));

    // Opening the base game is the slow part, so it is only paid when there is a conflict to mark.
    if !asset_conflicts.is_empty() {
        let shipped = rivals_core::mod_report::shipped_by_game(
            game_root,
            asset_conflicts.iter().map(|c| c.asset.as_str()),
        )
        .unwrap_or_default();
        for conflict in &mut asset_conflicts {
            conflict.overrides_game = shipped.contains(&conflict.asset);
        }
    }

    // Build per-mod conflict groups.
    let mut mod_conflicts: HashMap<String, HashSet<String>> = HashMap::new();
    let mut mod_asset_counts: HashMap<String, usize> = HashMap::new();

    for conflict in &asset_conflicts {
        for mod_name in &conflict.mods {
            for other in &conflict.mods {
                if other != mod_name {
                    mod_conflicts
                        .entry(mod_name.clone())
                        .or_default()
                        .insert(other.clone());
                }
            }
            *mod_asset_counts.entry(mod_name.clone()).or_insert(0) += 1;
        }
    }

    let mut groups: Vec<ConflictGroup> = mod_conflicts
        .into_iter()
        .map(|(mod_name, others)| {
            let mut conflicts_with: Vec<String> = others.into_iter().collect();
            conflicts_with.sort_by_key(|a| a.to_lowercase());
            ConflictGroup {
                display_name: mod_name.clone(),
                mod_name: mod_name.clone(),
                conflicting_asset_count: mod_asset_counts.get(&mod_name).copied().unwrap_or(0),
                conflicts_with,
            }
        })
        .collect();

    groups.sort_by(|a, b| {
        a.display_name
            .to_lowercase()
            .cmp(&b.display_name.to_lowercase())
    });

    let mods_scanned = enabled_paks.len();

    Ok(ConflictReport {
        groups,
        asset_conflicts,
        mods_scanned,
    })
}

#[cfg(test)]
mod tests {
    use super::canonical_asset_key;

    #[test]
    fn cross_project_paths_collide() {
        let marvel =
            canonical_asset_key("Marvel/Content/Marvel/Characters/1054/1054001/Mesh.uasset");
        let season6 =
            canonical_asset_key("Season6/Content/Marvel/Characters/1054/1054001/Mesh.uasset");
        assert_eq!(marvel, season6);
        assert_eq!(marvel, "marvel/characters/1054/1054001/mesh.uasset");
    }

    #[test]
    fn handles_backslash_paths() {
        let key = canonical_asset_key("Marvel\\Content\\Marvel\\Characters\\1054\\Mesh.uasset");
        assert_eq!(key, "marvel/characters/1054/mesh.uasset");
    }

    #[test]
    fn handles_leading_slash() {
        let key = canonical_asset_key("/Marvel/Content/Foo/Bar.uasset");
        assert_eq!(key, "foo/bar.uasset");
    }

    #[test]
    fn no_content_segment_falls_back_to_lowercase() {
        let key = canonical_asset_key("SomeOther/Path/File.uasset");
        assert_eq!(key, "someother/path/file.uasset");
    }

    #[test]
    fn nested_content_only_strips_top_project() {
        let key = canonical_asset_key("Marvel/Content/Plugins/Foo/Content/Bar.uasset");
        assert_eq!(key, "plugins/foo/content/bar.uasset");
    }
}
