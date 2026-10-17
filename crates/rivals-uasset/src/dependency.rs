//! Edits the preload dependency runs an export declares, which are the order the loader builds a
//! package in.
//!
//! Zen turns these runs into a graph with two nodes per export, create and serialize, and refuses
//! a package whose graph has a cycle. So an edit that reads as four harmless index lists can make
//! a package that converts nowhere and loads nowhere; every change here is checked against the
//! whole graph before it is written.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use retoc::legacy_asset::FLegacyPackageHeader;
use retoc::zen::FPackageIndex;
use serde::{Deserialize, Serialize};

use crate::package::ParsedPackage;
use crate::renumber::rebuild_runs;

/// The four runs an export declares, as raw `FPackageIndex` values.
///
/// A "create before serialize" entry says the target object must exist before this one's bytes are
/// read, which is what a reference to it needs; "serialize before create" is the stronger form
/// used where the target's values decide how this one is built.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Runs {
    #[serde(default)]
    pub serialize_before_serialize: Vec<i32>,
    #[serde(default)]
    pub create_before_serialize: Vec<i32>,
    #[serde(default)]
    pub serialize_before_create: Vec<i32>,
    #[serde(default)]
    pub create_before_create: Vec<i32>,
}

impl Runs {
    fn lists(&self) -> [&Vec<i32>; 4] {
        [
            &self.serialize_before_serialize,
            &self.create_before_serialize,
            &self.serialize_before_create,
            &self.create_before_create,
        ]
    }

    pub fn is_empty(&self) -> bool {
        self.lists().iter().all(|run| run.is_empty())
    }

    /// The runs as `rebuild_runs` hands them round, in the same order.
    fn indices(&self) -> [Vec<FPackageIndex>; 4] {
        let mut out: [Vec<FPackageIndex>; 4] = Default::default();
        for (slot, run) in out.iter_mut().zip(self.lists()) {
            *slot = run.iter().map(|&index| FPackageIndex { index }).collect();
        }
        out
    }

    fn from_indices(runs: &[Vec<FPackageIndex>; 4]) -> Self {
        let read = |at: usize| runs[at].iter().map(|index| index.index).collect();
        Self {
            serialize_before_serialize: read(0),
            create_before_serialize: read(1),
            serialize_before_create: read(2),
            create_before_create: read(3),
        }
    }
}

/// Replaces one export's four runs outright. Adding to a run moves every run after it, so the
/// whole table is rewritten from whatever these edits leave.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyEdit {
    pub export: u32,
    pub runs: Runs,
}

/// What a set of dependency edits would do, and what stands in the way.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DependencyPlan {
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
    /// The cycle the edited graph holds, as object paths, when it holds one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cycle: Option<Vec<String>>,
}

/// The runs every export declares, read out of the header's shared table.
pub fn runs_of(header: &FLegacyPackageHeader) -> Result<Vec<Runs>, String> {
    let mut out = Vec::with_capacity(header.exports.len());
    for export in &header.exports {
        let first = export.first_export_dependency_index;
        let mut cursor = usize::try_from(first).unwrap_or(0);
        let mut held: [Vec<FPackageIndex>; 4] = Default::default();
        let counts = [
            export.serialize_before_serialize_dependencies,
            export.create_before_serialize_dependencies,
            export.serialize_before_create_dependencies,
            export.create_before_create_dependencies,
        ];
        for (run, count) in held.iter_mut().zip(counts) {
            for _ in 0..usize::try_from(count).unwrap_or(0) {
                if first < 0 {
                    break;
                }
                let dep = header
                    .preload_dependencies
                    .get(cursor)
                    .copied()
                    .ok_or("the preload dependencies run past their table")?;
                cursor += 1;
                run.push(dep);
            }
        }
        out.push(Runs::from_indices(&held));
    }
    Ok(out)
}

/// Checks the edits against the tables and the graph they would leave, without writing anything.
pub fn plan_dependency_edits(
    parsed: &ParsedPackage,
    header: &FLegacyPackageHeader,
    edits: &[DependencyEdit],
) -> Result<DependencyPlan, String> {
    let mut plan = DependencyPlan::default();
    if edits.is_empty() {
        return Err("no export was named".into());
    }
    let mut seen: BTreeSet<u32> = BTreeSet::new();
    for edit in edits {
        let Some(export) = parsed.exports.get(edit.export as usize) else {
            plan.blockers
                .push(format!("this package has no export {}", edit.export));
            continue;
        };
        if !seen.insert(edit.export) {
            plan.blockers
                .push(format!("{} is edited twice in one save", export.path));
        }
        let names = [
            "serialize before serialize",
            "create before serialize",
            "serialize before create",
            "create before create",
        ];
        for (run, what) in edit.runs.lists().iter().zip(names) {
            let mut held: BTreeSet<i32> = BTreeSet::new();
            for &index in run.iter() {
                let target = FPackageIndex { index };
                if target.is_null() {
                    plan.blockers.push(format!(
                        "{}'s {what} run holds a null, which names nothing to wait for",
                        export.path
                    ));
                    continue;
                }
                if !held.insert(index) {
                    plan.blockers.push(format!(
                        "{}'s {what} run names {} twice",
                        export.path,
                        path_of(parsed, index)
                    ));
                }
                if target.is_export() {
                    let at = target.to_export_index();
                    if at as usize >= parsed.exports.len() {
                        plan.blockers.push(format!(
                            "{}'s {what} run names export {at}, which this package does not have",
                            export.path
                        ));
                        continue;
                    }
                    if at == edit.export {
                        plan.blockers.push(format!(
                            "{} cannot wait for itself in its {what} run",
                            export.path
                        ));
                    }
                } else if target.to_import_index() as usize >= parsed.imports.len() {
                    plan.blockers.push(format!(
                        "{}'s {what} run names import {}, which this package does not have",
                        export.path,
                        target.to_import_index()
                    ));
                }
            }
        }
        // The create-before-serialize run is what makes a referenced object exist in time. Losing
        // one it still points at leaves a reference the loader resolves to nothing.
        let referenced: BTreeSet<i32> = references_of(parsed, export);
        let kept: BTreeSet<i32> = edit.runs.create_before_serialize.iter().copied().collect();
        for lost in referenced.difference(&kept) {
            let was = runs_of(header)?
                .get(edit.export as usize)
                .is_some_and(|runs| runs.create_before_serialize.contains(lost));
            if was {
                plan.warnings.push(format!(
                    "{} still points at {}, which it will no longer wait for",
                    export.path,
                    path_of(parsed, *lost)
                ));
            }
        }
    }
    if plan.blockers.is_empty() {
        let mut runs = runs_of(header)?;
        for edit in edits {
            if let Some(slot) = runs.get_mut(edit.export as usize) {
                *slot = edit.runs.clone();
            }
        }
        if let Some(cycle) = check_acyclic(&runs) {
            plan.cycle = Some(cycle.iter().map(|node| node.render(parsed)).collect());
            plan.blockers.push(format!(
                "these runs make a cycle, which no package can load: {}",
                plan.cycle
                    .as_ref()
                    .map(|steps| steps.join(" -> "))
                    .unwrap_or_default()
            ));
        }
    }
    Ok(plan)
}

/// The table the edits leave, with every export's `first_export_dependency_index` moved to match.
pub(crate) fn apply_dependency_edits(
    header: &FLegacyPackageHeader,
    edits: &[DependencyEdit],
) -> Result<(Vec<retoc::legacy_asset::FObjectExport>, Vec<FPackageIndex>), String> {
    let wanted: BTreeMap<usize, &Runs> = edits
        .iter()
        .map(|edit| (edit.export as usize, &edit.runs))
        .collect();
    let mut exports = header.exports.clone();
    let table = rebuild_runs(header, &mut exports, |position, held| {
        Ok(match wanted.get(&position) {
            Some(runs) => runs.indices(),
            None => held,
        })
    })?;
    Ok((exports, table))
}

/// Whether an IoStore package can hold `index` as a preload dependency of export `export`. Zen keeps
/// an edge to another export of the package, or to an object inside another package that is not
/// native code: a script object always exists, a package on its own is nothing to wait for, and
/// the converter drops both.
pub fn zen_keeps(parsed: &ParsedPackage, export: u32, index: i32) -> bool {
    let target = FPackageIndex { index };
    if target.is_export() {
        return target.to_export_index() != export;
    }
    if !target.is_import() {
        return false;
    }
    parsed
        .imports
        .get(target.to_import_index() as usize)
        .is_some_and(|import| {
            import.outer_index != 0
                && !import.path.starts_with("/Script/")
                && !import.path.starts_with("/Engine/UnknownPackage")
        })
}

/// Holds a save that gave `export` the members `records` to the runs the cook would write: each
/// type they hold values of read first, each class an object member may hold created first, as far
/// as an IoStore package can hold them, and every other export's runs as they were.
pub(crate) fn check_member_runs(
    before: &ParsedPackage,
    after: &ParsedPackage,
    export: u32,
    records: &[&crate::ustruct::FieldRecord],
) -> Result<(), String> {
    let (Some(was), Some(now)) = (&before.dependencies, &after.dependencies) else {
        return Ok(());
    };
    for (at, (old, new)) in was.iter().zip(now).enumerate() {
        if at as u32 != export && old != new {
            return Err(format!(
                "export {at}'s load order changed, where only export {export}'s should have"
            ));
        }
    }
    let runs = now
        .get(export as usize)
        .ok_or("the export has no load order after saving")?;
    for record in records {
        let (serialize, create) = crate::field_record::record_dependencies(record);
        for index in serialize
            .into_iter()
            .filter(|&i| zen_keeps(after, export, i))
        {
            if !runs.serialize_before_serialize.contains(&index)
                && !runs.serialize_before_create.contains(&index)
            {
                return Err(format!(
                    "{} holds values of import {index}, which is not read before it",
                    record.name
                ));
            }
        }
        for index in create.into_iter().filter(|&i| zen_keeps(after, export, i)) {
            if !runs.lists().iter().any(|run| run.contains(&index)) {
                return Err(format!(
                    "{} may hold import {index}, which is not created before it",
                    record.name
                ));
            }
        }
    }
    Ok(())
}

/// The runs export `export` reads back with from an IoStore package written with `runs`: what zen
/// keeps, in order, then the outer, parent class, class and archetype the reader puts back when a
/// run lacks them.
pub fn zen_readback(parsed: &ParsedPackage, export: u32, runs: &Runs) -> Runs {
    let keep = |run: &[i32]| -> Vec<i32> {
        run.iter()
            .copied()
            .filter(|&index| zen_keeps(parsed, export, index))
            .collect()
    };
    let mut out = Runs {
        serialize_before_serialize: keep(&runs.serialize_before_serialize),
        create_before_serialize: keep(&runs.create_before_serialize),
        serialize_before_create: keep(&runs.serialize_before_create),
        create_before_create: keep(&runs.create_before_create),
    };
    if let Some(held) = parsed.exports.get(export as usize) {
        let imply = |run: &mut Vec<i32>, index: i32| {
            if index != 0 && !run.contains(&index) {
                run.push(index);
            }
        };
        imply(&mut out.create_before_create, held.outer_index);
        imply(&mut out.serialize_before_serialize, held.super_index);
        imply(&mut out.serialize_before_create, held.class_index);
        imply(&mut out.serialize_before_create, held.template_index);
    }
    out
}

/// Why an IoStore package cannot carry `runs` for export `export` as given: every entry the
/// converter drops and every one the reader puts back. Empty when the runs survive as written.
pub fn zen_losses(parsed: &ParsedPackage, export: u32, runs: &Runs) -> Vec<String> {
    // Waiting on its own creation is the edge every export has anyway, which cooked packages
    // sometimes spell out and the converter leaves implicit, so losing it changes nothing.
    let own = FPackageIndex::create_export(export).index;
    let implicit =
        |run: &[i32]| -> Vec<i32> { run.iter().copied().filter(|&i| i != own).collect() };
    let runs = &Runs {
        serialize_before_serialize: implicit(&runs.serialize_before_serialize),
        create_before_serialize: implicit(&runs.create_before_serialize),
        serialize_before_create: implicit(&runs.serialize_before_create),
        create_before_create: implicit(&runs.create_before_create),
    };
    let back = zen_readback(parsed, export, runs);
    if back == *runs {
        return Vec::new();
    }
    let path = parsed
        .exports
        .get(export as usize)
        .map(|held| held.path.clone())
        .unwrap_or_else(|| format!("export {export}"));
    let names = [
        "serialize before serialize",
        "create before serialize",
        "serialize before create",
        "create before create",
    ];
    let mut out = Vec::new();
    for ((asked, read), what) in runs.lists().iter().zip(back.lists()).zip(names) {
        if *asked == read {
            continue;
        }
        for &index in asked.iter() {
            if zen_keeps(parsed, export, index) {
                continue;
            }
            // Dropped, then put back from the export table: only its place in the run is lost.
            if read.contains(&index) {
                out.push(format!(
                    "{path}'s {what} run cannot hold {} where it was put: it is dropped in an IoStore package and read back last, since the export table names it",
                    path_of(parsed, index)
                ));
                continue;
            }
            out.push(format!(
                "{path}'s {what} run cannot hold {} in an IoStore package: {}",
                path_of(parsed, index),
                why_dropped(parsed, export, index)
            ));
        }
        for &index in read.iter().filter(|index| !asked.contains(index)) {
            let risk = if (FPackageIndex { index }).is_export() {
                ", while the package itself loses the edge that orders it"
            } else {
                ""
            };
            out.push(format!(
                "{path}'s {what} run reads {} back from an IoStore package whether or not it is listed, since the export table names it{risk}",
                path_of(parsed, index)
            ));
        }
    }
    out
}

fn why_dropped(parsed: &ParsedPackage, export: u32, index: i32) -> &'static str {
    let target = FPackageIndex { index };
    if target.is_export() && target.to_export_index() == export {
        return "an export does not wait on itself";
    }
    let import = target
        .is_import()
        .then(|| parsed.imports.get(target.to_import_index() as usize))
        .flatten();
    match import {
        Some(import) if import.path.starts_with("/Script/") => {
            "it is native code, which always exists before anything loads"
        }
        Some(import) if import.outer_index == 0 => {
            "it names a package rather than an object in one"
        }
        _ => "the converter keeps only exports and objects in other packages",
    }
}

/// One place an export's runs and what its bytes name disagree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EdgeFinding {
    /// The export names an object no run of it waits for, so the loader can read the reference
    /// before the object exists and resolve it to nothing.
    Unwaited {
        export: u32,
        target: i32,
        in_script: bool,
    },
    /// The export waits for another export to be created that nothing in its bytes names, which is
    /// what a link saved against the wrong export leaves behind.
    Unnamed { export: u32, target: i32 },
}

impl EdgeFinding {
    /// The census line the audit counts this finding under.
    pub fn census_line(&self) -> &'static str {
        match self {
            Self::Unwaited {
                in_script: true, ..
            } => "objects a script names with no edge",
            Self::Unwaited { .. } => "objects a value names with no edge",
            Self::Unnamed { .. } => "edges to an export nothing in the export names",
        }
    }

    pub fn render(&self, parsed: &ParsedPackage) -> String {
        match self {
            Self::Unwaited {
                export,
                target,
                in_script,
            } => format!(
                "{}'s {} names {}, which none of its runs waits for",
                path_of(parsed, FPackageIndex::create_export(*export).index),
                if *in_script { "script" } else { "values" },
                path_of(parsed, *target)
            ),
            Self::Unnamed { export, target } => format!(
                "{} waits for {} to be created, which nothing in it names",
                path_of(parsed, FPackageIndex::create_export(*export).index),
                path_of(parsed, *target)
            ),
        }
    }
}

/// Every export whose runs disagree with what its bytes name. Only exports read to their end are
/// held to it, since one read in part may name more than was read. Every package the game ships
/// passes. An edge to an import nothing names is not a finding: the cook leaves those everywhere,
/// for a component's class or a text's string table.
pub fn edge_findings(parsed: &ParsedPackage) -> Vec<EdgeFinding> {
    findings(parsed, false)
}

/// [`edge_findings`], and with `imports` also every edge to an import nothing in the export names.
fn findings(parsed: &ParsedPackage, imports: bool) -> Vec<EdgeFinding> {
    let Some(runs) = &parsed.dependencies else {
        return Vec::new();
    };
    let mut bounds: Vec<(u64, u64, usize)> = parsed
        .exports
        .iter()
        .enumerate()
        .map(|(at, export)| {
            let start = export.serial_offset.max(0) as u64;
            (start, start + export.serial_size.max(0) as u64, at)
        })
        .collect();
    bounds.sort_unstable();
    let mut named: Vec<Vec<(i32, bool)>> = vec![Vec::new(); parsed.exports.len()];
    for reference in &parsed.references {
        let slot = bounds.partition_point(|(start, _, _)| *start <= reference.at);
        let Some(&(_, end, at)) = slot.checked_sub(1).and_then(|slot| bounds.get(slot)) else {
            continue;
        };
        if reference.at >= end {
            continue;
        }
        let in_script = parsed.exports[at]
            .script
            .as_ref()
            .is_some_and(|script| script.start <= reference.at && reference.at < script.end);
        named[at].push((reference.index, in_script));
    }
    let export_of = |index: i32| {
        let target = FPackageIndex { index };
        target
            .is_export()
            .then(|| parsed.exports.get(target.to_export_index() as usize))
            .flatten()
    };
    let mut out = Vec::new();
    for (at, (export, held)) in parsed.exports.iter().zip(runs).enumerate() {
        let read_whole = match &export.status {
            crate::package::ExportStatus::Complete => true,
            crate::package::ExportStatus::Payload { kind, .. } => {
                *kind == "bytecode"
                    && export
                        .script
                        .as_ref()
                        .is_some_and(crate::kismet::Script::complete)
            }
            _ => false,
        };
        if !read_whole {
            continue;
        }
        let position = at as u32;
        let own = FPackageIndex::create_export(position).index;
        let table = [
            export.class_index,
            export.super_index,
            export.template_index,
            export.outer_index,
        ];
        let waited = |index: i32| {
            table.contains(&index) || held.lists().iter().any(|run| run.contains(&index))
        };
        let mut seen: BTreeSet<i32> = BTreeSet::new();
        for &(index, in_script) in &named[at] {
            // A class names its own objects, its default one above all, which are built from it and
            // so cannot be waited for.
            let instance = export_of(index).is_some_and(|object| object.class_index == own);
            if seen.insert(index)
                && zen_keeps(parsed, position, index)
                && !waited(index)
                && !instance
            {
                out.push(EdgeFinding::Unwaited {
                    export: position,
                    target: index,
                    in_script,
                });
            }
        }
        let unnamed = |index: i32| !named[at].iter().any(|(named, _)| *named == index);
        // A text names its string table by the table's id, not by an index, and the edge to the
        // table is how the loader has it ready for the text.
        let string_table = |index: i32| {
            parsed
                .imports
                .get((-index - 1) as usize)
                .is_some_and(|import| import.class_name == "StringTable")
        };
        for &index in &held.create_before_serialize {
            let Some(target) = export_of(index) else {
                if imports
                    && zen_keeps(parsed, position, index)
                    && !table.contains(&index)
                    && unnamed(index)
                    && !string_table(index)
                {
                    out.push(EdgeFinding::Unnamed {
                        export: position,
                        target: index,
                    });
                }
                continue;
            };
            // The cook waits for an object's own subobjects whether or not anything in it names
            // them.
            if index != own
                && target.outer_index != own
                && !table.contains(&index)
                && unnamed(index)
            {
                out.push(EdgeFinding::Unnamed {
                    export: position,
                    target: index,
                });
            }
        }
    }
    out
}

/// An object by its place inside its package, so a renamed or moved package still compares, and
/// an import by its path.
fn place_of(parsed: &ParsedPackage, index: i32) -> String {
    let path = path_of(parsed, index);
    match path.strip_prefix(parsed.info.package_name.as_str()) {
        Some(rest) if (FPackageIndex { index }).is_export() => rest.to_string(),
        _ => path,
    }
}

/// The edges a save left waiting for what their export no longer names, as `(export, target)` in
/// `after`: the export was there before, waited for the object then, and named it then. The edit
/// that took out the last value naming the object is what left the wait with no reason, so the
/// wait can go with it. An edge the export had to what it never named is the cook's, and stays.
pub fn edges_left_unnamed(before: &ParsedPackage, after: &ParsedPackage) -> Vec<(u32, i32)> {
    let Some(runs) = &before.dependencies else {
        return Vec::new();
    };
    let key = |parsed: &ParsedPackage, export: u32, target: i32| {
        (
            place_of(parsed, FPackageIndex::create_export(export).index),
            place_of(parsed, target),
        )
    };
    let waited: BTreeSet<(String, String)> = runs
        .iter()
        .enumerate()
        .flat_map(|(export, held)| {
            held.create_before_serialize
                .iter()
                .map(move |&target| key(before, export as u32, target))
        })
        .collect();
    let unnamed: BTreeSet<(String, String)> = findings(before, true)
        .into_iter()
        .filter_map(|finding| match finding {
            EdgeFinding::Unnamed { export, target } => Some(key(before, export, target)),
            EdgeFinding::Unwaited { .. } => None,
        })
        .collect();
    findings(after, true)
        .into_iter()
        .filter_map(|finding| match finding {
            EdgeFinding::Unnamed { export, target } => {
                let was = key(after, export, target);
                (waited.contains(&was) && !unnamed.contains(&was)).then_some((export, target))
            }
            EdgeFinding::Unwaited { .. } => None,
        })
        .collect()
}

/// What a save added to a package's load-order problems, each as its finding reads in `after`. The
/// package needs no export to keep its place or its name, so problems are counted rather than
/// matched one for one:
/// - an object named with no edge, and an edge to another export nothing names, never belong in a
///   package, so neither may grow in number anywhere in it;
/// - an edge to an import nothing names is one the cook writes everywhere, so only an export the
///   package already had may not gain one. A copy of an export carries its source's.
pub fn introduced_edge_findings(before: &ParsedPackage, after: &ParsedPackage) -> Vec<String> {
    #[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Hash)]
    enum Kind {
        Unwaited,
        ToExport,
        ToImport,
    }
    /// A finding's kind, its export's place, its target's place, and the line it reads as.
    type Keyed = (Kind, String, String, String);
    /// Where each kind's findings sit in a list, in all and for each export, in list order.
    type Groups<'a> = HashMap<(Kind, Option<&'a str>), Vec<usize>>;
    fn group_of<'a, 'k>(
        groups: &'a Groups<'k>,
        kind: Kind,
        export: Option<&'k str>,
    ) -> &'a [usize] {
        groups.get(&(kind, export)).map_or(&[], Vec::as_slice)
    }
    // Grouped once, so a package with many findings is not walked again for each export.
    fn grouped(list: &[Keyed]) -> Groups<'_> {
        let mut groups: Groups<'_> = HashMap::new();
        for (at, (kind, held, _, _)) in list.iter().enumerate() {
            groups.entry((*kind, None)).or_default().push(at);
            groups
                .entry((*kind, Some(held.as_str())))
                .or_default()
                .push(at);
        }
        groups
    }
    let kind = |finding: &EdgeFinding| match finding {
        EdgeFinding::Unwaited { .. } => Kind::Unwaited,
        EdgeFinding::Unnamed { target, .. } if (FPackageIndex { index: *target }).is_export() => {
            Kind::ToExport
        }
        EdgeFinding::Unnamed { .. } => Kind::ToImport,
    };
    let place = place_of;
    let keyed = |parsed: &ParsedPackage| -> Vec<Keyed> {
        findings(parsed, true)
            .iter()
            .map(|finding| {
                let (export, target) = match finding {
                    EdgeFinding::Unwaited { export, target, .. }
                    | EdgeFinding::Unnamed { export, target } => (*export, *target),
                };
                (
                    kind(finding),
                    place(parsed, FPackageIndex::create_export(export).index),
                    place(parsed, target),
                    finding.render(parsed),
                )
            })
            .collect()
    };
    let (was, now) = (keyed(before), keyed(after));
    let had_export: BTreeSet<String> = before
        .exports
        .iter()
        .enumerate()
        .map(|(at, _)| place(before, FPackageIndex::create_export(at as u32).index))
        .collect();
    let (was_groups, now_groups) = (grouped(&was), grouped(&now));
    let mut out = Vec::new();
    // The findings `after` holds of one kind, and of one export when given, that `before` did not.
    let mut report = |wanted: Kind, export| {
        let mut old: HashMap<(&str, &str), usize> = HashMap::new();
        for &at in group_of(&was_groups, wanted, export) {
            let (_, held, target, _) = &was[at];
            *old.entry((held.as_str(), target.as_str())).or_default() += 1;
        }
        for &at in group_of(&now_groups, wanted, export) {
            let (_, held, target, line) = &now[at];
            match old.get_mut(&(held.as_str(), target.as_str())) {
                Some(left) if *left > 0 => *left -= 1,
                _ => out.push(line.clone()),
            }
        }
    };
    let grew = |wanted: Kind, export| {
        group_of(&now_groups, wanted, export).len() > group_of(&was_groups, wanted, export).len()
    };
    for wanted in [Kind::Unwaited, Kind::ToExport] {
        if grew(wanted, None) {
            report(wanted, None);
        }
    }
    let exports: BTreeSet<&String> = now
        .iter()
        .filter(|(kind, held, _, _)| *kind == Kind::ToImport && had_export.contains(held))
        .map(|(_, held, _, _)| held)
        .collect();
    for export in exports {
        if grew(Kind::ToImport, Some(export.as_str())) {
            report(Kind::ToImport, Some(export.as_str()));
        }
    }
    out
}

/// One half of an export's loading: zen builds every object before it reads any of their bytes
/// that need to, and the runs say which halves wait on which.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Node {
    Create(u32),
    Serialize(u32),
}

impl Node {
    fn export(self) -> u32 {
        match self {
            Self::Create(at) | Self::Serialize(at) => at,
        }
    }

    fn render(self, parsed: &ParsedPackage) -> String {
        let path = parsed
            .exports
            .get(self.export() as usize)
            .map(|export| export.path.clone())
            .unwrap_or_else(|| format!("export {}", self.export()));
        match self {
            Self::Create(_) => format!("create {path}"),
            Self::Serialize(_) => format!("serialize {path}"),
        }
    }
}

/// Whether the graph these runs describe can be walked at all, and the cycle it holds when it
/// cannot. Imports are leaves: they are loaded from another package, so nothing here waits on
/// anything of theirs.
pub(crate) fn check_acyclic(runs: &[Runs]) -> Option<Vec<Node>> {
    let mut edges: BTreeMap<Node, Vec<Node>> = BTreeMap::new();
    for (at, held) in runs.iter().enumerate() {
        let at = at as u32;
        // An object exists before its own bytes are read, always.
        edges
            .entry(Node::Serialize(at))
            .or_default()
            .push(Node::Create(at));
        let mut wait = |from: Node, index: i32, to: fn(u32) -> Node| {
            let target = FPackageIndex { index };
            if target.is_export() {
                edges
                    .entry(from)
                    .or_default()
                    .push(to(target.to_export_index()));
            }
        };
        for &index in &held.serialize_before_serialize {
            wait(Node::Serialize(at), index, Node::Serialize);
        }
        for &index in &held.create_before_serialize {
            wait(Node::Serialize(at), index, Node::Create);
        }
        for &index in &held.serialize_before_create {
            wait(Node::Create(at), index, Node::Serialize);
        }
        for &index in &held.create_before_create {
            wait(Node::Create(at), index, Node::Create);
        }
    }
    let mut state: BTreeMap<Node, u8> = BTreeMap::new();
    let mut path: Vec<Node> = Vec::new();
    for &start in edges.keys() {
        if let Some(cycle) = walk(start, &edges, &mut state, &mut path) {
            return Some(cycle);
        }
    }
    None
}

/// Depth-first with an explicit stack, since a package can hold more exports than the call stack
/// would take. `state`: 1 on the current path, 2 finished.
fn walk(
    start: Node,
    edges: &BTreeMap<Node, Vec<Node>>,
    state: &mut BTreeMap<Node, u8>,
    path: &mut Vec<Node>,
) -> Option<Vec<Node>> {
    if state.get(&start).is_some_and(|held| *held == 2) {
        return None;
    }
    let mut stack: Vec<(Node, usize)> = vec![(start, 0)];
    state.insert(start, 1);
    path.push(start);
    while let Some((node, step)) = stack.pop() {
        let next = edges.get(&node).and_then(|held| held.get(step)).copied();
        match next {
            Some(target) => {
                stack.push((node, step + 1));
                match state.get(&target).copied().unwrap_or(0) {
                    1 => {
                        let from = path.iter().position(|held| *held == target).unwrap_or(0);
                        let mut cycle = path[from..].to_vec();
                        cycle.push(target);
                        return Some(cycle);
                    }
                    2 => {}
                    _ => {
                        state.insert(target, 1);
                        path.push(target);
                        stack.push((target, 0));
                    }
                }
            }
            None => {
                state.insert(node, 2);
                path.pop();
            }
        }
    }
    None
}

/// The exports one export's bytes point at, which is what its create-before-serialize run exists
/// for. Imports are left out: nothing in this package builds them.
fn references_of(parsed: &ParsedPackage, export: &crate::package::ParsedExport) -> BTreeSet<i32> {
    let start = export.serial_offset.max(0) as u64;
    let end = start + export.serial_size.max(0) as u64;
    parsed
        .references
        .iter()
        .filter(|reference| start <= reference.at && reference.at < end)
        .map(|reference| reference.index)
        .filter(|index| FPackageIndex { index: *index }.is_export())
        .collect()
}

fn path_of(parsed: &ParsedPackage, index: i32) -> String {
    let target = FPackageIndex { index };
    if target.is_export() {
        parsed
            .exports
            .get(target.to_export_index() as usize)
            .map(|export| export.path.clone())
            .unwrap_or_else(|| format!("export {}", target.to_export_index()))
    } else if target.is_import() {
        parsed
            .imports
            .get(target.to_import_index() as usize)
            .map(|import| import.path.clone())
            .unwrap_or_else(|| format!("import {}", target.to_import_index()))
    } else {
        "None".to_string()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn runs(create_before_serialize: Vec<i32>) -> Runs {
        Runs {
            create_before_serialize,
            ..Default::default()
        }
    }

    /// An object exists before its own bytes are read, so a package whose exports only wait on
    /// each other's creation is always walkable.
    #[test]
    fn a_chain_of_creations_is_acyclic() {
        let table = vec![
            runs(vec![FPackageIndex::create_export(1).index]),
            runs(vec![FPackageIndex::create_export(2).index]),
            runs(Vec::new()),
        ];
        assert!(check_acyclic(&table).is_none());
    }

    /// Two exports that each need the other built first cannot both be built, and the cycle is
    /// reported rather than left for the converter to hit.
    #[test]
    fn a_creation_cycle_is_found() {
        let table = vec![
            Runs {
                create_before_create: vec![FPackageIndex::create_export(1).index],
                ..Default::default()
            },
            Runs {
                create_before_create: vec![FPackageIndex::create_export(0).index],
                ..Default::default()
            },
        ];
        let cycle = check_acyclic(&table).expect("a cycle");
        assert!(cycle.len() >= 2, "{cycle:?}");
    }

    /// An export waiting on its own creation before serializing is the implicit edge, not a cycle.
    #[test]
    fn waiting_on_your_own_creation_is_not_a_cycle() {
        let table = vec![runs(vec![FPackageIndex::create_export(0).index])];
        assert!(check_acyclic(&table).is_none());
    }

    /// An import is loaded from another package, so nothing in this one waits on it and it cannot
    /// take part in a cycle.
    #[test]
    fn an_import_is_a_leaf() {
        let table = vec![runs(vec![FPackageIndex::create_import(0).index])];
        assert!(check_acyclic(&table).is_none());
    }

    /// The runs a header holds read back as the lists an edit replaces, and a long chain does not
    /// exhaust the stack.
    #[test]
    fn a_deep_chain_is_walked_without_recursion() {
        let table: Vec<Runs> = (0..50_000u32)
            .map(|at| runs(vec![FPackageIndex::create_export(at + 1).index]))
            .collect();
        assert!(check_acyclic(&table).is_none());
    }

    /// A Blueprint whose every export waits for exactly the exports it names, then one of those
    /// edges moved to another export, as a link saved against the wrong export moves it: the
    /// package, the function that lost the edge, the export it names, and the export that gained it.
    fn moved_edge() -> (ParsedPackage, u32, i32, u32) {
        let mut parsed = crate::script_fixture::two_events_laid().parsed();
        let mut matching: Vec<Runs> = parsed
            .exports
            .iter()
            .enumerate()
            .map(|(at, export)| {
                let own = FPackageIndex::create_export(at as u32).index;
                runs(
                    references_of(&parsed, export)
                        .into_iter()
                        .filter(|&index| index != own)
                        .collect(),
                )
            })
            .collect();
        parsed.dependencies = Some(matching.clone());
        assert_eq!(edge_findings(&parsed), Vec::new());
        let (from, to) = parsed
            .exports
            .iter()
            .enumerate()
            .filter(|(_, export)| export.script.is_some())
            .find_map(|(at, export)| {
                matching[at]
                    .create_before_serialize
                    .iter()
                    .find(|&&index| index != export.class_index && index != export.outer_index)
                    .map(|&index| (at as u32, index))
            })
            .expect("a function naming another export");
        let wrong = (0..parsed.exports.len() as u32)
            .find(|&at| {
                at != from
                    && FPackageIndex::create_export(at).index != to
                    && !references_of(&parsed, &parsed.exports[at as usize]).contains(&to)
            })
            .expect("an export not naming it");
        matching[from as usize]
            .create_before_serialize
            .retain(|&index| index != to);
        matching[wrong as usize].create_before_serialize.push(to);
        parsed.dependencies = Some(matching);
        (parsed, from, to, wrong)
    }

    /// The edge is found from both ends: the function names an object it no longer waits for, and
    /// the export that gained the edge waits for an object it never names.
    #[test]
    fn an_edge_on_the_wrong_export_is_found_from_both_ends() {
        let (parsed, from, to, wrong) = moved_edge();
        let found = edge_findings(&parsed);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found.contains(&EdgeFinding::Unwaited {
            export: from,
            target: to,
            in_script: true,
        }));
        assert!(found.contains(&EdgeFinding::Unnamed {
            export: wrong,
            target: to,
        }));
    }

    /// What the cook itself writes is not a finding: an object waits for its own subobjects whether
    /// or not it names them, and a class names its objects without waiting for them. An export read
    /// only in part is not held to what was read of it.
    #[test]
    fn the_cook_s_own_edges_and_a_part_read_export_are_not_findings() {
        let (parsed, from, to, wrong) = moved_edge();
        let at = FPackageIndex { index: to }.to_export_index() as usize;

        let mut owned = parsed.clone();
        owned.exports[at].outer_index = FPackageIndex::create_export(wrong).index;
        assert!(
            !edge_findings(&owned)
                .iter()
                .any(|found| matches!(found, EdgeFinding::Unnamed { .. }))
        );

        let mut built = parsed.clone();
        built.exports[at].class_index = FPackageIndex::create_export(from).index;
        assert!(
            !edge_findings(&built)
                .iter()
                .any(|found| matches!(found, EdgeFinding::Unwaited { .. }))
        );

        let mut partial = parsed;
        for export in [from, wrong] {
            partial.exports[export as usize].status = crate::package::ExportStatus::Partial {
                consumed: 0,
                expected: 1,
            };
        }
        assert_eq!(edge_findings(&partial), Vec::new());
    }

    /// `parsed` with an import of an object in another package that its first function waits to
    /// create without naming it, the way the cook leaves a component waiting for its class.
    fn with_import_edge(mut parsed: ParsedPackage) -> (ParsedPackage, i32) {
        let mut thing = parsed.imports[0].clone();
        thing.path = "/Game/Mods/X.Thing".into();
        thing.outer_index = -1;
        thing.index = -(parsed.imports.len() as i32) - 1;
        let index = thing.index;
        parsed.imports.push(thing);
        if let Some(runs) = parsed.dependencies.as_mut() {
            runs[1].create_before_serialize.push(index);
        }
        (parsed, index)
    }

    /// A save that moves an edge onto the wrong export is caught from both ends, and one that leaves
    /// the package as it was is not.
    #[test]
    fn a_save_moving_an_edge_is_caught_and_one_changing_nothing_is_not() {
        let (moved, _, _, _) = moved_edge();
        let mut matching = moved.clone();
        matching.dependencies = Some(
            matching
                .exports
                .iter()
                .enumerate()
                .map(|(at, export)| {
                    let own = FPackageIndex::create_export(at as u32).index;
                    runs(
                        references_of(&matching, export)
                            .into_iter()
                            .filter(|&index| index != own)
                            .collect(),
                    )
                })
                .collect(),
        );
        assert_eq!(
            introduced_edge_findings(&matching, &matching),
            Vec::<String>::new()
        );
        let added = introduced_edge_findings(&matching, &moved);
        assert_eq!(added.len(), 2, "{added:?}");
        assert!(
            added
                .iter()
                .any(|line| line.contains("which none of its runs waits for"))
        );
        assert!(
            added
                .iter()
                .any(|line| line.contains("which nothing in it names"))
        );
        // Undoing it adds nothing.
        assert_eq!(
            introduced_edge_findings(&moved, &matching),
            Vec::<String>::new()
        );
    }

    /// The edges the cook leaves to imports nothing names are judged per export: one gained by an
    /// export the package had is caught, while one to a string table a text shows, a retargeted
    /// import and a copy carrying its source's edge are not.
    #[test]
    fn an_edge_to_an_import_is_caught_only_where_an_export_gains_one() {
        let (moved, _, _, _) = moved_edge();
        let (with, index) = with_import_edge(moved.clone());
        let mut without = with.clone();
        if let Some(runs) = without.dependencies.as_mut() {
            runs[1]
                .create_before_serialize
                .retain(|&held| held != index);
        }
        let gained = introduced_edge_findings(&without, &with);
        assert_eq!(gained.len(), 1, "{gained:?}");
        assert!(gained[0].contains("/Game/Mods/X.Thing"), "{gained:?}");

        let mut table = with.clone();
        table.imports[(-index - 1) as usize].class_name = "StringTable".into();
        let mut untabled = table.clone();
        if let Some(runs) = untabled.dependencies.as_mut() {
            runs[1]
                .create_before_serialize
                .retain(|&held| held != index);
        }
        assert_eq!(
            introduced_edge_findings(&untabled, &table),
            Vec::<String>::new()
        );

        let mut retargeted = with.clone();
        let at = (-index - 1) as usize;
        retargeted.imports[at].path = "/Game/Mods/Y.Thing".into();
        assert_eq!(
            introduced_edge_findings(&with, &retargeted),
            Vec::<String>::new()
        );

        let mut copied = with.clone();
        let mut copy = copied.exports[1].clone();
        copy.index = copied.exports.len() as u32;
        copy.object_name = "Copy".into();
        copy.path = format!("{}.Copy", copied.info.package_name);
        copy.serial_offset = copied
            .exports
            .iter()
            .map(|export| export.serial_offset + export.serial_size)
            .max()
            .unwrap_or(0);
        copy.serial_size = 0;
        copy.script = None;
        copy.status = crate::package::ExportStatus::Complete;
        copied.exports.push(copy);
        if let Some(runs) = copied.dependencies.as_mut() {
            runs.push(Runs {
                create_before_serialize: vec![index],
                ..Default::default()
            });
        }
        assert_eq!(
            introduced_edge_findings(&with, &copied),
            Vec::<String>::new()
        );
    }

    /// [`introduced_edge_findings`] as it was first written, walking every finding again for each
    /// export: the reference the counted one has to agree with, line for line.
    fn introduced_by_walking(before: &ParsedPackage, after: &ParsedPackage) -> Vec<String> {
        #[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
        enum Kind {
            Unwaited,
            ToExport,
            ToImport,
        }
        let kind = |finding: &EdgeFinding| match finding {
            EdgeFinding::Unwaited { .. } => Kind::Unwaited,
            EdgeFinding::Unnamed { target, .. }
                if (FPackageIndex { index: *target }).is_export() =>
            {
                Kind::ToExport
            }
            EdgeFinding::Unnamed { .. } => Kind::ToImport,
        };
        let place = place_of;
        let keyed = |parsed: &ParsedPackage| -> Vec<(Kind, String, String, String)> {
            findings(parsed, true)
                .iter()
                .map(|finding| {
                    let (export, target) = match finding {
                        EdgeFinding::Unwaited { export, target, .. }
                        | EdgeFinding::Unnamed { export, target } => (*export, *target),
                    };
                    (
                        kind(finding),
                        place(parsed, FPackageIndex::create_export(export).index),
                        place(parsed, target),
                        finding.render(parsed),
                    )
                })
                .collect()
        };
        let (was, now) = (keyed(before), keyed(after));
        let had_export: BTreeSet<String> = before
            .exports
            .iter()
            .enumerate()
            .map(|(at, _)| place(before, FPackageIndex::create_export(at as u32).index))
            .collect();
        let count = |list: &[(Kind, String, String, String)],
                     wanted: Kind,
                     export: Option<&str>| {
            list.iter()
                .filter(|(kind, held, _, _)| *kind == wanted && export.is_none_or(|e| e == held))
                .count()
        };
        let mut out = Vec::new();
        // The findings `after` holds of one kind, and of one export when given, that `before` did not.
        let mut report = |wanted: Kind, export: Option<&str>| {
            let mut old: Vec<(&String, &String)> = was
                .iter()
                .filter(|(kind, held, _, _)| *kind == wanted && export.is_none_or(|e| e == held))
                .map(|(_, held, target, _)| (held, target))
                .collect();
            for (kind, held, target, line) in &now {
                if *kind != wanted || export.is_some_and(|e| e != held) {
                    continue;
                }
                match old.iter().position(|pair| *pair == (held, target)) {
                    Some(at) => {
                        old.swap_remove(at);
                    }
                    None => out.push(line.clone()),
                }
            }
        };
        for wanted in [Kind::Unwaited, Kind::ToExport] {
            if count(&now, wanted, None) > count(&was, wanted, None) {
                report(wanted, None);
            }
        }
        let exports: BTreeSet<&String> = now
            .iter()
            .filter(|(kind, held, _, _)| *kind == Kind::ToImport && had_export.contains(held))
            .map(|(_, held, _, _)| held)
            .collect();
        for export in exports {
            if count(&now, Kind::ToImport, Some(export)) > count(&was, Kind::ToImport, Some(export))
            {
                report(Kind::ToImport, Some(export));
            }
        }
        out
    }

    /// Counting each export's findings once reports what walking them again for every export
    /// reported, in the same order: over a package and variants of it with edges moved, gained,
    /// lost and doubled, both ways round.
    #[test]
    fn counted_findings_read_as_walked_ones() {
        let (moved, _, _, _) = moved_edge();
        let (with, index) = with_import_edge(moved.clone());
        let base = with.clone();
        let count = base.exports.len() as u32;
        let mut variants = vec![moved, with];
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = |bound: u32| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((seed >> 33) as u32) % bound.max(1)
        };
        for _ in 0..24 {
            let mut variant = base.clone();
            let runs = variant.dependencies.as_mut().expect("runs");
            for _ in 0..1 + next(6) {
                let at = next(count) as usize;
                match next(4) {
                    0 => runs[at].create_before_serialize.push(index),
                    1 => {
                        let target = FPackageIndex::create_export(next(count)).index;
                        runs[at].create_before_serialize.push(target);
                        runs[at].create_before_serialize.push(target);
                    }
                    2 => runs[at].create_before_serialize.clear(),
                    _ => {
                        let held = &mut runs[at].create_before_serialize;
                        if !held.is_empty() {
                            let gone = next(held.len() as u32) as usize;
                            held.remove(gone);
                        }
                    }
                }
            }
            variants.push(variant);
        }
        let mut reported = 0;
        for before in &variants {
            for after in &variants {
                let counted = introduced_edge_findings(before, after);
                assert_eq!(counted, introduced_by_walking(before, after));
                reported += counted.len();
            }
        }
        assert!(reported > 0, "the variants differ in their findings");
    }
}
