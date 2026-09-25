//! Drops imports a package no longer names, and reports which ones those are.
//!
//! An import is named from five places, and removing one shifts every index above it, so the whole
//! table has to move together. Anything the reader cannot account for is a blocker rather than a
//! risk taken: a reference left pointing at a shifted row names a different object entirely.

use std::collections::BTreeSet;

use retoc::legacy_asset::{
    FLegacyPackageHeader, FObjectDataResource, FObjectExport, FObjectImport,
};
use retoc::zen::FPackageIndex;
use serde::Serialize;

use crate::edit::AppliedEdit;
use crate::package::ParsedPackage;
use crate::renumber::{Renumber, Runs, rebuild_runs, reference_splices, remap_table};
use crate::write::Splice;

/// How often an import is named, and by what.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ImportUsage {
    /// Decoded references inside the export bytes.
    pub references: u32,
    /// Roles in the export table: class, super, template or outer.
    pub roles: Vec<&'static str>,
    /// Imports whose own outer this is, as raw indices.
    pub outer_of: Vec<i32>,
    pub preload: u32,
    pub resources: u32,
}

impl ImportUsage {
    /// Whether nothing in the package names it, which is what makes it a candidate to drop.
    pub fn unused(&self) -> bool {
        self.references == 0
            && self.roles.is_empty()
            && self.outer_of.is_empty()
            && self.preload == 0
            && self.resources == 0
    }
}

/// What every import is used for, by position in the table.
pub fn import_usage(parsed: &ParsedPackage, header: &FLegacyPackageHeader) -> Vec<ImportUsage> {
    usage_from(&parsed.references, header)
}

/// The same count off the raw reference list, so a parse can record it as it builds the table.
pub(crate) fn usage_from(
    references: &[crate::props::IndexRef],
    header: &FLegacyPackageHeader,
) -> Vec<ImportUsage> {
    let mut usage = vec![ImportUsage::default(); header.imports.len()];
    let mut note = |index: FPackageIndex, role: &'static str| {
        if index.is_import()
            && let Some(held) = usage.get_mut(index.to_import_index() as usize)
        {
            held.roles.push(role);
        }
    };
    for export in &header.exports {
        note(export.class_index, "class");
        note(export.super_index, "super");
        note(export.template_index, "template");
        note(export.outer_index, "outer");
    }
    for (position, import) in header.imports.iter().enumerate() {
        if import.outer_index.is_import()
            && let Some(held) = usage.get_mut(import.outer_index.to_import_index() as usize)
        {
            held.outer_of
                .push(FPackageIndex::create_import(position as u32).index);
        }
    }
    for reference in references {
        let index = FPackageIndex {
            index: reference.index,
        };
        if index.is_import()
            && let Some(held) = usage.get_mut(index.to_import_index() as usize)
        {
            held.references += 1;
        }
    }
    for dependency in &header.preload_dependencies {
        if dependency.is_import()
            && let Some(held) = usage.get_mut(dependency.to_import_index() as usize)
        {
            held.preload += 1;
        }
    }
    for resource in &header.data_resources {
        if resource.outer_index.is_import()
            && let Some(held) = usage.get_mut(resource.outer_index.to_import_index() as usize)
        {
            held.resources += 1;
        }
    }
    usage
}

/// An import nothing names, with what still stands in the way of dropping it.
#[derive(Debug, Clone, Serialize)]
pub struct UnusedImport {
    pub index: i32,
    pub path: String,
    pub class_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked: Option<String>,
}

/// Every import the package never names, so a table can be tidied without reading all of it.
pub fn unused_imports(parsed: &ParsedPackage, header: &FLegacyPackageHeader) -> Vec<UnusedImport> {
    import_usage(parsed, header)
        .into_iter()
        .enumerate()
        .filter(|(_, usage)| usage.unused())
        .map(|(position, _)| {
            let info = parsed.imports.get(position);
            UnusedImport {
                index: FPackageIndex::create_import(position as u32).index,
                path: info.map(|i| i.path.clone()).unwrap_or_default(),
                class_name: info.map(|i| i.class_name.clone()).unwrap_or_default(),
                blocked: plan_import_removal(parsed, header, &[position as u32])
                    .ok()
                    .and_then(|plan| plan.blockers.into_iter().next()),
            }
        })
        .collect()
}

#[derive(Debug, Clone, Serialize)]
pub struct RemovedImport {
    pub index: i32,
    pub path: String,
    pub class_name: String,
}

/// What dropping some imports would do. Nothing is written.
#[derive(Debug, Clone, Serialize)]
pub struct ImportRemovalPlan {
    pub removed: Vec<RemovedImport>,
    /// Why it cannot go ahead. Empty when it can.
    pub blockers: Vec<String>,
    /// Decoded references that would be set to None.
    pub cleared: Vec<String>,
    pub warnings: Vec<String>,
    /// Retained imports whose index moves down.
    pub renumbered: usize,
    pub dropped_dependencies: usize,
}

impl ImportRemovalPlan {
    pub(crate) fn positions(&self) -> BTreeSet<u32> {
        self.removed
            .iter()
            .map(|import| {
                FPackageIndex {
                    index: import.index,
                }
                .to_import_index()
            })
            .collect()
    }
}

/// What removing `requested` would do: what goes, what would break, and what would be cleared.
pub fn plan_import_removal(
    parsed: &ParsedPackage,
    header: &FLegacyPackageHeader,
    requested: &[u32],
) -> Result<ImportRemovalPlan, String> {
    if requested.is_empty() {
        return Err("no imports were named".to_string());
    }
    let mut plan = ImportRemovalPlan {
        removed: Vec::new(),
        blockers: Vec::new(),
        cleared: Vec::new(),
        warnings: Vec::new(),
        renumbered: 0,
        dropped_dependencies: 0,
    };
    if !parsed.info.unversioned_properties {
        plan.blockers.push(
            "this package stores tagged properties, whose references the reader does not track, so its imports cannot be removed".to_string(),
        );
    }
    let mut going: BTreeSet<u32> = BTreeSet::new();
    for &position in requested {
        if position as usize >= header.imports.len() {
            plan.blockers
                .push(format!("this package has no import {position}"));
            continue;
        }
        if !going.insert(position) {
            plan.blockers
                .push(format!("import {position} is named twice"));
        }
    }
    let usage = import_usage(parsed, header);
    for &position in &going {
        let info = parsed.imports.get(position as usize);
        let path = info.map(|i| i.path.clone()).unwrap_or_default();
        let held = usage.get(position as usize).cloned().unwrap_or_default();
        if !held.roles.is_empty() {
            plan.blockers.push(format!(
                "{path} is the {} of an export, which cannot be left pointing at nothing",
                held.roles.join(" and ")
            ));
        }
        for &outer_of in &held.outer_of {
            let owner = FPackageIndex { index: outer_of }.to_import_index();
            if !going.contains(&owner) {
                plan.blockers.push(format!(
                    "{path} is the outer of import {outer_of}, so that one has to go with it"
                ));
            }
        }
        if held.resources > 0 {
            plan.blockers
                .push(format!("{path} owns bulk data addressed by its position"));
        }
        if info.is_some_and(|i| i.unresolved) {
            plan.warnings.push(format!(
                "{path} was already unresolved when the package was read"
            ));
        }
        if held.references > 0 {
            plan.cleared.push(format!(
                "{} reference(s) to {path} are set to None",
                held.references
            ));
        }
        plan.dropped_dependencies += held.preload as usize;
        plan.removed.push(RemovedImport {
            index: FPackageIndex::create_import(position).index,
            path,
            class_name: info.map(|i| i.class_name.clone()).unwrap_or_default(),
        });
    }
    // An export whose bytes the reader cannot account for may name any import without this
    // knowing where, and renumbering would leave that name pointing at a different object.
    let opaque: Vec<String> = parsed
        .exports
        .iter()
        .filter(|export| crate::remove::holds_undecoded_indices(export))
        .map(|export| format!("{} ({})", export.object_name, export.class_name))
        .collect();
    if !opaque.is_empty() {
        plan.blockers.push(format!(
            "{} export(s) hold data this editor does not decode, which may name any import: {}",
            opaque.len(),
            opaque
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if let Some(&first) = going.first() {
        plan.renumbered = header
            .imports
            .len()
            .saturating_sub(first as usize + going.len());
    }
    Ok(plan)
}

/// The tables and splices one import removal comes to.
pub(crate) struct ImportRemoval {
    pub splices: Vec<Splice>,
    pub imports: Vec<FObjectImport>,
    pub exports: Vec<FObjectExport>,
    pub preload_dependencies: Vec<FPackageIndex>,
    pub data_resources: Vec<FObjectDataResource>,
    pub applied: Vec<AppliedEdit>,
}

pub(crate) fn remove_imports(
    parsed: &ParsedPackage,
    header: &FLegacyPackageHeader,
    plan: &ImportRemovalPlan,
) -> Result<ImportRemoval, String> {
    if !plan.blockers.is_empty() {
        return Err(format!(
            "these imports cannot be removed: {}",
            plan.blockers.join("; ")
        ));
    }
    let going = plan.positions();
    let renumber = Renumber::removing_imports(going.clone());

    // Every export keeps its bytes, so nothing is skipped when the references are rewritten.
    let splices = reference_splices(parsed, header, &renumber, &BTreeSet::new());

    let mut exports = header.exports.clone();
    let mut imports = header.imports.clone();
    let mut data_resources = header.data_resources.clone();
    remap_table(&mut exports, &mut imports, &mut data_resources, &renumber);
    let preload_dependencies = rebuild_runs(header, &mut exports, |_, runs| {
        let mut kept: Runs = Default::default();
        for (out, run) in kept.iter_mut().zip(runs) {
            for dependency in run {
                if renumber.drops(dependency) {
                    continue;
                }
                out.push(renumber.remap(dependency));
            }
        }
        Ok(kept)
    })?;

    // Dropped last, so every index above has already been read at its old position.
    let mut position = 0usize;
    imports.retain(|_| {
        let keep = !going.contains(&(position as u32));
        position += 1;
        keep
    });

    let applied = plan
        .removed
        .iter()
        .map(|import| AppliedEdit {
            name: format!("import {}", import.index),
            offset: 0,
            offset_after: 0,
            element: None,
            elements_after: None,
            before: import.path.clone(),
            after: "(removed)".into(),
        })
        .collect();

    Ok(ImportRemoval {
        splices,
        imports,
        exports,
        preload_dependencies,
        data_resources,
        applied,
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::package::{ExportStatus, ImportInfo, PackageInfo, ParsedExport};

    fn minimal(index: i32) -> retoc::legacy_asset::FMinimalName {
        retoc::legacy_asset::FMinimalName { index, number: 0 }
    }

    fn import(outer: FPackageIndex) -> FObjectImport {
        FObjectImport {
            class_package: minimal(0),
            class_name: minimal(0),
            outer_index: outer,
            object_name: minimal(0),
            is_optional: false,
        }
    }

    fn info(index: i32, path: &str) -> ImportInfo {
        ImportInfo {
            index,
            class_package: String::new(),
            class_name: "Object".into(),
            outer_index: 0,
            object_name: String::new(),
            path: path.into(),
            unresolved: false,
            usage: ImportUsage::default(),
        }
    }

    /// Three imports, the middle one named by nothing, and one export whose class is the first.
    fn package() -> (ParsedPackage, FLegacyPackageHeader) {
        let header = FLegacyPackageHeader {
            imports: vec![
                import(FPackageIndex::create_null()),
                import(FPackageIndex::create_null()),
                import(FPackageIndex::create_import(0)),
            ],
            exports: vec![FObjectExport {
                class_index: FPackageIndex::create_import(0),
                serial_offset: 0,
                serial_size: 8,
                first_export_dependency_index: -1,
                ..Default::default()
            }],
            ..Default::default()
        };
        let parsed = ParsedPackage {
            info: PackageInfo {
                package_name: "/Game/Test".into(),
                cooked: true,
                unversioned_properties: true,
                name_count: 0,
                import_count: 3,
                export_count: 1,
            },
            names: Vec::new(),
            imports: vec![
                info(-1, "/Game/A.A"),
                info(-2, "/Game/B.B"),
                info(-3, "/Game/C.C"),
            ],
            exports: vec![ParsedExport {
                index: 0,
                object_name: "Thing".into(),
                class_name: "Object".into(),
                serial_offset: 0,
                serial_size: 8,
                outer_index: 0,
                class_index: -1,
                super_index: 0,
                template_index: 0,
                object_flags: 0,
                generate_public_hash: false,
                path: "/Game/Test.Thing".into(),
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
            }],
            unresolved_structs: Vec::new(),
            property_kinds: Default::default(),
            schema_fixups: Vec::new(),
            missing_schemas: Vec::new(),
            header_check: Default::default(),
            containers: Vec::new(),
            unset: Vec::new(),
            references: vec![crate::props::IndexRef { at: 4, index: -3 }],
            string_tables: Vec::new(),
            instanced: Vec::new(),
            tables: Vec::new(),
            channels: Vec::new(),
            native_leaves: Vec::new(),
            script_tokens: Default::default(),
            text_histories: Default::default(),
            twins: Vec::new(),
            resources: Vec::new(),
            dependencies: None,
        };
        (parsed, header)
    }

    /// Usage separates an import a table field names from one only the bytes name.
    #[test]
    fn usage_names_every_place_an_import_is_used() {
        let (parsed, header) = package();
        let usage = import_usage(&parsed, &header);
        assert_eq!(usage[0].roles, vec!["class"]);
        assert_eq!(usage[0].outer_of, vec![-3]);
        assert!(usage[1].unused(), "nothing names the middle import");
        assert_eq!(usage[2].references, 1);
    }

    /// Only the import nothing names comes back, and it comes back removable.
    #[test]
    fn the_unused_report_names_the_one_nothing_uses() {
        let (parsed, header) = package();
        let unused = unused_imports(&parsed, &header);
        assert_eq!(unused.len(), 1);
        assert_eq!(unused[0].path, "/Game/B.B");
        assert!(unused[0].blocked.is_none(), "{:?}", unused[0].blocked);
    }

    /// An import an export's class names cannot go, and one that is another import's outer takes
    /// that one with it.
    #[test]
    fn an_import_a_table_field_names_is_refused() {
        let (parsed, header) = package();
        let plan = plan_import_removal(&parsed, &header, &[0]).expect("plan");
        assert!(
            plan.blockers.iter().any(|b| b.contains("class")),
            "{:?}",
            plan.blockers
        );
        assert!(
            plan.blockers.iter().any(|b| b.contains("outer of import")),
            "{:?}",
            plan.blockers
        );
    }

    /// Dropping an import shifts the ones above it, in the table and in the bytes alike.
    #[test]
    fn removing_an_import_shifts_the_rest_and_rewrites_the_references() {
        let (parsed, header) = package();
        let plan = plan_import_removal(&parsed, &header, &[1]).expect("plan");
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(plan.renumbered, 1, "the import above it moves down");

        let removal = remove_imports(&parsed, &header, &plan).expect("removal");
        assert_eq!(removal.imports.len(), 2);
        assert_eq!(
            removal.exports[0].class_index,
            FPackageIndex::create_import(0),
            "an import below the one removed does not move"
        );
        assert_eq!(removal.splices.len(), 1);
        assert_eq!(
            removal.splices[0].bytes,
            FPackageIndex::create_import(1).index.to_le_bytes().to_vec(),
            "the reference to the third import now names the second"
        );
    }

    /// An export the reader cannot account for may name any import, so the whole table is frozen.
    #[test]
    fn an_opaque_export_blocks_every_import_removal() {
        let (mut parsed, header) = package();
        parsed.exports[0].status = ExportStatus::Failed {
            reason: "did not decode".into(),
        };
        let plan = plan_import_removal(&parsed, &header, &[1]).expect("plan");
        assert!(
            plan.blockers.iter().any(|b| b.contains("does not decode")),
            "{:?}",
            plan.blockers
        );
    }
}
