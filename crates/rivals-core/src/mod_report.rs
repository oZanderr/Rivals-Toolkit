//! Summarises what an IoStore mod ships: which packages replace the game's, what natives it calls
//! and what files, save slots and links its scripts reach for at runtime.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use retoc::{EIoChunkType, FIoChunkId, FPackageId};
use rivals_uasset::{AssetBundle, Expr, Mappings, ParseOptions, PropertyValue, dotted_path};
use serde::Serialize;

use crate::asset::{AssetSource, PackageConverter, list_packages};
use crate::pak::containers::open_base_game_paks;
use crate::schema_synth::{self, PackageSource};

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
    let own = utoc_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string();
    let converter = PackageConverter::new(store.as_ref());
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
    };
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
        let python = imports
            .iter()
            .filter(|p| p.starts_with("/Script/UnrealEnginePython/") && !p.contains(':'))
            .filter(|p| p.contains('.') && !p.contains(".Default__"))
            .cloned();
        add(&mut report.python_classes, path, python);

        let mut error = None;
        let stem = Path::new(path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let mut kind = kind_of(&header, stem);
        match schema_synth::parse_package_opts(
            &bundle,
            mappings,
            &PackageSource {
                game_root,
                container: &utoc,
                entry: path,
                kind: AssetSource::Utoc,
            },
            ParseOptions::default(),
        ) {
            Ok(parsed) => {
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
        });
    }
    report.packages.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(report)
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
fn kind_of(header: &retoc::legacy_asset::FLegacyPackageHeader, stem: &str) -> String {
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

fn friendly_kind(class: &str) -> String {
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

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

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
