//! Recovers a struct's layout from the package that defines it when the mappings file has no entry.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex};

use rivals_uasset::{
    AssetBundle, ExportStatus, Mappings, MissingSchema, ParseOptions, ParsedPackage, TraceEntry,
};

use crate::asset::{self, AssetSource};

type Cache = Mutex<HashMap<String, Arc<Mappings>>>;
static CACHE: LazyLock<Cache> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Where the package being read came from, so the packages it references can be found too.
pub struct PackageSource<'a> {
    pub game_root: &'a str,
    pub container: &'a str,
    pub entry: &'a str,
    pub kind: AssetSource,
}

/// Parses, and when the mappings file lacks a struct that another package defines, reads the
/// definition from there and parses again.
///
/// Blueprint structs are only in a `.usmap` if that asset happened to be loaded when the mappings
/// were dumped, so a seasonal event can make its own DataTables unreadable until the next dump.
/// Reading the definition out of the game's own files removes that dependency.
pub fn parse_package(
    bundle: &AssetBundle<'_>,
    mappings: Option<&Mappings>,
    source: &PackageSource<'_>,
) -> Result<ParsedPackage, String> {
    parse_package_opts(bundle, mappings, source, ParseOptions::default())
}

/// [`parse_package`] with the caller choosing what the parse records, such as the declared slots
/// an export does not store.
pub fn parse_package_opts(
    bundle: &AssetBundle<'_>,
    mappings: Option<&Mappings>,
    source: &PackageSource<'_>,
    options: ParseOptions,
) -> Result<ParsedPackage, String> {
    // Repairs in order: the package's own definitions first (a class recovered from its package
    // is the class that was cooked), then the mappings file's other entries for a name it holds
    // twice, which the reader tries on its own once synthesis has had its turn.
    let first = rivals_uasset::parse_package_opts(
        bundle,
        mappings,
        None,
        ParseOptions {
            skip_twins: true,
            ..options
        },
    )?;
    let synth = synth_for(&first, mappings, source);
    if synth.is_none() && !has_failures(&first) {
        return Ok(first);
    }
    rivals_uasset::parse_package_opts(bundle, mappings, synth.as_deref(), options)
}

fn has_failures(parsed: &ParsedPackage) -> bool {
    parsed
        .exports
        .iter()
        .any(|export| matches!(export.status, ExportStatus::Failed { .. }))
}

/// The mappings a second parse should carry, or `None` when the first parse needs nothing.
fn synth_for(
    parsed: &ParsedPackage,
    mappings: Option<&Mappings>,
    source: &PackageSource<'_>,
) -> Option<Arc<Mappings>> {
    let (wanted, local) = wanted_definitions(parsed);
    if wanted.is_empty() && !local {
        return None;
    }
    let own = if local {
        rivals_uasset::definitions_of(parsed)
    } else {
        Vec::new()
    };
    synthesise(&wanted, own, mappings, source)
}

/// What a second parse needs the game's own packages for: the structs the mappings file lacks,
/// and the Blueprint classes whose instances did not read to their end, which is what a class
/// revised after the mappings were dumped looks like. The flag says whether such a class is
/// defined in this very package, whose own definitions then join the second parse.
fn wanted_definitions(parsed: &ParsedPackage) -> (Vec<MissingSchema>, bool) {
    let mut wanted: Vec<MissingSchema> = parsed
        .missing_schemas
        .iter()
        .map(|missing| MissingSchema {
            name: missing.name.clone(),
            object_path: missing.object_path.clone(),
        })
        .collect();
    let mut local = false;
    for export in &parsed.exports {
        if matches!(
            export.status,
            ExportStatus::Complete | ExportStatus::Payload { .. }
        ) {
            continue;
        }
        if export.class_index > 0 {
            local |= parsed
                .exports
                .iter()
                .find(|candidate| candidate.index as i32 + 1 == export.class_index)
                .is_some_and(|class| class.struct_definition.is_some());
            continue;
        }
        // Widget, animation and control rig Blueprints each have a generated class of their own.
        let Some(class) = parsed.imports.iter().find(|import| {
            import.index == export.class_index
                && import.class_name.ends_with("BlueprintGeneratedClass")
        }) else {
            continue;
        };
        if wanted
            .iter()
            .any(|missing| missing.object_path == class.path)
        {
            continue;
        }
        wanted.push(MissingSchema {
            name: class.object_name.clone(),
            object_path: class.path.clone(),
        });
    }
    (wanted, local)
}

/// [`parse_package`] that also re-encodes every unversioned header and compares it with the
/// bytes it came from, which is the precondition for writing into the package.
pub fn parse_package_checked(
    bundle: &AssetBundle<'_>,
    mappings: Option<&Mappings>,
    source: &PackageSource<'_>,
) -> Result<ParsedPackage, String> {
    parse_package_opts(
        bundle,
        mappings,
        source,
        ParseOptions {
            check_headers: true,
            ..Default::default()
        },
    )
}

/// [`parse_package`] with the byte range each property consumed, for the byte viewer.
pub fn parse_package_traced(
    bundle: &AssetBundle<'_>,
    mappings: Option<&Mappings>,
    source: &PackageSource<'_>,
) -> Result<(ParsedPackage, Vec<TraceEntry>), String> {
    let first = rivals_uasset::parse_package_opts(
        bundle,
        mappings,
        None,
        ParseOptions {
            skip_twins: true,
            ..Default::default()
        },
    )?;
    let synth = synth_for(&first, mappings, source);
    rivals_uasset::parse_package_traced_with(bundle, mappings, synth.as_deref())
}

/// `own` are definitions the package being parsed carries itself, which need no loading but do
/// make the result specific to that package.
fn synthesise(
    missing: &[MissingSchema],
    own: Vec<rivals_uasset::StructDefinition>,
    mappings: Option<&Mappings>,
    source: &PackageSource<'_>,
) -> Option<Arc<Mappings>> {
    let mut entries: Vec<String> = missing
        .iter()
        .filter_map(|m| defining_entry(&m.object_path, source))
        .collect();
    entries.sort();
    entries.dedup();
    if entries.is_empty() && own.is_empty() {
        return None;
    }

    let scope = if own.is_empty() { "" } else { source.entry };
    let key = format!(
        "{}\u{1}{scope}\u{1}{}",
        source.container,
        entries.join("\u{1}")
    );
    if let Ok(cache) = CACHE.lock()
        && let Some(hit) = cache.get(&key)
    {
        return Some(Arc::clone(hit));
    }

    // A Blueprint class names a Blueprint parent by path, and that parent's package has to join
    // too, up the chain until a native class the mappings know, or the chain never roots at Object.
    let mut definitions = own;
    let mut loaded: HashSet<String> = HashSet::new();
    let mut queue = entries.clone();
    for _ in 0..=MAX_PARENT_DEPTH {
        queue.retain(|entry| !loaded.contains(entry));
        queue.sort();
        queue.dedup();
        for entry in queue.drain(..) {
            loaded.insert(entry.clone());
            let Ok(bundle) =
                asset::load_bundle(source.game_root, source.container, &entry, source.kind)
            else {
                continue;
            };
            let bundle = AssetBundle {
                asset: &bundle.asset_file_buffer,
                exports: &bundle.exports_file_buffer,
            };
            if let Ok(found) = rivals_uasset::read_struct_definitions_pathed(&bundle, mappings) {
                definitions.extend(found);
            }
        }
        let known: HashSet<&str> = definitions.iter().map(|d| d.name()).collect();
        queue = definitions
            .iter()
            .filter_map(|d| d.super_struct())
            .filter(|parent| parent.starts_with('/') && !known.contains(parent))
            .filter_map(|parent| defining_entry(parent, source))
            .collect();
        if queue.is_empty() {
            break;
        }
    }
    if definitions.is_empty() {
        return None;
    }

    let synth = Arc::new(rivals_uasset::mappings_from_definitions_with(
        definitions,
        mappings,
    ));
    if let Ok(mut cache) = CACHE.lock() {
        cache.insert(key, Arc::clone(&synth));
    }
    Some(synth)
}

/// How many Blueprint parents deep a class chain is followed before giving up on rooting it.
const MAX_PARENT_DEPTH: usize = 8;

/// Turns an object path into the package that holds it. UE's dotted form names the package before
/// the dot; a slash-joined path is the package plus one object segment, since a row struct is always
/// the top-level object of its own package. Native types live under `/Script` and have no defining
/// asset. A container is asked by package name, which resolves any mount point, plugins included;
/// a pak or a loose tree only knows the two mount points it can spell.
fn defining_entry(object_path: &str, source: &PackageSource<'_>) -> Option<String> {
    let package = match object_path.split_once('.') {
        Some((package, _)) => package,
        None => object_path.rsplit_once('/').map(|(p, _)| p)?,
    };
    if package.starts_with("/Script/") {
        return None;
    }
    match source.kind {
        AssetSource::Utoc => Some(package.to_string()),
        AssetSource::Loose => {
            sibling_on_disk(source.entry, &crate::asset::mount_relative(package)?)
        }
        AssetSource::Pak => Some(format!("{}.uasset", crate::asset::mount_relative(package)?)),
    }
}

/// For an extracted tree the caller holds a real path, so rebuild the sibling from the content root
/// inside it rather than from a container entry.
fn sibling_on_disk(entry: &str, relative: &str) -> Option<String> {
    let normalised = entry.replace('\\', "/");
    let root = normalised.rfind("Marvel/Content/")?;
    let base = Path::new(&normalised[..root]);
    Some(
        base.join(format!("{relative}.uasset"))
            .to_string_lossy()
            .into_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(kind: AssetSource, entry: &str) -> PackageSource<'_> {
        PackageSource {
            game_root: "",
            container: "c.utoc",
            entry,
            kind,
        }
    }

    #[test]
    fn a_game_object_path_maps_onto_a_pak_entry() {
        let entry = "Marvel/Content/Marvel/Data/DataTable/GameMode/2206/Row.uasset";
        for path in [
            "/Game/Marvel/Data/DataTable/GameMode/2206/Row.Row",
            "/Game/Marvel/Data/DataTable/GameMode/2206/Row/Row",
        ] {
            assert_eq!(
                defining_entry(path, &source(AssetSource::Pak, "x.uasset")),
                Some(entry.to_string()),
                "{path}"
            );
        }
    }

    /// A container resolves the package by name, so a plugin's mount point needs no table.
    #[test]
    fn a_container_is_asked_by_package_name_whatever_the_mount() {
        assert_eq!(
            defining_entry(
                "/MovieRenderPipeline/Blueprints/UI_Row.UI_Row_C",
                &source(AssetSource::Utoc, "x.uasset")
            ),
            Some("/MovieRenderPipeline/Blueprints/UI_Row".to_string())
        );
    }

    /// Engine types have no defining asset to read, so they must not send the loader looking.
    #[test]
    fn a_script_path_has_no_defining_package() {
        let path = "/Script/Marvel.HeroAttributeRow";
        assert_eq!(
            defining_entry(path, &source(AssetSource::Utoc, "x.uasset")),
            None
        );
    }

    #[test]
    fn a_loose_asset_resolves_its_sibling_from_the_content_root() {
        let entry = r"D:\exports\Marvel\Content\Marvel\Data\Table.uasset";
        let path = "/Game/Marvel/Data/Struct/Row.Row";
        assert_eq!(
            defining_entry(path, &source(AssetSource::Loose, entry)),
            Some("D:/exports/Marvel/Content/Marvel/Data/Struct/Row.uasset".to_string())
        );
    }
}

/// Set `RIVALS_GAME_ROOT` and `RIVALS_USMAP` to a real install to run these. Skipped otherwise,
/// because reading a struct definition out of a package needs the packages.
#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod game_data_tests {
    use super::*;
    use crate::mappings;

    const TABLE: &str =
        "Marvel/Content/Marvel/Data/DataTable/GameMode/2206/2206_UIHeroInfoTable.uasset";

    fn install() -> Option<(String, String)> {
        let root = std::env::var("RIVALS_GAME_ROOT").ok()?;
        let usmap = std::env::var("RIVALS_USMAP").ok()?;
        Some((root, usmap))
    }

    /// `MarvelM2206UIHeroAsset` is a Blueprint struct that this game's mappings dumps have never
    /// contained, so the row layout has to come from the package that defines it. Reading it wrong
    /// would not merely mislabel fields: the rows would stop landing on the export's declared size.
    #[test]
    fn a_row_struct_absent_from_the_mappings_is_read_from_its_own_package() {
        let Some((root, usmap)) = install() else {
            return;
        };
        let container = format!(
            "{}/MarvelGame/Marvel/Content/Paks/pakchunk0-Windows.utoc",
            root.replace('\\', "/")
        );
        let schema = mappings::load(std::path::Path::new(&usmap)).expect("mappings");
        let bundle =
            asset::load_bundle(&root, &container, TABLE, AssetSource::Utoc).expect("load table");
        let parsed = parse_package(
            &AssetBundle {
                asset: &bundle.asset_file_buffer,
                exports: &bundle.exports_file_buffer,
            },
            Some(&schema),
            &PackageSource {
                game_root: &root,
                container: &container,
                entry: TABLE,
                kind: AssetSource::Utoc,
            },
        )
        .expect("parse");

        let export = parsed.exports.first().expect("one export");
        assert!(
            matches!(export.status, rivals_uasset::ExportStatus::Complete),
            "expected an exact parse, got {:?}",
            export.status
        );
        let table = export.data_table.as_ref().expect("data table");
        assert_eq!(table.row_struct, "MarvelM2206UIHeroAsset");
        // The shipped row and column counts move with the game; that the synthesised struct
        // supplies a cell for every column of every row is what the test is for.
        assert!(!table.rows.is_empty());
        assert!(!table.columns.is_empty());
        for row in &table.rows {
            assert_eq!(row.fields.len(), table.columns.len(), "{}", row.name);
        }
    }
}
