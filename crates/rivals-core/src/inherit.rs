//! The values an export does not store, as the archetype it was made from holds them.
//!
//! UE writes an object's properties as differences from its archetype: a placed actor from its
//! Blueprint's default object, a component from the template its class holds, and each of those
//! from its own archetype in turn. A property the object leaves out takes the nearest archetype's
//! value; one inside a stored struct takes that archetype's value at the same field. Native
//! classes keep their defaults in code, so a chain that reaches `/Script/` ends there.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use rivals_uasset::{
    AssetBundle, Mappings, ParseOptions, ParsedPackage, PropertyEntry, PropertyValue,
};
use serde::Serialize;

use crate::asset::{self, AssetSource};
use crate::schema_synth::{self, PackageSource};

/// How many archetypes a chain follows before giving up on the rest.
const MAX_CHAIN: usize = 8;
/// How many packages one request may read, beyond those already cached.
const MAX_LOADS: usize = 64;
/// How many parsed archetype packages are kept between requests.
const CACHED_PACKAGES: usize = 256;

/// A value an export does not store, as an archetype holds it.
#[derive(Debug, Clone, Serialize)]
pub struct Inherited {
    /// From the export down to the value: field names as a field set writes them.
    pub path: Vec<String>,
    pub value: PropertyValue,
    /// The archetype holding it, by object path.
    pub from: String,
}

/// What a lookup found, and why it stopped where it did.
#[derive(Debug, Default, Serialize)]
pub struct InheritReport {
    pub values: Vec<Inherited>,
    /// Why the values still missing have none here: a native class, or an archetype that could
    /// not be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stopped: Option<String>,
    /// How many packages this lookup read rather than found cached.
    pub loaded: usize,
}

/// Where the archetypes' packages are read from: the store the viewed package's own container
/// opens, so a mod's own Blueprints are the ones its objects copy.
pub struct ArchetypeSource<'a> {
    pub game_root: &'a str,
    pub container: &'a str,
    pub mappings: Option<&'a Mappings>,
}

impl ArchetypeSource<'_> {
    fn container(&self) -> String {
        if self.container.to_ascii_lowercase().ends_with(".utoc") {
            self.container.to_string()
        } else {
            "pakchunk0-Windows.utoc".to_string()
        }
    }

    /// The package `name` names, parsed for its stored values, from the cache when it holds it.
    fn package(&self, name: &str, loaded: &mut usize) -> Result<Arc<ParsedPackage>, String> {
        static CACHE: OnceLock<Mutex<HashMap<String, Arc<ParsedPackage>>>> = OnceLock::new();
        let container = self.container();
        let key = format!("{}\u{1}{container}\u{1}{name}", self.game_root);
        let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        if let Some(found) = cache.lock().map_err(|e| e.to_string())?.get(&key) {
            return Ok(found.clone());
        }
        if *loaded >= MAX_LOADS {
            return Err(format!("this lookup has read {MAX_LOADS} packages already"));
        }
        *loaded += 1;
        let bundle = asset::load_bundle(self.game_root, &container, name, AssetSource::Utoc)?;
        let parsed = schema_synth::parse_package_opts(
            &AssetBundle {
                asset: &bundle.asset_file_buffer,
                exports: &bundle.exports_file_buffer,
            },
            self.mappings,
            &PackageSource {
                game_root: self.game_root,
                container: &container,
                entry: name,
                kind: AssetSource::Utoc,
            },
            ParseOptions::default(),
        )?;
        let parsed = Arc::new(parsed);
        let mut cache = cache.lock().map_err(|e| e.to_string())?;
        if cache.len() >= CACHED_PACKAGES {
            cache.clear();
        }
        cache.insert(key, parsed.clone());
        Ok(parsed)
    }
}

/// The values export `export` of `parsed` does not store, from its archetypes.
pub fn inherited_for(
    parsed: &ParsedPackage,
    export: u32,
    source: &ArchetypeSource<'_>,
) -> Result<InheritReport, String> {
    let object = parsed
        .exports
        .iter()
        .find(|candidate| candidate.index == export)
        .ok_or_else(|| format!("no export {export}"))?;
    let mut wanted = Vec::new();
    unstored_paths(&object.properties, &[], &mut wanted);
    let mut report = InheritReport::default();

    // The package the chain has reached, `None` while it is still in the viewed one.
    let mut holder: Option<Arc<ParsedPackage>> = None;
    let mut template = object.template_index;
    for _ in 0..MAX_CHAIN {
        if wanted.is_empty() {
            break;
        }
        let package = holder.as_deref().unwrap_or(parsed);
        let (next, index) = match resolve(package, template, source, &mut report.loaded) {
            Ok(Some(found)) => found,
            Ok(None) => break,
            Err(reason) => {
                report.stopped = Some(reason);
                break;
            }
        };
        if next.is_some() {
            holder = next;
        }
        let Some(held) = holder.as_deref().unwrap_or(parsed).exports.get(index) else {
            break;
        };
        let (found, missing) = take_inherited(&held.properties, wanted);
        report
            .values
            .extend(found.into_iter().map(|(path, value)| Inherited {
                path,
                value,
                from: held.path.clone(),
            }));
        wanted = missing;
        template = held.template_index;
    }
    Ok(report)
}

/// The archetype a template index names: the package holding it, `None` for the one it was named
/// in, and its position there. `None` altogether when there is none; an error says why a chain
/// cannot go on.
type Found = Option<(Option<Arc<ParsedPackage>>, usize)>;

fn resolve(
    package: &ParsedPackage,
    template: i32,
    source: &ArchetypeSource<'_>,
    loaded: &mut usize,
) -> Result<Found, String> {
    if template == 0 {
        return Ok(None);
    }
    if template > 0 {
        return Ok(Some((None, template as usize - 1)));
    }
    let import = package
        .imports
        .iter()
        .find(|import| import.index == template)
        .ok_or_else(|| format!("import {template} is not in the table"))?;
    if import.unresolved {
        return Err(format!("the archetype {} could not be named", import.path));
    }
    if import.path.starts_with("/Script/") {
        return Err(format!(
            "{} is native, so the values it gives are in the game's code",
            import.path
        ));
    }
    let name = import.path.split('.').next().unwrap_or(&import.path);
    let holder = source
        .package(name, loaded)
        .map_err(|reason| format!("{name} could not be read: {reason}"))?;
    let index = holder
        .exports
        .iter()
        .position(|export| export.path == import.path)
        .ok_or_else(|| format!("{} is not in its package", import.path))?;
    Ok(Some((Some(holder), index)))
}

/// A field set's segment for an entry: its name, and its slot for a static array.
fn segment(entry: &PropertyEntry) -> String {
    match entry.element {
        Some(at) => format!("{}[{at}]", entry.name),
        None => entry.name.clone(),
    }
}

/// Every path under `entries` that stores nothing, looking inside stored structs, which UE writes
/// as differences from the archetype's struct. Containers are written whole and zero values are
/// values, so neither is looked into.
fn unstored_paths(entries: &[PropertyEntry], prefix: &[String], out: &mut Vec<Vec<String>>) {
    for entry in entries {
        let mut path = prefix.to_vec();
        path.push(segment(entry));
        match &entry.value {
            PropertyValue::Unset { .. } => out.push(path),
            PropertyValue::Struct { fields, .. } => unstored_paths(fields, &path, out),
            _ => {}
        }
    }
}

/// Paths from an export, each as the segments a field set names.
type Paths = Vec<Vec<String>>;

/// Splits `wanted` into the paths `entries` store a value at, with those values, and the paths it
/// leaves for an archetype further up.
fn take_inherited(
    entries: &[PropertyEntry],
    wanted: Paths,
) -> (Vec<(Vec<String>, PropertyValue)>, Paths) {
    let mut found = Vec::new();
    let mut missing = Vec::new();
    for path in wanted {
        match value_at(entries, &path) {
            Some(value) => found.push((path, value.clone())),
            None => missing.push(path),
        }
    }
    (found, missing)
}

/// The value stored at `path` under `entries`, through stored structs; `None` where anything on
/// the way stores nothing.
fn value_at<'a>(entries: &'a [PropertyEntry], path: &[String]) -> Option<&'a PropertyValue> {
    let (first, rest) = path.split_first()?;
    let entry = entries.iter().find(|entry| segment(entry) == *first)?;
    match (&entry.value, rest.is_empty()) {
        (PropertyValue::Unset { .. }, _) => None,
        (value, true) => Some(value),
        (PropertyValue::Struct { fields, .. }, false) => value_at(fields, rest),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn entry(name: &str, value: PropertyValue) -> PropertyEntry {
        PropertyEntry {
            name: name.into(),
            element: None,
            value,
            span: None,
            slot: None,
        }
    }

    fn unset() -> PropertyValue {
        PropertyValue::Unset {
            declared: "Int",
            enum_type: None,
            fields: Vec::new(),
        }
    }

    fn int(value: i64) -> PropertyValue {
        PropertyValue::Int { value }
    }

    fn holder(fields: Vec<PropertyEntry>) -> PropertyValue {
        PropertyValue::Struct {
            name: "Holder".into(),
            fields,
        }
    }

    fn path(segments: &[&str]) -> Vec<String> {
        segments.iter().map(|s| s.to_string()).collect()
    }

    /// An object's unstored properties are wanted, and so are the unstored fields of a struct it
    /// stores; a zero value is a value.
    #[test]
    fn unstored_properties_and_fields_are_what_is_looked_up() {
        let object = [
            entry("Health", unset()),
            entry("Speed", int(3)),
            entry("Pos", holder(vec![entry("X", int(1)), entry("Y", unset())])),
            entry(
                "Zero",
                PropertyValue::Default {
                    declared: None,
                    fields: Vec::new(),
                },
            ),
        ];
        let mut wanted = Vec::new();
        unstored_paths(&object, &[], &mut wanted);
        assert_eq!(wanted, [path(&["Health"]), path(&["Pos", "Y"])]);
    }

    /// An archetype gives the values it stores, through its own stored structs, and leaves the
    /// rest for the archetype after it.
    #[test]
    fn an_archetype_gives_what_it_stores_and_leaves_the_rest() {
        let archetype = [
            entry("Health", int(100)),
            entry("Pos", holder(vec![entry("X", int(9)), entry("Y", unset())])),
        ];
        let (found, missing) = take_inherited(
            &archetype,
            vec![path(&["Health"]), path(&["Pos", "Y"]), path(&["Armor"])],
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, path(&["Health"]));
        assert!(matches!(found[0].1, PropertyValue::Int { value: 100 }));
        assert_eq!(missing, [path(&["Pos", "Y"]), path(&["Armor"])]);
    }

    /// The game's install and mappings, when the environment names them.
    fn game() -> Option<(String, Arc<Mappings>)> {
        let root = std::env::var("RIVALS_GAME_ROOT").ok()?;
        let usmap = std::env::var("RIVALS_USMAP").ok()?;
        let mappings = crate::mappings::load(std::path::Path::new(&usmap)).expect("mappings");
        Some((root, mappings))
    }

    fn read(root: &str, entry: &str, mappings: &Mappings, declared: bool) -> ParsedPackage {
        let container = "pakchunk0-Windows.utoc";
        let bundle = asset::load_bundle(root, container, entry, AssetSource::Utoc).expect("load");
        schema_synth::parse_package_opts(
            &AssetBundle {
                asset: &bundle.asset_file_buffer,
                exports: &bundle.exports_file_buffer,
            },
            Some(mappings),
            &PackageSource {
                game_root: root,
                container,
                entry,
                kind: AssetSource::Utoc,
            },
            ParseOptions {
                declared_slots: declared,
                ..Default::default()
            },
        )
        .expect("parse")
    }

    const TERMINAL: &str = "Marvel/Content/Marvel/Blueprints/LevelGameplay/Activity/10151/M2201BallGameTerminalBP.uasset";
    const SCOPE_CHECK: &str =
        "/Game/Marvel/Blueprints/LevelGameplay/Components/LevelScopeCheckComponentBP";

    /// A component template inherits what it leaves out from its Blueprint's default object in
    /// another package, value for value as that package reads; the chain stops at native code and
    /// says so, and a second lookup reads nothing again.
    #[test]
    fn a_component_template_inherits_from_its_blueprint_s_default_object() {
        let Some((root, mappings)) = game() else {
            return;
        };
        let parsed = read(&root, TERMINAL, &mappings, true);
        let export = parsed
            .exports
            .iter()
            .find(|e| e.object_name == "LevelScopeCheckComponentBP_GEN_VARIABLE")
            .expect("the component template")
            .index;
        let source = ArchetypeSource {
            game_root: &root,
            container: "pakchunk0-Windows.utoc",
            mappings: Some(&mappings),
        };
        let report = inherited_for(&parsed, export, &source).expect("lookup");
        assert!(!report.values.is_empty(), "{report:?}");
        assert!(
            report
                .stopped
                .as_deref()
                .is_some_and(|why| why.contains("/Script/")),
            "{:?}",
            report.stopped
        );

        let archetypes = read(&root, SCOPE_CHECK, &mappings, false);
        let from_default = report
            .values
            .iter()
            .find(|value| value.from.starts_with(SCOPE_CHECK))
            .expect("a value from the Blueprint's default object");
        let archetype = archetypes
            .exports
            .iter()
            .find(|e| e.path == from_default.from)
            .expect("the archetype");
        let held = value_at(&archetype.properties, &from_default.path).expect("stored there");
        assert_eq!(held.summary(), from_default.value.summary());

        let again = inherited_for(&parsed, export, &source).expect("lookup");
        assert_eq!(again.loaded, 0, "the archetypes are cached");
        assert_eq!(again.values.len(), report.values.len());
    }
}
