//! Writes new values into a package, including edits that change how many bytes a value occupies.
//!
//! Nothing is re-serialized wholesale. Each edit produces bytes for one value, which are spliced
//! over the bytes that value came from, and [`crate::write`] repairs the export table around the
//! change. Everything the reader did not touch keeps its original bytes, which is what makes a diff
//! after an edit attributable to the edit.

use std::cmp::Reverse;
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use retoc::legacy_asset::FPackageNameMap;
use retoc::zen::FPackageIndex;

use crate::datatable::{DataTable, DataTableLayout, DataTableRow, RowSpan};
use crate::header_edit::{ImportEdit, Tables, add_import, apply_import_edit};
use crate::kismet::{self, Expr};
use crate::mappings::Mappings;
use crate::package::ExportStatus;
use crate::package::{
    AssetBundle, ParsedExport, ParsedPackage, header_size, path_from, read_header,
};
use crate::props::InstancedLayout;
use crate::reader::Cursor;
use crate::remove::{plan_removal, remove_exports, reset_export};
use crate::stringtable::{StringTable, StringTableLayout};
use crate::unversioned::{self, UnversionedHeader};
use crate::value::{PropertyEntry, PropertyValue};
use crate::write::{HeaderDraft, Splice, check_inline_bulk, header_round_trips, rewrite};
use retoc::legacy_asset::FObjectExport;

/// One requested change, addressed by the byte offset the reader reported.
///
/// A defaulted value occupies no bytes, so its offset is shared with whatever is stored next. The
/// name and element index are what tell the two apart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValueEdit {
    pub offset: u64,
    #[serde(rename = "name")]
    pub expect_name: String,
    #[serde(rename = "element", default, skip_serializing_if = "Option::is_none")]
    pub expect_element: Option<u32>,
    /// What the caller believed was at that offset. A mismatch means the caller is working from a
    /// stale read, which is the one case where writing would corrupt an unrelated property.
    #[serde(rename = "kind")]
    pub expect_kind: String,
    #[serde(flatten)]
    pub op: EditOp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum EditOp {
    /// Replace a stored value, or give a defaulted or unset one a value of its own.
    Set { text: String },
    /// Flag a value as all zero, dropping its bytes. Not the same as unset: the loader clears the
    /// value rather than leaving the one it inherited.
    Clear,
    /// Give an unset struct, container or reference its minimal stored form, so its fields or
    /// elements can be edited afterwards.
    Store,
    /// Take a value out of the header altogether, so the object keeps the value it inherits.
    Unset,
    /// Replace one element of the container this edit addresses.
    SetElement { index: u32, text: String },
    /// Add an element before position `index`. An array copies the element already there, or takes
    /// the type's default when empty. A set or a map keys on its contents, so its new element
    /// takes `key`, written in the key's own kind; without one only an empty container grows, by
    /// the default key.
    Insert {
        index: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
    },
    /// Drop element `index`. A map pair counts as one element.
    Remove { index: u32 },
}

/// What a patch did, for reporting back to the user.
#[derive(Debug, Clone, Serialize)]
pub struct AppliedEdit {
    pub name: String,
    /// Where the value this edit addressed started before anything moved. For an edit that touches
    /// one element, this is the container holding it rather than the element.
    pub offset: u64,
    /// Where that value ended up once earlier edits had moved everything after them.
    pub offset_after: u64,
    /// Which element of a container was touched, for the operations that address one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub element: Option<u32>,
    /// How many elements the container should hold once the edit has landed, for the operations
    /// that change that. Verification compares against this rather than against a rendered string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elements_after: Option<usize>,
    pub before: String,
    pub after: String,
}

pub struct PatchedBundle {
    pub asset: Vec<u8>,
    pub exports: Vec<u8>,
    /// What each edit did: the value edits in order, then the row edits, then the import edits.
    pub applied: Vec<AppliedEdit>,
    /// The `.ubulk` after a bulk edit rewrote it; `None` leaves the file as it was read.
    pub bulk: Option<Vec<u8>>,
    /// The `.uptnl` after a bulk edit rewrote it.
    pub optional_bulk: Option<Vec<u8>>,
}

/// The bulk data files read beside a package, for edits that replace a payload in one.
#[derive(Debug, Clone, Copy, Default)]
pub struct Sidecars<'a> {
    pub bulk: Option<&'a [u8]>,
    pub optional_bulk: Option<&'a [u8]>,
}

/// A copy of an export and its subobjects, appended to the export table under a new name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateExport {
    pub export: u32,
    pub name: String,
    /// A level export to list the copy in. An actor the level does not name is loaded with the
    /// package and then never spawned, so a copy meant to appear in the world needs this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub into_level: Option<u32>,
}

/// New bytes for one bulk data resource, inline in its export or in a sidecar file.
#[derive(Debug, Clone)]
pub struct BulkEdit {
    pub resource: u32,
    pub bytes: Vec<u8>,
}

/// New bytes for the payload an export carries after its properties.
#[derive(Debug, Clone)]
pub struct PayloadEdit {
    pub export: u32,
    pub bytes: Vec<u8>,
}

/// A literal constant inside a function's bytecode given a new value at its own width, addressed
/// the way the disassembly prints it: the statement's loaded offset and which literal in it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptConstEdit {
    pub export: u32,
    /// The statement's loaded offset, as the disassembly prints at the start of its line.
    pub statement: u32,
    /// Which literal in that statement, counted from 0 in bytecode order.
    #[serde(default)]
    pub constant: u32,
    pub value: String,
}

/// Everything one save changes: values inside exports, the import table they point into, or the
/// export table itself. Removing or resetting exports is a save of its own: the value edits
/// address bytes those operations move or delete.
#[derive(Debug, Clone, Default)]
pub struct PackageEdits {
    pub values: Vec<ValueEdit>,
    pub imports: Vec<ImportEdit>,
    /// Changes to the export table's own rows: names, outers, archetypes and flags.
    pub exports: Vec<crate::export_edit::ExportEdit>,
    /// Rows added to, dropped from or renamed in DataTables. See [`RowOp`].
    pub rows: Vec<RowEdit>,
    /// Entries changed, added or dropped in StringTables. See [`StringOp`].
    pub strings: Vec<StringEdit>,
    /// Keys added to or dropped from MovieScene channels. See [`KeyOp`].
    pub keys: Vec<KeyEdit>,
    /// Bulk data resources given new bytes. See [`BulkEdit`].
    pub bulk: Vec<BulkEdit>,
    /// Export payloads given new bytes. See [`PayloadEdit`].
    pub payloads: Vec<PayloadEdit>,
    /// Literal constants changed in place inside bytecode. See [`ScriptConstEdit`].
    pub scripts: Vec<ScriptConstEdit>,
    /// Exports to remove, with their subobjects. See [`crate::plan_removal`].
    pub remove_exports: Vec<u32>,
    /// Exports whose stored values are dropped so they inherit everything.
    pub reset_exports: Vec<u32>,
    /// Exports copied to the end of the table with their subobjects. See [`crate::plan_duplication`].
    pub duplicate_exports: Vec<DuplicateExport>,
    /// Replacements for whole preload dependency runs. A save of its own: the runs sit in one
    /// shared table, so changing any of them rewrites all of them.
    pub dependencies: Vec<crate::dependency::DependencyEdit>,
    /// Values set inside a struct that is not stored yet, which it is stored to hold.
    pub field_sets: Vec<FieldSet>,
    /// What the edits were written against, checked before anything is patched.
    pub expect: Expected,
    /// Patch even where the package no longer matches `expect`.
    pub allow_drift: bool,
}

/// A value for a field inside a struct that stores nothing yet, addressed through the struct the
/// way a value edit addresses a value, then by field names. The struct is stored first and the
/// field set in it, which takes the package reading again in between, so this is applied by the
/// caller that can read it: see `rivals_core::asset_edit::preview_edits`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldSet {
    pub offset: u64,
    #[serde(rename = "name")]
    pub expect_name: String,
    #[serde(rename = "element", default, skip_serializing_if = "Option::is_none")]
    pub expect_element: Option<u32>,
    /// Field names from the struct down to the value set, each `Name` or `Name[element]`.
    pub path: Vec<String>,
    pub text: String,
}

/// The mark [`Expected::values`] holds for a struct a [`FieldSet`] expects to find unstored.
pub const NOT_STORED: &str = "(not stored)";

/// What an edit list expected to find, so one applied to a package that has since changed is
/// refused rather than landing on whatever now sits at the same index or offset.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Expected {
    /// Export index to the path it had.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub exports: BTreeMap<u32, String>,
    /// Import table position to the path it had.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub imports: BTreeMap<u32, String>,
    /// The value a value edit replaces, keyed by its offset, or `offset[index]` for one element.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub values: BTreeMap<String, String>,
    /// The constant a script edit replaces, keyed `export:statement:constant`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub scripts: BTreeMap<String, String>,
}

impl Expected {
    pub fn is_empty(&self) -> bool {
        self.exports.is_empty()
            && self.imports.is_empty()
            && self.values.is_empty()
            && self.scripts.is_empty()
    }

    /// The key a value edit's expected value is filed under.
    pub fn value_key(edit: &ValueEdit) -> String {
        match &edit.op {
            EditOp::SetElement { index, .. } | EditOp::Remove { index } => {
                format!("{}[{index}]", edit.offset)
            }
            _ => edit.offset.to_string(),
        }
    }

    /// The key a script edit's expected constant is filed under.
    pub fn script_key(edit: &ScriptConstEdit) -> String {
        format!("{}:{}:{}", edit.export, edit.statement, edit.constant)
    }

    fn merge(&mut self, other: Expected) {
        self.exports.extend(other.exports);
        self.imports.extend(other.imports);
        self.values.extend(other.values);
        self.scripts.extend(other.scripts);
    }
}

/// Every way the package differs from what the edits expected, as one refusal.
pub fn check_expectations(parsed: &ParsedPackage, edits: &PackageEdits) -> Result<(), String> {
    if edits.allow_drift || edits.expect.is_empty() {
        return Ok(());
    }
    let expect = &edits.expect;
    let mut drift = Vec::new();
    for (index, path) in &expect.exports {
        match parsed.exports.get(*index as usize) {
            Some(export) if export.path == *path => {}
            Some(export) => drift.push(format!(
                "export {index} was {path}, and is now {}",
                export.path
            )),
            None => drift.push(format!("export {index} was {path}, and is gone")),
        }
    }
    for (index, path) in &expect.imports {
        match parsed.imports.get(*index as usize) {
            Some(import) if import.path == *path => {}
            Some(import) => drift.push(format!(
                "import {index} was {path}, and is now {}",
                import.path
            )),
            None => drift.push(format!("import {index} was {path}, and is gone")),
        }
    }
    for edit in &edits.values {
        let Some(was) = expect.values.get(&Expected::value_key(edit)) else {
            continue;
        };
        // A value the edit cannot find is refused by the patch itself, with its own reason.
        let Some(entry) = find_at(parsed, edit.offset, &edit.expect_name, edit.expect_element)
        else {
            continue;
        };
        let now = match &edit.op {
            EditOp::SetElement { index, .. } | EditOp::Remove { index } => {
                element_value(&entry.value, *index)
            }
            EditOp::Set { .. } => Some(&entry.value),
            _ => continue,
        };
        if !now.is_some_and(|now| reads_back_as(now, was)) {
            drift.push(format!(
                "{} was {was}, and is now {}",
                entry.label(),
                now.map_or_else(|| "missing".to_string(), PropertyValue::summary)
            ));
        }
    }
    for edit in &edits.field_sets {
        if expect
            .values
            .get(&edit.offset.to_string())
            .map(String::as_str)
            != Some(NOT_STORED)
        {
            continue;
        }
        let Some(entry) = find_at(parsed, edit.offset, &edit.expect_name, edit.expect_element)
        else {
            continue;
        };
        if !matches!(
            entry.value,
            PropertyValue::Unset { .. } | PropertyValue::Default { .. }
        ) {
            drift.push(format!(
                "{} was not stored, and now stores {}",
                entry.label(),
                entry.value.summary()
            ));
        }
    }
    for edit in &edits.scripts {
        let Some(was) = expect.scripts.get(&Expected::script_key(edit)) else {
            continue;
        };
        let Some(script) = parsed
            .exports
            .get(edit.export as usize)
            .and_then(|export| export.script.as_ref())
        else {
            continue;
        };
        let Ok(old) = kismet::literal_at(script, edit.statement, edit.constant) else {
            continue;
        };
        let same = kismet::with_value(old, was)
            .is_ok_and(|expected| kismet::render(&expected) == kismet::render(old));
        if !same {
            drift.push(format!(
                "the constant at {:#X} in export {} was {was}, and is now {}",
                edit.statement,
                edit.export,
                kismet::render(old)
            ));
        }
    }
    if drift.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{DRIFT}: {}. Re-read the asset and make the edits again, or apply them anyway",
        drift.join("; ")
    ))
}

/// What `edits` finds in `parsed`, the package they were written against: the path at every export
/// and import index they name, and the value each value edit and script edit replaces. Kept with
/// the edits, it lets [`check_expectations`] notice a package that has changed since.
pub fn expectations(parsed: &ParsedPackage, edits: &PackageEdits) -> Expected {
    let mut exports: Vec<u32> = Vec::new();
    let mut imports: Vec<u32> = Vec::new();
    let raw = |index: i32, exports: &mut Vec<u32>, imports: &mut Vec<u32>| {
        if index > 0 {
            exports.push(index as u32 - 1);
        } else if index < 0 {
            imports.push((-index) as u32 - 1);
        }
    };
    exports.extend(edits.rows.iter().map(|edit| edit.export));
    exports.extend(edits.strings.iter().map(|edit| edit.export));
    exports.extend(edits.scripts.iter().map(|edit| edit.export));
    exports.extend(edits.payloads.iter().map(|edit| edit.export));
    exports.extend(edits.remove_exports.iter().copied());
    exports.extend(edits.reset_exports.iter().copied());
    exports.extend(edits.duplicate_exports.iter().map(|edit| edit.export));
    exports.extend(
        edits
            .duplicate_exports
            .iter()
            .filter_map(|edit| edit.into_level),
    );
    for edit in &edits.exports {
        exports.push(edit.export());
        match edit {
            crate::export_edit::ExportEdit::SetOuter {
                outer: Some(outer), ..
            } => exports.push(*outer),
            crate::export_edit::ExportEdit::SetClass { class: index, .. }
            | crate::export_edit::ExportEdit::SetSuper {
                super_index: index, ..
            }
            | crate::export_edit::ExportEdit::SetTemplate {
                template: index, ..
            } => raw(*index, &mut exports, &mut imports),
            _ => {}
        }
    }
    for edit in &edits.dependencies {
        exports.push(edit.export);
        let runs = &edit.runs;
        for index in runs
            .serialize_before_serialize
            .iter()
            .chain(&runs.create_before_serialize)
            .chain(&runs.serialize_before_create)
            .chain(&runs.create_before_create)
        {
            raw(*index, &mut exports, &mut imports);
        }
    }
    for edit in &edits.imports {
        match edit {
            ImportEdit::Retarget { import, .. } | ImportEdit::Remove { import } => {
                imports.push(*import)
            }
            ImportEdit::Add { .. } => {}
        }
    }

    let mut expect = Expected::default();
    for index in exports {
        if let Some(export) = parsed.exports.get(index as usize) {
            expect.exports.insert(index, export.path.clone());
        }
    }
    for index in imports {
        if let Some(import) = parsed.imports.get(index as usize) {
            expect.imports.insert(index, import.path.clone());
        }
    }
    for edit in &edits.values {
        let Some(entry) = find_at(parsed, edit.offset, &edit.expect_name, edit.expect_element)
        else {
            continue;
        };
        let now = match &edit.op {
            EditOp::SetElement { index, .. } | EditOp::Remove { index } => {
                element_value(&entry.value, *index)
            }
            EditOp::Set { .. } => Some(&entry.value),
            _ => None,
        };
        if let Some(text) = now.and_then(value_text) {
            expect.values.insert(Expected::value_key(edit), text);
        }
    }
    for edit in &edits.field_sets {
        let unstored = find_at(parsed, edit.offset, &edit.expect_name, edit.expect_element)
            .is_some_and(|entry| {
                matches!(
                    entry.value,
                    PropertyValue::Unset { .. } | PropertyValue::Default { .. }
                )
            });
        if unstored {
            expect
                .values
                .insert(edit.offset.to_string(), NOT_STORED.to_string());
        }
    }
    for edit in &edits.scripts {
        let old = parsed
            .exports
            .get(edit.export as usize)
            .and_then(|export| export.script.as_ref())
            .and_then(|script| kismet::literal_at(script, edit.statement, edit.constant).ok());
        if let Some(slot) = old.and_then(|old| kismet::literal_slots(old).into_iter().next()) {
            expect
                .scripts
                .insert(Expected::script_key(edit), slot.value);
        }
    }
    expect
}

/// A value as an edit would type it, for the kinds that can be compared that way.
fn value_text(value: &PropertyValue) -> Option<String> {
    let text = match value {
        PropertyValue::Str { value } | PropertyValue::Name { value } => value.clone(),
        PropertyValue::SoftObject { path } => path.clone(),
        PropertyValue::Text {
            value: Some(value),
            parts,
        } if parts.is_empty() => value.clone(),
        PropertyValue::Object {
            path: Some(path), ..
        } => path.clone(),
        PropertyValue::Object { index, path: None } => index.to_string(),
        PropertyValue::Bool { value } => value.to_string(),
        PropertyValue::Int { value } => value.to_string(),
        PropertyValue::UInt { value } => value.to_string(),
        PropertyValue::Byte { value } => value.to_string(),
        PropertyValue::Float { value } => value.to_string(),
        PropertyValue::Enum {
            name: Some(name), ..
        } => name.clone(),
        PropertyValue::Enum { value, .. } => value.to_string(),
        _ => return None,
    };
    reads_back_as(value, &text).then_some(text)
}

/// How a refusal from [`check_expectations`] starts, so a caller can offer to apply anyway.
pub const DRIFT: &str = "The package changed since these edits were written";

impl PackageEdits {
    /// Adds another list's edits to this one, for edits written against the same package that are
    /// to land in one save.
    pub fn merge(&mut self, other: PackageEdits) {
        self.values.extend(other.values);
        self.imports.extend(other.imports);
        self.exports.extend(other.exports);
        self.rows.extend(other.rows);
        self.strings.extend(other.strings);
        self.keys.extend(other.keys);
        self.bulk.extend(other.bulk);
        self.payloads.extend(other.payloads);
        self.scripts.extend(other.scripts);
        self.remove_exports.extend(other.remove_exports);
        self.reset_exports.extend(other.reset_exports);
        self.duplicate_exports.extend(other.duplicate_exports);
        self.dependencies.extend(other.dependencies);
        self.field_sets.extend(other.field_sets);
        self.expect.merge(other.expect);
        self.allow_drift |= other.allow_drift;
    }

    /// Whether this asks for nothing at all, so a caller can refuse before reading the package.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
            && self.imports.is_empty()
            && self.rows.is_empty()
            && self.strings.is_empty()
            && self.keys.is_empty()
            && self.bulk.is_empty()
            && self.payloads.is_empty()
            && self.scripts.is_empty()
            && self.remove_exports.is_empty()
            && self.reset_exports.is_empty()
            && self.duplicate_exports.is_empty()
            && self.exports.is_empty()
            && self.dependencies.is_empty()
            && self.field_sets.is_empty()
    }
}

/// One change to a DataTable's rows. Rows are addressed by name, which the engine keeps unique
/// within a table and compares without regard to case.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RowEdit {
    pub export: u32,
    #[serde(flatten)]
    pub op: RowOp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum RowOp {
    /// A row that stores nothing, so every column reads the row struct's default. `at` is a
    /// position in the table as it was read: the row goes in front of the one there, or last when
    /// it is `None` or equal to the row count.
    Add {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<u32>,
    },
    /// A copy of `source`'s bytes under a new name, placed like [`RowOp::Add`].
    Duplicate {
        source: String,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<u32>,
    },
    Remove {
        name: String,
    },
    Rename {
        name: String,
        to: String,
    },
}

impl RowOp {
    fn count_delta(&self) -> i32 {
        match self {
            RowOp::Add { .. } | RowOp::Duplicate { .. } => 1,
            RowOp::Remove { .. } => -1,
            RowOp::Rename { .. } => 0,
        }
    }
}

/// One change to a StringTable's entries. Existing entries are addressed by position, with the
/// key the caller saw there: a mismatch means the caller is working from a stale read.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StringEdit {
    pub export: u32,
    #[serde(flatten)]
    pub op: StringOp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum StringOp {
    SetKey {
        index: u32,
        key: String,
        to: String,
    },
    SetSource {
        index: u32,
        key: String,
        to: String,
    },
    /// A new entry with no metadata, appended after the last one.
    Add {
        key: String,
        source: String,
    },
    Remove {
        index: u32,
        key: String,
    },
    /// The marker string after an entry's source.
    SetTag {
        index: u32,
        key: String,
        to: String,
    },
    /// One metadata item of an entry, by id: replaced where the entry has one, added otherwise.
    SetMetaData {
        index: u32,
        key: String,
        id: String,
        to: String,
    },
    RemoveMetaData {
        index: u32,
        key: String,
        id: String,
    },
}

impl StringOp {
    fn count_delta(&self) -> i32 {
        match self {
            StringOp::Add { .. } => 1,
            StringOp::Remove { .. } => -1,
            StringOp::SetKey { .. }
            | StringOp::SetSource { .. }
            | StringOp::SetTag { .. }
            | StringOp::SetMetaData { .. }
            | StringOp::RemoveMetaData { .. } => 0,
        }
    }

    fn index(&self) -> Option<u32> {
        match self {
            StringOp::Add { .. } => None,
            StringOp::SetKey { index, .. }
            | StringOp::SetSource { index, .. }
            | StringOp::Remove { index, .. }
            | StringOp::SetTag { index, .. }
            | StringOp::SetMetaData { index, .. }
            | StringOp::RemoveMetaData { index, .. } => Some(*index),
        }
    }
}

/// One change to a MovieScene channel's keys. The channel is addressed like a value: by offset,
/// with the name and static-array slot the caller saw there.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyEdit {
    pub offset: u64,
    #[serde(rename = "name")]
    pub expect_name: String,
    #[serde(rename = "element", default, skip_serializing_if = "Option::is_none")]
    pub expect_element: Option<u32>,
    #[serde(flatten)]
    pub op: KeyOp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum KeyOp {
    /// A key at frame `time` holding `value`, with the tangents and modes of the key before it (or
    /// cubic, auto-tangent defaults in an empty channel). A frame that already has a key is
    /// refused.
    Add {
        time: i32,
        value: f64,
    },
    /// A copy of key `index` at frame `time`.
    Duplicate {
        index: u32,
        time: i32,
    },
    /// Key `index` carried to frame `time`, its value block travelling with it. A frame that
    /// already has a key is refused, the key's own included.
    Move {
        index: u32,
        time: i32,
    },
    Remove {
        index: u32,
    },
}

/// A splice waiting to be applied, with what decides its place among others at the same offset.
/// Everything zero-width at a block position shares that offset with whatever follows, and the
/// stream order there is innermost block first, then schema order.
struct Pending {
    splice: Splice,
    /// The block header's offset and the schema slot the bytes belong to.
    order: (u64, u32),
}

/// Applies value edits alone. See [`patch_package`].
pub fn patch_values(
    bundle: &AssetBundle<'_>,
    parsed: &ParsedPackage,
    edits: &[ValueEdit],
    mappings: Option<&Mappings>,
) -> Result<PatchedBundle, String> {
    patch_package(
        bundle,
        parsed,
        &PackageEdits {
            values: edits.to_vec(),
            ..Default::default()
        },
        mappings,
    )
}

/// Applies every edit, or none of them. `mappings` lets an enum be typed by enumerator name.
/// Without the sidecar files, a bulk edit on a payload in one is refused.
pub fn patch_package(
    bundle: &AssetBundle<'_>,
    parsed: &ParsedPackage,
    edits: &PackageEdits,
    mappings: Option<&Mappings>,
) -> Result<PatchedBundle, String> {
    patch_package_with(bundle, Sidecars::default(), parsed, edits, mappings)
}

/// Copies exports out of other packages into this one. A save of its own: the destination's tables
/// grow, so every offset an ordinary edit was addressed against would move.
pub fn patch_package_copy(
    bundle: &AssetBundle<'_>,
    parsed: &ParsedPackage,
    sources: &std::collections::BTreeMap<String, crate::copy::CopySource<'_>>,
    requests: &[crate::copy::CopyExport],
) -> Result<PatchedBundle, String> {
    header_round_trips(bundle)?;
    check_inline_bulk(bundle)?;
    let package = read_header(bundle)?;
    crate::copy::patch_copy(bundle, parsed, &package, sources, requests)
}

/// The copies read back as the objects they came from, and the destination is otherwise untouched.
pub fn verify_copy(
    before: &ParsedPackage,
    after: &ParsedPackage,
    sources: &std::collections::BTreeMap<String, crate::copy::CopySource<'_>>,
    requests: &[crate::copy::CopyExport],
) -> Result<(), String> {
    let plan = crate::copy::plan_copy(before, sources, requests)?;
    crate::copy::verify_copy(after, sources, &plan, before.exports.len())?;
    // A level listing a copied actor reads one element longer, as the request asked. The list is
    // checked on its own terms below, so skipping it here loses nothing.
    let skip: Vec<ValueEdit> = plan
        .listed
        .iter()
        .map(|slot| ValueEdit {
            offset: slot.count_at,
            expect_name: "Actors".into(),
            expect_element: None,
            expect_kind: "array".into(),
            op: EditOp::Insert {
                index: slot.actors as u32,
                key: None,
            },
        })
        .collect();
    for (was, is) in before.exports.iter().zip(&after.exports) {
        if was.path != is.path || status_name(was) != status_name(is) {
            return Err(format!(
                "{} reads as {} after a copy that only appends",
                was.path,
                status_name(is)
            ));
        }
        same_entries(
            &was.properties,
            &is.properties,
            &skip,
            &Excuses::default(),
            was.index,
        )
        .map_err(|e| format!("{}: {e}", was.path))?;
    }
    verify_copied_actors(before, after, &plan)?;
    Ok(())
}

/// Each level a copy was listed in reads one longer, with the copy last and nothing else moved,
/// and still reads as a level: a grown actor list that stopped decoding would be a package the
/// game loads and then cannot spawn from.
fn verify_copied_actors(
    before: &ParsedPackage,
    after: &ParsedPackage,
    plan: &crate::copy::CopyPlan,
) -> Result<(), String> {
    let mut wanted: std::collections::BTreeMap<u32, usize> = std::collections::BTreeMap::new();
    for slot in &plan.listed {
        *wanted.entry(slot.export).or_default() += 1;
    }
    for (level, added) in wanted {
        let was = actor_indices(before, level)?;
        let is = actor_indices(after, level)?;
        if is.len() != was.len() + added {
            return Err(format!(
                "export {level} lists {} actors after the copy, not {}",
                is.len(),
                was.len() + added
            ));
        }
        if is[..was.len()] != was[..] {
            return Err(format!("export {level}'s existing actors moved"));
        }
        for index in &is[was.len()..] {
            let at = (index - 1) as usize;
            if after.exports.get(at).is_none_or(|export| {
                export.outer_index != FPackageIndex::create_export(level).index
            }) {
                return Err(format!(
                    "export {level} lists {index}, which is not an object it owns"
                ));
            }
        }
    }
    Ok(())
}

/// [`patch_package`] with the sidecar files at hand, so bulk data in them can be replaced.
pub fn patch_package_with(
    bundle: &AssetBundle<'_>,
    sidecars: Sidecars<'_>,
    parsed: &ParsedPackage,
    edits: &PackageEdits,
    mappings: Option<&Mappings>,
) -> Result<PatchedBundle, String> {
    check_expectations(parsed, edits)?;
    header_round_trips(bundle)?;
    check_inline_bulk(bundle)?;
    let base = header_size(bundle)?;
    let package = read_header(bundle)?;
    let dropping_imports: Vec<u32> = edits
        .imports
        .iter()
        .filter_map(|edit| match edit {
            ImportEdit::Remove { import } => Some(*import),
            _ => None,
        })
        .collect();
    if !edits.dependencies.is_empty() {
        // The runs share one table addressed by one index per export, so changing any of them
        // rewrites the whole thing and moves every other export's run.
        if !edits.values.is_empty()
            || !edits.imports.is_empty()
            || !edits.rows.is_empty()
            || !edits.strings.is_empty()
            || !edits.keys.is_empty()
            || !edits.bulk.is_empty()
            || !edits.payloads.is_empty()
            || !edits.scripts.is_empty()
            || !edits.remove_exports.is_empty()
            || !edits.reset_exports.is_empty()
            || !edits.duplicate_exports.is_empty()
            || !edits.exports.is_empty()
        {
            return Err(
                "changing preload dependencies is a save of its own; save or discard the other edits first"
                    .into(),
            );
        }
        return patch_dependencies(bundle, parsed, &package, &edits.dependencies);
    }
    if !edits.exports.is_empty() {
        // These move an object's path, and every other edit here was addressed against the path
        // and the table as they stand now. A reset is the exception: a retype needs one, since the
        // old values were written under a schema that no longer applies.
        if !edits.values.is_empty()
            || !edits.imports.is_empty()
            || !edits.rows.is_empty()
            || !edits.strings.is_empty()
            || !edits.keys.is_empty()
            || !edits.bulk.is_empty()
            || !edits.payloads.is_empty()
            || !edits.scripts.is_empty()
            || !edits.remove_exports.is_empty()
            || !edits.duplicate_exports.is_empty()
        {
            return Err(
                "changing the export table is a save of its own; save or discard the other edits first"
                    .into(),
            );
        }
        // A reset rides along only with a retype, which empties the export it retypes. Nothing else
        // needs one, so any other reset is a save of its own.
        let retyped: Vec<u32> = edits
            .exports
            .iter()
            .filter_map(|edit| match edit {
                crate::export_edit::ExportEdit::SetClass { export, .. } => Some(*export),
                _ => None,
            })
            .collect();
        if let Some(loose) = edits
            .reset_exports
            .iter()
            .find(|index| !retyped.contains(index))
        {
            return Err(format!(
                "export {loose} is reset without being retyped, which is a save of its own"
            ));
        }
        return patch_export_edits(bundle, parsed, &package, edits, mappings);
    }
    if !dropping_imports.is_empty() {
        // Every index above a dropped import moves, so nothing else can ride along: the other
        // edits were addressed against the table as it stands now.
        if edits.imports.len() != dropping_imports.len()
            || !edits.values.is_empty()
            || !edits.rows.is_empty()
            || !edits.strings.is_empty()
            || !edits.keys.is_empty()
            || !edits.bulk.is_empty()
            || !edits.payloads.is_empty()
            || !edits.scripts.is_empty()
            || !edits.remove_exports.is_empty()
            || !edits.reset_exports.is_empty()
            || !edits.duplicate_exports.is_empty()
        {
            return Err(
                "removing an import is a save of its own; save or discard the other edits first"
                    .into(),
            );
        }
        return patch_import_removal(bundle, parsed, &package, &dropping_imports);
    }
    if !edits.remove_exports.is_empty()
        || !edits.reset_exports.is_empty()
        || !edits.duplicate_exports.is_empty()
    {
        if !edits.values.is_empty()
            || !edits.imports.is_empty()
            || !edits.rows.is_empty()
            || !edits.strings.is_empty()
            || !edits.keys.is_empty()
            || !edits.bulk.is_empty()
            || !edits.payloads.is_empty()
            || !edits.scripts.is_empty()
        {
            return Err(
                "removing, resetting or duplicating an export is a save of its own; save or discard the other edits first"
                    .into(),
            );
        }
        return patch_structure(bundle, parsed, &package, edits);
    }
    let mut tables = Tables {
        names: package.name_map.clone(),
        imports: package.imports.clone(),
    };
    let grown = tables.names.num_names();
    let imports_before = tables.imports.len();

    // Imports first, so a value edit can point at one added in the same save.
    let mut applied_imports = Vec::with_capacity(edits.imports.len());
    for edit in &edits.imports {
        applied_imports.push(apply_import_edit(&mut tables, &package, edit)?);
    }

    let mut applied: Vec<AppliedEdit> = Vec::with_capacity(edits.values.len());
    let mut pending: Vec<Pending> = Vec::new();
    // Where each value edit's entry starts and how it sorts among splices at that offset, so its
    // final offset is whatever lands ahead of it, whichever of its own splices moved.
    let mut anchors: Vec<(u64, (u64, u32))> = Vec::with_capacity(edits.values.len());
    // Elements removed from each container this save, by the offset the container is edited at.
    // Indices are the container's own as read, however many other edits it takes.
    let removing = removals(edits)?;
    // Every container whose element count changes, with its count's width and the net change.
    let mut counts: BTreeMap<u64, (u8, i32, usize)> = BTreeMap::new();
    // Which applied edits act on which container, so each can be told its final element count.
    let mut counted: Vec<(usize, u64)> = Vec::new();
    // Containers with no bytes yet that this save has begun writing, and the count each will hold.
    let mut absent_started: BTreeSet<u64> = BTreeSet::new();
    let mut absent_counts: Vec<(usize, usize)> = Vec::new();
    // Exports newly pointed at from a value, which the pointing export has to be able to create
    // before it serializes.
    let mut object_links: Vec<(u64, u32)> = Vec::new();
    // The keys added to each set or map in this save, since two edits cannot see each other.
    let mut inserted_keys: Vec<(u64, Vec<u8>)> = Vec::new();
    // Several edits can land in one property block, and its header may only be re-emitted once.
    let mut blocks: BTreeMap<u64, (UnversionedHeader, usize)> = BTreeMap::new();

    let tagged = !parsed.info.unversioned_properties;
    for edit in &edits.values {
        let entry = locate(parsed, edit)?;
        let (start, end) = entry
            .span
            .ok_or_else(|| format!("{} has no recorded position in this package", entry.label()))?;
        // These change which properties the header says are stored, and a tagged package has no
        // such header: each property is a tag of its own.
        if tagged && matches!(edit.op, EditOp::Clear | EditOp::Unset | EditOp::Store) {
            return Err(format!(
                "{} is a tagged property; clearing, unsetting or storing one is not supported, \
                 so set it to a value instead",
                entry.label()
            ));
        }
        let kind = kind_of(&entry.value);
        if kind != edit.expect_kind {
            return Err(format!(
                "{} is a {kind} here, not a {}. Re-read the asset and try again.",
                entry.label(),
                edit.expect_kind
            ));
        }
        let stored = end > start;

        // Every operation comes out the same shape: the bytes it splices, and the record of what
        // it did. Container edits produce two splices because the element count is written apart
        // from the elements.
        let (splices, done) = match &edit.op {
            EditOp::Set { text } => {
                let was = bytes_at(bundle, base, start, end)?;
                let bytes = encode(
                    &entry.value,
                    text,
                    Target {
                        declared: entry.slot.map_or("", |slot| slot.declared),
                        native: native_leaf_at(parsed, start),
                        width: stored.then_some(end - start),
                        was,
                        tables: &mut tables,
                        package: &package,
                        // A tagged enum is written as its enumerator's name, the way a
                        // container element is.
                        element: tagged
                            && matches!(entry.value, PropertyValue::Enum { .. })
                            && end - start == 8,
                        enums: mappings,
                    },
                )?;
                if let Some(target) = export_reference(&entry.value, &bytes) {
                    object_links.push((start, target));
                }
                if let Some((layout, slot)) = channel_time_at(parsed, start) {
                    check_frame_order(layout, slot, &bytes, &entry.label())?;
                }
                if !stored {
                    mark(&mut blocks, bundle, base, entry, true)?;
                }
                (
                    vec![Splice { start, end, bytes }],
                    AppliedEdit {
                        name: entry.label(),
                        offset: start,
                        offset_after: start,
                        element: None,
                        elements_after: None,
                        before: entry.value.summary(),
                        after: text.clone(),
                    },
                )
            }
            EditOp::Clear => {
                if !stored && !matches!(entry.value, PropertyValue::Unset { .. }) {
                    return Err(format!("{} is already zero", entry.label()));
                }
                mark(&mut blocks, bundle, base, entry, false)?;
                (
                    vec![Splice {
                        start,
                        end,
                        bytes: Vec::new(),
                    }],
                    AppliedEdit {
                        name: entry.label(),
                        offset: start,
                        offset_after: start,
                        element: None,
                        elements_after: None,
                        before: entry.value.summary(),
                        after: "(zero)".to_string(),
                    },
                )
            }
            EditOp::Store => {
                if !matches!(
                    entry.value,
                    PropertyValue::Unset { .. } | PropertyValue::Default { .. }
                ) {
                    return Err(format!("{} is already stored", entry.label()));
                }
                let bytes = stored_default(parsed, entry, &mut tables.names)?;
                mark(&mut blocks, bundle, base, entry, true)?;
                (
                    vec![Splice {
                        start,
                        end: start,
                        bytes,
                    }],
                    AppliedEdit {
                        name: entry.label(),
                        offset: start,
                        offset_after: start,
                        element: None,
                        elements_after: None,
                        before: entry.value.summary(),
                        after: "(stored)".to_string(),
                    },
                )
            }
            EditOp::Unset => {
                if matches!(entry.value, PropertyValue::Unset { .. }) {
                    return Err(format!("{} is not stored", entry.label()));
                }
                with_block(&mut blocks, bundle, base, entry, |header, slot| {
                    header.remove_value(slot)
                })?;
                (
                    vec![Splice {
                        start,
                        end,
                        bytes: Vec::new(),
                    }],
                    AppliedEdit {
                        name: entry.label(),
                        offset: start,
                        offset_after: start,
                        element: None,
                        elements_after: None,
                        before: entry.value.summary(),
                        after: "(not stored)".to_string(),
                    },
                )
            }
            EditOp::SetElement { index, text } => {
                if removing
                    .get(&start)
                    .is_some_and(|gone| gone.contains(index))
                {
                    return Err(format!(
                        "{}[{index}] is both removed and given a value in one save",
                        entry.label()
                    ));
                }
                let layout = container_at(parsed, start, entry)?;
                let (from, to) = value_span(layout, *index, entry)?;
                let element = element_value(&entry.value, *index)
                    .ok_or_else(|| format!("{} has no element {index}", entry.label()))?;
                let bytes = encode(
                    element,
                    text,
                    Target {
                        declared: layout.element_kind,
                        native: native_leaf_at(parsed, from),
                        width: Some(to - from),
                        was: bytes_at(bundle, base, from, to)?,
                        tables: &mut tables,
                        package: &package,
                        element: layout.element_is_enum,
                        enums: mappings,
                    },
                )?;
                if let Some(target) = export_reference(element, &bytes) {
                    object_links.push((start, target));
                }
                (
                    vec![Splice {
                        start: from,
                        end: to,
                        bytes,
                    }],
                    AppliedEdit {
                        name: format!("{}[{index}]", entry.label()),
                        offset: start,
                        offset_after: start,
                        element: Some(*index),
                        elements_after: None,
                        before: element.summary(),
                        after: text.clone(),
                    },
                )
            }
            EditOp::Insert { index, key } => {
                let layout = container_at(parsed, start, entry)?;
                let (at, bytes, key_len) = insertion(
                    layout,
                    *index,
                    key.as_deref(),
                    removing.get(&start),
                    bundle,
                    base,
                    entry,
                    &mut tables,
                    &package,
                    mappings,
                )?;
                // A map pair is its key then its value; a set's element is its own key.
                let (key_part, value_part) = match &layout.keys {
                    Some(_) => bytes.split_at(key_len.min(bytes.len())),
                    None => (&[][..], &bytes[..]),
                };
                if layout
                    .keys
                    .as_ref()
                    .is_some_and(|keys| is_object_kind(keys.kind))
                    && let Some(target) = export_target(key_part)
                {
                    object_links.push((start, target));
                }
                if is_object_kind(layout.element_kind)
                    && let Some(target) = export_target(value_part)
                {
                    object_links.push((start, target));
                }
                if key_len > 0 {
                    let key_bytes = bytes[..key_len].to_vec();
                    if inserted_keys
                        .iter()
                        .any(|(at, held)| *at == start && *held == key_bytes)
                    {
                        return Err(format!(
                            "{} is given the same key twice in one save",
                            entry.label()
                        ));
                    }
                    inserted_keys.push((start, key_bytes));
                }
                let mut bytes = bytes;
                if layout.absent.is_some() {
                    // Nothing is stored yet: the first element brings the count, and a set or map
                    // the removal list in front of it, and the header learns the slot is stored.
                    let total = edits
                        .values
                        .iter()
                        .filter(|other| {
                            other.offset == edit.offset
                                && other.expect_name == edit.expect_name
                                && matches!(other.op, EditOp::Insert { .. })
                        })
                        .count();
                    if absent_started.insert(start) {
                        let mut head = Vec::with_capacity(8);
                        if matches!(
                            entry.value,
                            PropertyValue::Set { .. }
                                | PropertyValue::Map { .. }
                                | PropertyValue::Unset {
                                    declared: "Set" | "Map",
                                    ..
                                }
                        ) {
                            head.extend_from_slice(&0i32.to_le_bytes());
                        }
                        head.extend_from_slice(&(total as i32).to_le_bytes());
                        head.extend(bytes);
                        bytes = head;
                        mark(&mut blocks, bundle, base, entry, true)?;
                    }
                    absent_counts.push((applied.len(), total));
                } else {
                    let held = counts.entry(layout.count_at).or_insert((
                        layout.count_width,
                        0,
                        layout.elements.len(),
                    ));
                    held.1 += 1;
                    counted.push((applied.len(), layout.count_at));
                }
                (
                    vec![Splice {
                        start: at,
                        end: at,
                        bytes,
                    }],
                    AppliedEdit {
                        name: entry.label(),
                        offset: start,
                        offset_after: start,
                        element: Some(*index),
                        elements_after: None,
                        before: entry.value.summary(),
                        after: String::new(),
                    },
                )
            }
            EditOp::Remove { index } => {
                let layout = container_at(parsed, start, entry)?;
                let (from, to) = element_span(layout, *index, entry)?;
                let held = counts.entry(layout.count_at).or_insert((
                    layout.count_width,
                    0,
                    layout.elements.len(),
                ));
                held.1 -= 1;
                counted.push((applied.len(), layout.count_at));
                (
                    vec![Splice {
                        start: from,
                        end: to,
                        bytes: Vec::new(),
                    }],
                    AppliedEdit {
                        name: format!("{}[{index}]", entry.label()),
                        offset: start,
                        offset_after: start,
                        element: Some(*index),
                        elements_after: None,
                        before: entry.value.summary(),
                        after: String::new(),
                    },
                )
            }
        };

        let order = entry
            .slot
            .map_or((0, 0), |slot| (slot.header_at, slot.schema_index));
        anchors.push((start, order));
        pending.extend(splices.into_iter().map(|splice| Pending { splice, order }));
        applied.push(done);
    }
    // One count change per container, however many elements it gains and loses.
    for (&count_at, &(width, delta, _)) in &counts {
        if delta != 0 {
            pending.push(Pending {
                splice: adjust_count_width(bundle, base, count_at, width, delta)?,
                order: (count_at, 0),
            });
        }
    }
    for (at, count_at) in counted {
        let (_, delta, was) = counts[&count_at];
        let now = (was as i64 + i64::from(delta)).max(0) as usize;
        applied[at].elements_after = Some(now);
        applied[at].after = format!("{now} items");
    }
    for (at, now) in absent_counts {
        applied[at].elements_after = Some(now);
        applied[at].after = format!("{now} items");
    }

    // A row inserted at a row boundary goes behind anything a value edit put there: whatever the
    // previous row's block stores at its end belongs to that row. An order of zero sorts last.
    let mut row_applied = Vec::with_capacity(edits.rows.len());
    let mut row_owns = Vec::with_capacity(edits.rows.len());
    for (splice, done) in row_splices(parsed, bundle, base, &mut tables.names, edits)? {
        row_owns.push(pending.len());
        pending.push(Pending {
            splice,
            order: (0, 0),
        });
        row_applied.push(done);
    }
    let mut row_deltas: BTreeMap<u32, i32> = BTreeMap::new();
    for edit in &edits.rows {
        *row_deltas.entry(edit.export).or_default() += edit.op.count_delta();
    }
    for (export, delta) in row_deltas {
        if delta == 0 {
            continue;
        }
        let (layout, _) = table_of(parsed, export)?;
        pending.push(Pending {
            splice: adjust_count(bundle, base, layout.count_at, delta)?,
            order: (layout.count_at, 0),
        });
    }

    // String table entries sit after the export's properties, so nothing else shares their bytes.
    let strings = string_splices(parsed, edits, &mut tables.names)?;
    for (splice, done) in strings.edits {
        row_owns.push(pending.len());
        pending.push(Pending {
            splice,
            order: (0, 0),
        });
        row_applied.push(done);
    }
    for splice in strings.follow {
        pending.push(Pending {
            splice,
            order: (0, 0),
        });
    }
    let mut string_deltas: BTreeMap<u32, i32> = BTreeMap::new();
    for edit in &edits.strings {
        *string_deltas.entry(edit.export).or_default() += edit.op.count_delta();
    }
    for (export, delta) in string_deltas {
        if delta == 0 {
            continue;
        }
        let (layout, _) = string_table_of(parsed, export)?;
        pending.push(Pending {
            splice: adjust_count(bundle, base, layout.count_at, delta)?,
            order: (layout.count_at, 0),
        });
    }
    let mut record_deltas: BTreeMap<u64, i32> = BTreeMap::new();
    for (count_at, delta) in strings.counts {
        *record_deltas.entry(count_at).or_default() += delta;
    }
    for (count_at, delta) in record_deltas {
        if delta != 0 {
            pending.push(Pending {
                splice: adjust_count(bundle, base, count_at, delta)?,
                order: (count_at, 0),
            });
        }
    }

    // Payloads and bulk data are replaced whole; the bulk table follows their new sizes.
    let payload_out = payload_splices(parsed, edits, &package)?;
    for (splice, done) in payload_out {
        pending.push(Pending {
            splice,
            order: (0, 0),
        });
        applied_imports.push(done);
    }
    for (splice, done) in script_splices(parsed, edits, &mut tables.names)? {
        pending.push(Pending {
            splice,
            order: (0, 0),
        });
        applied_imports.push(done);
    }
    let bulk_out = bulk_edits(bundle, &package, sidecars, edits)?;
    for splice in bulk_out.splices {
        pending.push(Pending {
            splice,
            order: (0, 0),
        });
    }
    applied_imports.extend(bulk_out.applied);

    // A channel's keys are two bulk arrays whose counts move together.
    let key_out = key_splices(parsed, bundle, base, edits)?;
    for splice in key_out.splices {
        pending.push(Pending {
            splice,
            order: (0, 0),
        });
    }
    let mut key_deltas: BTreeMap<u64, i32> = BTreeMap::new();
    for (count_at, delta) in key_out.counts {
        *key_deltas.entry(count_at).or_default() += delta;
    }
    for (count_at, delta) in key_deltas {
        if delta != 0 {
            pending.push(Pending {
                splice: adjust_count(bundle, base, count_at, delta)?,
                order: (count_at, 0),
            });
        }
    }

    for (at, (header, was)) in blocks {
        pending.push(Pending {
            splice: Splice {
                start: at,
                end: at + was as u64,
                bytes: header.write()?,
            },
            order: (at, 0),
        });
    }

    // An instanced struct guards its payload with a byte length, so every edit inside one moves
    // that length by what it added or removed. Each enclosing payload is summed on its own, which
    // is what keeps nested payloads right.
    let splices_so_far: Vec<Splice> = pending.iter().map(|held| held.splice.clone()).collect();
    for (size_at, delta) in prefix_deltas(&parsed.instanced, &splices_so_far) {
        let was = i32::from_le_bytes(
            bytes_at(bundle, base, size_at, size_at + 4)?
                .try_into()
                .map_err(|_| "instanced struct length is not four bytes".to_string())?,
        );
        let now = i64::from(was)
            .checked_add(delta)
            .and_then(|now| i32::try_from(now).ok())
            .ok_or("instanced struct length does not fit")?;
        pending.push(Pending {
            splice: Splice {
                start: size_at,
                end: size_at + 4,
                bytes: now.to_le_bytes().to_vec(),
            },
            order: (size_at, 0),
        });
    }

    // Zero-width splices share their offset with the value that follows them; among themselves the
    // innermost block comes first, then schema order, which is the order the bytes take in the
    // stream.
    let mut ranked: Vec<usize> = (0..pending.len()).collect();
    ranked.sort_by_key(|&index| {
        let held = &pending[index];
        (
            held.splice.start,
            held.splice.end,
            Reverse(held.order.0),
            held.order.1,
        )
    });
    let mut rank_of = vec![0usize; pending.len()];
    for (rank, &index) in ranked.iter().enumerate() {
        rank_of[index] = rank;
    }
    let splices: Vec<Splice> = ranked
        .iter()
        .map(|&index| pending[index].splice.clone())
        .collect();

    let names_changed = tables.names.num_names() > grown;
    let imports_changed = !edits.imports.is_empty() || tables.imports.len() != imports_before;
    let links: Vec<(usize, u32)> = object_links
        .iter()
        .filter_map(|&(at, target)| {
            package
                .exports
                .iter()
                .position(|export| {
                    let start = export.serial_offset.max(0) as u64;
                    start <= at && at < start + export.serial_size.max(0) as u64
                })
                .map(|owner| (owner, target))
        })
        .collect();
    let (dependency_exports, dependencies) = match add_serialize_dependencies(&package, &links)? {
        Some((exports, dependencies)) => (Some(exports), Some(dependencies)),
        None => (None, None),
    };
    let rewritten = rewrite(
        bundle,
        &splices,
        HeaderDraft {
            names: names_changed.then_some(tables.names),
            imports: imports_changed.then_some(tables.imports),
            exports: dependency_exports,
            preload_dependencies: dependencies,
            data_resources: bulk_out.table,
            ..Default::default()
        },
    )?;

    // Spans are absolute across the two files, so a name map that grew pushes every value in the
    // export data along with the header it sits behind. Within the export data a value moves by
    // every splice sorted before its own, which is what makes two edits at one offset come out
    // right.
    let header_delta = rewritten.asset.len() as i64 - bundle.asset.len() as i64;
    let mut shift_before = vec![0i64; splices.len() + 1];
    for (rank, splice) in splices.iter().enumerate() {
        shift_before[rank + 1] = shift_before[rank] + splice.delta();
    }
    let sort_key =
        |start: u64, end: u64, order: (u64, u32)| (start, end, Reverse(order.0), order.1);
    for (entry, (start, order)) in applied.iter_mut().zip(&anchors) {
        let ahead = ranked.partition_point(|&index| {
            let held = &pending[index];
            sort_key(held.splice.start, held.splice.end, held.order)
                < sort_key(*start, *start, *order)
        });
        entry.offset_after = entry
            .offset
            .saturating_add_signed(shift_before[ahead] + header_delta);
    }
    for (entry, own) in row_applied.iter_mut().zip(&row_owns) {
        entry.offset_after = entry
            .offset
            .saturating_add_signed(shift_before[rank_of[*own]] + header_delta);
    }
    applied.extend(row_applied);
    applied.extend(key_out.applied);
    applied.extend(applied_imports);

    check_inline_bulk(&AssetBundle {
        asset: &rewritten.asset,
        exports: &rewritten.exports,
    })?;
    Ok(PatchedBundle {
        asset: rewritten.asset,
        exports: rewritten.exports,
        applied,
        bulk: bulk_out.bulk,
        optional_bulk: bulk_out.optional_bulk,
    })
}

/// Removes and resets exports. Reference splices inside an export being reset are dropped: its
/// property bytes are replaced wholesale, and the two would otherwise cover the same bytes.
/// The export a written object reference points at, when it is one of this package's exports.
fn export_reference(value: &PropertyValue, bytes: &[u8]) -> Option<u32> {
    let object = matches!(value, PropertyValue::Object { .. })
        || matches!(value, PropertyValue::Unset { declared, .. } if *declared == "Object");
    if !object {
        return None;
    }
    export_target(bytes)
}

/// The export a four-byte `FPackageIndex` names, when it names one of this package's exports.
fn export_target(bytes: &[u8]) -> Option<u32> {
    if bytes.len() != 4 {
        return None;
    }
    let raw = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    (raw > 0).then(|| (raw - 1) as u32)
}

/// The kinds stored as a hard `FPackageIndex`, whose target the loader has to create first. A weak
/// or soft reference resolves later and asks for no such promise.
fn is_object_kind(kind: &str) -> bool {
    matches!(kind, "Object" | "Interface")
}

/// Which native struct starts at `at`, when a value edit lands on one written unlike its kind.
fn native_leaf_at(parsed: &ParsedPackage, at: u64) -> Option<crate::props::NativeLeaf> {
    parsed
        .native_leaves
        .iter()
        .find_map(|(start, leaf)| (*start == at).then_some(*leaf))
}

/// The channel and key slot whose frame word starts at `at`, when a value edit lands on one.
fn channel_time_at(
    parsed: &ParsedPackage,
    at: u64,
) -> Option<(&crate::props::ChannelLayout, usize)> {
    parsed.channels.iter().find_map(|layout| {
        layout
            .times
            .iter()
            .position(|time| *time == at)
            .map(|slot| (layout, slot))
    })
}

/// A key's frame may only change within the gap its neighbours leave. Passing one would reorder
/// the stream, which is what `KeyOp::Move` is for.
fn check_frame_order(
    layout: &crate::props::ChannelLayout,
    slot: usize,
    bytes: &[u8],
    label: &str,
) -> Result<(), String> {
    let frame = i32::from_le_bytes(
        bytes
            .try_into()
            .map_err(|_| format!("{label}: a frame is a four-byte word"))?,
    );
    let lower = slot
        .checked_sub(1)
        .and_then(|before| layout.frames.get(before));
    let upper = layout.frames.get(slot + 1);
    if lower.is_some_and(|held| frame <= *held) || upper.is_some_and(|held| frame >= *held) {
        return Err(format!(
            "{label}: frame {frame} would put key {slot} out of order with its neighbours; move \
             the key instead"
        ));
    }
    Ok(())
}

/// The dependency runs with a create-before-serialize edge from each owner to each export it now
/// points at, where the runs lack one. The loader creates a dependency before serializing the
/// export that names it, which a pointer written by hand would otherwise not be promised. Rebuilt
/// in table order so every export's first index stays right; `None` when nothing was missing.
type DependencyDraft = (Vec<FObjectExport>, Vec<FPackageIndex>);

pub(crate) fn add_serialize_dependencies(
    header: &retoc::legacy_asset::FLegacyPackageHeader,
    links: &[(usize, u32)],
) -> Result<Option<DependencyDraft>, String> {
    if links.is_empty() {
        return Ok(None);
    }
    let mut exports = header.exports.clone();
    let added = link_dependencies(&mut exports, &header.preload_dependencies, links)?;
    Ok(added.map(|dependencies| (exports, dependencies)))
}

/// The same over a table the caller is already holding, which is how a duplication links its
/// copies: their runs sit in the list `duplicate_exports` built, not in the header's.
///
/// Returns the whole rebuilt list when anything was added, since the runs are addressed by one
/// index per export and adding to one moves every run after it.
pub(crate) fn link_dependencies(
    exports: &mut [FObjectExport],
    preload: &[FPackageIndex],
    links: &[(usize, u32)],
) -> Result<Option<Vec<FPackageIndex>>, String> {
    let mut dependencies = Vec::with_capacity(preload.len() + links.len());
    let mut added = false;
    for (position, export) in exports.iter_mut().enumerate() {
        let first = export.first_export_dependency_index;
        let mut cursor = usize::try_from(first).unwrap_or(0);
        let start = dependencies.len() as i32;
        let mut runs = [
            export.serialize_before_serialize_dependencies,
            export.create_before_serialize_dependencies,
            export.serialize_before_create_dependencies,
            export.create_before_create_dependencies,
        ];
        for (slot, run) in runs.iter_mut().enumerate() {
            let mut held = Vec::new();
            for _ in 0..usize::try_from(*run).unwrap_or(0) {
                let dep = preload
                    .get(cursor)
                    .copied()
                    .ok_or("the preload dependencies run past their table")?;
                cursor += 1;
                held.push(dep);
            }
            if slot == 1 {
                for &(_, target) in links.iter().filter(|(owner, _)| *owner == position) {
                    let wanted = FPackageIndex::create_export(target);
                    if target as usize != position && !held.contains(&wanted) {
                        held.push(wanted);
                        added = true;
                    }
                }
            }
            *run = held.len() as i32;
            dependencies.extend(held);
        }
        [
            export.serialize_before_serialize_dependencies,
            export.create_before_serialize_dependencies,
            export.serialize_before_create_dependencies,
            export.create_before_create_dependencies,
        ] = runs;
        if first >= 0 || dependencies.len() as i32 > start {
            export.first_export_dependency_index = start;
        }
    }
    Ok(added.then_some(dependencies))
}

/// Each level the duplication listed a copy in reads one longer, with the copies last and nothing
/// else moved, and still reads as a level: an actor list that grew but stopped decoding would be a
/// package the game loads and then cannot spawn from.
fn verify_level_listing(
    before: &ParsedPackage,
    after: &ParsedPackage,
    plans: &[crate::DuplicatePlan],
) -> Result<(), String> {
    let mut wanted: std::collections::BTreeMap<u32, Vec<u32>> = std::collections::BTreeMap::new();
    for plan in plans {
        let Some(slot) = &plan.level else {
            continue;
        };
        let copy = plan
            .members
            .iter()
            .position(|&member| member == plan.root)
            .and_then(|at| plan.copies.get(at).copied())
            .ok_or("the duplication plan does not name the copy of its own root")?;
        wanted.entry(slot.export).or_default().push(copy);
    }
    for (level, added) in wanted {
        let was = actor_indices(before, level)?;
        let is = actor_indices(after, level)?;
        if is.len() != was.len() + added.len() {
            return Err(format!(
                "export {level} lists {} actors after the copy, not {}",
                is.len(),
                was.len() + added.len()
            ));
        }
        if is[..was.len()] != was[..] {
            return Err(format!("export {level}'s existing actors moved"));
        }
        let appended: Vec<u32> = is[was.len()..]
            .iter()
            .map(|index| (index - 1) as u32)
            .collect();
        if appended != added {
            return Err(format!(
                "export {level} lists {appended:?} as its new actors, not {added:?}"
            ));
        }
    }
    Ok(())
}

/// The raw indices a level's actor list holds, refusing an export that stopped reading as one.
fn actor_indices(parsed: &ParsedPackage, level: u32) -> Result<Vec<i32>, String> {
    let export = parsed
        .exports
        .get(level as usize)
        .ok_or_else(|| format!("the package has no export {level}"))?;
    if !matches!(
        export.status,
        ExportStatus::Payload {
            kind: "level data",
            ..
        }
    ) {
        return Err(format!(
            "export {level} does not read as level data after the copy"
        ));
    }
    let entry = export
        .properties
        .iter()
        .find(|entry| entry.name == "Actors")
        .ok_or_else(|| format!("export {level} has no actor list"))?;
    match &entry.value {
        PropertyValue::Array { items } => items
            .iter()
            .map(|item| match item {
                PropertyValue::Object { index, .. } => Ok(*index),
                other => Err(format!(
                    "export {level}'s actor list holds {}, which is not a reference",
                    other.summary()
                )),
            })
            .collect(),
        other => Err(format!(
            "export {level}'s actor list reads as {}",
            other.summary()
        )),
    }
}

/// The splices that list a duplication's copies in the levels the request named, and the
/// create-before-serialize edges that make each level load the actor it now names.
struct LevelListing {
    splices: Vec<Splice>,
    /// One `(level, root copy)` pair per copy listed.
    links: Vec<(usize, u32)>,
}

/// An actor a level does not name is loaded with the package and never spawned, so a copy meant to
/// appear in the world is appended to the list and the count moved to match. Counts are summed per
/// level first: two copies into one list are one change to one word, not two overlapping ones.
fn list_in_levels(
    bundle: &AssetBundle<'_>,
    plans: &[crate::DuplicatePlan],
) -> Result<LevelListing, String> {
    let base = header_size(bundle)?;
    let mut splices = Vec::new();
    let mut links = Vec::new();
    let mut growth: std::collections::BTreeMap<u64, i32> = std::collections::BTreeMap::new();
    for plan in plans {
        let Some(slot) = &plan.level else {
            continue;
        };
        let copy = plan
            .members
            .iter()
            .position(|&member| member == plan.root)
            .and_then(|at| plan.copies.get(at).copied())
            .ok_or("the duplication plan does not name the copy of its own root")?;
        splices.push(Splice {
            start: slot.append_at,
            end: slot.append_at,
            bytes: FPackageIndex::create_export(copy)
                .index
                .to_le_bytes()
                .to_vec(),
        });
        *growth.entry(slot.count_at).or_default() += 1;
        links.push((slot.export as usize, copy));
    }
    for (count_at, by) in growth {
        splices.push(adjust_count(bundle, base, count_at, by)?);
    }
    splices.sort_by_key(|splice| (splice.start, splice.end));
    Ok(LevelListing { splices, links })
}

/// The import table after a removal: shorter by what went, and every retained import still
/// naming exactly the object it named before, at whatever index it now sits.
fn verify_import_removal(
    before: &ParsedPackage,
    after: &ParsedPackage,
    dropped: &[i32],
) -> Result<(), String> {
    let kept: Vec<&crate::package::ImportInfo> = before
        .imports
        .iter()
        .filter(|import| !dropped.contains(&import.index))
        .collect();
    if kept.len() != after.imports.len() {
        return Err(format!(
            "the import table should hold {} entries after the removal but holds {}",
            kept.len(),
            after.imports.len()
        ));
    }
    for (was, is) in kept.iter().zip(&after.imports) {
        if was.path != is.path || was.class_name != is.class_name {
            return Err(format!(
                "import {} reads as {} after the removal, not {}",
                is.index, is.path, was.path
            ));
        }
    }
    Ok(())
}

/// An export table edit changes rows and nothing else, so every export still reads the same
/// values, at the same status, and only the paths the plan named have moved.
fn verify_export_edits(
    before: &ParsedPackage,
    after: &ParsedPackage,
    edits: &PackageEdits,
) -> Result<(), String> {
    if before.exports.len() != after.exports.len() {
        return Err("the export table changed length, which no table edit does".to_string());
    }
    let plan = crate::export_edit::plan_export_edits_with(
        before,
        &edits.exports,
        None,
        &edits.reset_exports,
    )?;
    let moved: std::collections::BTreeMap<&str, &str> = plan
        .repathed
        .iter()
        .map(|(was, now)| (was.as_str(), now.as_str()))
        .collect();
    let excuses = Excuses {
        repathed: plan.repathed.clone(),
        ..Default::default()
    };
    // A retype rewrites the export outright, and a reparent changes how it reads, so both are
    // checked on their own terms rather than against the export as it was.
    let retyped: std::collections::BTreeMap<u32, i32> = edits
        .exports
        .iter()
        .filter_map(|edit| match edit {
            crate::export_edit::ExportEdit::SetClass { export, class } => Some((*export, *class)),
            _ => None,
        })
        .collect();
    let reparented: std::collections::BTreeMap<u32, i32> = edits
        .exports
        .iter()
        .filter_map(|edit| match edit {
            crate::export_edit::ExportEdit::SetSuper {
                export,
                super_index,
            } => Some((*export, *super_index)),
            _ => None,
        })
        .collect();
    for (was, is) in before.exports.iter().zip(&after.exports) {
        let wanted = moved.get(was.path.as_str()).copied().unwrap_or(&was.path);
        if is.path != wanted {
            return Err(format!(
                "{} reads at {} after the edit, not {wanted}",
                was.path, is.path
            ));
        }
        if let Some(&class) = retyped.get(&was.index) {
            if is.class_index != class {
                return Err(format!(
                    "{} reads at class index {} after the retype, not {class}",
                    was.path, is.class_index
                ));
            }
            if !matches!(is.status, ExportStatus::Complete) {
                return Err(format!(
                    "{} reads as {} after the retype, not complete",
                    was.path,
                    status_name(is)
                ));
            }
            if let Some(entry) = is
                .properties
                .iter()
                .find(|entry| !matches!(entry.value, PropertyValue::Unset { .. }))
            {
                return Err(format!(
                    "{} still stores {} after being retyped",
                    was.path,
                    entry.label()
                ));
            }
            continue;
        }
        if let Some(&parent) = reparented.get(&was.index) {
            if is.super_index != parent {
                return Err(format!(
                    "{} reads at parent index {} after the reparent, not {parent}",
                    was.path, is.super_index
                ));
            }
            continue;
        }

        if was.class_name != is.class_name || status_name(was) != status_name(is) {
            return Err(format!(
                "{} reads as a {} ({}) after a table edit that changes neither",
                was.path,
                is.class_name,
                status_name(is)
            ));
        }
        same_entries(&was.properties, &is.properties, &[], &excuses, was.index)
            .map_err(|e| format!("{}: {e}", was.path))?;
    }
    Ok(())
}

/// A dependency edit changes the loading order and nothing else, so every export still reads the
/// same values at the same status, and the runs read back exactly as the edits asked.
///
/// Reading the runs back is the point: they are addressed by one index per export into a shared
/// table, so an edit that rebuilt the table wrongly would leave every other export pointing at
/// someone else's run, which nothing in the bytes would show.
fn verify_dependency_edits(
    before: &ParsedPackage,
    after: &ParsedPackage,
    edits: &PackageEdits,
) -> Result<(), String> {
    if before.exports.len() != after.exports.len() {
        return Err("the export table changed length, which no dependency edit does".to_string());
    }
    for (was, is) in before.exports.iter().zip(&after.exports) {
        if was.path != is.path || status_name(was) != status_name(is) {
            return Err(format!(
                "{} reads as {} after a dependency edit that changes neither",
                was.path,
                status_name(is)
            ));
        }
        same_entries(
            &was.properties,
            &is.properties,
            &[],
            &Excuses::default(),
            was.index,
        )
        .map_err(|e| format!("{}: {e}", was.path))?;
    }
    let wanted: std::collections::BTreeMap<u32, &crate::dependency::Runs> = edits
        .dependencies
        .iter()
        .map(|edit| (edit.export, &edit.runs))
        .collect();
    let was = before
        .dependencies
        .as_ref()
        .ok_or("the package's dependency runs were not read, so a change cannot be checked")?;
    let is = after
        .dependencies
        .as_ref()
        .ok_or("the patched package's dependency runs did not read back")?;
    if was.len() != is.len() {
        return Err("the dependency table changed shape".to_string());
    }
    for (at, (old, new)) in was.iter().zip(is).enumerate() {
        let expected = wanted.get(&(at as u32)).copied().unwrap_or(old);
        if new != expected {
            return Err(format!(
                "export {at}'s dependency runs read back as {new:?}, not {expected:?}"
            ));
        }
    }
    Ok(())
}

/// The whole of a dependency edit: the table rebuilt, every export's index into it moved to match,
/// and not one byte of any export touched.
fn patch_dependencies(
    bundle: &AssetBundle<'_>,
    parsed: &ParsedPackage,
    package: &retoc::legacy_asset::FLegacyPackageHeader,
    edits: &[crate::dependency::DependencyEdit],
) -> Result<PatchedBundle, String> {
    let plan = crate::dependency::plan_dependency_edits(parsed, package, edits)?;
    if !plan.blockers.is_empty() {
        return Err(plan.blockers.join("; "));
    }
    let (exports, dependencies) = crate::dependency::apply_dependency_edits(package, edits)?;
    let rewritten = rewrite(
        bundle,
        &[],
        HeaderDraft {
            exports: Some(exports),
            preload_dependencies: Some(dependencies),
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
        applied: edits
            .iter()
            .map(|edit| AppliedEdit {
                name: parsed
                    .exports
                    .get(edit.export as usize)
                    .map(|export| export.object_name.clone())
                    .unwrap_or_else(|| format!("export {}", edit.export)),
                offset: 0,
                offset_after: 0,
                element: None,
                elements_after: None,
                before: "(dependencies)".to_string(),
                after: format!(
                    "{} entries",
                    edit.runs.serialize_before_serialize.len()
                        + edit.runs.create_before_serialize.len()
                        + edit.runs.serialize_before_create.len()
                        + edit.runs.create_before_create.len()
                ),
            })
            .collect(),
        bulk: None,
        optional_bulk: None,
    })
}

/// The whole of an export table edit: rows changed, bytes untouched.
fn patch_export_edits(
    bundle: &AssetBundle<'_>,
    parsed: &ParsedPackage,
    package: &retoc::legacy_asset::FLegacyPackageHeader,
    edits: &PackageEdits,
    mappings: Option<&Mappings>,
) -> Result<PatchedBundle, String> {
    let patch = crate::export_edit::apply_export_edits(
        parsed,
        package,
        &edits.exports,
        mappings,
        &edits.reset_exports,
    )?;
    // A retype writes its own empty header over the range a reset would, so the resets that reach
    // here are already covered by the edits themselves.
    let mut splices = patch.splices;
    let applied = patch.applied;
    splices.sort_by_key(|splice| (splice.start, splice.end));
    let rewritten = rewrite(
        bundle,
        &splices,
        HeaderDraft {
            names: patch.names,
            exports: Some(patch.exports),
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
    })
}

/// The whole of an import removal: the table shifted, the references rewritten, nothing else.
fn patch_import_removal(
    bundle: &AssetBundle<'_>,
    parsed: &ParsedPackage,
    package: &retoc::legacy_asset::FLegacyPackageHeader,
    dropping: &[u32],
) -> Result<PatchedBundle, String> {
    let plan = crate::import_remove::plan_import_removal(parsed, package, dropping)?;
    let removal = crate::import_remove::remove_imports(parsed, package, &plan)?;
    let mut splices = removal.splices;
    splices.sort_by_key(|splice| (splice.start, splice.end));
    let rewritten = rewrite(
        bundle,
        &splices,
        HeaderDraft {
            imports: Some(removal.imports),
            exports: Some(removal.exports),
            preload_dependencies: Some(removal.preload_dependencies),
            data_resources: Some(removal.data_resources),
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
        applied: removal.applied,
        bulk: None,
        optional_bulk: None,
    })
}

fn patch_structure(
    bundle: &AssetBundle<'_>,
    parsed: &ParsedPackage,
    package: &retoc::legacy_asset::FLegacyPackageHeader,
    edits: &PackageEdits,
) -> Result<PatchedBundle, String> {
    let mut applied = Vec::new();
    let mut resets = Vec::new();
    if !edits.duplicate_exports.is_empty() {
        if !edits.remove_exports.is_empty() || !edits.reset_exports.is_empty() {
            return Err("duplicating an export is a save of its own".into());
        }
        let plans = crate::plan_duplication(parsed, &edits.duplicate_exports)?;
        let mut names = package.name_map.clone();
        let grown = names.num_names();
        let mut copies = crate::duplicate::duplicate_exports(
            parsed,
            package,
            bundle.exports,
            &mut names,
            &plans,
        )?;
        let listed = list_in_levels(bundle, &plans)?;
        if let Some(dependencies) = link_dependencies(
            &mut copies.exports,
            &copies.preload_dependencies,
            &listed.links,
        )? {
            copies.preload_dependencies = dependencies;
        }
        let rewritten = rewrite(
            bundle,
            &listed.splices,
            HeaderDraft {
                names: (names.num_names() > grown).then_some(names),
                exports: Some(copies.exports),
                preload_dependencies: Some(copies.preload_dependencies),
                appended: copies.appended,
                ..Default::default()
            },
        )?;
        check_inline_bulk(&AssetBundle {
            asset: &rewritten.asset,
            exports: &rewritten.exports,
        })?;
        return Ok(PatchedBundle {
            asset: rewritten.asset,
            exports: rewritten.exports,
            applied: copies.applied,
            bulk: None,
            optional_bulk: None,
        });
    }
    for &index in &edits.reset_exports {
        if edits.remove_exports.contains(&index) {
            return Err(format!("export {index} cannot be both removed and reset"));
        }
        let (splice, done) = reset_export(parsed, index)?;
        resets.push(splice);
        applied.push(done);
    }
    let mut splices = Vec::new();
    let mut draft = HeaderDraft::default();
    if !edits.remove_exports.is_empty() {
        let plan = plan_removal(parsed, &edits.remove_exports)?;
        let removal = remove_exports(parsed, package, bundle.exports, &plan)?;
        splices.extend(removal.splices.into_iter().filter(|splice| {
            !resets
                .iter()
                .any(|reset| reset.start <= splice.start && splice.end <= reset.end)
        }));
        applied.extend(removal.applied);
        draft.exports = Some(removal.exports);
        draft.drop_exports = removal.drop;
        draft.preload_dependencies = Some(removal.preload_dependencies);
        draft.data_resources = Some(removal.data_resources);
    }
    splices.extend(resets);
    splices.sort_by_key(|splice| (splice.start, splice.end));
    let rewritten = rewrite(bundle, &splices, draft)?;
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
    })
}

/// Payload kinds known to embed this package's own indices; new bytes have to come from the same
/// package's layout, which the report says.
const INDEX_BEARING_PAYLOADS: &[&str] = &["level data", "particle system data", "bytecode"];

/// Why an export's payload cannot be replaced: a layout the walk could not follow leaves its
/// bytecode without a known start, and inline bulk data inside a payload is addressed by the
/// table, so it goes through the bulk editor. Measured bytecode is replaceable, at its own length.
pub fn payload_lock(
    export: &ParsedExport,
    resources: &[crate::write::ResourceInfo],
) -> Option<String> {
    match &export.status {
        ExportStatus::Payload { kind, .. } => {
            if kind.ends_with("layout and bytecode") {
                return Some(
                    "The layout walk stopped before this bytecode, so it has no known start and cannot be replaced.".into(),
                );
            }
            if resources.iter().any(|r| r.owner == Some(export.index)) {
                return Some(
                    "This export holds inline bulk data inside its payload; replace that through the bulk data table instead.".into(),
                );
            }
            None
        }
        ExportStatus::Complete => {
            Some("This export carries no payload after its properties.".into())
        }
        ExportStatus::Partial { .. } => {
            Some("This export did not decode to its end, so its payload has no known start.".into())
        }
        ExportStatus::Failed { .. } => Some("This export did not decode.".into()),
    }
}

/// Where an export's payload sits: after the properties, to the export's end.
fn payload_range(export: &ParsedExport) -> Option<(u64, u64)> {
    match &export.status {
        ExportStatus::Payload {
            consumed,
            payload_bytes,
            ..
        } => {
            let start = u64::try_from(export.serial_offset).ok()? + consumed;
            Some((start, start + payload_bytes))
        }
        _ => None,
    }
}

/// One splice per payload edit, over exactly the payload's bytes.
/// The two size words a replacement script needs: the size it takes once loaded, then its stored
/// length. `None` when it does not disassemble, which is when its loaded size is unknowable.
fn bytecode_size_words(
    bytes: &[u8],
    package: &retoc::legacy_asset::FLegacyPackageHeader,
) -> Option<[u8; 8]> {
    let ctx = crate::props::Ctx {
        mappings: None,
        header: package,
        fixups: None,
        synth: None,
        local: None,
    };
    let mut scratch = crate::props::Diagnostics::default();
    let script =
        crate::kismet::read_script(bytes, 0, 0, None, bytes.len() as u32, &ctx, &mut scratch);
    if !script.complete() {
        return None;
    }
    let mut words = [0u8; 8];
    words[..4].copy_from_slice(&script.decoded_size.to_le_bytes());
    words[4..].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
    Some(words)
}

/// One narrow splice per script constant: the value bytes after the token, nothing else, so a
/// script keeps its length, its size words and every jump in it. A name that is new to the
/// package goes into `names`, which is how the header learns to grow.
fn script_splices(
    parsed: &ParsedPackage,
    edits: &PackageEdits,
    names: &mut FPackageNameMap,
) -> Result<Vec<(Splice, AppliedEdit)>, String> {
    let mut out = Vec::with_capacity(edits.scripts.len());
    let mut seen: Vec<(u32, u64)> = Vec::new();
    for edit in &edits.scripts {
        let export = parsed
            .exports
            .iter()
            .find(|e| e.index == edit.export)
            .ok_or_else(|| format!("no export {}", edit.export))?;
        if edits.payloads.iter().any(|p| p.export == edit.export) {
            return Err(format!(
                "{} is given a whole new payload in this save, so a constant inside it cannot also be set",
                export.object_name
            ));
        }
        let script = export.script.as_ref().ok_or_else(|| {
            format!(
                "export {} ({}) carries no bytecode this reader measured",
                edit.export, export.class_name
            )
        })?;
        if let Some(stop) = &script.stopped {
            return Err(format!(
                "{}'s bytecode did not disassemble whole ({}), so no constant in it can be trusted to sit where the walk says",
                export.object_name, stop.reason
            ));
        }
        let old = kismet::literal_at(script, edit.statement, edit.constant)?;
        let new = kismet::with_value(old, &edit.value)?;
        let bytes = kismet::literal_bytes(&new, names)?;
        let width = kismet::stored_width(old);
        if bytes.len() as u64 != width {
            return Err(format!(
                "{} takes {width} byte(s) and the new value {} would take {}; a script constant can only be replaced at its own width",
                kismet::literal_kind(old),
                kismet::render(&new),
                bytes.len()
            ));
        }
        let at = match old {
            Expr::IntConst { at, .. }
            | Expr::Int64Const { at, .. }
            | Expr::UInt64Const { at, .. }
            | Expr::FloatConst { at, .. }
            | Expr::DoubleConst { at, .. }
            | Expr::ByteConst { at, .. }
            | Expr::StringConst { at, .. }
            | Expr::UnicodeStringConst { at, .. }
            | Expr::NameConst { at, .. }
            | Expr::Numbers { at, .. } => *at,
            other => {
                return Err(format!(
                    "{} has no value bytes to write",
                    kismet::literal_kind(other)
                ));
            }
        };
        if seen.contains(&(edit.export, at)) {
            return Err(format!(
                "{} constant {} at 0x{:04X} is set twice in one save",
                export.object_name, edit.constant, edit.statement
            ));
        }
        seen.push((edit.export, at));
        out.push((
            Splice {
                start: at + 1,
                end: at + 1 + width,
                bytes,
            },
            AppliedEdit {
                name: format!(
                    "{} script constant {} at 0x{:04X}",
                    export.object_name, edit.constant, edit.statement
                ),
                offset: at,
                offset_after: at,
                element: None,
                elements_after: None,
                before: kismet::render(old),
                after: kismet::render(&new),
            },
        ));
    }
    Ok(out)
}

fn payload_splices(
    parsed: &ParsedPackage,
    edits: &PackageEdits,
    package: &retoc::legacy_asset::FLegacyPackageHeader,
) -> Result<Vec<(Splice, AppliedEdit)>, String> {
    let mut out = Vec::with_capacity(edits.payloads.len());
    let mut seen = Vec::new();
    for edit in &edits.payloads {
        if seen.contains(&edit.export) {
            return Err(format!(
                "export {} is given two payloads in one save",
                edit.export
            ));
        }
        seen.push(edit.export);
        let export = parsed
            .exports
            .iter()
            .find(|e| e.index == edit.export)
            .ok_or_else(|| format!("no export {}", edit.export))?;
        if let Some(reason) = payload_lock(export, &parsed.resources) {
            return Err(format!("{}: {reason}", export.object_name));
        }
        let (start, end) = payload_range(export).ok_or("the export has no payload")?;
        let kind = match &export.status {
            ExportStatus::Payload { kind, .. } => *kind,
            _ => "",
        };
        // A script that disassembles knows its own loaded size, so the two words in front of it
        // can be rewritten and the replacement may be any length. One that does not is held to its
        // own length, since nothing can say what space it would take once loaded.
        let mut words = None;
        if kind == "bytecode" {
            // Both halves are needed: what the replacement takes once loaded, and where the words
            // saying so sit. Without either, only a replacement of the same length is safe.
            let sized = export.script.as_ref().and_then(|script| {
                bytecode_size_words(&edit.bytes, package).map(|words| (script.sizes_at, words))
            });
            // The event stubs call into the event graph at fixed offsets, and latent actions resume
            // at them. Nothing rewrites those, so the graph keeps its size in both measures.
            if export.object_name.starts_with("ExecuteUbergraph_") {
                let loaded = sized
                    .map(|(_, words)| u32::from_le_bytes([words[0], words[1], words[2], words[3]]));
                let was = export.script.as_ref().map(|script| script.buffer_size);
                if edit.bytes.len() as u64 != end - start || (loaded.is_some() && loaded != was) {
                    return Err(format!(
                        "{}: this is the event graph, which the event stubs and latent actions point into at fixed offsets that are not rewritten, so its replacement has to keep its size",
                        export.object_name
                    ));
                }
            }
            match sized {
                Some(sized) => words = Some(sized),
                None if edit.bytes.len() as u64 == end - start => {}
                None => {
                    return Err(format!(
                        "{}: this replacement does not disassemble, so its loaded size is unknown. Bytecode that does not decode takes only a replacement of its own length, {} bytes",
                        export.object_name,
                        end - start
                    ));
                }
            }
        }
        let note = if INDEX_BEARING_PAYLOADS.contains(&kind) {
            "; this kind embeds package indices, so the bytes must come from this package's own layout"
        } else {
            ""
        };
        let mut resized = "";
        if let Some((at, words)) = words {
            let loaded = u32::from_le_bytes([words[0], words[1], words[2], words[3]]);
            let was = export
                .script
                .as_ref()
                .map_or_else(String::new, |s| format!("{} loaded", s.buffer_size));
            out.push((
                Splice {
                    start: at,
                    end: at + 8,
                    bytes: words.to_vec(),
                },
                AppliedEdit {
                    name: format!("{} script size", export.object_name),
                    offset: at,
                    offset_after: at,
                    element: None,
                    elements_after: None,
                    before: was,
                    after: format!("{loaded} loaded, {} stored", edit.bytes.len()),
                },
            ));
            resized = "; its size words follow it";
        }
        out.push((
            Splice {
                start,
                end,
                bytes: edit.bytes.clone(),
            },
            AppliedEdit {
                name: format!("{} payload", export.object_name),
                offset: start,
                offset_after: start,
                element: None,
                elements_after: None,
                before: format!("{} bytes of {kind}", end - start),
                after: format!("{} bytes{note}{resized}", edit.bytes.len()),
            },
        ));
    }
    Ok(out)
}

/// What the bulk edits of one save come to: splices over inline payloads, the sidecar files
/// rewritten, the table with the new sizes and offsets, and a report per edit.
struct BulkOut {
    splices: Vec<Splice>,
    applied: Vec<AppliedEdit>,
    table: Option<Vec<retoc::legacy_asset::FObjectDataResource>>,
    bulk: Option<Vec<u8>>,
    optional_bulk: Option<Vec<u8>>,
}

fn bulk_edits(
    bundle: &AssetBundle<'_>,
    package: &retoc::legacy_asset::FLegacyPackageHeader,
    sidecars: Sidecars<'_>,
    edits: &PackageEdits,
) -> Result<BulkOut, String> {
    let mut out = BulkOut {
        splices: Vec::new(),
        applied: Vec::new(),
        table: None,
        bulk: None,
        optional_bulk: None,
    };
    if edits.bulk.is_empty() {
        return Ok(out);
    }
    let mut table = package.data_resources.clone();
    let mut in_file: Vec<(u32, usize, &[u8])> = Vec::new();
    let total = i64::from(package.summary.versioning_info.total_header_size);
    for edit in &edits.bulk {
        let index = edit.resource as usize;
        let resource = table
            .get(index)
            .ok_or_else(|| format!("no bulk data resource {index}"))?;
        if edits
            .bulk
            .iter()
            .filter(|e| e.resource == edit.resource)
            .count()
            > 1
        {
            return Err(format!(
                "bulk data resource {index} is replaced twice in one save"
            ));
        }
        if let Some(reason) = crate::write::bulk_lock(resource) {
            return Err(format!("bulk data resource {index}: {reason}"));
        }
        let flags = resource.legacy_bulk_data_flags;
        if flags & crate::write::SEPARATE_PAYLOAD_FLAGS == 0 {
            let payload = crate::write::locate_inline_payload(package, bundle.exports, index)
                .ok_or_else(|| {
                    format!("bulk data resource {index} is inline, but no export holds it where the table points")
                })?;
            let start = u64::try_from(total + payload.start).map_err(|_| "offset does not fit")?;
            out.splices.push(Splice {
                start,
                end: start + payload.size as u64,
                bytes: edit.bytes.clone(),
            });
            out.applied.push(AppliedEdit {
                name: format!("bulk data {index}"),
                offset: start,
                offset_after: start,
                element: None,
                elements_after: None,
                before: format!("{} bytes inline in export {}", payload.size, payload.owner),
                after: format!("{} bytes", edit.bytes.len()),
            });
            table[index].serial_size = edit.bytes.len() as i64;
            table[index].raw_size = edit.bytes.len() as i64;
        } else {
            in_file.push((
                flags & crate::write::SEPARATE_PAYLOAD_FLAGS,
                index,
                &edit.bytes,
            ));
        }
    }
    for (class, name, file, slot) in [
        (
            crate::write::IN_SEPARATE_FILE,
            ".ubulk",
            sidecars.bulk,
            &mut out.bulk,
        ),
        (
            crate::write::OPTIONAL_PAYLOAD,
            ".uptnl",
            sidecars.optional_bulk,
            &mut out.optional_bulk,
        ),
    ] {
        let mine: Vec<(usize, &[u8])> = in_file
            .iter()
            .filter(|(held, _, _)| *held == class)
            .map(|(_, index, bytes)| (*index, *bytes))
            .collect();
        if mine.is_empty() {
            continue;
        }
        let file = file.ok_or_else(|| {
            format!(
                "the {name} file was not read with the package, so its payloads cannot be replaced"
            )
        })?;
        let rewritten = patch_sidecar(file, &mut table, class, &mine)?;
        for (index, bytes) in &mine {
            out.applied.push(AppliedEdit {
                name: format!("bulk data {index}"),
                offset: table[*index].serial_offset as u64,
                offset_after: table[*index].serial_offset as u64,
                element: None,
                elements_after: None,
                before: format!(
                    "{} bytes in {name}",
                    package.data_resources[*index].serial_size
                ),
                after: format!("{} bytes", bytes.len()),
            });
        }
        *slot = Some(rewritten);
    }
    out.table = Some(table);
    Ok(out)
}

/// Rewrites one sidecar around the resources being replaced: the entries of that file have to tile
/// it from its start, and they come out tiling the new file, offsets and sizes moved in the table.
fn patch_sidecar(
    file: &[u8],
    table: &mut [retoc::legacy_asset::FObjectDataResource],
    class: u32,
    edits: &[(usize, &[u8])],
) -> Result<Vec<u8>, String> {
    let mut members: Vec<usize> = (0..table.len())
        .filter(|&i| {
            table[i].legacy_bulk_data_flags & crate::write::SEPARATE_PAYLOAD_FLAGS == class
        })
        .collect();
    members.sort_by_key(|&i| table[i].serial_offset);
    let mut running = 0i64;
    for &i in &members {
        if table[i].serial_offset != running {
            return Err(format!(
                "bulk data resource {i} does not follow the one before it in its file, so the file cannot be rewritten safely"
            ));
        }
        running += table[i].serial_size;
    }
    if running != file.len() as i64 {
        return Err(format!(
            "the bulk data entries cover {running} bytes but the file holds {}",
            file.len()
        ));
    }
    let mut out = Vec::with_capacity(file.len());
    for &i in &members {
        let start = table[i].serial_offset as usize;
        let end = start + table[i].serial_size as usize;
        let bytes = match edits.iter().find(|(index, _)| *index == i) {
            Some((_, bytes)) => *bytes,
            None => &file[start..end],
        };
        table[i].serial_offset = out.len() as i64;
        table[i].serial_size = bytes.len() as i64;
        table[i].raw_size = bytes.len() as i64;
        out.extend_from_slice(bytes);
    }
    Ok(out)
}

/// How much each instanced struct's payload grows or shrinks under `splices`: the sum of the deltas
/// of every splice inside it. A zero-width splice on the payload's end belongs to it, the same way
/// `rewrite` charges an insertion on an export boundary to the export it came out of.
fn prefix_deltas(instanced: &[InstancedLayout], splices: &[Splice]) -> Vec<(u64, i64)> {
    instanced
        .iter()
        .filter_map(|layout| {
            let delta: i64 = splices
                .iter()
                .filter(|splice| {
                    splice.start >= layout.payload_start && splice.end <= layout.payload_end
                })
                .map(Splice::delta)
                .sum();
            (delta != 0).then_some((layout.size_at, delta))
        })
        .collect()
}

/// The layout the reader recorded for the container starting at `at`.
fn container_at<'a>(
    parsed: &'a ParsedPackage,
    at: u64,
    entry: &PropertyEntry,
) -> Result<&'a crate::props::ContainerLayout, String> {
    // A container with no bytes shares its offset with whatever is stored next, so it is found
    // by its header slot instead.
    let absent = entry.span.is_some_and(|(start, end)| start == end);
    let slot = entry.slot.map(|slot| (slot.header_at, slot.schema_index));
    parsed
        .containers
        .iter()
        .find(|layout| match layout.absent {
            Some(held) => absent && Some(held) == slot,
            None => !absent && layout.at == at,
        })
        .ok_or_else(|| {
            format!(
                "{} is not a container this reader recorded the shape of",
                entry.label()
            )
        })
}

fn element_span(
    layout: &crate::props::ContainerLayout,
    index: u32,
    entry: &PropertyEntry,
) -> Result<(u64, u64), String> {
    layout.elements.get(index as usize).copied().ok_or_else(|| {
        format!(
            "{} has {} elements, so there is no element {index}",
            entry.label(),
            layout.elements.len()
        )
    })
}

/// The bytes an element's value occupies: the whole element, or for a map pair the part after the
/// key.
fn value_span(
    layout: &crate::props::ContainerLayout,
    index: u32,
    entry: &PropertyEntry,
) -> Result<(u64, u64), String> {
    let (start, end) = element_span(layout, index, entry)?;
    let Some(keys) = &layout.keys else {
        return Ok((start, end));
    };
    let (_, key_end) = keys
        .spans
        .get(index as usize)
        .copied()
        .ok_or_else(|| format!("{} has no key recorded for pair {index}", entry.label()))?;
    Ok((key_end, end))
}

/// Finishes a default the reader left in parts: the name map spells each `None` it needs.
pub(crate) fn realise_default(
    parts: &[crate::props::DefaultPart],
    names: &mut FPackageNameMap,
) -> Result<Vec<u8>, String> {
    use crate::props::DefaultPart;
    let mut out = Vec::new();
    for part in parts {
        match part {
            DefaultPart::Bytes(bytes) => out.extend_from_slice(bytes),
            DefaultPart::NoneName => out.extend(encode_name("None", names)),
            DefaultPart::Struct(name) => {
                return Err(format!(
                    "this default needs the layout of {name}, which the mappings file does not describe"
                ));
            }
        }
    }
    Ok(out)
}

/// The bytes of an element written from nothing, for the kinds whose default the layout settled.
fn fresh_element(
    kind: &str,
    bytes: Option<&[u8]>,
    name: Option<&str>,
    recipe: Option<&[crate::props::DefaultPart]>,
    names: &mut FPackageNameMap,
    entry: &PropertyEntry,
) -> Result<Vec<u8>, String> {
    if let Some(bytes) = bytes {
        return Ok(bytes.to_vec());
    }
    if let Some(parts) = recipe {
        return realise_default(parts, names);
    }
    match (kind, name) {
        // A struct only names its default when it is tagged: the empty block is its `None`.
        ("Name" | "Enum" | "Struct", Some(name)) => Ok(encode_name(name, names)),
        ("SoftObject" | "AssetObject", Some(path)) => Ok(encode_soft_object(path, names)),
        _ => Err(format!(
            "{}: a {kind} element has no default this editor can write from nothing",
            entry.label()
        )),
    }
}

/// Where a new element goes and what it holds. In an array a copy of the element already at that
/// position is always valid bytes, so only an empty array needs the type's default. A set or a map
/// keys on its contents, so a copy would repeat a key: the new element takes the key typed for it,
/// or the default key while the container is empty, and never a key an element already carries.
#[allow(clippy::too_many_arguments)]
fn insertion(
    layout: &crate::props::ContainerLayout,
    index: u32,
    key: Option<&str>,
    removed: Option<&BTreeSet<u32>>,
    bundle: &AssetBundle<'_>,
    base: u64,
    entry: &PropertyEntry,
    tables: &mut Tables,
    package: &retoc::legacy_asset::FLegacyPackageHeader,
    mappings: Option<&Mappings>,
) -> Result<(u64, Vec<u8>, usize), String> {
    let keyed = matches!(
        entry.value,
        PropertyValue::Set { .. } | PropertyValue::Map { .. }
    ) || matches!(
        entry.value,
        PropertyValue::Unset {
            declared: "Set" | "Map",
            ..
        }
    );
    if layout.elements.is_empty() || keyed {
        let value = |names: &mut FPackageNameMap| {
            fresh_element(
                layout.element_kind,
                layout.default_element.as_deref(),
                layout.default_name.as_deref(),
                layout.default_recipe.as_deref(),
                names,
                entry,
            )
        };
        // A set's element is its own key; a map's pairs keep their keys apart from their values.
        let (key_kind, key_enum, key_default, key_default_name, key_recipe) = match &layout.keys {
            Some(keys) => (
                keys.kind,
                keys.is_enum.then_some(keys.enum_type.as_deref()),
                keys.default.as_deref(),
                keys.default_name.as_deref(),
                keys.default_recipe.as_deref(),
            ),
            None => (
                layout.element_kind,
                layout
                    .element_is_enum
                    .then_some(layout.element_enum.as_deref()),
                layout.default_element.as_deref(),
                layout.default_name.as_deref(),
                layout.default_recipe.as_deref(),
            ),
        };
        let typed = key.map(str::trim).filter(|text| !text.is_empty());
        // A struct key has no text form, so its only way in is the type's default, once.
        let mut key_len = 0;
        let mut bytes = match (keyed, typed) {
            (false, _) => value(&mut tables.names)?,
            (true, Some(text)) => {
                encode_key(key_kind, key_enum, text, tables, package, mappings, entry)?
            }
            (true, None) if layout.elements.is_empty() || key_kind == "Struct" => fresh_element(
                key_kind,
                key_default,
                key_default_name,
                key_recipe,
                &mut tables.names,
                entry,
            )?,
            (true, None) => {
                return Err(format!(
                    "{} already holds elements, so a new one needs a key of its own",
                    entry.label()
                ));
            }
        };
        if keyed {
            key_len = bytes.len();
            for (position, (start, end)) in layout.elements.iter().enumerate() {
                // A key the same save removes is free to be added again.
                if removed.is_some_and(|gone| gone.contains(&(position as u32))) {
                    continue;
                }
                let key_end = layout
                    .keys
                    .as_ref()
                    .and_then(|keys| keys.spans.get(position))
                    .map_or(*end, |(_, key_end)| *key_end);
                if key_end - start == bytes.len() as u64
                    && bytes_at(bundle, base, *start, key_end)? == bytes.as_slice()
                {
                    return Err(format!(
                        "{} already holds the key {}, and a new element would repeat it; edit \
                         that element instead",
                        entry.label(),
                        typed.unwrap_or("it defaults to")
                    ));
                }
            }
            if layout.keys.is_some() {
                bytes.extend(value(&mut tables.names)?);
            }
        }
        let at = layout.elements.last().map_or(
            layout
                .elements_at
                .unwrap_or(layout.count_at + u64::from(layout.count_width)),
            |(_, end)| *end,
        );
        return Ok((at, bytes, key_len));
    }
    let source = (index as usize).min(layout.elements.len() - 1);
    let (from, to) = layout.elements[source];
    let (data, at) = buffer_at(bundle, base, from)?;
    let bytes = data
        .get(at..at + (to - from) as usize)
        .ok_or("the element being copied lies outside the package")?
        .to_vec();
    let target = layout.elements.get(index as usize).map_or(
        layout.elements[layout.elements.len() - 1].1,
        |(start, _)| *start,
    );
    Ok((target, bytes, 0))
}

/// A typed key written in the key's own kind. An enum key is the enumerator's name (a number is
/// named through the mappings), an object key a path the package already names, a number the
/// width its kind declares. A struct key has no text form, so it is refused rather than guessed.
#[allow(clippy::too_many_arguments)]
fn encode_key(
    kind: &str,
    enum_type: Option<Option<&str>>,
    text: &str,
    tables: &mut Tables,
    package: &retoc::legacy_asset::FLegacyPackageHeader,
    mappings: Option<&Mappings>,
    entry: &PropertyEntry,
) -> Result<Vec<u8>, String> {
    if let Some(enum_type) = enum_type {
        let name = match text.parse::<i64>() {
            Ok(number) => enumerator_name(mappings, enum_type, number)?,
            Err(_) => text.to_string(),
        };
        return Ok(encode_name(&name, &mut tables.names));
    }
    let scalar = |value: PropertyValue| encode_scalar(&value, text, None, kind);
    match kind {
        "Name" => Ok(encode_name(text, &mut tables.names)),
        "Str" => Ok(encode_string(text)),
        "Object" | "WeakObject" | "Interface" => encode_object(text, package, tables, None),
        "SoftObject" | "AssetObject" => Ok(encode_soft_object(text, &mut tables.names)),
        "Int" | "Int8" | "Int16" | "Int64" => scalar(PropertyValue::Int { value: 0 }),
        "UInt16" | "UInt32" | "UInt64" => scalar(PropertyValue::UInt { value: 0 }),
        "Byte" => scalar(PropertyValue::Byte { value: 0 }),
        "Bool" => scalar(PropertyValue::Bool { value: false }),
        "Float" | "Double" => scalar(PropertyValue::Float { value: 0.0 }),
        "Struct" => Err(format!(
            "{}: a struct key cannot be typed; edit an existing element instead",
            entry.label()
        )),
        other => Err(format!("{}: a {other} key cannot be typed", entry.label())),
    }
}

/// An element or row count is a plain `i32` in front of what it counts, so changing it is a
/// four-byte splice rather than anything that has to move.
pub(crate) fn adjust_count(
    bundle: &AssetBundle<'_>,
    base: u64,
    count_at: u64,
    by: i32,
) -> Result<Splice, String> {
    adjust_count_width(bundle, base, count_at, 4, by)
}

/// The same at the width the count is written in. A native struct may hold its list behind a
/// single byte, which caps that list at 255 elements.
fn adjust_count_width(
    bundle: &AssetBundle<'_>,
    base: u64,
    count_at: u64,
    width: u8,
    by: i32,
) -> Result<Splice, String> {
    let (data, at) = buffer_at(bundle, base, count_at)?;
    let bytes = data
        .get(at..at + usize::from(width))
        .ok_or("this count lies outside the package")?;
    let count = match width {
        1 => i32::from(bytes[0]),
        4 => i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        other => return Err(format!("a {other} byte element count cannot be changed")),
    };
    let next = count
        .checked_add(by)
        .filter(|value| *value >= 0)
        .ok_or("this container cannot hold that many elements")?;
    let bytes = if width == 1 {
        vec![u8::try_from(next).map_err(|_| "this list holds at most 255 elements")?]
    } else {
        next.to_le_bytes().to_vec()
    };
    Ok(Splice {
        start: count_at,
        end: count_at + u64::from(width),
        bytes,
    })
}

/// The elements each container loses this save, keyed by the offset its edits address it at. The
/// same element removed twice is refused: indices are the container's own as read, so it would be
/// one removal written twice.
fn removals(edits: &PackageEdits) -> Result<BTreeMap<u64, BTreeSet<u32>>, String> {
    let mut out: BTreeMap<u64, BTreeSet<u32>> = BTreeMap::new();
    for edit in &edits.values {
        if let EditOp::Remove { index } = edit.op
            && !out.entry(edit.offset).or_default().insert(index)
        {
            return Err(format!(
                "{}[{index}] is removed twice in one save",
                edit.expect_name
            ));
        }
    }
    Ok(out)
}

/// Where an element read at `index` ends up once a save's inserts and removals in the same
/// container have landed. A set or map adds at its end; an array inserts before the element at
/// the index it was given.
fn index_after(edits: &PackageEdits, offset: u64, index: u32, keyed: bool) -> usize {
    let mut at = index as i64;
    for edit in edits.values.iter().filter(|edit| edit.offset == offset) {
        match edit.op {
            EditOp::Remove { index: gone } if gone < index => at -= 1,
            EditOp::Insert { index: added, .. } if !keyed && added <= index => at += 1,
            _ => {}
        }
    }
    at.max(0) as usize
}

/// The layout and decoded entries of the StringTable at `export`, for a string edit.
fn string_table_of(
    parsed: &ParsedPackage,
    export: u32,
) -> Result<(&StringTableLayout, &StringTable), String> {
    let table = parsed
        .exports
        .iter()
        .find(|candidate| candidate.index == export)
        .and_then(|candidate| candidate.string_table.as_ref())
        .ok_or_else(|| format!("export {export} is not a StringTable this reader decoded"))?;
    let layout = parsed
        .string_tables
        .iter()
        .find(|layout| layout.export == export)
        .ok_or_else(|| format!("export {export} has no entry positions recorded"))?;
    if layout.entries.len() != table.entries.len() {
        return Err(format!(
            "export {export} recorded {} entry positions for {} entries",
            layout.entries.len(),
            table.entries.len()
        ));
    }
    Ok((layout, table))
}

/// What the string edits of one save come to: a splice with its report per edit, the splices that
/// follow an edit into the metadata map (a record renamed with its entry, or removed with it), and
/// the item and record counts to move.
#[derive(Debug)]
struct StringSplices {
    edits: Vec<(Splice, AppliedEdit)>,
    follow: Vec<Splice>,
    counts: Vec<(u64, i32)>,
}

/// One splice per string edit, in edit order. An entry is addressed by position and has to carry
/// the key the caller saw; the caller is refused a key the table will still hold, two edits on one
/// entry's bytes, and any edit to an entry being removed. Metadata edits address an item by id in
/// the record keyed on the entry; a record or an item that is not there yet is appended.
fn string_splices(
    parsed: &ParsedPackage,
    edits: &PackageEdits,
    names: &mut FPackageNameMap,
) -> Result<StringSplices, String> {
    let mut out = Vec::with_capacity(edits.strings.len());
    let mut follow = Vec::new();
    let mut counts = Vec::new();
    let mut removed: Vec<(u32, u32)> = Vec::new();
    let mut rekeyed: Vec<(u32, u32, String)> = Vec::new();
    for edit in &edits.strings {
        match &edit.op {
            StringOp::Remove { index, .. } => removed.push((edit.export, *index)),
            StringOp::SetKey { index, to, .. } => rekeyed.push((edit.export, *index, to.clone())),
            _ => {}
        }
    }
    let mut taken: Vec<(u32, String)> = Vec::new();
    // Metadata items touched, and records created, so one save never writes the same item twice
    // or the same new record twice.
    let mut touched_items: Vec<(u32, u32, String)> = Vec::new();
    let mut created_records: Vec<(u32, u32)> = Vec::new();
    for edit in &edits.strings {
        let (layout, table) = string_table_of(parsed, edit.export)?;
        let record_of = |key: &str| layout.records.iter().find(|record| record.key == key);
        // The key the entry leaves the save under, which is what a new record has to be keyed on.
        let final_key = |index: u32, key: &str| -> String {
            rekeyed
                .iter()
                .find(|(export, at, _)| *export == edit.export && *at == index)
                .map_or_else(|| key.to_string(), |(_, _, to)| to.clone())
        };
        let entry_at = |index: u32, key: &str| -> Result<usize, String> {
            let position = index as usize;
            let entry = table
                .entries
                .get(position)
                .ok_or_else(|| format!("this table has no entry {index}"))?;
            if entry.key != key {
                return Err(format!(
                    "entry {index} is {} here, not {key}. Re-read the asset and try again.",
                    entry.key
                ));
            }
            if removed.contains(&(edit.export, index))
                && !matches!(edit.op, StringOp::Remove { .. })
            {
                return Err(format!(
                    "entry {key} is being removed, so it cannot be edited in the same save"
                ));
            }
            Ok(position)
        };
        let mut claim = |key: &str| -> Result<(), String> {
            if key.is_empty() {
                return Err("a string table entry needs a key".into());
            }
            let survives = table.entries.iter().enumerate().any(|(position, entry)| {
                entry.key == key
                    && !removed.contains(&(edit.export, position as u32))
                    && !rekeyed
                        .iter()
                        .any(|(export, at, _)| *export == edit.export && *at == position as u32)
            });
            let introduced = taken
                .iter()
                .any(|(export, held)| *export == edit.export && held == key);
            if survives || introduced {
                return Err(format!("this table already has an entry keyed {key}"));
            }
            taken.push((edit.export, key.to_string()));
            Ok(())
        };
        let record = |name: String, offset: u64, before: String, after: String| AppliedEdit {
            name,
            offset,
            offset_after: offset,
            element: None,
            elements_after: None,
            before,
            after,
        };
        if let (
            Some(index),
            StringOp::SetMetaData { id, .. } | StringOp::RemoveMetaData { id, .. },
        ) = (edit.op.index(), &edit.op)
        {
            let item = (edit.export, index, id.clone());
            if touched_items.contains(&item) {
                return Err(format!(
                    "metadata {id} of entry {index} is edited twice in one save"
                ));
            }
            touched_items.push(item);
        }
        out.push(match &edit.op {
            StringOp::SetKey { index, key, to } => {
                let position = entry_at(*index, key)?;
                claim(to)?;
                let (start, end) = layout.entries[position].key;
                // The metadata map keys on the entry key, so the record follows the rename.
                if let Some(held) = record_of(key) {
                    follow.push(Splice {
                        start: held.start,
                        end: held.count_at,
                        bytes: encode_string(to),
                    });
                }
                (
                    Splice {
                        start,
                        end,
                        bytes: encode_string(to),
                    },
                    record(format!("key {key}"), start, key.clone(), to.clone()),
                )
            }
            StringOp::SetTag { index, key, to } => {
                let position = entry_at(*index, key)?;
                let span = &layout.entries[position];
                (
                    Splice {
                        start: span.tag_at,
                        end: span.end,
                        bytes: encode_string(to),
                    },
                    record(
                        format!("tag {key}"),
                        span.tag_at,
                        table.entries[position].tag.clone(),
                        to.clone(),
                    ),
                )
            }
            StringOp::SetMetaData { index, key, id, to } => {
                let position = entry_at(*index, key)?;
                let was = table.entries[position]
                    .metadata
                    .iter()
                    .find(|(held, _)| held == id)
                    .map_or_else(|| "(none)".to_string(), |(_, value)| value.clone());
                let report = record(format!("metadata {key}.{id}"), 0, was, to.clone());
                match record_of(key) {
                    Some(held) => match held.items.iter().find(|item| item.id == *id) {
                        Some(item) => (
                            Splice {
                                start: item.value.0,
                                end: item.value.1,
                                bytes: encode_string(to),
                            },
                            AppliedEdit {
                                offset: item.value.0,
                                offset_after: item.value.0,
                                ..report
                            },
                        ),
                        None => {
                            let mut bytes = encode_name(id, names);
                            bytes.extend(encode_string(to));
                            counts.push((held.count_at, 1));
                            (
                                Splice {
                                    start: held.end,
                                    end: held.end,
                                    bytes,
                                },
                                AppliedEdit {
                                    offset: held.end,
                                    offset_after: held.end,
                                    ..report
                                },
                            )
                        }
                    },
                    None => {
                        if created_records.contains(&(edit.export, *index)) {
                            return Err(format!(
                                "entry {key} has no metadata yet; add its first item in one save \
                                 and the rest in the next"
                            ));
                        }
                        created_records.push((edit.export, *index));
                        let mut bytes = encode_string(&final_key(*index, key));
                        bytes.extend_from_slice(&1i32.to_le_bytes());
                        bytes.extend(encode_name(id, names));
                        bytes.extend(encode_string(to));
                        counts.push((layout.trailer_at, 1));
                        (
                            Splice {
                                start: layout.end,
                                end: layout.end,
                                bytes,
                            },
                            AppliedEdit {
                                offset: layout.end,
                                offset_after: layout.end,
                                ..report
                            },
                        )
                    }
                }
            }
            StringOp::RemoveMetaData { index, key, id } => {
                let position = entry_at(*index, key)?;
                let held = record_of(key).ok_or_else(|| format!("entry {key} has no metadata"))?;
                let item = held
                    .items
                    .iter()
                    .find(|item| item.id == *id)
                    .ok_or_else(|| format!("entry {key} has no metadata {id}"))?;
                let was = table.entries[position]
                    .metadata
                    .iter()
                    .find(|(name, _)| name == id)
                    .map_or_else(String::new, |(_, value)| value.clone());
                let (start, end) = if held.items.len() == 1 {
                    counts.push((layout.trailer_at, -1));
                    (held.start, held.end)
                } else {
                    counts.push((held.count_at, -1));
                    (item.start, item.value.1)
                };
                (
                    Splice {
                        start,
                        end,
                        bytes: Vec::new(),
                    },
                    record(
                        format!("metadata {key}.{id}"),
                        start,
                        was,
                        "(removed)".into(),
                    ),
                )
            }
            StringOp::SetSource { index, key, to } => {
                let position = entry_at(*index, key)?;
                let (start, end) = layout.entries[position].source;
                (
                    Splice {
                        start,
                        end,
                        bytes: encode_string(to),
                    },
                    record(
                        format!("string {key}"),
                        start,
                        table.entries[position].source.clone(),
                        to.clone(),
                    ),
                )
            }
            StringOp::Add { key, source } => {
                claim(key)?;
                let mut bytes = encode_string(key);
                bytes.extend(encode_string(source));
                bytes.extend_from_slice(&0i32.to_le_bytes());
                (
                    Splice {
                        start: layout.trailer_at,
                        end: layout.trailer_at,
                        bytes,
                    },
                    record(
                        format!("string {key}"),
                        layout.trailer_at,
                        "(none)".into(),
                        source.clone(),
                    ),
                )
            }
            StringOp::Remove { index, key } => {
                let position = entry_at(*index, key)?;
                if removed
                    .iter()
                    .filter(|held| **held == (edit.export, *index))
                    .count()
                    > 1
                {
                    return Err(format!("entry {key} is removed twice"));
                }
                let span = &layout.entries[position];
                // A removed entry takes its metadata record with it.
                if let Some(held) = record_of(key) {
                    follow.push(Splice {
                        start: held.start,
                        end: held.end,
                        bytes: Vec::new(),
                    });
                    counts.push((layout.trailer_at, -1));
                }
                (
                    Splice {
                        start: span.key.0,
                        end: span.end,
                        bytes: Vec::new(),
                    },
                    record(
                        format!("string {key}"),
                        span.key.0,
                        table.entries[position].source.clone(),
                        "(removed)".into(),
                    ),
                )
            }
        });
    }
    Ok(StringSplices {
        edits: out,
        follow,
        counts,
    })
}

/// What the key edits of one save come to: the frame and value splices, the counts to move, and
/// a report per edit. A channel's own splices never shift its address, so the report keeps it.
#[derive(Debug)]
struct KeySplices {
    splices: Vec<Splice>,
    counts: Vec<(u64, i32)>,
    applied: Vec<AppliedEdit>,
}

/// `RCIM_Cubic`, the interpolation a fresh key takes in a channel with no key to copy from.
const CUBIC_INTERPOLATION: u8 = 2;

/// Two splices per key edit: a frame word into `Times` and a value block into `Values`, both at the
/// position the frame sorts to, or both taken out. Edits on one channel are placed on the layout as
/// it was read, sorted so that keys landing in the same gap come out in frame order.
fn key_splices(
    parsed: &ParsedPackage,
    bundle: &AssetBundle<'_>,
    base: u64,
    edits: &PackageEdits,
) -> Result<KeySplices, String> {
    let mut out = KeySplices {
        splices: Vec::new(),
        counts: Vec::new(),
        applied: Vec::new(),
    };
    // Each edit with the channel it lands in, the frame it lands on (`i32::MIN` for a removal),
    // the key it acts on, and whether it takes that key out of its slot.
    let mut placed: Vec<(usize, usize, i32, usize, bool)> = Vec::new();
    for (position, edit) in edits.keys.iter().enumerate() {
        let layout = channel_at(parsed, edit.offset, &edit.expect_name, edit.expect_element)?;
        let frame = match &edit.op {
            KeyOp::Add { time, .. } | KeyOp::Duplicate { time, .. } | KeyOp::Move { time, .. } => {
                if layout.frames.contains(time) {
                    return Err(format!(
                        "{} already has a key at frame {time}",
                        edit.expect_name
                    ));
                }
                if placed
                    .iter()
                    .any(|(_, at, held, _, _)| *at == edit.offset as usize && *held == *time)
                {
                    return Err(format!(
                        "{} is given two keys at frame {time} in one save",
                        edit.expect_name
                    ));
                }
                *time
            }
            KeyOp::Remove { .. } => i32::MIN,
        };
        let takes = matches!(edit.op, KeyOp::Remove { .. } | KeyOp::Move { .. });
        let slot = match &edit.op {
            KeyOp::Remove { index }
            | KeyOp::Duplicate { index, .. }
            | KeyOp::Move { index, .. } => {
                if *index as usize >= layout.frames.len() {
                    return Err(format!(
                        "{} has {} keys and no key {index}",
                        edit.expect_name,
                        layout.frames.len()
                    ));
                }
                if takes
                    && placed.iter().any(|(_, at, _, held, took)| {
                        *at == edit.offset as usize && *took && *held == *index as usize
                    })
                {
                    return Err(format!(
                        "key {index} of {} is removed or moved twice",
                        edit.expect_name
                    ));
                }
                *index as usize
            }
            KeyOp::Add { .. } => 0,
        };
        placed.push((position, edit.offset as usize, frame, slot, takes));
    }
    // Adds, copies and moves sort by the frame they land on, so two in one gap come out in order.
    placed.sort_by_key(|(position, at, frame, _, _)| {
        let edit = &edits.keys[*position];
        let gap = match &edit.op {
            KeyOp::Remove { .. } => 0,
            _ => channel_at(parsed, edit.offset, &edit.expect_name, edit.expect_element)
                .map(|layout| layout.frames.iter().filter(|held| **held < *frame).count())
                .unwrap_or_default(),
        };
        (*at, gap, *frame)
    });
    for (position, _, frame, slot, _) in placed {
        let edit = &edits.keys[position];
        let layout = channel_at(parsed, edit.offset, &edit.expect_name, edit.expect_element)?;
        let label = match edit.expect_element {
            Some(element) => format!("{}[{element}]", edit.expect_name),
            None => edit.expect_name.clone(),
        };
        let count = layout.frames.len();
        let report = |before: String, after: String| AppliedEdit {
            name: label.clone(),
            offset: edit.offset,
            offset_after: edit.offset,
            element: None,
            elements_after: None,
            before,
            after,
        };
        match &edit.op {
            KeyOp::Remove { index } => {
                let (from, to) = layout.values[slot];
                out.splices.push(Splice {
                    start: layout.times[slot],
                    end: layout.times[slot] + 4,
                    bytes: Vec::new(),
                });
                out.splices.push(Splice {
                    start: from,
                    end: to,
                    bytes: Vec::new(),
                });
                out.counts.push((layout.times_count_at, -1));
                out.counts.push((layout.values_count_at, -1));
                out.applied.push(report(
                    format!("key {index} at frame {}", layout.frames[slot]),
                    format!("{} keys", count - 1),
                ));
            }
            KeyOp::Add { .. } | KeyOp::Duplicate { .. } | KeyOp::Move { .. } => {
                // Offsets are all in the bytes as read, so the gap is counted over every key held,
                // a moved key's own frame included; its own words come out by the removal splices.
                let gap = layout.frames.iter().filter(|held| **held < frame).count();
                let time_at = layout
                    .times
                    .get(gap)
                    .copied()
                    .unwrap_or(layout.times_count_at + 4 + 4 * count as u64);
                let value_at = layout.values.get(gap).map_or_else(
                    || {
                        layout
                            .values
                            .last()
                            .map_or(layout.values_count_at + 4, |(_, end)| *end)
                    },
                    |(start, _)| *start,
                );
                let block = match &edit.op {
                    KeyOp::Duplicate { .. } | KeyOp::Move { .. } => {
                        let (from, to) = layout.values[slot];
                        bytes_at(bundle, base, from, to)?.to_vec()
                    }
                    KeyOp::Add { value, .. } => key_block(layout, bundle, base, gap, *value)?,
                    KeyOp::Remove { .. } => unreachable!(),
                };
                out.splices.push(Splice {
                    start: time_at,
                    end: time_at,
                    bytes: frame.to_le_bytes().to_vec(),
                });
                out.splices.push(Splice {
                    start: value_at,
                    end: value_at,
                    bytes: block,
                });
                if let KeyOp::Move { index, .. } = &edit.op {
                    let (from, to) = layout.values[slot];
                    out.splices.push(Splice {
                        start: layout.times[slot],
                        end: layout.times[slot] + 4,
                        bytes: Vec::new(),
                    });
                    out.splices.push(Splice {
                        start: from,
                        end: to,
                        bytes: Vec::new(),
                    });
                    out.applied.push(report(
                        format!("key {index} at frame {}", layout.frames[slot]),
                        format!("key at frame {frame}"),
                    ));
                    continue;
                }
                out.counts.push((layout.times_count_at, 1));
                out.counts.push((layout.values_count_at, 1));
                out.applied.push(report(
                    format!("{count} keys"),
                    format!("{} keys, one at frame {frame}", count + 1),
                ));
            }
        }
    }
    Ok(out)
}

/// The value block of a key added at `gap`: the block of the key before it (or of the first key
/// when it goes in front) with the value replaced, or a cubic, auto-tangent block in an empty
/// channel.
fn key_block(
    layout: &crate::props::ChannelLayout,
    bundle: &AssetBundle<'_>,
    base: u64,
    gap: usize,
    value: f64,
) -> Result<Vec<u8>, String> {
    let width = layout.value_bytes as usize;
    let mut block = match layout.values.get(gap.saturating_sub(1)) {
        Some(&(from, to)) => bytes_at(bundle, base, from, to)?.to_vec(),
        None => {
            let mut fresh = vec![0u8; width];
            fresh[width - 4] = CUBIC_INTERPOLATION;
            fresh
        }
    };
    if block.len() != width {
        return Err("a key value block is not the width its channel declares".into());
    }
    // The value leads the block; the tangents and modes follow with the in-memory padding.
    match width - 24 {
        4 => block[..4].copy_from_slice(&(value as f32).to_le_bytes()),
        _ => block[..8].copy_from_slice(&value.to_le_bytes()),
    }
    Ok(block)
}

/// The layout the reader recorded for the channel starting at `at`.
fn channel_at<'a>(
    parsed: &'a ParsedPackage,
    at: u64,
    name: &str,
    element: Option<u32>,
) -> Result<&'a crate::props::ChannelLayout, String> {
    let layout = parsed
        .channels
        .iter()
        .find(|layout| layout.at == at)
        .ok_or_else(|| format!("{name} is not a channel this reader recorded the keys of"))?;
    // The offset alone would take whichever channel now sits there.
    if find_at(parsed, at, name, element).is_none() {
        return Err(format!(
            "no channel called {name} starts at {at:#X}; re-read the asset"
        ));
    }
    Ok(layout)
}

/// Replays the key edits over each channel as it was read and holds the patched channel to the
/// result: the frames in order with the added ones in, the removed ones out and the moved and
/// retimed ones where they were sent, an added key reading the value it was given, and a copied
/// or moved key reading the value it came with.
/// A script constant reads back as the value asked for, and the script around it is untouched:
/// same length, same size words, same statements, disassembled whole.
fn verify_script_edits(
    before: &ParsedPackage,
    after: &ParsedPackage,
    edits: &PackageEdits,
) -> Result<(), String> {
    for edit in &edits.scripts {
        let was = before.exports.iter().find(|e| e.index == edit.export);
        let is = after.exports.iter().find(|e| e.index == edit.export);
        let (was, is) = match (was, is) {
            (Some(was), Some(is)) => (was, is),
            _ => return Err(format!("export {} did not read back", edit.export)),
        };
        let (was_script, is_script) = match (&was.script, &is.script) {
            (Some(was_script), Some(is_script)) => (was_script, is_script),
            _ => {
                return Err(format!(
                    "{} no longer carries bytecode after patching",
                    is.object_name
                ));
            }
        };
        if let Some(stop) = &is_script.stopped {
            return Err(format!(
                "{}'s script stopped after patching: {}",
                is.object_name, stop.reason
            ));
        }
        if (
            is_script.buffer_size,
            is_script.storage_size,
            is_script.statements.len(),
        ) != (
            was_script.buffer_size,
            was_script.storage_size,
            was_script.statements.len(),
        ) {
            return Err(format!(
                "{}'s script changed shape after patching a constant in it",
                is.object_name
            ));
        }
        let wanted = kismet::with_value(
            kismet::literal_at(was_script, edit.statement, edit.constant)?,
            &edit.value,
        )?;
        let got = kismet::literal_at(is_script, edit.statement, edit.constant)?;
        if kismet::render(got) != kismet::render(&wanted) {
            return Err(format!(
                "{} constant {} at 0x{:04X} reads back as {} rather than {}",
                is.object_name,
                edit.constant,
                edit.statement,
                kismet::render(got),
                kismet::render(&wanted)
            ));
        }
    }
    Ok(())
}

fn verify_keys(
    before: &ParsedPackage,
    after: &ParsedPackage,
    edits: &PackageEdits,
) -> Result<(), String> {
    // A frame word set as a plain value is a key edit too, and the channel has to stay in order.
    let retimed: Vec<(u64, i32)> = edits
        .values
        .iter()
        .filter_map(|edit| {
            let EditOp::Set { text } = &edit.op else {
                return None;
            };
            let on_time = before
                .channels
                .iter()
                .any(|channel| channel.times.contains(&edit.offset));
            on_time
                .then(|| text.trim().parse::<i32>().ok())
                .flatten()
                .map(|frame| (edit.offset, frame))
        })
        .collect();
    if edits.keys.is_empty() && retimed.is_empty() {
        return Ok(());
    }
    if before.channels.len() != after.channels.len() {
        return Err(format!(
            "the package decoded {} channels before the edit and {} after",
            before.channels.len(),
            after.channels.len()
        ));
    }
    for (was, is) in before.channels.iter().zip(&after.channels) {
        let mine: Vec<&KeyEdit> = edits.keys.iter().filter(|e| e.offset == was.at).collect();
        let retimed_here: Vec<(usize, i32)> = retimed
            .iter()
            .filter_map(|(at, frame)| {
                was.times
                    .iter()
                    .position(|time| time == at)
                    .map(|slot| (slot, *frame))
            })
            .collect();
        if mine.is_empty() && retimed_here.is_empty() {
            if was.frames != is.frames {
                return Err(format!(
                    "the channel at {:#X} changed its keys, and nothing asked it to",
                    was.at
                ));
            }
            continue;
        }
        let source = before
            .exports
            .iter()
            .find_map(|export| entry_at(&export.properties, was.at));
        let value_of = |frame: i32| {
            source
                .and_then(|entry| key_fields(entry, frame))
                .and_then(|fields| fields.iter().find(|f| f.name == "Value"))
                .and_then(|f| match f.value {
                    PropertyValue::Float { value } => Some(value),
                    _ => None,
                })
        };
        let mut frames: Vec<Option<i32>> = was.frames.iter().copied().map(Some).collect();
        for (slot, frame) in retimed_here {
            frames[slot] = Some(frame);
        }
        let mut added: Vec<(i32, Option<f64>)> = Vec::new();
        for edit in &mine {
            match &edit.op {
                KeyOp::Remove { index } => frames[*index as usize] = None,
                KeyOp::Add { time, value } => added.push((*time, Some(*value))),
                KeyOp::Duplicate { index, time } => {
                    added.push((*time, value_of(was.frames[*index as usize])));
                }
                KeyOp::Move { index, time } => {
                    frames[*index as usize] = None;
                    added.push((*time, value_of(was.frames[*index as usize])));
                }
            }
        }
        let mut expected: Vec<i32> = frames.into_iter().flatten().collect();
        expected.extend(added.iter().map(|(time, _)| *time));
        expected.sort_unstable();
        if expected != is.frames {
            return Err(format!(
                "the channel at {:#X} reads keys at {:?}, where {:?} was expected",
                was.at, is.frames, expected
            ));
        }
        if is.frames.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(format!(
                "the channel at {:#X} reads keys out of frame order: {:?}",
                is.at, is.frames
            ));
        }
        let entry = after
            .exports
            .iter()
            .find_map(|export| entry_at(&export.properties, is.at))
            .ok_or_else(|| format!("no channel reads at {:#X} after patching", is.at))?;
        for (time, value) in added {
            let Some(value) = value else { continue };
            let key = key_fields(entry, time)
                .ok_or_else(|| format!("the key added at frame {time} did not read back"))?;
            let read = key
                .iter()
                .find(|f| f.name == "Value")
                .and_then(|f| match f.value {
                    PropertyValue::Float { value } => Some(value),
                    _ => None,
                })
                .ok_or_else(|| format!("the key at frame {time} has no value"))?;
            if (read - value).abs() > 1e-6 * value.abs().max(1.0) {
                return Err(format!(
                    "the key at frame {time} reads {read}, where {value} was written"
                ));
            }
        }
    }
    Ok(())
}

/// The entry whose bytes start at `at`, looking inside structs and array elements.
fn entry_at(fields: &[PropertyEntry], at: u64) -> Option<&PropertyEntry> {
    for field in fields {
        if field.span.is_some_and(|(start, _)| start == at) {
            return Some(field);
        }
        let found = match &field.value {
            PropertyValue::Struct { fields, .. } => entry_at(fields, at),
            PropertyValue::Array { items } => items.iter().find_map(|item| match item {
                PropertyValue::Struct { fields, .. } => entry_at(fields, at),
                _ => None,
            }),
            _ => None,
        };
        if found.is_some() {
            return found;
        }
    }
    None
}

/// The fields of the `Keys[i]` entry of a channel whose frame is `time`.
fn key_fields(channel: &PropertyEntry, time: i32) -> Option<&[PropertyEntry]> {
    let PropertyValue::Struct { fields, .. } = &channel.value else {
        return None;
    };
    fields.iter().find_map(|key| match &key.value {
        PropertyValue::Struct { fields, .. }
            if key.name == "Keys" && fields.first().is_some_and(|f| {
                f.name == "Time"
                    && matches!(f.value, PropertyValue::Int { value } if value == i64::from(time))
            }) =>
        {
            Some(fields.as_slice())
        }
        _ => None,
    })
}

/// The layout and decoded rows of the DataTable at `export`, for a row edit.
fn table_of(parsed: &ParsedPackage, export: u32) -> Result<(&DataTableLayout, &DataTable), String> {
    let table = parsed
        .exports
        .iter()
        .find(|candidate| candidate.index == export)
        .and_then(|candidate| candidate.data_table.as_ref())
        .ok_or_else(|| format!("export {export} is not a DataTable this reader decoded"))?;
    if table.truncated.is_some() {
        return Err(format!(
            "export {export} did not decode every row, so its rows cannot be edited"
        ));
    }
    let layout = parsed
        .tables
        .iter()
        .find(|layout| layout.export == export)
        .ok_or_else(|| format!("export {export} has no row positions recorded"))?;
    Ok((layout, table))
}

fn row_index(table: &DataTable, name: &str) -> Result<usize, String> {
    table
        .rows
        .iter()
        .position(|row| row.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| format!("this table has no row named {name}"))
}

/// What a save's row edits do to each table, gathered before any splice is built so a new name is
/// checked against the rows that will still be there rather than the ones that were.
#[derive(Default)]
struct RowBook {
    /// Rows a `Remove` takes out, by export and position.
    removed: Vec<(u32, usize)>,
    /// Rows a `Rename` takes the name from.
    renamed: Vec<(u32, usize)>,
    /// Names the save introduces, so two edits cannot introduce the same one.
    taken: Vec<(u32, String)>,
}

impl RowBook {
    fn new(parsed: &ParsedPackage, edits: &[RowEdit]) -> Result<Self, String> {
        let mut book = Self::default();
        for edit in edits {
            let (_, table) = table_of(parsed, edit.export)?;
            match &edit.op {
                RowOp::Remove { name } => {
                    let index = row_index(table, name)?;
                    if book.removed.contains(&(edit.export, index)) {
                        return Err(format!("row {name} is removed twice"));
                    }
                    book.removed.push((edit.export, index));
                }
                RowOp::Rename { name, .. } => {
                    let index = row_index(table, name)?;
                    if book.renamed.contains(&(edit.export, index)) {
                        return Err(format!("row {name} is renamed twice"));
                    }
                    book.renamed.push((edit.export, index));
                }
                RowOp::Add { .. } | RowOp::Duplicate { .. } => {}
            }
        }
        if let Some((export, index)) = book
            .removed
            .iter()
            .find(|removed| book.renamed.contains(removed))
        {
            let (_, table) = table_of(parsed, *export)?;
            let name = table.rows.get(*index).map_or("", |row| row.name.as_str());
            return Err(format!("row {name} cannot be both removed and renamed"));
        }
        Ok(book)
    }

    /// Refuses `name` when the table will already hold it once the save lands.
    fn claim(&mut self, table: &DataTable, export: u32, name: &str) -> Result<(), String> {
        if name.trim().is_empty() {
            return Err("a row needs a name".into());
        }
        let survives = table.rows.iter().enumerate().any(|(index, row)| {
            row.name.eq_ignore_ascii_case(name)
                && !self.removed.contains(&(export, index))
                && !self.renamed.contains(&(export, index))
        });
        let introduced = self
            .taken
            .iter()
            .any(|(taken_in, taken)| *taken_in == export && taken.eq_ignore_ascii_case(name));
        if survives || introduced {
            return Err(format!("this table already has a row named {name}"));
        }
        self.taken.push((export, name.to_string()));
        Ok(())
    }
}

/// One splice per row edit, in edit order. Positions are the table's as it was read, so several
/// edits can address one table without seeing each other's bytes; the count adjustment is the
/// caller's, summed over the table.
fn row_splices(
    parsed: &ParsedPackage,
    bundle: &AssetBundle<'_>,
    base: u64,
    names: &mut FPackageNameMap,
    edits: &PackageEdits,
) -> Result<Vec<(Splice, AppliedEdit)>, String> {
    let mut book = RowBook::new(parsed, &edits.rows)?;
    let mut out = Vec::with_capacity(edits.rows.len());
    for edit in &edits.rows {
        let (layout, table) = table_of(parsed, edit.export)?;
        let span_of = |index: usize| -> Result<RowSpan, String> {
            layout.rows.get(index).copied().ok_or_else(|| {
                format!(
                    "export {} has no position recorded for row {index}",
                    edit.export
                )
            })
        };
        let after_last = layout
            .rows
            .last()
            .map_or(layout.count_at + 4, |span| span.end);
        let insertion_point = |at: Option<u32>| -> Result<u64, String> {
            match at {
                None => Ok(after_last),
                Some(at) if at as usize == layout.rows.len() => Ok(after_last),
                Some(at) => span_of(at as usize).map(|span| span.start),
            }
        };
        let record = |name: String, offset: u64, before: String, after: String| AppliedEdit {
            name,
            offset,
            offset_after: offset,
            element: None,
            elements_after: None,
            before,
            after,
        };
        out.push(match &edit.op {
            RowOp::Add { name, at } => {
                book.claim(table, edit.export, name)?;
                let at = insertion_point(*at)?;
                let mut bytes = encode_name(name, names);
                if layout.tagged {
                    bytes.extend(encode_name("None", names));
                } else {
                    bytes.extend(unversioned::empty_header(layout.row_slots));
                }
                (
                    Splice {
                        start: at,
                        end: at,
                        bytes,
                    },
                    record(format!("row {name}"), at, "(none)".into(), "(added)".into()),
                )
            }
            RowOp::Duplicate { source, name, at } => {
                let from = span_of(row_index(table, source)?)?;
                book.claim(table, edit.export, name)?;
                let at = insertion_point(*at)?;
                let mut bytes = encode_name(name, names);
                bytes.extend_from_slice(bytes_at(bundle, base, from.start + 8, from.end)?);
                (
                    Splice {
                        start: at,
                        end: at,
                        bytes,
                    },
                    record(
                        format!("row {name}"),
                        at,
                        format!("copy of {source}"),
                        "(added)".into(),
                    ),
                )
            }
            RowOp::Remove { name } => {
                let span = span_of(row_index(table, name)?)?;
                // A value the row stores at its very end has no bytes and sits on the boundary,
                // which is why the boundary itself counts as inside.
                if edits
                    .values
                    .iter()
                    .any(|value| value.offset > span.start && value.offset <= span.end)
                {
                    return Err(format!(
                        "row {name} is being removed, so its values cannot be edited in the same save"
                    ));
                }
                (
                    Splice {
                        start: span.start,
                        end: span.end,
                        bytes: Vec::new(),
                    },
                    record(
                        format!("row {name}"),
                        span.start,
                        "(row)".into(),
                        "(removed)".into(),
                    ),
                )
            }
            RowOp::Rename { name, to } => {
                let span = span_of(row_index(table, name)?)?;
                book.claim(table, edit.export, to)?;
                (
                    Splice {
                        start: span.start,
                        end: span.start + 8,
                        bytes: encode_name(to, names),
                    },
                    record(format!("row {name}"), span.start, name.clone(), to.clone()),
                )
            }
        });
    }
    Ok(out)
}

fn element_value(container: &PropertyValue, index: u32) -> Option<&PropertyValue> {
    match container {
        PropertyValue::Array { items } | PropertyValue::Set { items } => items.get(index as usize),
        PropertyValue::Map { entries } => entries.get(index as usize).map(|pair| &pair.value),
        _ => None,
    }
}

/// Marks the slot as stored or as zero. A slot the header does not carry yet is inserted, which is
/// how an inherited value gets one of its own.
fn mark(
    blocks: &mut BTreeMap<u64, (UnversionedHeader, usize)>,
    bundle: &AssetBundle<'_>,
    base: u64,
    entry: &PropertyEntry,
    store: bool,
) -> Result<(), String> {
    with_block(blocks, bundle, base, entry, |header, slot| {
        match (store, header.has(slot)) {
            (true, true) => header.store(slot),
            (true, false) => header.insert_value(slot, false),
            (false, true) => header.clear(slot),
            (false, false) => header.insert_value(slot, true),
        }
    })
}

/// The minimal stored form of an unset slot: what the reader recorded for it, or for the kinds that
/// spell their default with a name, the name map's `None`.
fn stored_default(
    parsed: &ParsedPackage,
    entry: &PropertyEntry,
    names: &mut FPackageNameMap,
) -> Result<Vec<u8>, String> {
    let slot = entry
        .slot
        .ok_or_else(|| format!("{} has no header slot to store into", entry.label()))?;
    let unset = parsed
        .unset
        .iter()
        .find(|unset| unset.header_at == slot.header_at && unset.schema_index == slot.schema_index)
        .ok_or_else(|| {
            format!(
                "{} is not a slot this reader recorded as unset",
                entry.label()
            )
        })?;
    if matches!(entry.value, PropertyValue::Default { .. })
        && let Some(bytes) = &unset.zero_bytes
    {
        return Ok(bytes.clone());
    }
    if let Some(bytes) = &unset.default_bytes {
        return Ok(bytes.clone());
    }
    if let Some(parts) = &unset.default_recipe {
        return realise_default(parts, names);
    }
    let none = |names: &mut FPackageNameMap| encode_name("None", names);
    Ok(match (unset.declared, unset.struct_name.as_deref()) {
        ("Name", _) => none(names),
        ("SoftObject" | "AssetObject", _) => encode_soft_object("", names),
        ("Struct", Some("SoftObjectPath" | "SoftClassPath")) => encode_soft_object("", names),
        ("Struct", Some("TopLevelAssetPath")) => {
            let mut out = none(names);
            out.extend_from_slice(&none(names));
            out
        }
        ("Delegate", _) => {
            let mut out = vec![0u8; 4];
            out.extend_from_slice(&none(names));
            out
        }
        ("Struct", Some(name)) => {
            return Err(format!(
                "{} is a {name}, whose layout this editor cannot write from nothing",
                entry.label()
            ));
        }
        (declared, _) => {
            return Err(format!(
                "{} is a {declared}; type a value for it instead",
                entry.label()
            ));
        }
    })
}

/// Reads the header of the block holding `entry`, once per block, and hands it to `change`.
fn with_block(
    blocks: &mut BTreeMap<u64, (UnversionedHeader, usize)>,
    bundle: &AssetBundle<'_>,
    base: u64,
    entry: &PropertyEntry,
    change: impl FnOnce(&mut UnversionedHeader, u32) -> Result<(), String>,
) -> Result<(), String> {
    let slot = entry.slot.ok_or_else(|| {
        format!(
            "{} does not come from an unversioned property block, so its header cannot be changed",
            entry.label()
        )
    })?;
    let (header, _) = match blocks.entry(slot.header_at) {
        Entry::Occupied(held) => held.into_mut(),
        Entry::Vacant(empty) => {
            let (data, at) = buffer_at(bundle, base, slot.header_at)?;
            let mut cursor = Cursor::new(
                data.get(at..)
                    .ok_or("the property block header lies outside the package")?,
                slot.header_at,
            );
            let header = unversioned::read_header(&mut cursor)?;
            let length = cursor.position();
            empty.insert((header, length))
        }
    };
    change(header, slot.schema_index)
}

/// The bytes between two offsets, which is what an encoder rebuilds an FText's namespace and key
/// from and what an insertion copies.
fn bytes_at<'a>(
    bundle: &AssetBundle<'a>,
    base: u64,
    from: u64,
    to: u64,
) -> Result<&'a [u8], String> {
    let (data, at) = buffer_at(bundle, base, from)?;
    let width = usize::try_from(to.saturating_sub(from)).map_err(|_| "implausible width")?;
    data.get(at..at + width)
        .ok_or_else(|| "this value lies outside the package".to_string())
}

/// Export data lives in the `.uexp`, addressed from the end of the `.uasset` header.
fn buffer_at<'a>(
    bundle: &AssetBundle<'a>,
    base: u64,
    at: u64,
) -> Result<(&'a [u8], usize), String> {
    let (buffer, offset) = if at >= base {
        (bundle.exports, at - base)
    } else {
        (bundle.asset, at)
    };
    let offset =
        usize::try_from(offset).map_err(|_| "offset does not fit in memory".to_string())?;
    Ok((buffer, offset))
}

/// Confirms a patched package still reads as the one that was edited, with only the edited values
/// different. A patch that fails this is discarded rather than written.
pub fn verify_patch(
    before: &ParsedPackage,
    after: &ParsedPackage,
    edits: &PackageEdits,
    applied: &[AppliedEdit],
) -> Result<(), String> {
    let removed = if edits.remove_exports.is_empty() {
        Vec::new()
    } else {
        plan_removal(before, &edits.remove_exports)?.indices()
    };
    if !edits.exports.is_empty() {
        verify_export_edits(before, after, edits)?;
        return Ok(());
    }
    if !edits.dependencies.is_empty() {
        verify_dependency_edits(before, after, edits)?;
        return Ok(());
    }
    let dropped_imports: Vec<i32> = edits
        .imports
        .iter()
        .filter_map(|edit| match edit {
            ImportEdit::Remove { import } => Some(FPackageIndex::create_import(*import).index),
            _ => None,
        })
        .collect();
    if !dropped_imports.is_empty() {
        verify_import_removal(before, after, &dropped_imports)?;
    }
    let kept: Vec<&ParsedExport> = before
        .exports
        .iter()
        .filter(|export| !removed.contains(&export.index))
        .collect();
    let plans = if edits.duplicate_exports.is_empty() {
        Vec::new()
    } else {
        crate::plan_duplication(before, &edits.duplicate_exports)?
    };
    let copies: usize = plans.iter().map(|plan| plan.members.len()).sum();
    if kept.len() + copies != after.exports.len() {
        return Err(format!(
            "the patched package has {} exports where {} were expected",
            after.exports.len(),
            kept.len() + copies
        ));
    }
    for plan in &plans {
        let remap: std::collections::BTreeMap<i32, i32> = plan
            .members
            .iter()
            .zip(&plan.copies)
            .map(|(&member, &copy)| (member as i32 + 1, copy as i32 + 1))
            .collect();
        for (&member, &copy) in plan.members.iter().zip(&plan.copies) {
            let source = &before.exports[member as usize];
            let made = after
                .exports
                .get(copy as usize)
                .ok_or_else(|| format!("the copy of {} did not read back", source.path))?;
            if made.class_name != source.class_name || status_name(made) != status_name(source) {
                return Err(format!(
                    "the copy of {} reads as a {} ({}), not a {} ({})",
                    source.path,
                    made.class_name,
                    status_name(made),
                    source.class_name,
                    status_name(source)
                ));
            }
            let wanted_name = if member == plan.root {
                plan.name.as_str()
            } else {
                source.object_name.as_str()
            };
            if !made.object_name.eq_ignore_ascii_case(wanted_name) {
                return Err(format!(
                    "the copy of {} is named {}, not {wanted_name}",
                    source.path, made.object_name
                ));
            }
            let wanted_outer = remap
                .get(&source.outer_index)
                .copied()
                .unwrap_or(source.outer_index);
            if made.outer_index != wanted_outer {
                return Err(format!(
                    "the copy of {} sits under export index {}, not {wanted_outer}",
                    source.path, made.outer_index
                ));
            }
            same_copy(&source.properties, &made.properties, &remap)
                .map_err(|e| format!("the copy of {}: {e}", source.path))?;
        }
    }
    let excuses = Excuses {
        // A retargeted import changes the path of every value pointing at it, by design.
        retargeted: edits
            .imports
            .iter()
            .filter_map(|edit| match edit {
                ImportEdit::Retarget { import, .. } => {
                    Some(FPackageIndex::create_import(*import).index)
                }
                ImportEdit::Add { .. } | ImportEdit::Remove { .. } => None,
            })
            .collect(),
        // A reference to a removed export or import reads None afterwards, which is the point.
        removed: removed
            .iter()
            .map(|&index| FPackageIndex::create_export(index).index)
            .chain(dropped_imports.iter().copied())
            .collect(),
        removed_paths: removed
            .iter()
            .filter_map(|&index| before.exports.get(index as usize))
            .map(|export| export.path.clone())
            .collect(),
        repathed: Vec::new(),
    };
    for edit in &edits.imports {
        let (index, path) = match edit {
            ImportEdit::Retarget { import, path, .. } => {
                (Some(FPackageIndex::create_import(*import).index), path)
            }
            ImportEdit::Add { path, .. } => (None, path),
            // A removal is checked against the table it leaves behind, not by reading a path back.
            ImportEdit::Remove { .. } => continue,
        };
        let wanted = path.trim();
        let found = after
            .imports
            .iter()
            .any(|import| index.is_none_or(|index| import.index == index) && import.path == wanted);
        if !found {
            return Err(format!(
                "the import table does not read {wanted} back after patching"
            ));
        }
    }
    verify_keys(before, after, edits)?;
    verify_script_edits(before, after, edits)?;
    for edit in &edits.bulk {
        let index = edit.resource as usize;
        let (was, is) = match (before.resources.get(index), after.resources.get(index)) {
            (Some(was), Some(is)) => (was, is),
            _ => return Err(format!("bulk data resource {index} did not read back")),
        };
        if is.serial_size != edit.bytes.len() as i64 || is.raw_size != edit.bytes.len() as i64 {
            return Err(format!(
                "bulk data resource {index} reads {} bytes where {} were written",
                is.serial_size,
                edit.bytes.len()
            ));
        }
        if was.placement != is.placement || was.owner != is.owner {
            return Err(format!(
                "bulk data resource {index} moved from {} to {} after patching",
                was.placement, is.placement
            ));
        }
    }
    // A channel whose keys changed reads differently inside, as the edit asked; the comparison
    // skips it the way it skips an edited value.
    let skip: Vec<ValueEdit> = edits
        .values
        .iter()
        .cloned()
        .chain(edits.keys.iter().map(|edit| ValueEdit {
            offset: edit.offset,
            expect_name: edit.expect_name.clone(),
            expect_element: edit.expect_element,
            expect_kind: "struct".into(),
            op: EditOp::Clear,
        }))
        // A level listing a new actor reads one element longer, as the request asked. The list is
        // then checked on its own terms below, so skipping it here loses nothing.
        .chain(plans.iter().filter_map(|plan| {
            plan.level.as_ref().map(|slot| ValueEdit {
                offset: slot.count_at,
                expect_name: "Actors".into(),
                expect_element: None,
                expect_kind: "array".into(),
                op: EditOp::Insert {
                    index: slot.actors as u32,
                    key: None,
                },
            })
        }))
        .collect();
    verify_level_listing(before, after, &plans)?;
    for (old, new) in kept.iter().zip(&after.exports) {
        if status_name(old) != status_name(new) {
            return Err(format!(
                "export {} went from {} to {} after patching",
                old.index,
                status_name(old),
                status_name(new)
            ));
        }
        if let Some(edit) = edits.payloads.iter().find(|edit| edit.export == old.index) {
            match (&old.status, &new.status) {
                (
                    ExportStatus::Payload { consumed: was, .. },
                    ExportStatus::Payload {
                        consumed: is,
                        payload_bytes,
                        ..
                    },
                ) if was == is && *payload_bytes == edit.bytes.len() as u64 => {}
                _ => {
                    return Err(format!(
                        "{} does not carry the {} bytes written as its payload",
                        old.object_name,
                        edit.bytes.len()
                    ));
                }
            }
        }
        if edits.reset_exports.contains(&old.index) {
            if let Some(entry) = new
                .properties
                .iter()
                .find(|entry| !matches!(entry.value, PropertyValue::Unset { .. }))
            {
                return Err(format!(
                    "{} still stores {} after being reset",
                    old.object_name,
                    entry.label()
                ));
            }
            continue;
        }
        same_entries(&old.properties, &new.properties, &skip, &excuses, old.index)?;
        match (&old.data_table, &new.data_table) {
            (None, None) => {}
            (Some(was), Some(is)) => verify_rows(old.index, was, is, edits, &excuses)?,
            _ => {
                return Err(format!(
                    "export {} stopped decoding as a DataTable",
                    old.index
                ));
            }
        }
        match (&old.string_table, &new.string_table) {
            (None, None) => {}
            (Some(was), Some(is)) => verify_strings(old.index, was, is, edits)?,
            _ => {
                return Err(format!(
                    "export {} stopped decoding as a StringTable",
                    old.index
                ));
            }
        }
    }

    for (edit, done) in edits.values.iter().zip(applied) {
        let entry = find_at(
            after,
            done.offset_after,
            &edit.expect_name,
            edit.expect_element,
        )
        .ok_or_else(|| {
            format!(
                "{} is no longer at {:#X} after patching",
                edit.expect_name, done.offset_after
            )
        })?;
        match &edit.op {
            EditOp::Set { text } => {
                if !reads_back_as(&entry.value, text) {
                    return Err(format!(
                        "{} reads back as {} rather than {text}",
                        edit.expect_name,
                        entry.value.summary()
                    ));
                }
            }
            EditOp::Clear => {
                if entry.span.is_some_and(|(start, end)| end > start)
                    || matches!(entry.value, PropertyValue::Unset { .. })
                {
                    return Err(format!(
                        "{} is not zero after being cleared",
                        edit.expect_name
                    ));
                }
            }
            EditOp::Store => {
                if matches!(entry.value, PropertyValue::Unset { .. }) {
                    return Err(format!(
                        "{} is still unset after being stored",
                        edit.expect_name
                    ));
                }
            }
            EditOp::Unset => {
                if !matches!(entry.value, PropertyValue::Unset { .. }) {
                    return Err(format!(
                        "{} is still stored after being unset",
                        edit.expect_name
                    ));
                }
            }
            EditOp::SetElement { index, text } => {
                let keyed = matches!(
                    entry.value,
                    PropertyValue::Set { .. } | PropertyValue::Map { .. }
                );
                let now = index_after(edits, edit.offset, *index, keyed) as u32;
                let element = element_value(&entry.value, now).ok_or_else(|| {
                    format!("{} lost element {index} after patching", edit.expect_name)
                })?;
                if !reads_back_as(element, text) {
                    return Err(format!(
                        "{}[{index}] reads back as {} rather than {text}",
                        edit.expect_name,
                        element.summary()
                    ));
                }
            }
            EditOp::Insert { .. } | EditOp::Remove { .. } => {
                check_count(&entry.value, done, &edit.expect_name)?;
            }
        }
    }
    Ok(())
}

/// An add or a drop is only right if the container really holds the number of elements the edit
/// said it would, and the element count is written separately from the elements themselves.
fn check_count(value: &PropertyValue, done: &AppliedEdit, name: &str) -> Result<(), String> {
    let now = match value {
        PropertyValue::Array { items } | PropertyValue::Set { items } => items.len(),
        PropertyValue::Map { entries } => entries.len(),
        other => {
            return Err(format!(
                "{name} is a {} and holds no elements",
                kind_of(other)
            ));
        }
    };
    let want = done
        .elements_after
        .ok_or_else(|| format!("{name} did not record how many elements to expect"))?;
    if now != want {
        return Err(format!(
            "{name} holds {now} elements, and {want} was expected"
        ));
    }
    Ok(())
}

/// Differences a structural edit accounts for, which the comparison lets through.
#[derive(Default)]
struct Excuses {
    /// Imports pointed elsewhere: every value holding one of these indices reads a new path.
    retargeted: Vec<i32>,
    /// Exports that went: a value holding one of these indices reads None.
    removed: Vec<i32>,
    /// Their paths, for the values that render an object by path rather than by index.
    removed_paths: Vec<String>,
    /// Objects that kept their index but read at a new path, from a rename or a reparent.
    repathed: Vec<(String, String)>,
}

impl Excuses {
    fn names_removed(&self, struct_name: &str) -> bool {
        self.removed_paths
            .iter()
            .any(|path| path.rsplit(['.', ':']).next() == Some(struct_name))
    }

    /// Whether an object that read at `was` is expected to read at `is` now.
    fn moved(&self, was: &str, is: &str) -> bool {
        self.repathed
            .iter()
            .any(|(from, to)| from == was && to == is)
    }
}

/// Refuses any difference between two decoded trees that no edit accounts for. An edited entry may
/// differ throughout its subtree: a container gains or loses an element, and a struct above an
/// edited field renders differently because of it, which is why ancestors are not compared as a
/// whole. An edit excuses only the entry at the offset it named: rows of a table share column
/// names, and the other rows must still read exactly as they did.
/// Replays the string edits over the table as it was read and holds the patched table to the
/// result: every entry in order with the changed keys and strings, less the removed, plus the
/// added ones at the end, and the namespace as it was.
fn verify_strings(
    export: u32,
    was: &StringTable,
    is: &StringTable,
    edits: &PackageEdits,
) -> Result<(), String> {
    if was.namespace != is.namespace {
        return Err(format!(
            "export {export} now has namespace {} where it had {}",
            is.namespace, was.namespace
        ));
    }
    let mut expected: Vec<Option<crate::stringtable::StringTableEntry>> =
        was.entries.iter().cloned().map(Some).collect();
    let mut added = Vec::new();
    let slots = expected.len();
    for edit in edits.strings.iter().filter(|edit| edit.export == export) {
        let slot = |index: u32| -> Result<usize, String> {
            let position = index as usize;
            (position < slots)
                .then_some(position)
                .ok_or_else(|| format!("export {export} has no entry {index}"))
        };
        match &edit.op {
            StringOp::SetKey { index, to, .. } => {
                if let Some(entry) = &mut expected[slot(*index)?] {
                    entry.key = to.clone();
                }
            }
            StringOp::SetSource { index, to, .. } => {
                if let Some(entry) = &mut expected[slot(*index)?] {
                    entry.source = to.clone();
                }
            }
            StringOp::Add { key, source } => added.push(crate::stringtable::StringTableEntry {
                key: key.clone(),
                source: source.clone(),
                tag: String::new(),
                metadata: Vec::new(),
            }),
            StringOp::Remove { index, .. } => expected[slot(*index)?] = None,
            StringOp::SetTag { index, to, .. } => {
                if let Some(entry) = &mut expected[slot(*index)?] {
                    entry.tag = to.clone();
                }
            }
            StringOp::SetMetaData { index, id, to, .. } => {
                if let Some(entry) = &mut expected[slot(*index)?] {
                    match entry.metadata.iter_mut().find(|(held, _)| held == id) {
                        Some(pair) => pair.1 = to.clone(),
                        None => entry.metadata.push((id.clone(), to.clone())),
                    }
                }
            }
            StringOp::RemoveMetaData { index, id, .. } => {
                if let Some(entry) = &mut expected[slot(*index)?] {
                    entry.metadata.retain(|(held, _)| held != id);
                }
            }
        }
    }
    let expected: Vec<_> = expected.into_iter().flatten().chain(added).collect();
    if expected.len() != is.entries.len() {
        return Err(format!(
            "export {export} was expected to hold {} entries but decoded {}",
            expected.len(),
            is.entries.len()
        ));
    }
    for (want, got) in expected.iter().zip(&is.entries) {
        if want.key != got.key
            || want.source != got.source
            || want.tag != got.tag
            || want.metadata != got.metadata
        {
            return Err(format!(
                "export {export} now reads entry {} as {:?}, where {:?} was expected",
                got.key, got.source, want.source
            ));
        }
    }
    Ok(())
}

/// What a row of the patched table has to hold.
enum Expect<'a> {
    Kept(&'a DataTableRow),
    Copy(&'a DataTableRow),
    Empty,
}

/// Replays the row edits over the table as it was read and holds the patched table to the result:
/// the rows that were there in their order, less the removed, with the added ones where they were
/// put and renamed rows under their new names. A kept row compares value by value, a duplicate has
/// to equal its source and an added row has to store nothing.
fn verify_rows(
    export: u32,
    was: &DataTable,
    is: &DataTable,
    edits: &PackageEdits,
    excuses: &Excuses,
) -> Result<(), String> {
    let rows = was.rows.len();
    let mut inserted: Vec<Vec<(String, Expect<'_>)>> = (0..=rows).map(|_| Vec::new()).collect();
    let mut removed = vec![false; rows];
    let mut renamed: Vec<Option<&str>> = vec![None; rows];
    let slot = |at: Option<u32>| at.map_or(rows, |at| at as usize).min(rows);
    for edit in edits.rows.iter().filter(|edit| edit.export == export) {
        match &edit.op {
            RowOp::Add { name, at } => inserted[slot(*at)].push((name.clone(), Expect::Empty)),
            RowOp::Duplicate { source, name, at } => {
                let source = &was.rows[row_index(was, source)?];
                inserted[slot(*at)].push((name.clone(), Expect::Copy(source)));
            }
            RowOp::Remove { name } => removed[row_index(was, name)?] = true,
            RowOp::Rename { name, to } => renamed[row_index(was, name)?] = Some(to),
        }
    }
    let mut expected: Vec<(String, Expect<'_>)> = Vec::with_capacity(is.rows.len());
    for (index, row) in was.rows.iter().enumerate() {
        expected.append(&mut inserted[index]);
        if !removed[index] {
            let name = renamed[index].map_or_else(|| row.name.clone(), str::to_string);
            expected.push((name, Expect::Kept(row)));
        }
    }
    expected.append(&mut inserted[rows]);
    if expected.len() != is.rows.len() || is.declared_rows as usize != is.rows.len() {
        return Err(format!(
            "export {export} was expected to hold {} rows but decoded {} of {} declared",
            expected.len(),
            is.rows.len(),
            is.declared_rows
        ));
    }
    for ((name, expect), row) in expected.iter().zip(&is.rows) {
        if !row.name.eq_ignore_ascii_case(name) {
            return Err(format!(
                "export {export} now has row {} where {name} was expected",
                row.name
            ));
        }
        match expect {
            Expect::Kept(before) => {
                same_entries(&before.fields, &row.fields, &edits.values, excuses, export)?
            }
            Expect::Copy(source) => {
                same_entries(&source.fields, &row.fields, &[], excuses, export)?
            }
            Expect::Empty => {
                if let Some(field) = row
                    .fields
                    .iter()
                    .find(|field| !matches!(field.value, PropertyValue::Unset { .. }))
                {
                    return Err(format!(
                        "export {export}: the added row {name} stores {}",
                        field.label()
                    ));
                }
            }
        }
    }
    Ok(())
}

fn same_entries(
    before: &[PropertyEntry],
    after: &[PropertyEntry],
    edits: &[ValueEdit],
    excuses: &Excuses,
    export: u32,
) -> Result<(), String> {
    if before.len() != after.len() {
        return Err(format!(
            "export {export} decoded {} values before the edit and {} after",
            before.len(),
            after.len()
        ));
    }
    for (old, new) in before.iter().zip(after) {
        if old.name != new.name || old.element != new.element {
            return Err(format!(
                "export {export} now reads {} where it read {}",
                new.label(),
                old.label()
            ));
        }
        if edits.iter().any(|edit| {
            edit.expect_name == old.name
                && edit.expect_element == old.element
                && old.span.is_some_and(|(start, _)| start == edit.offset)
        }) {
            continue;
        }
        if let Some((from, to)) = first_difference(&old.value, &new.value, edits, excuses, export)?
        {
            return Err(format!(
                "{} changed from {from} to {to}, and nothing asked it to",
                old.label()
            ));
        }
    }
    Ok(())
}

/// A copy has to read as its source with every reference into the copied set pointing at the
/// copies: the same entries in the same order, equal except for those object indices.
fn same_copy(
    before: &[PropertyEntry],
    after: &[PropertyEntry],
    remap: &std::collections::BTreeMap<i32, i32>,
) -> Result<(), String> {
    if before.len() != after.len() {
        return Err(format!(
            "decodes {} values where the source has {}",
            after.len(),
            before.len()
        ));
    }
    for (old, new) in before.iter().zip(after) {
        if old.name != new.name || old.element != new.element {
            return Err(format!(
                "reads {} where the source reads {}",
                new.label(),
                old.label()
            ));
        }
        same_copy_value(&old.value, &new.value, remap)
            .map_err(|e| format!("{}: {e}", old.label()))?;
    }
    Ok(())
}

fn same_copy_value(
    was: &PropertyValue,
    is: &PropertyValue,
    remap: &std::collections::BTreeMap<i32, i32>,
) -> Result<(), String> {
    match (was, is) {
        (PropertyValue::Object { index: a, .. }, PropertyValue::Object { index: b, .. }) => {
            let wanted = remap.get(a).copied().unwrap_or(*a);
            if wanted != *b {
                return Err(format!(
                    "points at export index {b} where {wanted} was expected"
                ));
            }
            Ok(())
        }
        (PropertyValue::Struct { fields: a, .. }, PropertyValue::Struct { fields: b, .. }) => {
            same_copy(a, b, remap)
        }
        (
            PropertyValue::Array { items: a } | PropertyValue::Set { items: a },
            PropertyValue::Array { items: b } | PropertyValue::Set { items: b },
        ) => {
            if a.len() != b.len() {
                return Err(format!(
                    "holds {} elements where the source holds {}",
                    b.len(),
                    a.len()
                ));
            }
            for (x, y) in a.iter().zip(b) {
                same_copy_value(x, y, remap)?;
            }
            Ok(())
        }
        (PropertyValue::Map { entries: a }, PropertyValue::Map { entries: b }) => {
            if a.len() != b.len() {
                return Err(format!(
                    "holds {} pairs where the source holds {}",
                    b.len(),
                    a.len()
                ));
            }
            for (x, y) in a.iter().zip(b) {
                same_copy_value(&x.key, &y.key, remap)?;
                same_copy_value(&x.value, &y.value, remap)?;
            }
            Ok(())
        }
        _ => {
            if was.summary() != is.summary() {
                return Err(format!(
                    "reads {} where the source reads {}",
                    is.summary(),
                    was.summary()
                ));
            }
            Ok(())
        }
    }
}

/// Where two values first read differently, looking inside containers, or `None` when they agree.
/// Struct fields are entries in their own right wherever they sit, so an edit may name one inside
/// a container element; that is why structs go back through [`same_entries`].
fn first_difference(
    was: &PropertyValue,
    is: &PropertyValue,
    edits: &[ValueEdit],
    excuses: &Excuses,
    export: u32,
) -> Result<Option<(String, String)>, String> {
    match (was, is) {
        // The reason names the offset it stopped at, which an earlier edit in the same export
        // moves. The payload is untouched either way, so its length is what identifies it.
        (PropertyValue::Undecoded { bytes, .. }, PropertyValue::Undecoded { bytes: still, .. }) => {
            return Ok((bytes != still).then(|| (was.summary(), is.summary())));
        }
        (PropertyValue::Object { index, .. }, PropertyValue::Object { .. })
            if excuses.retargeted.contains(index) =>
        {
            return Ok(None);
        }
        (PropertyValue::Object { index, .. }, PropertyValue::Object { index: 0, .. })
            if excuses.removed.contains(index) =>
        {
            return Ok(None);
        }
        (
            PropertyValue::Object {
                index,
                path: Some(was),
            },
            PropertyValue::Object {
                index: still,
                path: Some(is),
            },
        ) if index == still && excuses.moved(was, is) => {
            return Ok(None);
        }
        (
            PropertyValue::Delegate {
                object: Some(was),
                function,
            },
            PropertyValue::Delegate {
                object: Some(is),
                function: still,
            },
        ) if function == still && excuses.moved(was, is) => {
            return Ok(None);
        }
        (
            PropertyValue::Delegate {
                object: Some(object),
                function,
            },
            PropertyValue::Delegate {
                object: None,
                function: still,
            },
        ) if function == still && excuses.removed_paths.contains(object) => {
            return Ok(None);
        }
        // An instanced struct whose type export went reads as nothing at all.
        (PropertyValue::Struct { name, .. }, PropertyValue::Default { .. })
            if excuses.names_removed(name) =>
        {
            return Ok(None);
        }
        _ => {}
    }
    if std::mem::discriminant(was) != std::mem::discriminant(is) {
        return Ok(Some((was.summary(), is.summary())));
    }
    match (was, is) {
        (PropertyValue::Struct { fields: a, .. }, PropertyValue::Struct { fields: b, .. }) => {
            same_entries(a, b, edits, excuses, export)?;
            Ok(None)
        }
        (
            PropertyValue::Array { items: a } | PropertyValue::Set { items: a },
            PropertyValue::Array { items: b } | PropertyValue::Set { items: b },
        ) => {
            if a.len() != b.len() {
                return Ok(Some((was.summary(), is.summary())));
            }
            for (x, y) in a.iter().zip(b) {
                if let Some(found) = first_difference(x, y, edits, excuses, export)? {
                    return Ok(Some(found));
                }
            }
            Ok(None)
        }
        (PropertyValue::Map { entries: a }, PropertyValue::Map { entries: b }) => {
            if a.len() != b.len() {
                return Ok(Some((was.summary(), is.summary())));
            }
            for (x, y) in a.iter().zip(b) {
                if let Some(found) = first_difference(&x.key, &y.key, edits, excuses, export)? {
                    return Ok(Some(found));
                }
                if let Some(found) = first_difference(&x.value, &y.value, edits, excuses, export)? {
                    return Ok(Some(found));
                }
            }
            Ok(None)
        }
        // A text built from parts shows what its parts produce, so the parts are what to compare.
        (PropertyValue::Text { parts: a, .. }, PropertyValue::Text { parts: b, .. })
            if !a.is_empty() || !b.is_empty() =>
        {
            same_entries(a, b, edits, excuses, export)?;
            Ok(None)
        }
        _ => {
            let (from, to) = (was.summary(), is.summary());
            Ok((from != to).then_some((from, to)))
        }
    }
}

fn status_name(export: &ParsedExport) -> &'static str {
    match export.status {
        crate::package::ExportStatus::Complete => "complete",
        crate::package::ExportStatus::Payload { .. } => "payload",
        crate::package::ExportStatus::Partial { .. } => "partial",
        crate::package::ExportStatus::Failed { .. } => "failed",
    }
}

/// The tag the frontend sees on a value, so a stale request can be told from a fresh one.
pub fn kind_of(value: &PropertyValue) -> String {
    match value {
        PropertyValue::Bool { .. } => "bool",
        PropertyValue::Int { .. } => "int",
        PropertyValue::UInt { .. } => "uint",
        PropertyValue::Float { .. } => "float",
        PropertyValue::Byte { .. } => "byte",
        PropertyValue::Str { .. } => "str",
        PropertyValue::Name { .. } => "name",
        PropertyValue::Text { .. } => "text",
        PropertyValue::Enum { .. } => "enum",
        PropertyValue::Object { .. } => "object",
        PropertyValue::SoftObject { .. } => "soft_object",
        PropertyValue::Delegate { .. } => "delegate",
        PropertyValue::FieldPath { .. } => "field_path",
        PropertyValue::LazyObject { .. } => "lazy_object",
        PropertyValue::Array { .. } => "array",
        PropertyValue::Set { .. } => "set",
        PropertyValue::Map { .. } => "map",
        PropertyValue::Struct { .. } => "struct",
        PropertyValue::Undecoded { .. } => "undecoded",
        PropertyValue::Default { .. } => "default",
        PropertyValue::Unset { .. } => "unset",
    }
    .to_string()
}

/// How wide a property of this declared type is when stored, for values that have no bytes yet and
/// so cannot be measured.
fn declared_width(declared: &str) -> Option<u64> {
    Some(match declared {
        "Bool" | "Byte" | "Int8" => 1,
        "Int16" | "UInt16" => 2,
        "Int" | "UInt32" | "Float" => 4,
        "Int64" | "UInt64" | "Double" => 8,
        _ => return None,
    })
}

/// What an encoder needs beyond the text: the package's own tables, and the bytes the value holds
/// today, which is the only source for the parts of an FText the decoded value throws away.
struct Target<'a> {
    declared: &'a str,
    /// Set when the value is a native struct written unlike the kind it decodes as.
    native: Option<crate::props::NativeLeaf>,
    /// `None` when the value holds its default and so has no bytes to measure.
    width: Option<u64>,
    was: &'a [u8],
    tables: &'a mut Tables,
    package: &'a retoc::legacy_asset::FLegacyPackageHeader,
    /// A container element goes through the inner property's own serialization, which writes an
    /// enum as its enumerator name rather than as the underlying integer.
    element: bool,
    /// The mappings, for turning an enumerator name into its number and back.
    enums: Option<&'a Mappings>,
}

/// Produces the bytes for a value. Kinds that reach into the package's own tables are handled
/// here; everything with a self-contained encoding goes through [`encode_scalar`].
fn encode(value: &PropertyValue, text: &str, target: Target<'_>) -> Result<Vec<u8>, String> {
    if let Some(leaf) = target.native {
        return encode_native_leaf(leaf, text.trim(), &mut target.tables.names);
    }
    // A zero value has no bytes to rebuild from, so it is written from nothing like an unset one.
    if matches!(value, PropertyValue::Default { .. }) && !target.declared.is_empty() {
        let declared = target.declared;
        return encode_declared(declared, text, target);
    }
    if let PropertyValue::Unset {
        declared,
        enum_type,
        ..
    } = value
    {
        // An enum slot stores its underlying integer; a typed enumerator name becomes that first.
        let resolved;
        let text = if enum_type.is_some() && text.trim().parse::<i64>().is_err() {
            resolved =
                enumerator_value(target.enums, enum_type.as_deref(), text.trim())?.to_string();
            resolved.as_str()
        } else {
            text
        };
        return encode_declared(declared, text, target);
    }
    if let PropertyValue::Enum { enum_type, .. } = value {
        let text = text.trim();
        if target.element {
            let name = match text.parse::<i64>() {
                Ok(number) => enumerator_name(target.enums, enum_type.as_deref(), number)?,
                Err(_) => text.to_string(),
            };
            return Ok(encode_name(&name, &mut target.tables.names));
        }
        if text.parse::<i64>().is_err() {
            let number = enumerator_value(target.enums, enum_type.as_deref(), text)?;
            return encode_scalar(value, &number.to_string(), target.width, target.declared);
        }
    }
    match value {
        PropertyValue::Str { .. } => Ok(encode_string(text)),
        PropertyValue::Name { .. } => Ok(encode_name(text, &mut target.tables.names)),
        PropertyValue::SoftObject { .. } => Ok(encode_soft_object(text, &mut target.tables.names)),
        PropertyValue::Text { parts, .. } if !parts.is_empty() => {
            Err("this text is built from the parts below; edit one of them instead".to_string())
        }
        PropertyValue::Text { .. } => encode_text(text, target.was),
        PropertyValue::Object { index, .. } => {
            encode_object(text, target.package, target.tables, Some(*index))
        }
        _ => encode_scalar(value, text, target.width, target.declared),
    }
}

/// The number behind an enumerator name, through the mappings; refused by name when unknown.
fn enumerator_value(
    mappings: Option<&Mappings>,
    enum_type: Option<&str>,
    text: &str,
) -> Result<i64, String> {
    let enum_type = enum_type.ok_or_else(|| {
        format!("{text} is not a number, and the enum's type is not known here to look it up")
    })?;
    let mappings = mappings.ok_or_else(|| {
        format!("{text} cannot be looked up in {enum_type} without a mappings file")
    })?;
    mappings
        .enum_value(enum_type, text)
        .ok_or_else(|| format!("{text} is not an enumerator of {enum_type}"))
}

/// The enumerator a number stands for, for container elements written by name.
fn enumerator_name(
    mappings: Option<&Mappings>,
    enum_type: Option<&str>,
    number: i64,
) -> Result<String, String> {
    let enum_type = enum_type.ok_or_else(|| {
        format!("{number} needs the enum's type to be spelt as a name here; type the enumerator")
    })?;
    let mappings = mappings.ok_or_else(|| {
        format!("{number} cannot be looked up in {enum_type} without a mappings file")
    })?;
    mappings
        .enum_name(enum_type, number)
        .map(str::to_string)
        .ok_or_else(|| format!("{enum_type} has no enumerator {number}"))
}

/// A slot with no value has only its declared type to say how the text is written. An enum slot
/// declares its underlying integer, so it takes the number rather than the enumerator name.
fn encode_declared(declared: &str, text: &str, target: Target<'_>) -> Result<Vec<u8>, String> {
    // Flags and the None history, which is all a text made from nothing needs to rebuild from.
    const UNSET_TEXT: [u8; 5] = [0, 0, 0, 0, 0xFF];
    let scalar = |value: PropertyValue| encode_scalar(&value, text, None, declared);
    match declared {
        "Str" | "Utf8Str" | "AnsiStr" => Ok(encode_string(text)),
        "Name" => Ok(encode_name(text.trim(), &mut target.tables.names)),
        "SoftObject" | "AssetObject" => {
            Ok(encode_soft_object(text.trim(), &mut target.tables.names))
        }
        "Object" | "WeakObject" | "Interface" => {
            encode_object(text, target.package, target.tables, None)
        }
        "Text" => encode_text(text, &UNSET_TEXT),
        "Bool" => scalar(PropertyValue::Bool { value: false }),
        "Byte" => scalar(PropertyValue::Byte { value: 0 }),
        "Int8" | "Int16" | "Int" | "Int64" => scalar(PropertyValue::Int { value: 0 }),
        "UInt16" | "UInt32" | "UInt64" => scalar(PropertyValue::UInt { value: 0 }),
        "Float" | "Double" => scalar(PropertyValue::Float { value: 0.0 }),
        other => Err(format!(
            "a {other} cannot be typed in; store it first and then edit what is inside"
        )),
    }
}

/// Numbers are written at the width the asset already uses, or at the width the schema declares
/// when the value is not stored yet, so the size is never a guess.
fn encode_scalar(
    value: &PropertyValue,
    text: &str,
    width: Option<u64>,
    declared: &str,
) -> Result<Vec<u8>, String> {
    let text = text.trim();
    let width = width.or_else(|| declared_width(declared)).ok_or_else(|| {
        format!("nothing says how wide a {declared} is here, so it cannot be written")
    })?;
    let bytes = match value {
        PropertyValue::Bool { .. } => {
            let on = match text.to_ascii_lowercase().as_str() {
                "true" | "1" | "yes" | "on" => true,
                "false" | "0" | "no" | "off" => false,
                other => return Err(format!("{other} is not true or false")),
            };
            match width {
                1 => vec![u8::from(on)],
                4 => u32::from(on).to_le_bytes().to_vec(),
                other => return Err(format!("cannot write a {other}-byte boolean")),
            }
        }
        PropertyValue::Byte { .. } => vec![parse::<u8>(text)?],
        PropertyValue::Int { .. } | PropertyValue::Enum { .. } => match width {
            1 => parse::<i8>(text)?.to_le_bytes().to_vec(),
            2 => parse::<i16>(text)?.to_le_bytes().to_vec(),
            4 => parse::<i32>(text)?.to_le_bytes().to_vec(),
            8 => parse::<i64>(text)?.to_le_bytes().to_vec(),
            other => return Err(format!("cannot write a {other}-byte integer")),
        },
        PropertyValue::UInt { .. } => match width {
            1 => parse::<u8>(text)?.to_le_bytes().to_vec(),
            2 => parse::<u16>(text)?.to_le_bytes().to_vec(),
            4 => parse::<u32>(text)?.to_le_bytes().to_vec(),
            8 => parse::<u64>(text)?.to_le_bytes().to_vec(),
            other => return Err(format!("cannot write a {other}-byte integer")),
        },
        PropertyValue::Float { .. } => match width {
            4 => parse::<f32>(text)?.to_le_bytes().to_vec(),
            8 => parse::<f64>(text)?.to_le_bytes().to_vec(),
            other => return Err(format!("cannot write a {other}-byte float")),
        },
        other => return Err(format!("{} values cannot be edited yet", kind_of(other))),
    };
    if bytes.len() as u64 != width {
        return Err(format!(
            "{text} needs {width} bytes but encoded to {}",
            bytes.len()
        ));
    }
    Ok(bytes)
}

/// An FString is a count then the characters, including the terminator. A positive count means one
/// byte per character, a negative one means UTF-16, which is what anything above ASCII needs.
fn encode_string(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + 8);
    if text.is_empty() {
        out.extend_from_slice(&0i32.to_le_bytes());
        return out;
    }
    if text.is_ascii() {
        out.extend_from_slice(&(text.len() as i32 + 1).to_le_bytes());
        out.extend_from_slice(text.as_bytes());
        out.push(0);
        return out;
    }
    let units: Vec<u16> = text.encode_utf16().collect();
    out.extend_from_slice(&(-(units.len() as i32 + 1)).to_le_bytes());
    for unit in units {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// An FName is an index into the package name map and a number suffix. Storing one that is not
/// there yet appends it, which is what makes the header grow.
fn encode_name(text: &str, names: &mut FPackageNameMap) -> Vec<u8> {
    let stored = names.store(text);
    let mut out = Vec::with_capacity(8);
    out.extend_from_slice(&stored.index.to_le_bytes());
    out.extend_from_slice(&stored.number.to_le_bytes());
    out
}

/// A cooked soft object path is the package name, the asset name and a sub-path, rendered as
/// `/Game/Thing.Thing:Sub`. An empty part is written as `None`, which is how the reader sees it.
/// The bytes of a native struct that serializes itself, from the text its decoded value shows.
fn encode_native_leaf(
    leaf: crate::props::NativeLeaf,
    text: &str,
    names: &mut FPackageNameMap,
) -> Result<Vec<u8>, String> {
    use crate::props::NativeLeaf;
    match leaf {
        NativeLeaf::Guid => {
            let digits: String = text
                .chars()
                .filter(|c| !matches!(c, '-' | '{' | '}'))
                .collect();
            if digits.len() != 32 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(format!("{text} is not a guid: it takes 32 hex digits"));
            }
            let mut out = Vec::with_capacity(16);
            for part in 0..4 {
                let word = u32::from_str_radix(&digits[part * 8..part * 8 + 8], 16)
                    .map_err(|e| format!("{text} is not a guid: {e}"))?;
                out.extend_from_slice(&word.to_le_bytes());
            }
            Ok(out)
        }
        NativeLeaf::TopLevelAssetPath => {
            if text.contains(':') {
                return Err(format!(
                    "{text} names a subobject, and an asset path reaches only as far as the asset"
                ));
            }
            let (package, asset) = text.rsplit_once('.').unwrap_or((text, ""));
            let mut out = Vec::with_capacity(16);
            for part in [package, asset] {
                out.extend_from_slice(&encode_name(
                    if part.is_empty() { "None" } else { part },
                    names,
                ));
            }
            Ok(out)
        }
        NativeLeaf::MarvelSoftObjectPath => Ok(encode_string(text)),
    }
}

fn encode_soft_object(text: &str, names: &mut FPackageNameMap) -> Vec<u8> {
    let (head, sub_path) = match text.split_once(':') {
        Some((head, sub)) => (head, sub),
        None => (text, ""),
    };
    let (package, asset) = match head.rsplit_once('.') {
        Some((package, asset)) => (package, asset),
        None => (head, ""),
    };
    let mut out = Vec::with_capacity(24);
    out.extend_from_slice(&encode_name(
        if package.is_empty() { "None" } else { package },
        names,
    ));
    out.extend_from_slice(&encode_name(
        if asset.is_empty() { "None" } else { asset },
        names,
    ));
    out.extend_from_slice(&encode_string(sub_path));
    out
}

/// `PropertyValue::Text` keeps only the displayed string, so the flags, the history type and a
/// localized entry's namespace and key have to come back out of the bytes that are already there.
/// Histories this cannot reproduce are refused by name rather than written half-formed.
fn encode_text(text: &str, was: &[u8]) -> Result<Vec<u8>, String> {
    if was.len() < 5 {
        return Err("this text has no bytes to rebuild from, so it cannot be edited yet".into());
    }
    let flags = &was[..4];
    let history = was[4] as i8;
    let mut out = Vec::with_capacity(text.len() + 24);
    out.extend_from_slice(flags);
    out.push(history as u8);
    match history {
        // None, carrying an optional culture-invariant string.
        -1 => {
            out.extend_from_slice(&1u32.to_le_bytes());
            out.extend_from_slice(&encode_string(text));
        }
        // Base: namespace and key identify the entry and must survive untouched.
        0 => {
            let mut at = 5usize;
            let namespace = take_string(was, &mut at)?;
            let key = take_string(was, &mut at)?;
            out.extend_from_slice(namespace);
            out.extend_from_slice(key);
            out.extend_from_slice(&encode_string(text));
        }
        // AsNumber, AsPercent and AsCurrency: only the source value changes; the formatting
        // options and the culture are copied through.
        4..=6 => return encode_formatted_number(text, was, history),
        other => {
            return Err(format!(
                "text with history type {other} cannot be edited yet"
            ));
        }
    }
    Ok(out)
}

/// `FTextHistory_FormatNumber` holds the number it formats as a typed `FFormatArgumentValue`, so
/// the typed text is parsed in that argument's own type and written over the value alone. A
/// currency may be retyped as `CODE value`; a percent may keep its `as percent` suffix.
fn encode_formatted_number(text: &str, was: &[u8], history: i8) -> Result<Vec<u8>, String> {
    let mut out = was[..5].to_vec();
    let mut at = 5usize;
    let text = text.trim();
    let mut text = text.strip_suffix("as percent").map_or(text, str::trim_end);
    if history == 6 {
        let code = take_string(was, &mut at)?;
        match text.split_once(char::is_whitespace) {
            Some((first, rest)) if first.parse::<f64>().is_err() => {
                out.extend_from_slice(&encode_string(first));
                text = rest.trim_start();
            }
            _ => out.extend_from_slice(code),
        }
    }
    let kind = *was
        .get(at)
        .ok_or("this text ends before its source value")? as i8;
    out.push(kind as u8);
    at += 1;
    let (bytes, width) = match kind {
        0 => (
            parse_number::<i64>(text, "a whole number")?
                .to_le_bytes()
                .to_vec(),
            8,
        ),
        1 => (
            parse_number::<u64>(text, "a whole number of zero or more")?
                .to_le_bytes()
                .to_vec(),
            8,
        ),
        2 => (
            parse_number::<f32>(text, "a number")?
                .to_le_bytes()
                .to_vec(),
            4,
        ),
        3 => (
            parse_number::<f64>(text, "a number")?
                .to_le_bytes()
                .to_vec(),
            8,
        ),
        4 => {
            return Err(
                "this text formats another text, so no numeric source value can be typed for it"
                    .into(),
            );
        }
        5 => return Err(
            "this text formats a gender argument, so no numeric source value can be typed for it"
                .into(),
        ),
        other => return Err(format!("unknown FText format argument type {other}")),
    };
    out.extend_from_slice(&bytes);
    at += width;
    let rest = was
        .get(at..)
        .ok_or("this text ends before its formatting options")?;
    out.extend_from_slice(rest);
    Ok(out)
}

fn parse_number<T: std::str::FromStr>(text: &str, wanted: &str) -> Result<T, String> {
    text.parse::<T>()
        .map_err(|_| format!("{text} is not {wanted}"))
}

/// A formatted number displays with its currency code or `as percent` around the value, so the
/// value is compared on its own, as a number when both sides parse as one.
fn formatted_number_reads_back(shown: &str, typed: &str) -> bool {
    fn split(text: &str) -> (&str, &str) {
        let text = text.trim();
        let text = text.strip_suffix("as percent").map_or(text, str::trim_end);
        match text.rsplit_once(char::is_whitespace) {
            Some((code, value)) => (code.trim(), value),
            None => ("", text),
        }
    }
    let (shown_code, shown_value) = split(shown);
    let (typed_code, typed_value) = split(typed);
    let same_value = shown_value == typed_value
        || matches!(
            (shown_value.parse::<f64>(), typed_value.parse::<f64>()),
            (Ok(a), Ok(b)) if a == b
        );
    same_value && (typed_code.is_empty() || typed_code == shown_code)
}

/// The bytes of one FString starting at `at`, advancing past it.
fn take_string<'a>(data: &'a [u8], at: &mut usize) -> Result<&'a [u8], String> {
    let start = *at;
    let count = data
        .get(start..start + 4)
        .ok_or("this text ends before its strings do")?;
    let count = i32::from_le_bytes([count[0], count[1], count[2], count[3]]);
    let payload = if count < 0 {
        count.unsigned_abs() as usize * 2
    } else {
        count as usize
    };
    *at = start
        .checked_add(4 + payload)
        .ok_or("this text declares an implausible string length")?;
    data.get(start..*at)
        .ok_or_else(|| "this text ends before its strings do".to_string())
}

/// An object reference is an index into this package's import and export tables, so only something
/// the package already names can be pointed at. `None` is index zero.
fn encode_object(
    text: &str,
    package: &retoc::legacy_asset::FLegacyPackageHeader,
    tables: &mut Tables,
    current: Option<i32>,
) -> Result<Vec<u8>, String> {
    let text = text.trim();
    let index = if text.is_empty() || text.eq_ignore_ascii_case("none") {
        0
    } else if let Ok(raw) = text.parse::<i32>() {
        raw
    } else if let Some(index) = object_index(package, tables, text) {
        index
    } else {
        // Nothing in the package answers to the path, so it names an object outside it. The new
        // import takes the class of whatever the property points at today, which is the class the
        // property expects.
        let class = current.and_then(|index| tables.import_class(index));
        add_import(tables, text, class).map_err(|reason| {
            format!(
                "{text} is not among this package's imports or exports, and could not be added as an import: {reason}"
            )
        })?
    };
    let package_index = FPackageIndex { index };
    let valid = index == 0
        || (package_index.is_import()
            && (package_index.to_import_index() as usize) < tables.imports.len())
        || (package_index.is_export()
            && (package_index.to_export_index() as usize) < package.exports.len());
    if !valid {
        return Err(format!(
            "{index} is outside the {} imports and {} exports this package declares",
            tables.imports.len(),
            package.exports.len()
        ));
    }
    Ok(index.to_le_bytes().to_vec())
}

/// The import or export a path names, in either the dotted form or the slash-joined one the reader
/// renders, or `None` when nothing matches.
fn object_index(
    package: &retoc::legacy_asset::FLegacyPackageHeader,
    tables: &Tables,
    path: &str,
) -> Option<i32> {
    let candidates = (0..tables.imports.len())
        .map(|at| FPackageIndex::create_import(at as u32))
        .chain((0..package.exports.len()).map(|at| FPackageIndex::create_export(at as u32)));
    for candidate in candidates {
        let dotted = path_from(
            &tables.names,
            &tables.imports,
            &package.exports,
            &package.summary.package_name,
            candidate,
        );
        if dotted.as_deref() == Some(path) {
            return Some(candidate.index);
        }
        // The slash form is only defined for objects the package named before this save.
        let original = !candidate.is_import()
            || (candidate.to_import_index() as usize) < package.imports.len();
        if original {
            let (_, full) = retoc::legacy_asset::get_package_object_full_name(
                package, candidate, '/', false, None,
            );
            if full == path {
                return Some(candidate.index);
            }
        }
    }
    None
}

fn parse<T: std::str::FromStr>(text: &str) -> Result<T, String> {
    text.parse::<T>()
        .map_err(|_| format!("{text} is not a valid number for this property"))
}

/// Compares the decoded value with what was asked for, tolerating the formatting differences that
/// come from parsing text (`1.5` against `1.50`, `true` against `1`).
fn reads_back_as(value: &PropertyValue, text: &str) -> bool {
    match value {
        // A guid reads back as its 32 hex digits, whatever case or dashes it was typed with.
        PropertyValue::Str { value } => {
            value == text
                || (value.len() == 32
                    && text
                        .trim()
                        .chars()
                        .filter(|c| !matches!(c, '-' | '{' | '}'))
                        .collect::<String>()
                        .eq_ignore_ascii_case(value))
        }
        PropertyValue::Name { value } => value == text.trim(),
        PropertyValue::SoftObject { path } => path == text.trim(),
        PropertyValue::Text { value, .. } => value
            .as_deref()
            .is_some_and(|shown| shown == text || formatted_number_reads_back(shown, text)),
        // Typed as a path but stored as an index, so the path it resolves back to is the check.
        // Either separator convention may have been typed; only the segments have to agree.
        PropertyValue::Object { index, path } => {
            let want = text.trim();
            path.as_deref()
                .is_some_and(|path| path.replace(['.', ':'], "/") == want.replace(['.', ':'], "/"))
                || want.parse::<i32>().is_ok_and(|raw| raw == *index)
                || (*index == 0 && (want.is_empty() || want.eq_ignore_ascii_case("none")))
        }
        PropertyValue::Bool { value } => matches!(
            (value, text.trim().to_ascii_lowercase().as_str()),
            (true, "true" | "1" | "yes" | "on") | (false, "false" | "0" | "no" | "off")
        ),
        PropertyValue::Int { value } => text.trim().parse::<i64>().is_ok_and(|want| want == *value),
        PropertyValue::Enum { value, name, .. } => {
            text.trim().parse::<i64>().is_ok_and(|want| want == *value)
                || name.as_deref() == Some(text.trim())
        }
        PropertyValue::UInt { value } => {
            text.trim().parse::<u64>().is_ok_and(|want| want == *value)
        }
        PropertyValue::Byte { value } => text.trim().parse::<u8>().is_ok_and(|want| want == *value),
        // Written as f32 and read back as f64, so the exact bits will not match the typed decimal.
        PropertyValue::Float { value } => text
            .trim()
            .parse::<f64>()
            .is_ok_and(|want| (want - *value).abs() <= want.abs() * 1e-6 + f32::EPSILON as f64),
        _ => false,
    }
}

fn locate<'a>(parsed: &'a ParsedPackage, edit: &ValueEdit) -> Result<&'a PropertyEntry, String> {
    find_at(parsed, edit.offset, &edit.expect_name, edit.expect_element).ok_or_else(|| {
        format!(
            "no property called {} starts at {:#X}",
            edit.expect_name, edit.offset
        )
    })
}

/// Finds the entry of that name whose value starts at `offset`, anywhere in the package. A value
/// holding its default shares its offset with the one stored next, so the name separates them.
/// The entry an edit addressed by offset, name and element would land on.
pub fn entry_named_at<'a>(
    parsed: &'a ParsedPackage,
    offset: u64,
    name: &str,
    element: Option<u32>,
) -> Option<&'a PropertyEntry> {
    find_at(parsed, offset, name, element)
}

fn find_at<'a>(
    parsed: &'a ParsedPackage,
    offset: u64,
    name: &str,
    element: Option<u32>,
) -> Option<&'a PropertyEntry> {
    for export in &parsed.exports {
        if let Some(found) = find_in(&export.properties, offset, name, element) {
            return Some(found);
        }
        if let Some(table) = &export.data_table {
            for row in &table.rows {
                if let Some(found) = find_in(&row.fields, offset, name, element) {
                    return Some(found);
                }
            }
        }
    }
    None
}

fn find_in<'a>(
    entries: &'a [PropertyEntry],
    offset: u64,
    name: &str,
    element: Option<u32>,
) -> Option<&'a PropertyEntry> {
    for entry in entries {
        if entry.name == name
            && entry.element == element
            && entry.span.is_some_and(|(start, _)| start == offset)
        {
            return Some(entry);
        }
        if let Some(found) = find_in_value(&entry.value, offset, name, element) {
            return Some(found);
        }
    }
    None
}

fn find_in_value<'a>(
    value: &'a PropertyValue,
    offset: u64,
    name: &str,
    element: Option<u32>,
) -> Option<&'a PropertyEntry> {
    match value {
        PropertyValue::Struct { fields, .. } => find_in(fields, offset, name, element),
        PropertyValue::Array { items } | PropertyValue::Set { items } => items
            .iter()
            .find_map(|item| find_in_value(item, offset, name, element)),
        PropertyValue::Map { entries } => entries.iter().find_map(|entry| {
            find_in_value(&entry.key, offset, name, element)
                .or_else(|| find_in_value(&entry.value, offset, name, element))
        }),
        PropertyValue::Text { parts, .. } => find_in(parts, offset, name, element),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn at(value: &PropertyValue, text: &str, width: u64) -> Vec<u8> {
        encode_scalar(value, text, Some(width), "").expect("encode")
    }

    /// The width already in the asset picks the integer size, which is what makes preserving it
    /// structural rather than a check that could be forgotten.
    #[test]
    fn an_integer_is_written_at_whatever_width_the_asset_already_uses() {
        let value = PropertyValue::Int { value: 0 };
        assert_eq!(at(&value, "-1", 1), vec![0xFF]);
        assert_eq!(at(&value, "-1", 2), vec![0xFF, 0xFF]);
        assert_eq!(at(&value, "258", 4), vec![0x02, 0x01, 0, 0]);
        assert_eq!(at(&value, "1", 8).len(), 8);
    }

    /// A value that is not stored yet has no width to measure, so the schema has to supply it.
    #[test]
    fn a_value_with_no_bytes_yet_takes_the_width_its_type_declares() {
        let value = PropertyValue::Int { value: 0 };
        assert_eq!(
            encode_scalar(&value, "7", None, "Int16").expect("encode"),
            vec![7, 0]
        );
        assert_eq!(
            encode_scalar(&value, "7", None, "Int64")
                .expect("encode")
                .len(),
            8
        );
        assert!(encode_scalar(&value, "7", None, "Struct").is_err());
    }

    #[test]
    fn a_float_narrows_to_the_stored_precision() {
        let value = PropertyValue::Float { value: 0.0 };
        assert_eq!(at(&value, "1.5", 4), 1.5f32.to_le_bytes());
        assert_eq!(at(&value, "1.5", 8), 1.5f64.to_le_bytes());
    }

    #[test]
    fn a_bool_takes_the_spellings_a_person_would_type() {
        let value = PropertyValue::Bool { value: false };
        for yes in ["true", "TRUE", " 1 ", "yes", "on"] {
            assert_eq!(at(&value, yes, 1), vec![1], "{yes}");
        }
        for no in ["false", "0", "no", "off"] {
            assert_eq!(at(&value, no, 1), vec![0], "{no}");
        }
        assert!(encode_scalar(&value, "maybe", Some(1), "").is_err());
        // A natively serialized struct writes its bools as full words.
        assert_eq!(at(&value, "true", 4), vec![1, 0, 0, 0]);
    }

    /// A value that does not fit is refused rather than truncated, because a silent wrap would
    /// write a plausible-looking wrong number.
    #[test]
    fn a_value_too_large_for_the_stored_width_is_refused() {
        assert!(encode_scalar(&PropertyValue::Int { value: 0 }, "300", Some(1), "").is_err());
        assert!(encode_scalar(&PropertyValue::Byte { value: 0 }, "-1", Some(1), "").is_err());
        assert!(encode_scalar(&PropertyValue::UInt { value: 0 }, "-1", Some(4), "").is_err());
    }

    fn colours() -> Mappings {
        Mappings::from_structs_and_enums(
            Vec::new(),
            vec![usmap::Enum {
                name: "EColour".into(),
                entries: [(0, "Red".to_string()), (2, "Blue".to_string())].into(),
            }],
        )
    }

    /// A scalar enum stores a number, but a person types the enumerator; the mappings turn one
    /// into the other and refuse a name the enum does not have rather than guessing a number.
    #[test]
    fn an_enumerator_name_resolves_through_the_mappings_or_is_refused_by_name() {
        let schema = colours();
        assert_eq!(
            enumerator_value(Some(&schema), Some("EColour"), "Blue"),
            Ok(2)
        );
        assert_eq!(
            enumerator_value(Some(&schema), Some("EColour"), "Green"),
            Err("Green is not an enumerator of EColour".to_string())
        );
        assert!(enumerator_value(None, Some("EColour"), "Blue").is_err());
        assert!(enumerator_value(Some(&schema), None, "Blue").is_err());
        assert_eq!(
            enumerator_name(Some(&schema), Some("EColour"), 2),
            Ok("Blue".to_string())
        );
        assert_eq!(
            enumerator_name(Some(&schema), Some("EColour"), 1),
            Err("EColour has no enumerator 1".to_string())
        );
        assert_eq!(
            schema.enumerators("EColour"),
            vec![(0, "Red".to_string()), (2, "Blue".to_string())]
        );
        assert!(schema.enumerators("EShape").is_empty());
    }

    #[test]
    fn an_ascii_string_is_written_one_byte_per_character_with_a_terminator() {
        assert_eq!(encode_string("Hi"), vec![3, 0, 0, 0, b'H', b'i', 0]);
    }

    #[test]
    fn an_empty_string_is_just_a_zero_count() {
        assert_eq!(encode_string(""), vec![0, 0, 0, 0]);
    }

    #[test]
    fn a_string_beyond_ascii_is_written_as_utf16_with_a_negative_count() {
        let bytes = encode_string("\u{00e9}");
        assert_eq!(
            i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            -2
        );
        assert_eq!(&bytes[4..], &[0xE9, 0x00, 0x00, 0x00]);
    }

    /// A string edit is the whole point of variable width, so its length must follow the text.
    #[test]
    fn a_string_is_written_at_whatever_length_it_needs() {
        assert_eq!(encode_string("hi").len(), 7);
        assert_eq!(encode_string("hello there").len(), 16);
    }

    /// Written as f32 and read back as f64, so an exact comparison would reject every float edit.
    #[test]
    fn reading_back_a_float_tolerates_the_precision_it_was_stored_at() {
        let stored = PropertyValue::Float {
            value: f64::from(0.1f32),
        };
        assert!(reads_back_as(&stored, "0.1"));
        assert!(!reads_back_as(&stored, "0.2"));
    }

    #[test]
    fn a_string_reads_back_only_as_exactly_what_was_typed() {
        let stored = PropertyValue::Str {
            value: " padded ".into(),
        };
        assert!(reads_back_as(&stored, " padded "));
        assert!(!reads_back_as(&stored, "padded"));
    }

    fn export_with(properties: Vec<PropertyEntry>) -> ParsedPackage {
        ParsedPackage {
            info: crate::package::PackageInfo {
                package_name: "/Game/Test".into(),
                cooked: true,
                unversioned_properties: true,
                name_count: 0,
                import_count: 0,
                export_count: 1,
            },
            exports: vec![ParsedExport {
                index: 0,
                object_name: "Test".into(),
                class_name: "DataAsset".into(),
                serial_offset: 0,
                serial_size: 0,
                outer_index: 0,
                class_index: 0,
                super_index: 0,
                template_index: 0,
                object_flags: 0,
                generate_public_hash: false,
                path: String::new(),
                status: crate::package::ExportStatus::Complete,
                properties,
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
            references: Vec::new(),
            string_tables: Vec::new(),
            instanced: Vec::new(),
            tables: Vec::new(),
            channels: Vec::new(),
            native_leaves: Vec::new(),
            script_tokens: Default::default(),
            text_histories: Default::default(),
            twins: Vec::new(),
            resources: Vec::new(),
            names: Vec::new(),
            imports: Vec::new(),
            dependencies: None,
        }
    }

    fn int_at(name: &str, offset: u64, value: i64) -> PropertyEntry {
        PropertyEntry {
            name: name.into(),
            element: None,
            value: PropertyValue::Int { value },
            span: Some((offset, offset + 4)),
            slot: None,
        }
    }

    /// Two entries can share a name at different offsets: every row of a table repeats its columns.
    /// Excusing the name alone would let a corrupted neighbour through.
    #[test]
    fn an_edit_excuses_only_the_entry_at_its_own_offset() {
        let before = export_with(vec![int_at("Damage", 0x10, 1), int_at("Damage", 0x20, 2)]);
        let edit = ValueEdit {
            offset: 0x10,
            expect_name: "Damage".into(),
            expect_element: None,
            expect_kind: "int".into(),
            op: EditOp::Set { text: "7".into() },
        };
        let applied = AppliedEdit {
            name: "Damage".into(),
            offset: 0x10,
            offset_after: 0x10,
            element: None,
            elements_after: None,
            before: "1".into(),
            after: "7".into(),
        };

        let asked = export_with(vec![int_at("Damage", 0x10, 7), int_at("Damage", 0x20, 2)]);
        let edits = PackageEdits {
            values: vec![edit],
            ..Default::default()
        };
        verify_patch(&before, &asked, &edits, std::slice::from_ref(&applied))
            .expect("the asked change");

        let neighbour = export_with(vec![int_at("Damage", 0x10, 7), int_at("Damage", 0x20, 9)]);
        let error = verify_patch(&before, &neighbour, &edits, std::slice::from_ref(&applied))
            .expect_err("refused");
        assert!(error.contains("nothing asked"), "{error}");
    }

    fn name_map() -> FPackageNameMap {
        FPackageNameMap::create_from_names(vec![
            "None".into(),
            "Damage".into(),
            "/Game/Heroes/BP_Hero".into(),
            "BP_Hero".into(),
        ])
    }

    fn table(rows: &[(&str, i64)]) -> DataTable {
        DataTable {
            row_struct: "Row".into(),
            columns: vec!["X".into()],
            rows: rows
                .iter()
                .map(|(name, value)| DataTableRow {
                    name: name.to_string(),
                    fields: vec![int_at("X", 0x10, *value)],
                })
                .collect(),
            declared_rows: rows.len() as u32,
            truncated: None,
        }
    }

    fn row_edits(ops: Vec<RowOp>) -> PackageEdits {
        PackageEdits {
            rows: ops
                .into_iter()
                .map(|op| RowEdit { export: 0, op })
                .collect(),
            ..Default::default()
        }
    }

    /// Rows A, B, C become N (added before B), Z (A renamed), C and D (a copy of C at the end),
    /// with B gone. The patched table has to come out in exactly that order and nothing else.
    #[test]
    fn verification_replays_the_row_edits_over_the_table_as_it_was() {
        let was = table(&[("A", 1), ("B", 2), ("C", 3)]);
        let edits = row_edits(vec![
            RowOp::Remove { name: "b".into() },
            RowOp::Add {
                name: "N".into(),
                at: Some(1),
            },
            RowOp::Duplicate {
                source: "C".into(),
                name: "D".into(),
                at: None,
            },
            RowOp::Rename {
                name: "A".into(),
                to: "Z".into(),
            },
        ]);
        let excuses = Excuses::default();
        let mut is = table(&[("Z", 1), ("N", 0), ("C", 3), ("D", 3)]);
        is.rows[1].fields.clear();
        verify_rows(0, &was, &is, &edits, &excuses).expect("the replayed table");

        let mut stored = is.clone();
        stored.rows[1].fields = vec![int_at("X", 0x10, 0)];
        let err = verify_rows(0, &was, &stored, &edits, &excuses).expect_err("added row stores");
        assert!(err.contains("added row N stores X"), "{err}");

        let mut drifted = is.clone();
        drifted.rows[3].fields = vec![int_at("X", 0x10, 4)];
        let err = verify_rows(0, &was, &drifted, &edits, &excuses).expect_err("copy differs");
        assert!(err.contains("nothing asked"), "{err}");

        let mut reordered = is.clone();
        reordered.rows.swap(0, 1);
        let err = verify_rows(0, &was, &reordered, &edits, &excuses).expect_err("order");
        assert!(err.contains("where Z was expected"), "{err}");

        let mut short = is.clone();
        short.declared_rows = 5;
        let err = verify_rows(0, &was, &short, &edits, &excuses).expect_err("declared");
        assert!(err.contains("of 5 declared"), "{err}");
    }

    /// A two-row table in a package with names None, A and B: the count word, then each row as an
    /// eight-byte name and a one-slot header storing the slot (`0x0300`: one value, last fragment),
    /// followed by its value.
    fn table_package() -> (ParsedPackage, Vec<u8>) {
        let mut data = 2i32.to_le_bytes().to_vec();
        for (name, value) in [(1i32, 7i32), (2, 9)] {
            data.extend_from_slice(&name.to_le_bytes());
            data.extend_from_slice(&0i32.to_le_bytes());
            data.extend_from_slice(&[0x00, 0x03]);
            data.extend_from_slice(&value.to_le_bytes());
        }
        let mut parsed = export_with(Vec::new());
        parsed.exports[0].class_name = "DataTable".into();
        parsed.exports[0].data_table = Some(table(&[("A", 7), ("B", 9)]));
        parsed.tables = vec![DataTableLayout {
            export: 0,
            count_at: 0x100,
            row_slots: 1,
            tagged: false,
            rows: vec![
                RowSpan {
                    start: 0x104,
                    end: 0x112,
                },
                RowSpan {
                    start: 0x112,
                    end: 0x120,
                },
            ],
        }];
        (parsed, data)
    }

    fn table_names() -> FPackageNameMap {
        FPackageNameMap::create_from_names(vec!["None".into(), "A".into(), "B".into()])
    }

    #[test]
    fn each_row_edit_becomes_one_splice_on_the_rows_it_addresses() {
        let (parsed, data) = table_package();
        let bundle = AssetBundle {
            asset: &[],
            exports: &data,
        };
        let mut names = table_names();
        let edits = row_edits(vec![
            RowOp::Add {
                name: "C".into(),
                at: None,
            },
            RowOp::Duplicate {
                source: "a".into(),
                name: "D".into(),
                at: Some(1),
            },
            RowOp::Remove { name: "B".into() },
            RowOp::Rename {
                name: "A".into(),
                to: "B".into(),
            },
        ]);
        let out = row_splices(&parsed, &bundle, 0x100, &mut names, &edits).expect("splices");
        let splices: Vec<(u64, u64, Vec<u8>)> = out
            .iter()
            .map(|(splice, _)| (splice.start, splice.end, splice.bytes.clone()))
            .collect();
        let name = |index: i32| {
            let mut out = index.to_le_bytes().to_vec();
            out.extend_from_slice(&0i32.to_le_bytes());
            out
        };
        // An empty row is a header that skips its one slot and stops: `0x0101`.
        let mut added = name(3);
        added.extend_from_slice(&[0x01, 0x01]);
        let mut copied = name(4);
        copied.extend_from_slice(&[0x00, 0x03, 7, 0, 0, 0]);
        assert_eq!(
            splices,
            vec![
                (0x120, 0x120, added),
                (0x112, 0x112, copied),
                (0x112, 0x120, Vec::new()),
                (0x104, 0x10C, name(2)),
            ]
        );
        assert_eq!(out[1].1.before, "copy of a");
        assert_eq!(out[2].1.after, "(removed)");
        assert_eq!(names.num_names(), 5);
    }

    #[test]
    fn a_row_added_to_a_tagged_table_is_its_name_and_a_lone_none() {
        let (mut parsed, data) = table_package();
        parsed.tables[0].tagged = true;
        parsed.tables[0].row_slots = 0;
        let bundle = AssetBundle {
            asset: &[],
            exports: &data,
        };
        let edits = row_edits(vec![RowOp::Add {
            name: "C".into(),
            at: None,
        }]);
        let out =
            row_splices(&parsed, &bundle, 0x100, &mut table_names(), &edits).expect("splices");
        let mut added = 3i32.to_le_bytes().to_vec();
        added.extend_from_slice(&[0; 12]);
        assert_eq!(out[0].0.bytes, added);
    }

    #[test]
    fn a_row_name_the_table_will_still_hold_is_refused() {
        let (parsed, data) = table_package();
        let bundle = AssetBundle {
            asset: &[],
            exports: &data,
        };
        let refused = |ops: Vec<RowOp>| {
            row_splices(&parsed, &bundle, 0x100, &mut table_names(), &row_edits(ops))
                .expect_err("refused")
        };
        let err = refused(vec![RowOp::Add {
            name: "a".into(),
            at: None,
        }]);
        assert!(err.contains("already has a row named a"), "{err}");
        let err = refused(vec![
            RowOp::Add {
                name: "C".into(),
                at: None,
            },
            RowOp::Rename {
                name: "B".into(),
                to: "c".into(),
            },
        ]);
        assert!(err.contains("already has a row named c"), "{err}");
        let err = refused(vec![
            RowOp::Remove { name: "A".into() },
            RowOp::Rename {
                name: "A".into(),
                to: "C".into(),
            },
        ]);
        assert!(err.contains("both removed and renamed"), "{err}");
        let err = refused(vec![RowOp::Remove { name: "Q".into() }]);
        assert!(err.contains("no row named Q"), "{err}");
        let err = refused(vec![RowOp::Add {
            name: "C".into(),
            at: Some(3),
        }]);
        assert!(err.contains("no position recorded for row 3"), "{err}");
    }

    #[test]
    fn a_value_inside_a_removed_row_cannot_be_edited_in_the_same_save() {
        let (parsed, data) = table_package();
        let bundle = AssetBundle {
            asset: &[],
            exports: &data,
        };
        let mut edits = row_edits(vec![RowOp::Remove { name: "B".into() }]);
        edits.values.push(ValueEdit {
            offset: 0x11C,
            expect_name: "X".into(),
            expect_element: None,
            expect_kind: "int".into(),
            op: EditOp::Set { text: "1".into() },
        });
        let err =
            row_splices(&parsed, &bundle, 0x100, &mut table_names(), &edits).expect_err("refused");
        assert!(err.contains("row B is being removed"), "{err}");
        // A boundary belongs to the row before it: a value with no bytes at row B's start is row
        // A's, while one at row B's end is row B's.
        edits.values[0].offset = 0x112;
        row_splices(&parsed, &bundle, 0x100, &mut table_names(), &edits).expect("row A's value");
        edits.values[0].offset = 0x120;
        let err =
            row_splices(&parsed, &bundle, 0x100, &mut table_names(), &edits).expect_err("refused");
        assert!(err.contains("row B is being removed"), "{err}");
    }

    #[test]
    fn a_name_already_in_the_package_is_written_as_its_index() {
        let mut names = name_map();
        let bytes = encode_name("Damage", &mut names);
        assert_eq!(bytes, vec![1, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(names.num_names(), 4, "nothing was appended");
    }

    /// A name the package has never seen has to be appended, which is what makes the header grow.
    #[test]
    fn a_name_the_package_does_not_have_is_appended_to_the_map() {
        let mut names = name_map();
        let bytes = encode_name("Healing", &mut names);
        assert_eq!(
            i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            4
        );
        assert_eq!(names.num_names(), 5);
    }

    /// UE splits a trailing number out of the name, so `Thing_3` is `Thing` with number four.
    #[test]
    fn a_numbered_name_keeps_its_suffix_in_the_number_field() {
        let mut names = name_map();
        let bytes = encode_name("Damage_3", &mut names);
        assert_eq!(
            i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            1
        );
        assert_eq!(
            i32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            4
        );
        assert_eq!(names.num_names(), 4);
    }

    #[test]
    fn a_soft_object_path_splits_into_package_asset_and_sub_path() {
        let mut names = name_map();
        let bytes = encode_soft_object("/Game/Heroes/BP_Hero.BP_Hero:Node", &mut names);
        assert_eq!(
            i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            2
        );
        assert_eq!(
            i32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            3
        );
        assert_eq!(&bytes[16..], &encode_string("Node")[..]);
    }

    /// An empty reference is written as the name `None`, which is how the reader renders it back.
    #[test]
    fn an_empty_soft_object_path_writes_none_for_both_names() {
        let mut names = name_map();
        let bytes = encode_soft_object("", &mut names);
        assert_eq!(
            i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            0
        );
        assert_eq!(
            i32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            0
        );
        assert_eq!(&bytes[16..], &encode_string("")[..]);
    }

    /// The namespace and key identify a localized entry, so editing the source string must leave
    /// them exactly as they were.
    #[test]
    fn editing_localized_text_keeps_its_namespace_and_key() {
        let mut was = vec![0u8; 4];
        was.push(0);
        was.extend_from_slice(&encode_string("NS"));
        was.extend_from_slice(&encode_string("KEY"));
        was.extend_from_slice(&encode_string("old"));
        let out = encode_text("new", &was).expect("encode");
        let mut want = vec![0u8; 4];
        want.push(0);
        want.extend_from_slice(&encode_string("NS"));
        want.extend_from_slice(&encode_string("KEY"));
        want.extend_from_slice(&encode_string("new"));
        assert_eq!(out, want);
    }

    #[test]
    fn editing_plain_text_writes_a_culture_invariant_string() {
        let mut was = vec![0u8; 4];
        was.push(0xFF);
        was.extend_from_slice(&0u32.to_le_bytes());
        let out = encode_text("hello", &was).expect("encode");
        assert_eq!(out[4], 0xFF);
        assert_eq!(&out[5..9], &1u32.to_le_bytes());
        assert_eq!(&out[9..], &encode_string("hello")[..]);
    }

    /// `FTextHistory_FormatNumber` as UE writes it: flags, history, an optional currency code, the
    /// argument's type byte and value, the options flag with its block, then the culture.
    fn formatted_number(history: i8, code: Option<&str>, kind: i8, value: &[u8]) -> Vec<u8> {
        let mut was = vec![0u8; 4];
        was.push(history as u8);
        if let Some(code) = code {
            was.extend_from_slice(&encode_string(code));
        }
        was.push(kind as u8);
        was.extend_from_slice(value);
        was.extend_from_slice(&1u32.to_le_bytes());
        was.extend_from_slice(&[7u8; 25]);
        was.extend_from_slice(&encode_string("en"));
        was
    }

    /// Only the source value changes: the type byte, the options and the culture come through
    /// untouched, and the value is written in the argument's own type.
    #[test]
    fn editing_a_formatted_number_rewrites_only_its_source_value() {
        let was = formatted_number(4, None, 3, &2.0f64.to_le_bytes());
        let out = encode_text("3", &was).expect("encode");
        assert_eq!(out, formatted_number(4, None, 3, &3.0f64.to_le_bytes()));

        let was = formatted_number(4, None, 0, &(-4i64).to_le_bytes());
        assert_eq!(
            encode_text("12", &was).expect("encode"),
            formatted_number(4, None, 0, &12i64.to_le_bytes())
        );
        let error = encode_text("1.5", &was).expect_err("refused");
        assert!(error.contains("not a whole number"), "{error}");

        let was = formatted_number(5, None, 2, &0.25f32.to_le_bytes());
        assert_eq!(
            encode_text("0.5 as percent", &was).expect("encode"),
            formatted_number(5, None, 2, &0.5f32.to_le_bytes())
        );

        let was = formatted_number(6, Some("USD"), 3, &9.0f64.to_le_bytes());
        assert_eq!(
            encode_text("EUR 10", &was).expect("encode"),
            formatted_number(6, Some("EUR"), 3, &10.0f64.to_le_bytes())
        );
        assert_eq!(
            encode_text("11", &was).expect("encode"),
            formatted_number(6, Some("USD"), 3, &11.0f64.to_le_bytes())
        );

        let was = formatted_number(4, None, 4, &[]);
        let error = encode_text("3", &was).expect_err("refused");
        assert!(error.contains("formats another text"), "{error}");
    }

    #[test]
    fn a_formatted_number_reads_back_around_its_decoration() {
        assert!(formatted_number_reads_back("3", "3.0"));
        assert!(formatted_number_reads_back("0.5 as percent", "0.5"));
        assert!(formatted_number_reads_back("EUR 10", "EUR 10"));
        assert!(formatted_number_reads_back("EUR 10", "10"));
        assert!(!formatted_number_reads_back("USD 10", "EUR 10"));
        assert!(!formatted_number_reads_back("4", "3"));
    }

    /// A history this cannot rebuild is refused by name, because writing a plausible-looking but
    /// wrong FText would break the string silently.
    #[test]
    fn text_with_a_history_that_cannot_be_rebuilt_is_refused() {
        let mut was = vec![0u8; 4];
        was.push(11);
        let error = encode_text("hello", &was).expect_err("refused");
        assert!(error.contains("history type 11"), "{error}");
    }

    /// One wire shape serves the desktop app, the CLI and an edit file on disk, so these strings
    /// are the format: a change here changes every caller and every saved file.
    #[test]
    fn every_edit_kind_round_trips_through_its_json_form() {
        fn same<T>(value: T, json: &str)
        where
            T: serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug,
        {
            let written = serde_json::to_string(&value).expect("serialize");
            assert_eq!(written, json);
            let read: T = serde_json::from_str(json).expect("deserialize");
            assert_eq!(
                serde_json::to_string(&read).expect("re-serialize"),
                json,
                "{value:?} did not survive the round trip"
            );
        }

        same(
            ValueEdit {
                offset: 4660,
                expect_name: "Damage".into(),
                expect_element: None,
                expect_kind: "float".into(),
                op: EditOp::Set {
                    text: "42.5".into(),
                },
            },
            r#"{"offset":4660,"name":"Damage","kind":"float","op":"set","text":"42.5"}"#,
        );
        same(
            ValueEdit {
                offset: 16,
                expect_name: "Tags".into(),
                expect_element: Some(2),
                expect_kind: "set".into(),
                op: EditOp::Insert {
                    index: 0,
                    key: Some("Hero.A".into()),
                },
            },
            r#"{"offset":16,"name":"Tags","element":2,"kind":"set","op":"insert","index":0,"key":"Hero.A"}"#,
        );
        same(
            ValueEdit {
                offset: 8,
                expect_name: "Radius".into(),
                expect_element: None,
                expect_kind: "float".into(),
                op: EditOp::Clear,
            },
            r#"{"offset":8,"name":"Radius","kind":"float","op":"clear"}"#,
        );
        same(
            RowEdit {
                export: 0,
                op: RowOp::Add {
                    name: "NewRow".into(),
                    at: Some(2),
                },
            },
            r#"{"export":0,"op":"add","name":"NewRow","at":2}"#,
        );
        same(
            RowEdit {
                export: 0,
                op: RowOp::Rename {
                    name: "Old".into(),
                    to: "New".into(),
                },
            },
            r#"{"export":0,"op":"rename","name":"Old","to":"New"}"#,
        );
        same(
            StringEdit {
                export: 0,
                op: StringOp::SetMetaData {
                    index: 12,
                    key: "Lobby_Play".into(),
                    id: "Comment".into(),
                    to: "probe".into(),
                },
            },
            r#"{"export":0,"op":"set_meta_data","index":12,"key":"Lobby_Play","id":"Comment","to":"probe"}"#,
        );
        same(
            KeyEdit {
                offset: 6100,
                expect_name: "Channel".into(),
                expect_element: None,
                op: KeyOp::Move { index: 0, time: 12 },
            },
            r#"{"offset":6100,"name":"Channel","op":"move","index":0,"time":12}"#,
        );
        same(
            DuplicateExport {
                export: 2,
                name: "Cue_Copy".into(),
                into_level: None,
            },
            r#"{"export":2,"name":"Cue_Copy"}"#,
        );
        same(
            crate::header_edit::ImportEdit::Retarget {
                import: 3,
                path: "/Game/X.X".into(),
                class: None,
            },
            r#"{"op":"retarget","import":3,"path":"/Game/X.X"}"#,
        );
    }

    /// An import can be added by path alone; the placeholder class is what the linker takes.
    #[test]
    fn an_import_add_defaults_its_class() {
        let edit: crate::header_edit::ImportEdit =
            serde_json::from_str(r#"{"op":"add","path":"/Game/X.X"}"#).expect("parse");
        let crate::header_edit::ImportEdit::Add {
            class_package,
            class_name,
            ..
        } = edit
        else {
            panic!("expected an add");
        };
        assert_eq!(
            (class_package.as_str(), class_name.as_str()),
            ("/Script/CoreUObject", "Object")
        );
    }

    fn entry(name: &str, value: PropertyValue, span: Option<(u64, u64)>) -> PropertyEntry {
        PropertyEntry {
            name: name.into(),
            element: None,
            value,
            span,
            slot: None,
        }
    }

    fn splice_at(start: u64, end: u64, width: usize) -> Splice {
        Splice {
            start,
            end,
            bytes: vec![0; width],
        }
    }

    fn container(
        kind: &'static str,
        default_element: Option<Vec<u8>>,
        default_name: Option<&str>,
        elements: Vec<(u64, u64)>,
        keys: Option<crate::props::MapKeys>,
    ) -> crate::props::ContainerLayout {
        crate::props::ContainerLayout {
            at: 0x100,
            count_at: 0x100,
            count_width: 4,
            elements_at: None,
            absent: None,
            elements,
            element_kind: kind,
            default_element,
            element_is_enum: kind == "Enum",
            element_enum: None,
            default_name: default_name.map(str::to_string),
            default_recipe: None,
            keys,
        }
    }

    /// A recipe's `None` takes whatever index the package's name map gives it, and a reflected
    /// block the reader could not resolve is refused rather than written empty.
    #[test]
    fn a_default_recipe_spells_none_through_the_name_map() {
        use crate::props::DefaultPart;
        let mut names = FPackageNameMap::create_from_names(vec!["Other".into(), "None".into()]);
        let bytes = realise_default(
            &[DefaultPart::NoneName, DefaultPart::Bytes(vec![7, 7])],
            &mut names,
        )
        .expect("realise");
        assert_eq!(bytes, vec![1, 0, 0, 0, 0, 0, 0, 0, 7, 7]);
        let error =
            realise_default(&[DefaultPart::Struct("Mystery")], &mut names).expect_err("unresolved");
        assert!(error.contains("Mystery"), "{error}");
    }

    fn holding(value: PropertyValue) -> PropertyEntry {
        entry("Things", value, Some((0x100, 0x104)))
    }

    fn tables() -> Tables {
        Tables {
            names: name_map(),
            imports: Vec::new(),
        }
    }

    /// `insertion` over a bare export buffer, with no key typed.
    fn insert(
        layout: &crate::props::ContainerLayout,
        index: u32,
        key: Option<&str>,
        data: &[u8],
        entry: &PropertyEntry,
        tables: &mut Tables,
    ) -> Result<(u64, Vec<u8>), String> {
        let bundle = AssetBundle {
            asset: &[],
            exports: data,
        };
        let package = retoc::legacy_asset::FLegacyPackageHeader::default();
        insertion(
            layout, index, key, None, &bundle, 0x100, entry, tables, &package, None,
        )
        .map(|(at, bytes, _)| (at, bytes))
    }

    /// A list a native struct writes behind a byte count moves that byte rather than a word, and
    /// cannot be grown past what one byte holds.
    #[test]
    fn a_byte_wide_count_is_adjusted_one_byte_at_a_time() {
        let data = [2u8, 0, 0, 0, 0, 0, 0, 0];
        let bundle = AssetBundle {
            asset: &[],
            exports: &data,
        };
        let splice = adjust_count_width(&bundle, 0x100, 0x100, 1, 1).expect("grow");
        assert_eq!(
            (splice.start, splice.end, splice.bytes),
            (0x100, 0x101, vec![3])
        );
        assert_eq!(
            adjust_count_width(&bundle, 0x100, 0x100, 1, -1)
                .expect("shrink")
                .bytes,
            vec![1]
        );
        let error = adjust_count_width(&bundle, 0x100, 0x100, 1, -3).expect_err("below zero");
        assert!(error.contains("that many elements"), "{error}");

        let full = [255u8, 0, 0, 0];
        let bundle = AssetBundle {
            asset: &[],
            exports: &full,
        };
        let error = adjust_count_width(&bundle, 0x100, 0x100, 1, 1).expect_err("above 255");
        assert!(error.contains("at most 255"), "{error}");
        let splice = adjust_count_width(&bundle, 0x100, 0x100, 4, 1).expect("the word form");
        assert_eq!(
            (splice.end - splice.start, splice.bytes),
            (4, vec![0, 1, 0, 0])
        );
    }

    /// The first element of an empty list lands right after the count, whatever the count's width.
    #[test]
    fn an_empty_byte_counted_list_grows_right_after_its_count() {
        let data = [0u8; 8];
        let mut tables = tables();
        let mut segments = container("Str", Some(vec![0, 0, 0, 0]), None, vec![], None);
        segments.count_width = 1;
        let empty = holding(PropertyValue::Array { items: vec![] });
        let (at, bytes) = insert(&segments, 0, None, &data, &empty, &mut tables).expect("segment");
        assert_eq!((at, bytes), (0x101, vec![0, 0, 0, 0]));
    }

    #[test]
    fn an_empty_array_grows_its_first_element_from_the_type_default() {
        let data = [0u8; 8];
        let mut tables = tables();
        let structs = container(
            "Struct",
            Some(unversioned::empty_header(3)),
            None,
            vec![],
            None,
        );
        let empty = holding(PropertyValue::Array { items: vec![] });
        let (at, bytes) = insert(&structs, 0, None, &data, &empty, &mut tables).expect("struct");
        assert_eq!((at, bytes), (0x104, vec![0x03, 0x01]));

        let tags = container("Name", None, Some("None"), vec![], None);
        let (_, bytes) = insert(&tags, 0, None, &data, &empty, &mut tables).expect("name");
        assert_eq!(bytes, encode_name("None", &mut tables.names));

        let unknown = container("Struct", None, None, vec![], None);
        let err = insert(&unknown, 0, None, &data, &empty, &mut tables).expect_err("refused");
        assert!(err.contains("no default"), "{err}");
    }

    /// A key typed for a set or a map is written in the key's own kind, lands after the last
    /// element, and is refused when an element already carries it or when the kind has no text
    /// form. A container that holds elements needs a key; the default serves only an empty one.
    #[test]
    fn a_keyed_container_takes_a_typed_key_in_the_keys_own_kind() {
        let mut data = vec![0u8; 4];
        data.extend_from_slice(&7i32.to_le_bytes());
        let mut tables = tables();
        let set = holding(PropertyValue::Set {
            items: vec![PropertyValue::Int { value: 7 }],
        });
        let ints = container("Int", Some(vec![0; 4]), None, vec![(0x104, 0x108)], None);
        let (at, bytes) = insert(&ints, 0, Some("9"), &data, &set, &mut tables).expect("int key");
        assert_eq!((at, bytes), (0x108, 9i32.to_le_bytes().to_vec()));
        let err = insert(&ints, 0, Some("7"), &data, &set, &mut tables).expect_err("repeat");
        assert!(err.contains("already holds the key 7"), "{err}");
        let err = insert(&ints, 0, Some("nine"), &data, &set, &mut tables).expect_err("text");
        assert!(err.contains("nine"), "{err}");
        let err = insert(&ints, 0, None, &data, &set, &mut tables).expect_err("no key");
        assert!(err.contains("needs a key"), "{err}");

        let mut data = vec![0u8; 4];
        data.extend_from_slice(&encode_name("Damage", &mut tables.names));
        let names = container("Name", None, Some("None"), vec![(0x104, 0x10C)], None);
        let (_, bytes) =
            insert(&names, 0, Some("BP_Hero"), &data, &set, &mut tables).expect("name key");
        assert_eq!(bytes, encode_name("BP_Hero", &mut tables.names));
        let err = insert(&names, 0, Some("Damage"), &data, &set, &mut tables).expect_err("repeat");
        assert!(err.contains("already holds the key Damage"), "{err}");

        let mut data = vec![0u8; 4];
        data.extend_from_slice(&1i32.to_le_bytes());
        data.extend_from_slice(&9i32.to_le_bytes());
        let keys = crate::props::MapKeys {
            spans: vec![(0x104, 0x108)],
            kind: "Int",
            is_enum: false,
            enum_type: None,
            default: Some(vec![0; 4]),
            default_name: None,
            default_recipe: None,
        };
        let map = container(
            "Int",
            Some(vec![0; 4]),
            None,
            vec![(0x104, 0x10C)],
            Some(keys),
        );
        let pairs = holding(PropertyValue::Map {
            entries: vec![crate::value::MapEntry {
                key: PropertyValue::Int { value: 1 },
                value: PropertyValue::Int { value: 9 },
            }],
        });
        let (at, bytes) = insert(&map, 0, Some("2"), &data, &pairs, &mut tables).expect("pair");
        assert_eq!((at, bytes), (0x10C, vec![2, 0, 0, 0, 0, 0, 0, 0]));

        let structs = crate::props::MapKeys {
            spans: vec![(0x104, 0x108)],
            kind: "Struct",
            is_enum: false,
            enum_type: None,
            default: Some(vec![7, 0, 0, 0]),
            default_name: None,
            default_recipe: None,
        };
        let map = container(
            "Int",
            Some(vec![0; 4]),
            None,
            vec![(0x104, 0x10C)],
            Some(structs),
        );
        let err = insert(&map, 0, Some("x"), &data, &pairs, &mut tables).expect_err("struct");
        assert!(err.contains("struct key cannot be typed"), "{err}");
        // With no text form, a struct key goes in as its default, and only while no pair has it.
        let (at, bytes) = insert(&map, 0, None, &data, &pairs, &mut tables).expect("default key");
        assert_eq!((at, bytes), (0x10C, vec![7, 0, 0, 0, 0, 0, 0, 0]));
        let mut held = vec![0u8; 4];
        held.extend_from_slice(&7i32.to_le_bytes());
        held.extend_from_slice(&9i32.to_le_bytes());
        let err = insert(&map, 0, None, &held, &pairs, &mut tables).expect_err("held");
        assert!(
            err.contains("already holds the key it defaults to"),
            "{err}"
        );

        let enums = container("Enum", None, Some("Red"), vec![], None);
        let empty = holding(PropertyValue::Set { items: vec![] });
        let err = insert(&enums, 0, Some("2"), &[0u8; 4], &empty, &mut tables).expect_err("number");
        assert!(err.contains("type the enumerator"), "{err}");
        let (_, bytes) =
            insert(&enums, 0, Some("Blue"), &[0u8; 4], &empty, &mut tables).expect("enum");
        assert_eq!(bytes, encode_name("Blue", &mut tables.names));
    }

    /// A set keys on its contents, so it grows by the default element and only while that key is
    /// absent; the new element lands after the last one whatever position was asked for.
    #[test]
    fn a_set_takes_a_default_element_unless_it_already_holds_that_key() {
        let mut data = vec![0u8; 4];
        data.extend_from_slice(&7i32.to_le_bytes());
        let mut tables = tables();
        let set = holding(PropertyValue::Set {
            items: vec![PropertyValue::Int { value: 7 }],
        });
        let ints = container("Int", Some(vec![0; 4]), None, vec![], None);
        let empty = holding(PropertyValue::Set { items: vec![] });
        let (at, bytes) = insert(&ints, 0, None, &data[..4], &empty, &mut tables).expect("set");
        assert_eq!((at, bytes), (0x104, vec![0, 0, 0, 0]));

        data[4..8].copy_from_slice(&0i32.to_le_bytes());
        let ints = container("Int", Some(vec![0; 4]), None, vec![(0x104, 0x108)], None);
        let err = insert(&ints, 0, Some("0"), &data, &set, &mut tables).expect_err("refused");
        assert!(err.contains("already holds the key 0"), "{err}");
    }

    /// A map pair is a key and a value; a fresh pair is both defaults, and a value edit addresses
    /// only the bytes after the key.
    #[test]
    fn a_map_grows_by_a_default_pair_and_edits_the_value_after_the_key() {
        let mut data = vec![0u8; 4];
        data.extend_from_slice(&1i32.to_le_bytes());
        data.extend_from_slice(&9i32.to_le_bytes());
        let mut tables = tables();
        let keys = crate::props::MapKeys {
            spans: vec![(0x104, 0x108)],
            kind: "Int",
            is_enum: false,
            enum_type: None,
            default: Some(vec![0; 4]),
            default_name: None,
            default_recipe: None,
        };
        let map = container(
            "Str",
            Some(vec![0; 4]),
            None,
            vec![(0x104, 0x10C)],
            Some(keys),
        );
        let entry = holding(PropertyValue::Map {
            entries: vec![crate::value::MapEntry {
                key: PropertyValue::Int { value: 1 },
                value: PropertyValue::Str {
                    value: String::new(),
                },
            }],
        });
        let (at, bytes) = insert(&map, 5, Some("0"), &data, &entry, &mut tables).expect("pair");
        assert_eq!((at, bytes), (0x10C, vec![0; 8]));
        assert_eq!(value_span(&map, 0, &entry).expect("span"), (0x108, 0x10C));
        let err = value_span(&map, 1, &entry).expect_err("refused");
        assert!(err.contains("no element 1"), "{err}");
    }

    fn string_table(entries: &[(&str, &str)]) -> StringTable {
        StringTable {
            namespace: "NS".into(),
            entries: entries
                .iter()
                .map(|(key, source)| crate::stringtable::StringTableEntry {
                    key: key.to_string(),
                    source: source.to_string(),
                    tag: String::new(),
                    metadata: Vec::new(),
                })
                .collect(),
            loose_metadata: Vec::new(),
        }
    }

    fn string_edits(ops: Vec<StringOp>) -> PackageEdits {
        PackageEdits {
            strings: ops
                .into_iter()
                .map(|op| StringEdit { export: 0, op })
                .collect(),
            ..Default::default()
        }
    }

    /// Entries A, B, C: B is removed, A is rekeyed to Z and given a new string, D is added. The
    /// patched table has to read Z, C, D in that order and nothing else.
    #[test]
    fn verification_replays_the_string_edits_over_the_table_as_it_was() {
        let was = string_table(&[("A", "a"), ("B", "b"), ("C", "c")]);
        let edits = string_edits(vec![
            StringOp::Remove {
                index: 1,
                key: "B".into(),
            },
            StringOp::SetKey {
                index: 0,
                key: "A".into(),
                to: "Z".into(),
            },
            StringOp::SetSource {
                index: 0,
                key: "A".into(),
                to: "zed".into(),
            },
            StringOp::Add {
                key: "D".into(),
                source: "d".into(),
            },
        ]);
        let is = string_table(&[("Z", "zed"), ("C", "c"), ("D", "d")]);
        verify_strings(0, &was, &is, &edits).expect("the replayed table");
        let drifted = string_table(&[("Z", "zed"), ("C", "changed"), ("D", "d")]);
        let err = verify_strings(0, &was, &drifted, &edits).expect_err("drift");
        assert!(err.contains("entry C"), "{err}");
        let short = string_table(&[("Z", "zed"), ("C", "c")]);
        let err = verify_strings(0, &was, &short, &edits).expect_err("count");
        assert!(err.contains("expected to hold 3"), "{err}");
    }

    /// A two-entry table laid out as the reader records it: each entry a key, a string and a zero
    /// metadata count, then the closing word.
    fn string_package() -> (ParsedPackage, Vec<u8>) {
        let mut data = Vec::new();
        data.extend_from_slice(&encode_string("NS"));
        let count_at = 0x100 + data.len() as u64;
        data.extend_from_slice(&2i32.to_le_bytes());
        let mut spans = Vec::new();
        for (key, source) in [("A", "a"), ("B", "bee")] {
            let key_at = 0x100 + data.len() as u64;
            data.extend_from_slice(&encode_string(key));
            let source_at = 0x100 + data.len() as u64;
            data.extend_from_slice(&encode_string(source));
            let tag_at = 0x100 + data.len() as u64;
            data.extend_from_slice(&0i32.to_le_bytes());
            spans.push(crate::stringtable::StringEntrySpan {
                key: (key_at, source_at),
                source: (source_at, tag_at),
                tag_at,
                end: 0x100 + data.len() as u64,
            });
        }
        let trailer_at = 0x100 + data.len() as u64;
        data.extend_from_slice(&0i32.to_le_bytes());
        let mut parsed = export_with(Vec::new());
        parsed.exports[0].class_name = "StringTable".into();
        parsed.exports[0].string_table = Some(string_table(&[("A", "a"), ("B", "bee")]));
        parsed.string_tables = vec![StringTableLayout {
            export: 0,
            namespace: (0x100, count_at),
            count_at,
            entries: spans,
            trailer_at,
            records: Vec::new(),
            end: 0x100 + data.len() as u64,
        }];
        (parsed, data)
    }

    /// The two-entry table with A tagged `Encrypt` and one metadata record on it: `Comment` = `c`.
    fn tagged_string_package() -> (ParsedPackage, Vec<u8>) {
        let mut names = name_map();
        let mut data = Vec::new();
        data.extend_from_slice(&encode_string("NS"));
        let count_at = 0x100 + data.len() as u64;
        data.extend_from_slice(&2i32.to_le_bytes());
        let mut spans = Vec::new();
        for (key, source, tag) in [("A", "a", "Encrypt"), ("B", "bee", "")] {
            let key_at = 0x100 + data.len() as u64;
            data.extend_from_slice(&encode_string(key));
            let source_at = 0x100 + data.len() as u64;
            data.extend_from_slice(&encode_string(source));
            let tag_at = 0x100 + data.len() as u64;
            data.extend_from_slice(&encode_string(tag));
            spans.push(crate::stringtable::StringEntrySpan {
                key: (key_at, source_at),
                source: (source_at, tag_at),
                tag_at,
                end: 0x100 + data.len() as u64,
            });
        }
        let trailer_at = 0x100 + data.len() as u64;
        data.extend_from_slice(&1i32.to_le_bytes());
        let record_start = 0x100 + data.len() as u64;
        data.extend_from_slice(&encode_string("A"));
        let record_count_at = 0x100 + data.len() as u64;
        data.extend_from_slice(&1i32.to_le_bytes());
        let item_at = 0x100 + data.len() as u64;
        data.extend(encode_name("Comment", &mut names));
        let value_at = 0x100 + data.len() as u64;
        data.extend_from_slice(&encode_string("c"));
        let end = 0x100 + data.len() as u64;
        let mut table = string_table(&[("A", "a"), ("B", "bee")]);
        table.entries[0].tag = "Encrypt".into();
        table.entries[0].metadata = vec![("Comment".into(), "c".into())];
        let mut parsed = export_with(Vec::new());
        parsed.exports[0].class_name = "StringTable".into();
        parsed.exports[0].string_table = Some(table);
        parsed.string_tables = vec![StringTableLayout {
            export: 0,
            namespace: (0x100, count_at),
            count_at,
            entries: spans,
            trailer_at,
            records: vec![crate::stringtable::MetaRecordSpan {
                key: "A".into(),
                start: record_start,
                count_at: record_count_at,
                items: vec![crate::stringtable::MetaItemSpan {
                    id: "Comment".into(),
                    start: item_at,
                    value: (value_at, end),
                }],
                end,
            }],
            end,
        }];
        (parsed, data)
    }

    /// A tag is rewritten in place. Metadata lands where the map has room for it: an item the
    /// entry has is rewritten, a new item is appended to the entry's record with its count moved,
    /// and an entry without a record gets one at the end of the table with the map's count moved.
    /// Renaming an entry renames its record; removing the entry removes the record.
    #[test]
    fn tag_and_metadata_edits_splice_the_marker_and_the_map() {
        let (parsed, _data) = tagged_string_package();
        let layout = &parsed.string_tables[0];
        let record = &layout.records[0];
        let mut names = name_map();
        let edits = string_edits(vec![
            StringOp::SetTag {
                index: 0,
                key: "A".into(),
                to: String::new(),
            },
            StringOp::SetMetaData {
                index: 0,
                key: "A".into(),
                id: "Comment".into(),
                to: "changed".into(),
            },
            StringOp::SetMetaData {
                index: 0,
                key: "A".into(),
                id: "Note".into(),
                to: "n".into(),
            },
            StringOp::SetMetaData {
                index: 1,
                key: "B".into(),
                id: "Comment".into(),
                to: "fresh".into(),
            },
        ]);
        let out = string_splices(&parsed, &edits, &mut names).expect("splices");
        let spans: Vec<(u64, u64)> = out
            .edits
            .iter()
            .map(|(splice, _)| (splice.start, splice.end))
            .collect();
        assert_eq!(
            spans,
            vec![
                (layout.entries[0].tag_at, layout.entries[0].end),
                record.items[0].value,
                (record.end, record.end),
                (layout.end, layout.end),
            ]
        );
        assert_eq!(out.edits[0].0.bytes, encode_string(""));
        assert_eq!(out.edits[1].0.bytes, encode_string("changed"));
        let mut item = encode_name("Note", &mut names);
        item.extend(encode_string("n"));
        assert_eq!(out.edits[2].0.bytes, item);
        let mut fresh = encode_string("B");
        fresh.extend_from_slice(&1i32.to_le_bytes());
        fresh.extend(encode_name("Comment", &mut names));
        fresh.extend(encode_string("fresh"));
        assert_eq!(out.edits[3].0.bytes, fresh);
        assert_eq!(
            out.counts,
            vec![(record.count_at, 1), (layout.trailer_at, 1)]
        );
        assert!(out.follow.is_empty());

        let edits = string_edits(vec![
            StringOp::SetKey {
                index: 0,
                key: "A".into(),
                to: "Z".into(),
            },
            StringOp::RemoveMetaData {
                index: 0,
                key: "A".into(),
                id: "Comment".into(),
            },
        ]);
        let out = string_splices(&parsed, &edits, &mut names).expect("splices");
        assert_eq!(out.follow.len(), 1, "the record is renamed with its entry");
        assert_eq!(
            (out.follow[0].start, out.follow[0].end),
            (record.start, record.count_at)
        );
        assert_eq!(out.follow[0].bytes, encode_string("Z"));
        assert_eq!(
            (out.edits[1].0.start, out.edits[1].0.end),
            (record.start, record.end),
            "the last item takes the record with it"
        );
        assert_eq!(out.counts, vec![(layout.trailer_at, -1)]);

        let edits = string_edits(vec![StringOp::Remove {
            index: 0,
            key: "A".into(),
        }]);
        let out = string_splices(&parsed, &edits, &mut names).expect("splices");
        assert_eq!(out.follow.len(), 1, "the record goes with the entry");
        assert_eq!(out.counts, vec![(layout.trailer_at, -1)]);

        let twice = string_edits(vec![
            StringOp::SetMetaData {
                index: 1,
                key: "B".into(),
                id: "X".into(),
                to: "1".into(),
            },
            StringOp::SetMetaData {
                index: 1,
                key: "B".into(),
                id: "Y".into(),
                to: "2".into(),
            },
        ]);
        let err = string_splices(&parsed, &twice, &mut names).expect_err("two new items");
        assert!(err.contains("first item in one save"), "{err}");
        let missing = string_edits(vec![StringOp::RemoveMetaData {
            index: 1,
            key: "B".into(),
            id: "Comment".into(),
        }]);
        let err = string_splices(&parsed, &missing, &mut names).expect_err("no record");
        assert!(err.contains("has no metadata"), "{err}");

        let was = parsed.exports[0].string_table.clone().expect("table");
        let mut is = was.clone();
        is.entries[0].tag = String::new();
        is.entries[0].metadata = vec![
            ("Comment".into(), "changed".into()),
            ("Note".into(), "n".into()),
        ];
        is.entries[1].metadata = vec![("Comment".into(), "fresh".into())];
        let edits = string_edits(vec![
            StringOp::SetTag {
                index: 0,
                key: "A".into(),
                to: String::new(),
            },
            StringOp::SetMetaData {
                index: 0,
                key: "A".into(),
                id: "Comment".into(),
                to: "changed".into(),
            },
            StringOp::SetMetaData {
                index: 0,
                key: "A".into(),
                id: "Note".into(),
                to: "n".into(),
            },
            StringOp::SetMetaData {
                index: 1,
                key: "B".into(),
                id: "Comment".into(),
                to: "fresh".into(),
            },
        ]);
        verify_strings(0, &was, &is, &edits).expect("replayed");
        is.entries[0].metadata.pop();
        let err = verify_strings(0, &was, &is, &edits).expect_err("drift");
        assert!(err.contains("entry A"), "{err}");
    }

    #[test]
    fn each_string_edit_becomes_one_splice_on_the_entry_it_addresses() {
        let (parsed, _data) = string_package();
        let edits = string_edits(vec![
            StringOp::SetSource {
                index: 1,
                key: "B".into(),
                to: "buzz".into(),
            },
            StringOp::SetKey {
                index: 0,
                key: "A".into(),
                to: "Alpha".into(),
            },
            StringOp::Add {
                key: "C".into(),
                source: "sea".into(),
            },
            StringOp::Remove {
                index: 1,
                key: "B".into(),
            },
        ]);
        let mut names = name_map();
        let err = string_splices(&parsed, &edits, &mut names).expect_err("removed entry");
        assert!(err.contains("B is being removed"), "{err}");

        let edits = string_edits(vec![
            StringOp::SetSource {
                index: 0,
                key: "A".into(),
                to: "alpha".into(),
            },
            StringOp::SetKey {
                index: 0,
                key: "A".into(),
                to: "Alpha".into(),
            },
            StringOp::Add {
                key: "C".into(),
                source: "sea".into(),
            },
            StringOp::Remove {
                index: 1,
                key: "B".into(),
            },
        ]);
        let out = string_splices(&parsed, &edits, &mut names)
            .expect("splices")
            .edits;
        let layout = &parsed.string_tables[0];
        let spans: Vec<(u64, u64)> = out
            .iter()
            .map(|(splice, _)| (splice.start, splice.end))
            .collect();
        assert_eq!(
            spans,
            vec![
                layout.entries[0].source,
                layout.entries[0].key,
                (layout.trailer_at, layout.trailer_at),
                (layout.entries[1].key.0, layout.entries[1].end),
            ]
        );
        assert_eq!(out[0].0.bytes, encode_string("alpha"));
        let mut added = encode_string("C");
        added.extend(encode_string("sea"));
        added.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(out[2].0.bytes, added);
        assert!(out[3].0.bytes.is_empty());
        assert_eq!(out[3].1.after, "(removed)");

        let stale = string_edits(vec![StringOp::SetSource {
            index: 0,
            key: "Q".into(),
            to: "x".into(),
        }]);
        let err = string_splices(&parsed, &stale, &mut names).expect_err("stale");
        assert!(err.contains("entry 0 is A here, not Q"), "{err}");
        let clash = string_edits(vec![StringOp::Add {
            key: "B".into(),
            source: "again".into(),
        }]);
        let err = string_splices(&parsed, &clash, &mut names).expect_err("clash");
        assert!(err.contains("already has an entry keyed B"), "{err}");
    }

    /// A header of two exports at the given data offsets, the second a subobject of the first,
    /// with the second depending on the first before it serializes.
    fn two_export_header(sizes: [i64; 2]) -> retoc::legacy_asset::FLegacyPackageHeader {
        let mut header = retoc::legacy_asset::FLegacyPackageHeader {
            name_map: name_map(),
            ..Default::default()
        };
        header.summary.versioning_info.total_header_size = 0x100;
        header.exports = vec![
            FObjectExport {
                object_name: retoc::legacy_asset::FMinimalName {
                    index: 1,
                    number: 0,
                },
                serial_offset: 0x100,
                serial_size: sizes[0],
                first_export_dependency_index: -1,
                ..Default::default()
            },
            FObjectExport {
                object_name: retoc::legacy_asset::FMinimalName {
                    index: 3,
                    number: 0,
                },
                outer_index: FPackageIndex::create_export(0),
                serial_offset: 0x100 + sizes[0],
                serial_size: sizes[1],
                first_export_dependency_index: 0,
                create_before_serialize_dependencies: 1,
                ..Default::default()
            },
        ];
        header.preload_dependencies = vec![FPackageIndex::create_export(0)];
        header
    }

    fn two_export_package() -> ParsedPackage {
        let mut parsed = export_with(Vec::new());
        parsed.info.unversioned_properties = true;
        parsed.exports[0].object_name = "Damage".into();
        parsed.exports[0].path = "/Game/Test.Damage".into();
        parsed.exports[0].class_name = "Thing".into();
        parsed.exports[0].serial_offset = 0x100;
        parsed.exports[0].serial_size = 8;
        let mut sub = parsed.exports[0].clone();
        sub.index = 1;
        sub.object_name = "BP_Hero".into();
        sub.path = "/Game/Test.Damage:BP_Hero".into();
        sub.outer_index = 1;
        sub.serial_offset = 0x108;
        parsed.exports.push(sub);
        parsed.references = vec![crate::props::IndexRef {
            at: 0x108 + 4,
            index: 1,
        }];
        parsed
    }

    /// The root and its subobject are copied to the end of the table: the root under the new name
    /// and its old outer, the subobject under the root's copy, the reference between them pointed
    /// at the copy, and the dependency run copied with the same remap.
    #[test]
    fn duplicating_an_export_copies_its_set_and_repoints_the_references_inside_it() {
        let parsed = two_export_package();
        let header = two_export_header([8, 8]);
        let mut data: Vec<u8> = (0u8..16).collect();
        data[12..16].copy_from_slice(&1i32.to_le_bytes());
        let plans = crate::plan_duplication(
            &parsed,
            &[DuplicateExport {
                export: 0,
                name: "Copy".into(),
                into_level: None,
            }],
        )
        .expect("plan");
        assert_eq!(plans[0].members, vec![0, 1]);
        assert_eq!(plans[0].copies, vec![2, 3]);
        let mut names = name_map();
        let out = crate::duplicate::duplicate_exports(&parsed, &header, &data, &mut names, &plans)
            .expect("duplicate");
        assert_eq!(out.exports.len(), 4);
        let root = &out.exports[2];
        assert_eq!(
            names.get(root.object_name).ok().map(|n| n.into_owned()),
            Some("Copy".to_string())
        );
        assert_eq!(root.outer_index, FPackageIndex::create_null());
        assert_eq!(root.serial_offset, 0x110);
        let sub = &out.exports[3];
        assert_eq!(sub.outer_index, FPackageIndex::create_export(2));
        assert_eq!(sub.serial_offset, 0x118);
        assert_eq!(sub.first_export_dependency_index, 1);
        assert_eq!(sub.create_before_serialize_dependencies, 1);
        assert_eq!(
            out.preload_dependencies,
            vec![
                FPackageIndex::create_export(0),
                FPackageIndex::create_export(2)
            ]
        );
        assert_eq!(&out.appended[..8], &data[..8]);
        assert_eq!(&out.appended[8..12], &data[8..12]);
        assert_eq!(
            &out.appended[12..16],
            &3i32.to_le_bytes(),
            "the reference points at the copy"
        );
        assert_eq!(out.applied.len(), 1);
    }

    #[test]
    fn duplication_refuses_what_it_could_not_copy_faithfully() {
        let request = |export: u32, name: &str| DuplicateExport {
            export,
            name: name.into(),
            into_level: None,
        };
        let parsed = two_export_package();
        let err = crate::plan_duplication(&parsed, &[request(1, "BP_Hero")]).expect_err("sibling");
        assert!(err.contains("already"), "{err}");
        let err = crate::plan_duplication(&parsed, &[request(0, "")]).expect_err("empty");
        assert!(err.contains("needs a name"), "{err}");
        let err = crate::plan_duplication(&parsed, &[request(5, "X")]).expect_err("range");
        assert!(err.contains("no export 5"), "{err}");

        let mut cdo = two_export_package();
        cdo.exports[1].object_flags = 0x10;
        let err = crate::plan_duplication(&cdo, &[request(0, "X")]).expect_err("cdo");
        assert!(err.contains("class default object"), "{err}");

        let mut class = two_export_package();
        class.exports[0].class_name = "BlueprintGeneratedClass".into();
        let err = crate::plan_duplication(&class, &[request(0, "X")]).expect_err("class");
        assert!(err.contains("layout is not copied"), "{err}");

        let mut tagged = two_export_package();
        tagged.info.unversioned_properties = false;
        let err = crate::plan_duplication(&tagged, &[request(0, "X")]).expect_err("tagged");
        assert!(err.contains("tagged"), "{err}");

        let mut opaque = two_export_package();
        opaque.exports[1].status = ExportStatus::Payload {
            consumed: 4,
            payload_bytes: 4,
            kind: "bytecode",
        };
        let err = crate::plan_duplication(&opaque, &[request(0, "X")]).expect_err("opaque");
        assert!(err.contains("does not decode"), "{err}");
    }

    /// A replacement that disassembles knows the space it will take once loaded, so it may be any
    /// length and the two words in front of the script are rewritten to match.
    /// A script constant becomes one splice over its value bytes and nothing else; the wrong
    /// index, a form with no value bytes, and a payload for the same export are all refused.
    #[test]
    fn a_script_constant_is_spliced_at_its_own_width() {
        let mut names = FPackageNameMap::create();
        let mut parsed = two_export_package();
        parsed.exports[1].status = ExportStatus::Payload {
            consumed: 4,
            payload_bytes: 14,
            kind: "bytecode",
        };
        let start = parsed.exports[1].serial_offset as u64 + 4;
        let call = |literal: Expr| Expr::FinalCall {
            name: "LocalFinalFunction",
            function: crate::kismet::ObjectRef {
                index: -1,
                path: None,
            },
            params: vec![literal],
        };
        let script = |literal: Expr| crate::kismet::Script {
            buffer_size: 18,
            storage_size: 14,
            decoded_size: 18,
            sizes_at: start - 8,
            start,
            end: start + 14,
            statements: vec![crate::kismet::Statement {
                offset: 0,
                at: start,
                expr: call(literal),
            }],
            stopped: None,
        };
        parsed.exports[1].script = Some(script(Expr::IntConst {
            value: 411,
            at: start + 5,
        }));

        let edits = PackageEdits {
            scripts: vec![ScriptConstEdit {
                export: 1,
                statement: 0,
                constant: 0,
                value: "1000".to_string(),
            }],
            ..Default::default()
        };
        let out = script_splices(&parsed, &edits, &mut names).expect("one splice");
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].0.start, out[0].0.end), (start + 6, start + 10));
        assert_eq!(out[0].0.bytes, 1000i32.to_le_bytes().to_vec());
        assert_eq!(
            (out[0].1.before.as_str(), out[0].1.after.as_str()),
            ("411", "1000")
        );
        assert_eq!(out[0].1.offset, start + 5);

        let wrong = PackageEdits {
            scripts: vec![ScriptConstEdit {
                export: 1,
                statement: 0,
                constant: 1,
                value: "1".to_string(),
            }],
            ..Default::default()
        };
        let err = script_splices(&parsed, &wrong, &mut names).expect_err("no second literal");
        assert!(err.contains("[0] IntConst 411"), "{err}");

        let with_payload = PackageEdits {
            payloads: vec![PayloadEdit {
                export: 1,
                bytes: vec![0x53],
            }],
            ..edits.clone()
        };
        let err = script_splices(&parsed, &with_payload, &mut names).expect_err("payload too");
        assert!(err.contains("whole new payload"), "{err}");

        parsed.exports[1].script = Some(script(Expr::Simple { name: "IntZero" }));
        let err = script_splices(&parsed, &edits, &mut names).expect_err("no value bytes");
        assert!(err.contains("one-byte form"), "{err}");
    }

    #[test]
    fn bytecode_that_disassembles_is_replaced_at_any_length() {
        let header = retoc::legacy_asset::FLegacyPackageHeader::default();
        let mut parsed = two_export_package();
        parsed.exports[1].status = ExportStatus::Payload {
            consumed: 4,
            payload_bytes: 4,
            kind: "bytecode",
        };
        let start = parsed.exports[1].serial_offset as u64 + 4;
        parsed.exports[1].script = Some(crate::kismet::Script {
            buffer_size: 4,
            storage_size: 4,
            decoded_size: 4,
            sizes_at: start - 8,
            start,
            end: start + 4,
            statements: Vec::new(),
            stopped: None,
        });

        // Four Nothings and the end marker: five bytes stored and five once loaded.
        let longer = PackageEdits {
            payloads: vec![PayloadEdit {
                export: 1,
                bytes: vec![0x0B, 0x0B, 0x0B, 0x0B, 0x53],
            }],
            ..Default::default()
        };
        let out = payload_splices(&parsed, &longer, &header).expect("any length");
        assert_eq!(out.len(), 2, "the size words and the script itself");
        assert_eq!((out[0].0.start, out[0].0.end), (start - 8, start));
        assert_eq!(out[0].0.bytes, vec![5, 0, 0, 0, 5, 0, 0, 0]);
        assert_eq!((out[1].0.start, out[1].0.end), (start, start + 4));
        assert!(
            out[1].1.after.contains("size words follow"),
            "{:?}",
            out[1].1
        );

        // A replacement that does not disassemble keeps the old rule.
        let garbage = PackageEdits {
            payloads: vec![PayloadEdit {
                export: 1,
                bytes: vec![9; 5],
            }],
            ..Default::default()
        };
        let err = payload_splices(&parsed, &garbage, &header).expect_err("unknown size");
        assert!(err.contains("does not disassemble"), "{err}");
    }

    /// Measured bytecode can be swapped at its own length; a layout the walk could not follow
    /// leaves its bytecode without a start and stays locked.
    #[test]
    fn bytecode_is_replaced_at_its_own_length_and_an_unwalked_layout_stays_locked() {
        let mut parsed = two_export_package();
        parsed.exports[1].status = ExportStatus::Payload {
            consumed: 4,
            payload_bytes: 4,
            kind: "bytecode",
        };
        assert!(payload_lock(&parsed.exports[1], &[]).is_none());
        let start = parsed.exports[1].serial_offset as u64 + 4;
        let same = PackageEdits {
            payloads: vec![PayloadEdit {
                export: 1,
                bytes: vec![9, 9, 9, 9],
            }],
            ..Default::default()
        };
        let out = payload_splices(
            &parsed,
            &same,
            &retoc::legacy_asset::FLegacyPackageHeader::default(),
        )
        .expect("same length");
        assert_eq!((out[0].0.start, out[0].0.end), (start, start + 4));
        assert!(out[0].1.after.contains("embeds package indices"));
        let longer = PackageEdits {
            payloads: vec![PayloadEdit {
                export: 1,
                bytes: vec![9; 5],
            }],
            ..Default::default()
        };
        let err = payload_splices(
            &parsed,
            &longer,
            &retoc::legacy_asset::FLegacyPackageHeader::default(),
        )
        .expect_err("length");
        assert!(err.contains("own length, 4 bytes"), "{err}");

        parsed.exports[1].status = ExportStatus::Payload {
            consumed: 4,
            payload_bytes: 4,
            kind: "function layout and bytecode",
        };
        let reason = payload_lock(&parsed.exports[1], &[]).expect("locked");
        assert!(reason.contains("no known start"), "{reason}");
        assert!(crate::remove::INDEX_BEARING.contains(&"function layout and bytecode"));
    }

    /// Pointing an export at another one it never depended on adds the edge the loader needs, once,
    /// and never from an export to itself.
    #[test]
    fn an_object_edit_gains_a_create_before_serialize_dependency() {
        let header = two_export_header([8, 8]);
        let (exports, dependencies) =
            add_serialize_dependencies(&header, &[(0, 1), (1, 0), (1, 1)])
                .expect("rebuilt")
                .expect("an edge was missing");
        assert_eq!(exports[0].first_export_dependency_index, 0);
        assert_eq!(exports[0].create_before_serialize_dependencies, 1);
        assert_eq!(exports[1].first_export_dependency_index, 1);
        assert_eq!(
            exports[1].create_before_serialize_dependencies, 1,
            "the edge it had is kept once"
        );
        assert_eq!(
            dependencies,
            vec![
                FPackageIndex::create_export(1),
                FPackageIndex::create_export(0)
            ]
        );
        assert!(
            add_serialize_dependencies(&header, &[(1, 0)])
                .expect("rebuilt")
                .is_none(),
            "an edge already there adds nothing"
        );
    }

    fn resource(flags: u32, offset: i64, size: i64) -> retoc::legacy_asset::FObjectDataResource {
        retoc::legacy_asset::FObjectDataResource {
            legacy_bulk_data_flags: flags,
            serial_offset: offset,
            duplicate_serial_offset: -1,
            serial_size: size,
            raw_size: size,
            ..Default::default()
        }
    }

    /// The entries of a sidecar tile it; a replaced payload of another width moves the entries
    /// after it and the table follows. A file the entries do not tile is refused, as is a
    /// compressed, memory-mapped or duplicated payload.
    #[test]
    fn a_sidecar_is_rewritten_around_the_replaced_payload() {
        let file: Vec<u8> = (0u8..35).collect();
        let mut table = vec![
            resource(crate::write::IN_SEPARATE_FILE, 0, 10),
            resource(0, 12, 4),
            resource(crate::write::IN_SEPARATE_FILE, 10, 20),
            resource(crate::write::IN_SEPARATE_FILE, 30, 5),
            resource(crate::write::OPTIONAL_PAYLOAD, 0, 3),
        ];
        let fresh = [9u8; 24];
        let out = patch_sidecar(
            &file,
            &mut table,
            crate::write::IN_SEPARATE_FILE,
            &[(2, &fresh)],
        )
        .expect("rewritten");
        assert_eq!(out.len(), 39);
        assert_eq!(&out[..10], &file[..10]);
        assert_eq!(&out[10..34], &fresh);
        assert_eq!(&out[34..], &file[30..]);
        assert_eq!(table[2].serial_size, 24);
        assert_eq!(table[2].raw_size, 24);
        assert_eq!(table[3].serial_offset, 34);
        assert_eq!(table[1].serial_offset, 12, "an inline entry is not touched");
        assert_eq!(
            table[4].serial_offset, 0,
            "another file's entry is not touched"
        );

        let mut gapped = vec![
            resource(crate::write::IN_SEPARATE_FILE, 0, 10),
            resource(crate::write::IN_SEPARATE_FILE, 12, 23),
        ];
        let err = patch_sidecar(
            &file,
            &mut gapped,
            crate::write::IN_SEPARATE_FILE,
            &[(0, &fresh)],
        )
        .expect_err("gap");
        assert!(err.contains("does not follow"), "{err}");
        let mut short = vec![resource(crate::write::IN_SEPARATE_FILE, 0, 10)];
        let err = patch_sidecar(
            &file,
            &mut short,
            crate::write::IN_SEPARATE_FILE,
            &[(0, &fresh)],
        )
        .expect_err("short");
        assert!(err.contains("cover 10 bytes"), "{err}");

        let compressed = resource(crate::write::IN_SEPARATE_FILE | 0x2, 0, 10);
        assert!(crate::write::bulk_lock(&compressed).is_some());
        let mapped = resource(crate::write::MEMORY_MAPPED, 0, 10);
        assert!(crate::write::bulk_lock(&mapped).is_some());
        let mut duplicated = resource(crate::write::IN_SEPARATE_FILE | 0x4000, 0, 10);
        assert!(crate::write::bulk_lock(&duplicated).is_some());
        duplicated.legacy_bulk_data_flags = crate::write::IN_SEPARATE_FILE;
        duplicated.raw_size = 12;
        assert!(
            crate::write::bulk_lock(&duplicated).is_some(),
            "raw and serial sizes differ"
        );
        assert!(
            crate::write::bulk_lock(&resource(crate::write::IN_SEPARATE_FILE, 0, 10)).is_none()
        );
    }

    /// A float channel with keys at the given frames and values, laid out as the reader records
    /// it, in an export whose properties hold the channel entry.
    fn channel_package(keys: &[(i32, f32)]) -> (ParsedPackage, Vec<u8>) {
        let at = 0x100u64;
        let mut data = vec![4u8, 4u8];
        data.extend_from_slice(&4i32.to_le_bytes());
        let times_count_at = at + data.len() as u64;
        data.extend_from_slice(&(keys.len() as i32).to_le_bytes());
        let mut times = Vec::new();
        for (frame, _) in keys {
            times.push(at + data.len() as u64);
            data.extend_from_slice(&frame.to_le_bytes());
        }
        data.extend_from_slice(&28i32.to_le_bytes());
        let values_count_at = at + data.len() as u64;
        data.extend_from_slice(&(keys.len() as i32).to_le_bytes());
        let mut values = Vec::new();
        let mut entries = Vec::new();
        for (index, (frame, value)) in keys.iter().enumerate() {
            let start = at + data.len() as u64;
            data.extend_from_slice(&value.to_le_bytes());
            data.extend_from_slice(&[0u8; 16]);
            data.extend_from_slice(&[0, 0, 0, 0, CUBIC_INTERPOLATION, 0, 0, 0]);
            values.push((start, at + data.len() as u64));
            entries.push(PropertyEntry {
                name: "Keys".into(),
                element: Some(index as u32),
                value: PropertyValue::Struct {
                    name: "MovieSceneFloatKey".into(),
                    fields: vec![
                        entry(
                            "Time",
                            PropertyValue::Int {
                                value: i64::from(*frame),
                            },
                            None,
                        ),
                        entry(
                            "Value",
                            PropertyValue::Float {
                                value: f64::from(*value),
                            },
                            None,
                        ),
                    ],
                },
                span: None,
                slot: None,
            });
        }
        data.extend_from_slice(&[0u8; 4 + 4]);
        data.extend_from_slice(&24000i32.to_le_bytes());
        data.extend_from_slice(&1i32.to_le_bytes());
        data.extend_from_slice(&[0u8; 4]);
        let end = at + data.len() as u64;
        let mut parsed = export_with(vec![entry(
            "FloatCurve",
            PropertyValue::Struct {
                name: "MovieSceneFloatChannel".into(),
                fields: entries,
            },
            Some((at, end)),
        )]);
        parsed.channels = vec![crate::props::ChannelLayout {
            at,
            times_count_at,
            times,
            frames: keys.iter().map(|(frame, _)| *frame).collect(),
            values_count_at,
            values,
            value_bytes: 28,
        }];
        (parsed, data)
    }

    fn key_edit(op: KeyOp) -> KeyEdit {
        KeyEdit {
            offset: 0x100,
            expect_name: "FloatCurve".into(),
            expect_element: None,
            op,
        }
    }

    /// A zero struct stored from nothing keeps every field zero; the empty form it would get as an
    /// unset one would leave each field to its archetype.
    #[test]
    fn a_zero_struct_is_stored_with_its_fields_zero() {
        let slot = crate::value::SlotRef {
            header_at: 0x10,
            schema_index: 2,
            declared: "Struct",
        };
        let mut parsed = export_with(vec![PropertyEntry {
            name: "Params".into(),
            element: None,
            value: PropertyValue::Default { fields: Vec::new() },
            span: Some((0x40, 0x40)),
            slot: Some(slot),
        }]);
        let zero = crate::unversioned::zero_header(3).expect("zero header");
        parsed.unset = vec![crate::props::UnsetSlot {
            at: 0x40,
            header_at: 0x10,
            schema_index: 2,
            declared: "Struct",
            struct_name: Some("Params".into()),
            default_bytes: Some(crate::unversioned::empty_header(3)),
            default_recipe: None,
            zero_bytes: Some(zero.clone()),
        }];
        let mut names = FPackageNameMap::create_from_names(vec!["None".into()]);
        let entry = parsed.exports[0].properties[0].clone();
        assert_eq!(
            stored_default(&parsed, &entry, &mut names).expect("stored"),
            zero
        );
    }

    /// A field set expects its struct to store nothing yet; one that now stores something is
    /// refused rather than written over.
    #[test]
    fn a_field_set_on_a_struct_stored_since_is_refused_as_drift() {
        let set = FieldSet {
            offset: 0x40,
            expect_name: "Where".into(),
            expect_element: None,
            path: vec!["X".into()],
            text: "1".into(),
        };
        let unset = export_with(vec![entry(
            "Where",
            PropertyValue::Unset {
                declared: "Struct",
                enum_type: None,
                fields: Vec::new(),
            },
            Some((0x40, 0x40)),
        )]);
        let mut edits = PackageEdits {
            field_sets: vec![set],
            ..Default::default()
        };
        edits.expect = expectations(&unset, &edits);
        assert_eq!(
            edits.expect.values.get("64").map(String::as_str),
            Some(NOT_STORED)
        );
        check_expectations(&unset, &edits).expect("still unset");

        let stored = export_with(vec![entry(
            "Where",
            PropertyValue::Struct {
                name: "Vector".into(),
                fields: Vec::new(),
            },
            Some((0x40, 0x58)),
        )]);
        let err = check_expectations(&stored, &edits).expect_err("stored since");
        assert!(err.contains("Where was not stored"), "{err}");
    }

    /// A key edit names the channel it was made for, and a different channel now at that offset is
    /// refused rather than edited.
    #[test]
    fn a_key_edit_for_another_channel_is_refused() {
        let (parsed, data) = channel_package(&[(0, 1.0), (20000, 2.0)]);
        let bundle = AssetBundle {
            asset: &[],
            exports: &data,
        };
        let mut edit = key_edit(KeyOp::Remove { index: 0 });
        edit.expect_name = "OtherCurve".into();
        let err = key_splices(
            &parsed,
            &bundle,
            0x100,
            &PackageEdits {
                keys: vec![edit],
                ..Default::default()
            },
        )
        .expect_err("refused");
        assert!(err.contains("no channel called OtherCurve"), "{err}");
    }

    /// A key between two others goes into both arrays at the same position with the previous
    /// key's block and the new value; a copy lands where its frame sorts; a removal takes both
    /// words out. Frames already keyed, out-of-range keys and doubled removals are refused.
    #[test]
    fn key_edits_splice_the_frame_and_the_value_together() {
        let (parsed, data) = channel_package(&[(0, 1.0), (20000, 2.0)]);
        let bundle = AssetBundle {
            asset: &[],
            exports: &data,
        };
        let layout = &parsed.channels[0];
        let edits = PackageEdits {
            keys: vec![
                key_edit(KeyOp::Add {
                    time: 10000,
                    value: 1.5,
                }),
                key_edit(KeyOp::Duplicate {
                    index: 1,
                    time: 30000,
                }),
                key_edit(KeyOp::Remove { index: 0 }),
            ],
            ..Default::default()
        };
        let out = key_splices(&parsed, &bundle, 0x100, &edits).expect("splices");
        let spans: Vec<(u64, u64)> = out.splices.iter().map(|s| (s.start, s.end)).collect();
        let times_end = layout.times[1] + 4;
        let values_end = layout.values[1].1;
        assert_eq!(
            spans,
            vec![
                (layout.times[0], layout.times[0] + 4),
                layout.values[0],
                (layout.times[1], layout.times[1]),
                (layout.values[1].0, layout.values[1].0),
                (times_end, times_end),
                (values_end, values_end),
            ],
            "the removal first, then the adds in frame order"
        );
        assert_eq!(out.splices[2].bytes, 10000i32.to_le_bytes());
        let mut block = 1.5f32.to_le_bytes().to_vec();
        block.extend_from_slice(&[0u8; 16]);
        block.extend_from_slice(&[0, 0, 0, 0, CUBIC_INTERPOLATION, 0, 0, 0]);
        assert_eq!(
            out.splices[3].bytes, block,
            "key 0's block with the new value"
        );
        assert_eq!(out.splices[4].bytes, 30000i32.to_le_bytes());
        assert_eq!(out.splices[5].bytes[..4], 2.0f32.to_le_bytes());
        assert_eq!(
            out.counts,
            vec![
                (layout.times_count_at, -1),
                (layout.values_count_at, -1),
                (layout.times_count_at, 1),
                (layout.values_count_at, 1),
                (layout.times_count_at, 1),
                (layout.values_count_at, 1),
            ]
        );
        assert_eq!(out.applied.len(), 3);

        let taken = PackageEdits {
            keys: vec![key_edit(KeyOp::Add {
                time: 20000,
                value: 0.0,
            })],
            ..Default::default()
        };
        let err = key_splices(&parsed, &bundle, 0x100, &taken).expect_err("frame taken");
        assert!(err.contains("already has a key at frame 20000"), "{err}");
        let far = PackageEdits {
            keys: vec![key_edit(KeyOp::Remove { index: 2 })],
            ..Default::default()
        };
        let err = key_splices(&parsed, &bundle, 0x100, &far).expect_err("no key 2");
        assert!(err.contains("no key 2"), "{err}");
        let twice = PackageEdits {
            keys: vec![
                key_edit(KeyOp::Remove { index: 1 }),
                key_edit(KeyOp::Remove { index: 1 }),
            ],
            ..Default::default()
        };
        let err = key_splices(&parsed, &bundle, 0x100, &twice).expect_err("twice");
        assert!(err.contains("removed or moved twice"), "{err}");

        let (empty, data) = channel_package(&[]);
        let bundle = AssetBundle {
            asset: &[],
            exports: &data,
        };
        let first = PackageEdits {
            keys: vec![key_edit(KeyOp::Add {
                time: 5,
                value: 0.25,
            })],
            ..Default::default()
        };
        let out = key_splices(&empty, &bundle, 0x100, &first).expect("first key");
        assert_eq!(out.splices[1].bytes[..4], 0.25f32.to_le_bytes());
        assert_eq!(
            out.splices[1].bytes[24], CUBIC_INTERPOLATION,
            "a fresh key is cubic"
        );
        assert_eq!(out.splices[1].bytes.len(), 28);
    }

    /// A frame word edited as a value may only move within the gap its neighbours leave.
    #[test]
    fn a_retimed_key_stays_between_its_neighbours() {
        let (parsed, _) = channel_package(&[(0, 1.0), (10, 2.0), (20, 3.0)]);
        let layout = &parsed.channels[0];
        check_frame_order(layout, 1, &15i32.to_le_bytes(), "Keys[1].Time").expect("in the gap");
        check_frame_order(layout, 0, &(-5i32).to_le_bytes(), "Keys[0].Time").expect("in front");
        check_frame_order(layout, 2, &99i32.to_le_bytes(), "Keys[2].Time").expect("at the back");
        for (slot, frame) in [(1usize, 20i32), (1, 0), (0, 10), (2, 5)] {
            let err = check_frame_order(layout, slot, &frame.to_le_bytes(), "Keys.Time")
                .expect_err("out of order");
            assert!(err.contains("out of order"), "{err}");
        }
        assert_eq!(
            channel_time_at(&parsed, layout.times[2]).map(|(_, slot)| slot),
            Some(2)
        );
        assert!(channel_time_at(&parsed, layout.times[2] + 1).is_none());

        let retime = PackageEdits {
            values: vec![ValueEdit {
                offset: layout.times[1],
                expect_name: "Time".into(),
                expect_element: None,
                expect_kind: "int".into(),
                op: EditOp::Set { text: "15".into() },
            }],
            ..Default::default()
        };
        let (moved, _) = channel_package(&[(0, 1.0), (15, 2.0), (20, 3.0)]);
        verify_keys(&parsed, &moved, &retime).expect("retimed in place");
        let (disordered, _) = channel_package(&[(0, 1.0), (25, 2.0), (20, 3.0)]);
        let err = verify_keys(&parsed, &disordered, &retime).expect_err("disordered");
        assert!(err.contains("was expected"), "{err}");
    }

    /// A move takes the key's frame and block out of their slots and puts them where the new
    /// frame sorts; the counts do not change. The verification reads the value back at the new
    /// frame, and refuses a move onto a frame that has a key or a key moved and removed at once.
    #[test]
    fn a_moved_key_carries_its_value_to_where_its_frame_sorts() {
        let (parsed, data) = channel_package(&[(0, 1.0), (10, 2.0), (20, 3.0)]);
        let bundle = AssetBundle {
            asset: &[],
            exports: &data,
        };
        let layout = &parsed.channels[0];
        let edits = PackageEdits {
            keys: vec![key_edit(KeyOp::Move { index: 0, time: 15 })],
            ..Default::default()
        };
        let out = key_splices(&parsed, &bundle, 0x100, &edits).expect("moved");
        assert!(out.counts.is_empty(), "a move changes no count");
        assert_eq!(out.splices.len(), 4);
        assert_eq!(
            out.splices[0].start, layout.times[2],
            "the frame goes in front of 20"
        );
        assert_eq!(out.splices[0].bytes, 15i32.to_le_bytes());
        assert_eq!(out.splices[1].start, layout.values[2].0);
        assert_eq!(
            out.splices[1].bytes[..4],
            1.0f32.to_le_bytes(),
            "the block travels"
        );
        assert_eq!(
            (out.splices[2].start, out.splices[2].end),
            (layout.times[0], layout.times[0] + 4)
        );
        assert_eq!((out.splices[3].start, out.splices[3].end), layout.values[0]);
        assert_eq!(out.applied.len(), 1);

        let (after, _) = channel_package(&[(10, 2.0), (15, 1.0), (20, 3.0)]);
        verify_keys(&parsed, &after, &edits).expect("value travelled");
        let (wrong, _) = channel_package(&[(10, 2.0), (15, 2.0), (20, 3.0)]);
        let err = verify_keys(&parsed, &wrong, &edits).expect_err("value lost");
        assert!(err.contains("reads 2"), "{err}");

        let taken = PackageEdits {
            keys: vec![key_edit(KeyOp::Move { index: 0, time: 10 })],
            ..Default::default()
        };
        let err = key_splices(&parsed, &bundle, 0x100, &taken).expect_err("taken");
        assert!(err.contains("already has a key at frame 10"), "{err}");
        let twice = PackageEdits {
            keys: vec![
                key_edit(KeyOp::Move { index: 1, time: 5 }),
                key_edit(KeyOp::Remove { index: 1 }),
            ],
            ..Default::default()
        };
        let err = key_splices(&parsed, &bundle, 0x100, &twice).expect_err("twice");
        assert!(err.contains("removed or moved twice"), "{err}");
    }

    #[test]
    fn hard_object_kinds_take_a_dependency_edge_and_soft_ones_do_not() {
        assert!(is_object_kind("Object") && is_object_kind("Interface"));
        assert!(!is_object_kind("SoftObject") && !is_object_kind("WeakObject"));
        assert_eq!(export_target(&3i32.to_le_bytes()), Some(2));
        assert_eq!(export_target(&(-3i32).to_le_bytes()), None);
        assert_eq!(export_target(&[0, 0, 0, 0]), None);
        assert_eq!(export_target(&[1, 0]), None);
    }

    /// Verification replays the edits over the frames and reads the added key's value back.
    #[test]
    fn verification_replays_the_key_edits_over_the_channel() {
        let (before, _) = channel_package(&[(0, 1.0), (20000, 2.0)]);
        let edits = PackageEdits {
            keys: vec![
                key_edit(KeyOp::Add {
                    time: 10000,
                    value: 1.5,
                }),
                key_edit(KeyOp::Remove { index: 1 }),
            ],
            ..Default::default()
        };
        let (after, _) = channel_package(&[(0, 1.0), (10000, 1.5)]);
        verify_keys(&before, &after, &edits).expect("replayed");
        let (wrong_value, _) = channel_package(&[(0, 1.0), (10000, 1.75)]);
        let err = verify_keys(&before, &wrong_value, &edits).expect_err("value");
        assert!(err.contains("reads 1.75"), "{err}");
        let (wrong_frames, _) = channel_package(&[(0, 1.0), (10000, 1.5), (20000, 2.0)]);
        let err = verify_keys(&before, &wrong_frames, &edits).expect_err("frames");
        assert!(err.contains("was expected"), "{err}");
        let untouched = PackageEdits::default();
        verify_keys(&before, &before, &untouched).expect("nothing to check");
    }

    /// Two payloads, one inside the other. An edit in the inner one moves both lengths, an edit in
    /// the outer one only the outer, and an edit before either moves neither.
    #[test]
    fn every_enclosing_instanced_struct_takes_the_delta_of_an_edit_inside_it() {
        let outer = InstancedLayout {
            size_at: 0x100,
            payload_start: 0x104,
            payload_end: 0x200,
        };
        let inner = InstancedLayout {
            size_at: 0x150,
            payload_start: 0x154,
            payload_end: 0x180,
        };
        let splices = vec![
            splice_at(0x0F0, 0x0F4, 8),  // outside both, +4
            splice_at(0x110, 0x114, 2),  // outer only, -2
            splice_at(0x160, 0x164, 10), // inside both, +6
            splice_at(0x180, 0x180, 3),  // on the inner end: inner and outer, +3
        ];
        let mut deltas = prefix_deltas(&[outer, inner], &splices);
        deltas.sort();
        assert_eq!(deltas, vec![(0x100, 7), (0x150, 9)]);
        assert!(prefix_deltas(&[outer], &[splice_at(0x0F0, 0x0F4, 4)]).is_empty());
    }

    #[test]
    fn a_nested_value_is_found_by_its_offset_and_name() {
        let outer = [entry(
            "Location",
            PropertyValue::Struct {
                name: "Vector".into(),
                fields: vec![entry(
                    "X",
                    PropertyValue::Float { value: 1.0 },
                    Some((64, 72)),
                )],
            },
            None,
        )];
        assert_eq!(find_in(&outer, 64, "X", None).expect("found").name, "X");
        assert!(find_in(&outer, 65, "X", None).is_none());
        assert!(find_in(&outer, 64, "Y", None).is_none());
    }

    /// A defaulted value has no bytes, so it starts where the next stored value does. Only the
    /// name separates them, and picking the wrong one would write into an unrelated property.
    #[test]
    fn a_defaulted_value_and_the_value_after_it_are_told_apart_by_name() {
        let entries = [
            entry("Defaulted", PropertyValue::Int { value: 0 }, Some((16, 16))),
            entry("Stored", PropertyValue::Int { value: 9 }, Some((16, 20))),
        ];
        assert_eq!(
            find_in(&entries, 16, "Stored", None).expect("found").span,
            Some((16, 20))
        );
        assert_eq!(
            find_in(&entries, 16, "Defaulted", None)
                .expect("found")
                .span,
            Some((16, 16))
        );
    }
}
