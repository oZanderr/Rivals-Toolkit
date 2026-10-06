//! Edits that name what they change by the object and the property path leading to it, rather than
//! by a byte offset. Resolved against whatever package the save reads, an edit file holding them
//! still fits after a game patch or an earlier save has moved every offset, and one that has
//! already landed changes nothing.
//!
//! A path names fields by name, `Name[i]` a static array's slot or a container's element, and
//! `Name{key}` a map's pair or a set's element by its key as the dump prints it. Each edit lowers
//! onto the [`ValueEdit`] or [`FieldSet`] that addresses the same value today, so the patcher is
//! unchanged. One that names what another edit in the same save makes or moves waits, and the
//! caller resolves it again once that edit has landed: see `rivals_core::asset_edit`.

use serde::{Deserialize, Serialize};

use crate::edit::{
    EditOp, Expected, FieldSet, PackageEdits, RowOp, ValueEdit, element_key, element_value,
    held_elements, kind_of, reads_back_as, same_elements, value_text,
};
use crate::package::{ParsedExport, ParsedPackage};
use crate::props::TYPE_FIELD;
use crate::value::{PropertyEntry, PropertyValue};

/// One change, addressed by the object it is in and the path to the value inside it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PathEdit {
    /// The object: its path below the package (`Table`, `BP_C:Mesh_GEN_VARIABLE`), its full path,
    /// its bare name when no other object shares it, or its index.
    pub export: String,
    /// A DataTable row the path starts in, matched however it is cased.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row: Option<String>,
    /// The path starts in the default values a Blueprint struct keeps.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub defaults: bool,
    pub path: String,
    #[serde(flatten)]
    pub op: PathOp,
    /// What the value read as when the edit was written. A value that reads otherwise has changed
    /// since, and the edit is refused rather than undoing that, unless the save allows drift.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub was: Option<String>,
}

/// What a path edit does to what its path names. The path names the element an element's edit
/// acts on, so no op carries an index but an insert's position.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PathOp {
    Set {
        text: String,
    },
    Clear,
    Store,
    Unset,
    /// A new element before `index`, or last without one; a set's or a map's under `key`.
    Insert {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
    },
    Remove,
    SetKey {
        text: String,
    },
    Reorder {
        order: Vec<u32>,
    },
    SetRaw {
        hex: String,
    },
}

impl PathOp {
    /// A value edit's op as a path edit makes it, with the element it acts on, which goes on the
    /// path. An insert keeps its position.
    pub fn split(op: EditOp) -> (Option<u32>, PathOp) {
        match op {
            EditOp::Set { text } => (None, PathOp::Set { text }),
            EditOp::Clear => (None, PathOp::Clear),
            EditOp::Store => (None, PathOp::Store),
            EditOp::Unset => (None, PathOp::Unset),
            EditOp::SetElement { index, text } => (Some(index), PathOp::Set { text }),
            EditOp::Insert { index, key } => (
                None,
                PathOp::Insert {
                    index: Some(index),
                    key,
                },
            ),
            EditOp::Remove { index } => (Some(index), PathOp::Remove),
            EditOp::SetKey { index, text } => (Some(index), PathOp::SetKey { text }),
            EditOp::Reorder { order } => (None, PathOp::Reorder { order }),
            EditOp::SetRaw { hex } => (None, PathOp::SetRaw { hex }),
        }
    }
}

/// One step of a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Field(String),
    /// A static array's slot, or a container's element.
    Index(u32),
    /// A map's pair or a set's element, by its key as the dump prints it.
    Key(String),
}

/// Reads a path: fields joined by `.`, each followed by `[i]` or `{key}` where it takes one.
/// Inside braces `\}` and `\\` stand for themselves, and dots are part of the key.
pub fn parse_path(path: &str) -> Result<Vec<Segment>, String> {
    let chars: Vec<char> = path.chars().collect();
    let mut out = Vec::new();
    let mut at = 0;
    let bad = |why: &str| format!("{path} is not a path: {why}");
    while at < chars.len() {
        let start = at;
        while at < chars.len() && !matches!(chars[at], '.' | '[' | '{' | ']' | '}') {
            at += 1;
        }
        let name: String = chars[start..at].iter().collect();
        if name.trim().is_empty() {
            return Err(bad("every step starts with a field's name"));
        }
        out.push(Segment::Field(name.trim().to_string()));
        if at < chars.len() && chars[at] != '.' {
            at = accessor(&chars, at, &mut out).map_err(|why| bad(&why))?;
        }
        if at < chars.len() {
            if chars[at] != '.' {
                return Err(bad(
                    "an element is named once, then a field follows after a dot",
                ));
            }
            at += 1;
            if at == chars.len() {
                return Err(bad("it ends with a dot"));
            }
        }
    }
    if out.is_empty() {
        return Err(bad("it names nothing"));
    }
    Ok(out)
}

/// Reads one `[i]` or `{key}` at `at`, returning where it ends.
fn accessor(chars: &[char], mut at: usize, out: &mut Vec<Segment>) -> Result<usize, String> {
    match chars[at] {
        '[' => {
            let start = at + 1;
            at = start;
            while at < chars.len() && chars[at] != ']' {
                at += 1;
            }
            if at == chars.len() {
                return Err("a [ is never closed".into());
            }
            let digits: String = chars[start..at].iter().collect();
            let index = digits
                .trim()
                .parse::<u32>()
                .map_err(|_| format!("[{digits}] is not an index"))?;
            out.push(Segment::Index(index));
            Ok(at + 1)
        }
        '{' => {
            let mut key = String::new();
            at += 1;
            loop {
                match chars.get(at) {
                    None => return Err("a { is never closed".into()),
                    Some('\\') => {
                        let escaped = chars.get(at + 1).ok_or("it ends inside a key")?;
                        key.push(*escaped);
                        at += 2;
                    }
                    Some('}') => break,
                    Some(c) => {
                        key.push(*c);
                        at += 1;
                    }
                }
            }
            out.push(Segment::Key(key));
            Ok(at + 1)
        }
        other => Err(format!("{other} cannot follow a field's name")),
    }
}

/// Writes segments back as a path [`parse_path`] reads.
pub fn format_path(segments: &[Segment]) -> String {
    let mut out = String::new();
    for segment in segments {
        match segment {
            Segment::Field(name) => {
                if !out.is_empty() {
                    out.push('.');
                }
                out.push_str(name);
            }
            Segment::Index(index) => out.push_str(&format!("[{index}]")),
            Segment::Key(key) => {
                out.push('{');
                for c in key.chars() {
                    if matches!(c, '}' | '\\') {
                        out.push('\\');
                    }
                    out.push(c);
                }
                out.push('}');
            }
        }
    }
    out
}

/// The object a selector names: its path below the package or its full path, then its bare name
/// when it is the only one so called, then its index.
pub fn export_of<'a>(
    parsed: &'a ParsedPackage,
    selector: &str,
) -> Result<&'a ParsedExport, String> {
    let selector = selector.trim();
    if let Some(found) = parsed.exports.iter().find(|export| {
        export.path == selector
            || export
                .path
                .split_once('.')
                .is_some_and(|(_, below)| below == selector)
    }) {
        return Ok(found);
    }
    let named: Vec<&ParsedExport> = parsed
        .exports
        .iter()
        .filter(|export| export.object_name == selector)
        .collect();
    match named.as_slice() {
        [one] => return Ok(one),
        [] => {}
        more => {
            return Err(format!(
                "{} objects are called {selector}; name one by its path: {}",
                more.len(),
                more.iter()
                    .map(|export| below_package(&export.path))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    if let Some(found) = selector
        .parse::<usize>()
        .ok()
        .and_then(|index| parsed.exports.get(index))
    {
        return Ok(found);
    }
    Err(format!("this package holds no object {selector}"))
}

/// An object's path without its package, which is how a path edit names it: the same in a copy of
/// the package saved under another name.
pub fn below_package(path: &str) -> &str {
    path.split_once('.').map_or(path, |(_, below)| below)
}

/// What a path reaches in the package as it reads.
pub(crate) enum Reached<'a> {
    /// A value with a place of its own: a property, a struct's field, a text's part.
    Entry(&'a PropertyEntry),
    /// One element of a container, which edits act on through the container.
    Element {
        container: &'a PropertyEntry,
        index: u32,
    },
    /// A path into a struct that stores nothing yet: what is under it is set through it, which
    /// stores it on the way, as a [`FieldSet`] does.
    Unstored {
        anchor: &'a PropertyEntry,
        rest: Vec<String>,
    },
}

/// Why a path reaches nothing.
pub(crate) enum Unreached {
    /// What it names is not there, which a later stage may make.
    Missing(String),
    /// It cannot name anything in this value.
    Wrong(String),
}

/// Follows `segments` down from `entries`, the top values of an object, a row or a struct's
/// defaults.
pub(crate) fn reach<'a>(
    entries: &'a [PropertyEntry],
    segments: &[Segment],
) -> Result<Reached<'a>, Unreached> {
    let mut list = entries;
    let mut at = 0;
    loop {
        let Some(Segment::Field(name)) = segments.get(at) else {
            return Err(Unreached::Wrong(format!(
                "{} names an element where a field's name goes",
                format_path(&segments[..=at.min(segments.len() - 1)])
            )));
        };
        // `Name[i]` is a static array's slot when the value declares slots, else an element.
        let slot = match segments.get(at + 1) {
            Some(Segment::Index(index)) => list
                .iter()
                .find(|entry| entry.name == *name && entry.element == Some(*index)),
            _ => None,
        };
        let entry = match slot {
            Some(entry) => {
                at += 2;
                entry
            }
            None => {
                let entry = list
                    .iter()
                    .find(|entry| entry.name == *name && entry.element.is_none())
                    .ok_or_else(|| {
                        if list.iter().any(|entry| entry.name == *name) {
                            Unreached::Wrong(format!(
                                "{name} is a static array: name its slot as {name}[0]"
                            ))
                        } else {
                            Unreached::Missing(format!(
                                "{} names nothing",
                                format_path(&segments[..=at])
                            ))
                        }
                    })?;
                at += 1;
                entry
            }
        };
        if at == segments.len() {
            return Ok(Reached::Entry(entry));
        }
        match &entry.value {
            PropertyValue::Unset { .. } | PropertyValue::Default { .. } => {
                let mut rest = Vec::new();
                let mut left = segments[at..].iter().peekable();
                while let Some(segment) = left.next() {
                    match segment {
                        Segment::Field(field) => match left.peek() {
                            Some(Segment::Index(index)) => {
                                rest.push(format!("{field}[{index}]"));
                                left.next();
                            }
                            _ => rest.push(field.clone()),
                        },
                        Segment::Index(index) => rest.push(format!("[{index}]")),
                        Segment::Key(_) => {
                            return Err(Unreached::Missing(format!(
                                "{} stores nothing yet, so it holds no key",
                                entry.label()
                            )));
                        }
                    }
                }
                return Ok(Reached::Unstored {
                    anchor: entry,
                    rest,
                });
            }
            PropertyValue::Struct { fields, .. } => list = fields,
            PropertyValue::Text { parts, .. } => list = parts,
            container @ (PropertyValue::Array { .. }
            | PropertyValue::Set { .. }
            | PropertyValue::Map { .. }) => {
                let index = match &segments[at] {
                    Segment::Index(index) => *index,
                    Segment::Key(key) => keyed(container, key).map_err(|why| match why {
                        None => Unreached::Missing(format!("{} holds no {{{key}}}", entry.label())),
                        Some(why) => Unreached::Wrong(why),
                    })?,
                    Segment::Field(field) => {
                        return Err(Unreached::Wrong(format!(
                            "{} is a container: name one of its elements as [i] or {{key}} before {field}",
                            entry.label()
                        )));
                    }
                };
                let item = element_value(container, index).ok_or_else(|| {
                    Unreached::Missing(format!("{} has no element {index}", entry.label()))
                })?;
                at += 1;
                if at == segments.len() {
                    return Ok(Reached::Element {
                        container: entry,
                        index,
                    });
                }
                list = match item {
                    PropertyValue::Struct { fields, .. } => fields,
                    PropertyValue::Text { parts, .. } => parts,
                    other => {
                        return Err(Unreached::Wrong(format!(
                            "element {index} of {} is a {}, which has no fields",
                            entry.label(),
                            kind_of(other)
                        )));
                    }
                };
            }
            other => {
                return Err(Unreached::Wrong(format!(
                    "{} is a {}, which has no field {}",
                    entry.label(),
                    kind_of(other),
                    format_path(&segments[at..=at])
                )));
            }
        }
    }
}

/// The element of a set, or the pair of a map, whose key reads as `key`. `Err(None)` when none
/// does, and a reason when more than one does.
fn keyed(container: &PropertyValue, key: &str) -> Result<u32, Option<String>> {
    let keys: Vec<&PropertyValue> = match container {
        PropertyValue::Set { items } => items.iter().collect(),
        PropertyValue::Map { entries } => entries.iter().map(|pair| &pair.key).collect(),
        _ => return Err(Some("only a set or a map is looked up by key".into())),
    };
    let found: Vec<u32> = keys
        .iter()
        .enumerate()
        .filter(|(_, held)| key_reads_as(held, key))
        .map(|(at, _)| at as u32)
        .collect();
    match found.as_slice() {
        [one] => Ok(*one),
        [] => Err(None),
        more => Err(Some(format!(
            "{} elements read as {{{key}}}; name the one meant as [i]",
            more.len()
        ))),
    }
}

/// Whether a key reads as `text`: as itself, or for a struct holding one field with text, such as
/// a gameplay tag, as that field.
fn key_reads_as(key: &PropertyValue, text: &str) -> bool {
    if value_text(key).is_some() {
        return reads_back_as(key, text);
    }
    match key {
        PropertyValue::Struct { fields, .. } => {
            let texts: Vec<&PropertyEntry> = fields
                .iter()
                .filter(|field| value_text(&field.value).is_some())
                .collect();
            matches!(texts.as_slice(), [only] if reads_back_as(&only.value, text))
        }
        _ => false,
    }
}

/// The text a key is named by in a path, when it has one: itself, or its one field with text.
pub(crate) fn key_text(key: &PropertyValue) -> Option<String> {
    if let Some(text) = value_text(key) {
        return Some(text);
    }
    match key {
        PropertyValue::Struct { fields, .. } => {
            let texts: Vec<String> = fields
                .iter()
                .filter_map(|field| value_text(&field.value))
                .collect();
            match texts.as_slice() {
                [only] => Some(only.clone()),
                _ => None,
            }
        }
        _ => None,
    }
}

/// What a value reads as, for [`PathEdit::was`]: its text where it has one, else its summary.
pub fn was_of(value: &PropertyValue) -> String {
    value_text(value).unwrap_or_else(|| value.summary())
}

/// What a container holds, as a reorder's [`PathEdit::was`] lists it.
pub fn elements_of(value: &PropertyValue) -> Option<String> {
    held_elements(value)
}

/// Whether a value still reads as `was`.
fn still(value: &PropertyValue, was: &str) -> bool {
    match value_text(value) {
        Some(_) => reads_back_as(value, was),
        None => value.summary() == was,
    }
}

/// Path edits lowered onto the edits the patcher takes, against one reading of the package.
#[derive(Debug, Default)]
pub struct Lowered {
    pub values: Vec<ValueEdit>,
    pub field_sets: Vec<FieldSet>,
    /// Edits naming what an edit here makes or moves, to resolve once it has landed.
    pub waiting: Vec<PathEdit>,
    /// Edits left out because they have landed already.
    pub notes: Vec<String>,
}

/// Where a path edit's path starts: an object's values, one of its rows, or its defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Root {
    export: u32,
    /// Lowercased, as rows are matched.
    row: Option<String>,
    defaults: bool,
}

/// Resolves every path edit in `changes` against `parsed`, the package as the save reads it now.
/// Each lowers onto the value edit or field set addressing its value today, is left out with a
/// note when it has landed already, or waits on an edit here that makes, moves or replaces what it
/// names, to be resolved again once that has landed.
pub fn lower_paths(parsed: &ParsedPackage, changes: &PackageEdits) -> Result<Lowered, String> {
    let mut edits: Vec<(&PathEdit, Root, Vec<Segment>)> = Vec::new();
    for edit in &changes.paths {
        if edit.row.is_some() && edit.defaults {
            return Err(format!(
                "{}: an edit starts in a row or in a struct's defaults, not both",
                describe(edit)
            ));
        }
        let segments = parse_path(&edit.path)?;
        let export = export_of(parsed, &edit.export)?;
        let root = Root {
            export: export.index,
            row: edit.row.as_deref().map(str::to_lowercase),
            defaults: edit.defaults,
        };
        edits.push((edit, root, segments));
    }
    // What is under a value another edit changes means something only once that has landed, so
    // an edit is placed only after every edit on a value above it.
    edits.sort_by_key(|(_, _, segments)| segments.len());

    let mut out = Lowered::default();
    let mut drift = Vec::new();
    // Values this stage changes the shape of, or leaves to a later one: nothing under them is
    // placed yet.
    let mut changing: Vec<(Root, Vec<Segment>)> = Vec::new();
    // The edit each field set came from, which waits instead when its struct is stored here.
    let mut sets: Vec<(FieldSet, &PathEdit)> = Vec::new();
    for (edit, root, segments) in &edits {
        let under_change = changing.iter().any(|(held, above)| {
            held == root && above.len() < segments.len() && segments.starts_with(above)
        });
        if under_change {
            out.waiting.push((*edit).clone());
            changing.push((root.clone(), segments.clone()));
            continue;
        }
        let export = &parsed.exports[root.export as usize];
        let entries: &[PropertyEntry] = match (&edit.row, root.defaults) {
            (Some(row), _) => {
                let found = export.data_table.as_ref().and_then(|table| {
                    table
                        .rows
                        .iter()
                        .find(|held| held.name.eq_ignore_ascii_case(row))
                });
                match found {
                    Some(found) => &found.fields,
                    None if row_made_here(changes, root.export, row) => {
                        out.waiting.push((*edit).clone());
                        changing.push((root.clone(), segments.clone()));
                        continue;
                    }
                    None => {
                        return Err(format!(
                            "{} holds no row {row}",
                            below_package(&export.path)
                        ));
                    }
                }
            }
            (None, true) => &export.defaults,
            (None, false) => &export.properties,
        };
        let label = describe(edit);
        let reached = match reach(entries, segments) {
            Ok(reached) => reached,
            // A remove whose element is gone has landed already.
            Err(Unreached::Missing(_)) if edit.op == PathOp::Remove => {
                out.notes.push(format!("{label}: already gone"));
                continue;
            }
            // So has a new key for an element named by its old one, once the container holds it.
            Err(Unreached::Missing(_)) if rekeyed(entries, segments, &edit.op) => {
                out.notes.push(format!("{label}: already keyed so"));
                continue;
            }
            Err(Unreached::Missing(why) | Unreached::Wrong(why)) => {
                return Err(format!("{label}: {why}"));
            }
        };
        let mut lowering = lower_one(edit, &reached, &label, true)?;
        if let Lowering::Drift(why) = lowering {
            if !changes.allow_drift {
                drift.push(format!("{label} {why}"));
                continue;
            }
            lowering = lower_one(edit, &reached, &label, false)?;
        }
        match lowering {
            Lowering::Value(value) => {
                if reshapes(&edit.op, segments) {
                    changing.push((root.clone(), reshaped(segments).to_vec()));
                }
                if !out.values.contains(&value) {
                    out.values.push(value);
                }
            }
            Lowering::Field(set) => sets.push((set, edit)),
            Lowering::Store(store) => {
                if !out.values.contains(&store) {
                    out.values.push(store);
                }
                out.waiting.push((*edit).clone());
                changing.push((root.clone(), segments.clone()));
            }
            Lowering::Done(note) => out.notes.push(format!("{label}: {note}")),
            Lowering::Drift(_) => unreachable!("drift is checked only once"),
        }
    }
    if !drift.is_empty() {
        return Err(format!(
            "{}: {}. Re-read the asset and make the edits again, or apply them anyway",
            crate::edit::DRIFT,
            drift.join("; ")
        ));
    }
    // A field set walks through the struct it names and stores it on the way; one this stage
    // stores already is set once it has been.
    for (set, edit) in sets {
        let stored_here = out.values.iter().any(|value| {
            value.op == EditOp::Store
                && value.offset == set.offset
                && value.expect_name == set.expect_name
                && value.expect_element == set.expect_element
        });
        if stored_here {
            out.waiting.push(edit.clone());
        } else if !out.field_sets.contains(&set) {
            out.field_sets.push(set);
        }
    }
    refuse_conflicts(&out.values)?;
    Ok(out)
}

/// Whether an element named by a key it no longer holds is in its container under the key this edit
/// gives it: a map's pair given a new key, or a set's element a new value.
fn rekeyed(entries: &[PropertyEntry], segments: &[Segment], op: &PathOp) -> bool {
    let (PathOp::SetKey { text } | PathOp::Set { text }) = op else {
        return false;
    };
    let Some((Segment::Key(_), parent)) = segments.split_last() else {
        return false;
    };
    let Ok(Reached::Entry(container)) = reach(entries, parent) else {
        return false;
    };
    match (&container.value, op) {
        (PropertyValue::Map { .. }, PathOp::SetKey { .. })
        | (PropertyValue::Set { .. }, PathOp::Set { .. }) => keyed(&container.value, text).is_ok(),
        _ => false,
    }
}

/// Whether an edit changes the shape of what it names, so that a path under it reads otherwise
/// once it has landed: anything but a value set, and a set of a struct's type.
fn reshapes(op: &PathOp, segments: &[Segment]) -> bool {
    !matches!(op, PathOp::Set { .. })
        || matches!(segments.last(), Some(Segment::Field(name)) if name == TYPE_FIELD)
}

/// What an edit reshapes: the struct holding a type it sets, or else what it names.
fn reshaped(segments: &[Segment]) -> &[Segment] {
    match segments.split_last() {
        Some((Segment::Field(name), parent)) if name == TYPE_FIELD => parent,
        _ => segments,
    }
}

/// An edit as a message names it: its object, row and path.
fn describe(edit: &PathEdit) -> String {
    match &edit.row {
        Some(row) => format!("{} row {row}: {}", edit.export, edit.path),
        None if edit.defaults => format!("{} defaults: {}", edit.export, edit.path),
        None => format!("{}: {}", edit.export, edit.path),
    }
}

/// Whether a row edit in this save makes a row called `name` in export `export`.
fn row_made_here(changes: &PackageEdits, export: u32, name: &str) -> bool {
    changes.rows.iter().any(|edit| {
        edit.export == export
            && match &edit.op {
                RowOp::Add { name: made, .. } | RowOp::Duplicate { name: made, .. } => {
                    made.eq_ignore_ascii_case(name)
                }
                RowOp::Rename { to, .. } => to.eq_ignore_ascii_case(name),
                _ => false,
            }
    })
}

enum Lowering {
    Value(ValueEdit),
    Field(FieldSet),
    /// The struct on the way stores nothing yet; this stores it, and the edit waits.
    Store(ValueEdit),
    /// Landed already, with what to say about it.
    Done(String),
    /// What the value reads as now, against what the edit was written against.
    Drift(String),
}

/// The value edit addressing `entry` itself.
fn at_entry(entry: &PropertyEntry, op: EditOp, label: &str) -> Result<ValueEdit, String> {
    let (start, _) = entry
        .span
        .ok_or_else(|| format!("{label}: {} has no recorded position", entry.label()))?;
    Ok(ValueEdit {
        offset: start,
        expect_name: entry.name.clone(),
        expect_element: entry.element,
        expect_kind: kind_of(&entry.value),
        op,
    })
}

/// One edit lowered onto what it reached. `check` holds it to [`PathEdit::was`].
fn lower_one(
    edit: &PathEdit,
    reached: &Reached<'_>,
    label: &str,
    check: bool,
) -> Result<Lowering, String> {
    let was = edit.was.as_deref().filter(|_| check);
    match reached {
        Reached::Unstored { anchor, rest } => match &edit.op {
            PathOp::Set { text } => {
                let (start, _) = anchor.span.ok_or_else(|| {
                    format!("{label}: {} has no recorded position", anchor.label())
                })?;
                Ok(Lowering::Field(FieldSet {
                    offset: start,
                    expect_name: anchor.name.clone(),
                    expect_element: anchor.element,
                    path: rest.clone(),
                    text: text.clone(),
                }))
            }
            _ => Ok(Lowering::Store(at_entry(anchor, EditOp::Store, label)?)),
        },
        Reached::Entry(entry) => {
            let value = &entry.value;
            if let Some(done) = landed_on_entry(value, &edit.op, edit.was.as_deref()) {
                return Ok(Lowering::Done(done));
            }
            // A value stored as a struct that reads as a list, such as a tag container, has no
            // list to add to until it is stored.
            if matches!(edit.op, PathOp::Insert { .. }) && listed_once_stored(value) {
                return Ok(Lowering::Store(at_entry(entry, EditOp::Store, label)?));
            }
            let whole = matches!(edit.op, PathOp::Reorder { .. } | PathOp::Insert { .. })
                && held_elements(value).is_some();
            if let Some(was) = was {
                // A container's reorder or insert is held to every element it held.
                let still_was = if whole {
                    same_elements(value, was)
                } else {
                    still(value, was)
                };
                if !still_was {
                    let now = held_elements(value)
                        .filter(|_| whole)
                        .unwrap_or_else(|| was_of(value));
                    return Ok(Lowering::Drift(format!("was {was}, and is now {now}")));
                }
            }
            let op = match &edit.op {
                PathOp::Set { text } => EditOp::Set { text: text.clone() },
                PathOp::Clear => EditOp::Clear,
                PathOp::Store => EditOp::Store,
                PathOp::Unset => EditOp::Unset,
                PathOp::Insert { index, key } => EditOp::Insert {
                    index: index.unwrap_or_else(|| element_count(value)),
                    key: key.clone(),
                },
                PathOp::Reorder { order } => EditOp::Reorder {
                    order: order.clone(),
                },
                PathOp::SetRaw { hex } => EditOp::SetRaw { hex: hex.clone() },
                PathOp::Remove | PathOp::SetKey { .. } => {
                    return Err(format!(
                        "{label}: names a value, and this edit acts on a container's element: name it as [i] or {{key}}"
                    ));
                }
            };
            Ok(Lowering::Value(at_entry(entry, op, label)?))
        }
        Reached::Element { container, index } => {
            let index = *index;
            let item = element_value(&container.value, index);
            let key = element_key(&container.value, index);
            let done = match (&edit.op, item, key) {
                (PathOp::Set { text }, Some(item), _) if reads_back_as(item, text) => {
                    Some(format!("already reads as {text}"))
                }
                (PathOp::SetKey { text }, _, Some(key)) if reads_back_as(key, text) => {
                    Some(format!("already keyed {text}"))
                }
                _ => None,
            };
            if let Some(done) = done {
                return Ok(Lowering::Done(done));
            }
            if let Some(was) = was {
                let now = match &edit.op {
                    PathOp::SetKey { .. } => key,
                    _ => item,
                };
                if !now.is_some_and(|now| still(now, was)) {
                    return Ok(Lowering::Drift(format!(
                        "was {was}, and is now {}",
                        now.map_or_else(|| "missing".to_string(), was_of)
                    )));
                }
            }
            let op = match &edit.op {
                PathOp::Set { text } => EditOp::SetElement {
                    index,
                    text: text.clone(),
                },
                PathOp::Remove => EditOp::Remove { index },
                PathOp::SetKey { text } => EditOp::SetKey {
                    index,
                    text: text.clone(),
                },
                _ => {
                    return Err(format!(
                        "{label}: names one element, and this edit acts on a whole value: drop the [i] or {{key}} to name the container"
                    ));
                }
            };
            Ok(Lowering::Value(at_entry(container, op, label)?))
        }
    }
}

/// Whether a value nothing stores yet is declared as something other than a container, so that
/// only its stored form reads as one.
fn listed_once_stored(value: &PropertyValue) -> bool {
    let declared = match value {
        PropertyValue::Unset { declared, .. } => *declared,
        PropertyValue::Default {
            declared: Some(declared),
            ..
        } => declared,
        _ => return false,
    };
    !matches!(declared, "Array" | "Set" | "Map") && !declared.starts_with("Multicast")
}

/// Why an edit on a value itself has landed already, when it has. A reorder has when the elements
/// it was written against read in its order now.
fn landed_on_entry(value: &PropertyValue, op: &PathOp, was: Option<&str>) -> Option<String> {
    let unstored = matches!(value, PropertyValue::Unset { .. });
    let zero = matches!(value, PropertyValue::Default { .. });
    match op {
        PathOp::Set { text } if value_text(value).is_some() && reads_back_as(value, text) => {
            Some(format!("already reads as {text}"))
        }
        PathOp::Clear if zero => Some("already zero".into()),
        PathOp::Unset if unstored => Some("already inherits".into()),
        PathOp::Store if !unstored && !zero => Some("already stored".into()),
        PathOp::Insert { key: Some(key), .. }
            if matches!(value, PropertyValue::Set { .. } | PropertyValue::Map { .. })
                && keyed(value, key).is_ok() =>
        {
            Some(format!("already holds {{{key}}}"))
        }
        PathOp::Reorder { order } => {
            let was: Vec<serde_json::Value> = serde_json::from_str(was?).ok()?;
            let moved: Option<Vec<&serde_json::Value>> =
                order.iter().map(|from| was.get(*from as usize)).collect();
            let moved = serde_json::to_string(&moved?).ok()?;
            let identity = order
                .iter()
                .enumerate()
                .all(|(at, from)| at as u32 == *from);
            (!identity && same_elements(value, &moved)).then(|| "already in that order".into())
        }
        _ => None,
    }
}

/// How many elements a container holds, where an insert with no position goes.
fn element_count(value: &PropertyValue) -> u32 {
    match value {
        PropertyValue::Array { items } | PropertyValue::Set { items } => items.len() as u32,
        PropertyValue::Map { entries } => entries.len() as u32,
        _ => 0,
    }
}

/// Two path edits that land on one value differently are refused, rather than left to fail as
/// overlapping splices. Several inserts into one container are fine, as each names its place.
fn refuse_conflicts(values: &[ValueEdit]) -> Result<(), String> {
    for (at, edit) in values.iter().enumerate() {
        let clash = values[at + 1..].iter().any(|other| {
            other.offset == edit.offset
                && other.expect_name == edit.expect_name
                && other.expect_element == edit.expect_element
                && Expected::value_key(other) == Expected::value_key(edit)
                && !matches!(
                    (&edit.op, &other.op),
                    (EditOp::Insert { .. }, EditOp::Insert { .. })
                )
        });
        if clash {
            return Err(format!(
                "two edits change {} at {:#X} differently",
                edit.expect_name, edit.offset
            ));
        }
    }
    Ok(())
}

/// Where the value an offset edit addresses sits, as a path edit names it: the object, the row or
/// the defaults it is in, and the path down to it. `None` for a value inside a map's key, which no
/// path reaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    pub export: u32,
    pub row: Option<String>,
    pub defaults: bool,
    pub segments: Vec<Segment>,
}

/// The place of the value at `offset` called `name`, the way [`crate::entry_named_at`] finds it.
pub fn place_of(
    parsed: &ParsedPackage,
    offset: u64,
    name: &str,
    element: Option<u32>,
) -> Option<Place> {
    for export in &parsed.exports {
        let tops: [(&[PropertyEntry], Option<String>, bool); 2] = [
            (&export.properties, None, false),
            (&export.defaults, None, true),
        ];
        for (entries, row, defaults) in tops {
            if let Some(segments) = path_in(entries, offset, name, element) {
                return Some(Place {
                    export: export.index,
                    row,
                    defaults,
                    segments,
                });
            }
        }
        if let Some(table) = &export.data_table {
            for row in &table.rows {
                if let Some(segments) = path_in(&row.fields, offset, name, element) {
                    return Some(Place {
                        export: export.index,
                        row: Some(row.name.clone()),
                        defaults: false,
                        segments,
                    });
                }
            }
        }
    }
    None
}

fn path_in(
    entries: &[PropertyEntry],
    offset: u64,
    name: &str,
    element: Option<u32>,
) -> Option<Vec<Segment>> {
    for entry in entries {
        let mut here = vec![Segment::Field(entry.name.clone())];
        if let Some(slot) = entry.element {
            here.push(Segment::Index(slot));
        }
        if entry.name == name
            && entry.element == element
            && entry.span.is_some_and(|(start, _)| start == offset)
        {
            return Some(here);
        }
        if let Some(mut below) = path_in_value(&entry.value, offset, name, element) {
            here.append(&mut below);
            return Some(here);
        }
    }
    None
}

fn path_in_value(
    value: &PropertyValue,
    offset: u64,
    name: &str,
    element: Option<u32>,
) -> Option<Vec<Segment>> {
    match value {
        PropertyValue::Struct { fields, .. } => path_in(fields, offset, name, element),
        PropertyValue::Text { parts, .. } => path_in(parts, offset, name, element),
        PropertyValue::Array { items } | PropertyValue::Set { items } => {
            items.iter().enumerate().find_map(|(at, item)| {
                let mut below = path_in_value(item, offset, name, element)?;
                below.insert(0, element_segment(value, at as u32));
                Some(below)
            })
        }
        PropertyValue::Map { entries } => entries.iter().enumerate().find_map(|(at, pair)| {
            let mut below = path_in_value(&pair.value, offset, name, element)?;
            below.insert(0, element_segment(value, at as u32));
            Some(below)
        }),
        _ => None,
    }
}

/// How a path names element `index` of a container: by its key where that is unique text, which
/// survives a reorder and an insert before it, else by position.
pub fn element_segment(container: &PropertyValue, index: u32) -> Segment {
    let key = match container {
        PropertyValue::Set { items } => items.get(index as usize),
        PropertyValue::Map { entries } => entries.get(index as usize).map(|pair| &pair.key),
        _ => None,
    };
    match key.and_then(key_text) {
        Some(text) if keyed(container, &text) == Ok(index) => Segment::Key(text),
        _ => Segment::Index(index),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    use crate::edit::{patch_package, verify_patch};
    use crate::package::{AssetBundle, parse_package};
    use crate::tagged_fixture::tagged_package;

    fn parse(asset: &[u8], exports: &[u8]) -> ParsedPackage {
        parse_package(&AssetBundle { asset, exports }, None).expect("parses")
    }

    fn fixture() -> (ParsedPackage, Vec<u8>, Vec<u8>) {
        let (asset, exports) = tagged_package();
        (parse(&asset, &exports), asset, exports)
    }

    fn at(path: &str, op: PathOp) -> PathEdit {
        PathEdit {
            export: "TestObject".into(),
            row: None,
            defaults: false,
            path: path.into(),
            op,
            was: None,
        }
    }

    fn set(path: &str, text: &str) -> PathEdit {
        at(path, PathOp::Set { text: text.into() })
    }

    fn lower(parsed: &ParsedPackage, paths: Vec<PathEdit>) -> Result<Lowered, String> {
        lower_paths(
            parsed,
            &PackageEdits {
                paths,
                ..Default::default()
            },
        )
    }

    /// Lowers `paths`, patches, and reads back, a stage at a time until none waits.
    fn apply(
        before: &ParsedPackage,
        asset: &[u8],
        exports: &[u8],
        paths: Vec<PathEdit>,
    ) -> (ParsedPackage, Vec<u8>, Vec<u8>) {
        let lowered = lower(before, paths).expect("lowers");
        assert!(lowered.field_sets.is_empty());
        let changes = PackageEdits {
            values: lowered.values,
            ..Default::default()
        };
        let patched =
            patch_package(&AssetBundle { asset, exports }, before, &changes, None).expect("patch");
        let after = parse(&patched.asset, &patched.exports);
        verify_patch(before, &after, &changes, &patched.applied).expect("verifies");
        if lowered.waiting.is_empty() {
            return (after, patched.asset, patched.exports);
        }
        apply(&after, &patched.asset, &patched.exports, lowered.waiting)
    }

    fn top<'a>(parsed: &'a ParsedPackage, name: &str) -> &'a PropertyValue {
        &parsed.exports[0]
            .properties
            .iter()
            .find(|entry| entry.name == name)
            .unwrap_or_else(|| panic!("no {name}"))
            .value
    }

    #[test]
    fn an_object_is_named_by_its_path_its_name_or_its_index() {
        let (parsed, _, _) = fixture();
        let full = parsed.exports[0].path.clone();
        for selector in [full.as_str(), below_package(&full), "TestObject", "0"] {
            assert_eq!(export_of(&parsed, selector).expect(selector).index, 0);
        }
        assert!(export_of(&parsed, "Nothing").is_err());
        // Subobjects of two objects can share a name, and are then named by path.
        let mut twice = parsed.clone();
        for (index, outer) in ["A", "B"].into_iter().enumerate() {
            let mut sub = parsed.exports[0].clone();
            sub.index = index as u32 + 1;
            sub.object_name = "Mesh".into();
            sub.path = format!("/Game/TestPackage.{outer}:Mesh");
            twice.exports.push(sub);
        }
        let refused = export_of(&twice, "Mesh").expect_err("ambiguous");
        assert!(refused.contains("2 objects"), "{refused}");
        assert!(refused.contains("B:Mesh"), "{refused}");
        assert_eq!(export_of(&twice, "B:Mesh").expect("by path").index, 2);
    }

    /// Every value and element is placed, and reached again by the path written for it.
    #[test]
    fn every_value_is_reached_by_the_path_written_for_it() {
        let (parsed, _, _) = fixture();
        fn walk(parsed: &ParsedPackage, entries: &[PropertyEntry], seen: &mut usize) {
            for entry in entries {
                let (start, _) = entry.span.expect("span");
                let place = place_of(parsed, start, &entry.name, entry.element).expect("placed");
                let found = reach(&parsed.exports[0].properties, &place.segments);
                assert!(
                    matches!(found, Ok(Reached::Entry(held)) if std::ptr::eq(held, entry)),
                    "{}",
                    format_path(&place.segments)
                );
                *seen += 1;
                let count = element_count(&entry.value);
                for index in 0..count {
                    let mut segments = place.segments.clone();
                    segments.push(element_segment(&entry.value, index));
                    let found = reach(&parsed.exports[0].properties, &segments);
                    assert!(
                        matches!(found, Ok(Reached::Element { container, index: at })
                            if std::ptr::eq(container, entry) && at == index),
                        "{}",
                        format_path(&segments)
                    );
                    *seen += 1;
                }
                match &entry.value {
                    PropertyValue::Struct { fields, .. } => walk(parsed, fields, seen),
                    PropertyValue::Array { items } => {
                        for item in items {
                            if let PropertyValue::Struct { fields, .. } = item {
                                walk(parsed, fields, seen);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut seen = 0;
        walk(&parsed, &parsed.exports[0].properties, &mut seen);
        assert!(seen >= 30, "{seen}");
        // A set's element and a map's pair are named by key.
        assert_eq!(
            element_segment(top(&parsed, "Names"), 1),
            Segment::Key("Label".into())
        );
        assert_eq!(
            element_segment(top(&parsed, "Scores"), 0),
            Segment::Key("Foo".into())
        );
        assert_eq!(
            element_segment(top(&parsed, "Values"), 1),
            Segment::Index(1)
        );
    }

    #[test]
    fn path_edits_land_where_their_paths_name() {
        let (before, asset, exports) = fixture();
        let (after, _, _) = apply(
            &before,
            &asset,
            &exports,
            vec![
                set("Pos.X", "3"),
                set("Points[0].X", "8"),
                set("Words[1]", "ccc"),
                set("Scores{Foo}", "9"),
                at("Names{Label}", PathOp::Remove),
                at(
                    "Values",
                    PathOp::Insert {
                        index: None,
                        key: None,
                    },
                ),
            ],
        );
        let field = |value: &PropertyValue| match value {
            PropertyValue::Struct { fields, .. } => fields[0].value.summary(),
            other => panic!("{other:?}"),
        };
        assert_eq!(field(top(&after, "Pos")), "3");
        let PropertyValue::Array { items } = top(&after, "Points") else {
            panic!()
        };
        assert_eq!(field(&items[0]), "8");
        let PropertyValue::Array { items } = top(&after, "Words") else {
            panic!()
        };
        assert_eq!(items[1].summary(), "ccc");
        let PropertyValue::Map { entries } = top(&after, "Scores") else {
            panic!()
        };
        assert_eq!(entries[0].value.summary(), "9");
        let PropertyValue::Set { items } = top(&after, "Names") else {
            panic!()
        };
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].summary(), "Foo");
        let PropertyValue::Array { items } = top(&after, "Values") else {
            panic!()
        };
        assert_eq!(items.len(), 3, "an insert with no position goes last");

        // A map's pair given a new key by its old one; a second pass finds it keyed so.
        let rekey = || {
            at(
                "Scores{Foo}",
                PathOp::SetKey {
                    text: "Label".into(),
                },
            )
        };
        let (keyed, _, _) = apply(&before, &asset, &exports, vec![rekey()]);
        let PropertyValue::Map { entries } = top(&keyed, "Scores") else {
            panic!()
        };
        assert_eq!(entries[0].key.summary(), "Label");
        let again = lower(&keyed, vec![rekey()]).expect("lowers");
        assert!(again.values.is_empty(), "{:?}", again.values);
        assert_eq!(again.notes.len(), 1);
    }

    #[test]
    fn an_edit_under_one_that_reshapes_its_value_waits_for_it() {
        let (parsed, _, _) = fixture();
        let insert = at(
            "Values",
            PathOp::Insert {
                index: None,
                key: None,
            },
        );
        let lowered = lower(&parsed, vec![set("Values[2]", "4"), insert.clone()]).expect("lowers");
        assert_eq!(lowered.values.len(), 1);
        assert_eq!(lowered.waiting, [set("Values[2]", "4")]);

        let reorder = at("Values", PathOp::Reorder { order: vec![1, 0] });
        let lowered = lower(&parsed, vec![reorder, set("Values[0]", "4")]).expect("lowers");
        assert_eq!(lowered.waiting, [set("Values[0]", "4")]);

        // A sibling element is not under the remove.
        let lowered = lower(
            &parsed,
            vec![at("Values[1]", PathOp::Remove), set("Values[0]", "4")],
        )
        .expect("lowers");
        assert_eq!(lowered.values.len(), 2);
        assert!(lowered.waiting.is_empty());

        // One under an edit that has landed already is placed at once.
        let held = at(
            "Scores",
            PathOp::Insert {
                index: None,
                key: Some("Foo".into()),
            },
        );
        let lowered = lower(&parsed, vec![held, set("Scores{Foo}", "6")]).expect("lowers");
        assert_eq!(lowered.values.len(), 1);
        assert!(lowered.waiting.is_empty());
        assert_eq!(lowered.notes.len(), 1);
    }

    #[test]
    fn an_edit_is_held_to_what_it_was_written_against() {
        let (parsed, _, _) = fixture();
        let was = |text: &str| PathEdit {
            was: Some(text.into()),
            ..set("Damage", "250")
        };
        assert_eq!(
            lower(&parsed, vec![was("99")])
                .expect("lowers")
                .values
                .len(),
            1
        );
        let refused = lower(&parsed, vec![was("98")]).expect_err("drift");
        assert!(refused.starts_with(DRIFT_START), "{refused}");
        assert!(refused.contains("was 98, and is now 99"), "{refused}");
        let anyway = lower_paths(
            &parsed,
            &PackageEdits {
                paths: vec![was("98")],
                allow_drift: true,
                ..Default::default()
            },
        )
        .expect("allowed");
        assert_eq!(anyway.values.len(), 1);

        // An element is held to what it held, and a reorder to every element.
        let element = PathEdit {
            was: Some("1".into()),
            ..set("Values[0]", "4")
        };
        assert!(lower(&parsed, vec![element.clone()]).is_ok());
        let moved = PathEdit {
            was: Some("7".into()),
            ..element
        };
        assert!(lower(&parsed, vec![moved]).is_err());
        let reorder = PathEdit {
            was: elements_of(top(&parsed, "Values")),
            ..at("Values", PathOp::Reorder { order: vec![1, 0] })
        };
        assert_eq!(
            lower(&parsed, vec![reorder.clone()])
                .expect("lowers")
                .values
                .len(),
            1
        );
        let insert = PathEdit {
            was: Some(r#"[["1"]]"#.into()),
            ..at(
                "Values",
                PathOp::Insert {
                    index: None,
                    key: None,
                },
            )
        };
        assert!(
            lower(&parsed, vec![insert]).is_err(),
            "it held two elements"
        );
    }

    #[test]
    fn an_edit_that_has_landed_already_changes_nothing() {
        let (before, asset, exports) = fixture();
        let reorder = PathEdit {
            was: elements_of(top(&before, "Values")),
            ..at("Values", PathOp::Reorder { order: vec![1, 0] })
        };
        let edits = vec![
            set("Damage", "250"),
            set("Scores{Foo}", "9"),
            at("Names{Label}", PathOp::Remove),
            at(
                "Names",
                PathOp::Insert {
                    index: None,
                    key: Some("Tag".into()),
                },
            ),
            reorder,
        ];
        let (after, _, _) = apply(&before, &asset, &exports, edits.clone());
        let again = lower(&after, edits).expect("lowers");
        assert!(again.values.is_empty(), "{:?}", again.values);
        assert!(again.waiting.is_empty());
        assert_eq!(again.notes.len(), 5, "{:?}", again.notes);
        assert!(lower(&after, vec![at("Clear", PathOp::Clear)]).is_err());
    }

    #[test]
    fn a_path_that_names_nothing_is_refused_by_what_it_names() {
        let (parsed, _, _) = fixture();
        let refused = lower(&parsed, vec![set("Pos.Y", "1")]).expect_err("refused");
        assert!(refused.contains("Pos.Y names nothing"), "{refused}");
        let refused = lower(&parsed, vec![set("Values.X", "1")]).expect_err("refused");
        assert!(refused.contains("is a container"), "{refused}");
        let refused = lower(&parsed, vec![set("Scores{Nope}", "1")]).expect_err("refused");
        assert!(refused.contains("holds no {Nope}"), "{refused}");
        let refused = lower(&parsed, vec![at("Damage", PathOp::Remove)]).expect_err("refused");
        assert!(refused.contains("name it as [i]"), "{refused}");
        let refused =
            lower(&parsed, vec![set("Damage", "1"), set("Damage", "2")]).expect_err("refused");
        assert!(refused.contains("differently"), "{refused}");
        let same = lower(&parsed, vec![set("Damage", "1"), set("Damage", "1")]).expect("one");
        assert_eq!(same.values.len(), 1);
    }

    const DRIFT_START: &str = crate::edit::DRIFT;

    #[test]
    fn a_path_reads_fields_slots_elements_and_keys() {
        assert_eq!(
            parse_path("Foo.Bar[2].Baz").unwrap(),
            vec![
                Segment::Field("Foo".into()),
                Segment::Field("Bar".into()),
                Segment::Index(2),
                Segment::Field("Baz".into()),
            ]
        );
        assert_eq!(
            parse_path("Options{Hero.Ability.X}.Text").unwrap(),
            vec![
                Segment::Field("Options".into()),
                Segment::Key("Hero.Ability.X".into()),
                Segment::Field("Text".into()),
            ]
        );
        assert_eq!(
            parse_path("Icon is Visible").unwrap(),
            vec![Segment::Field("Icon is Visible".into())]
        );
        let odd = parse_path(r"Map{a\}b\\c}").unwrap();
        assert_eq!(odd[1], Segment::Key(r"a}b\c".into()));
        for bad in [
            "",
            "Foo.",
            ".Foo",
            "Foo[x]",
            "Foo[1",
            "Foo{a",
            "Foo[1]Bar",
            "[1]",
        ] {
            assert!(parse_path(bad).is_err(), "{bad}");
        }
        for path in ["A.B[3].C", r"Map{a\}b\\c}.X", "A{k}"] {
            assert_eq!(format_path(&parse_path(path).unwrap()), path);
        }
    }

    #[test]
    fn a_value_edit_s_element_moves_onto_the_path() {
        assert_eq!(
            PathOp::split(EditOp::SetElement {
                index: 3,
                text: "x".into()
            }),
            (Some(3), PathOp::Set { text: "x".into() })
        );
        assert_eq!(
            PathOp::split(EditOp::Insert {
                index: 1,
                key: None
            }),
            (
                None,
                PathOp::Insert {
                    index: Some(1),
                    key: None
                }
            )
        );
        assert_eq!(
            PathOp::split(EditOp::Remove { index: 0 }),
            (Some(0), PathOp::Remove)
        );
    }
}
