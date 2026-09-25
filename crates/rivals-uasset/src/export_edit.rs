//! Changes to the export table itself: an object's name, where it sits, and the flags the loader
//! reads it by.
//!
//! None of these touch an export's bytes, but several change what its path is, and a package's
//! public export hashes are that path lowercased. Anything that moves a hash is reported rather
//! than done quietly, because the packages importing it have no way to follow.

use retoc::legacy_asset::{FLegacyPackageHeader, FObjectExport, FPackageNameMap};
use retoc::zen::FPackageIndex;
use serde::{Deserialize, Serialize};

use crate::edit::AppliedEdit;
use crate::mappings::Mappings;
use crate::package::{ParsedExport, ParsedPackage};
use crate::value::PropertyValue;
use crate::write::Splice;

/// Flags the loader reads off an export. UE keeps many more, but only these survive a cook and
/// mean anything to a package on disk.
pub const EDITABLE_FLAGS: &[(u32, &str)] = &[
    (0x0000_0001, "Public"),
    (0x0000_0002, "Standalone"),
    (0x0000_0008, "Transactional"),
    (0x0000_0010, "ClassDefaultObject"),
    (0x0000_0020, "ArchetypeObject"),
    (0x0004_0000, "DefaultSubObject"),
    (0x0010_0000, "TextExportTransient"),
    (0x0040_0000, "InheritableComponentTemplate"),
    (0x0080_0000, "DuplicateTransient"),
    (0x0200_0000, "NonPIEDuplicateTransient"),
];

const RF_PUBLIC: u32 = 0x0000_0001;
const RF_CLASS_DEFAULT_OBJECT: u32 = 0x0000_0010;

/// Every flag bit this editor will write. The loader drops the rest on load, so setting one would
/// be a change that does not survive the trip.
fn writable_flags() -> u32 {
    EDITABLE_FLAGS.iter().fold(0, |mask, (bit, _)| mask | bit)
}

pub fn flag_names(flags: u32) -> Vec<&'static str> {
    EDITABLE_FLAGS
        .iter()
        .filter(|(bit, _)| flags & bit != 0)
        .map(|(_, name)| *name)
        .collect()
}

/// One change to an export's row in the table.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ExportEdit {
    Rename {
        export: u32,
        name: String,
    },
    /// Move the object under another export, or to the package root with no `outer`.
    SetOuter {
        export: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outer: Option<u32>,
    },
    /// Retype the object. Its values are written under the old class's schema, so they cannot be
    /// carried across: the export is emptied and takes its new class's defaults.
    SetClass {
        export: u32,
        class: i32,
    },
    /// Reparent a class, struct or enum. Every instance of it is read under the flattened chain,
    /// so this changes how other exports decode.
    SetSuper {
        export: u32,
        super_index: i32,
    },
    /// Point the object at another archetype, which is what its unset properties inherit from.
    SetTemplate {
        export: u32,
        template: i32,
    },
    SetFlags {
        export: u32,
        #[serde(default)]
        set: u32,
        #[serde(default)]
        clear: u32,
    },
    /// Whether other packages may import the object by hash.
    SetPublicHash {
        export: u32,
        on: bool,
    },
    /// Whether the object is stripped when the container targets the other side.
    SetFilter {
        export: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        not_for_client: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        not_for_server: Option<bool>,
    },
}

impl ExportEdit {
    pub fn export(&self) -> u32 {
        match self {
            Self::Rename { export, .. }
            | Self::SetOuter { export, .. }
            | Self::SetClass { export, .. }
            | Self::SetSuper { export, .. }
            | Self::SetTemplate { export, .. }
            | Self::SetFlags { export, .. }
            | Self::SetPublicHash { export, .. }
            | Self::SetFilter { export, .. } => *export,
        }
    }
}

/// What a set of export edits would do. Nothing is written.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ExportEditPlan {
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
    /// Exports whose public export hash moves, which is what other packages import them by.
    pub public: Vec<String>,
    /// Paths that change, so verification can allow those and only those.
    pub repathed: Vec<(String, String)>,
    /// Which packages import each entry of `public`, once an import index has been consulted.
    pub importers: Vec<crate::remove::Importers>,
    /// Whether an index answered for `public`. Without one the warning stays generic.
    pub index_available: bool,
}

impl ExportEditPlan {
    /// Replaces the generic hash warning with what an index knows: the packages importing each
    /// export whose hash moves, and nothing at all for one nothing imports.
    pub fn resolve_importers(&mut self, lookup: impl Fn(&str) -> Vec<String>) {
        self.importers = self
            .public
            .iter()
            .map(|path| crate::remove::Importers {
                path: path.clone(),
                packages: lookup(path),
            })
            .collect();
        for importers in &self.importers {
            if importers.packages.is_empty() {
                continue;
            }
            let shown: Vec<&str> = importers
                .packages
                .iter()
                .take(3)
                .map(String::as_str)
                .collect();
            let more = importers.packages.len().saturating_sub(shown.len());
            self.warnings.push(format!(
                "{} is imported by {} package(s), which would stop finding it: {}{}",
                importers.path,
                importers.packages.len(),
                shown.join(", "),
                if more > 0 {
                    format!(" and {more} more")
                } else {
                    String::new()
                }
            ));
        }
        self.index_available = true;
    }
}

/// The classes whose layout is walked rather than decoded. Renaming one moves the key its
/// instances are read under, so they stop resolving.
fn is_type_like(class: &str) -> bool {
    class.ends_with("Class")
        || matches!(
            class,
            "Function"
                | "DelegateFunction"
                | "SparseDelegateFunction"
                | "ScriptStruct"
                | "UserDefinedStruct"
                | "Enum"
                | "UserDefinedEnum"
        )
}

fn export_of(parsed: &ParsedPackage, index: u32) -> Result<&ParsedExport, String> {
    parsed
        .exports
        .get(index as usize)
        .ok_or_else(|| format!("this package has no export {index}"))
}

/// Whether `ancestor` is `index` or sits above it in the outer chain, which is what an outer
/// change must never create.
fn descends_from(parsed: &ParsedPackage, index: u32, ancestor: u32) -> bool {
    let mut at = index;
    for _ in 0..64 {
        if at == ancestor {
            return true;
        }
        let Ok(export) = export_of(parsed, at) else {
            return false;
        };
        let outer = FPackageIndex {
            index: export.outer_index,
        };
        if !outer.is_export() {
            return false;
        }
        at = outer.to_export_index();
    }
    false
}

/// Every export under `index`, itself included, so a rename can report what it repaths.
fn subtree(parsed: &ParsedPackage, index: u32) -> Vec<u32> {
    parsed
        .exports
        .iter()
        .filter(|export| descends_from(parsed, export.index, index))
        .map(|export| export.index)
        .collect()
}

fn name_is_usable(name: &str) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("an object needs a name".to_string());
    }
    if trimmed.contains(['.', ':', '/', '\\']) {
        return Err(format!(
            "{trimmed} cannot be an object name: a dot, colon or slash separates a path"
        ));
    }
    Ok(())
}

/// The table word for an outer: an export, or null for the package itself.
pub(crate) fn outer_index(outer: Option<u32>) -> FPackageIndex {
    outer.map_or_else(FPackageIndex::create_null, FPackageIndex::create_export)
}

/// Whether another object already sits under `outer` with this name, which the loader resolves by.
fn name_taken(parsed: &ParsedPackage, outer: i32, name: &str, except: u32) -> bool {
    parsed.exports.iter().any(|export| {
        export.index != except
            && export.outer_index == outer
            && export.object_name.eq_ignore_ascii_case(name)
    })
}

/// What the requested edits would do, with everything standing in the way named at once.
pub fn plan_export_edits(
    parsed: &ParsedPackage,
    edits: &[ExportEdit],
    mappings: Option<&Mappings>,
) -> Result<ExportEditPlan, String> {
    plan_export_edits_with(parsed, edits, mappings, &[])
}

/// The same, told which exports the same save is emptying. A retype needs its export emptied,
/// since its values were written under the old class's schema and mean nothing under the new one.
pub fn plan_export_edits_with(
    parsed: &ParsedPackage,
    edits: &[ExportEdit],
    mappings: Option<&Mappings>,
    resets: &[u32],
) -> Result<ExportEditPlan, String> {
    if edits.is_empty() {
        return Err("no export edits were named".to_string());
    }
    let mut plan = ExportEditPlan::default();
    if !parsed.info.unversioned_properties {
        plan.blockers.push(
            "this package stores tagged properties, whose layout this editor does not rewrite"
                .to_string(),
        );
    }
    // One change of each kind per export: two renames of the same object would disagree.
    let mut seen: Vec<(u32, std::mem::Discriminant<ExportEdit>)> = Vec::new();
    for edit in edits {
        let index = edit.export();
        let export = match export_of(parsed, index) {
            Ok(export) => export,
            Err(reason) => {
                plan.blockers.push(reason);
                continue;
            }
        };
        let kind = (index, std::mem::discriminant(edit));
        if seen.contains(&kind) {
            plan.blockers
                .push(format!("{} is given the same change twice", export.path));
        }
        seen.push(kind);
        match edit {
            ExportEdit::Rename { name, .. } => {
                if let Err(reason) = name_is_usable(name) {
                    plan.blockers.push(reason);
                    continue;
                }
                let name = name.trim();
                if is_type_like(&export.class_name) {
                    plan.blockers.push(format!(
                        "{} is a {}, which its instances are read under by name, so renaming it would leave them unreadable",
                        export.path, export.class_name
                    ));
                }
                if export.object_flags & RF_CLASS_DEFAULT_OBJECT != 0 {
                    plan.blockers.push(format!(
                        "{} is a class default object, whose name follows its class",
                        export.path
                    ));
                }
                if name_taken(parsed, export.outer_index, name, index) {
                    plan.blockers.push(format!(
                        "something called {name} already sits beside {}",
                        export.path
                    ));
                }
                if export.object_name != name {
                    note_repaths(parsed, &mut plan, index, |path| {
                        let head = path.rfind([':', '.']).map_or(0, |at| at + 1);
                        format!("{}{name}", &path[..head])
                    });
                }
            }
            ExportEdit::SetOuter { outer, .. } => {
                let wanted = outer_index(*outer);
                if let Some(outer) = *outer {
                    if outer as usize >= parsed.exports.len() {
                        plan.blockers
                            .push(format!("this package has no export {outer} to sit under"));
                        continue;
                    }
                    if descends_from(parsed, outer, index) {
                        plan.blockers.push(format!(
                            "{} cannot sit inside itself or anything under it",
                            export.path
                        ));
                        continue;
                    }
                }
                if name_taken(parsed, wanted.index, &export.object_name, index) {
                    let place = outer.map_or("at the package root".to_string(), |outer| {
                        format!("under export {outer}")
                    });
                    plan.blockers.push(format!(
                        "something called {} already sits {place}",
                        export.object_name
                    ));
                }
                if is_type_like(&export.class_name) {
                    plan.blockers.push(format!(
                        "{} is a {}, which is found by path, so moving it would leave its instances unreadable",
                        export.path, export.class_name
                    ));
                }
                // A root object is named `Package.Name`; anything deeper `Outer:Name`.
                let head = match *outer {
                    Some(outer) => parsed
                        .exports
                        .get(outer as usize)
                        .map(|outer| format!("{}:", outer.path))
                        .unwrap_or_default(),
                    None => format!("{}.", parsed.info.package_name),
                };
                note_repaths(parsed, &mut plan, index, move |path| {
                    let tail = path.rsplit([':', '.']).next().unwrap_or(path);
                    format!("{head}{tail}")
                });
            }
            ExportEdit::SetClass { class, .. } => {
                plan_set_class(parsed, &mut plan, export, *class, mappings, resets);
            }
            ExportEdit::SetSuper { super_index, .. } => {
                plan_set_super(parsed, &mut plan, export, *super_index, mappings);
            }
            ExportEdit::SetTemplate { template, .. } => {
                let wanted = FPackageIndex { index: *template };
                if wanted.is_export() && wanted.to_export_index() as usize >= parsed.exports.len() {
                    plan.blockers.push(format!(
                        "this package has no export for template {template}"
                    ));
                }
                if wanted.is_import() && wanted.to_import_index() as usize >= parsed.imports.len() {
                    plan.blockers.push(format!(
                        "this package has no import for template {template}"
                    ));
                }
                if wanted.is_export() && descends_from(parsed, wanted.to_export_index(), index) {
                    plan.blockers.push(format!(
                        "{} cannot take itself or something under it as its archetype",
                        export.path
                    ));
                }
                plan.warnings.push(format!(
                    "{} inherits every value it does not store from its archetype, so changing it changes what those read",
                    export.path
                ));
            }
            ExportEdit::SetFlags { set, clear, .. } => {
                let touched = set | clear;
                let unknown = touched & !writable_flags();
                if unknown != 0 {
                    plan.blockers.push(format!(
                        "{unknown:#010X} is not a flag the loader keeps on a cooked object"
                    ));
                }
                if touched & RF_CLASS_DEFAULT_OBJECT != 0 {
                    plan.blockers.push(
                        "a class default object is one its class names, so that flag is not editable"
                            .to_string(),
                    );
                }
                if clear & RF_PUBLIC != 0 && !export.generate_public_hash {
                    plan.public.push(export.path.clone());
                }
            }
            ExportEdit::SetPublicHash { on, .. } => {
                if !on && (export.generate_public_hash || export.object_flags & RF_PUBLIC != 0) {
                    plan.public.push(export.path.clone());
                }
            }
            ExportEdit::SetFilter { .. } => {
                plan.warnings.push(format!(
                    "{} is stripped from a container built for the side it is filtered out of",
                    export.path
                ));
            }
        }
    }
    plan.repathed.sort();
    plan.repathed.dedup();
    plan.public.sort();
    plan.public.dedup();
    Ok(plan)
}

/// A retype rewrites what the export's bytes mean, so it is refused wherever the bytes cannot
/// simply be dropped: a table or type whose layout the reader walks rather than decodes, a class
/// with no schema to read the object back under, an export whose class writes a tail, and one
/// still storing values that the same save is not emptying.
fn plan_set_class(
    parsed: &ParsedPackage,
    plan: &mut ExportEditPlan,
    export: &ParsedExport,
    class: i32,
    mappings: Option<&Mappings>,
    resets: &[u32],
) {
    let wanted = FPackageIndex { index: class };
    if wanted.is_null() {
        plan.blockers
            .push("an object needs a class; null is not one".to_string());
        return;
    }
    let Some(name) = class_name_of(parsed, wanted) else {
        plan.blockers
            .push(format!("this package has no class at index {class}"));
        return;
    };
    if is_type_like(&export.class_name) || is_type_like(&name) {
        plan.blockers.push(format!(
            "{} is a {}, whose layout is walked rather than decoded, so it cannot be retyped",
            export.path, export.class_name
        ));
    }
    if export.object_flags & RF_CLASS_DEFAULT_OBJECT != 0 {
        plan.blockers.push(format!(
            "{} is the default object of its class, so its class is not something to change",
            export.path
        ));
    }
    for held in [export.class_name.as_str(), name.as_str()] {
        if matches!(held, "DataTable" | "StringTable" | "CompositeDataTable") {
            plan.blockers.push(format!(
                "a {held} writes its rows after its properties, which a retype cannot carry across"
            ));
        }
    }
    match mappings {
        Some(schema) => {
            if schema.class_schema(&name, None).is_none() {
                plan.blockers.push(format!(
                    "the mappings file has no class schema for {name}, so the object could not be read back"
                ));
            }
        }
        None => plan
            .blockers
            .push("reading an object back under a new class needs the mappings file".to_string()),
    }
    if export.properties_end.is_none() {
        plan.blockers.push(format!(
            "{}'s property block did not read to its end, so a retype has nothing to replace",
            export.path
        ));
    }
    if !matches!(export.status, crate::package::ExportStatus::Complete) {
        plan.blockers.push(format!(
            "{} does not read to its end today, so what a retype would leave cannot be predicted",
            export.path
        ));
    }
    // The bytes past the property block belong to the class, and a retype does not rewrite them.
    // So the two chains have to write the same tail: the old one is still there afterwards, and
    // the new class has to read exactly it.
    if let Some(schema) = mappings {
        let was = schema.ancestry(&export.class_name);
        let is = schema.ancestry(&name);
        if crate::tails::payload_kind(&as_owned(&was)).is_some()
            || crate::tails::payload_kind(&as_owned(&is)).is_some()
        {
            plan.blockers.push(format!(
                "{} or {name} keeps bulk data after its properties, which a retype cannot carry across",
                export.class_name
            ));
        } else if crate::tails::tail_steps(&was) != crate::tails::tail_steps(&is) {
            plan.blockers.push(format!(
                "{} writes {:?} after its properties and {name} writes {:?}, so the bytes already there would not read back",
                export.class_name,
                crate::tails::tail_steps(&was),
                crate::tails::tail_steps(&is)
            ));
        }
    }
    let stores = export
        .properties
        .iter()
        .any(|entry| !matches!(entry.value, PropertyValue::Unset { .. }));
    if stores && !resets.contains(&export.index) {
        plan.blockers.push(format!(
            "{} stores values written under {}; reset it in the same save to retype it",
            export.path, export.class_name
        ));
    }
    plan.warnings.push(format!(
        "{} takes every value from {name} after the retype; nothing it stored is carried across",
        export.path
    ));
}

/// Reparenting changes the flattened schema every instance is read under, so it is offered only
/// for the type-like exports that have a parent at all, and only where the new chain resolves.
fn plan_set_super(
    parsed: &ParsedPackage,
    plan: &mut ExportEditPlan,
    export: &ParsedExport,
    super_index: i32,
    mappings: Option<&Mappings>,
) {
    if !is_type_like(&export.class_name) {
        plan.blockers.push(format!(
            "{} is a {}, not a class or struct, so it has no parent to change",
            export.path, export.class_name
        ));
        return;
    }
    if export.super_struct_at.is_none() {
        plan.blockers.push(format!(
            "{}'s layout was not walked far enough to find where its parent is written",
            export.path
        ));
        return;
    }
    let wanted = FPackageIndex { index: super_index };
    if wanted.is_export() {
        let at = wanted.to_export_index();
        if at as usize >= parsed.exports.len() {
            plan.blockers
                .push(format!("this package has no export {at} to parent it to"));
            return;
        }
        if at == export.index || descends_from(parsed, at, export.index) {
            plan.blockers.push(format!(
                "{} cannot inherit from itself or from something under it",
                export.path
            ));
        }
        if !is_type_like(&parsed.exports[at as usize].class_name) {
            plan.blockers.push(format!(
                "{} is a {}, which is not something to inherit from",
                parsed.exports[at as usize].path, parsed.exports[at as usize].class_name
            ));
        }
    } else if wanted.is_import() && wanted.to_import_index() as usize >= parsed.imports.len() {
        plan.blockers.push(format!(
            "this package has no import {} to parent it to",
            wanted.to_import_index()
        ));
        return;
    }
    // An instance's values sit in bytes laid out for the flattened chain it was written under, and
    // this editor splices bytes rather than re-encoding them. So the bytes stay valid exactly when
    // the two parents flatten to the same property list, and are misread at the wrong widths when
    // they do not. Emptying the instance is not a way out: a class default object holds the
    // component wiring its construction script needs, and clearing that crashes the loader on the
    // defaults that are no longer there. Both failures were seen in game on 2026-09-08.
    let stored_by: Vec<(&str, usize)> = parsed
        .exports
        .iter()
        .filter(|held| held.class_index == FPackageIndex::create_export(export.index).index)
        .map(|held| {
            (
                held.path.as_str(),
                held.properties
                    .iter()
                    .filter(|entry| !matches!(entry.value, PropertyValue::Unset { .. }))
                    .count(),
            )
        })
        .filter(|(_, stored)| *stored > 0)
        .collect();
    if !stored_by.is_empty() {
        match same_layout(parsed, mappings, export.super_index, super_index) {
            Some(true) => plan.warnings.push(format!(
                "{} keeps the same property layout under its new parent, so what its instances store still reads the same way",
                export.path
            )),
            Some(false) => {
                for (path, stored) in &stored_by {
                    plan.blockers.push(format!(
                        "{path} stores {stored} value(s) laid out for {}'s current parent, and the \
                         new one flattens to a different property list, so they would be read at \
                         the wrong widths",
                        export.path
                    ));
                }
            }
            None => plan.blockers.push(format!(
                "the mappings file does not describe both of {}'s parents, so whether its \
                 instances still read the same way cannot be established",
                export.path
            )),
        }
    }
    plan.warnings.push(format!(
        "every object read under {} takes its values from the flattened chain, so reparenting it changes what they decode as. Instances in other packages are not checked here",
        export.path
    ));
}

/// Whether two parents flatten to the same property list, which is what decides if the bytes an
/// instance already holds still read correctly. `None` when either parent cannot be resolved, so a
/// caller refuses rather than guesses.
///
/// Compared slot by slot rather than by name: two chains can reach the same layout by different
/// routes, and that is still safe, while two same-named chains that reordered a property are not.
fn same_layout(
    parsed: &ParsedPackage,
    mappings: Option<&Mappings>,
    was: i32,
    is: i32,
) -> Option<bool> {
    if was == is {
        return Some(true);
    }
    let mappings = mappings?;
    let schema_of = |index: i32| {
        schema_keys(parsed, FPackageIndex { index })
            .into_iter()
            .find_map(|name| mappings.schema(&name))
    };
    // A null parent flattens to nothing, which is a layout like any other.
    let slots = |index: i32| -> Option<Vec<(String, u32, &'static str)>> {
        if (FPackageIndex { index }).is_null() {
            return Some(Vec::new());
        }
        let schema = schema_of(index)?;
        Some(
            (0..schema.len())
                .filter_map(|at| schema.slot(at))
                .map(|slot| {
                    (
                        slot.property.name.clone(),
                        slot.element,
                        crate::mappings::kind_name(&slot.property.inner),
                    )
                })
                .collect(),
        )
    };
    Some(slots(was)? == slots(is)?)
}

/// The names the mappings file might key a parent under, best first. A Blueprint class is keyed by
/// its object path where two share a short name and by the short name otherwise, so both are worth
/// trying before concluding a parent is undescribed.
fn schema_keys(parsed: &ParsedPackage, index: FPackageIndex) -> Vec<String> {
    if index.is_import() {
        return parsed
            .imports
            .get(index.to_import_index() as usize)
            .map(|import| vec![import.path.clone(), import.object_name.clone()])
            .unwrap_or_default();
    }
    if index.is_export() {
        return parsed
            .exports
            .get(index.to_export_index() as usize)
            .map(|held| vec![held.path.clone(), held.object_name.clone()])
            .unwrap_or_default();
    }
    Vec::new()
}

/// `payload_kind` keys on owned names, which is what an ancestry is not.
fn as_owned(chain: &[&str]) -> Vec<String> {
    chain.iter().map(|step| (*step).to_string()).collect()
}

/// The short class name an index points at, whichever table it names.
fn class_name_of(parsed: &ParsedPackage, index: FPackageIndex) -> Option<String> {
    if index.is_export() {
        parsed
            .exports
            .get(index.to_export_index() as usize)
            .map(|export| export.object_name.clone())
    } else if index.is_import() {
        parsed
            .imports
            .get(index.to_import_index() as usize)
            .map(|import| import.object_name.clone())
    } else {
        None
    }
}

/// Records the new path of every export under `index`, and notes the ones other packages import.
fn note_repaths(
    parsed: &ParsedPackage,
    plan: &mut ExportEditPlan,
    index: u32,
    rename_root: impl Fn(&str) -> String,
) {
    let Ok(root) = export_of(parsed, index) else {
        return;
    };
    let was = root.path.clone();
    let now = rename_root(&was);
    for member in subtree(parsed, index) {
        let Ok(export) = export_of(parsed, member) else {
            continue;
        };
        let path = export.path.clone();
        let moved = if member == index {
            now.clone()
        } else if let Some(tail) = path.strip_prefix(&was) {
            format!("{now}{tail}")
        } else {
            continue;
        };
        if export.object_flags & RF_PUBLIC != 0 || export.generate_public_hash {
            plan.public.push(path.clone());
        }
        plan.repathed.push((path, moved));
    }
}

/// The table an export edit leaves behind.
pub(crate) struct ExportPatch {
    pub names: Option<FPackageNameMap>,
    pub exports: Vec<FObjectExport>,
    pub applied: Vec<AppliedEdit>,
    /// Splices inside the export data: a retype empties the property block, a reparent rewrites the
    /// parent index the layout holds. The other edits touch the table only.
    pub splices: Vec<Splice>,
}

pub(crate) fn apply_export_edits(
    parsed: &ParsedPackage,
    header: &FLegacyPackageHeader,
    edits: &[ExportEdit],
    mappings: Option<&Mappings>,
    resets: &[u32],
) -> Result<ExportPatch, String> {
    let plan = plan_export_edits_with(parsed, edits, mappings, resets)?;
    if !plan.blockers.is_empty() {
        return Err(format!(
            "these export changes cannot be made: {}",
            plan.blockers.join("; ")
        ));
    }
    let mut names = header.name_map.clone();
    let mut exports = header.exports.clone();
    let mut applied = Vec::with_capacity(edits.len());
    let mut renamed = false;
    let mut splices: Vec<Splice> = Vec::new();

    for edit in edits {
        let index = edit.export() as usize;
        let was = parsed
            .exports
            .get(index)
            .map(|export| export.path.clone())
            .unwrap_or_default();
        let export = exports
            .get_mut(index)
            .ok_or_else(|| format!("this package has no export {index}"))?;
        let (field, before, after) = match edit {
            ExportEdit::Rename { name, .. } => {
                let name = name.trim();
                let before = header
                    .name_map
                    .get(export.object_name)
                    .map(|held| held.into_owned())
                    .unwrap_or_default();
                export.object_name = names.store(name);
                renamed = true;
                ("name", before, name.to_string())
            }
            ExportEdit::SetOuter { outer, .. } => {
                let before = export.outer_index.index.to_string();
                export.outer_index = outer_index(*outer);
                ("outer", before, export.outer_index.index.to_string())
            }
            ExportEdit::SetClass { class, .. } => {
                let before = export.class_index.index.to_string();
                export.class_index = FPackageIndex { index: *class };
                // The old values were written under the old schema, so the block is replaced with
                // an empty header for the new class. A reset in the same save writes the same
                // range, and the two would overlap, so this one stands in its place.
                let held = parsed
                    .exports
                    .get(index)
                    .ok_or_else(|| format!("this package has no export {index}"))?;
                let end = held
                    .properties_end
                    .ok_or_else(|| format!("{}'s property block did not read", held.path))?;
                let slots = mappings
                    .and_then(|schema| {
                        schema
                            .class_schema(class_name_of(parsed, export.class_index)?.as_str(), None)
                    })
                    .map(|schema| schema.len())
                    .ok_or("the new class has no schema in the mappings file")?;
                splices.push(Splice {
                    start: held.serial_offset.max(0) as u64,
                    end,
                    bytes: crate::unversioned::empty_header(slots),
                });
                ("class", before, class.to_string())
            }
            ExportEdit::SetSuper { super_index, .. } => {
                let before = export.super_index.index.to_string();
                export.super_index = FPackageIndex {
                    index: *super_index,
                };
                // The index is written twice: in the table, and inside the layout the reader walks.
                let at = parsed
                    .exports
                    .get(index)
                    .and_then(|held| held.super_struct_at)
                    .ok_or_else(|| format!("export {index} has no parent index to rewrite"))?;
                splices.push(Splice {
                    start: at,
                    end: at + 4,
                    bytes: super_index.to_le_bytes().to_vec(),
                });
                ("parent", before, super_index.to_string())
            }
            ExportEdit::SetTemplate { template, .. } => {
                let before = export.template_index.index.to_string();
                export.template_index = FPackageIndex { index: *template };
                ("archetype", before, template.to_string())
            }
            ExportEdit::SetFlags { set, clear, .. } => {
                let before = export.object_flags;
                export.object_flags = (before & !clear) | set;
                (
                    "flags",
                    flag_names(before).join(" "),
                    flag_names(export.object_flags).join(" "),
                )
            }
            ExportEdit::SetPublicHash { on, .. } => {
                let before = export.generate_public_hash;
                export.generate_public_hash = *on;
                ("public hash", before.to_string(), on.to_string())
            }
            ExportEdit::SetFilter {
                not_for_client,
                not_for_server,
                ..
            } => {
                let before = format!(
                    "client {} server {}",
                    !export.is_not_for_client, !export.is_not_for_server
                );
                if let Some(value) = not_for_client {
                    export.is_not_for_client = *value;
                }
                if let Some(value) = not_for_server {
                    export.is_not_for_server = *value;
                }
                (
                    "filter",
                    before,
                    format!(
                        "client {} server {}",
                        !export.is_not_for_client, !export.is_not_for_server
                    ),
                )
            }
        };
        applied.push(AppliedEdit {
            name: format!("{was} {field}"),
            offset: 0,
            offset_after: 0,
            element: None,
            elements_after: None,
            before,
            after,
        });
    }

    splices.sort_by_key(|splice| (splice.start, splice.end));
    Ok(ExportPatch {
        names: renamed.then_some(names),
        exports,
        applied,
        splices,
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::package::{ExportStatus, PackageInfo};

    fn export(index: u32, name: &str, class: &str, outer: i32) -> ParsedExport {
        ParsedExport {
            index,
            object_name: name.into(),
            class_name: class.into(),
            serial_offset: 0,
            serial_size: 0,
            outer_index: outer,
            class_index: 0,
            super_index: 0,
            template_index: 0,
            object_flags: 0,
            generate_public_hash: false,
            path: if outer == 0 {
                format!("/Game/Test.{name}")
            } else {
                format!("/Game/Test.Root:{name}")
            },
            status: ExportStatus::Complete,
            properties: Vec::new(),
            properties_end: None,
            schema_slots: None,
            data_table: None,
            string_table: None,
            struct_definition: None,
            signature: None,
            trailing_hex: String::new(),
            note: None,
            script: None,
            undecoded: Vec::new(),
            super_struct_at: None,
            name_refs: Vec::new(),
        }
    }

    /// Root at 0, two children under it, and a class the package defines.
    fn package() -> ParsedPackage {
        ParsedPackage {
            info: PackageInfo {
                package_name: "/Game/Test".into(),
                cooked: true,
                unversioned_properties: true,
                name_count: 0,
                import_count: 0,
                export_count: 4,
            },
            names: Vec::new(),
            imports: Vec::new(),
            exports: vec![
                export(0, "Root", "Actor", 0),
                export(1, "Mesh", "StaticMeshComponent", 1),
                export(2, "Light", "PointLightComponent", 1),
                export(3, "Thing_C", "BlueprintGeneratedClass", 0),
            ],
            unresolved_structs: Vec::new(),
            property_kinds: Default::default(),
            schema_fixups: Vec::new(),
            missing_schemas: Vec::new(),
            header_check: Default::default(),
            containers: Vec::new(),
            unset: Vec::new(),
            references: Vec::new(),
            string_tables: Vec::new(),
            instanced: Vec::new(),
            tables: Vec::new(),
            channels: Vec::new(),
            native_leaves: Vec::new(),
            tag_bounds: Vec::new(),
            tagged_absent: Vec::new(),
            script_tokens: Default::default(),
            text_histories: Default::default(),
            twins: Vec::new(),
            resources: Vec::new(),
            dependencies: None,
        }
    }

    /// A rename repaths the object and everything under it, which is what moves their hashes.
    #[test]
    fn a_rename_repaths_the_whole_subtree() {
        let parsed = package();
        let plan = plan_export_edits(
            &parsed,
            &[ExportEdit::Rename {
                export: 0,
                name: "Renamed".into(),
            }],
            None,
        )
        .expect("plan");
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(
            plan.repathed,
            vec![
                (
                    "/Game/Test.Root".to_string(),
                    "/Game/Test.Renamed".to_string()
                ),
                (
                    "/Game/Test.Root:Light".to_string(),
                    "/Game/Test.Renamed:Light".to_string()
                ),
                (
                    "/Game/Test.Root:Mesh".to_string(),
                    "/Game/Test.Renamed:Mesh".to_string()
                ),
            ]
        );
    }

    /// A name another object beside it already answers to would make the two indistinguishable.
    #[test]
    fn a_name_already_beside_it_is_refused() {
        let parsed = package();
        let plan = plan_export_edits(
            &parsed,
            &[ExportEdit::Rename {
                export: 1,
                name: "Light".into(),
            }],
            None,
        )
        .expect("plan");
        assert!(
            plan.blockers.iter().any(|b| b.contains("already sits")),
            "{:?}",
            plan.blockers
        );
    }

    /// A class is found by path, so renaming one leaves every instance of it unreadable.
    #[test]
    fn renaming_a_class_is_refused() {
        let parsed = package();
        let plan = plan_export_edits(
            &parsed,
            &[ExportEdit::Rename {
                export: 3,
                name: "Other_C".into(),
            }],
            None,
        )
        .expect("plan");
        assert!(
            plan.blockers
                .iter()
                .any(|b| b.contains("read under by name")),
            "{:?}",
            plan.blockers
        );
    }

    /// An object cannot be moved inside itself, nor inside anything already under it.
    #[test]
    fn an_outer_cycle_is_refused() {
        let parsed = package();
        for outer in [0, 1] {
            let plan = plan_export_edits(
                &parsed,
                &[ExportEdit::SetOuter {
                    export: 0,
                    outer: Some(outer),
                }],
                None,
            )
            .expect("plan");
            assert!(
                plan.blockers.iter().any(|b| b.contains("inside itself")),
                "outer {outer}: {:?}",
                plan.blockers
            );
        }
    }

    /// No outer is the package root, which is where the package's own asset sits, not export 0.
    #[test]
    fn a_move_with_no_outer_goes_to_the_package_root() {
        let parsed = package();
        let plan = plan_export_edits(
            &parsed,
            &[ExportEdit::SetOuter {
                export: 1,
                outer: None,
            }],
            None,
        )
        .expect("plan");
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(
            plan.repathed,
            vec![(
                "/Game/Test.Root:Mesh".to_string(),
                "/Game/Test.Mesh".to_string()
            )]
        );
        assert_eq!(outer_index(None).index, 0);
        assert_eq!(outer_index(Some(0)).index, 1);
    }

    /// Only flags a cooked object actually keeps may be written, and the class default object flag
    /// is the class's business.
    #[test]
    fn only_flags_the_loader_keeps_are_writable() {
        let parsed = package();
        let plan = plan_export_edits(
            &parsed,
            &[ExportEdit::SetFlags {
                export: 0,
                set: 0x0000_0040,
                clear: 0,
            }],
            None,
        )
        .expect("plan");
        assert!(
            plan.blockers.iter().any(|b| b.contains("not a flag")),
            "{:?}",
            plan.blockers
        );

        let plan = plan_export_edits(
            &parsed,
            &[ExportEdit::SetFlags {
                export: 0,
                set: RF_CLASS_DEFAULT_OBJECT,
                clear: 0,
            }],
            None,
        )
        .expect("plan");
        assert!(
            plan.blockers
                .iter()
                .any(|b| b.contains("class default object")),
            "{:?}",
            plan.blockers
        );
    }

    /// Turning off a public hash is allowed, and reported, because importers find the object by it.
    #[test]
    fn dropping_a_public_hash_is_reported() {
        let mut parsed = package();
        parsed.exports[0].generate_public_hash = true;
        let plan = plan_export_edits(
            &parsed,
            &[ExportEdit::SetPublicHash {
                export: 0,
                on: false,
            }],
            None,
        )
        .expect("plan");
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(plan.public, vec!["/Game/Test.Root".to_string()]);
    }
}
