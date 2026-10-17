//! Renaming a Blueprint class's variable, function or local along with every use of it a package
//! holds.
//!
//! Nearly every use is an FName: the field record and the function map declaring the member, the
//! field paths and calls of the bytecode, the tags its objects store values under and the binding
//! objects pointing at it. Those are eight bytes each, rewritten in place to a name added to the
//! package's map, so nothing moves. The rest are the export or the import naming a function or a
//! widget, which the tables rename, and strings: a timer's function name, a widget binding's object
//! name. Those take a second pass, since a string of another length moves what follows it.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::edit::{
    AppliedEdit, EditOp, PackageEdits, PatchedBundle, ScriptConstEdit, Sidecars, ValueEdit,
    kind_of, patch_package_with,
};
use crate::mappings::Mappings;
use crate::members::{MemberKind, MemberTarget, MemberUse, UseRole, UseSite, member_uses};
use crate::package::{AssetBundle, ParsedExport, ParsedPackage, header_size, read_header};
use crate::write::{HeaderDraft, Splice, check_inline_bulk, rewrite};

/// What to do with uses only possibly of the member: by name on an object the package does not say
/// is of the class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PossibleUses {
    /// Refuse while there are any, listing them.
    #[default]
    Refuse,
    /// Rename them too.
    Follow,
    /// Leave them naming the old name.
    Leave,
}

/// Rename a member, and every use of it the package holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberRename {
    pub target: MemberTarget,
    pub to: String,
    pub possible: PossibleUses,
    /// Whether this package declares the member, where the new name must be free.
    pub declares: bool,
}

/// What a rename does in one package.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RenamePlan {
    /// The uses it renames.
    pub renamed: Vec<MemberUse>,
    /// The possible uses it leaves, when told to.
    pub left: Vec<MemberUse>,
}

/// Whether `name` can be an object or a field name.
fn plain_name(name: &str) -> bool {
    !name.is_empty() && !name.contains(['.', ':', '/', '\\', '"', '\''])
}

/// What the rename would change, refusing what it could not make safe.
pub fn plan_member_rename(
    bundle: &AssetBundle<'_>,
    parsed: &ParsedPackage,
    rename: &MemberRename,
    allow_unchecked: bool,
) -> Result<RenamePlan, String> {
    let target = &rename.target;
    let to = rename.to.trim();
    if !plain_name(to) {
        return Err(format!("{to:?} is not a name a member can take"));
    }
    if to.eq_ignore_ascii_case(&target.name) {
        return Err(format!("{} is already named that", target.name));
    }
    let uses = member_uses(bundle, parsed, target)?;
    if rename.declares {
        check_declaration(parsed, target, to, &uses.uses)?;
    }
    if !uses.unclassified.is_empty() && !allow_unchecked {
        return Err(format!(
            "{} hold(s) the name {} where this cannot tell what for, so renaming would leave them \
             naming nothing or rename something else: {}. Pass --allow-unchecked to rename the \
             uses it can place and leave these",
            uses.unclassified.len(),
            target.name,
            shown(&uses.unclassified)
        ));
    }
    if !uses.unread.is_empty() && !allow_unchecked {
        return Err(format!(
            "these exports were not read whole, so they may name {} where nothing records it: {}. \
             Pass --allow-unchecked to rename the rest",
            target.name,
            shown(&uses.unread)
        ));
    }
    let mut plan = RenamePlan {
        renamed: uses.uses.clone(),
        left: Vec::new(),
    };
    if !uses.possible.is_empty() {
        match rename.possible {
            PossibleUses::Refuse => {
                let labels: Vec<String> =
                    uses.possible.iter().map(|one| one.label.clone()).collect();
                return Err(format!(
                    "{} use(s) by name act on an object this package does not say is of {}, so \
                     they may or may not be {}: {}. Say --possible follow to rename them too, or \
                     --possible leave to keep them as they are",
                    labels.len(),
                    short_name(&target.class),
                    target.name,
                    shown(&labels)
                ));
            }
            // Code outside the packages, such as a Python parent's, takes no rename: it is left.
            PossibleUses::Follow => {
                plan.renamed
                    .extend(uses.possible.iter().filter(|one| followable(one)).cloned());
                plan.left = uses
                    .possible
                    .iter()
                    .filter(|one| !followable(one))
                    .cloned()
                    .collect();
            }
            PossibleUses::Leave => plan.left = uses.possible.clone(),
        }
    }
    // A string of another length moves what follows it, which a script that has to keep its size
    // cannot take.
    for one in &plan.renamed {
        if let UseSite::Script { export, .. } = one.site
            && to.len() != target.name.len()
            && let Some(lock) = parsed
                .exports
                .get(export as usize)
                .and_then(|e| e.script.as_ref())
                .and_then(|script| script.resize_lock())
        {
            return Err(format!(
                "{} names {} in a string, and its script has to keep its size ({lock}), so the \
                 new name has to be as long as the old one",
                one.label, target.name
            ));
        }
    }
    Ok(plan)
}

/// Whether a possible use sits in a package, where a rename can follow it, rather than in code
/// outside them.
fn followable(one: &MemberUse) -> bool {
    !matches!(one.role, crate::members::UseRole::PythonParent { .. })
}

fn short_name(path: &str) -> &str {
    path.rsplit(['.', ':', '/']).next().unwrap_or(path)
}

fn shown(items: &[String]) -> String {
    let mut list: Vec<&str> = items.iter().take(5).map(String::as_str).collect();
    let more = items.len().saturating_sub(list.len());
    let more_text;
    if more > 0 {
        more_text = format!("and {more} more");
        list.push(&more_text);
    }
    list.join("; ")
}

/// Refuses a rename the declaring package could not take: no such member, a new name already in
/// use beside it, an override, which the engine calls by its parent's name, and a component, whose
/// template and construction script node are named after it.
fn check_declaration(
    parsed: &ParsedPackage,
    target: &MemberTarget,
    to: &str,
    uses: &[MemberUse],
) -> Result<(), String> {
    if !uses
        .iter()
        .any(|u| matches!(u.role, UseRole::Declared | UseRole::Export))
    {
        return Err(format!(
            "{} declares no {} named {}",
            target.function.as_deref().unwrap_or(&target.class),
            match target.kind {
                MemberKind::Variable => "variable",
                MemberKind::Function => "function",
                MemberKind::Local => "parameter or local",
            },
            target.name
        ));
    }
    let owner = target.function.as_deref().unwrap_or(&target.class);
    let owner_export = parsed
        .exports
        .iter()
        .find(|e| e.path.eq_ignore_ascii_case(owner))
        .ok_or_else(|| format!("this package does not hold {owner}"))?;
    let taken_record = owner_export.layout.as_ref().is_some_and(|layout| {
        layout
            .records
            .iter()
            .any(|(record, _)| record.name.eq_ignore_ascii_case(to))
    });
    // Functions, widgets and anything else directly under the class share its namespace.
    let class_index = parsed
        .exports
        .iter()
        .find(|e| e.path.eq_ignore_ascii_case(&target.class))
        .map(|e| e.index as i32 + 1);
    let taken_object = target.kind != MemberKind::Local
        && parsed.exports.iter().any(|e| {
            e.object_name.eq_ignore_ascii_case(to)
                && (Some(e.outer_index) == class_index
                    || parsed
                        .exports
                        .get((e.outer_index as usize).wrapping_sub(1))
                        .is_some_and(|outer| {
                            outer.class_name == "WidgetTree"
                                && Some(outer.outer_index) == class_index
                        }))
        });
    if taken_record || taken_object {
        return Err(format!("{owner} already has something named {to}"));
    }
    if target.kind == MemberKind::Function {
        let function_path = format!("{}:{}", target.class, target.name);
        if let Some(function) = parsed
            .exports
            .iter()
            .find(|e| e.path.eq_ignore_ascii_case(&function_path))
            && function.super_index != 0
        {
            return Err(format!(
                "{} overrides its parent class's {}, which the engine calls by that name",
                target.name, target.name
            ));
        }
    }
    if uses
        .iter()
        .any(|u| matches!(&u.role, UseRole::Binding { field } if field == "InternalVariableName"))
    {
        return Err(format!(
            "{} is a component, whose template and construction script node go by its name; \
             add a component under the new name and remove this one instead",
            target.name
        ));
    }
    Ok(())
}

/// Renames the member and its uses in the package: every name in place in one rewrite, then the
/// strings, read again in between.
pub(crate) fn patch_member_rename(
    bundle: &AssetBundle<'_>,
    sidecars: Sidecars<'_>,
    parsed: &ParsedPackage,
    rename: &MemberRename,
    allow_unchecked: bool,
    mappings: Option<&Mappings>,
) -> Result<PatchedBundle, String> {
    let plan = plan_member_rename(bundle, parsed, rename, allow_unchecked)?;
    let target = &rename.target;
    let to = rename.to.trim();
    let header = read_header(bundle)?;
    let total = header_size(bundle)?;
    let mut names = header.name_map.clone();
    let renamed = names.store(to);
    let value_name = format!("{}_Value", target.name);
    let renamed_value = names.store(&format!("{to}_Value"));
    let mut exports = header.exports.clone();
    let mut imports = header.imports.clone();
    let mut splices = Vec::new();
    let mut applied = Vec::new();
    let mut strings = Vec::new();
    for one in &plan.renamed {
        match one.site {
            UseSite::Name { at } => {
                let local = at
                    .checked_sub(total)
                    .and_then(|local| usize::try_from(local).ok())
                    .ok_or_else(|| format!("a name is recorded at {at:#X}, in the header"))?;
                let bytes = bundle.exports.get(local..local + 8).ok_or_else(|| {
                    format!("a name is recorded at {at:#X}, past the export data")
                })?;
                let index = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                let held = header
                    .name_map
                    .raw_names()
                    .get(index as usize)
                    .cloned()
                    .unwrap_or_default();
                let name = if held.eq_ignore_ascii_case(&value_name) {
                    renamed_value
                } else {
                    renamed
                };
                splices.push(Splice {
                    start: at,
                    end: at + 8,
                    bytes: [name.index.to_le_bytes(), name.number.to_le_bytes()].concat(),
                });
            }
            UseSite::Export { index } => {
                let row = exports
                    .get_mut(index as usize)
                    .ok_or_else(|| format!("this package has no export {index}"))?;
                row.object_name = renamed;
            }
            UseSite::Import { index } => {
                let row = imports
                    .get_mut((-index - 1) as usize)
                    .ok_or_else(|| format!("this package has no import {index}"))?;
                row.object_name = renamed;
            }
            UseSite::Script { .. } | UseSite::Value { .. } => strings.push(one),
            UseSite::Index { .. } => continue,
        }
        applied.push(AppliedEdit {
            name: one.label.clone(),
            offset: match one.site {
                UseSite::Name { at } => at,
                _ => 0,
            },
            offset_after: 0,
            element: None,
            elements_after: None,
            before: target.name.clone(),
            after: to.to_string(),
        });
    }
    splices.sort_by_key(|splice| splice.start);
    splices.dedup_by_key(|splice| splice.start);
    let rewritten = rewrite(
        bundle,
        &splices,
        HeaderDraft {
            names: Some(names),
            exports: Some(exports),
            imports: Some(imports),
            ..Default::default()
        },
    )?;
    let renamed_bundle = AssetBundle {
        asset: &rewritten.asset,
        exports: &rewritten.exports,
    };
    check_inline_bulk(&renamed_bundle)?;
    let mut notes: Vec<String> = plan
        .left
        .iter()
        .map(|one| {
            format!(
                "{} still names {}: it may be another class's",
                one.label, target.name
            )
        })
        .collect();
    if strings.is_empty() {
        return Ok(PatchedBundle {
            asset: rewritten.asset,
            exports: rewritten.exports,
            applied,
            bulk: None,
            optional_bulk: None,
            notes,
        });
    }

    // The strings, against the package as the first pass left it: a grown name map moved every
    // value by the same amount, and a script's loaded offsets do not move at all.
    let shift = header_size(&renamed_bundle)? as i64 - total as i64;
    let again = crate::package::parse_package(&renamed_bundle, mappings)?;
    let mut edits = PackageEdits {
        allow_unchecked,
        ..Default::default()
    };
    for one in strings {
        match one.site {
            UseSite::Script { export, at } => {
                let script = parsed
                    .exports
                    .get(export as usize)
                    .and_then(|e| e.script.as_ref())
                    .ok_or_else(|| format!("export {export} holds no script"))?;
                let span = script
                    .spans
                    .iter()
                    .find(|span| span.at == at)
                    .ok_or_else(|| format!("no expression starts at {at:#X} in export {export}"))?;
                let statement = script
                    .statements
                    .iter()
                    .rev()
                    .find(|statement| statement.at <= at)
                    .ok_or_else(|| format!("no statement holds {at:#X}"))?;
                edits.scripts.push(ScriptConstEdit {
                    export,
                    statement: statement.offset,
                    at: Some(span.offset),
                    value: to.to_string(),
                    ..ScriptConstEdit::default()
                });
            }
            UseSite::Value { export, at } => {
                let moved = (at as i64 + shift) as u64;
                let field = match &one.role {
                    UseRole::Binding { field } => field.as_str(),
                    _ => "",
                };
                let entry =
                    crate::edit::entry_named_at(&again, moved, field, None).ok_or_else(|| {
                        format!(
                            "no {field} starts at {moved:#X} in export {export} after the rename"
                        )
                    })?;
                edits.values.push(ValueEdit {
                    offset: moved,
                    expect_name: entry.name.clone(),
                    expect_element: entry.element,
                    expect_kind: kind_of(&entry.value),
                    op: EditOp::Set {
                        text: to.to_string(),
                    },
                });
            }
            _ => {}
        }
    }
    let mut patched = patch_package_with(&renamed_bundle, sidecars, &again, &edits, mappings)?;
    patched.applied.splice(0..0, applied);
    notes.append(&mut patched.notes);
    patched.notes = notes;
    Ok(patched)
}

/// Checks a rename on the package it left: the new name has the uses the old one had, and the old
/// one none but those it was told to leave.
pub fn verify_member_rename(
    before: (&AssetBundle<'_>, &ParsedPackage),
    after: (&AssetBundle<'_>, &ParsedPackage),
    rename: &MemberRename,
    allow_unchecked: bool,
) -> Result<(), String> {
    let plan = plan_member_rename(before.0, before.1, rename, allow_unchecked)?;
    let renamed = MemberTarget {
        name: rename.to.trim().to_string(),
        ..rename.target.clone()
    };
    let now = member_uses(after.0, after.1, &renamed)?;
    let count = |uses: &[MemberUse]| {
        let mut roles: Vec<String> = uses.iter().map(|u| format!("{:?}", u.role)).collect();
        roles.sort();
        roles
    };
    let mut found = now.uses.clone();
    if rename.possible == PossibleUses::Follow {
        found.extend(now.possible.iter().filter(|one| followable(one)).cloned());
    }
    if count(&found) != count(&plan.renamed) {
        return Err(format!(
            "after the rename {} has the uses {:?}, where {} had {:?}",
            renamed.name,
            count(&found),
            rename.target.name,
            count(&plan.renamed)
        ));
    }
    let old = member_uses(after.0, after.1, &rename.target)?;
    if !old.uses.is_empty() {
        return Err(format!(
            "{} is still used after the rename: {}",
            rename.target.name,
            old.uses
                .iter()
                .map(|u| u.label.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    Ok(())
}

/// Take field records out of a class or a function: variables nothing uses, or locals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveRecords {
    /// The class or function export declaring them.
    pub owner: u32,
    pub names: Vec<String>,
}

/// `CPF_Parm`: a function's parameter, which its callers pass and so cannot go.
const CPF_PARM: u64 = 0x80;

/// `CPF_Net`: a replicated variable, which the server numbers among the class's others.
const CPF_NET: u64 = 0x20;

/// Whether `record` is one of an Anim Blueprint class's graph nodes or subsystems: a struct
/// variable of a node or subsystem type, which the class runs from lists of its own by where it
/// sits among its variables, no bytecode naming it.
fn anim_graph_record(
    parsed: &ParsedPackage,
    owner: &ParsedExport,
    record: &crate::ustruct::FieldRecord,
) -> bool {
    if owner.class_name != "AnimBlueprintGeneratedClass" || record.kind != "StructProperty" {
        return false;
    }
    let crate::ustruct::RecordTail::Index(index) = record.tail else {
        return false;
    };
    let path = match index {
        index if index < 0 => parsed
            .imports
            .get((-index - 1) as usize)
            .map(|import| import.path.as_str()),
        index if index > 0 => parsed
            .exports
            .get((index - 1) as usize)
            .map(|export| export.path.as_str()),
        _ => None,
    };
    path.and_then(|path| path.rsplit('.').next())
        .is_some_and(|name| name.starts_with("AnimNode_") || name.starts_with("AnimSubsystem"))
        || record.name.starts_with("AnimGraphNode_")
        || record.name.starts_with("AnimBlueprintExtension_")
}

/// Cuts the records out of their owner's layout and rewrites the count in front of them. A class
/// whose objects store values by position has every object of it in the package lose the slots too,
/// the values in them cut out and the slots after them moved down.
pub(crate) fn patch_record_removal(
    bundle: &AssetBundle<'_>,
    parsed: &ParsedPackage,
    removes: &[RemoveRecords],
) -> Result<PatchedBundle, String> {
    let mut splices = Vec::new();
    let mut applied = Vec::new();
    let mut freed: BTreeMap<usize, BTreeSet<i32>> = BTreeMap::new();
    for remove in removes {
        let owner = parsed
            .exports
            .get(remove.owner as usize)
            .ok_or_else(|| format!("this package has no export {}", remove.owner))?;
        let layout = owner
            .layout
            .as_ref()
            .ok_or_else(|| format!("{} has no field records to remove", owner.path))?;
        let function = owner.signature.is_some();
        let mut gone = 0;
        for name in &remove.names {
            let (record, (start, end)) = layout
                .records
                .iter()
                .find(|(record, _)| record.name.eq_ignore_ascii_case(name))
                .ok_or_else(|| format!("{} declares no field named {name}", owner.path))?;
            if function && record.property_flags & CPF_PARM != 0 {
                return Err(format!(
                    "{name} is a parameter of {}, which its callers pass",
                    owner.path
                ));
            }
            if !function && anim_graph_record(parsed, owner, record) {
                return Err(format!(
                    "{name} is a node of {}'s anim graph, which the anim class runs by where it \
                     sits among its variables",
                    owner.path
                ));
            }
            if !function && record.property_flags & CPF_NET != 0 {
                return Err(format!(
                    "{name} is replicated, and the server numbers {}'s replicated variables with \
                     it among them, so taking it out would leave the two disagreeing",
                    owner.path
                ));
            }
            splices.push(Splice {
                start: *start,
                end: *end,
                bytes: Vec::new(),
            });
            gone += 1;
            applied.push(AppliedEdit {
                name: format!("{}.{}", owner.object_name, record.name),
                offset: *start,
                offset_after: *start,
                element: None,
                elements_after: None,
                before: record.kind.clone(),
                after: "(removed)".into(),
            });
        }
        let count = (layout.records.len() - gone) as i32;
        splices.push(Splice {
            start: layout.child_properties_at,
            end: layout.child_properties_at + 4,
            bytes: count.to_le_bytes().to_vec(),
        });
        if !function && parsed.info.unversioned_properties {
            let (cut, renumbered) =
                crate::class_variable::drop_variable_slots(parsed, bundle, owner, &remove.names)?;
            splices.extend(cut);
            applied.extend(renumbered);
        }
        freed
            .entry(remove.owner as usize)
            .or_default()
            .extend(freed_by(parsed, owner, &remove.names));
    }
    splices.sort_by_key(|splice| splice.start);
    let (exports, preload_dependencies) =
        crate::renumber::drop_runs(&read_header(bundle)?, &freed)?.unzip();
    let rewritten = rewrite(
        bundle,
        &splices,
        HeaderDraft {
            exports,
            preload_dependencies,
            ..Default::default()
        },
    )?;
    check_inline_bulk(&AssetBundle {
        asset: &rewritten.asset,
        exports: &rewritten.exports,
    })?;
    Ok(PatchedBundle {
        asset: rewritten.asset,
        exports: rewritten.exports,
        applied,
        bulk: None,
        optional_bulk: None,
        notes: Vec::new(),
    })
}

/// The objects only the records `names` named in `owner`, whose edges the cook gave the owner for
/// those records' types and which go with them. One still named by anything left in the export, or
/// by its class, parent, template or outer, keeps its edge.
fn freed_by(parsed: &ParsedPackage, owner: &ParsedExport, names: &[String]) -> BTreeSet<i32> {
    let Some(layout) = owner.layout.as_ref() else {
        return BTreeSet::new();
    };
    let mut indices = Vec::new();
    let mut ranges = Vec::new();
    for (record, range) in &layout.records {
        if names
            .iter()
            .any(|name| name.eq_ignore_ascii_case(&record.name))
        {
            crate::field_record::record_indices(record, &mut indices);
            ranges.push(*range);
        }
    }
    let named = still_named(parsed, owner, &ranges);
    let table = [
        owner.class_index,
        owner.super_index,
        owner.template_index,
        owner.outer_index,
    ];
    indices
        .into_iter()
        .filter(|index| !named.contains(index) && !table.contains(index))
        .collect()
}

/// What `owner`'s bytes name outside `ranges`.
fn still_named(
    parsed: &ParsedPackage,
    owner: &ParsedExport,
    ranges: &[(u64, u64)],
) -> BTreeSet<i32> {
    let start = owner.serial_offset.max(0) as u64;
    let end = start + owner.serial_size.max(0) as u64;
    parsed
        .references
        .iter()
        .filter(|reference| start <= reference.at && reference.at < end)
        .filter(|reference| {
            !ranges
                .iter()
                .any(|&(from, to)| from <= reference.at && reference.at < to)
        })
        .map(|reference| reference.index)
        .collect()
}

/// Checks a record removal: the owner declares what it did less the records taken out, it waits for
/// what it did less what only those records named, and every object reads the values it read.
pub(crate) fn verify_record_removal(
    before: &ParsedPackage,
    after: &ParsedPackage,
    removes: &[RemoveRecords],
) -> Result<(), String> {
    if before.exports.len() != after.exports.len() {
        return Err("the export table changed length, which no record removal does".into());
    }
    if let (Some(was), Some(now)) = (&before.dependencies, &after.dependencies) {
        for (at, (old, new)) in was.iter().zip(now).enumerate() {
            let freed: BTreeSet<i32> = removes
                .iter()
                .filter(|remove| remove.owner as usize == at)
                .flat_map(|remove| freed_by(before, &before.exports[at], &remove.names))
                .collect();
            let keep = |run: &[i32]| -> Vec<i32> {
                run.iter()
                    .copied()
                    .filter(|index| !freed.contains(index))
                    .collect()
            };
            let wanted = crate::dependency::Runs {
                serialize_before_serialize: keep(&old.serialize_before_serialize),
                create_before_serialize: keep(&old.create_before_serialize),
                ..old.clone()
            };
            if *new != wanted {
                return Err(format!(
                    "{}'s load order after the removal is {new:?}, not {wanted:?}",
                    after.exports[at].path
                ));
            }
            let named = still_named(after, &after.exports[at], &[]);
            if let Some(lost) = freed.iter().find(|index| named.contains(index)) {
                return Err(format!(
                    "{} still names {lost}, which it no longer waits for",
                    after.exports[at].path
                ));
            }
        }
    }
    for remove in removes {
        let names = |parsed: &ParsedPackage| -> Vec<String> {
            parsed.exports[remove.owner as usize]
                .layout
                .iter()
                .flat_map(|layout| &layout.records)
                .map(|(record, _)| record.name.clone())
                .collect()
        };
        let wanted: Vec<String> = names(before)
            .into_iter()
            .filter(|name| {
                !remove
                    .names
                    .iter()
                    .any(|gone| gone.eq_ignore_ascii_case(name))
            })
            .collect();
        if names(after) != wanted {
            return Err(format!(
                "{} declares {:?} after the removal, not {wanted:?}",
                after.exports[remove.owner as usize].path,
                names(after)
            ));
        }
    }
    // What each object stores; a value it leaves unset is listed from its class's records, which
    // the removal changes, and the value an object of the class held for a variable taken out goes
    // with it.
    let stored = |export: &crate::package::ParsedExport| -> Vec<(String, String)> {
        let gone = |name: &str| {
            removes.iter().any(|remove| {
                crate::class_variable::offset_in(before, export.class_index, remove.owner).is_some()
                    && remove
                        .names
                        .iter()
                        .any(|gone| gone.eq_ignore_ascii_case(name))
            })
        };
        export
            .properties
            .iter()
            .filter(|entry| !matches!(entry.value, crate::value::PropertyValue::Unset { .. }))
            .filter(|entry| !gone(&entry.name))
            .map(|entry| (entry.label(), entry.value.summary()))
            .collect()
    };
    for (was, now) in before.exports.iter().zip(&after.exports) {
        if std::mem::discriminant(&was.status) != std::mem::discriminant(&now.status) {
            return Err(format!(
                "{} reads as {:?} after the removal, where it read as {:?}",
                now.path, now.status, was.status
            ));
        }
        if stored(was) != stored(now) {
            return Err(format!(
                "{} reads other values after the removal than it held",
                now.path
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::package::parse_package;
    use crate::script_fixture::{Built, Code, Function, Import, Package, event_graph};

    fn rename_of(parsed: &ParsedPackage, name: &str, kind: MemberKind, to: &str) -> MemberRename {
        MemberRename {
            target: MemberTarget {
                class: parsed.exports[0].path.clone(),
                function: None,
                name: name.into(),
                kind,
                ancestors: Vec::new(),
            },
            to: to.into(),
            possible: PossibleUses::Refuse,
            declares: true,
        }
    }

    /// Renames, reads the result back and checks it the way a save does.
    fn renamed(built: &Built, rename: &MemberRename, allow_unchecked: bool) -> ParsedPackage {
        let parsed = built.parsed();
        let edits = PackageEdits {
            rename_members: vec![rename.clone()],
            allow_unchecked,
            ..Default::default()
        };
        let patched =
            crate::edit::patch_package(&built.bundle(), &parsed, &edits, None).expect("patch");
        let bundle = AssetBundle {
            asset: &patched.asset,
            exports: &patched.exports,
        };
        let after = parse_package(&bundle, None).expect("reads");
        crate::edit::verify_patch(&parsed, &after, &edits, &patched.applied).expect("verifies");
        verify_member_rename(
            (&built.bundle(), &parsed),
            (&bundle, &after),
            rename,
            allow_unchecked,
        )
        .expect("every use follows");
        after
    }

    fn script_of(parsed: &ParsedPackage, export: usize) -> String {
        crate::kismet::render_script(parsed.exports[export].script.as_ref().expect("script"))
    }

    fn firing() -> Built {
        let mut package = Package::new(
            "BP_Test_C",
            &[
                "Fire",
                "/Script/Engine",
                "KismetSystemLibrary",
                "K2_SetTimer",
            ],
        );
        let engine = package.import(Import {
            class_package: "/Script/CoreUObject",
            class: "Package",
            outer: 0,
            name: "/Script/Engine",
        });
        let library = package.import(Import {
            class_package: "/Script/CoreUObject",
            class: "Class",
            outer: engine,
            name: "KismetSystemLibrary",
        });
        let timer = package.import(Import {
            class_package: "/Script/CoreUObject",
            class: "Function",
            outer: library,
            name: "K2_SetTimer",
        });
        let code = Code::new(&package)
            .op(0x45)
            .name("Fire")
            .op(0x16)
            .op(0x68)
            .int(timer)
            .op(0x17)
            .op(0x1F)
            .ops(b"Fire\0")
            .op(0x16)
            .op(0x21)
            .name("Fire")
            .ops(&[0x04, 0x0B, 0x53])
            .bytes;
        package.functions.push(Function {
            name: "Fire",
            fields: Vec::new(),
            script: code,
            flags: 0,
        });
        package.build()
    }

    /// A function called by name and named to a timer takes the new name in its export, its call
    /// and the timer's string, which grows; a name literal nothing places is refused until told to
    /// leave it, and then left.
    #[test]
    fn a_function_is_renamed_with_its_calls_and_timer() {
        let built = firing();
        let parsed = built.parsed();
        let rename = rename_of(&parsed, "Fire", MemberKind::Function, "Ignite");
        let refused = plan_member_rename(&built.bundle(), &parsed, &rename, false)
            .expect_err("an unplaced literal");
        assert!(refused.contains("--allow-unchecked"), "{refused}");
        let after = renamed(&built, &rename, true);
        assert_eq!(after.exports[1].object_name, "Ignite");
        let text = script_of(&after, 1);
        assert!(text.contains("LocalVirtualFunction Ignite("), "{text}");
        assert!(text.contains("\"Ignite\""), "{text}");
        assert!(
            text.contains("'Fire'"),
            "the unplaced literal is left: {text}"
        );
    }

    /// The event graph takes the new name in its export and the latent action resuming into it;
    /// its stub calls it by object, which is left as it is.
    #[test]
    fn the_event_graph_is_renamed_with_its_latent_resume() {
        let built = event_graph().build();
        let parsed = built.parsed();
        let rename = rename_of(
            &parsed,
            "ExecuteUbergraph_BP_Test",
            MemberKind::Function,
            "ExecuteUbergraph_BP_Renamed",
        );
        let after = renamed(&built, &rename, false);
        assert_eq!(after.exports[1].object_name, "ExecuteUbergraph_BP_Renamed");
        let text = script_of(&after, 1);
        assert!(text.contains("'ExecuteUbergraph_BP_Renamed'"), "{text}");
        assert!(after.exports[1].is_event_graph());
    }

    /// A local takes the new name in its record and every field path naming it; a name already in
    /// use beside it is refused.
    #[test]
    fn a_local_is_renamed_and_a_taken_name_refused() {
        let built = event_graph().build();
        let parsed = built.parsed();
        let local = |to: &str| MemberRename {
            target: MemberTarget {
                function: Some(parsed.exports[1].path.clone()),
                ..rename_of(&parsed, "Count", MemberKind::Local, to).target
            },
            ..rename_of(&parsed, "Count", MemberKind::Local, to)
        };
        let refused =
            plan_member_rename(&built.bundle(), &parsed, &local("Flag"), false).expect_err("taken");
        assert!(refused.contains("already has"), "{refused}");
        let after = renamed(&built, &local("Total"), false);
        let text = script_of(&after, 1);
        assert!(text.contains("LocalVariable(Total)"), "{text}");
        assert!(!text.contains("(Count)"), "{text}");
        let signature = after.exports[1].signature.as_ref().expect("signature");
        assert!(signature.locals.iter().any(|field| field.name == "Total"));
    }

    fn two_functions() -> Built {
        let mut package = Package::new("BP_Test_C", &["Fire", "Spare", "Arg", "Health"]);
        package
            .members
            .push(crate::script_fixture::local("Health", "IntProperty"));
        for name in ["Fire", "Spare"] {
            let code = Code::new(&package).ops(&[0x04, 0x0B, 0x53]).bytes;
            package.functions.push(Function {
                name,
                fields: if name == "Fire" {
                    vec![
                        crate::script_fixture::param("Arg", "IntProperty"),
                        crate::script_fixture::local("Spare", "IntProperty"),
                    ]
                } else {
                    Vec::new()
                },
                script: code,
                flags: 0,
            });
        }
        package.build()
    }

    fn saved(built: &Built, edits: &PackageEdits) -> Result<ParsedPackage, String> {
        let parsed = built.parsed();
        let patched = crate::edit::patch_package(&built.bundle(), &parsed, edits, None)?;
        let after = parse_package(
            &AssetBundle {
                asset: &patched.asset,
                exports: &patched.exports,
            },
            None,
        )?;
        crate::edit::verify_patch(&parsed, &after, edits, &patched.applied)?;
        Ok(after)
    }

    /// Saves `edits` over `asset` and `exports` and reads the result back, checked as a save is.
    fn saved_over(
        (asset, exports): (&[u8], &[u8]),
        edits: &PackageEdits,
    ) -> (Vec<u8>, Vec<u8>, ParsedPackage) {
        let bundle = AssetBundle { asset, exports };
        let parsed = parse_package(&bundle, None).expect("read");
        let patched = crate::edit::patch_package(&bundle, &parsed, edits, None).expect("saved");
        let after = parse_package(
            &AssetBundle {
                asset: &patched.asset,
                exports: &patched.exports,
            },
            None,
        )
        .expect("read back");
        crate::edit::verify_patch(&parsed, &after, edits, &patched.applied).expect("verified");
        (patched.asset, patched.exports, after)
    }

    /// A removed variable takes the edges its types needed with it: the class stops waiting for an
    /// object class only it could hold and for a struct only it held, while a class another variable
    /// still holds keeps its edge.
    #[test]
    fn a_removed_variable_takes_the_edges_only_it_needed() {
        let built = two_functions();
        let variable = |name: &str, ty: &str| crate::class_variable::AddVariable {
            class: None,
            name: name.into(),
            ty: ty.into(),
        };
        let (asset, exports, added) = saved_over(
            (&built.asset, &built.exports),
            &PackageEdits {
                add_variables: vec![
                    variable("Loader", "Object</Game/Mods/X.Loader_C>"),
                    variable("Shape", "Struct</Game/Mods/X.S>"),
                    variable("Other", "Object</Game/Mods/X.Other_C>"),
                    variable("Twin", "Object</Game/Mods/X.Other_C>"),
                ],
                ..Default::default()
            },
        );
        let import = |parsed: &ParsedPackage, path: &str| {
            let at = parsed
                .imports
                .iter()
                .position(|import| import.path == path)
                .unwrap_or_else(|| panic!("no import {path}"));
            -(at as i32) - 1
        };
        let runs = |parsed: &ParsedPackage| parsed.dependencies.as_ref().expect("runs")[0].clone();
        let (loader, shape, other) = (
            import(&added, "/Game/Mods/X.Loader_C"),
            import(&added, "/Game/Mods/X.S"),
            import(&added, "/Game/Mods/X.Other_C"),
        );
        let held = runs(&added);
        assert!(held.create_before_serialize.contains(&loader), "{held:?}");
        assert!(held.create_before_serialize.contains(&other), "{held:?}");
        assert!(held.serialize_before_serialize.contains(&shape), "{held:?}");

        let (_, _, removed) = saved_over(
            (&asset, &exports),
            &PackageEdits {
                remove_records: vec![RemoveRecords {
                    owner: 0,
                    names: vec!["Loader".into(), "Shape".into(), "Twin".into()],
                }],
                ..Default::default()
            },
        );
        let left = runs(&removed);
        let all: Vec<i32> = [
            &left.serialize_before_serialize,
            &left.create_before_serialize,
            &left.serialize_before_create,
            &left.create_before_create,
        ]
        .into_iter()
        .flatten()
        .copied()
        .collect();
        assert!(!all.contains(&loader) && !all.contains(&shape), "{left:?}");
        assert!(left.create_before_serialize.contains(&other), "{left:?}");
        for (at, (was, now)) in added
            .dependencies
            .iter()
            .flatten()
            .zip(removed.dependencies.iter().flatten())
            .enumerate()
            .skip(1)
        {
            assert_eq!(was, now, "export {at} keeps its runs");
        }
    }

    /// A removed function goes out of its class's function list and function map, which keep the
    /// rest renumbered, rather than being left naming nothing.
    #[test]
    fn a_removed_function_leaves_its_classes_lists() {
        let built = two_functions();
        let after = saved(
            &built,
            &PackageEdits {
                remove_exports: vec![2],
                ..Default::default()
            },
        )
        .expect("removes");
        let layout = after.exports[0].layout.as_ref().expect("layout");
        assert_eq!(layout.children, [2]);
        assert_eq!(layout.function_map, [("Fire".to_string(), 2)]);
        let plan = crate::remove::plan_removal(&built.parsed(), &[2]).expect("plan");
        assert!(
            plan.warnings
                .iter()
                .all(|w| !w.contains("pointing at nothing")),
            "{:?}",
            plan.warnings
        );
    }

    /// A local and a class variable go out of their owners' records; a parameter is refused, since
    /// callers pass it.
    #[test]
    fn records_are_taken_out_and_a_parameter_refused() {
        let built = two_functions();
        let local = saved(
            &built,
            &PackageEdits {
                remove_records: vec![RemoveRecords {
                    owner: 1,
                    names: vec!["Spare".into()],
                }],
                ..Default::default()
            },
        )
        .expect("a local goes");
        let signature = local.exports[1].signature.as_ref().expect("signature");
        assert!(signature.locals.is_empty());
        assert_eq!(signature.params.len(), 1);

        let variable = saved(
            &built,
            &PackageEdits {
                remove_records: vec![RemoveRecords {
                    owner: 0,
                    names: vec!["Health".into()],
                }],
                ..Default::default()
            },
        )
        .expect("a variable goes");
        assert!(
            variable.exports[0]
                .layout
                .as_ref()
                .expect("layout")
                .records
                .is_empty()
        );

        let refused = saved(
            &built,
            &PackageEdits {
                remove_records: vec![RemoveRecords {
                    owner: 1,
                    names: vec!["Arg".into()],
                }],
                ..Default::default()
            },
        )
        .expect_err("a parameter");
        assert!(refused.contains("parameter"), "{refused}");
    }

    /// A replicated variable is refused: the server numbers the class's replicated variables, and
    /// would no longer agree with a client that lost one.
    #[test]
    fn a_replicated_variable_is_refused() {
        let mut package = Package::new("BP_Test_C", &["Health"]);
        package.members.push(crate::script_fixture::Field {
            name: "Health",
            kind: "IntProperty",
            flags: CPF_NET,
        });
        let refused = saved(
            &package.build(),
            &PackageEdits {
                remove_records: vec![RemoveRecords {
                    owner: 0,
                    names: vec!["Health".into()],
                }],
                ..Default::default()
            },
        )
        .expect_err("replicated");
        assert!(refused.contains("replicated"), "{refused}");
    }
}
