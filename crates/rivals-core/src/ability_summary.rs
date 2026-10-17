//! What one ability is made of, read from the game's own data: its row in its hero's ability table,
//! the ability item the row names, and every package carrying its id, the game's own and each game
//! mode's and event's version alike, with the Python classes, summons, gameplay tags and telling
//! calls they hold. A hero's id lists its abilities instead, one line each.

use std::collections::BTreeSet;

use rivals_uasset::{Mappings, ParsedPackage, PropertyEntry, PropertyValue, TermKind};
use serde::Serialize;

use crate::ability_ids::{Variant, place_of};
use crate::game_search::read_packages;

/// An ability as its hero's ability table lists it.
#[derive(Debug, Clone, Serialize)]
pub struct AbilityRow {
    pub id: u32,
    pub name: String,
    /// The key it is cast with, as the table names it: `E`, `MouseLeft`, `Passive`,
    /// `BondAbility1`.
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cooldown: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub charges: Option<String>,
    /// What the table marks it as: ultimate, passive, team-up, weapon, melee, reload, summoned.
    pub flags: Vec<&'static str>,
    pub description: String,
    /// The ability item the row names.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset: Option<String>,
}

/// The ability item a row names: what the ability runs as and the inputs it binds.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AbilityItem {
    /// The Blueprint class the ability runs as.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    /// The ability's own inputs: each method and the key it is bound to, with how it triggers.
    pub inputs: Vec<MethodInput>,
    /// The input actions its bindings name.
    pub actions: Vec<String>,
    /// The Python class its configuration is an object of.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MethodInput {
    pub method: String,
    pub key: String,
    pub mode: String,
}

/// A package carrying the ability's id, with the version it belongs to.
#[derive(Debug, Clone, Serialize)]
pub struct AbilityPackage {
    pub path: String,
    pub variant: Variant,
    /// The container it was read from, which is where the inspector opens it.
    pub container: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_mod: Option<String>,
}

/// A Python class one of the packages is made from or names.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct PythonClass {
    pub class: String,
    pub variant: Variant,
}

/// A call or a delegate one of the packages' scripts makes, as its statement reads it.
#[derive(Debug, Clone, Serialize)]
pub struct SummaryCall {
    pub text: String,
    pub package: String,
    pub export: String,
    pub export_index: u32,
    pub offset: u32,
    /// One of the kinds a summary lists by default: launching, spawning, summoning, timers and
    /// delegates bound or broadcast.
    pub notable: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AbilitySummary {
    pub id: u32,
    pub hero: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row: Option<AbilityRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<AbilityItem>,
    pub packages: Vec<AbilityPackage>,
    pub python: Vec<PythonClass>,
    pub summons: Vec<String>,
    pub tags: Vec<String>,
    /// The notable calls, or every call when all were asked for.
    pub calls: Vec<SummaryCall>,
    /// How many calls were left out for not being notable.
    pub other_calls: usize,
    pub unreadable: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HeroAbilities {
    pub hero: u32,
    pub rows: Vec<AbilityRow>,
}

/// Every ability hero `hero`'s table lists, its texts in `culture` where one is given.
pub fn hero_abilities(
    game_root: &str,
    mappings: &Mappings,
    culture: Option<&str>,
    hero: u32,
) -> Result<HeroAbilities, String> {
    let read = read_packages(game_root, mappings, true, &[&table_filter(hero)])?;
    Ok(HeroAbilities {
        hero,
        rows: table_rows(game_root, mappings, culture, hero, read)?,
    })
}

/// What ability `id` is made of, its texts in `culture` where one is given. Every call its scripts
/// make is listed with `all_calls`, and only the telling ones without.
pub fn ability_summary(
    game_root: &str,
    mappings: &Mappings,
    culture: Option<&str>,
    id: u32,
    all_calls: bool,
) -> Result<AbilitySummary, String> {
    let hero = id / 100;
    let own = id.to_string();
    let table = table_filter(hero);
    let read = read_packages(game_root, mappings, true, &[&table, &own])?;
    let (tables, read): (Vec<_>, Vec<_>) = read
        .into_iter()
        .partition(|package| package.path.contains(&table));
    let row = table_rows(game_root, mappings, culture, hero, tables)
        .ok()
        .and_then(|rows| rows.into_iter().find(|row| row.id == id));
    let mut summary = AbilitySummary {
        id,
        hero,
        row,
        item: None,
        packages: Vec::new(),
        python: Vec::new(),
        summons: Vec::new(),
        tags: Vec::new(),
        calls: Vec::new(),
        other_calls: 0,
        unreadable: Vec::new(),
    };
    let asset = summary.row.as_ref().and_then(|row| row.asset.clone());
    let mut python = BTreeSet::new();
    let mut tags = BTreeSet::new();
    let mut seen_calls = BTreeSet::new();
    for package in read {
        let place = place_of(&package.path);
        // The filter keeps any path holding the digits; only this ability's are its own.
        let stem = package.path.rsplit('/').next().unwrap_or(&package.path);
        if place.ability != Some(id) && !stem.contains(&own) {
            continue;
        }
        summary.packages.push(AbilityPackage {
            path: package.path.clone(),
            variant: place.variant,
            container: package.container.clone(),
            in_mod: package.in_mod.clone(),
        });
        if stem.starts_with("Summoned_") {
            summary.summons.push(package.path.clone());
        }
        let parsed = match package.parsed {
            Ok(parsed) => parsed,
            Err(why) => {
                summary.unreadable.push((package.path, why));
                continue;
            }
        };
        for import in &parsed.imports {
            if crate::mod_report::is_python_class(&import.path) {
                python.insert(PythonClass {
                    class: import.path.clone(),
                    variant: place_of(&import.path).variant,
                });
            }
        }
        if summary.item.is_none()
            && let Some(asset) = &asset
        {
            summary.item = ability_item(&parsed, asset);
        }
        for export in &parsed.exports {
            tags_in(&export.properties, &mut tags);
            tags_in(&export.defaults, &mut tags);
        }
        calls_in(
            &package.path,
            &parsed,
            &mut summary,
            &mut tags,
            &mut seen_calls,
            all_calls,
        );
    }
    summary.python = python.into_iter().collect();
    summary.tags = tags.into_iter().collect();
    Ok(summary)
}

/// The path a hero's ability table sits at, as a filter takes it.
fn table_filter(hero: u32) -> String {
    format!("AbilitySystem/{hero}/{hero}_Table/{hero}_AbilityTable")
}

/// The rows of hero `hero`'s ability table, read among `read`.
fn table_rows(
    game_root: &str,
    mappings: &Mappings,
    culture: Option<&str>,
    hero: u32,
    read: Vec<crate::game_search::ReadPackage>,
) -> Result<Vec<AbilityRow>, String> {
    let package = read
        .into_iter()
        .next()
        .ok_or_else(|| format!("hero {hero} has no ability table"))?;
    let mut parsed = package.parsed?;
    if let Some(culture) = culture {
        let localizer = crate::localization::Localizer::for_culture(game_root, culture)?;
        crate::localization::localize(
            &mut parsed,
            &localizer,
            &crate::localization::TableSource {
                game_root,
                container: &package.container,
                mappings: Some(mappings),
            },
        );
    }
    let table = parsed
        .exports
        .iter()
        .find_map(|export| export.data_table.as_ref())
        .ok_or_else(|| format!("hero {hero}'s ability table did not read as a table"))?;
    Ok(table
        .rows
        .iter()
        .filter_map(|row| ability_row(&row.name, &row.fields))
        .collect())
}

/// The value of the field named `name`.
fn field<'a>(fields: &'a [PropertyEntry], name: &str) -> Option<&'a PropertyValue> {
    fields
        .iter()
        .find(|entry| entry.name == name)
        .map(|entry| &entry.value)
}

/// A value as the game shows it: a text in the language looked up, an enumerator without its
/// enum's name.
fn shown(value: &PropertyValue) -> String {
    if let PropertyValue::Text {
        display: Some(display),
        ..
    } = value
    {
        return display.clone();
    }
    let text = value.summary();
    match value {
        PropertyValue::Enum { .. } => text.rsplit("::").next().unwrap_or(&text).to_string(),
        _ => text,
    }
}

fn ability_row(name: &str, fields: &[PropertyEntry]) -> Option<AbilityRow> {
    let id = name.parse().ok()?;
    let text = |field_name: &str| field(fields, field_name).map(shown).unwrap_or_default();
    let key = text("ActivateInput");
    let mut flags = Vec::new();
    let tags: Vec<String> = match field(fields, "AbilityUITags") {
        Some(PropertyValue::Array { items } | PropertyValue::Set { items }) => {
            items.iter().map(PropertyValue::summary).collect()
        }
        _ => Vec::new(),
    };
    for (tag, flag) in [
        ("Ability.UI.Ultimate", "ultimate"),
        ("Ability.UI.Weapon", "weapon"),
        ("Ability.UI.Melee", "melee"),
        ("Ability.UI.Reload", "reload"),
    ] {
        if tags.iter().any(|held| held.contains(tag)) {
            flags.push(flag);
        }
    }
    if key == "Passive" {
        flags.push("passive");
    }
    if key.starts_with("BondAbility") {
        flags.push("team-up");
    }
    if text("AbilityUser") == "Summoned" {
        flags.push("summoned");
    }
    let some = |text: String| (!text.is_empty()).then_some(text);
    Some(AbilityRow {
        id,
        name: text("TName"),
        key,
        cooldown: some(text("AbilityCD"))
            .filter(|cooldown| !matches!(cooldown.as_str(), "0" | "0.0")),
        charges: some(text("MaxChargeCount")).filter(|charges| charges != "0"),
        flags,
        description: text("Description"),
        asset: some(text("AbilityAsset")),
    })
}

/// The ability item at `asset` in `parsed`, when the package holds it.
fn ability_item(parsed: &ParsedPackage, asset: &str) -> Option<AbilityItem> {
    let export = parsed
        .exports
        .iter()
        .find(|export| export.path.eq_ignore_ascii_case(asset))?;
    let fields = &export.properties;
    let mut item = AbilityItem {
        class: field(fields, "GrantedAbility").map(PropertyValue::summary),
        ..Default::default()
    };
    if let Some(PropertyValue::Array { items }) = field(fields, "BindInputs") {
        for bind in items {
            let PropertyValue::Struct { fields, .. } = bind else {
                continue;
            };
            let config = match field(fields, "InputConfig") {
                Some(PropertyValue::Struct { fields, .. }) => fields.as_slice(),
                _ => &[],
            };
            item.inputs.push(MethodInput {
                method: field(fields, "MethodName").map(shown).unwrap_or_default(),
                key: field(config, "InputBind").map(shown).unwrap_or_default(),
                mode: field(config, "SkillActivationMode")
                    .map(shown)
                    .unwrap_or_default(),
            });
        }
    }
    if let Some(PropertyValue::Array { items }) = field(fields, "BindInputConfigs") {
        for config in items {
            if let PropertyValue::Struct { fields, .. } = config
                && let Some(action) = field(fields, "BindInputAction")
            {
                let action = action.summary();
                if !action.is_empty() && action != "None" && !item.actions.contains(&action) {
                    item.actions.push(action);
                }
            }
        }
    }
    // The configuration is an object inside the item, made from a Python class.
    if let Some(PropertyValue::Object {
        path: Some(config), ..
    }) = field(fields, "ExtendConfigInline")
    {
        item.config = parsed
            .exports
            .iter()
            .find(|export| export.path.eq_ignore_ascii_case(config))
            .and_then(|export| {
                parsed
                    .imports
                    .iter()
                    .find(|import| import.index == export.class_index)
            })
            .map(|import| import.path.clone());
    }
    Some(item)
}

/// Every gameplay tag `entries` hold, at any depth.
fn tags_in(entries: &[PropertyEntry], out: &mut BTreeSet<String>) {
    fn walk(value: &PropertyValue, out: &mut BTreeSet<String>) {
        match value {
            PropertyValue::Struct { name, fields } if name == "GameplayTag" => {
                if let Some(tag) = field(fields, "TagName").map(PropertyValue::summary)
                    && tag_like(&tag)
                {
                    out.insert(tag);
                }
            }
            PropertyValue::Struct { fields, .. } => tags_in(fields, out),
            PropertyValue::Array { items } | PropertyValue::Set { items } => {
                items.iter().for_each(|item| walk(item, out));
            }
            PropertyValue::Map { entries } => {
                for pair in entries {
                    walk(&pair.key, out);
                    walk(&pair.value, out);
                }
            }
            _ => {}
        }
    }
    for entry in entries {
        walk(&entry.value, out);
    }
}

/// Whether a name reads as a gameplay tag: dotted words, as `Ability.UI.Ultimate`.
fn tag_like(text: &str) -> bool {
    let mut words = text.split('.');
    text.contains('.')
        && words.all(|word| {
            !word.is_empty() && word.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
        && text.chars().next().is_some_and(|c| c.is_ascii_uppercase())
}

/// Whether a call says something about what an ability does: a launch, a spawn, a summon or a
/// timer.
fn notable(callee: &str) -> bool {
    let name = callee
        .rsplit([':', '.'])
        .next()
        .unwrap_or(callee)
        .to_ascii_lowercase();
    name.contains("launchcharacter")
        || name.contains("spawn")
        || name.contains("summon")
        || name.contains("timer")
}

/// The calls and delegates `parsed`'s scripts make, each distinct one once, and the tags they
/// name.
fn calls_in(
    package: &str,
    parsed: &ParsedPackage,
    summary: &mut AbilitySummary,
    tags: &mut BTreeSet<String>,
    seen: &mut BTreeSet<String>,
    all_calls: bool,
) {
    let mut printer: Option<rivals_uasset::ScriptPrinter<'_>> = None;
    for export in &parsed.exports {
        let Some(script) = &export.script else {
            continue;
        };
        let lines = printer
            .get_or_insert_with(|| rivals_uasset::ScriptPrinter::new(parsed))
            .print(export.index)
            .map(|text| text.lines)
            .unwrap_or_default();
        let calls = rivals_uasset::CallTexts::new(script);
        for (at, statement) in script.statements.iter().enumerate() {
            let line = lines.get(at);
            for term in rivals_uasset::statement_terms(&statement.expr) {
                let (text, is_notable) = match term.kind {
                    TermKind::Call => match calls.text(at, &term.text, line) {
                        Some(text) => (text, notable(&term.text)),
                        None => continue,
                    },
                    TermKind::Bind | TermKind::Broadcast => (
                        line.map_or_else(
                            || rivals_uasset::print_expr(&statement.expr),
                            |line| line.text.clone(),
                        ),
                        true,
                    ),
                    TermKind::Name if tag_like(&term.text) => {
                        tags.insert(term.text);
                        continue;
                    }
                    _ => continue,
                };
                if !seen.insert(text.clone()) {
                    continue;
                }
                if !is_notable && !all_calls {
                    summary.other_calls += 1;
                    continue;
                }
                summary.calls.push(SummaryCall {
                    text,
                    package: package.to_string(),
                    export: export.object_name.clone(),
                    export_index: export.index,
                    offset: statement.offset,
                    notable: is_notable,
                });
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    /// A row's key, name and flags read as the table marks them.
    #[test]
    fn a_row_reads_its_key_and_flags() {
        let entry = |name: &str, value| PropertyEntry {
            name: name.into(),
            element: None,
            value,
            span: None,
            slot: None,
        };
        let enumerator = |value: &str| PropertyValue::Enum {
            value: 0,
            name: Some(value.into()),
            enum_type: None,
        };
        let row = ability_row(
            "101451",
            &[
                entry(
                    "TName",
                    PropertyValue::Str {
                        value: "BIG GUN".into(),
                    },
                ),
                entry("ActivateInput", enumerator("EAbilityInput::Q")),
                entry(
                    "AbilityUITags",
                    PropertyValue::Array {
                        items: vec![PropertyValue::Name {
                            value: "Ability.UI.Ultimate".into(),
                        }],
                    },
                ),
            ],
        )
        .expect("a row");
        assert_eq!((row.id, row.key.as_str()), (101451, "Q"));
        assert_eq!(row.flags, ["ultimate"]);
        let team = ability_row(
            "101491",
            &[entry("ActivateInput", enumerator("BondAbility3"))],
        )
        .expect("a row");
        assert_eq!(team.flags, ["team-up"]);
        assert!(ability_row("Name", &[]).is_none());
    }

    /// Dotted words read as a tag; a path or a sentence does not.
    #[test]
    fn a_tag_is_dotted_words() {
        assert!(tag_like("Ability.UI.Ultimate"));
        assert!(tag_like("Summoned.Turret_1"));
        assert!(!tag_like("/Game/X.X"));
        assert!(!tag_like("Hello world. Bye"));
        assert!(!tag_like("lowercase.tag"));
        assert!(notable("/Script/Engine.Character:LaunchCharacter"));
        assert!(notable("K2_SetTimer"));
        assert!(!notable("/Script/Engine.Actor:K2_GetActorLocation"));
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod game_data_tests {
    use super::*;

    fn install() -> Option<(String, std::sync::Arc<Mappings>)> {
        let root = std::env::var("RIVALS_GAME_ROOT").ok()?;
        let usmap = std::env::var("RIVALS_USMAP").ok()?;
        let mappings = crate::mappings::load(std::path::Path::new(&usmap)).expect("mappings");
        Some((root, mappings))
    }

    /// Punisher's turret: cast with E, a method each for firing and getting out, a Python class,
    /// a summon, and a mode's and an event's versions told apart from its own.
    #[test]
    fn the_turret_is_summed_up_from_its_packages() {
        let Some((root, mappings)) = install() else {
            return;
        };
        let summary = ability_summary(&root, &mappings, Some("en"), 101461, false).expect("read");
        let row = summary.row.as_ref().expect("its row");
        assert_eq!(row.key, "E");
        assert_eq!(row.name, "CULLING TURRET");
        let item = summary.item.as_ref().expect("its item");
        let inputs: Vec<(&str, &str)> = item
            .inputs
            .iter()
            .map(|input| (input.method.as_str(), input.key.as_str()))
            .collect();
        assert!(
            inputs.contains(&("TurretFire", "PrimaryAttack")),
            "{inputs:?}"
        );
        assert!(inputs.contains(&("GetOutTurret", "Ability1")), "{inputs:?}");
        assert!(
            item.config
                .as_deref()
                .is_some_and(|config| config.contains("PyAbility_101461"))
        );
        assert!(
            summary
                .summons
                .iter()
                .any(|path| path.contains("Summoned_10146101_BP"))
        );
        let variants: BTreeSet<Variant> = summary.packages.iter().map(|p| p.variant).collect();
        assert!(variants.contains(&Variant::Base), "{variants:?}");
        assert!(variants.contains(&Variant::Event(10130)), "{variants:?}");
        assert!(summary.unreadable.is_empty(), "{:?}", summary.unreadable);
    }

    /// A hero's id lists its abilities with their keys and flags, an ultimate and team-ups
    /// among them.
    #[test]
    fn a_hero_lists_its_abilities() {
        let Some((root, mappings)) = install() else {
            return;
        };
        let hero = hero_abilities(&root, &mappings, Some("en"), 1014).expect("read");
        assert!(
            hero.rows
                .iter()
                .any(|row| row.id == 101461 && row.key == "E")
        );
        assert!(hero.rows.iter().any(|row| row.flags.contains(&"ultimate")));
        assert!(hero.rows.iter().any(|row| row.flags.contains(&"team-up")));
    }
}
