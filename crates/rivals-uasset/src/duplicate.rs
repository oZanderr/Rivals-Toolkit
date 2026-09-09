//! Duplicates an export within its package: the export and its subobjects are copied to the end
//! of the export table, with every reference between them pointing at the copies.

use std::collections::BTreeMap;

use retoc::legacy_asset::{FLegacyPackageHeader, FObjectExport, FPackageNameMap};
use retoc::zen::FPackageIndex;
use serde::Serialize;

use crate::edit::{AppliedEdit, DuplicateExport};
use crate::package::ParsedPackage;

/// `RF_ClassDefaultObject`: a class's default object, of which a class has exactly one.
const RF_CLASS_DEFAULT_OBJECT: u32 = 0x10;

/// What duplicating one export entails: the set that is copied and where the copies land.
#[derive(Debug, Clone, Serialize)]
pub struct DuplicatePlan {
    pub root: u32,
    pub name: String,
    /// The root and its subobjects, in table order.
    pub members: Vec<u32>,
    /// The index each member's copy takes, in the same order.
    pub copies: Vec<u32>,
    /// Where the copy is listed as an actor, when the request asked for it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<LevelSlot>,
}

/// Where a level's actor list sits, so a copy can be appended to it.
#[derive(Debug, Clone, Serialize)]
pub struct LevelSlot {
    pub export: u32,
    /// The `i32` holding how many actors follow.
    pub count_at: u64,
    /// Where a new index goes: after the last actor, or right after the count when there are none.
    pub append_at: u64,
    /// How many actors the list holds now.
    pub actors: usize,
}

/// Works out what each request copies, refusing what could not be copied faithfully: a class,
/// function or struct (whose layout is not walked), a class default object (a class has one), a
/// member holding data this editor does not decode (its indices could not be pointed at the
/// copies), one holding inline bulk data (the table addresses it), and a name already in use under
/// the same outer.
pub fn plan_duplication(
    parsed: &ParsedPackage,
    requests: &[DuplicateExport],
) -> Result<Vec<DuplicatePlan>, String> {
    if requests.is_empty() {
        return Err("no export was named".into());
    }
    if !parsed.info.unversioned_properties {
        return Err(
            "this package stores tagged properties, whose references the reader does not track, so its exports cannot be duplicated"
                .into(),
        );
    }
    let mut plans: Vec<DuplicatePlan> = Vec::with_capacity(requests.len());
    let mut next = parsed.exports.len() as u32;
    for request in requests {
        let root = parsed.exports.get(request.export as usize).ok_or_else(|| {
            format!(
                "this package has {} exports, so there is no export {}",
                parsed.exports.len(),
                request.export
            )
        })?;
        let name = request.name.trim();
        if name.is_empty() {
            return Err("the copy needs a name".into());
        }
        if name.contains(['.', ':', '/', '\\']) {
            return Err(format!("{name} is not a plain object name"));
        }
        if is_type_like(&root.class_name) {
            return Err(format!(
                "{} is a {}, whose layout is not copied",
                root.path, root.class_name
            ));
        }
        let taken = parsed.exports.iter().any(|export| {
            export.outer_index == root.outer_index && export.object_name.eq_ignore_ascii_case(name)
        }) || plans.iter().any(|plan| {
            parsed.exports[plan.root as usize].outer_index == root.outer_index
                && plan.name.eq_ignore_ascii_case(name)
        });
        if taken {
            return Err(format!(
                "there is already an object named {name} beside {}",
                root.path
            ));
        }
        if plans.iter().any(|plan| plan.members.contains(&root.index)) {
            return Err(format!("{} is duplicated twice in one save", root.path));
        }

        let mut members = vec![root.index];
        loop {
            let more: Vec<u32> = parsed
                .exports
                .iter()
                .filter(|export| !members.contains(&export.index))
                .filter(|export| {
                    export_of(export.outer_index).is_some_and(|outer| members.contains(&outer))
                })
                .map(|export| export.index)
                .collect();
            if more.is_empty() {
                break;
            }
            members.extend(more);
        }
        members.sort_unstable();
        for &member in &members {
            let export = &parsed.exports[member as usize];
            if export.object_flags & RF_CLASS_DEFAULT_OBJECT != 0 {
                return Err(format!(
                    "{} is a class default object, of which a class has exactly one",
                    export.path
                ));
            }
            if crate::remove::holds_undecoded_indices(export) {
                return Err(format!(
                    "{} holds data this editor does not decode, whose references could not be pointed at the copies",
                    export.path
                ));
            }
            if parsed.resources.iter().any(|r| r.owner == Some(member)) {
                return Err(format!(
                    "{} holds inline bulk data, which the bulk table addresses by position",
                    export.path
                ));
            }
        }
        let level = match request.into_level {
            Some(level) => Some(level_slot(parsed, root, level)?),
            None => None,
        };
        let copies: Vec<u32> = (0..members.len() as u32).map(|at| next + at).collect();
        next += members.len() as u32;
        plans.push(DuplicatePlan {
            root: root.index,
            name: name.to_string(),
            members,
            copies,
            level,
        });
    }
    Ok(plans)
}

/// Where the copy of `root` would be listed in `level`'s actor list, refusing what the list could
/// not hold: anything but a level, an actor that does not sit in it, and a level whose tail the
/// reader could not follow far enough to find the list.
pub(crate) fn level_slot(
    parsed: &ParsedPackage,
    root: &crate::package::ParsedExport,
    level: u32,
) -> Result<LevelSlot, String> {
    let held = parsed
        .exports
        .get(level as usize)
        .ok_or_else(|| format!("this package has no export {level} to add the copy to"))?;
    if held.class_name != "Level" {
        return Err(format!(
            "{} is a {}, not a Level, so it has no actor list",
            held.path, held.class_name
        ));
    }
    if root.outer_index != FPackageIndex::create_export(level).index {
        return Err(format!(
            "{} does not sit in {}, and a level lists only its own actors",
            root.path, held.path
        ));
    }
    let actors = held
        .properties
        .iter()
        .find(|entry| entry.name == "Actors")
        .ok_or_else(|| format!("{}'s actor list was not read", held.path))?;
    let (count_at, _) = actors
        .span
        .ok_or_else(|| format!("{}'s actor list has no recorded span", held.path))?;
    let layout = parsed
        .containers
        .iter()
        .find(|layout| layout.at == count_at)
        .ok_or_else(|| format!("{}'s actor list was not recorded as a container", held.path))?;
    Ok(LevelSlot {
        export: level,
        count_at,
        append_at: layout.elements.last().map_or(count_at + 4, |(_, at)| *at),
        actors: layout.elements.len(),
    })
}

/// Classes, functions, structs and enums: their exports carry layouts the reader only walks.
fn is_type_like(class_name: &str) -> bool {
    class_name.ends_with("Class")
        || matches!(
            class_name,
            "Function"
                | "DelegateFunction"
                | "SparseDelegateFunction"
                | "ScriptStruct"
                | "UserDefinedStruct"
                | "Enum"
                | "UserDefinedEnum"
        )
}

/// The pieces of a duplication the writer applies: the table with the copies appended, the
/// dependency runs with theirs, the copies' bytes, and the record of what changed.
pub(crate) struct Duplication {
    pub exports: Vec<FObjectExport>,
    pub preload_dependencies: Vec<FPackageIndex>,
    pub appended: Vec<u8>,
    pub applied: Vec<AppliedEdit>,
}

/// Turns the plans into table entries and bytes. A copy keeps everything its source declares; the
/// root takes the new name, a subobject's outer becomes the copy of its outer, and every reference
/// into the set, in the bytes and in the dependency runs, is pointed at the copies.
pub(crate) fn duplicate_exports(
    parsed: &ParsedPackage,
    header: &FLegacyPackageHeader,
    exports_bytes: &[u8],
    names: &mut FPackageNameMap,
    plans: &[DuplicatePlan],
) -> Result<Duplication, String> {
    let total = i64::from(header.summary.versioning_info.total_header_size);
    let data_end = header
        .exports
        .iter()
        .map(|export| export.serial_offset + export.serial_size)
        .max()
        .unwrap_or(total);
    let mut exports = header.exports.clone();
    let mut preload_dependencies = header.preload_dependencies.clone();
    let mut appended = Vec::new();
    let mut applied = Vec::new();
    for plan in plans {
        let map: BTreeMap<u32, u32> = plan
            .members
            .iter()
            .copied()
            .zip(plan.copies.iter().copied())
            .collect();
        let renumber = crate::renumber::Renumber::copying(map.clone());
        let remap = |index: FPackageIndex| -> FPackageIndex { renumber.remap(index) };
        for (&member, &copy) in plan.members.iter().zip(&plan.copies) {
            let source = header
                .exports
                .get(member as usize)
                .ok_or_else(|| format!("the header has no export {member}"))?;
            if exports.len() != copy as usize {
                return Err("the duplication plan does not match the table".into());
            }
            let start = usize::try_from(source.serial_offset - total)
                .map_err(|_| "an export starts before the export data")?;
            let size = usize::try_from(source.serial_size)
                .map_err(|_| "an export declares a negative size")?;
            let mut bytes = exports_bytes
                .get(start..start + size)
                .ok_or("an export lies outside the export data")?
                .to_vec();
            for reference in &parsed.references {
                let Some(target) = export_of(reference.index) else {
                    continue;
                };
                let Some(&to) = map.get(&target) else {
                    continue;
                };
                let Some(at) = usize::try_from(reference.at)
                    .ok()
                    .and_then(|at| at.checked_sub(start + total as usize))
                else {
                    continue;
                };
                if at + 4 <= bytes.len() {
                    bytes[at..at + 4]
                        .copy_from_slice(&FPackageIndex::create_export(to).index.to_le_bytes());
                }
            }

            let mut entry = source.clone();
            if member == plan.root {
                entry.object_name = names.store(&plan.name);
            }
            entry.outer_index = remap(source.outer_index);
            entry.class_index = remap(source.class_index);
            entry.super_index = remap(source.super_index);
            entry.template_index = remap(source.template_index);
            entry.serial_offset = data_end + appended.len() as i64;

            let first = source.first_export_dependency_index;
            let mut cursor = usize::try_from(first).unwrap_or(0);
            let run_start = preload_dependencies.len() as i32;
            for count in [
                source.serialize_before_serialize_dependencies,
                source.create_before_serialize_dependencies,
                source.serialize_before_create_dependencies,
                source.create_before_create_dependencies,
            ] {
                for _ in 0..usize::try_from(count).unwrap_or(0) {
                    let dep = header
                        .preload_dependencies
                        .get(cursor)
                        .copied()
                        .ok_or("the preload dependencies run past their table")?;
                    cursor += 1;
                    preload_dependencies.push(remap(dep));
                }
            }
            if first >= 0 {
                entry.first_export_dependency_index = run_start;
            }
            appended.extend_from_slice(&bytes);
            exports.push(entry);
        }
        let root = &parsed.exports[plan.root as usize];
        let root_copy = plan
            .members
            .iter()
            .position(|&member| member == plan.root)
            .map_or(plan.copies[0], |at| plan.copies[at]);
        applied.push(AppliedEdit {
            name: plan.name.clone(),
            offset: data_end as u64,
            offset_after: data_end as u64,
            element: None,
            elements_after: None,
            before: "(none)".into(),
            after: format!(
                "copy of {} as export {root_copy}, with {} subobject(s)",
                root.path,
                plan.members.len() - 1
            ),
        });
    }
    Ok(Duplication {
        exports,
        preload_dependencies,
        appended,
        applied,
    })
}

fn export_of(index: i32) -> Option<u32> {
    (index > 0).then(|| (index - 1) as u32)
}
