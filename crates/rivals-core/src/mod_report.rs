//! Summarises what an IoStore mod ships: which packages replace the game's, what natives it calls
//! and what files, save slots and links its scripts reach for at runtime.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use retoc::{EIoChunkType, FIoChunkId, FPackageId};
use rivals_uasset::{
    AssetBundle, DataTableRow, Expr, Mappings, ParseOptions, ParsedExport, ParsedPackage,
    PropertyValue, dotted_path,
};
use serde::Serialize;

use crate::asset::{AssetSource, PackageConverter, list_packages};
use crate::pak::containers::open_base_game_paks;
use crate::schema_synth::{self, LayoutReader, PackageSource};

/// Extensions a script string has to end in to count as a file the mod reads or writes.
const FILE_EXTENSIONS: &[&str] = &[
    "txt", "json", "ini", "cfg", "csv", "log", "sav", "pak", "utoc", "ucas",
];

/// Save game calls whose presence means the package keeps state in a slot.
const SAVE_CALLS: &[&str] = &[
    "GameplayStatics:SaveGameToSlot",
    "GameplayStatics:LoadGameFromSlot",
    "GameplayStatics:DoesSaveGameExist",
    "GameplayStatics:DeleteGameInSlot",
];

#[derive(Debug, Clone, Serialize)]
pub struct ModReport {
    pub container: String,
    pub patch_priority: Option<u32>,
    pub packages: Vec<PackageReport>,
    /// Native objects the game registers only at runtime, keyed by the package that calls them.
    pub runtime_natives: BTreeMap<String, Vec<String>>,
    /// Classes backed by the game's embedded Python, keyed by the package that references them.
    pub python_classes: BTreeMap<String, Vec<String>>,
    /// Loose files a script names, keyed by the package that names them.
    pub files: BTreeMap<String, Vec<String>>,
    /// Save slots a package keeps state in, keyed by the package.
    pub save_slots: BTreeMap<String, Vec<String>>,
    pub urls: BTreeMap<String, Vec<String>>,
    /// The combined tables the mod ships, each against the rows its parent tables merge to.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub composites: Vec<CompositeCheck>,
}

/// A combined table checked against its parents. The game is expected to rebuild such a table from
/// its parents when it loads, so the rows it stores matter only where they disagree with them: a
/// row no parent has, or a cell a parent holds otherwise, is an edit that would not show.
#[derive(Debug, Clone, Serialize)]
pub struct CompositeCheck {
    /// The table's object path.
    pub table: String,
    /// Its parent tables' object paths, in the order they merge.
    pub parents: Vec<String>,
    /// How many rows it stores, and how many its parents merge to.
    pub stored: usize,
    pub merged: usize,
    /// The rows it stores that no parent has.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extra: Vec<String>,
    /// The cells of the rows it stores that its parents hold otherwise: `game` is the parent's.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub differing: Vec<crate::value_diff::ValueChange>,
    /// Why its parents could not be read, when they could not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unreadable: Option<String>,
}

impl CompositeCheck {
    /// What disagrees with the parents, one line each.
    pub fn warnings(&self) -> Vec<String> {
        const REBUILT: &str = "the game is expected to rebuild the table from its parents";
        let mut out = Vec::new();
        if !self.extra.is_empty() {
            out.push(format!(
                "stores {} row(s) no parent has, {REBUILT}, so add them to a parent: {}",
                self.extra.len(),
                self.extra.join(", ")
            ));
        }
        for change in &self.differing {
            let row = change.row.as_deref().unwrap_or_default();
            out.push(format!(
                "{row}.{} reads {} here and {} in its parent, {REBUILT}, so edit the parent's row",
                change.path, change.ours, change.game
            ));
        }
        out
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PackageReport {
    pub path: String,
    /// What the package's asset is: Blueprint, Widget Blueprint, DataTable, Texture2D and so on.
    pub kind: String,
    /// The base game ships a package at the same path, so this one replaces it.
    pub overrides_game: bool,
    /// Why the package's scripts and values could not be read, when they could not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The imports nothing in the package names, load-order edges included, that can be dropped.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unused_imports: Vec<String>,
    /// The names nothing in the package uses.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unused_names: Vec<String>,
    /// Why the package's unused names cannot be known, when they cannot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub names_unknown: Option<String>,
    /// Objects the package names that no load-order edge has the loader create first, so they can
    /// read as nothing once it loads.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unwaited: Vec<String>,
    /// Load-order edges to an export of the package that nothing in the waiting export names, as a
    /// link saved against the wrong export leaves them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub stray_edges: Vec<String>,
}

/// Reads every package `utoc_path` ships. Unversioned packages need `mappings` for their scripts
/// and values; without it they still report their kind, whether they override the game, and the
/// natives they import.
pub fn mod_report(
    game_root: &str,
    utoc_path: &Path,
    mappings: Option<&Mappings>,
) -> Result<ModReport, String> {
    let utoc = utoc_path.to_string_lossy();
    let (store, packages) = list_packages(game_root, &utoc)?;
    let own = crate::pak::containers::container_stem(utoc_path).unwrap_or_default();
    let converter = PackageConverter::new(store.as_ref());
    // The store holds the mod and the base game, which is where its Blueprints' layouts come from.
    let layouts = LayoutReader::through(store.as_ref(), &converter);
    let runtime_natives: BTreeSet<String> = crate::script_objects::current().into_iter().collect();

    let mut report = ModReport {
        container: own.clone(),
        patch_priority: crate::mods::pak_priority(&own),
        packages: Vec::new(),
        runtime_natives: BTreeMap::new(),
        python_classes: BTreeMap::new(),
        files: BTreeMap::new(),
        save_slots: BTreeMap::new(),
        urls: BTreeMap::new(),
        composites: Vec::new(),
    };
    // Each combined table the mod ships, its parents and the rows it stores, checked once every
    // package is read.
    let mut composites: Vec<(String, Vec<String>, Vec<DataTableRow>)> = Vec::new();
    for (id, path) in &packages {
        let chunk = FIoChunkId::from_package_id(*id, 0, EIoChunkType::ExportBundleData);
        let overrides_game = store
            .child_containers()
            .any(|c| c.container_name() != own && c.has_chunk_id(chunk));
        let bundle = match converter.convert(*id, path) {
            Ok(bundle) => bundle,
            Err(error) => {
                report.packages.push(PackageReport {
                    path: path.clone(),
                    kind: "?".into(),
                    overrides_game,
                    error: Some(error),
                    unused_imports: Vec::new(),
                    unused_names: Vec::new(),
                    names_unknown: None,
                    unwaited: Vec::new(),
                    stray_edges: Vec::new(),
                });
                continue;
            }
        };
        let bundle = AssetBundle {
            asset: &bundle.asset_file_buffer,
            exports: &bundle.exports_file_buffer,
        };
        let header = rivals_uasset::read_header(&bundle)?;
        let imports: Vec<String> = (0..header.imports.len())
            .filter_map(|at| {
                dotted_path(&header, retoc::zen::FPackageIndex::create_import(at as u32))
            })
            .collect();
        let natives = imports
            .iter()
            .filter(|p| runtime_natives.contains(*p) && p.contains(':'))
            .cloned();
        add(&mut report.runtime_natives, path, natives);
        let python = imports.iter().filter(|p| is_python_class(p)).cloned();
        add(&mut report.python_classes, path, python);

        let mut error = None;
        let mut unused_imports = Vec::new();
        let mut unused_names = Vec::new();
        let mut names_unknown = None;
        let mut unwaited = Vec::new();
        let mut stray_edges = Vec::new();
        let stem = Path::new(path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let mut kind = kind_of(&header, stem);
        match schema_synth::parse_package_through(
            &bundle,
            mappings,
            &PackageSource {
                game_root,
                container: &utoc,
                entry: path,
                kind: AssetSource::Utoc,
                mod_container: None,
            },
            &layouts,
            ParseOptions::default(),
        ) {
            Ok(parsed) => {
                unused_imports = rivals_uasset::unused_imports(&parsed, &header)
                    .into_iter()
                    .filter(|import| import.blocked.is_none())
                    .map(|import| import.path)
                    .collect();
                match rivals_uasset::header_size(&bundle).and_then(|total| {
                    rivals_uasset::unused_names(&parsed, &header, bundle.exports, total)
                }) {
                    Ok(names) => unused_names = names,
                    Err(reason) => names_unknown = Some(reason),
                }
                for finding in rivals_uasset::edge_findings(&parsed) {
                    let line = finding.render(&parsed);
                    match finding {
                        rivals_uasset::EdgeFinding::Unwaited { .. } => unwaited.push(line),
                        rivals_uasset::EdgeFinding::Unnamed { .. } => stray_edges.push(line),
                    }
                }
                for export in parsed.exports.iter().filter(|e| is_composite(e)) {
                    if let Some(table) = &export.data_table {
                        composites.push((
                            export.path.clone(),
                            parent_tables(export),
                            table.rows.clone(),
                        ));
                    }
                }
                let mut strings = Vec::new();
                for export in &parsed.exports {
                    if let Some(script) = &export.script {
                        for statement in &script.statements {
                            string_literals(&statement.expr, &mut strings);
                        }
                    }
                }
                let slots = if imports
                    .iter()
                    .any(|p| SAVE_CALLS.iter().any(|call| p.ends_with(call)))
                {
                    save_slot_defaults(&parsed)
                } else {
                    Vec::new()
                };
                add(
                    &mut report.files,
                    path,
                    strings.iter().filter(|s| is_file(s)).cloned(),
                );
                add(
                    &mut report.urls,
                    path,
                    strings.iter().filter(|s| is_url(s)).cloned(),
                );
                add(&mut report.save_slots, path, slots.into_iter());
                if kind.is_empty() {
                    kind = parsed
                        .exports
                        .first()
                        .map(|e| e.class_name.clone())
                        .unwrap_or_default();
                }
            }
            Err(reason) => error = Some(reason),
        }
        report.packages.push(PackageReport {
            path: path.clone(),
            kind: friendly_kind(&kind),
            overrides_game,
            error,
            unused_imports,
            unused_names,
            names_unknown,
            unwaited,
            stray_edges,
        });
    }
    report.packages.sort_by(|a, b| a.path.cmp(&b.path));

    // A parent is read where the game would find it: the mod's copy, or the game's.
    let read = |package: &str| -> Result<ParsedPackage, String> {
        let id = FPackageId::from_name(package);
        let chunk = FIoChunkId::from_package_id(id, 0, EIoChunkType::ExportBundleData);
        let entry = store
            .chunk_path(chunk)
            .map(|path| crate::pak::profile::strip_mount_prefix(&path))
            .ok_or_else(|| format!("{package} is not in the mod or the game"))?;
        let bundle = converter.convert(id, &entry)?;
        schema_synth::parse_package_through(
            &AssetBundle {
                asset: &bundle.asset_file_buffer,
                exports: &bundle.exports_file_buffer,
            },
            mappings,
            &PackageSource {
                game_root,
                container: &utoc,
                entry: &entry,
                kind: AssetSource::Utoc,
                mod_container: None,
            },
            &layouts,
            ParseOptions::default(),
        )
    };
    for (table, parents, stored) in composites {
        let mut check = CompositeCheck {
            table: table.clone(),
            parents: parents.clone(),
            stored: stored.len(),
            merged: 0,
            extra: Vec::new(),
            differing: Vec::new(),
            unreadable: None,
        };
        match merged_rows(&parents, 0, &read) {
            Ok(merged) => {
                check.merged = merged.len();
                (check.extra, check.differing) = against_parents(&table, &merged, &stored);
            }
            Err(reason) => check.unreadable = Some(reason),
        }
        report.composites.push(check);
    }
    Ok(report)
}

fn is_composite(export: &ParsedExport) -> bool {
    export.class_name == "CompositeDataTable"
}

/// The object paths a combined table names as its parents, in order.
fn parent_tables(export: &ParsedExport) -> Vec<String> {
    export
        .properties
        .iter()
        .filter(|entry| entry.name == "ParentTables")
        .flat_map(|entry| match &entry.value {
            PropertyValue::Array { items } => items.as_slice(),
            _ => &[],
        })
        .filter_map(|item| match item {
            PropertyValue::Object {
                path: Some(path), ..
            } => Some(path.clone()),
            _ => None,
        })
        .collect()
}

/// The rows `parents` merge to, as the engine merges them: in order, a later parent's row taking
/// the place of an earlier one of the same name. A parent that is itself a combined table gives the
/// rows its own parents merge to.
fn merged_rows(
    parents: &[String],
    depth: usize,
    read: &dyn Fn(&str) -> Result<ParsedPackage, String>,
) -> Result<Vec<DataTableRow>, String> {
    if depth > 8 {
        return Err("its parent tables nest too deeply to follow".into());
    }
    let mut rows: Vec<DataTableRow> = Vec::new();
    for parent in parents {
        let package = parent.split_once('.').map_or(parent.as_str(), |(p, _)| p);
        let parsed = read(package)?;
        let export = parsed
            .exports
            .iter()
            .find(|export| export.path.eq_ignore_ascii_case(parent))
            .ok_or_else(|| format!("{package} holds no {parent}"))?;
        let own = if is_composite(export) {
            merged_rows(&parent_tables(export), depth + 1, read)?
        } else {
            export
                .data_table
                .as_ref()
                .map(|table| table.rows.clone())
                .ok_or_else(|| format!("{parent} is not a table"))?
        };
        merge_rows(&mut rows, own);
    }
    Ok(rows)
}

/// Merges a parent's rows into those merged so far: one of a name already there takes its place,
/// whatever its case, and a new one goes last.
fn merge_rows(rows: &mut Vec<DataTableRow>, own: Vec<DataTableRow>) {
    for row in own {
        match rows
            .iter_mut()
            .find(|held| held.name.eq_ignore_ascii_case(&row.name))
        {
            Some(held) => *held = row,
            None => rows.push(row),
        }
    }
}

/// The rows a combined table stores that its parents do not have, and the cells of the others its
/// parents hold otherwise.
fn against_parents(
    table: &str,
    merged: &[DataTableRow],
    stored: &[DataTableRow],
) -> (Vec<String>, Vec<crate::value_diff::ValueChange>) {
    let mut extra = Vec::new();
    let mut differing = Vec::new();
    for change in crate::value_diff::row_changes(table, merged, stored) {
        if change.path.is_empty() && change.game == "(no row)" {
            extra.extend(change.row);
        } else {
            differing.push(change);
        }
    }
    (extra, differing)
}

/// Which of `assets` the base game also ships. Each is a path below a project's `Content` folder,
/// such as `marvel/ui/foo.uasset`, which is where `/Game/` points; case does not matter.
pub fn shipped_by_game<'a>(
    game_root: &str,
    assets: impl IntoIterator<Item = &'a str>,
) -> Result<HashSet<String>, String> {
    let store = open_base_game_paks(&crate::paths::paks_dir(game_root), "")?;
    Ok(assets
        .into_iter()
        .filter(|asset| {
            let Some(stem) = asset
                .strip_suffix(".uasset")
                .or_else(|| asset.strip_suffix(".umap"))
            else {
                return false;
            };
            let id = FPackageId::from_name(&format!("/Game/{stem}"));
            store.has_chunk_id(FIoChunkId::from_package_id(
                id,
                0,
                EIoChunkType::ExportBundleData,
            ))
        })
        .map(str::to_string)
        .collect())
}

fn add(
    into: &mut BTreeMap<String, Vec<String>>,
    package: &str,
    values: impl Iterator<Item = String>,
) {
    let set: BTreeSet<String> = values.collect();
    if !set.is_empty() {
        into.insert(package.to_string(), set.into_iter().collect());
    }
}

/// The class of the export the package is named for: `Name_C` for a Blueprint, `Name` for any
/// other asset. Widget trees flag their own subobjects as assets, so the flag alone misleads.
pub(crate) fn kind_of(header: &retoc::legacy_asset::FLegacyPackageHeader, stem: &str) -> String {
    let named = |wanted: &str| {
        header.exports.iter().find(|e| {
            header
                .name_map
                .get(e.object_name)
                .is_ok_and(|name| name == wanted)
        })
    };
    let asset = named(&format!("{stem}_C"))
        .or_else(|| named(stem))
        .or_else(|| header.exports.iter().find(|e| e.is_asset))
        .or_else(|| header.exports.first());
    asset
        .and_then(|e| dotted_path(header, e.class_index))
        .map(|p| {
            p.rsplit(['.', ':', '/'])
                .next()
                .unwrap_or_default()
                .to_string()
        })
        .unwrap_or_default()
}

pub(crate) fn friendly_kind(class: &str) -> String {
    match class {
        "BlueprintGeneratedClass" => "Blueprint",
        "WidgetBlueprintGeneratedClass" => "Widget Blueprint",
        "AnimBlueprintGeneratedClass" => "Anim Blueprint",
        "UserDefinedStruct" => "Struct",
        "UserDefinedEnum" => "Enum",
        other => other,
    }
    .to_string()
}

/// Every string constant a statement carries, however deep in its calls.
fn string_literals(expr: &Expr, out: &mut Vec<String>) {
    for literal in rivals_uasset::literals(expr) {
        match literal {
            Expr::StringConst { value, .. } | Expr::UnicodeStringConst { value, .. } => {
                out.push(value.clone());
            }
            _ => {}
        }
    }
}

/// A slot is usually kept in a string variable, so its default on the class default object is
/// what names it.
fn save_slot_defaults(parsed: &rivals_uasset::ParsedPackage) -> Vec<String> {
    parsed
        .exports
        .iter()
        .filter(|e| e.object_name.starts_with("Default__"))
        .flat_map(|e| &e.properties)
        .filter(|p| p.name.to_lowercase().contains("slot"))
        .filter_map(|p| match &p.value {
            PropertyValue::Str { value } if !value.is_empty() => Some(value.clone()),
            _ => None,
        })
        .collect()
}

fn is_url(text: &str) -> bool {
    text.starts_with("http://") || text.starts_with("https://")
}

fn is_file(text: &str) -> bool {
    if text.is_empty() || text.contains(char::is_whitespace) || is_url(text) {
        return false;
    }
    Path::new(text)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| FILE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Whether an import path names a class the game's embedded Python defines, rather than its
/// module, a default object or something inside one.
pub(crate) fn is_python_class(path: &str) -> bool {
    path.starts_with("/Script/UnrealEnginePython/")
        && !path.contains(':')
        && path.contains('.')
        && !path.contains(".Default__")
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn row(name: &str, value: i64) -> DataTableRow {
        DataTableRow {
            name: name.into(),
            fields: vec![rivals_uasset::PropertyEntry {
                name: "Order".into(),
                element: None,
                value: PropertyValue::Int { value },
                span: None,
                slot: None,
            }],
        }
    }

    /// Parents merge in order: a later parent's row takes the place of an earlier one of the same
    /// name, whatever its case, and a new one goes last.
    #[test]
    fn a_later_parent_s_row_takes_the_place_of_an_earlier_one() {
        let mut rows = Vec::new();
        merge_rows(&mut rows, vec![row("Title", 1), row("Graphics", 2)]);
        merge_rows(&mut rows, vec![row("graphics", 5), row("Mods", 3)]);
        let merged: Vec<(&str, String)> = rows
            .iter()
            .map(|row| (row.name.as_str(), row.fields[0].value.summary()))
            .collect();
        assert_eq!(
            merged,
            vec![
                ("Title", "1".into()),
                ("graphics", "5".into()),
                ("Mods", "3".into())
            ]
        );
    }

    /// A combined table storing a few of its parents' rows as they are is in step; a row no parent
    /// has, or a cell a parent holds otherwise, is named.
    #[test]
    fn a_combined_table_is_held_to_its_parents() {
        let merged = vec![row("Title", 1), row("Mods", 3), row("Graphics", 2)];
        let (extra, differing) = against_parents("/Game/C.C", &merged, &[row("Mods", 3)]);
        assert!(extra.is_empty() && differing.is_empty());
        let (extra, differing) =
            against_parents("/Game/C.C", &merged, &[row("Mods", 4), row("Sound", 1)]);
        assert_eq!(extra, vec!["Sound".to_string()]);
        assert_eq!(differing.len(), 1);
        assert_eq!(differing[0].row.as_deref(), Some("Mods"));
        assert_eq!(
            (differing[0].game.as_str(), differing[0].ours.as_str()),
            ("3", "4")
        );
        let check = CompositeCheck {
            table: "/Game/C.C".into(),
            parents: Vec::new(),
            stored: 2,
            merged: 3,
            extra,
            differing,
            unreadable: None,
        };
        let warnings = check.warnings();
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings[0].contains("Sound"), "{warnings:?}");
        assert!(
            warnings[1].starts_with("Mods.Order reads 4 here and 3 in its parent"),
            "{warnings:?}"
        );
    }

    #[test]
    fn a_file_is_a_bare_path_with_a_known_extension() {
        assert!(is_file("Galacta_SwapperKey.txt"));
        assert!(is_file("Marvel/ProjectGalacta/skins.txt"));
        assert!(is_file("GalactaMods/classicWhitefoxmesh_9999999_P.pak"));
        assert!(!is_file("Project Galacta – All mods loaded!"));
        assert!(!is_file("Content/Paks/"));
        assert!(!is_file("https://example.com/readme.txt"));
        assert!(!is_file("Ability.ID.103171"));
    }

    #[test]
    fn generated_classes_read_as_what_the_editor_calls_them() {
        assert_eq!(
            friendly_kind("WidgetBlueprintGeneratedClass"),
            "Widget Blueprint"
        );
        assert_eq!(friendly_kind("CompositeDataTable"), "CompositeDataTable");
    }
}
