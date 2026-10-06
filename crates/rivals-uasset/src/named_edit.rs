//! Edits to a DataTable's rows, a StringTable's entries, a function's script or the import table
//! that name their object or import by path rather than by index. Each is put at the index that
//! path has in the package the save reads, so an edit file holding them still fits once a patch
//! has moved the tables, and one that has landed already is left out, with a note.

use crate::edit::{PackageEdits, RowOp, StringOp};
use crate::export_edit::ExportEdit;
use crate::header_edit::ImportEdit;
use crate::package::ParsedPackage;
use crate::path_edit::export_of;

/// Whether any edit in `changes` names its object or import by path.
pub fn names_any(changes: &PackageEdits) -> bool {
    changes.rows.iter().any(|edit| edit.object.is_some())
        || changes.strings.iter().any(|edit| edit.object.is_some())
        || changes
            .script_texts
            .iter()
            .any(|edit| edit.object.is_some())
        || changes
            .exports
            .iter()
            .any(|edit| matches!(edit, ExportEdit::Rename { from: Some(_), .. }))
        || changes.imports.iter().any(|edit| {
            matches!(
                edit,
                ImportEdit::Retarget { from: Some(_), .. }
                    | ImportEdit::Remove { from: Some(_), .. }
            )
        })
}

/// Puts each edit in `changes` that names its object or import by path at the index that path
/// has in `parsed`, the package as the save reads it, and leaves out each that has landed already,
/// saying so in what comes back. A path that names nothing is refused.
pub fn place_named(
    parsed: &ParsedPackage,
    changes: &mut PackageEdits,
) -> Result<Vec<String>, String> {
    let mut notes = Vec::new();
    place_renames(parsed, changes, &mut notes)?;
    place_rows(parsed, changes, &mut notes)?;
    place_strings(parsed, changes, &mut notes)?;
    place_scripts(parsed, changes, &mut notes)?;
    place_imports(parsed, changes, &mut notes)?;
    Ok(notes)
}

/// The path an object has below the package once renamed: its old path with its own name, the last
/// step, replaced.
fn renamed(from: &str, name: &str) -> String {
    match from.rfind([':', '.']) {
        Some(at) => format!("{}{name}", &from[..=at]),
        None => name.to_string(),
    }
}

/// Puts each rename naming its object by path at the object's index. One already made, its old
/// path naming nothing and its new one an object, is left out, and every edit of the save naming
/// the old path is pointed at the new one, so a file holding a rename applies again.
fn place_renames(
    parsed: &ParsedPackage,
    changes: &mut PackageEdits,
    notes: &mut Vec<String>,
) -> Result<(), String> {
    let mut moved: Vec<(String, String)> = Vec::new();
    let mut kept = Vec::new();
    for edit in std::mem::take(&mut changes.exports) {
        let ExportEdit::Rename {
            name,
            from: Some(from),
            ..
        } = edit
        else {
            kept.push(edit);
            continue;
        };
        match export_of(parsed, &from) {
            Ok(export) => kept.push(ExportEdit::Rename {
                export: export.index,
                name,
                from: None,
            }),
            Err(why) => {
                let now = renamed(&from, &name);
                if export_of(parsed, &now).is_err() {
                    return Err(why);
                }
                notes.push(format!("{from}: already called {name}"));
                moved.push((from, now));
            }
        }
    }
    changes.exports = kept;
    let follow = |selector: &mut String| {
        if let Some((_, now)) = moved.iter().find(|(from, _)| from == selector.trim()) {
            *selector = now.clone();
        }
    };
    for edit in &mut changes.paths {
        follow(&mut edit.export);
    }
    let objects = changes
        .rows
        .iter_mut()
        .filter_map(|edit| edit.object.as_mut())
        .chain(
            changes
                .strings
                .iter_mut()
                .filter_map(|edit| edit.object.as_mut()),
        )
        .chain(
            changes
                .script_texts
                .iter_mut()
                .filter_map(|edit| edit.object.as_mut()),
        );
    for object in objects {
        follow(object);
    }
    Ok(())
}

fn place_rows(
    parsed: &ParsedPackage,
    changes: &mut PackageEdits,
    notes: &mut Vec<String>,
) -> Result<(), String> {
    let mut kept = Vec::new();
    for mut edit in std::mem::take(&mut changes.rows) {
        let Some(object) = edit.object.take() else {
            kept.push(edit);
            continue;
        };
        let export = export_of(parsed, &object)?;
        let table = export
            .data_table
            .as_ref()
            .ok_or_else(|| format!("{object} is not a DataTable"))?;
        let holds = |name: &str| {
            table
                .rows
                .iter()
                .any(|row| row.name.eq_ignore_ascii_case(name))
        };
        let landed = match &edit.op {
            RowOp::Add { name, .. } | RowOp::Duplicate { name, .. } => {
                holds(name).then(|| format!("already holds a row {name}"))
            }
            RowOp::Remove { name } => {
                (!holds(name)).then(|| format!("holds no row {name} to drop"))
            }
            RowOp::Rename { name, to } => {
                (!holds(name) && holds(to)).then(|| format!("row {name} is already called {to}"))
            }
        };
        match landed {
            Some(why) => notes.push(format!("{object}: {why}")),
            None => {
                edit.export = export.index;
                kept.push(edit);
            }
        }
    }
    changes.rows = kept;
    Ok(())
}

fn place_strings(
    parsed: &ParsedPackage,
    changes: &mut PackageEdits,
    notes: &mut Vec<String>,
) -> Result<(), String> {
    // An entry this save gives a new key is found by that key once it has landed.
    let rekeyed: Vec<(String, String, String)> = changes
        .strings
        .iter()
        .filter_map(|edit| match (&edit.object, &edit.op) {
            (Some(object), StringOp::SetKey { key, to, .. }) => {
                Some((object.clone(), key.clone(), to.clone()))
            }
            _ => None,
        })
        .collect();
    let mut kept = Vec::new();
    for mut edit in std::mem::take(&mut changes.strings) {
        let Some(object) = edit.object.take() else {
            kept.push(edit);
            continue;
        };
        let export = export_of(parsed, &object)?;
        let table = export
            .string_table
            .as_ref()
            .ok_or_else(|| format!("{object} is not a StringTable"))?;
        let find = |key: &str| table.entries.iter().position(|entry| entry.key == key);
        let entry_of = |key: &str| {
            find(key)
                .or_else(|| {
                    rekeyed
                        .iter()
                        .find(|(held, from, _)| *held == object && from == key)
                        .and_then(|(_, _, to)| find(to))
                })
                .ok_or_else(|| format!("{object} holds no key {key}"))
        };
        let source = matches!(edit.op, StringOp::SetSource { .. });
        let landed = match &mut edit.op {
            StringOp::Add { key, .. } => find(key).map(|_| format!("already holds a key {key}")),
            StringOp::Remove { index, key } => match find(key) {
                Some(at) => {
                    *index = at as u32;
                    None
                }
                None => Some(format!("holds no key {key} to drop")),
            },
            StringOp::SetKey { index, key, to } => match find(key) {
                Some(at) => {
                    *index = at as u32;
                    None
                }
                None if find(to).is_some() => Some(format!("{key} is already keyed {to}")),
                None => return Err(format!("{object} holds no key {key}")),
            },
            StringOp::SetSource { index, key, to } | StringOp::SetTag { index, key, to } => {
                let at = entry_of(key)?;
                let entry = &table.entries[at];
                let now = if source { &entry.source } else { &entry.tag };
                if now == to {
                    Some(format!("{} already reads {to}", entry.key))
                } else {
                    *index = at as u32;
                    *key = entry.key.clone();
                    None
                }
            }
            StringOp::SetMetaData { index, key, id, to } => {
                let at = entry_of(key)?;
                let entry = &table.entries[at];
                if entry
                    .metadata
                    .iter()
                    .any(|(held, value)| held == id && value == to)
                {
                    Some(format!("{}'s {id} already reads {to}", entry.key))
                } else {
                    *index = at as u32;
                    *key = entry.key.clone();
                    None
                }
            }
            StringOp::RemoveMetaData { index, key, id } => {
                let at = entry_of(key)?;
                let entry = &table.entries[at];
                if entry.metadata.iter().any(|(held, _)| held == id) {
                    *index = at as u32;
                    *key = entry.key.clone();
                    None
                } else {
                    Some(format!("{} holds no {id} to drop", entry.key))
                }
            }
        };
        match landed {
            Some(why) => notes.push(format!("{object}: {why}")),
            None => {
                edit.export = export.index;
                kept.push(edit);
            }
        }
    }
    changes.strings = kept;
    Ok(())
}

fn place_scripts(
    parsed: &ParsedPackage,
    changes: &mut PackageEdits,
    notes: &mut Vec<String>,
) -> Result<(), String> {
    let mut kept = Vec::new();
    for mut edit in std::mem::take(&mut changes.script_texts) {
        let Some(object) = edit.object.take() else {
            kept.push(edit);
            continue;
        };
        let export = export_of(parsed, &object)?;
        let now = crate::script_text::print_script(parsed, export.index).map(|text| text.text());
        if now.is_some_and(|now| same_script(&now, &edit.text)) {
            notes.push(format!(
                "{object}: its script already reads as the text given"
            ));
            continue;
        }
        edit.export = export.index;
        kept.push(edit);
    }
    changes.script_texts = kept;
    Ok(())
}

/// Whether two texts spell one script, wherever its code sits: each label a printing gives an
/// offset is compared by the order it first appears in.
fn same_script(a: &str, b: &str) -> bool {
    labels_in_order(a.trim()) == labels_in_order(b.trim())
}

fn labels_in_order(text: &str) -> String {
    let mut named: Vec<&str> = Vec::new();
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('@') {
        out.push_str(&rest[..=at]);
        rest = &rest[at + 1..];
        let digits = rest
            .find(|c: char| !c.is_ascii_hexdigit())
            .unwrap_or(rest.len());
        if digits < 4 {
            continue;
        }
        let label = &rest[..digits];
        let order = named
            .iter()
            .position(|held| *held == label)
            .unwrap_or_else(|| {
                named.push(label);
                named.len() - 1
            });
        out.push_str(&format!("#{order}"));
        rest = &rest[digits..];
    }
    out.push_str(rest);
    out
}

fn place_imports(
    parsed: &ParsedPackage,
    changes: &mut PackageEdits,
    notes: &mut Vec<String>,
) -> Result<(), String> {
    let at = |path: &str| parsed.imports.iter().position(|import| import.path == path);
    let mut kept = Vec::new();
    for edit in std::mem::take(&mut changes.imports) {
        match edit {
            ImportEdit::Retarget {
                path,
                class,
                from: Some(from),
                ..
            } => match at(&from) {
                Some(import) => kept.push(ImportEdit::Retarget {
                    import: import as u32,
                    path,
                    class,
                    from: None,
                }),
                None if at(&path).is_some() => {
                    notes.push(format!("{from}: an import already points at {path}"));
                }
                None => return Err(format!("this package imports nothing at {from}")),
            },
            ImportEdit::Remove {
                from: Some(from), ..
            } => match at(&from) {
                Some(import) => kept.push(ImportEdit::Remove {
                    import: import as u32,
                    from: None,
                }),
                None => notes.push(format!(
                    "{from}: this package imports nothing there to drop"
                )),
            },
            other => kept.push(other),
        }
    }
    changes.imports = kept;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_renamed_object_keeps_its_outers() {
        assert_eq!(
            renamed("Level:PersistentLevel:BodySetup_0", "Probe"),
            "Level:PersistentLevel:Probe"
        );
        assert_eq!(renamed("BP_C.Default__BP_C", "Other"), "BP_C.Other");
        assert_eq!(renamed("Table", "NewTable"), "NewTable");
    }

    #[test]
    fn a_script_is_the_same_wherever_its_labels_sit() {
        let printed = "PushExecutionFlow @0888\nJump @025E\n@025E:\nReturn Nothing\n@0888:";
        let moved = "PushExecutionFlow @08F0\nJump @02C8\n@02C8:\nReturn Nothing\n@08F0:";
        assert!(same_script(printed, moved));
        let swapped = "PushExecutionFlow @02C8\nJump @08F0\n@08F0:\nReturn Nothing\n@02C8:";
        assert!(
            same_script(printed, swapped),
            "only the order labels appear in matters"
        );
        let other = "PushExecutionFlow @0888\nJump @0888\n@025E:\nReturn Nothing\n@0888:";
        assert!(!same_script(printed, other));
        assert!(same_script("Jump @loop\n@loop:", "Jump @loop\n@loop:"));
    }
}
