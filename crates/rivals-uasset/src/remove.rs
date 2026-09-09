//! Removes exports from a package, or resets one to the values it inherits, keeping every
//! reference the reader decoded pointing where it did.
//!
//! Every `FPackageIndex` in a package names an export by position, so taking one out moves every
//! export after it. The reader records where each decoded reference sits, which is what lets a
//! removal renumber them. References inside data the reader does not decode (class layouts,
//! bytecode, level data) cannot be followed, and the plan says so rather than pretending.

use std::collections::BTreeSet;

use retoc::legacy_asset::{FLegacyPackageHeader, FObjectDataResource, FObjectExport};
use retoc::zen::FPackageIndex;
use serde::Serialize;

use crate::edit::AppliedEdit;
use crate::package::{ExportStatus, ParsedExport, ParsedPackage};
use crate::unversioned;
use crate::value::{PropertyEntry, PropertyValue};
use crate::write::Splice;

/// `RF_Public`: other packages may import the object.
const RF_PUBLIC: u32 = 0x1;

/// How a reference outside any property is labelled: it sits in the class layout the tail scanner
/// walked (function list, interfaces, default object, field types).
const LAYOUT: &str = "(class layout)";

/// Payload kinds known to carry `FPackageIndex` values the reader does not decode.
pub(crate) const INDEX_BEARING: &[&str] = &[
    "class layout and bytecode",
    "function layout and bytecode",
    "bytecode",
    "rig VM class data",
    "rig VM data",
    "rig VM memory data",
    "level data",
    "particle system data",
];

/// What removing a set of exports entails, for the user to weigh before anything is written.
#[derive(Debug, Clone, Serialize)]
pub struct RemovalPlan {
    /// Every export that goes, in table order: the ones asked for and the subobjects that cannot
    /// outlive them.
    pub removed: Vec<RemovedExport>,
    /// Why the removal cannot go ahead at all. Empty when it can.
    pub blockers: Vec<String>,
    /// Decoded references into the removed set, which are set to None.
    pub cleared: Vec<ClearedReference>,
    /// What the toolkit cannot make safe.
    pub warnings: Vec<String>,
    /// How many kept exports move to a lower index because an earlier one goes.
    pub renumbered: usize,
    /// Removed exports other packages may import, by path.
    pub public: Vec<String>,
    /// Which packages import each public export, once an import index has been consulted.
    pub importers: Vec<Importers>,
    /// Whether an import index answered for `public`. Without one the warning stays generic.
    pub index_available: bool,
}

/// The packages importing one removed export.
#[derive(Debug, Clone, Serialize)]
pub struct Importers {
    pub path: String,
    pub packages: Vec<String>,
}

/// The generic wording an index replaces with names, and the wording of the names it puts there.
const PUBLIC_WARNING: &str = "may be imported by other packages";
const IMPORTED_WARNING: &str = "is imported by";

#[derive(Debug, Clone, Serialize)]
pub struct RemovedExport {
    pub index: u32,
    pub path: String,
    pub class_name: String,
    /// Named by the caller, rather than pulled in as the subobject of one that was.
    pub requested: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClearedReference {
    pub export: u32,
    pub export_name: String,
    pub property: String,
    pub target: String,
}

impl RemovalPlan {
    pub fn indices(&self) -> Vec<u32> {
        self.removed.iter().map(|export| export.index).collect()
    }

    /// Replaces the generic importable warning with what an index knows: the packages importing
    /// each public export, and no warning at all for one nothing imports.
    pub fn resolve_importers(&mut self, lookup: impl Fn(&str) -> Vec<String>) {
        self.warnings.retain(|warning| {
            !warning.contains(PUBLIC_WARNING) && !warning.contains(IMPORTED_WARNING)
        });
        self.importers = self
            .public
            .iter()
            .map(|path| Importers {
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
                "{} {IMPORTED_WARNING} {} package(s), which would fail to find it: {}{}",
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

/// Works out everything removing `requested` entails. Nothing is changed.
pub fn plan_removal(parsed: &ParsedPackage, requested: &[u32]) -> Result<RemovalPlan, String> {
    let count = parsed.exports.len();
    if requested.is_empty() {
        return Err("no export was named".into());
    }
    if let Some(index) = requested.iter().find(|&&index| index as usize >= count) {
        return Err(format!(
            "this package has {count} exports, so there is no export {index}"
        ));
    }
    let mut removed: BTreeSet<u32> = requested.iter().copied().collect();
    // An object cannot outlive its outer, so the subobjects of a removed export go with it.
    loop {
        let more: Vec<u32> = parsed
            .exports
            .iter()
            .filter(|export| !removed.contains(&export.index))
            .filter(|export| {
                export_of(export.outer_index).is_some_and(|outer| removed.contains(&outer))
            })
            .map(|export| export.index)
            .collect();
        if more.is_empty() {
            break;
        }
        removed.extend(more);
    }
    let export_at = |index: u32| parsed.exports.get(index as usize);
    let path_of = |index: u32| export_at(index).map(|e| e.path.clone()).unwrap_or_default();
    let kept: Vec<&ParsedExport> = parsed
        .exports
        .iter()
        .filter(|export| !removed.contains(&export.index))
        .collect();

    let mut blockers = Vec::new();
    if kept.is_empty() {
        blockers.push("every export would go, and a package needs at least one".into());
    }
    if !parsed.info.unversioned_properties {
        blockers.push(
            "this package stores tagged properties, whose references the reader does not track, so its exports cannot be removed"
                .into(),
        );
    }
    // Decoding an export follows its class, and its stored values were diffed against its
    // archetype, so neither may go while the export stays.
    for export in &kept {
        for (index, role) in [
            (export.class_index, "class"),
            (export.super_index, "parent class"),
            (export.template_index, "archetype"),
        ] {
            if let Some(target) = export_of(index)
                && removed.contains(&target)
            {
                blockers.push(format!(
                    "{} is the {role} of {}, which stays",
                    path_of(target),
                    export.path
                ));
            }
        }
    }

    let mut cleared = Vec::new();
    for reference in &parsed.references {
        let Some(target) = export_of(reference.index) else {
            continue;
        };
        if !removed.contains(&target) {
            continue;
        }
        let Some(owner) = owner_of(parsed, reference.at) else {
            continue;
        };
        if removed.contains(&owner.index) {
            continue;
        }
        cleared.push(ClearedReference {
            export: owner.index,
            export_name: owner.object_name.clone(),
            property: label_at(parsed, owner, reference.at).unwrap_or_else(|| LAYOUT.to_string()),
            target: path_of(target),
        });
    }

    let first = removed.first().copied().unwrap_or(0);
    let renumbered = kept.iter().filter(|export| export.index > first).count();

    let mut warnings = Vec::new();
    // A class's function list, interfaces or default object pointing at nothing is not something
    // the engine expects, unlike a property that reads None.
    let layout: Vec<String> = cleared
        .iter()
        .filter(|reference| reference.property == LAYOUT)
        .map(|reference| format!("{} -> {}", reference.export_name, reference.target))
        .collect();
    if !layout.is_empty() {
        warnings.push(format!(
            "a class layout points at a removed export and would be left pointing at nothing: {}",
            layout.join(", ")
        ));
    }
    let opaque: Vec<&str> = kept
        .iter()
        .filter(|export| holds_undecoded_indices(export))
        .map(|export| export.object_name.as_str())
        .collect();
    if !opaque.is_empty() {
        let tail = if renumbered > 0 {
            format!(", and {renumbered} export(s) are renumbered underneath")
        } else {
            " (the removed exports are last in the table, so nothing else is renumbered)"
                .to_string()
        };
        warnings.push(format!(
            "{} store(s) references in data this editor does not decode (class layouts, bytecode, level data); one pointing at a removed export would be left dangling{tail}",
            opaque.join(", ")
        ));
    }
    let removed_paths: BTreeSet<String> = removed.iter().map(|&index| path_of(index)).collect();
    let soft: Vec<String> = kept
        .iter()
        .flat_map(|export| soft_references(export, &removed_paths))
        .collect();
    if !soft.is_empty() {
        warnings.push(format!(
            "soft reference(s) to a removed object will resolve to nothing at runtime: {}",
            soft.join(", ")
        ));
    }
    let public: Vec<String> = removed
        .iter()
        .filter_map(|&index| export_at(index))
        .filter(|export| export.object_flags & RF_PUBLIC != 0 || export.generate_public_hash)
        .map(|export| export.path.clone())
        .collect();
    if !public.is_empty() {
        warnings.push(format!(
            "{} {PUBLIC_WARNING}, which would then fail to find {}",
            public.join(", "),
            if public.len() == 1 { "it" } else { "them" }
        ));
    }

    Ok(RemovalPlan {
        removed: removed
            .iter()
            .filter_map(|&index| export_at(index))
            .map(|export| RemovedExport {
                index: export.index,
                path: export.path.clone(),
                class_name: export.class_name.clone(),
                requested: requested.contains(&export.index),
            })
            .collect(),
        blockers,
        cleared,
        warnings,
        renumbered,
        public,
        importers: Vec::new(),
        index_available: false,
    })
}

/// The pieces of a removal the writer applies: the export data to cut, the header tables with
/// every index renumbered, and the record of what changed.
pub(crate) struct Removal {
    pub splices: Vec<Splice>,
    /// The whole table, removed entries included, so the splice that deletes each one has an
    /// entry to charge. Offsets as read.
    pub exports: Vec<FObjectExport>,
    pub drop: Vec<usize>,
    pub preload_dependencies: Vec<FPackageIndex>,
    pub data_resources: Vec<FObjectDataResource>,
    pub applied: Vec<AppliedEdit>,
}

/// Turns an accepted plan into splices and header tables.
pub(crate) fn remove_exports(
    parsed: &ParsedPackage,
    header: &FLegacyPackageHeader,
    plan: &RemovalPlan,
) -> Result<Removal, String> {
    if !plan.blockers.is_empty() {
        return Err(format!(
            "these exports cannot be removed: {}",
            plan.blockers.join("; ")
        ));
    }
    let removed: BTreeSet<u32> = plan.indices().into_iter().collect();
    let renumber = crate::renumber::Renumber::removing_exports(removed.clone());

    let mut splices = Vec::new();
    let mut applied = Vec::new();
    for &index in &removed {
        let export = header
            .exports
            .get(index as usize)
            .ok_or_else(|| format!("the header has no export {index}"))?;
        let (start, end) = range(export)?;
        if end > start {
            splices.push(Splice {
                start,
                end,
                bytes: Vec::new(),
            });
        }
        let known = parsed.exports.get(index as usize);
        applied.push(AppliedEdit {
            name: known.map_or_else(|| format!("export {index}"), |e| e.path.clone()),
            offset: start,
            offset_after: start,
            element: None,
            elements_after: None,
            before: known.map(|e| e.class_name.clone()).unwrap_or_default(),
            after: "(removed)".into(),
        });
    }
    splices.extend(crate::renumber::reference_splices(
        parsed, header, &renumber, &removed,
    ));
    for cleared in &plan.cleared {
        applied.push(AppliedEdit {
            name: format!("{}.{}", cleared.export_name, cleared.property),
            offset: 0,
            offset_after: 0,
            element: None,
            elements_after: None,
            before: cleared.target.clone(),
            after: "None".into(),
        });
    }
    splices.sort_by_key(|splice| (splice.start, splice.end));

    let mut exports = header.exports.clone();
    let mut imports = header.imports.clone();
    let mut data_resources = header.data_resources.clone();
    crate::renumber::remap_table(&mut exports, &mut imports, &mut data_resources, &renumber);
    // A row that is about to be dropped keeps no dependencies of its own, so the table holds only
    // what the surviving exports still name.
    let preload_dependencies =
        crate::renumber::rebuild_runs(header, &mut exports, |position, runs| {
            if removed.contains(&(position as u32)) {
                return Ok(Default::default());
            }
            let mut kept: crate::renumber::Runs = Default::default();
            for (out, run) in kept.iter_mut().zip(runs) {
                for dep in run {
                    if renumber.drops(dep) {
                        continue;
                    }
                    out.push(renumber.remap(dep));
                }
            }
            Ok(kept)
        })?;

    Ok(Removal {
        splices,
        exports,
        drop: removed.iter().map(|&index| index as usize).collect(),
        preload_dependencies,
        data_resources,
        applied,
    })
}

/// Replaces an export's stored values with the header UE writes for an object that inherits
/// everything, leaving its guid word and whatever the class serializes after its properties.
pub(crate) fn reset_export(
    parsed: &ParsedPackage,
    index: u32,
) -> Result<(Splice, AppliedEdit), String> {
    let export = parsed.exports.get(index as usize).ok_or_else(|| {
        format!(
            "this package has {} exports, so there is no export {index}",
            parsed.exports.len()
        )
    })?;
    if export.data_table.is_some() {
        return Err(format!(
            "{} is a DataTable, and RowStruct is one of the properties a reset would drop; edit its rows instead",
            export.object_name
        ));
    }
    let end = export.properties_end.ok_or_else(|| {
        format!(
            "{}'s properties did not decode, so there is nothing to reset",
            export.object_name
        )
    })?;
    let slots = export.schema_slots.ok_or_else(|| {
        format!(
            "{} stores tagged properties, which have no header to reset",
            export.object_name
        )
    })?;
    let start = u64::try_from(export.serial_offset)
        .map_err(|_| "an export declares a negative offset".to_string())?;
    let stored = export
        .properties
        .iter()
        .filter(|entry| !matches!(entry.value, PropertyValue::Unset { .. }))
        .count();
    if stored == 0 {
        return Err(format!(
            "{} already inherits every value",
            export.object_name
        ));
    }
    Ok((
        Splice {
            start,
            end,
            bytes: unversioned::empty_header(slots as usize),
        },
        AppliedEdit {
            name: export.path.clone(),
            offset: start,
            offset_after: start,
            element: None,
            elements_after: None,
            before: format!("{stored} stored value(s)"),
            after: "(inherited defaults)".into(),
        },
    ))
}

fn export_of(index: i32) -> Option<u32> {
    (index > 0).then(|| (index - 1) as u32)
}

fn range(export: &FObjectExport) -> Result<(u64, u64), String> {
    let start = u64::try_from(export.serial_offset)
        .map_err(|_| "an export declares a negative offset".to_string())?;
    let size = u64::try_from(export.serial_size)
        .map_err(|_| "an export declares a negative size".to_string())?;
    Ok((start, start + size))
}

fn owner_of(parsed: &ParsedPackage, at: u64) -> Option<&ParsedExport> {
    parsed.exports.iter().find(|export| {
        let start = u64::try_from(export.serial_offset).unwrap_or(u64::MAX);
        let size = u64::try_from(export.serial_size).unwrap_or(0);
        start <= at && at < start.saturating_add(size)
    })
}

/// Whether an export holds bytes the reader cannot account for, which may name exports without
/// this knowing where. An undecoded payload counts: it reads as exact only because its length
/// prefix puts the cursor back.
pub(crate) fn holds_undecoded_indices(export: &ParsedExport) -> bool {
    if !export.undecoded.is_empty() {
        return true;
    }
    match &export.status {
        // A script the disassembler followed to its end put every index it names on record, so
        // its export can be renumbered like any other. One that stopped cannot be trusted.
        ExportStatus::Payload { kind, .. } if *kind == "bytecode" => export
            .script
            .as_ref()
            .is_none_or(|script| !script.complete()),
        ExportStatus::Payload { kind, .. } => INDEX_BEARING.contains(kind),
        ExportStatus::Partial { .. } | ExportStatus::Failed { .. } => true,
        ExportStatus::Complete => false,
    }
}

fn contains(entry: &PropertyEntry, at: u64) -> bool {
    entry
        .span
        .is_some_and(|(start, end)| start <= at && at < end)
}

/// The property holding the bytes at `at`, as a dotted label from the export's root: struct fields
/// and container elements down to the value itself. `None` when no property covers the offset.
fn label_at(parsed: &ParsedPackage, export: &ParsedExport, at: u64) -> Option<String> {
    let mut labels = Vec::new();
    let mut entries: &[PropertyEntry] = &export.properties;
    if !entries.iter().any(|entry| contains(entry, at))
        && let Some(table) = &export.data_table
        && let Some(row) = table
            .rows
            .iter()
            .find(|row| row.fields.iter().any(|field| contains(field, at)))
    {
        labels.push(row.name.clone());
        entries = &row.fields;
    }
    while let Some(entry) = entries.iter().find(|entry| contains(entry, at)) {
        let mut label = entry.label();
        match &entry.value {
            PropertyValue::Struct { fields, .. } => {
                labels.push(label);
                entries = fields;
            }
            PropertyValue::Array { items } | PropertyValue::Set { items } => {
                let element = parsed
                    .containers
                    .iter()
                    .find(|layout| entry.span.is_some_and(|(start, _)| start == layout.at))
                    .and_then(|layout| {
                        layout
                            .elements
                            .iter()
                            .position(|&(start, end)| start <= at && at < end)
                    });
                if let Some(index) = element {
                    label.push_str(&format!("[{index}]"));
                }
                labels.push(label);
                match element.and_then(|index| items.get(index)) {
                    Some(PropertyValue::Struct { fields, .. }) => entries = fields,
                    _ => break,
                }
            }
            _ => {
                labels.push(label);
                break;
            }
        }
    }
    (!labels.is_empty()).then(|| labels.join("."))
}

/// Soft references in this export naming a removed object, labelled by export and property.
fn soft_references(export: &ParsedExport, removed_paths: &BTreeSet<String>) -> Vec<String> {
    let mut found = Vec::new();
    let mut visit = |label: String, value: &PropertyValue| {
        if let PropertyValue::SoftObject { path } = value
            && removed_paths
                .iter()
                .any(|removed| removed.eq_ignore_ascii_case(path))
        {
            found.push(format!("{}.{label}", export.object_name));
        }
    };
    walk_values(&export.properties, "", &mut visit);
    if let Some(table) = &export.data_table {
        for row in &table.rows {
            walk_values(&row.fields, &row.name, &mut visit);
        }
    }
    found
}

fn walk_values(
    entries: &[PropertyEntry],
    prefix: &str,
    visit: &mut impl FnMut(String, &PropertyValue),
) {
    for entry in entries {
        let label = if prefix.is_empty() {
            entry.label()
        } else {
            format!("{prefix}.{}", entry.label())
        };
        walk_value(&entry.value, &label, visit);
    }
}

fn walk_value(value: &PropertyValue, label: &str, visit: &mut impl FnMut(String, &PropertyValue)) {
    match value {
        PropertyValue::Struct { fields, .. } => walk_values(fields, label, visit),
        PropertyValue::Array { items } | PropertyValue::Set { items } => {
            for (index, item) in items.iter().enumerate() {
                walk_value(item, &format!("{label}[{index}]"), visit);
            }
        }
        PropertyValue::Map { entries } => {
            for (index, entry) in entries.iter().enumerate() {
                walk_value(&entry.key, &format!("{label}[{index}].key"), visit);
                walk_value(&entry.value, &format!("{label}[{index}]"), visit);
            }
        }
        other => visit(label.to_string(), other),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use retoc::legacy_asset::{
        EPackageFlags, FLegacyPackageFileSummary, FMinimalName, FPackageNameMap,
    };

    use super::*;
    use crate::package::{AssetBundle, PackageInfo, read_header};
    use crate::props::IndexRef;
    use crate::write::{HeaderDraft, rewrite};

    const HEADER: i64 = 0x400;

    fn export(index: u32, name: &str, outer: i32, at: i64, size: i64) -> ParsedExport {
        ParsedExport {
            index,
            object_name: name.into(),
            class_name: "Object".into(),
            serial_offset: at,
            serial_size: size,
            outer_index: outer,
            class_index: -1,
            super_index: 0,
            template_index: 0,
            object_flags: 0,
            generate_public_hash: false,
            path: format!("/Game/Test.{name}"),
            status: ExportStatus::Complete,
            properties: Vec::new(),
            properties_end: Some(at as u64 + 2),
            schema_slots: Some(4),
            data_table: None,
            string_table: None,
            struct_definition: None,
            trailing_hex: String::new(),
            note: None,
            script: None,
            undecoded: Vec::new(),
            super_struct_at: None,
            name_refs: Vec::new(),
        }
    }

    fn object(name: &str, at: u64, index: i32) -> PropertyEntry {
        PropertyEntry {
            name: name.into(),
            element: None,
            value: PropertyValue::Object {
                index,
                path: Some(format!("/Game/Test.Target{index}")),
            },
            span: Some((at, at + 4)),
            slot: None,
        }
    }

    fn package(exports: Vec<ParsedExport>, references: Vec<IndexRef>) -> ParsedPackage {
        ParsedPackage {
            info: PackageInfo {
                package_name: "/Game/Test".into(),
                cooked: true,
                unversioned_properties: true,
                name_count: 0,
                import_count: 1,
                export_count: exports.len(),
            },
            names: Vec::new(),
            imports: Vec::new(),
            exports,
            unresolved_structs: Vec::new(),
            property_kinds: Default::default(),
            schema_fixups: Vec::new(),
            missing_schemas: Vec::new(),
            header_check: Default::default(),
            containers: Vec::new(),
            unset: Vec::new(),
            references,
            string_tables: Vec::new(),
            instanced: Vec::new(),
            tables: Vec::new(),
            channels: Vec::new(),
            script_tokens: Default::default(),
            text_histories: Default::default(),
            twins: Vec::new(),
            resources: Vec::new(),
            dependencies: None,
        }
    }

    /// Three exports of 16 bytes each behind a 0x400 byte header: a class-like payload export,
    /// an owner, and a subobject the owner points at from its second value.
    fn three() -> ParsedPackage {
        let mut class = export(0, "Class", 0, HEADER, 16);
        class.status = ExportStatus::Payload {
            consumed: 8,
            payload_bytes: 8,
            kind: "class layout and bytecode",
        };
        let mut owner = export(1, "Owner", 0, HEADER + 16, 16);
        owner.properties = vec![
            object("First", HEADER as u64 + 18, -1),
            object("Sub", HEADER as u64 + 22, 3),
        ];
        let sub = export(2, "Sub", 2, HEADER + 32, 16);
        package(
            vec![class, owner, sub],
            vec![
                IndexRef {
                    at: HEADER as u64 + 18,
                    index: -1,
                },
                IndexRef {
                    at: HEADER as u64 + 22,
                    index: 3,
                },
            ],
        )
    }

    #[test]
    fn removing_the_last_export_clears_the_reference_to_it_and_renumbers_nothing() {
        let plan = plan_removal(&three(), &[2]).expect("plan");
        assert_eq!(plan.indices(), vec![2]);
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(plan.renumbered, 0);
        assert_eq!(plan.cleared.len(), 1);
        assert_eq!(plan.cleared[0].export_name, "Owner");
        assert_eq!(plan.cleared[0].property, "Sub");
        assert_eq!(plan.cleared[0].target, "/Game/Test.Sub");
        // The class export's tail is not decoded, so it is named even for a tail removal.
        assert_eq!(plan.warnings.len(), 1, "{:?}", plan.warnings);
        assert!(plan.warnings[0].contains("Class"), "{}", plan.warnings[0]);
        assert!(
            plan.warnings[0].contains("nothing else"),
            "{}",
            plan.warnings[0]
        );
    }

    #[test]
    fn a_subobject_goes_with_its_outer_and_later_exports_are_counted_as_renumbered() {
        let mut parsed = three();
        // Make Sub a subobject of Class rather than of Owner.
        parsed.exports[2].outer_index = 1;
        let plan = plan_removal(&parsed, &[0]).expect("plan");
        assert_eq!(plan.indices(), vec![0, 2]);
        assert!(plan.removed[0].requested);
        assert!(!plan.removed[1].requested);
        assert_eq!(plan.renumbered, 1, "Owner moves from 2 to 1");
        assert_eq!(plan.cleared.len(), 1);
        assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
    }

    #[test]
    fn an_export_whose_class_would_go_blocks_the_removal() {
        let mut parsed = three();
        parsed.exports[1].class_index = 1;
        let plan = plan_removal(&parsed, &[0]).expect("plan");
        assert_eq!(plan.blockers.len(), 1, "{:?}", plan.blockers);
        assert!(
            plan.blockers[0].contains("class of"),
            "{}",
            plan.blockers[0]
        );
    }

    #[test]
    fn a_public_export_and_a_soft_reference_to_it_are_warned_about() {
        let mut parsed = three();
        parsed.exports[2].object_flags = RF_PUBLIC;
        parsed.exports[1].properties.push(PropertyEntry {
            name: "Soft".into(),
            element: None,
            value: PropertyValue::SoftObject {
                path: "/game/test.sub".into(),
            },
            span: Some((HEADER as u64 + 26, HEADER as u64 + 30)),
            slot: None,
        });
        let mut plan = plan_removal(&parsed, &[2]).expect("plan");
        assert_eq!(plan.public, vec!["/Game/Test.Sub".to_string()]);
        assert!(!plan.index_available);
        assert!(
            plan.warnings.iter().any(|w| w.contains("Owner.Soft")),
            "{:?}",
            plan.warnings
        );
        assert!(
            plan.warnings.iter().any(|w| w.contains(PUBLIC_WARNING)),
            "{:?}",
            plan.warnings
        );
        plan.resolve_importers(|_| vec!["Marvel/Content/User.uasset".into()]);
        assert!(plan.index_available);
        assert_eq!(plan.importers[0].packages.len(), 1);
        assert!(
            plan.warnings
                .iter()
                .any(|w| w.contains("imported by 1 package(s)")),
            "{:?}",
            plan.warnings
        );
        assert!(
            !plan.warnings.iter().any(|w| w.contains(PUBLIC_WARNING)),
            "{:?}",
            plan.warnings
        );
        plan.resolve_importers(|_| Vec::new());
        assert!(
            !plan.warnings.iter().any(|w| w.contains("imported by")),
            "{:?}",
            plan.warnings
        );
        assert!(plan.importers[0].packages.is_empty());
    }

    #[test]
    fn removing_every_export_or_a_missing_one_is_refused() {
        let parsed = three();
        let plan = plan_removal(&parsed, &[0, 1, 2]).expect("plan");
        assert!(plan.blockers.iter().any(|b| b.contains("at least one")));
        assert!(plan_removal(&parsed, &[7]).is_err());
        assert!(plan_removal(&parsed, &[]).is_err());
    }

    #[test]
    fn a_reference_inside_a_struct_in_an_array_is_labelled_down_to_the_field() {
        let mut parsed = three();
        let at = HEADER as u64 + 24;
        parsed.exports[1].properties = vec![PropertyEntry {
            name: "Parts".into(),
            element: None,
            value: PropertyValue::Array {
                items: vec![
                    PropertyValue::Struct {
                        name: "Part".into(),
                        fields: vec![object("Target", at - 8, -1)],
                    },
                    PropertyValue::Struct {
                        name: "Part".into(),
                        fields: vec![object("Target", at, 3)],
                    },
                ],
            },
            span: Some((HEADER as u64 + 16, HEADER as u64 + 32)),
            slot: None,
        }];
        parsed.containers.push(crate::props::ContainerLayout {
            at: HEADER as u64 + 16,
            count_at: HEADER as u64 + 16,
            count_width: 4,
            elements: vec![
                (HEADER as u64 + 20, HEADER as u64 + 24),
                (HEADER as u64 + 24, HEADER as u64 + 28),
            ],
            element_kind: "Struct",
            default_element: None,
            element_is_enum: false,
            element_enum: None,
            default_name: None,
            default_recipe: None,
            keys: None,
        });
        parsed.references = vec![IndexRef { at, index: 3 }];
        let plan = plan_removal(&parsed, &[2]).expect("plan");
        assert_eq!(plan.cleared[0].property, "Parts[1].Target");
    }

    /// A reference the tail scanner found sits in no property, and clearing it is a warning in
    /// its own right.
    #[test]
    fn a_reference_from_a_class_layout_is_labelled_and_warned_about() {
        let mut parsed = three();
        parsed.references.push(IndexRef {
            at: HEADER as u64 + 12,
            index: 3,
        });
        let plan = plan_removal(&parsed, &[2]).expect("plan");
        let layout = plan
            .cleared
            .iter()
            .find(|c| c.export_name == "Class")
            .expect("the class export's reference");
        assert_eq!(layout.property, LAYOUT);
        assert!(
            plan.warnings
                .iter()
                .any(|w| w.contains("class layout points at a removed export")),
            "{:?}",
            plan.warnings
        );
    }

    #[test]
    fn a_reset_writes_the_all_skipped_header_and_refuses_a_data_table() {
        let mut parsed = three();
        parsed.exports[1].properties_end = Some(HEADER as u64 + 20);
        let (splice, done) = reset_export(&parsed, 1).expect("reset");
        assert_eq!(
            (splice.start, splice.end),
            (HEADER as u64 + 16, HEADER as u64 + 20)
        );
        assert_eq!(splice.bytes, unversioned::empty_header(4));
        assert!(done.before.starts_with("2 stored"), "{}", done.before);

        parsed.exports[1].data_table = Some(crate::datatable::DataTable {
            row_struct: "Row".into(),
            columns: Vec::new(),
            rows: Vec::new(),
            declared_rows: 0,
            truncated: None,
        });
        let error = reset_export(&parsed, 1).expect_err("refused");
        assert!(error.contains("RowStruct"), "{error}");
        let error = reset_export(&parsed, 2).expect_err("nothing stored");
        assert!(error.contains("already inherits"), "{error}");
    }

    const NAMES: &[&str] = &[
        "None",
        "Class",
        "Owner",
        "Sub",
        "Package",
        "Object",
        "/Script/CoreUObject",
    ];

    fn name(value: &str) -> FMinimalName {
        FMinimalName {
            index: NAMES.iter().position(|n| *n == value).expect("named") as i32,
            number: 0,
        }
    }

    /// A real header for the three-export package, with preload dependencies and a data resource
    /// pointing into it, serialized the way the writer expects to read it back.
    fn bundle() -> (Vec<u8>, Vec<u8>) {
        let mut summary = FLegacyPackageFileSummary {
            package_name: "/Game/Test".to_string(),
            ..Default::default()
        };
        summary.versioning_info.package_file_version =
            crate::package::FALLBACK_ENGINE_VERSION.package_file_version();
        summary.versioning_info.total_header_size = HEADER as i32;
        // Cooked packages always filter editor data, and retoc's summary writer relies on it: without
        // the flag it orders the persistent guid and the generations differently from its reader.
        summary.package_flags = EPackageFlags::Cooked as u32
            | EPackageFlags::FilterEditorOnly as u32
            | EPackageFlags::UsesUnversionedProperties as u32;
        let export_row =
            |name_of: &str, at: i64, outer: i32, first: i32, deps: [i32; 4]| FObjectExport {
                class_index: FPackageIndex::create_import(0),
                outer_index: FPackageIndex { index: outer },
                object_name: name(name_of),
                serial_offset: at,
                serial_size: 16,
                first_export_dependency_index: first,
                serialize_before_serialize_dependencies: deps[0],
                create_before_serialize_dependencies: deps[1],
                serialize_before_create_dependencies: deps[2],
                create_before_create_dependencies: deps[3],
                ..Default::default()
            };
        let header = FLegacyPackageHeader {
            summary,
            name_map: FPackageNameMap::create_from_names(
                NAMES.iter().map(|n| (*n).to_string()).collect(),
            ),
            imports: vec![retoc::legacy_asset::FObjectImport {
                class_package: name("/Script/CoreUObject"),
                class_name: name("Object"),
                outer_index: FPackageIndex::create_null(),
                object_name: name("Object"),
                is_optional: false,
            }],
            exports: vec![
                // Class depends on nothing; Owner on Class (serialize before serialize) and on
                // Sub (create before create); Sub on Owner.
                export_row("Class", 0, 0, -1, [0, 0, 0, 0]),
                export_row("Owner", 16, 0, 0, [1, 0, 0, 1]),
                export_row("Sub", 32, 2, 2, [0, 1, 0, 0]),
            ],
            preload_dependencies: vec![
                FPackageIndex::create_export(0),
                FPackageIndex::create_export(2),
                FPackageIndex::create_export(1),
            ],
            // Separate-file resources: the writer places inline ones by the index word before their
            // payload, which these synthetic exports do not carry.
            data_resources: vec![
                FObjectDataResource {
                    outer_index: FPackageIndex::create_export(1),
                    legacy_bulk_data_flags: 0x100,
                    ..Default::default()
                },
                FObjectDataResource {
                    outer_index: FPackageIndex::create_export(2),
                    legacy_bulk_data_flags: 0x100,
                    ..Default::default()
                },
            ],
            data_resource_version: Some(Default::default()),
            ..Default::default()
        };
        let mut asset = std::io::Cursor::new(Vec::new());
        header
            .serialize(
                &mut asset,
                Some(HEADER as usize),
                &retoc::logging::Log::no_log(),
            )
            .expect("serialize");
        let mut exports = Vec::new();
        for value in 0u8..48 {
            exports.push(value);
        }
        // Owner's second value points at Sub.
        exports[22..26].copy_from_slice(&3i32.to_le_bytes());
        (asset.into_inner(), exports)
    }

    #[test]
    fn removing_the_last_export_cuts_its_bytes_and_drops_what_pointed_at_it() {
        let (asset, exports) = bundle();
        let bundle = AssetBundle {
            asset: &asset,
            exports: &exports,
        };
        let header = read_header(&bundle).expect("header");
        let parsed = three();
        let plan = plan_removal(&parsed, &[2]).expect("plan");
        let removal = remove_exports(&parsed, &header, &plan).expect("removal");
        assert_eq!(
            removal.splices.len(),
            2,
            "the cut and the cleared reference"
        );

        let rewritten = rewrite(
            &bundle,
            &removal.splices,
            HeaderDraft {
                exports: Some(removal.exports),
                drop_exports: removal.drop,
                preload_dependencies: Some(removal.preload_dependencies),
                data_resources: Some(removal.data_resources),
                ..Default::default()
            },
        )
        .expect("rewrite");
        assert_eq!(rewritten.exports.len(), 32);
        assert_eq!(&rewritten.exports[22..26], &0i32.to_le_bytes());

        let after = read_header(&AssetBundle {
            asset: &rewritten.asset,
            exports: &rewritten.exports,
        })
        .expect("header");
        assert_eq!(after.exports.len(), 2);
        assert_eq!(after.exports[1].serial_offset, HEADER + 16);
        assert_eq!(after.exports[1].serial_size, 16);
        assert_eq!(
            after.preload_dependencies,
            vec![FPackageIndex::create_export(0)],
            "Owner's dependency on Sub is gone, Class stays"
        );
        assert_eq!(after.exports[1].first_export_dependency_index, 0);
        assert_eq!(after.exports[1].serialize_before_serialize_dependencies, 1);
        assert_eq!(after.exports[1].create_before_create_dependencies, 0);
        assert_eq!(after.data_resources.len(), 1);
        assert_eq!(after.summary.bulk_data_start_offset, HEADER + 32);
    }

    #[test]
    fn removing_the_first_export_renumbers_everything_behind_it() {
        let (asset, exports) = bundle();
        let bundle = AssetBundle {
            asset: &asset,
            exports: &exports,
        };
        let header = read_header(&bundle).expect("header");
        let parsed = three();
        let plan = plan_removal(&parsed, &[0]).expect("plan");
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        let removal = remove_exports(&parsed, &header, &plan).expect("removal");
        let rewritten = rewrite(
            &bundle,
            &removal.splices,
            HeaderDraft {
                exports: Some(removal.exports),
                drop_exports: removal.drop,
                preload_dependencies: Some(removal.preload_dependencies),
                data_resources: Some(removal.data_resources),
                ..Default::default()
            },
        )
        .expect("rewrite");
        // Owner's reference to Sub now reads 2: Sub moved from export 3 to export 2.
        assert_eq!(&rewritten.exports[6..10], &2i32.to_le_bytes());
        let after = read_header(&AssetBundle {
            asset: &rewritten.asset,
            exports: &rewritten.exports,
        })
        .expect("header");
        assert_eq!(after.exports.len(), 2);
        assert_eq!(after.exports[0].serial_offset, HEADER);
        assert_eq!(after.exports[1].serial_offset, HEADER + 16);
        assert_eq!(
            after.exports[1].outer_index,
            FPackageIndex::create_export(0)
        );
        assert_eq!(
            after.preload_dependencies,
            vec![
                FPackageIndex::create_export(1),
                FPackageIndex::create_export(0)
            ],
            "Owner's dependency on Class is gone; Owner and Sub renumbered"
        );
        assert_eq!(after.exports[0].serialize_before_serialize_dependencies, 0);
        assert_eq!(after.exports[0].create_before_create_dependencies, 1);
        assert_eq!(after.exports[1].first_export_dependency_index, 1);
        assert_eq!(after.data_resources.len(), 2);
        assert_eq!(
            after.data_resources[0].outer_index,
            FPackageIndex::create_export(0)
        );
    }
}
