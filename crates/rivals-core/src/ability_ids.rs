//! Where a package or a Python module sits among the heroes and their abilities, read from its
//! path: the hero's four-digit id, the ability's six-digit id, and whether it is the game's own or
//! a game mode's or an event's version of it.
//!
//! Heroes are numbered from 1011 and abilities after their hero, so `101461` is hero 1014's. A
//! mode's abilities sit under `Level/GameMode/<mode>/` and an event's under `Activity/<event>/`,
//! and their Python modules under `level_mode/m<mode>/` and `activity/a<event>/`. A summon or a
//! buff carries its ability's id with two more digits, as `10146101`. Another character's
//! abilities, as an enemy's in a mode (`4075/407569`), count as that character's where the ability
//! follows it.

use serde::Serialize;

/// Whose a path is, as far as its path says.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct AbilityPlace {
    pub hero: Option<u32>,
    pub ability: Option<u32>,
    /// The game's own, or a mode's or an event's version. Meaningful only with a hero.
    pub variant: Variant,
}

/// Which version of an ability a path belongs to: the game's own runs everywhere, the practice
/// range included, while a mode's or an event's runs only there.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum Variant {
    #[default]
    Base,
    Mode(u32),
    Event(u32),
}

impl std::fmt::Display for Variant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Base => write!(f, "base"),
            Self::Mode(id) => write!(f, "mode {id}"),
            Self::Event(id) => write!(f, "event {id}"),
        }
    }
}

/// Where `path` sits: a container entry, a `/Game` path or a Python module's path.
pub fn place_of(path: &str) -> AbilityPlace {
    let runs = digit_runs(path);
    let ability_of = |owner: &str| {
        runs.iter()
            .find(|run| (run.len() == 6 || run.len() == 8) && run.starts_with(owner))
            .and_then(|run| run[..6].parse::<u32>().ok())
    };
    let owned = runs
        .iter()
        .filter(|run| run.len() == 4)
        .find_map(|run| Some((run.parse::<u32>().ok()?, ability_of(run)?)));
    let (hero, ability) = match owned {
        Some((hero, ability)) => (Some(hero), Some(ability)),
        None => (
            runs.iter()
                .find(|run| run.len() == 4 && run.starts_with("10"))
                .and_then(|run| run.parse::<u32>().ok()),
            None,
        ),
    };
    AbilityPlace {
        hero,
        ability,
        variant: variant_of(path),
    }
}

/// Every run of ASCII digits in `text`, in order.
fn digit_runs(text: &str) -> Vec<&str> {
    let mut runs = Vec::new();
    let mut start = None;
    for (at, c) in text.char_indices() {
        match (c.is_ascii_digit(), start) {
            (true, None) => start = Some(at),
            (false, Some(from)) => {
                runs.push(&text[from..at]);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(from) = start {
        runs.push(&text[from..]);
    }
    runs
}

/// A mode's or an event's version, by the folder or the Python package it sits in.
fn variant_of(path: &str) -> Variant {
    let segments: Vec<&str> = path.split('/').collect();
    for pair in segments.windows(2) {
        let id = |text: &str| text.parse::<u32>().ok();
        let tagged = |text: &str, tag: char| text.strip_prefix(tag).and_then(id);
        let found = match pair[0].to_ascii_lowercase().as_str() {
            "gamemode" => id(pair[1]).map(Variant::Mode),
            "level_mode" => tagged(pair[1], 'm').map(Variant::Mode),
            // A folder holds the event's number, its Python package the number after an `a`.
            "activity" => id(pair[1])
                .or_else(|| tagged(pair[1], 'a'))
                .map(Variant::Event),
            _ => None,
        };
        if let Some(found) = found {
            return found;
        }
    }
    Variant::Base
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place(hero: u32, ability: Option<u32>, variant: Variant) -> AbilityPlace {
        AbilityPlace {
            hero: Some(hero),
            ability,
            variant,
        }
    }

    /// An ability's folders and files, its mode's and event's versions, its summons and its
    /// Python modules all say whose they are.
    #[test]
    fn a_path_names_its_hero_ability_and_version() {
        let cases = [
            (
                "Marvel/Content/Marvel/AbilitySystem/1014/101461/Summoned_10146101_BP.uasset",
                place(1014, Some(101461), Variant::Base),
            ),
            (
                "Marvel/Content/Marvel/AbilitySystem/Activity/10130/1014/101461/BuffAbility_10130_10149020_BP.uasset",
                place(1014, Some(101461), Variant::Event(10130)),
            ),
            (
                "Marvel/Content/Marvel/AbilitySystem/Level/GameMode/2206/1014/101463/101463.uasset",
                place(1014, Some(101463), Variant::Mode(2206)),
            ),
            (
                "Marvel/Content/Marvel/AbilitySystem/GameMode/2208/4075/407569/Summoner_40756901_BP.uasset",
                place(4075, Some(407569), Variant::Mode(2208)),
            ),
            (
                "/Script/UnrealEnginePython/subclassing/heroes/c1014/ability/PyAbility_101461.PyConfig_101461",
                place(1014, Some(101461), Variant::Base),
            ),
            (
                "/Script/UnrealEnginePython/subclassing/level_mode/m2206/heroes/c1014/ability/PyAbility_2206_101463",
                place(1014, Some(101463), Variant::Mode(2206)),
            ),
            (
                "/Script/UnrealEnginePython/subclassing/activity/a10130/heroes/c1014/PyAbility_10130_101467",
                place(1014, Some(101467), Variant::Event(10130)),
            ),
            (
                "Marvel/Content/Marvel/Characters/1033/1033001/Cues/WeaponCue/Cue_Weapon_Loop_103311_BP.uasset",
                place(1033, Some(103311), Variant::Base),
            ),
            (
                "Marvel/Content/Marvel/AI/1014/AIAbilityActivation_CancelPunisher101461.uasset",
                place(1014, Some(101461), Variant::Base),
            ),
            (
                "Marvel/Content/Marvel/Characters/1033/1033001/Meshes/SK_1033001.uasset",
                place(1033, None, Variant::Base),
            ),
        ];
        for (path, wanted) in cases {
            assert_eq!(place_of(path), wanted, "{path}");
        }
        let nobody = place_of("Marvel/Content/Marvel/Maps/Lobby/2014/Lobby_2014001405.umap");
        assert_eq!((nobody.hero, nobody.ability), (None, None));
    }
}
