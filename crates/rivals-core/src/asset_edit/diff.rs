//! Turns an edited JSON dump back into the edit list that produces it.
//!
//! The dump is the readable form of a package; editing it by hand is how a change gets described
//! without knowing byte offsets. What comes back is compared against the parse the dump came from,
//! and every difference becomes the edit that would make it, addressed by the offset the original
//! recorded. That ties an edit list to one source state: an offset from this dump only means
//! anything against this package, and only until an earlier edit moves it.
//!
//! What the comparison cannot express it reports as a note rather than guessing. A note is not a
//! failure: the edits beside it still apply, and the note says what a second dump-edit-diff pass
//! has to pick up.

use serde_json::Value as Json;

use rivals_uasset::{
    EditOp, ImportEdit, ParsedPackage, PropertyEntry, PropertyValue, RowEdit, RowOp, StringEdit,
    StringOp, ValueEdit, kind_of,
};

use super::json::EditList;

/// The edits that turn the original into the edited dump, and everything the comparison could not
/// express as one.
#[derive(Debug, Default)]
pub struct DiffOutcome {
    pub edits: EditList,
    pub notes: Vec<String>,
}

/// Compares an edited dump against the parse it came from. The parse has to be the editor's own,
/// with every declared slot listed, since an edit that stores an unset value is addressed by the
/// offset that slot would occupy.
pub fn diff_dump(original: &ParsedPackage, edited: &Json) -> Result<DiffOutcome, String> {
    let mut out = DiffOutcome::default();
    let exports = edited
        .get("exports")
        .and_then(Json::as_array)
        .ok_or("the edited dump has no exports array")?;
    if exports.len() != original.exports.len() {
        return Err(format!(
            "the edited dump holds {} exports and the package holds {}; adding or removing one is \
             not something a dump edit can say",
            exports.len(),
            original.exports.len()
        ));
    }
    diff_imports(original, edited, &mut out)?;
    for (was, is) in original.exports.iter().zip(exports) {
        let at = was.index;
        for (field, held) in [
            ("object_name", was.object_name.as_str()),
            ("class_name", was.class_name.as_str()),
        ] {
            let now = is.get(field).and_then(Json::as_str).unwrap_or(held);
            if now != held {
                return Err(format!(
                    "export {at}'s {field} reads {now} in the edited dump and {held} in the \
                     package; that is an export table edit, not a value edit"
                ));
            }
        }
        let Some(properties) = is.get("properties").and_then(Json::as_array) else {
            continue;
        };
        diff_entries(&was.properties, properties, at, &was.path, &mut out);
        diff_rows(was, is, at, &mut out);
        diff_strings(was, is, at, &mut out);
    }
    Ok(out)
}

/// An import whose path moved is retargeted, and one appended to the table is added. Imports the
/// edited dump drops are refused: a removal is addressed by table position, and a shorter list
/// does not say which position went.
fn diff_imports(
    original: &ParsedPackage,
    edited: &Json,
    out: &mut DiffOutcome,
) -> Result<(), String> {
    let Some(imports) = edited.get("imports").and_then(Json::as_array) else {
        return Ok(());
    };
    if imports.len() < original.imports.len() {
        return Err(format!(
            "the edited dump holds {} imports and the package holds {}; dropping one is an import \
             removal, which a dump edit cannot address",
            imports.len(),
            original.imports.len()
        ));
    }
    for (position, is) in imports.iter().enumerate() {
        let path = is.get("path").and_then(Json::as_str).unwrap_or_default();
        let class_name = is.get("class_name").and_then(Json::as_str);
        let class_package = is.get("class_package").and_then(Json::as_str);
        match original.imports.get(position) {
            Some(was) if was.path == path => {}
            Some(_) => out.edits.imports.push(ImportEdit::Retarget {
                import: position as u32,
                path: path.to_string(),
                class: class_package
                    .zip(class_name)
                    .map(|(package, name)| (package.to_string(), name.to_string())),
            }),
            None => out.edits.imports.push(ImportEdit::Add {
                path: path.to_string(),
                class_package: class_package.unwrap_or("/Script/CoreUObject").to_string(),
                class_name: class_name.unwrap_or("Object").to_string(),
            }),
        }
    }
    Ok(())
}

/// Rows match by name, case insensitively, the way the table itself keys them. A row the dump
/// gained is added at its position; one it lost is removed. A rename reads as one of each, which
/// is why it is not guessed at.
fn diff_rows(was: &rivals_uasset::ParsedExport, is: &Json, export: u32, out: &mut DiffOutcome) {
    let Some(table) = &was.data_table else {
        return;
    };
    let Some(rows) = is
        .get("data_table")
        .and_then(|t| t.get("rows"))
        .and_then(Json::as_array)
    else {
        return;
    };
    let key = |name: &str| name.to_lowercase();
    let held: std::collections::BTreeMap<String, &rivals_uasset::DataTableRow> =
        table.rows.iter().map(|row| (key(&row.name), row)).collect();
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for (position, row) in rows.iter().enumerate() {
        let name = row.get("name").and_then(Json::as_str).unwrap_or_default();
        seen.insert(key(name));
        match held.get(&key(name)) {
            Some(before) => {
                let Some(fields) = row.get("fields").and_then(Json::as_array) else {
                    continue;
                };
                diff_entries(
                    &before.fields,
                    fields,
                    export,
                    &format!("{}[{name}]", was.path),
                    out,
                );
            }
            None => {
                out.edits.rows.push(RowEdit {
                    export,
                    op: RowOp::Add {
                        name: name.to_string(),
                        at: Some(position as u32),
                    },
                });
                out.notes.push(format!(
                    "{}: row {name} is added with the row struct's defaults; dump the saved copy \
                     and diff again to give its columns values",
                    was.path
                ));
            }
        }
    }
    for row in &table.rows {
        if !seen.contains(&key(&row.name)) {
            out.edits.rows.push(RowEdit {
                export,
                op: RowOp::Remove {
                    name: row.name.clone(),
                },
            });
        }
    }
}

/// String table entries are addressed by position and the key seen there, so they are compared
/// over the common prefix. Beyond it the edited table has entries to add or has dropped some.
fn diff_strings(was: &rivals_uasset::ParsedExport, is: &Json, export: u32, out: &mut DiffOutcome) {
    let Some(table) = &was.string_table else {
        return;
    };
    let Some(entries) = is
        .get("string_table")
        .and_then(|t| t.get("entries"))
        .and_then(Json::as_array)
    else {
        return;
    };
    for (position, entry) in entries.iter().enumerate() {
        let index = position as u32;
        let text = |field: &str| {
            entry
                .get(field)
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let Some(before) = table.entries.get(position) else {
            out.edits.strings.push(StringEdit {
                export,
                op: StringOp::Add {
                    key: text("key"),
                    source: text("source"),
                },
            });
            continue;
        };
        let key = before.key.clone();
        if text("key") != before.key {
            out.edits.strings.push(StringEdit {
                export,
                op: StringOp::SetKey {
                    index,
                    key: key.clone(),
                    to: text("key"),
                },
            });
        }
        if text("source") != before.source {
            out.edits.strings.push(StringEdit {
                export,
                op: StringOp::SetSource {
                    index,
                    key: key.clone(),
                    to: text("source"),
                },
            });
        }
        if text("tag") != before.tag {
            out.edits.strings.push(StringEdit {
                export,
                op: StringOp::SetTag {
                    index,
                    key: key.clone(),
                    to: text("tag"),
                },
            });
        }
        diff_metadata(entry, before, export, index, &key, out);
    }
    for position in (entries.len()..table.entries.len()).rev() {
        out.edits.strings.push(StringEdit {
            export,
            op: StringOp::Remove {
                index: position as u32,
                key: table.entries[position].key.clone(),
            },
        });
    }
}

/// Metadata is a map keyed by id, so it compares by id rather than by position.
fn diff_metadata(
    entry: &Json,
    before: &rivals_uasset::StringTableEntry,
    export: u32,
    index: u32,
    key: &str,
    out: &mut DiffOutcome,
) {
    let mut now: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    if let Some(items) = entry.get("metadata").and_then(Json::as_array) {
        for item in items {
            if let Some(pair) = item.as_array()
                && let (Some(id), Some(value)) = (
                    pair.first().and_then(Json::as_str),
                    pair.get(1).and_then(Json::as_str),
                )
            {
                now.insert(id.to_string(), value.to_string());
            }
        }
    }
    for (id, value) in &now {
        let held = before
            .metadata
            .iter()
            .find(|(name, _)| name == id)
            .map(|(_, held)| held);
        if held != Some(value) {
            out.edits.strings.push(StringEdit {
                export,
                op: StringOp::SetMetaData {
                    index,
                    key: key.to_string(),
                    id: id.clone(),
                    to: value.clone(),
                },
            });
        }
    }
    for (id, _) in &before.metadata {
        if !now.contains_key(id) {
            out.edits.strings.push(StringEdit {
                export,
                op: StringOp::RemoveMetaData {
                    index,
                    key: key.to_string(),
                    id: id.clone(),
                },
            });
        }
    }
}

/// One property list against its edited form. Entries pair by name and element index, which is how
/// a static array's slots stay apart, and how a reordered dump still compares.
fn diff_entries(
    was: &[PropertyEntry],
    is: &[Json],
    export: u32,
    owner: &str,
    out: &mut DiffOutcome,
) {
    for entry in was {
        let element = entry.element;
        let found = is.iter().find(|held| {
            held.get("name").and_then(Json::as_str) == Some(entry.name.as_str())
                && held.get("element").and_then(Json::as_u64).map(|e| e as u32) == element
        });
        let Some(found) = found else {
            out.notes.push(format!(
                "{owner}.{}: the edited dump has no such property, so nothing is changed",
                entry.label()
            ));
            continue;
        };
        let Some(value) = found.get("value") else {
            continue;
        };
        diff_value(entry, value, export, owner, out);
    }
}

/// One value against its edited form, recursing where the value has parts of its own.
fn diff_value(
    entry: &PropertyEntry,
    edited: &Json,
    export: u32,
    owner: &str,
    out: &mut DiffOutcome,
) {
    let was = kind_of(&entry.value);
    let is = edited
        .get("kind")
        .and_then(Json::as_str)
        .unwrap_or_default();
    let label = format!("{owner}.{}", entry.label());
    let at = || entry.span.map(|(start, _)| start);

    if was != is {
        return diff_retyped(entry, edited, export, &label, &was, is, out);
    }
    match &entry.value {
        PropertyValue::Struct { fields, .. } => {
            if let Some(items) = edited.get("fields").and_then(Json::as_array) {
                diff_entries(fields, items, export, &label, out);
            }
        }
        PropertyValue::Text { parts, value } if !parts.is_empty() => {
            let _ = value;
            if let Some(items) = edited.get("parts").and_then(Json::as_array) {
                diff_entries(parts, items, export, &label, out);
            }
        }
        PropertyValue::Array { items } | PropertyValue::Set { items } => {
            diff_items(entry, items, edited, export, &label, out);
        }
        PropertyValue::Map { entries } => {
            let Some(items) = edited.get("entries").and_then(Json::as_array) else {
                return;
            };
            if items.len() != entries.len() {
                out.notes.push(format!(
                    "{label}: the map holds {} entries and the edited dump {}; adding or dropping \
                     one is keyed, so make it in the editor rather than in the dump",
                    entries.len(),
                    items.len()
                ));
                return;
            }
            for (position, (before, now)) in entries.iter().zip(items).enumerate() {
                if let Some(key) = now.get("key")
                    && text_of(key).as_deref() != Some(&before.key.summary())
                {
                    out.notes.push(format!(
                        "{label}: entry {position}'s key changed, which is a remove and an insert \
                         rather than a value edit"
                    ));
                }
                if let Some(value) = now.get("value") {
                    diff_map_value(
                        entry,
                        position as u32,
                        &before.value,
                        value,
                        export,
                        &label,
                        out,
                    );
                }
            }
        }
        PropertyValue::Delegate { .. }
        | PropertyValue::FieldPath { .. }
        | PropertyValue::LazyObject { .. } => {
            if bound_reference(edited).is_some_and(|now| now != entry.value.summary()) {
                out.notes.push(format!(
                    "{label}: a {was} is written by the loader, not stored as text, so a changed \
                     one is not an edit this tool can make"
                ));
            }
        }
        PropertyValue::Undecoded { bytes, .. } => {
            let now = edited.get("bytes").and_then(Json::as_u64);
            if now != Some(*bytes) {
                out.notes.push(format!(
                    "{label}: this payload did not decode, so its bytes are kept as they are"
                ));
            }
        }
        PropertyValue::Default | PropertyValue::Unset { .. } => {}
        _ => {
            if summary_changed(&entry.value, edited) {
                let Some(offset) = at() else {
                    out.notes.push(format!(
                        "{label}: the reader recorded no offset for it, so it cannot be addressed"
                    ));
                    return;
                };
                let Some(text) = text_of(edited) else {
                    out.notes
                        .push(format!("{label}: the edited value is not a {was}"));
                    return;
                };
                out.edits.values.push(ValueEdit {
                    offset,
                    expect_name: entry.name.clone(),
                    expect_element: entry.element,
                    expect_kind: was,
                    op: EditOp::Set { text },
                });
            }
        }
    }
}

/// A value whose kind changed. Only the moves between stored, defaulted and unset are edits; the
/// rest would rewrite the property as another type, which no save does.
#[allow(clippy::too_many_arguments)]
fn diff_retyped(
    entry: &PropertyEntry,
    edited: &Json,
    export: u32,
    label: &str,
    was: &str,
    is: &str,
    out: &mut DiffOutcome,
) {
    let Some(offset) = entry.span.map(|(start, _)| start) else {
        out.notes.push(format!(
            "{label}: it reads as a {is} rather than a {was}, and has no offset to address"
        ));
        return;
    };
    let mut push = |op: EditOp| {
        out.edits.values.push(ValueEdit {
            offset,
            expect_name: entry.name.clone(),
            expect_element: entry.element,
            expect_kind: was.to_string(),
            op,
        });
    };
    match (was, is) {
        (_, "default") => push(EditOp::Clear),
        (_, "unset") => push(EditOp::Unset),
        ("default", _) | ("unset", _) => match text_of(edited) {
            Some(text) => push(EditOp::Set { text }),
            None => {
                push(EditOp::Store);
                out.notes.push(format!(
                    "{label}: giving a {is} its stored form is one save on its own; dump the saved \
                     copy and diff again to fill it in"
                ));
            }
        },
        _ => out.notes.push(format!(
            "{label}: it reads as a {is} in the edited dump and a {was} in the package, which is a \
             retype rather than an edit"
        )),
    }
    let _ = export;
}

/// Container elements over the common prefix, then whatever the edited list added or dropped.
fn diff_items(
    entry: &PropertyEntry,
    items: &[PropertyValue],
    edited: &Json,
    export: u32,
    label: &str,
    out: &mut DiffOutcome,
) {
    let Some(now) = edited.get("items").and_then(Json::as_array) else {
        return;
    };
    let shared = items.len().min(now.len());
    for position in 0..shared {
        let before = &items[position];
        let after = &now[position];
        match before {
            PropertyValue::Struct { fields, .. } => {
                if let Some(inner) = after.get("fields").and_then(Json::as_array) {
                    diff_entries(fields, inner, export, &format!("{label}[{position}]"), out);
                }
            }
            _ => {
                if !summary_changed(before, after) {
                    continue;
                }
                let Some(offset) = entry.span.map(|(start, _)| start) else {
                    out.notes.push(format!(
                        "{label}[{position}]: the container has no recorded offset"
                    ));
                    continue;
                };
                let Some(text) = text_of(after) else {
                    out.notes.push(format!(
                        "{label}[{position}]: the edited element is not a {}",
                        kind_of(before)
                    ));
                    continue;
                };
                out.edits.values.push(ValueEdit {
                    offset,
                    expect_name: entry.name.clone(),
                    expect_element: entry.element,
                    expect_kind: kind_of(&entry.value),
                    op: EditOp::SetElement {
                        index: position as u32,
                        text,
                    },
                });
            }
        }
    }
    let Some(offset) = entry.span.map(|(start, _)| start) else {
        return;
    };
    let kind = kind_of(&entry.value);
    let is_set = matches!(entry.value, PropertyValue::Set { .. });
    for (position, added) in now.iter().enumerate().skip(items.len()) {
        // A set's element is its own key, so the new value goes in with the insert. One with no
        // text form, such as a struct, can only come in as the default.
        let key = if is_set { text_of(added) } else { None };
        if is_set && key.is_none() && !items.is_empty() && added.get("fields").is_none() {
            out.notes.push(format!(
                "{label}[{position}]: this set element has no text form to key it by; add it in \
                 the editor instead"
            ));
            continue;
        }
        out.edits.values.push(ValueEdit {
            offset,
            expect_name: entry.name.clone(),
            expect_element: entry.element,
            expect_kind: kind.clone(),
            op: EditOp::Insert {
                index: position as u32,
                key: key.clone(),
            },
        });
        if key.is_none() {
            let starts = if is_set {
                "takes the element type's default"
            } else {
                "copies the one before it"
            };
            out.notes.push(format!(
                "{label}[{position}]: a new element {starts}; dump the saved copy and diff again \
                 to give it a value"
            ));
        }
    }
    for position in (now.len()..items.len()).rev() {
        out.edits.values.push(ValueEdit {
            offset,
            expect_name: entry.name.clone(),
            expect_element: entry.element,
            expect_kind: kind.clone(),
            op: EditOp::Remove {
                index: position as u32,
            },
        });
    }
}

/// A map's value half. It is addressed as an element of the map, so only a scalar change is an
/// edit; a struct value recurses through its own fields' offsets.
#[allow(clippy::too_many_arguments)]
fn diff_map_value(
    entry: &PropertyEntry,
    position: u32,
    before: &PropertyValue,
    edited: &Json,
    export: u32,
    label: &str,
    out: &mut DiffOutcome,
) {
    if let PropertyValue::Struct { fields, .. } = before {
        if let Some(inner) = edited.get("fields").and_then(Json::as_array) {
            diff_entries(fields, inner, export, &format!("{label}[{position}]"), out);
        }
        return;
    }
    if !summary_changed(before, edited) {
        return;
    }
    let (Some(offset), Some(text)) = (entry.span.map(|(start, _)| start), text_of(edited)) else {
        out.notes.push(format!(
            "{label}[{position}]: the edited value cannot be addressed as an element"
        ));
        return;
    };
    out.edits.values.push(ValueEdit {
        offset,
        expect_name: entry.name.clone(),
        expect_element: entry.element,
        expect_kind: kind_of(&entry.value),
        op: EditOp::SetElement {
            index: position,
            text,
        },
    });
}

/// Whether the edited JSON says something the original does not. Compared through the text form
/// both ends already use, which is what an edit carries and what verification reads back.
fn summary_changed(was: &PropertyValue, edited: &Json) -> bool {
    match text_of(edited) {
        Some(text) => text != was.summary(),
        None => false,
    }
}

/// How the dump renders a reference the loader binds. Kept apart from `text_of` so a retype can
/// never turn one into a `Set`: none of these three is written from text.
fn bound_reference(edited: &Json) -> Option<String> {
    let field = |name: &str| edited.get(name).and_then(Json::as_str);
    match edited.get("kind").and_then(Json::as_str)? {
        "delegate" => {
            let function = field("function")?;
            Some(match field("object") {
                Some(object) => format!("{object}::{function}"),
                None => function.to_string(),
            })
        }
        "field_path" => field("path").map(str::to_string),
        "lazy_object" => field("guid").map(str::to_string),
        _ => None,
    }
}

/// The text form of an edited value, the way a `Set` carries it. `None` for a value with no single
/// text form, which is every container and struct.
fn text_of(edited: &Json) -> Option<String> {
    let kind = edited.get("kind").and_then(Json::as_str)?;
    let field = |name: &str| edited.get(name);
    Some(match kind {
        "bool" => field("value")?.as_bool()?.to_string(),
        "int" => field("value")?.as_i64()?.to_string(),
        "uint" | "byte" => field("value")?.as_u64()?.to_string(),
        "float" => format_float(field("value")?.as_f64()?),
        "str" | "name" => field("value")?.as_str()?.to_string(),
        "text" => field("value")
            .and_then(Json::as_str)
            .unwrap_or_default()
            .to_string(),
        "enum" => match field("name").and_then(Json::as_str) {
            Some(name) => name.to_string(),
            None => field("value")?.as_i64()?.to_string(),
        },
        "object" => match field("path").and_then(Json::as_str) {
            Some(path) => path.to_string(),
            None => match field("index")?.as_i64()? {
                0 => "None".to_string(),
                index => index.to_string(),
            },
        },
        "soft_object" => field("path")?.as_str()?.to_string(),
        _ => return None,
    })
}

/// The dump's own float rendering, so a value that was not edited compares equal.
fn format_float(value: f64) -> String {
    if value == value.trunc() && value.abs() < 1e15 {
        format!("{value:.1}")
    } else {
        format!("{value}")
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(name: &str, value: PropertyValue, at: u64) -> PropertyEntry {
        PropertyEntry {
            name: name.into(),
            element: None,
            span: Some((at, at + 4)),
            value,
            slot: None,
        }
    }

    /// One entry against one edited value, which is what every rule below is really testing.
    fn one(before: PropertyEntry, after: Json) -> DiffOutcome {
        let mut out = DiffOutcome::default();
        diff_entries(
            std::slice::from_ref(&before),
            &[json!({"name": before.name, "value": after})],
            0,
            "/Game/Thing.Thing",
            &mut out,
        );
        out
    }

    fn value(out: &DiffOutcome) -> &ValueEdit {
        assert_eq!(out.edits.values.len(), 1, "{:?}", out.notes);
        &out.edits.values[0]
    }

    #[test]
    fn a_changed_scalar_becomes_a_set_at_its_own_offset() {
        let out = one(
            entry("Count", PropertyValue::Int { value: 3 }, 0x40),
            json!({"kind": "int", "value": 9}),
        );
        let edit = value(&out);
        assert_eq!(edit.offset, 0x40);
        assert_eq!(edit.expect_name, "Count");
        assert_eq!(edit.expect_kind, "int");
        assert!(matches!(&edit.op, EditOp::Set { text } if text == "9"));
    }

    /// A value that reads the same produces nothing, which is what keeps a diff of an untouched
    /// dump empty rather than a rewrite of the whole package.
    #[test]
    fn an_unchanged_value_produces_no_edit() {
        let out = one(
            entry("Rate", PropertyValue::Float { value: 2.0 }, 0x10),
            json!({"kind": "float", "value": 2.0}),
        );
        assert!(out.edits.is_empty(), "{:?}", out.edits);
        assert!(out.notes.is_empty(), "{:?}", out.notes);
    }

    /// An enum is written by name, so the name is what the edit carries.
    #[test]
    fn an_enum_is_set_by_its_enumerator_name() {
        let out = one(
            entry(
                "Role",
                PropertyValue::Enum {
                    value: 0,
                    name: Some("Tank".into()),
                    enum_type: Some("EHeroRole".into()),
                },
                0x8,
            ),
            json!({"kind": "enum", "value": 2, "name": "Support", "enum_type": "EHeroRole"}),
        );
        assert!(matches!(&value(&out).op, EditOp::Set { text } if text == "Support"));
    }

    /// An object reference is written by path, and a cleared one by the word the dump renders.
    #[test]
    fn an_object_is_set_by_path() {
        let out = one(
            entry(
                "Mesh",
                PropertyValue::Object {
                    index: 3,
                    path: Some("/Game/A.A".into()),
                },
                0x20,
            ),
            json!({"kind": "object", "index": 4, "path": "/Game/B.B"}),
        );
        assert!(matches!(&value(&out).op, EditOp::Set { text } if text == "/Game/B.B"));
    }

    /// Moving a stored value to its default, to unset, and back are the three kind changes that
    /// are edits. Everything else is a retype, which no save makes.
    #[test]
    fn the_kind_changes_that_are_edits() {
        let stored = entry("Count", PropertyValue::Int { value: 3 }, 0x40);
        let out = one(stored.clone(), json!({"kind": "default"}));
        assert!(matches!(value(&out).op, EditOp::Clear));

        let out = one(stored.clone(), json!({"kind": "unset", "declared": "Int"}));
        assert!(matches!(value(&out).op, EditOp::Unset));

        let defaulted = entry("Count", PropertyValue::Default, 0x40);
        let out = one(defaulted, json!({"kind": "int", "value": 5}));
        assert!(matches!(&value(&out).op, EditOp::Set { text } if text == "5"));

        let unset = entry(
            "Count",
            PropertyValue::Unset {
                declared: "Int",
                enum_type: None,
            },
            0x40,
        );
        let out = one(unset, json!({"kind": "int", "value": 5}));
        assert_eq!(value(&out).expect_kind, "unset");
    }

    /// An unset struct has to be stored before its fields can be filled in, so the diff says so
    /// rather than pretending one pass is enough.
    #[test]
    fn an_unset_struct_is_stored_and_noted() {
        let out = one(
            entry(
                "Where",
                PropertyValue::Unset {
                    declared: "Struct",
                    enum_type: None,
                },
                0x40,
            ),
            json!({"kind": "struct", "name": "Vector", "fields": []}),
        );
        assert!(matches!(value(&out).op, EditOp::Store));
        assert_eq!(out.notes.len(), 1, "{:?}", out.notes);
        assert!(out.notes[0].contains("diff again"), "{}", out.notes[0]);
    }

    /// A retype is refused rather than guessed at: nothing rewrites a property as another type.
    #[test]
    fn a_retype_is_a_note_and_no_edit() {
        let out = one(
            entry("Count", PropertyValue::Int { value: 3 }, 0x40),
            json!({"kind": "str", "value": "three"}),
        );
        assert!(out.edits.is_empty());
        assert_eq!(out.notes.len(), 1);
        assert!(out.notes[0].contains("retype"), "{}", out.notes[0]);
    }

    /// A struct recurses, so an edit lands on the field's own offset rather than the struct's.
    #[test]
    fn a_struct_field_is_addressed_by_its_own_offset() {
        let out = one(
            entry(
                "Where",
                PropertyValue::Struct {
                    name: "Vector".into(),
                    fields: vec![entry("X", PropertyValue::Float { value: 1.0 }, 0x50)],
                },
                0x50,
            ),
            json!({
                "kind": "struct",
                "name": "Vector",
                "fields": [{"name": "X", "value": {"kind": "float", "value": 4.5}}],
            }),
        );
        let edit = value(&out);
        assert_eq!(edit.offset, 0x50);
        assert_eq!(edit.expect_name, "X");
        assert!(matches!(&edit.op, EditOp::Set { text } if text == "4.5"));
    }

    fn array(items: Vec<PropertyValue>) -> PropertyEntry {
        entry("Tags", PropertyValue::Array { items }, 0x30)
    }

    /// A changed element is set by index at the container's offset, since an element of a scalar
    /// array has no offset of its own.
    #[test]
    fn a_changed_element_is_set_by_index() {
        let out = one(
            array(vec![
                PropertyValue::Str { value: "a".into() },
                PropertyValue::Str { value: "b".into() },
            ]),
            json!({"kind": "array", "items": [
                {"kind": "str", "value": "a"},
                {"kind": "str", "value": "z"},
            ]}),
        );
        let edit = value(&out);
        assert_eq!(edit.offset, 0x30);
        assert_eq!(edit.expect_kind, "array");
        assert!(
            matches!(&edit.op, EditOp::SetElement { index: 1, text } if text == "z"),
            "{:?}",
            edit.op
        );
    }

    /// A longer list inserts at the end, each with the note that its value needs a second pass;
    /// a shorter one removes from the end down, so an earlier removal does not move a later one.
    #[test]
    fn a_container_grows_by_inserts_and_shrinks_by_removes() {
        let out = one(
            array(vec![PropertyValue::Str { value: "a".into() }]),
            json!({"kind": "array", "items": [
                {"kind": "str", "value": "a"},
                {"kind": "str", "value": "b"},
            ]}),
        );
        assert!(matches!(
            out.edits.values[0].op,
            EditOp::Insert { index: 1, .. }
        ));
        assert_eq!(out.notes.len(), 1);

        let out = one(
            array(vec![
                PropertyValue::Str { value: "a".into() },
                PropertyValue::Str { value: "b".into() },
                PropertyValue::Str { value: "c".into() },
            ]),
            json!({"kind": "array", "items": [{"kind": "str", "value": "a"}]}),
        );
        let indices: Vec<u32> = out
            .edits
            .values
            .iter()
            .filter_map(|edit| match edit.op {
                EditOp::Remove { index } => Some(index),
                _ => None,
            })
            .collect();
        assert_eq!(indices, vec![2, 1], "removed from the end down");
    }

    /// A set keys on its elements, so a new one goes in by its value in one pass. Without a key a
    /// set that already holds elements refuses the insert, so none is emitted for it.
    #[test]
    fn a_set_grows_by_keyed_inserts() {
        let set = |items| entry("Names", PropertyValue::Set { items }, 0x40);
        let out = one(
            set(vec![PropertyValue::Name { value: "A".into() }]),
            json!({"kind": "set", "items": [
                {"kind": "name", "value": "A"},
                {"kind": "name", "value": "B"},
            ]}),
        );
        assert!(
            matches!(&out.edits.values[0].op, EditOp::Insert { index: 1, key: Some(key) } if key == "B"),
            "{:?}",
            out.edits.values
        );
        assert!(out.notes.is_empty(), "{:?}", out.notes);

        let out = one(
            set(vec![PropertyValue::Name { value: "A".into() }]),
            json!({"kind": "set", "items": [
                {"kind": "name", "value": "A"},
                {"kind": "delegate", "object": null, "function": "F"},
            ]}),
        );
        assert!(out.edits.values.is_empty(), "{:?}", out.edits.values);
        assert_eq!(out.notes.len(), 1);
    }

    /// A delegate is bound by the loader rather than stored as text, so a changed one is reported
    /// and not written.
    #[test]
    fn a_changed_delegate_is_a_note() {
        let out = one(
            entry(
                "OnFired",
                PropertyValue::Delegate {
                    object: Some("/Game/A.A_C".into()),
                    function: "Handler".into(),
                },
                0x60,
            ),
            json!({"kind": "delegate", "object": "/Game/A.A_C", "function": "Other"}),
        );
        assert!(out.edits.is_empty());
        assert_eq!(out.notes.len(), 1);
    }

    /// A property the edited dump dropped altogether is reported rather than treated as a removal,
    /// since a property list is not something a save adds to or takes from.
    #[test]
    fn a_missing_property_is_a_note() {
        let mut out = DiffOutcome::default();
        diff_entries(
            &[entry("Count", PropertyValue::Int { value: 1 }, 0)],
            &[],
            0,
            "/Game/Thing.Thing",
            &mut out,
        );
        assert!(out.edits.is_empty());
        assert_eq!(out.notes.len(), 1);
        assert!(
            out.notes[0].contains("no such property"),
            "{}",
            out.notes[0]
        );
    }

    /// The text form the dump renders is the one an edit carries, so a whole float keeps the
    /// trailing zero and compares equal to itself.
    #[test]
    fn the_text_form_matches_the_dump() {
        assert_eq!(
            text_of(&json!({"kind": "float", "value": 2.0})).as_deref(),
            Some("2.0")
        );
        assert_eq!(
            text_of(&json!({"kind": "object", "index": 0})).as_deref(),
            Some("None")
        );
        assert_eq!(text_of(&json!({"kind": "array", "items": []})), None);
    }
}
