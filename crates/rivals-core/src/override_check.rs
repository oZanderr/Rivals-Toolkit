//! Checks the packages a mod overrides against the game's current copies of them.
//!
//! A mod keeps loading its own copy of a package after a game update changes the game's, so the
//! game's other code may reach for a widget, a function or a row the mod's copy never had. What the
//! mod's copy adds or changes is its own intent; what the game's copy has that the mod's lacks, or
//! declares differently, is the sign the copy was built against an older game.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use retoc::{EIoChunkType, FIoChunkId};
use rivals_uasset::{AssetBundle, Mappings, MemberKind, ParseOptions, ParsedPackage};
use serde::Serialize;

use crate::asset::{AssetSource, PackageConverter, list_packages};
use crate::pak::containers::open_base_game_paks;
use crate::schema_synth::{self, LayoutReader, PackageSource};

/// What part of the package a finding is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    /// An object in the package: a widget, a component, a subobject.
    Export,
    /// A Blueprint variable, or a struct's field.
    Variable,
    /// A Blueprint function or event.
    Function,
    /// A function's parameters.
    Signature,
    /// The class a Blueprint class derives from.
    Parent,
    /// A DataTable row.
    Row,
    /// A DataTable's row struct, or one of its columns.
    Column,
    /// A StringTable key.
    String,
    /// An object the package imports.
    Import,
    /// What an export holds beyond its values and scripts: a texture's pixels, or bytes the parse
    /// measures but does not decode.
    Payload,
}

impl FindingKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Export => "export",
            Self::Variable => "variable",
            Self::Function => "function",
            Self::Signature => "signature",
            Self::Parent => "parent",
            Self::Row => "row",
            Self::Column => "column",
            Self::String => "string",
            Self::Import => "import",
            Self::Payload => "payload",
        }
    }
}

/// One thing the game's copy has that the mod's lacks, or holds differently.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub kind: FindingKind,
    /// The export's path, the member's, row's or key's name, or the import's path.
    pub what: String,
    /// The game's form of it: a type, a parameter list, a parent class.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub game: Option<String>,
    /// The mod's form, when it holds the same thing differently.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ours: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OverrideCheck {
    /// The package's entry, mount-relative.
    pub package: String,
    /// What its asset is: Blueprint, Widget Blueprint, DataTable and so on.
    pub kind: String,
    pub findings: Vec<Finding>,
    /// How many of the game's exports reached by name the mod's copy has at the same path, of how
    /// many.
    pub shared_exports: usize,
    pub game_exports: usize,
    /// Why the two copies could not be compared, when they could not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unreadable: Option<String>,
    /// Each value the mod's copy holds differently from the game's, when asked for. They are the
    /// mod's intent, so the verdict leaves them out.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<crate::value_diff::ValueChange>,
}

impl OverrideCheck {
    /// One line saying how the mod's copy stands against the game's.
    pub fn verdict(&self) -> String {
        if let Some(reason) = &self.unreadable {
            return format!("not compared: {reason}");
        }
        let imports = self
            .findings
            .iter()
            .filter(|finding| finding.kind == FindingKind::Import)
            .count();
        let things = self.findings.len() - imports;
        let state = match (things, imports) {
            (0, 0) => return "matches the game's current layout".to_string(),
            (0, imports) => {
                format!("the game's copy imports {imports} object(s) this one does not")
            }
            (things, _) => format!(
                "the game's copy has {things} thing(s) this one lacks or declares differently: likely built before a game update"
            ),
        };
        if self.shared_exports * 2 < self.game_exports {
            format!(
                "replaces the game's copy, sharing {} of its {} exports; {state}",
                self.shared_exports, self.game_exports
            )
        } else {
            state
        }
    }
}

/// Every package one mod overrides, checked.
#[derive(Debug, Clone, Serialize)]
pub struct ModOverrides {
    pub container: String,
    pub packages: Vec<OverrideCheck>,
    /// Why the mod could not be read at all, when it could not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Checks each mod at `utocs` against the game, with `values` listing what each override holds
/// differently too. The game's copies are read from one store held for the whole run, since opening
/// it parses every container's table of contents.
pub fn check_overrides(
    game_root: &str,
    utocs: &[PathBuf],
    mappings: Option<&Mappings>,
    values: bool,
) -> Result<Vec<ModOverrides>, String> {
    let paks = crate::paths::paks_dir(game_root);
    let game = open_base_game_paks(&paks, "")?;
    let game_converter = PackageConverter::new(game.as_ref());
    let game_layouts = LayoutReader::through(game.as_ref(), &game_converter);
    let game_container = paks.join("pakchunk0-Windows.utoc");
    let game_container = game_container.to_string_lossy();
    let game_side = Side {
        converter: &game_converter,
        layouts: &game_layouts,
        container: &game_container,
    };
    let mut out = Vec::new();
    for utoc in utocs {
        let container = crate::pak::containers::container_stem(utoc).unwrap_or_default();
        match check_mod(game_root, game.as_ref(), &game_side, utoc, mappings, values) {
            Ok(packages) => out.push(ModOverrides {
                container,
                packages,
                error: None,
            }),
            Err(error) => out.push(ModOverrides {
                container,
                packages: Vec::new(),
                error: Some(error),
            }),
        }
    }
    Ok(out)
}

/// Where one side's copies are read from.
pub(crate) struct Side<'a> {
    pub(crate) converter: &'a PackageConverter<'a>,
    pub(crate) layouts: &'a LayoutReader<'a>,
    pub(crate) container: &'a str,
}

fn check_mod(
    game_root: &str,
    game: &dyn retoc::iostore::IoStoreTrait,
    game_side: &Side<'_>,
    utoc: &Path,
    mappings: Option<&Mappings>,
    values: bool,
) -> Result<Vec<OverrideCheck>, String> {
    let container = utoc.to_string_lossy();
    let (store, packages) = list_packages(game_root, &container)?;
    let converter = PackageConverter::new(store.as_ref());
    let layouts = LayoutReader::through(store.as_ref(), &converter);
    let ours_side = Side {
        converter: &converter,
        layouts: &layouts,
        container: &container,
    };
    let mut checks = Vec::new();
    for (id, path) in &packages {
        let chunk = FIoChunkId::from_package_id(*id, 0, EIoChunkType::ExportBundleData);
        if !game.has_chunk_id(chunk) {
            continue;
        }
        let read = |side: &Side<'_>| read_copy(side, *id, path, game_root, mappings);
        let (kind, compared) = match (read(game_side), read(&ours_side)) {
            (Ok((kind, game)), Ok((_, ours))) => {
                let changes = if values {
                    crate::value_diff::value_changes(&game, &ours)
                } else {
                    Vec::new()
                };
                (kind, Ok((compare(&game, &ours), changes)))
            }
            (Err(error), _) => (String::new(), Err(format!("the game's copy: {error}"))),
            (_, Err(error)) => (String::new(), Err(format!("this copy: {error}"))),
        };
        checks.push(match compared {
            Ok((comparison, changes)) => OverrideCheck {
                package: path.clone(),
                kind,
                findings: comparison.findings,
                shared_exports: comparison.shared_exports,
                game_exports: comparison.game_exports,
                unreadable: None,
                changes,
            },
            Err(reason) => OverrideCheck {
                package: path.clone(),
                kind,
                findings: Vec::new(),
                shared_exports: 0,
                game_exports: 0,
                unreadable: Some(reason),
                changes: Vec::new(),
            },
        });
    }
    checks.sort_by(|a, b| a.package.cmp(&b.package));
    Ok(checks)
}

/// One side's copy of a package, read and parsed, with what its asset is.
pub(crate) fn read_copy(
    side: &Side<'_>,
    id: retoc::FPackageId,
    path: &str,
    game_root: &str,
    mappings: Option<&Mappings>,
) -> Result<(String, ParsedPackage), String> {
    read_copy_with_bundle(side, id, path, game_root, mappings)
        .map(|(kind, _, parsed)| (kind, parsed))
}

/// [`read_copy`], with the package's bytes kept for what the parse does not decode.
pub(crate) fn read_copy_with_bundle(
    side: &Side<'_>,
    id: retoc::FPackageId,
    path: &str,
    game_root: &str,
    mappings: Option<&Mappings>,
) -> Result<
    (
        String,
        retoc::legacy_asset::FSerializedAssetBundle,
        ParsedPackage,
    ),
    String,
> {
    let loaded = side.converter.convert(id, path)?;
    let bundle = AssetBundle {
        asset: &loaded.asset_file_buffer,
        exports: &loaded.exports_file_buffer,
    };
    let header = rivals_uasset::read_header(&bundle)?;
    let stem = Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let kind = crate::mod_report::friendly_kind(&crate::mod_report::kind_of(&header, stem));
    let parsed = schema_synth::parse_package_through(
        &bundle,
        mappings,
        &PackageSource {
            game_root,
            container: side.container,
            entry: path,
            kind: AssetSource::Utoc,
            mod_container: None,
        },
        side.layouts,
        ParseOptions::default(),
    )?;
    Ok((kind, loaded, parsed))
}

/// What comparing two copies of one package found.
#[derive(Debug, Clone, Default)]
pub struct Comparison {
    pub findings: Vec<Finding>,
    pub shared_exports: usize,
    pub game_exports: usize,
}

/// What `game` has that `ours` lacks or declares differently. Exports pair up by path, members by
/// kind and name, rows by name and string table entries by key; names compare without case, as the
/// engine's do.
pub fn compare(game: &ParsedPackage, ours: &ParsedPackage) -> Comparison {
    let key = |path: &str| path.to_ascii_lowercase();
    let our_exports: BTreeMap<String, usize> = ours
        .exports
        .iter()
        .enumerate()
        .map(|(at, export)| (key(&export.path), at))
        .collect();
    let paired: Vec<(usize, Option<usize>)> = game
        .exports
        .iter()
        .enumerate()
        .map(|(at, export)| (at, our_exports.get(&key(&export.path)).copied()))
        .collect();
    let mut findings = Vec::new();
    // Exports a member finding already names, so a missing function is not counted twice.
    let mut named = BTreeSet::new();
    for &(at, theirs) in &paired {
        let Some(theirs) = theirs else {
            continue;
        };
        let export = &game.exports[at];
        let ours_export = &ours.exports[theirs];
        if export.layout.is_some() && export.signature.is_none() {
            compare_members(
                game,
                at as u32,
                ours,
                theirs as u32,
                &mut findings,
                &mut named,
            );
            let (was, now) = (
                object_path(game, export.super_index),
                object_path(ours, ours_export.super_index),
            );
            if let (Some(was), Some(now)) = (was, now)
                && !was.eq_ignore_ascii_case(now)
            {
                findings.push(Finding {
                    kind: FindingKind::Parent,
                    what: export.object_name.clone(),
                    game: Some(was.to_string()),
                    ours: Some(now.to_string()),
                });
            }
        }
        if let (Some(table), Some(our_table)) = (&export.data_table, &ours_export.data_table) {
            compare_rows(table, our_table, &mut findings);
        }
        if let (Some(table), Some(our_table)) = (&export.string_table, &ours_export.string_table) {
            let keys: BTreeSet<String> = our_table.entries.iter().map(|e| key(&e.key)).collect();
            findings.extend(
                table
                    .entries
                    .iter()
                    .filter(|entry| !keys.contains(&key(&entry.key)))
                    .map(|entry| Finding {
                        kind: FindingKind::String,
                        what: entry.key.clone(),
                        game: Some(entry.source.clone()),
                        ours: None,
                    }),
            );
        }
    }

    // Only what the game reaches by name counts: an object at the top of the package, in a class or
    // its default object, or a widget, which a widget tree holds whatever panel it sits in. A
    // panel's slots, an animation's scene and its tracks are reached through what holds them, and
    // an editor's save names them afresh.
    let addressed = |export: &rivals_uasset::ParsedExport| match export.outer_index {
        0 => true,
        outer if outer > 0 => game.exports.get(outer as usize - 1).is_some_and(|holder| {
            holder.class_name == "WidgetTree"
                || holder.object_name.starts_with("Default__")
                || (holder.layout.is_some() && holder.signature.is_none())
        }),
        _ => false,
    };
    let paired: Vec<(usize, Option<usize>)> = paired
        .into_iter()
        .filter(|&(at, _)| addressed(&game.exports[at]))
        .collect();
    // A missing export's own objects go unsaid: the export they sit in already says it.
    let missing: BTreeSet<usize> = paired
        .iter()
        .filter(|(_, theirs)| theirs.is_none())
        .map(|&(at, _)| at)
        .collect();
    for &at in &missing {
        let export = &game.exports[at];
        let outer = export.outer_index;
        if named.contains(&at) || (outer > 0 && missing.contains(&(outer as usize - 1))) {
            continue;
        }
        findings.push(Finding {
            kind: FindingKind::Export,
            what: export.path.clone(),
            game: Some(export.class_name.clone()),
            ours: None,
        });
    }

    // Packages say little, and an import under a missing object is that object again. One under a
    // missing package is not: the package goes unsaid, so its objects speak for it.
    let our_imports: BTreeSet<String> = ours.imports.iter().map(|i| key(&i.path)).collect();
    let missing_imports: BTreeMap<i32, i32> = game
        .imports
        .iter()
        .filter(|import| !our_imports.contains(&key(&import.path)))
        .map(|import| (import.index, import.outer_index))
        .collect();
    for import in &game.imports {
        let under_missing_object = missing_imports
            .get(&import.outer_index)
            .is_some_and(|&outer| outer != 0);
        if !missing_imports.contains_key(&import.index)
            || import.outer_index == 0
            || under_missing_object
        {
            continue;
        }
        findings.push(Finding {
            kind: FindingKind::Import,
            what: import.path.clone(),
            game: Some(import.class_name.clone()),
            ours: None,
        });
    }

    findings.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.what.cmp(&b.what)));
    Comparison {
        findings,
        shared_exports: paired.iter().filter(|(_, theirs)| theirs.is_some()).count(),
        game_exports: paired.len(),
    }
}

/// The variables and functions the game's class declares that ours does not, or declares with
/// another type or other parameters.
fn compare_members(
    game: &ParsedPackage,
    class: u32,
    ours: &ParsedPackage,
    our_class: u32,
    findings: &mut Vec<Finding>,
    named: &mut BTreeSet<usize>,
) {
    let (Ok(members), Ok(our_members)) = (
        rivals_uasset::members_of(game, class),
        rivals_uasset::members_of(ours, our_class),
    ) else {
        return;
    };
    let theirs: BTreeMap<(bool, String), &rivals_uasset::Member> = our_members
        .iter()
        .map(|m| {
            (
                (m.kind == MemberKind::Function, m.name.to_ascii_lowercase()),
                m,
            )
        })
        .collect();
    for member in &members {
        let function = member.kind == MemberKind::Function;
        let Some(our_member) = theirs.get(&(function, member.name.to_ascii_lowercase())) else {
            named.extend(member.export.map(|at| at as usize));
            findings.push(Finding {
                kind: if function {
                    FindingKind::Function
                } else {
                    FindingKind::Variable
                },
                what: member.name.clone(),
                game: Some(member.detail.clone()),
                ours: None,
            });
            continue;
        };
        if !function {
            if !member.detail.eq_ignore_ascii_case(&our_member.detail) {
                findings.push(Finding {
                    kind: FindingKind::Variable,
                    what: member.name.clone(),
                    game: Some(member.detail.clone()),
                    ours: Some(our_member.detail.clone()),
                });
            }
            continue;
        }
        let shape = |parsed: &ParsedPackage, export: Option<u32>| {
            let signature = parsed.exports.get(export? as usize)?.signature.as_ref()?;
            Some(parameters(signature))
        };
        if let (Some(was), Some(now)) = (shape(game, member.export), shape(ours, our_member.export))
            && !was.eq_ignore_ascii_case(&now)
        {
            findings.push(Finding {
                kind: FindingKind::Signature,
                what: member.name.clone(),
                game: Some(was),
                ours: Some(now),
            });
        }
    }
}

/// The rows and columns the game's table has that ours lacks, and a row struct it changed.
fn compare_rows(
    table: &rivals_uasset::DataTable,
    ours: &rivals_uasset::DataTable,
    findings: &mut Vec<Finding>,
) {
    if !table.row_struct.eq_ignore_ascii_case(&ours.row_struct) {
        findings.push(Finding {
            kind: FindingKind::Column,
            what: "row struct".to_string(),
            game: Some(table.row_struct.clone()),
            ours: Some(ours.row_struct.clone()),
        });
    }
    let columns: BTreeSet<String> = ours
        .columns
        .iter()
        .map(|c| c.to_ascii_lowercase())
        .collect();
    findings.extend(
        table
            .columns
            .iter()
            .filter(|column| !columns.contains(&column.to_ascii_lowercase()))
            .map(|column| Finding {
                kind: FindingKind::Column,
                what: column.clone(),
                game: None,
                ours: None,
            }),
    );
    let rows: BTreeSet<String> = ours
        .rows
        .iter()
        .map(|r| r.name.to_ascii_lowercase())
        .collect();
    findings.extend(
        table
            .rows
            .iter()
            .filter(|row| !rows.contains(&row.name.to_ascii_lowercase()))
            .map(|row| Finding {
                kind: FindingKind::Row,
                what: row.name.clone(),
                game: None,
                ours: None,
            }),
    );
}

/// A function's parameters as `name: Type`, with `ref`, `out` or `return` before those that are.
fn parameters(signature: &rivals_uasset::FunctionSignature) -> String {
    use rivals_uasset::FieldRole;
    signature
        .params
        .iter()
        .map(|field| {
            let role = match field.role {
                FieldRole::Ref => "ref ",
                FieldRole::Out => "out ",
                FieldRole::Return => "return ",
                FieldRole::In | FieldRole::Local => "",
            };
            format!("{role}{}: {}", field.name, field.kind)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The path of the object a raw package index names.
fn object_path(parsed: &ParsedPackage, index: i32) -> Option<&str> {
    match index {
        0 => None,
        index if index < 0 => parsed
            .imports
            .get((-index - 1) as usize)
            .map(|import| import.path.as_str()),
        index => parsed
            .exports
            .get(index as usize - 1)
            .map(|export| export.path.as_str()),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use rivals_uasset::script_fixture::{event_graph, event_graph_two_events};

    /// A copy of a class built before the game gave it a second event lacks the event: one
    /// function finding, and no export finding for the function's export besides.
    #[test]
    fn a_function_the_game_added_is_one_finding() {
        let game = event_graph_two_events().build().parsed();
        let ours = event_graph().build().parsed();
        let comparison = compare(&game, &ours);
        assert_eq!(
            comparison.findings,
            vec![Finding {
                kind: FindingKind::Function,
                what: "ReceiveTick".into(),
                game: Some("event".into()),
                ours: None,
            }],
        );
        assert_eq!(comparison.shared_exports, comparison.game_exports - 1);
    }

    /// A package the copy no longer imports at all goes unsaid, and its class is the finding: the
    /// function imported from the class is the class again.
    #[test]
    fn a_class_from_a_package_the_copy_drops_is_found() {
        let ours = event_graph().build().parsed();
        let mut game = ours.clone();
        let template = game.imports[0].clone();
        let mut add = |outer_index: i32, path: &str, class_name: &str| {
            let import = rivals_uasset::ImportInfo {
                index: -(game.imports.len() as i32) - 1,
                outer_index,
                object_name: path.rsplit(['/', '.', ':']).next().unwrap_or(path).into(),
                path: path.into(),
                class_name: class_name.into(),
                ..template.clone()
            };
            game.imports.push(import.clone());
            import
        };
        let package = add(0, "/Game/UI/WBP_Banner", "Package");
        let class = add(
            package.index,
            "/Game/UI/WBP_Banner.WBP_Banner_C",
            "WidgetBlueprintGeneratedClass",
        );
        add(
            class.index,
            "/Game/UI/WBP_Banner.WBP_Banner_C:ShowBanner",
            "Function",
        );
        assert_eq!(
            compare(&game, &ours).findings,
            vec![Finding {
                kind: FindingKind::Import,
                what: class.path,
                game: Some(class.class_name),
                ours: None,
            }],
        );
    }

    /// Two copies alike find nothing, and what only the mod's copy has is the mod's own business.
    #[test]
    fn what_only_the_mod_has_is_not_a_finding() {
        let game = event_graph().build().parsed();
        let same = compare(&game, &event_graph().build().parsed());
        assert!(same.findings.is_empty(), "{:?}", same.findings);
        let check = OverrideCheck {
            package: String::new(),
            kind: String::new(),
            findings: same.findings,
            shared_exports: same.shared_exports,
            game_exports: same.game_exports,
            unreadable: None,
            changes: Vec::new(),
        };
        assert_eq!(check.verdict(), "matches the game's current layout");

        let ours = event_graph_two_events().build().parsed();
        assert!(compare(&game, &ours).findings.is_empty());
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod game_data_tests {
    use super::*;
    use rivals_uasset::{PackageEdits, RowEdit, RowOp};

    /// A table whose rows are a Blueprint struct.
    const TABLE: &str = "Marvel/Content/Marvel/Data/DataTable/UI/CardTable.uasset";
    /// A widget class whose tree holds widgets nothing else in it names.
    const WIDGET: &str =
        "Marvel/Content/Marvel/UI/Blueprints/Common/CommonPanel/WBP_Common_Item_V2_Light.uasset";

    struct Game {
        root: String,
        container: String,
        mappings: std::sync::Arc<Mappings>,
    }

    impl Game {
        fn open() -> Option<Self> {
            let root = std::env::var("RIVALS_GAME_ROOT").ok()?;
            let usmap = std::env::var("RIVALS_USMAP").ok()?;
            std::env::var_os("OODLE_LIB_PATH")?;
            let container = format!(
                "{}/MarvelGame/Marvel/Content/Paks/pakchunk0-Windows.utoc",
                root.replace('\\', "/")
            );
            let mappings = crate::mappings::load(Path::new(&usmap)).expect("mappings");
            Some(Self {
                root,
                container,
                mappings,
            })
        }

        fn parse(&self, entry: &str, bundle: &AssetBundle<'_>) -> ParsedPackage {
            schema_synth::parse_package(
                bundle,
                Some(&self.mappings),
                &PackageSource {
                    game_root: &self.root,
                    container: &self.container,
                    entry,
                    kind: AssetSource::Utoc,
                    mod_container: None,
                },
            )
            .expect("parse")
        }

        /// The game's copy of `entry`, and the copy `edits` make of it, both parsed.
        fn with(&self, entry: &str, edits: &PackageEdits) -> Result<Copies, String> {
            let loaded =
                crate::asset::load_bundle(&self.root, &self.container, entry, AssetSource::Utoc)
                    .expect("load");
            let bundle = AssetBundle {
                asset: &loaded.asset_file_buffer,
                exports: &loaded.exports_file_buffer,
            };
            let game = self.parse(entry, &bundle);
            let patched =
                rivals_uasset::patch_package(&bundle, &game, edits, Some(&self.mappings))?;
            let ours = self.parse(
                entry,
                &AssetBundle {
                    asset: &patched.asset,
                    exports: &patched.exports,
                },
            );
            Ok(Copies { game, ours })
        }
    }

    struct Copies {
        game: ParsedPackage,
        ours: ParsedPackage,
    }

    fn first_row(game: &Game) -> (u32, String) {
        let copies = game.with(TABLE, &PackageEdits::default()).expect("read");
        let table = copies
            .game
            .exports
            .iter()
            .find(|export| {
                export
                    .data_table
                    .as_ref()
                    .is_some_and(|t| !t.rows.is_empty())
            })
            .expect("the table has rows");
        let row = table.data_table.as_ref().unwrap().rows[0].name.clone();
        (table.index, row)
    }

    fn row_removed(export: u32, name: &str) -> PackageEdits {
        PackageEdits {
            rows: vec![RowEdit {
                export,
                object: None,
                op: RowOp::Remove {
                    name: name.to_string(),
                },
            }],
            ..Default::default()
        }
    }

    /// A copy built before the game gave its table a row lacks that row, and nothing else.
    #[test]
    fn a_row_the_copy_lacks_is_found() {
        let Some(game) = Game::open() else {
            return;
        };
        let (export, row) = first_row(&game);
        let copies = game
            .with(TABLE, &row_removed(export, &row))
            .expect("remove the row");
        let comparison = compare(&copies.game, &copies.ours);
        assert_eq!(
            comparison.findings,
            vec![Finding {
                kind: FindingKind::Row,
                what: row,
                game: None,
                ours: None,
            }]
        );
        assert_eq!(comparison.shared_exports, comparison.game_exports);
    }

    /// A copy with one cell set holds that cell differently and nothing else, named by its row and
    /// column with both values.
    #[test]
    fn a_cell_the_copy_sets_is_its_one_value_change() {
        let Some(game) = Game::open() else {
            return;
        };
        let read = game.with(TABLE, &PackageEdits::default()).expect("read");
        let (export, row, cell, value) = read
            .game
            .exports
            .iter()
            .find_map(|export| {
                let table = export.data_table.as_ref()?;
                table.rows.iter().find_map(|row| {
                    row.fields.iter().find_map(|cell| match cell.value {
                        rivals_uasset::PropertyValue::Int { value }
                            if cell.span.is_some_and(|(start, end)| end > start) =>
                        {
                            Some((export, row.name.clone(), cell.clone(), value))
                        }
                        _ => None,
                    })
                })
            })
            .expect("a stored int cell");
        let edits = PackageEdits {
            values: vec![rivals_uasset::ValueEdit {
                offset: cell.span.expect("stored").0,
                expect_name: cell.name.clone(),
                expect_element: cell.element,
                expect_kind: "int".into(),
                op: rivals_uasset::EditOp::Set {
                    text: (value + 1).to_string(),
                },
            }],
            ..Default::default()
        };
        let copies = game.with(TABLE, &edits).expect("set the cell");
        assert!(compare(&copies.game, &copies.ours).findings.is_empty());
        assert_eq!(
            crate::value_diff::value_changes(&copies.game, &copies.ours),
            vec![crate::value_diff::ValueChange {
                export: export.path.clone(),
                row: Some(row),
                path: cell.label(),
                game: value.to_string(),
                ours: (value + 1).to_string(),
            }]
        );
    }

    /// A widget the copy lacks is one export finding, its own subobjects folded into it.
    #[test]
    fn a_widget_the_copy_lacks_is_found() {
        let Some(game) = Game::open() else {
            return;
        };
        let read = game.with(WIDGET, &PackageEdits::default()).expect("read");
        let tree = read
            .game
            .exports
            .iter()
            .find(|export| export.class_name == "WidgetTree")
            .expect("a widget tree")
            .index;
        // The first widget in the tree that can go on its own: one a slot or a variable holds
        // stays, as its removal is refused.
        let (widget, copies) = read
            .game
            .exports
            .iter()
            .filter(|export| export.outer_index == tree as i32 + 1)
            .find_map(|export| {
                let edits = PackageEdits {
                    remove_exports: vec![export.index],
                    ..Default::default()
                };
                Some((export.path.clone(), game.with(WIDGET, &edits).ok()?))
            })
            .expect("a widget that can be removed");
        let comparison = compare(&copies.game, &copies.ours);
        let exports: Vec<&Finding> = comparison
            .findings
            .iter()
            .filter(|finding| finding.kind != FindingKind::Import)
            .collect();
        assert_eq!(exports.len(), 1, "{:?}", comparison.findings);
        assert_eq!(exports[0].kind, FindingKind::Export);
        assert_eq!(exports[0].what, widget);
        assert!(comparison.shared_exports < comparison.game_exports);
    }

    /// An editor's save names a panel's slots afresh, and nothing reaches a slot by its name: a
    /// copy whose slot goes by another name matches.
    #[test]
    fn a_slot_by_another_name_is_not_a_finding() {
        let Some(game) = Game::open() else {
            return;
        };
        let read = game.with(WIDGET, &PackageEdits::default()).expect("read");
        let slot = read
            .game
            .exports
            .iter()
            .find(|export| export.class_name.ends_with("Slot"))
            .expect("a slot");
        let edits = PackageEdits {
            exports: vec![rivals_uasset::ExportEdit::Rename {
                export: slot.index,
                name: format!("{}_99", slot.object_name),
                from: None,
            }],
            ..Default::default()
        };
        let copies = game.with(WIDGET, &edits).expect("rename the slot");
        assert_ne!(copies.ours.exports[slot.index as usize].path, slot.path);
        let comparison = compare(&copies.game, &copies.ours);
        assert!(comparison.findings.is_empty(), "{:?}", comparison.findings);
    }

    /// A mod in `~mods` holding a copy of the table without its first row: the whole check reads
    /// the game's copy and the mod's and names the row.
    #[test]
    fn a_mod_s_override_is_checked_against_the_game() {
        let Some(game) = Game::open() else {
            return;
        };
        let (export, row) = first_row(&game);
        let _turn = crate::paths::ModsTurn::take();
        let name = "RivalsToolkitOverrideProbe";
        let pak = crate::asset_edit::mod_pak_path(&game.root, name).expect("pak path");
        let clean = || {
            crate::pak::containers::drop_cached_store();
            for extension in ["pak", "utoc", "ucas"] {
                std::fs::remove_file(pak.with_extension(extension)).ok();
            }
        };
        clean();
        let saved = crate::asset_edit::save_edits(
            &crate::asset_edit::AssetEditRequest {
                game_root: &game.root,
                container: &game.container,
                entry: TABLE,
                kind: AssetSource::Utoc,
                mod_name: name,
                changes: row_removed(export, &row),
            },
            Some(&game.mappings),
            &crate::asset_edit::SaveOptions::default(),
        );
        let checked = saved.and_then(|_| {
            check_overrides(
                &game.root,
                &[pak.with_extension("utoc")],
                Some(&game.mappings),
                false,
            )
        });
        clean();
        let checked = checked.expect("save and check");
        assert_eq!(checked.len(), 1);
        assert_eq!(checked[0].error, None);
        let packages = &checked[0].packages;
        assert_eq!(packages.len(), 1, "{packages:?}");
        assert_eq!(packages[0].package, TABLE);
        assert_eq!(packages[0].kind, "DataTable");
        assert_eq!(
            packages[0]
                .findings
                .iter()
                .map(|f| (f.kind, f.what.as_str()))
                .collect::<Vec<_>>(),
            vec![(FindingKind::Row, row.as_str())],
        );
        assert!(
            packages[0]
                .verdict()
                .starts_with("the game's copy has 1 thing(s)")
        );
    }
}
