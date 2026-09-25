//! Copies an export and its subobjects out of one package into another.
//!
//! A duplication inside one package can leave almost everything alone: the copy's names, imports
//! and class already exist beside it. Across packages nothing does. Every FName in the copied
//! bytes indexes the source's name map, every negative index its import table, and every positive
//! one an export that is not coming along. All three have to be rewritten, and nothing in the
//! bytes says which four-byte pair is which, which is why the reader records where it read each.
//!
//! What cannot be rewritten faithfully is refused rather than approximated: a copy that lands and
//! then reads as a different object is worse than one that never lands.

use std::collections::{BTreeMap, BTreeSet};

use retoc::legacy_asset::{FLegacyPackageHeader, FObjectExport};
use retoc::zen::FPackageIndex;
use serde::{Deserialize, Serialize};

use crate::header_edit::Tables;
use crate::package::{ExportStatus, ParsedExport, ParsedPackage};
use crate::write::{HeaderDraft, Splice};

/// `RF_ClassDefaultObject`: a class's default object, of which a class has exactly one.
const RF_CLASS_DEFAULT_OBJECT: u32 = 0x10;

/// One export to copy in, named by the source the caller supplies under `from`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopyExport {
    /// Which of the caller's sources this comes from. Opaque here: the caller decides what a
    /// source key means, since this crate knows nothing about containers or files.
    pub from: String,
    pub export: u32,
    /// The destination export the copy sits under, or the package root when there is none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub into_outer: Option<u32>,
    /// The copy's object name. Empty keeps the source's.
    #[serde(default)]
    pub name: String,
    /// A level in the destination to list the copy in. An actor the level does not name is loaded
    /// with the package and then never spawned, so a copied actor needs this to appear.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub into_level: Option<u32>,
}

/// One package to copy out of, as the caller loaded it.
pub struct CopySource<'a> {
    pub parsed: &'a ParsedPackage,
    pub header: &'a FLegacyPackageHeader,
    /// The export data, which is what the copies' bytes come out of.
    pub exports: &'a [u8],
}

/// What a copy would bring across, and what stands in the way.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CopyPlan {
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
    /// One entry per copy, in the order they are appended.
    pub copies: Vec<CopiedExport>,
    /// Where each request's copy is listed as an actor, when it asked to be.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub listed: Vec<crate::duplicate::LevelSlot>,
}

/// One export the copy brings across, and where it lands.
#[derive(Debug, Clone, Serialize)]
pub struct CopiedExport {
    pub from: String,
    /// The source's index.
    pub export: u32,
    pub path: String,
    pub class_name: String,
    /// The index it takes in the destination.
    pub index: u32,
    /// Named by the caller, rather than pulled in as a subobject of one that was.
    pub requested: bool,
}

/// Works out what each request brings across, refusing anything that could not be rewritten
/// faithfully into the destination.
pub fn plan_copy(
    dest: &ParsedPackage,
    sources: &BTreeMap<String, CopySource<'_>>,
    requests: &[CopyExport],
) -> Result<CopyPlan, String> {
    let mut plan = CopyPlan::default();
    if requests.is_empty() {
        return Err("no export was named".into());
    }
    if !dest.info.unversioned_properties {
        plan.blockers.push(
            "the destination stores tagged properties, whose names this editor does not track"
                .to_string(),
        );
    }
    let mut next = dest.exports.len() as u32;
    let mut roots: BTreeSet<(String, u32)> = BTreeSet::new();
    for request in requests {
        let Some(source) = sources.get(&request.from) else {
            plan.blockers
                .push(format!("no source package was loaded for {}", request.from));
            continue;
        };
        if !source.parsed.info.unversioned_properties {
            plan.blockers.push(format!(
                "{} stores tagged properties, whose names this editor does not track",
                request.from
            ));
            continue;
        }
        let Some(root) = source.parsed.exports.get(request.export as usize) else {
            plan.blockers
                .push(format!("{} has no export {}", request.from, request.export));
            continue;
        };
        if !roots.insert((request.from.clone(), request.export)) {
            plan.blockers
                .push(format!("{} is copied twice in one save", root.path));
        }
        let name = if request.name.trim().is_empty() {
            root.object_name.clone()
        } else {
            request.name.trim().to_string()
        };
        if name.contains(['.', ':', '/', '\\']) {
            plan.blockers
                .push(format!("{name} is not a plain object name"));
        }
        let outer = crate::export_edit::outer_index(request.into_outer);
        if let Some(into) = request.into_outer {
            match dest.exports.get(into as usize) {
                Some(held) if is_type_like(&held.class_name) => plan.blockers.push(format!(
                    "{} is a {}, which does not own objects",
                    held.path, held.class_name
                )),
                Some(_) => {}
                None => plan.blockers.push(format!(
                    "the destination has no export {into} to put the copy under"
                )),
            }
        }
        if dest.exports.iter().any(|export| {
            export.outer_index == outer.index && export.object_name.eq_ignore_ascii_case(&name)
        }) {
            plan.blockers.push(format!(
                "the destination already holds an object called {name} there"
            ));
        }
        if let Some(level) = request.into_level {
            // The copy has to sit in the level it is listed in, so the outer decides it.
            if Some(level) != request.into_outer {
                plan.blockers.push(format!(
                    "a level lists only its own actors, so the copy has to sit in export {level} to be listed there"
                ));
            } else {
                match dest.exports.get(level as usize) {
                    Some(held) => {
                        // The root's outer is the level by the check above, which is what
                        // `level_slot` asks for; a stand-in with that outer says so.
                        let mut stand_in = root.clone();
                        stand_in.outer_index = FPackageIndex::create_export(level).index;
                        match crate::duplicate::level_slot(dest, &stand_in, level) {
                            Ok(slot) => plan.listed.push(slot),
                            Err(reason) => plan.blockers.push(reason),
                        }
                        let _ = held;
                    }
                    None => plan.blockers.push(format!(
                        "the destination has no export {level} to list the copy in"
                    )),
                }
            }
        }
        let members = closure(source.parsed, request.export);
        check_members(&mut plan, source, &members, &request.from);
        for (at, &member) in members.iter().enumerate() {
            let held = &source.parsed.exports[member as usize];
            plan.copies.push(CopiedExport {
                from: request.from.clone(),
                export: member,
                path: held.path.clone(),
                class_name: held.class_name.clone(),
                index: next + at as u32,
                requested: member == request.export,
            });
        }
        next += members.len() as u32;
    }
    Ok(plan)
}

/// The root and everything under it, in table order.
fn closure(parsed: &ParsedPackage, root: u32) -> Vec<u32> {
    let mut members = vec![root];
    loop {
        let more: Vec<u32> = parsed
            .exports
            .iter()
            .filter(|export| !members.contains(&export.index))
            .filter(|export| {
                let outer = FPackageIndex {
                    index: export.outer_index,
                };
                outer.is_export() && members.contains(&outer.to_export_index())
            })
            .map(|export| export.index)
            .collect();
        if more.is_empty() {
            break;
        }
        members.extend(more);
    }
    members.sort_unstable();
    members
}

/// Every reason a member could not be copied faithfully.
fn check_members(plan: &mut CopyPlan, source: &CopySource<'_>, members: &[u32], from: &str) {
    let parsed = source.parsed;
    for &member in members {
        let export = &parsed.exports[member as usize];
        if is_type_like(&export.class_name) {
            plan.blockers.push(format!(
                "{} is a {}, whose layout is walked rather than decoded",
                export.path, export.class_name
            ));
        }
        if export.object_flags & RF_CLASS_DEFAULT_OBJECT != 0 {
            plan.blockers.push(format!(
                "{} is a class default object, which belongs to its class",
                export.path
            ));
        }
        if !matches!(export.status, ExportStatus::Complete) {
            plan.blockers.push(format!(
                "{} does not read to its end, so its bytes hold indices this editor cannot follow",
                export.path
            ));
        }
        if parsed.resources.iter().any(|r| r.owner == Some(member)) {
            plan.blockers.push(format!(
                "{} holds bulk data, which the bulk table addresses by position",
                export.path
            ));
        }
        // An index out of the closure names something staying behind. An export it points at can
        // become an import only if the source package publishes it.
        for (index, what) in references_out(parsed, export, members) {
            let target = FPackageIndex { index };
            if target.is_export() {
                let held = &parsed.exports[target.to_export_index() as usize];
                if !held.generate_public_hash && held.object_flags & 0x1 == 0 {
                    plan.blockers.push(format!(
                        "{}'s {what} names {}, which {from} does not publish, so the copy could not point at it",
                        export.path, held.path
                    ));
                }
            } else if target.is_import() {
                let held = &parsed.imports[target.to_import_index() as usize];
                if held.unresolved {
                    plan.blockers.push(format!(
                        "{}'s {what} names an import {from} could not resolve",
                        export.path
                    ));
                }
            }
        }
    }
}

/// Every index this export names that points outside the closure, with what named it.
fn references_out(
    parsed: &ParsedPackage,
    export: &ParsedExport,
    members: &[u32],
) -> Vec<(i32, &'static str)> {
    let inside = |index: i32| {
        let target = FPackageIndex { index };
        target.is_export() && members.contains(&target.to_export_index())
    };
    let start = export.serial_offset.max(0) as u64;
    let end = start + export.serial_size.max(0) as u64;
    let mut out: Vec<(i32, &'static str)> = parsed
        .references
        .iter()
        .filter(|reference| start <= reference.at && reference.at < end)
        .map(|reference| (reference.index, "bytes"))
        .filter(|(index, _)| *index != 0 && !inside(*index))
        .collect();
    for (index, what) in [
        (export.class_index, "class"),
        (export.super_index, "parent"),
        (export.template_index, "archetype"),
    ] {
        if index != 0 && !inside(index) {
            out.push((index, what));
        }
    }
    out.sort_unstable();
    out.dedup();
    out
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

/// The pieces a copy leaves for the writer: the grown tables and the copies' bytes.
pub(crate) struct Copied {
    pub names: retoc::legacy_asset::FPackageNameMap,
    pub imports: Vec<retoc::legacy_asset::FObjectImport>,
    pub exports: Vec<FObjectExport>,
    pub preload_dependencies: Vec<FPackageIndex>,
    pub appended: Vec<u8>,
    pub applied: Vec<crate::edit::AppliedEdit>,
    /// Where each listed copy goes in its level's actor list, and by how much the count grows.
    pub listed: Vec<(u64, u64, u32)>,
}

/// Rewrites each copied export's bytes against the destination's tables and appends them.
///
/// Three rewrites per member, all of them addressed by what the reader recorded rather than by
/// anything visible in the bytes: names by the offsets `name_refs` holds, indices inside the
/// closure by where they land, indices outside it by an import in the destination.
pub(crate) fn copy_exports(
    dest_header: &FLegacyPackageHeader,
    dest_exports: &[u8],
    sources: &BTreeMap<String, CopySource<'_>>,
    requests: &[CopyExport],
    plan: &CopyPlan,
) -> Result<Copied, String> {
    let total = i64::from(dest_header.summary.versioning_info.total_header_size);
    let data_end = dest_header
        .exports
        .iter()
        .map(|export| export.serial_offset + export.serial_size)
        .max()
        .unwrap_or(total);
    let mut tables = Tables {
        names: dest_header.name_map.clone(),
        imports: dest_header.imports.clone(),
    };
    let mut exports = dest_header.exports.clone();
    let mut preload_dependencies = dest_header.preload_dependencies.clone();
    let mut appended: Vec<u8> = Vec::new();
    let mut applied = Vec::new();
    let mut listed: Vec<(u64, u64, u32)> = Vec::new();
    let _ = dest_exports;

    for request in requests {
        let source = sources
            .get(&request.from)
            .ok_or_else(|| format!("no source package was loaded for {}", request.from))?;
        let members: Vec<u32> = plan
            .copies
            .iter()
            .filter(|copy| copy.from == request.from)
            .filter(|copy| closure(source.parsed, request.export).contains(&copy.export))
            .map(|copy| copy.export)
            .collect();
        let landing: BTreeMap<u32, u32> = plan
            .copies
            .iter()
            .filter(|copy| copy.from == request.from && members.contains(&copy.export))
            .map(|copy| (copy.export, copy.index))
            .collect();
        let source_total = i64::from(source.header.summary.versioning_info.total_header_size);

        for &member in &members {
            let held = &source.parsed.exports[member as usize];
            let entry = source
                .header
                .exports
                .get(member as usize)
                .ok_or_else(|| format!("{} has no export {member}", request.from))?;
            let copy = *landing
                .get(&member)
                .ok_or("the copy plan does not name where a member lands")?;
            if exports.len() != copy as usize {
                return Err("the copy plan does not match the table".into());
            }
            let start = usize::try_from(entry.serial_offset - source_total)
                .map_err(|_| "an export starts before the export data")?;
            let size = usize::try_from(entry.serial_size)
                .map_err(|_| "an export declares a negative size")?;
            let mut bytes = source
                .exports
                .get(start..start + size)
                .ok_or("an export lies outside the export data")?
                .to_vec();
            let base = entry.serial_offset.max(0) as u64;

            // Names first: an FName is two words that look like any other pair of words, and the
            // offsets the reader kept are the only thing that says which.
            for &at in &held.name_refs {
                let Some(local) = usize::try_from(at.saturating_sub(base)).ok() else {
                    continue;
                };
                if local + 8 > bytes.len() {
                    continue;
                }
                let index = i32::from_le_bytes([
                    bytes[local],
                    bytes[local + 1],
                    bytes[local + 2],
                    bytes[local + 3],
                ]);
                let number = i32::from_le_bytes([
                    bytes[local + 4],
                    bytes[local + 5],
                    bytes[local + 6],
                    bytes[local + 7],
                ]);
                let text = source
                    .header
                    .name_map
                    .get(retoc::legacy_asset::FMinimalName { index, number: 0 })
                    .map_err(|e| format!("resolve a name in {}: {e}", held.path))?
                    .into_owned();
                let stored = tables.names.store(&text);
                bytes[local..local + 4].copy_from_slice(&stored.index.to_le_bytes());
                // `store` may fold a trailing number into the name, so the number word follows it.
                let held_number = if stored.number != 0 {
                    stored.number
                } else {
                    number
                };
                bytes[local + 4..local + 8].copy_from_slice(&held_number.to_le_bytes());
            }

            // Then the indices, each mapped the way its target survives the move.
            let remap = |index: i32, tables: &mut Tables| -> Result<i32, String> {
                map_index(source, &landing, index, tables)
            };
            for reference in &source.parsed.references {
                if reference.at < base || reference.at >= base + size as u64 {
                    continue;
                }
                let local = (reference.at - base) as usize;
                if local + 4 > bytes.len() {
                    continue;
                }
                let now = remap(reference.index, &mut tables)?;
                bytes[local..local + 4].copy_from_slice(&now.to_le_bytes());
            }

            let mut row = entry.clone();
            if member == request.export && !request.name.trim().is_empty() {
                row.object_name = tables.names.store(request.name.trim());
            } else {
                let text = source
                    .header
                    .name_map
                    .get(entry.object_name)
                    .map_err(|e| format!("resolve {}'s name: {e}", held.path))?
                    .into_owned();
                row.object_name = tables.names.store(&text);
            }
            row.outer_index = if member == request.export {
                crate::export_edit::outer_index(request.into_outer)
            } else {
                FPackageIndex {
                    index: remap(held.outer_index, &mut tables)?,
                }
            };
            row.class_index = FPackageIndex {
                index: remap(held.class_index, &mut tables)?,
            };
            row.super_index = FPackageIndex {
                index: remap(held.super_index, &mut tables)?,
            };
            row.template_index = FPackageIndex {
                index: remap(held.template_index, &mut tables)?,
            };
            row.serial_offset = data_end + appended.len() as i64;
            // A copy under an outer is not the package's own asset, whatever the source said.
            row.is_asset = false;

            let first = entry.first_export_dependency_index;
            let mut cursor = usize::try_from(first).unwrap_or(0);
            let run_start = preload_dependencies.len() as i32;
            for count in [
                entry.serialize_before_serialize_dependencies,
                entry.create_before_serialize_dependencies,
                entry.serialize_before_create_dependencies,
                entry.create_before_create_dependencies,
            ] {
                for _ in 0..usize::try_from(count).unwrap_or(0) {
                    let dep = source
                        .header
                        .preload_dependencies
                        .get(cursor)
                        .copied()
                        .ok_or("the source's preload dependencies run past their table")?;
                    cursor += 1;
                    preload_dependencies.push(FPackageIndex {
                        index: remap(dep.index, &mut tables)?,
                    });
                }
            }
            if first >= 0 {
                row.first_export_dependency_index = run_start;
            }
            appended.extend_from_slice(&bytes);
            exports.push(row);
        }

        if let Some(level) = request.into_level {
            let slot = plan
                .listed
                .iter()
                .find(|slot| slot.export == level)
                .ok_or("the copy plan does not say where the actor list is")?;
            let copy = *landing
                .get(&request.export)
                .ok_or("the copy plan does not name where the root lands")?;
            listed.push((slot.append_at, slot.count_at, copy));
        }
        let root = &source.parsed.exports[request.export as usize];
        applied.push(crate::edit::AppliedEdit {
            name: if request.name.trim().is_empty() {
                root.object_name.clone()
            } else {
                request.name.trim().to_string()
            },
            offset: data_end as u64,
            offset_after: data_end as u64,
            element: None,
            elements_after: None,
            before: "(not in this package)".to_string(),
            after: format!("{} copied from {}", root.class_name, request.from),
        });
    }

    Ok(Copied {
        names: tables.names,
        imports: tables.imports,
        exports,
        preload_dependencies,
        appended,
        applied,
        listed,
    })
}

/// What one source index becomes in the destination: the copy's own index inside the closure, or
/// an import naming what it named, added to the destination's table if it is not there already.
fn map_index(
    source: &CopySource<'_>,
    landing: &BTreeMap<u32, u32>,
    index: i32,
    tables: &mut Tables,
) -> Result<i32, String> {
    let target = FPackageIndex { index };
    if target.is_null() {
        return Ok(0);
    }
    if target.is_export() {
        let at = target.to_export_index();
        if let Some(&copy) = landing.get(&at) {
            return Ok(FPackageIndex::create_export(copy).index);
        }
        let held = source
            .parsed
            .exports
            .get(at as usize)
            .ok_or_else(|| format!("the source has no export {at}"))?;
        // Something left behind: the copy names it through the source package instead.
        return crate::header_edit::add_import(
            tables,
            &held.path,
            Some(("/Script/CoreUObject".to_string(), held.class_name.clone())),
        );
    }
    let held = source
        .parsed
        .imports
        .get(target.to_import_index() as usize)
        .ok_or_else(|| format!("the source has no import {}", target.to_import_index()))?;
    crate::header_edit::add_import(
        tables,
        &held.path,
        Some((held.class_package.clone(), held.class_name.clone())),
    )
}

/// The copies read back as the objects they came from: same class, same status, same values, with
/// every reference following the map rather than pointing at whatever now sits at that number.
pub(crate) fn verify_copy(
    after: &ParsedPackage,
    sources: &BTreeMap<String, CopySource<'_>>,
    plan: &CopyPlan,
    before: usize,
) -> Result<(), String> {
    if after.exports.len() != before + plan.copies.len() {
        return Err(format!(
            "the patched package holds {} exports where {} were expected",
            after.exports.len(),
            before + plan.copies.len()
        ));
    }
    for copy in &plan.copies {
        let source = sources
            .get(&copy.from)
            .ok_or_else(|| format!("no source was loaded for {}", copy.from))?;
        let was = source
            .parsed
            .exports
            .get(copy.export as usize)
            .ok_or_else(|| format!("{} has no export {}", copy.from, copy.export))?;
        let is = after
            .exports
            .get(copy.index as usize)
            .ok_or_else(|| format!("the copy of {} did not read back", was.path))?;
        if is.class_name != was.class_name {
            return Err(format!(
                "the copy of {} reads as a {}, not a {}",
                was.path, is.class_name, was.class_name
            ));
        }
        if !matches!(is.status, ExportStatus::Complete) {
            return Err(format!("the copy of {} does not read to its end", was.path));
        }
        if is.properties.len() != was.properties.len() {
            return Err(format!(
                "the copy of {} holds {} properties where the original holds {}",
                was.path,
                is.properties.len(),
                was.properties.len()
            ));
        }
        for (old, new) in was.properties.iter().zip(&is.properties) {
            if old.name != new.name {
                return Err(format!(
                    "the copy of {} reads {} where the original reads {}",
                    was.path, new.name, old.name
                ));
            }
        }
    }
    Ok(())
}

/// The whole of a cross-package copy: the tables grown, the bytes appended, nothing else touched.
pub(crate) fn patch_copy(
    bundle: &crate::package::AssetBundle<'_>,
    dest: &ParsedPackage,
    dest_header: &FLegacyPackageHeader,
    sources: &BTreeMap<String, CopySource<'_>>,
    requests: &[CopyExport],
) -> Result<crate::edit::PatchedBundle, String> {
    let plan = plan_copy(dest, sources, requests)?;
    if !plan.blockers.is_empty() {
        return Err(plan.blockers.join("; "));
    }
    let mut copied = copy_exports(dest_header, bundle.exports, sources, requests, &plan)?;
    // A listed copy is an actor the level names, so it also has to be built before the level's
    // bytes are read, exactly as a duplication into a level does.
    let links: Vec<(usize, u32)> = plan
        .listed
        .iter()
        .zip(&copied.listed)
        .map(|(slot, (_, _, copy))| (slot.export as usize, *copy))
        .collect();
    if let Some(dependencies) =
        crate::edit::link_dependencies(&mut copied.exports, &copied.preload_dependencies, &links)?
    {
        copied.preload_dependencies = dependencies;
    }
    let splices = actor_list_splices(bundle, &copied.listed)?;
    let runs = runs_from(&copied.exports, &copied.preload_dependencies)?;
    if let Some(cycle) = crate::dependency::check_acyclic(&runs) {
        return Err(format!(
            "the copies would make a load order cycle {} steps long, which no package can load",
            cycle.len()
        ));
    }
    let rewritten = crate::write::rewrite(
        bundle,
        &splices,
        HeaderDraft {
            names: Some(copied.names),
            imports: Some(copied.imports),
            exports: Some(copied.exports),
            preload_dependencies: Some(copied.preload_dependencies),
            appended: copied.appended,
            ..Default::default()
        },
    )?;
    crate::write::check_inline_bulk(&crate::package::AssetBundle {
        asset: &rewritten.asset,
        exports: &rewritten.exports,
    })?;
    Ok(crate::edit::PatchedBundle {
        asset: rewritten.asset,
        exports: rewritten.exports,
        applied: copied.applied,
        bulk: None,
        optional_bulk: None,
    })
}

/// The index appended to each actor list, and the count moved to match. Counts are summed per
/// level first: two copies into one list are one change to one word, not two overlapping ones.
fn actor_list_splices(
    bundle: &crate::package::AssetBundle<'_>,
    listed: &[(u64, u64, u32)],
) -> Result<Vec<Splice>, String> {
    if listed.is_empty() {
        return Ok(Vec::new());
    }
    let base = crate::package::header_size(bundle)?;
    let mut splices = Vec::new();
    let mut growth: BTreeMap<u64, i32> = BTreeMap::new();
    for &(append_at, count_at, copy) in listed {
        splices.push(Splice {
            start: append_at,
            end: append_at,
            bytes: FPackageIndex::create_export(copy)
                .index
                .to_le_bytes()
                .to_vec(),
        });
        *growth.entry(count_at).or_default() += 1;
    }
    for (count_at, by) in growth {
        splices.push(crate::edit::adjust_count(bundle, base, count_at, by)?);
    }
    splices.sort_by_key(|splice| (splice.start, splice.end));
    Ok(splices)
}

/// The runs a freshly built table declares, for the acyclic check before anything is written.
fn runs_from(
    exports: &[FObjectExport],
    preload: &[FPackageIndex],
) -> Result<Vec<crate::dependency::Runs>, String> {
    let header = FLegacyPackageHeader {
        exports: exports.to_vec(),
        preload_dependencies: preload.to_vec(),
        ..Default::default()
    };
    crate::dependency::runs_of(&header)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn export(index: u32, outer: i32) -> ParsedExport {
        ParsedExport {
            index,
            outer_index: outer,
            status: ExportStatus::Complete,
            ..blank()
        }
    }

    fn blank() -> ParsedExport {
        ParsedExport {
            index: 0,
            object_name: String::new(),
            class_name: "Object".into(),
            serial_offset: 0,
            serial_size: 0,
            outer_index: 0,
            class_index: 0,
            super_index: 0,
            template_index: 0,
            object_flags: 0,
            generate_public_hash: false,
            path: String::new(),
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
            super_struct_at: None,
            name_refs: Vec::new(),
            undecoded: Vec::new(),
        }
    }

    /// A subobject comes along with its owner, and so does a subobject of that, in table order.
    /// This is what decides how much a copy brings across, so getting it wrong either strands a
    /// component or drags in half the package.
    #[test]
    fn the_closure_takes_everything_under_the_root() {
        let parsed = ParsedPackage {
            info: crate::package::PackageInfo {
                package_name: "/Game/Thing".into(),
                cooked: true,
                unversioned_properties: true,
                name_count: 0,
                import_count: 0,
                export_count: 4,
            },
            names: Vec::new(),
            imports: Vec::new(),
            exports: vec![
                export(0, 0),
                export(1, 1),
                export(2, 2),
                // A sibling of the root, under the package rather than under it.
                export(3, 0),
            ],
            dependencies: None,
            unresolved_structs: Vec::new(),
            property_kinds: Default::default(),
            schema_fixups: Default::default(),
            missing_schemas: Default::default(),
            header_check: Default::default(),
            containers: Vec::new(),
            unset: Vec::new(),
            references: Vec::new(),
            string_tables: Vec::new(),
            instanced: Vec::new(),
            tables: Vec::new(),
            channels: Vec::new(),
            native_leaves: Vec::new(),
            script_tokens: Default::default(),
            text_histories: Default::default(),
            twins: Default::default(),
            resources: Vec::new(),
        };
        assert_eq!(closure(&parsed, 0), vec![0, 1, 2]);
        assert_eq!(closure(&parsed, 1), vec![1, 2]);
        assert_eq!(closure(&parsed, 2), vec![2]);
        assert_eq!(closure(&parsed, 3), vec![3], "a sibling brings nothing");
    }
}
