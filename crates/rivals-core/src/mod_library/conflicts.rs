//! Which switched-on mods carry the same file, and which one's copy the game loads.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::mods::{walk_mod_files, winner_order};
use crate::paths::mods_dir;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetConflict {
    /// The file more than one mod carries, by its path below `Content`.
    pub asset: String,
    /// The mods carrying it, the one whose copy loads first: highest `_N_P` patch number, then
    /// alphabetical.
    pub mods: Vec<String>,
    /// The game ships this file too, so every mod here replaces the game's and only the winner's
    /// copy loads.
    #[serde(default)]
    pub overrides_game: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictGroup {
    /// The mod's pak below `~mods`.
    pub mod_name: String,
    pub display_name: String,
    /// The other mods it shares a file with.
    pub conflicts_with: Vec<String>,
    /// How many of its files another mod carries too.
    pub conflicting_asset_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictReport {
    /// One per mod sharing a file, by name.
    pub groups: Vec<ConflictGroup>,
    /// One per shared file, the most shared first.
    pub asset_conflicts: Vec<AssetConflict>,
    /// How many switched-on mods were read.
    pub mods_scanned: usize,
}

/// Lists what a mod's files hold, which a front end may answer from a cache.
pub trait ContentLister {
    /// The paths a container's directory index lists.
    fn utoc(&self, utoc: &Path) -> Result<Vec<String>, String>;
    /// The entries a pak's index lists.
    fn pak(&self, pak: &Path) -> Result<Vec<String>, String>;
}

/// Reads every listing from disk.
pub struct DiskLister;

impl ContentLister for DiskLister {
    fn utoc(&self, utoc: &Path) -> Result<Vec<String>, String> {
        crate::pak::containers::list_utoc_paths(utoc)
    }

    fn pak(&self, pak: &Path) -> Result<Vec<String>, String> {
        crate::pak::plain::list_files(pak)
    }
}

/// The game maps any `<Project>/Content/...` to `/Game/...`, so mods cooked under other project
/// names overlay the same file. Dropping the project lets them collide here too.
fn canonical_asset_key(path: &str) -> String {
    let lower = path.replace('\\', "/").to_lowercase();
    let trimmed = lower.trim_start_matches('/');
    if let Some(at) = trimmed.find("/content/") {
        let prefix = &trimmed[..at];
        if !prefix.is_empty() && !prefix.contains('/') {
            return trimmed[at + "/content/".len()..].to_string();
        }
    }
    lower
}

/// Every file more than one switched-on mod carries. A container is listed by its index, a plain
/// pak by its entries; a mod whose listing does not read counts as carrying nothing.
pub fn check_conflicts(
    game_root: &str,
    recursive: bool,
    lister: &dyn ContentLister,
) -> Result<ConflictReport, String> {
    let mods = mods_dir(game_root);
    if !mods.exists() {
        return Ok(ConflictReport {
            groups: Vec::new(),
            asset_conflicts: Vec::new(),
            mods_scanned: 0,
        });
    }
    let enabled: Vec<String> = walk_mod_files(&mods, recursive)
        .into_iter()
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
        .filter(|name| name.ends_with(".pak"))
        .collect();

    let mut carried: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for name in &enabled {
        let pak = mods.join(name);
        let utoc = pak.with_extension("utoc");
        // The pak beside a container carries the mod's own files too, INIs and sound banks, which
        // clash with another mod's like any package.
        let mut assets = if utoc.is_file() {
            lister.utoc(&utoc).unwrap_or_default()
        } else {
            Vec::new()
        };
        assets.extend(lister.pak(&pak).unwrap_or_default());
        for asset in assets {
            let key = canonical_asset_key(&asset);
            // What an older packer put in the pak beside a container is no file of the mod's.
            if crate::pak::plain::is_unsupported_entry(&key) {
                continue;
            }
            carried.entry(key).or_default().push(name.clone());
        }
    }

    let mut asset_conflicts: Vec<AssetConflict> = carried
        .into_iter()
        .filter_map(|(asset, mut holders)| {
            holders.sort_by(|a, b| winner_order(a, b));
            holders.dedup();
            (holders.len() > 1).then_some(AssetConflict {
                asset,
                mods: holders,
                overrides_game: false,
            })
        })
        .collect();
    // The most shared first, and by path among as many.
    asset_conflicts.sort_by_key(|conflict| std::cmp::Reverse(conflict.mods.len()));

    // Opening the base game is the slow part, so it is only paid when there is a conflict to mark.
    if !asset_conflicts.is_empty() {
        let shipped = crate::mod_report::shipped_by_game(
            game_root,
            asset_conflicts.iter().map(|c| c.asset.as_str()),
        )
        .unwrap_or_default();
        for conflict in &mut asset_conflicts {
            conflict.overrides_game = shipped.contains(&conflict.asset);
        }
    }

    let mut shared: BTreeMap<String, (BTreeSet<String>, usize)> = BTreeMap::new();
    for conflict in &asset_conflicts {
        for holder in &conflict.mods {
            let entry = shared.entry(holder.clone()).or_default();
            entry.0.extend(
                conflict
                    .mods
                    .iter()
                    .filter(|other| *other != holder)
                    .cloned(),
            );
            entry.1 += 1;
        }
    }
    let mut groups: Vec<ConflictGroup> = shared
        .into_iter()
        .map(|(name, (others, count))| {
            let mut conflicts_with: Vec<String> = others.into_iter().collect();
            conflicts_with.sort_by_key(|other| other.to_lowercase());
            ConflictGroup {
                display_name: name.clone(),
                mod_name: name,
                conflicts_with,
                conflicting_asset_count: count,
            }
        })
        .collect();
    groups.sort_by_key(|group| group.display_name.to_lowercase());

    Ok(ConflictReport {
        groups,
        asset_conflicts,
        mods_scanned: enabled.len(),
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn mods_cooked_under_other_projects_collide() {
        let marvel =
            canonical_asset_key("Marvel/Content/Marvel/Characters/1054/1054001/Mesh.uasset");
        let season6 =
            canonical_asset_key("Season6/Content/Marvel/Characters/1054/1054001/Mesh.uasset");
        assert_eq!(marvel, season6);
        assert_eq!(marvel, "marvel/characters/1054/1054001/mesh.uasset");
        assert_eq!(
            canonical_asset_key("Marvel\\Content\\Marvel\\Characters\\1054\\Mesh.uasset"),
            "marvel/characters/1054/mesh.uasset"
        );
        assert_eq!(
            canonical_asset_key("/Marvel/Content/Foo/Bar.uasset"),
            "foo/bar.uasset"
        );
        assert_eq!(
            canonical_asset_key("SomeOther/Path/File.uasset"),
            "someother/path/file.uasset"
        );
        assert_eq!(
            canonical_asset_key("Marvel/Content/Plugins/Foo/Content/Bar.uasset"),
            "plugins/foo/content/bar.uasset"
        );
    }

    /// A lister answering from a table, so no pak has to be built.
    struct Table(Vec<(&'static str, Vec<&'static str>)>);

    impl ContentLister for Table {
        fn utoc(&self, utoc: &Path) -> Result<Vec<String>, String> {
            self.pak(utoc)
        }

        fn pak(&self, pak: &Path) -> Result<Vec<String>, String> {
            let name = pak.file_stem().unwrap().to_string_lossy().into_owned();
            Ok(self
                .0
                .iter()
                .find(|(held, _)| *held == name)
                .map(|(_, files)| files.iter().map(|f| f.to_string()).collect())
                .unwrap_or_default())
        }
    }

    /// Two switched-on mods carrying one file conflict, the higher patch number first; a
    /// switched-off one and the toolkit's own listing do not count.
    #[test]
    fn switched_on_mods_sharing_a_file_conflict() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rivals-conflicts-{stamp}"));
        let game = root.to_str().unwrap();
        let mods = mods_dir(game);
        std::fs::create_dir_all(&mods).unwrap();
        for name in [
            "Low_100_P.pak",
            "High_9999999_P.pak",
            "Off_9999999_P.pak.disabled",
        ] {
            std::fs::write(mods.join(name), b"").unwrap();
        }
        let shared = "Marvel/Content/Marvel/UI/Shared.ini";
        let lister = Table(vec![
            (
                "Low_100_P",
                vec![shared, "chunknames", "Marvel/Content/Low.ini"],
            ),
            ("High_9999999_P", vec![shared, "chunknames"]),
            ("Off_9999999_P", vec![shared]),
        ]);
        let report = check_conflicts(game, false, &lister).expect("conflicts");
        assert_eq!(report.mods_scanned, 2);
        assert_eq!(report.asset_conflicts.len(), 1);
        let conflict = &report.asset_conflicts[0];
        assert_eq!(conflict.asset, "marvel/ui/shared.ini");
        assert_eq!(conflict.mods, vec!["High_9999999_P.pak", "Low_100_P.pak"]);
        assert_eq!(report.groups.len(), 2);
        assert_eq!(report.groups[0].mod_name, "High_9999999_P.pak");
        assert_eq!(report.groups[0].conflicts_with, vec!["Low_100_P.pak"]);
        std::fs::remove_dir_all(&root).ok();
    }

    /// A lister telling a mod's container from the pak beside it, by file name.
    struct Split(Vec<(&'static str, Vec<&'static str>)>);

    impl ContentLister for Split {
        fn utoc(&self, utoc: &Path) -> Result<Vec<String>, String> {
            self.pak(utoc)
        }

        fn pak(&self, file: &Path) -> Result<Vec<String>, String> {
            let name = file.file_name().unwrap().to_string_lossy().into_owned();
            Ok(self
                .0
                .iter()
                .find(|(held, _)| *held == name)
                .map(|(_, files)| files.iter().map(|f| f.to_string()).collect())
                .unwrap_or_default())
        }
    }

    /// Two IoStore mods whose paks carry the same sound bank conflict over it, beside what their
    /// containers hold.
    #[test]
    fn the_files_beside_a_container_conflict_too() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rivals-conflicts-pak-{stamp}"));
        let game = root.to_str().unwrap();
        let mods = mods_dir(game);
        std::fs::create_dir_all(&mods).unwrap();
        for name in [
            "A_9999999_P.pak",
            "A_9999999_P.utoc",
            "B_100_P.pak",
            "B_100_P.utoc",
        ] {
            std::fs::write(mods.join(name), b"").unwrap();
        }
        let bank = "Marvel/Content/WwiseAudio/English/bnk_vo_system.bnk";
        let lister = Split(vec![
            ("A_9999999_P.utoc", vec!["Marvel/Content/A.uasset"]),
            ("A_9999999_P.pak", vec![bank]),
            ("B_100_P.utoc", vec![]),
            ("B_100_P.pak", vec![bank, "chunknames"]),
        ]);
        let report = check_conflicts(game, false, &lister).expect("conflicts");
        assert_eq!(report.asset_conflicts.len(), 1);
        assert_eq!(
            report.asset_conflicts[0].asset,
            "wwiseaudio/english/bnk_vo_system.bnk"
        );
        assert_eq!(
            report.asset_conflicts[0].mods,
            vec!["A_9999999_P.pak", "B_100_P.pak"]
        );
        std::fs::remove_dir_all(&root).ok();
    }
}
