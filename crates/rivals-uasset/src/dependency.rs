//! Edits the preload dependency runs an export declares, which are the order the loader builds a
//! package in.
//!
//! Zen turns these runs into a graph with two nodes per export, create and serialize, and refuses
//! a package whose graph has a cycle. So an edit that reads as four harmless index lists can make
//! a package that converts nowhere and loads nowhere; every change here is checked against the
//! whole graph before it is written.

use std::collections::{BTreeMap, BTreeSet};

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
}
