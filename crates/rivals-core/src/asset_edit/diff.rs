//! Turns an edited JSON dump back into the edit list that produces it.
//!
//! The dump is the readable form of a package; editing it by hand is how a change gets described
//! without knowing byte offsets. What comes back is compared against the parse the dump came from,
//! and every difference becomes the edit that would make it, addressed by the object and property
//! path leading to it, with what it read as there. Such an edit is placed in whatever the save
//! reads, so the list still fits a copy of the package a patch or another save has changed. The
//! few values no path reaches keep the offset the original recorded, held to it by `expect`.
//!
//! A change that has to wait for another, such as a new row's columns or the fields an instanced
//! struct takes with a new type, is written to wait for the edit that makes its place, so one apply
//! makes it all. What the comparison cannot express it reports as a note rather than guessing; the
//! edits beside a note still apply.

use serde_json::Value as Json;

use rivals_uasset::{
    EditOp, Expected, ExportEdit, FieldSet, ImportEdit, PackageEdits, ParsedPackage, PathEdit,
    PathOp, PropertyEntry, PropertyValue, RowEdit, RowOp, Segment, StringEdit, StringOp,
    TYPE_FIELD, ValueEdit, kind_of,
};

use super::json::EditList;

/// The edits that turn the original into the edited dump, and everything the comparison could not
/// express as one.
#[derive(Debug, Default)]
pub struct DiffOutcome {
    pub edits: EditList,
    pub notes: Vec<String>,
    /// Changes that wait for an edit beside them, written out as path edits once the original is
    /// at hand: see [`to_paths`].
    follow: Vec<FollowUp>,
    /// What each container the edits add to, drop from or reorder holds once they have, by where
    /// it sits: see [`PathEdit::becomes`].
    becomes: Vec<Becomes>,
}

/// A container, by where the original holds it, and what the edited dump says it holds.
#[derive(Debug)]
struct Becomes {
    offset: u64,
    name: String,
    element: Option<u32>,
    elements: String,
}

/// Records what the edited dump says a container holds, for the edits that add to, drop from or
/// reorder it.
fn record_becomes(entry: &PropertyEntry, edited: &Json, out: &mut DiffOutcome) {
    if let (Some((offset, _)), Some(elements)) = (entry.span, elements_in(edited)) {
        out.becomes.push(Becomes {
            offset,
            name: entry.name.clone(),
            element: entry.element,
            elements,
        });
    }
}

/// A dumped container's elements as `rivals_uasset::elements_of` lists a container's: each
/// element's halves as text, `null` for one with no text form.
fn elements_in(edited: &Json) -> Option<String> {
    let halves: Vec<Vec<Option<String>>> = match edited.get("entries").and_then(Json::as_array) {
        Some(pairs) => pairs
            .iter()
            .map(|pair| {
                vec![
                    pair.get("key").and_then(text_of),
                    pair.get("value").and_then(text_of),
                ]
            })
            .collect(),
        None => edited
            .get("items")
            .and_then(Json::as_array)?
            .iter()
            .map(|item| vec![text_of(item)])
            .collect(),
    };
    serde_json::to_string(&halves).ok()
}

/// What the container an edit adds to, drops from or reorders becomes, where the diff recorded it.
fn becomes_of(becomes: &[Becomes], edit: &ValueEdit) -> Option<String> {
    if !matches!(
        edit.op,
        EditOp::Insert { .. } | EditOp::Remove { .. } | EditOp::Reorder { .. }
    ) {
        return None;
    }
    becomes
        .iter()
        .find(|held| {
            held.offset == edit.offset
                && held.name == edit.expect_name
                && held.element == edit.expect_element
        })
        .map(|held| held.elements.clone())
}

/// Where a change that waits for another is written from: a value the original holds, or a row
/// this diff adds.
#[derive(Debug, Clone)]
enum Anchor {
    Value {
        offset: u64,
        name: String,
        element: Option<u32>,
    },
    Row {
        export: u32,
        name: String,
    },
}

/// A change that lands right only once an edit beside it has, so it is written as a path edit,
/// which waits for that edit in the same apply.
#[derive(Debug)]
enum FollowUp {
    /// One edit on what `steps` names below the anchor.
    Edit {
        anchor: Anchor,
        steps: Vec<Segment>,
        op: PathOp,
        becomes: Option<String>,
        label: String,
    },
    /// What the edited dump gives a value the same save makes: see [`fill`].
    Fill {
        anchor: Anchor,
        steps: Vec<Segment>,
        edited: Json,
        label: String,
    },
    /// An edit worked out against the original that has to name its value by path: one in a map
    /// whose pairs move, or on an instanced struct whose fields follow its new type.
    Keyed { edit: Keyed, label: String },
}

#[derive(Debug)]
enum Keyed {
    Value(ValueEdit),
    Field(FieldSet),
}

/// The value an edit at `entry` starts from, when the reader recorded where it is.
fn anchor_of(entry: &PropertyEntry) -> Option<Anchor> {
    Some(Anchor::Value {
        offset: entry.span?.0,
        name: entry.name.clone(),
        element: entry.element,
    })
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
        let class = is
            .get("class_name")
            .and_then(Json::as_str)
            .unwrap_or(&was.class_name);
        if class != was.class_name {
            return Err(format!(
                "export {at}'s class_name reads {class} in the edited dump and {} in the package; \
                 an object takes another class with asset export-edit --class, which empties it",
                was.class_name
            ));
        }
        // A new name is the export table's own edit, made once everything else has been.
        let name = is
            .get("object_name")
            .and_then(Json::as_str)
            .unwrap_or(&was.object_name);
        if name != was.object_name {
            out.edits.export_edits.push(ExportEdit::Rename {
                export: at,
                name: name.to_string(),
                from: Some(rivals_uasset::below_package(&was.path).to_string()),
            });
        }
        let Some(properties) = is.get("properties").and_then(Json::as_array) else {
            continue;
        };
        diff_entries(&was.properties, properties, at, &was.path, &mut out);
        if let Some(defaults) = is.get("defaults").and_then(Json::as_array) {
            diff_entries(&was.defaults, defaults, at, &was.path, &mut out);
        }
        diff_rows(was, is, at, &mut out);
        diff_strings(was, is, at, &mut out);
    }
    to_paths(original, &mut out);
    out.edits.expect_from(original);
    Ok(out)
}

/// Rewrites each value edit and field set as a path edit wherever one is placed on the same value,
/// so the list names what it changes rather than where it sat in this copy of the package. One no
/// path reaches, such as a field of a map's key, keeps its offset. The changes that wait for an
/// edit beside them are written out here too.
fn to_paths(original: &ParsedPackage, out: &mut DiffOutcome) {
    let edits = &mut out.edits;
    let mut kept = Vec::new();
    for edit in std::mem::take(&mut edits.values) {
        match value_path(original, &edit).filter(|path| placed_alone(original, path, &edit)) {
            Some(mut path) => {
                path.becomes = becomes_of(&out.becomes, &edit);
                edits.paths.push(path);
            }
            None => kept.push(edit),
        }
    }
    edits.values = kept;
    for follow in std::mem::take(&mut out.follow) {
        place_follow_up(
            original,
            follow,
            &out.becomes,
            &mut out.edits.paths,
            &mut out.notes,
        );
    }
    let edits = &mut out.edits;
    let mut kept = Vec::new();
    for set in std::mem::take(&mut edits.field_sets) {
        let path = field_set_path(original, &set)
            .filter(|path| set_placed(original, path, &set, &edits.paths, &edits.rows));
        match path {
            Some(path) => edits.paths.push(path),
            None => kept.push(set),
        }
    }
    edits.field_sets = kept;
}

/// A value edit as a path edit: where its value sits, an element it acts on by key where that is
/// unique and by position where not, and what the value read as, which is what `expect` would have
/// held it to. A positional insert into an array is held to every element the array held.
fn value_path(original: &ParsedPackage, edit: &ValueEdit) -> Option<PathEdit> {
    let place = rivals_uasset::place_of(
        original,
        edit.offset,
        &edit.expect_name,
        edit.expect_element,
    )?;
    let entry = rivals_uasset::entry_named_at(
        original,
        edit.offset,
        &edit.expect_name,
        edit.expect_element,
    )?;
    let (element, op) = PathOp::split(edit.op.clone());
    let mut segments = place.segments;
    if let Some(index) = element {
        segments.push(rivals_uasset::element_segment(&entry.value, index));
    }
    let skeleton = PackageEdits {
        values: vec![edit.clone()],
        ..Default::default()
    };
    let mut was = rivals_uasset::expectations(original, &skeleton)
        .values
        .remove(&Expected::value_key(edit));
    if matches!(edit.op, EditOp::Insert { .. })
        && matches!(entry.value, PropertyValue::Array { .. })
    {
        was = rivals_uasset::elements_of(&entry.value);
    }
    Some(PathEdit {
        export: rivals_uasset::below_package(&original.exports.get(place.export as usize)?.path)
            .to_string(),
        row: place.row,
        defaults: place.defaults,
        path: rivals_uasset::format_path(&segments),
        op,
        was,
        becomes: None,
    })
}

/// A field set as a path edit: where the value it goes through sits, then its own steps.
fn field_set_path(original: &ParsedPackage, set: &FieldSet) -> Option<PathEdit> {
    let place =
        rivals_uasset::place_of(original, set.offset, &set.expect_name, set.expect_element)?;
    let mut segments = place.segments;
    segments.extend(steps_of(&set.path));
    Some(PathEdit {
        export: rivals_uasset::below_package(&original.exports.get(place.export as usize)?.path)
            .to_string(),
        row: place.row,
        defaults: place.defaults,
        path: rivals_uasset::format_path(&segments),
        op: PathOp::Set {
            text: set.text.clone(),
        },
        was: None,
        becomes: None,
    })
}

/// A field set's steps as path segments: a field as `Name` or `Name[slot]`, an element as `[i]`.
fn steps_of(path: &[String]) -> Vec<Segment> {
    let mut steps = Vec::new();
    for step in path {
        let slot = step
            .strip_suffix(']')
            .and_then(|step| step.split_once('['))
            .and_then(|(name, at)| Some((name, at.parse::<u32>().ok()?)));
        match slot {
            Some((name, at)) => {
                if !name.is_empty() {
                    steps.push(Segment::Field(name.to_string()));
                }
                steps.push(Segment::Index(at));
            }
            None => steps.push(Segment::Field(step.clone())),
        }
    }
    steps
}

/// A field's steps: its name, and its slot for a static array.
fn field_steps(name: &str, element: Option<u32>) -> Vec<Segment> {
    let mut steps = vec![Segment::Field(name.to_string())];
    steps.extend(element.map(Segment::Index));
    steps
}

/// The object, row or defaults a path edit starts in.
struct Head {
    export: String,
    row: Option<String>,
    defaults: bool,
}

impl Head {
    fn edit(&self, at: &[Segment], op: PathOp) -> PathEdit {
        PathEdit {
            export: self.export.clone(),
            row: self.row.clone(),
            defaults: self.defaults,
            path: rivals_uasset::format_path(at),
            op,
            was: None,
            becomes: None,
        }
    }
}

/// Where an anchor sits in the original, as a path edit names it.
fn place_anchor(original: &ParsedPackage, anchor: &Anchor) -> Option<(Head, Vec<Segment>)> {
    let export_named = |index: u32| {
        original
            .exports
            .get(index as usize)
            .map(|export| rivals_uasset::below_package(&export.path).to_string())
    };
    match anchor {
        Anchor::Value {
            offset,
            name,
            element,
        } => {
            let place = rivals_uasset::place_of(original, *offset, name, *element)?;
            let head = Head {
                export: export_named(place.export)?,
                row: place.row,
                defaults: place.defaults,
            };
            Some((head, place.segments))
        }
        Anchor::Row { export, name } => {
            let head = Head {
                export: export_named(*export)?,
                row: Some(name.clone()),
                defaults: false,
            };
            Some((head, Vec::new()))
        }
    }
}

/// Writes out a change that waits for an edit beside it, from where its anchor sits in the
/// original. One that no path reaches is a note.
fn place_follow_up(
    original: &ParsedPackage,
    follow: FollowUp,
    becomes: &[Becomes],
    paths: &mut Vec<PathEdit>,
    notes: &mut Vec<String>,
) {
    let unplaced = |label: &str| {
        format!(
            "{label}: this waits for another edit in the save, and no path names where it goes; \
             dump the saved copy and diff again to make it"
        )
    };
    match follow {
        FollowUp::Edit {
            anchor,
            steps,
            op,
            becomes,
            label,
        } => match place_anchor(original, &anchor) {
            Some((head, mut at)) => {
                at.extend(steps);
                paths.push(PathEdit {
                    becomes,
                    ..head.edit(&at, op)
                });
            }
            None => notes.push(unplaced(&label)),
        },
        FollowUp::Fill {
            anchor,
            steps,
            edited,
            label,
        } => match place_anchor(original, &anchor) {
            Some((head, mut at)) => {
                at.extend(steps);
                fill(&head, at, &edited, &label, false, paths, notes);
            }
            None => notes.push(unplaced(&label)),
        },
        FollowUp::Keyed { edit, label } => {
            let path = match &edit {
                Keyed::Value(value) => value_path(original, value)
                    .filter(|path| placed_alone(original, path, value))
                    .map(|path| PathEdit {
                        becomes: becomes_of(becomes, value),
                        ..path
                    }),
                Keyed::Field(set) => field_set_path(original, set),
            };
            match path {
                Some(path) => paths.push(path),
                None => notes.push(unplaced(&label)),
            }
        }
    }
}

/// Path edits giving a value the same save makes what the edited dump says of it. What a save
/// makes starts unset all the way down, an element of a container at its type's zero, so a value
/// with a text form is set, a zero is cleared and an unset value left alone. A struct is filled
/// field by field, an array's elements are added and each filled, a set's elements are keyed in,
/// and a map's pairs are keyed in and each value filled by its key; one given nothing is stored as
/// it is. Each edit under one that makes its place waits for it, so a single apply makes them all.
/// What has no text form is a note. `zeroed` says the value is an element, which starts at zero.
fn fill(
    head: &Head,
    at: Vec<Segment>,
    edited: &Json,
    label: &str,
    zeroed: bool,
    paths: &mut Vec<PathEdit>,
    notes: &mut Vec<String>,
) {
    let kind = kind_in(edited).unwrap_or_default();
    let fields = edited.get("fields").and_then(Json::as_array);
    let started = paths.len();
    let with = |step: Segment| {
        let mut here = at.clone();
        here.push(step);
        here
    };
    match kind {
        // Left as a new value starts, though an unset struct can still list a field given one.
        "unset" => {
            for field in fields.into_iter().flatten() {
                fill_field(head, &at, field, label, paths, notes);
            }
            return;
        }
        "default" => {
            if !zeroed {
                paths.push(head.edit(&at, PathOp::Clear));
            }
            return;
        }
        "array" | "set" => {
            let items = edited.get("items").and_then(Json::as_array);
            let becomes = elements_in(edited);
            for (index, item) in items.into_iter().flatten().enumerate() {
                let here = format!("{label}[{index}]");
                let index = index as u32;
                if kind == "array" {
                    paths.push(PathEdit {
                        becomes: becomes.clone(),
                        ..head.edit(
                            &at,
                            PathOp::Insert {
                                index: Some(index),
                                key: None,
                            },
                        )
                    });
                    fill(
                        head,
                        with(Segment::Index(index)),
                        item,
                        &here,
                        true,
                        paths,
                        notes,
                    );
                    continue;
                }
                // A set's element is its own key.
                match text_of(item) {
                    Some(key) => paths.push(head.edit(
                        &at,
                        PathOp::Insert {
                            index: Some(index),
                            key: Some(key),
                        },
                    )),
                    None => notes.push(format!(
                        "{here}: this set element has no text form to key it by; add it in the \
                         editor instead"
                    )),
                }
            }
        }
        "map" => {
            let pairs = edited.get("entries").and_then(Json::as_array);
            for (index, pair) in pairs.into_iter().flatten().enumerate() {
                let here = format!("{label}[{index}]");
                let Some(key) = pair.get("key").and_then(text_of) else {
                    notes.push(format!(
                        "{here}: this pair's key has no text form to key it by; add it in the \
                         editor instead"
                    ));
                    continue;
                };
                paths.push(head.edit(
                    &at,
                    PathOp::Insert {
                        index: Some(index as u32),
                        key: Some(key.clone()),
                    },
                ));
                if let Some(value) = pair.get("value") {
                    fill(
                        head,
                        with(Segment::Key(key)),
                        value,
                        &here,
                        true,
                        paths,
                        notes,
                    );
                }
            }
        }
        _ if fields.is_some() => {
            for field in fields.into_iter().flatten() {
                fill_field(head, &at, field, label, paths, notes);
            }
        }
        _ => {
            let text = match kind {
                "text" => text_literal_of(edited),
                _ => text_of(edited),
            };
            match text {
                Some(_) if zeroed && is_zero(edited) => {}
                Some(text) => paths.push(head.edit(&at, PathOp::Set { text })),
                None => notes.push(format!(
                    "{label}: a {kind} has no text form an edit can carry, so it keeps what a new \
                     one holds"
                )),
            }
        }
    }
    // Given nothing inside, it is still stored, as the dump has it.
    if matches!(kind, "struct" | "array" | "set" | "map") && paths.len() == started && !zeroed {
        paths.push(head.edit(&at, PathOp::Store));
    }
}

/// [`fill`] for one field of a struct, under its name and its slot for a static array.
fn fill_field(
    head: &Head,
    at: &[Segment],
    field: &Json,
    label: &str,
    paths: &mut Vec<PathEdit>,
    notes: &mut Vec<String>,
) {
    let (Some(name), Some(value)) = (field.get("name").and_then(Json::as_str), field.get("value"))
    else {
        return;
    };
    let element = field
        .get("element")
        .and_then(Json::as_u64)
        .map(|at| at as u32);
    let mut here = at.to_vec();
    here.extend(field_steps(name, element));
    let named = match element {
        Some(slot) => format!("{label}.{name}[{slot}]"),
        None => format!("{label}.{name}"),
    };
    fill(head, here, value, &named, false, paths, notes);
}

/// Whether a dumped value is its kind's zero, which a new element of a container already holds.
fn is_zero(edited: &Json) -> bool {
    let value = edited.get("value");
    match kind_in(edited).unwrap_or_default() {
        "bool" => value.and_then(Json::as_bool) == Some(false),
        "int" | "uint" | "byte" | "float" | "enum" => value.and_then(Json::as_f64) == Some(0.0),
        "str" => value.and_then(Json::as_str) == Some(""),
        "name" => value.and_then(Json::as_str) == Some("None"),
        _ => false,
    }
}

/// What a dumped text is typed as to make it again: a string table entry or a localized text by
/// the literal that names it, anything else by what it shows. `None` for a text built from parts
/// no literal spells.
fn text_literal_of(edited: &Json) -> Option<String> {
    use rivals_uasset::text_literal::{TextLiteral, format};
    let field = |name: &str| edited.get(name).and_then(Json::as_str);
    let parts = edited
        .get("parts")
        .and_then(Json::as_array)
        .filter(|parts| !parts.is_empty());
    if let Some(parts) = parts {
        let part = |name: &str| {
            parts
                .iter()
                .find(|part| part.get("name").and_then(Json::as_str) == Some(name))
                .and_then(|part| part.get("value"))
                .and_then(text_of)
        };
        return Some(format(&TextLiteral::Table {
            table_id: part("TableId")?,
            key: part("Key")?,
        }));
    }
    match field("namespace") {
        Some(namespace) => Some(format(&TextLiteral::Localized {
            namespace: namespace.to_string(),
            key: field("key").unwrap_or_default().to_string(),
            source: field("value").unwrap_or_default().to_string(),
        })),
        None => text_of(edited),
    }
}

/// Whether a path edit is placed on the original as exactly the value edit it was made from.
fn placed_alone(original: &ParsedPackage, path: &PathEdit, edit: &ValueEdit) -> bool {
    let changes = PackageEdits {
        paths: vec![path.clone()],
        ..Default::default()
    };
    rivals_uasset::lower_paths(original, &changes).is_ok_and(|lowered| {
        lowered.values == [edit.clone()]
            && lowered.field_sets.is_empty()
            && lowered.waiting.is_empty()
            && lowered.notes.is_empty()
    })
}

/// Whether a field set's path edit is placed on the original as that field set, or waits on an
/// edit beside it that makes what it goes into, as the field set would have.
fn set_placed(
    original: &ParsedPackage,
    path: &PathEdit,
    set: &FieldSet,
    placed: &[PathEdit],
    rows: &[RowEdit],
) -> bool {
    let beside = placed.iter().filter(|other| {
        other.export == path.export && other.row == path.row && other.defaults == path.defaults
    });
    let changes = PackageEdits {
        paths: beside.cloned().chain([path.clone()]).collect(),
        rows: rows.to_vec(),
        ..Default::default()
    };
    rivals_uasset::lower_paths(original, &changes)
        .is_ok_and(|lowered| lowered.waiting.contains(path) || lowered.field_sets == [set.clone()])
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
            // Named by the path it had, which still finds it once a patch has moved the table.
            Some(was) => out.edits.imports.push(ImportEdit::Retarget {
                import: position as u32,
                path: path.to_string(),
                class: class_package
                    .zip(class_name)
                    .map(|(package, name)| (package.to_string(), name.to_string())),
                from: Some(was.path.clone()),
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
    let rows_before = out.edits.rows.len();
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
                    object: None,
                });
                // Its columns are filled in once the row is there, in the same save.
                let fields = row.get("fields").and_then(Json::as_array);
                for field in fields.into_iter().flatten() {
                    let (Some(column), Some(value)) =
                        (field.get("name").and_then(Json::as_str), field.get("value"))
                    else {
                        continue;
                    };
                    let element = field
                        .get("element")
                        .and_then(Json::as_u64)
                        .map(|at| at as u32);
                    out.follow.push(FollowUp::Fill {
                        anchor: Anchor::Row {
                            export,
                            name: name.to_string(),
                        },
                        steps: field_steps(column, element),
                        edited: value.clone(),
                        label: format!("{}[{name}].{column}", was.path),
                    });
                }
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
                object: None,
            });
        }
    }
    // Rows are named, and so is their table, which still finds it once a patch has moved it.
    named_by(was, &mut out.edits.rows[rows_before..], |edit| {
        &mut edit.object
    });
}

/// Names the object each of `edits` changes by its path below the package.
fn named_by<T>(
    was: &rivals_uasset::ParsedExport,
    edits: &mut [T],
    object: impl Fn(&mut T) -> &mut Option<String>,
) {
    for edit in edits {
        *object(edit) = Some(rivals_uasset::below_package(&was.path).to_string());
    }
}

/// String table entries are addressed by position and the key seen there, so they are compared
/// over the common prefix. Beyond it the edited table has entries to add or has dropped some.
fn diff_strings(was: &rivals_uasset::ParsedExport, is: &Json, export: u32, out: &mut DiffOutcome) {
    let Some(table) = &was.string_table else {
        return;
    };
    let strings_before = out.edits.strings.len();
    diff_entries_of(table, is, export, out);
    // Entries are found by key, and their table by path, once a patch has moved either.
    named_by(was, &mut out.edits.strings[strings_before..], |edit| {
        &mut edit.object
    });
}

/// [`diff_strings`] for the table's entries.
fn diff_entries_of(
    table: &rivals_uasset::StringTable,
    is: &Json,
    export: u32,
    out: &mut DiffOutcome,
) {
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
                object: None,
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
                object: None,
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
                object: None,
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
                object: None,
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
            object: None,
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
                object: None,
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
                object: None,
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
    let named = |held: &&Json, name: &str, element: Option<u32>| {
        held.get("name").and_then(Json::as_str) == Some(name)
            && held.get("element").and_then(Json::as_u64).map(|e| e as u32) == element
    };
    // An object whose class writes values of its own after its properties can list a name twice,
    // so each one is paired with the one in the same place among those so named.
    let mut seen: std::collections::BTreeMap<(&str, Option<u32>), usize> = Default::default();
    for entry in was {
        let element = entry.element;
        let nth = seen.entry((entry.name.as_str(), element)).or_default();
        let found = is
            .iter()
            .filter(|held| named(held, &entry.name, element))
            .nth(*nth);
        *nth += 1;
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
    let mut listed: std::collections::BTreeMap<(&str, Option<u32>), usize> = Default::default();
    for held in is {
        let name = held.get("name").and_then(Json::as_str).unwrap_or_default();
        let element = held.get("element").and_then(Json::as_u64).map(|e| e as u32);
        let nth = listed.entry((name, element)).or_default();
        *nth += 1;
        let held_here = was
            .iter()
            .filter(|entry| entry.name == name && entry.element == element)
            .count();
        if *nth > held_here {
            let label = match element {
                Some(at) => format!("{name}[{at}]"),
                None => name.to_string(),
            };
            out.notes.push(format!(
                "{owner}.{label}: the package has no such property here, so it is not added"
            ));
        }
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
    let is = kind_in(edited).unwrap_or_default();
    let label = format!("{owner}.{}", entry.label());

    if was != is {
        return diff_retyped(entry, edited, export, &label, &was, is, out);
    }
    match &entry.value {
        PropertyValue::Array { items } | PropertyValue::Set { items } => {
            diff_items(entry, items, edited, export, &label, out);
        }
        PropertyValue::Map { entries } => diff_map(entry, entries, edited, export, &label, out),
        PropertyValue::Unset { fields, .. } | PropertyValue::Default { fields, .. } => {
            if let Some(edited) = edited.get("fields").and_then(Json::as_array) {
                field_sets_in(entry, fields, edited, &[], &label, out);
            }
        }
        value => diff_in_place(entry, Whole::Value, value, edited, export, &label, out),
    }
}

/// Where a value typed whole lands: the property itself, or one half of an element of the
/// container `entry` is, addressed through the container.
#[derive(Debug, Clone, Copy)]
enum Whole {
    Value,
    Element(u32),
    Key(u32),
}

/// A value that keeps its place, against its edited form: a property, or either half of a
/// container's element. A struct recurses through its fields and a text through its parts, which
/// have offsets of their own; anything else is typed whole, and a change no text can carry is a
/// note.
fn diff_in_place(
    entry: &PropertyEntry,
    whole: Whole,
    before: &PropertyValue,
    edited: &Json,
    export: u32,
    label: &str,
    out: &mut DiffOutcome,
) {
    let was = kind_of(before);
    let is = kind_in(edited).unwrap_or_default();
    if was != is {
        out.notes.push(format!(
            "{label}: it reads as a {is} in the edited dump and a {was} in the package; an \
             element is always its container's type"
        ));
        return;
    }
    match before {
        PropertyValue::Struct { name, fields } => {
            let typed = fields.first().filter(|field| field.name == TYPE_FIELD);
            let items = edited.get("fields").and_then(Json::as_array);
            // An instanced struct given another type: the type is the edit, and it then holds its
            // new type's defaults, which the fields the dump gives it fill in once it has landed.
            if let (Some(typed), Some(items)) = (typed, items)
                && let Some(now) = items
                    .iter()
                    .find(|item| item.get("name").and_then(Json::as_str) == Some(TYPE_FIELD))
                && now
                    .get("value")
                    .is_some_and(|value| differs(&typed.value, value))
            {
                let mut retyped = DiffOutcome::default();
                let typed = std::slice::from_ref(typed);
                diff_entries(
                    typed,
                    std::slice::from_ref(now),
                    export,
                    label,
                    &mut retyped,
                );
                out.notes.append(&mut retyped.notes);
                let step = match whole {
                    Whole::Value => Some(None),
                    Whole::Element(index) => {
                        Some(Some(rivals_uasset::element_segment(&entry.value, index)))
                    }
                    Whole::Key(_) => None,
                };
                let (Some(step), Some(anchor)) = (step, anchor_of(entry)) else {
                    // A map's key has no path to wait by.
                    out.edits.values.append(&mut retyped.edits.values);
                    if items.len() > 1 {
                        out.notes.push(format!(
                            "{label}: given another type, it holds that type's defaults; dump the \
                             saved copy and diff again to change its fields"
                        ));
                    }
                    return;
                };
                for edit in retyped.edits.values {
                    out.follow.push(FollowUp::Keyed {
                        edit: Keyed::Value(edit),
                        label: label.to_string(),
                    });
                }
                for item in items {
                    let (Some(name), Some(value)) =
                        (item.get("name").and_then(Json::as_str), item.get("value"))
                    else {
                        continue;
                    };
                    if name == TYPE_FIELD {
                        continue;
                    }
                    let element = item
                        .get("element")
                        .and_then(Json::as_u64)
                        .map(|at| at as u32);
                    let mut steps: Vec<Segment> = step.clone().into_iter().collect();
                    steps.extend(field_steps(name, element));
                    out.follow.push(FollowUp::Fill {
                        anchor: anchor.clone(),
                        steps,
                        edited: value.clone(),
                        label: format!("{label}.{name}"),
                    });
                }
                return;
            }
            if let Some(now) = edited.get("name").and_then(Json::as_str)
                && now != name
                && typed.is_none()
            {
                out.notes.push(format!(
                    "{label}: a struct's type is the property's, so it stays a {name} rather than \
                     a {now}"
                ));
            }
            match edited.get("fields").and_then(Json::as_array) {
                Some(items) => diff_entries(fields, items, export, label, out),
                None => out.notes.push(format!(
                    "{label}: the edited struct lists no fields, so nothing in it is changed"
                )),
            }
        }
        PropertyValue::Text { .. } => diff_text(entry, whole, before, edited, export, label, out),
        PropertyValue::Undecoded { bytes, .. } => {
            let now = edited.get("bytes").and_then(Json::as_u64);
            if now != Some(*bytes) {
                out.notes.push(format!(
                    "{label}: this payload did not decode, so the dump does not hold its bytes; \
                     replace them with asset set --op set-raw, or a set_raw edit"
                ));
            }
        }
        _ => match edited_text(before, edited) {
            Some(text) => {
                if text != before.summary() {
                    push_edit(entry, whole, text, label, out);
                }
            }
            None => {
                if differs(before, edited) {
                    out.notes.push(format!(
                        "{label}: a {was} has no text form an edit can carry, so the change is not \
                         made"
                    ));
                }
            }
        },
    }
}

/// A text against its edited form. Its parts are edited at their own offsets; a changed value is
/// the whole text typed again, and a localized text given another namespace or key is a text with
/// another identity, which only its literal can spell.
fn diff_text(
    entry: &PropertyEntry,
    whole: Whole,
    before: &PropertyValue,
    edited: &Json,
    export: u32,
    label: &str,
    out: &mut DiffOutcome,
) {
    let PropertyValue::Text {
        value,
        parts,
        namespace,
        key,
        ..
    } = before
    else {
        return;
    };
    let field = |name: &str| edited.get(name).and_then(Json::as_str);
    if !parts.is_empty() {
        let edits_before = out.edits.values.len();
        let edited_parts = edited.get("parts").and_then(Json::as_array);
        if let Some(items) = edited_parts {
            diff_entries(parts, items, export, label, out);
        }
        let Some(now) = field("value").filter(|now| Some(*now) != value.as_deref()) else {
            return;
        };
        // A changed value is the whole text typed again; changed parts already say the same thing
        // when the value is what they read as, and anything else is two answers.
        if out.edits.values.len() > edits_before {
            if let Some(read) = edited_parts.and_then(|items| parts_read_as(items))
                && read != now
            {
                out.notes.push(format!(
                    "{label}: both the text and its parts changed, and they disagree; the part \
                     edits are kept"
                ));
            }
            return;
        }
        // A pattern, a moment or a generator shows what its parts make, so only a part says what
        // changed. A number is typed whole.
        let built = rivals_uasset::text_literal::of_value(before).is_none();
        let number = parts.iter().any(|part| part.name == "SourceValue");
        if built && !number {
            out.notes.push(format!(
                "{label}: this text is built from parts, so it changes through one of them, not \
                 through what it shows"
            ));
            return;
        }
        return push_edit(entry, whole, now.to_string(), label, out);
    }
    if let (Some(namespace), Some(key)) = (namespace, key) {
        let now_namespace = field("namespace").unwrap_or(namespace);
        let now_key = field("key").unwrap_or(key);
        let now_value = field("value").map(str::to_string).or_else(|| value.clone());
        if now_namespace != namespace || now_key != key {
            let literal = rivals_uasset::text_literal::TextLiteral::Localized {
                namespace: now_namespace.to_string(),
                key: now_key.to_string(),
                source: now_value.unwrap_or_default(),
            };
            let text = rivals_uasset::text_literal::format(&literal);
            push_edit(entry, whole, text, label, out);
        } else if now_value != *value
            && let Some(now_value) = now_value
        {
            push_edit(entry, whole, now_value, label, out);
        }
        return;
    }
    let now = field("value").unwrap_or_default();
    if now != value.as_deref().unwrap_or_default() {
        push_edit(entry, whole, now.to_string(), label, out);
    }
}

/// A value typed whole, where `whole` says: at the property's own offset, or as an element of the
/// container `entry` is.
fn push_edit(
    entry: &PropertyEntry,
    whole: Whole,
    text: String,
    label: &str,
    out: &mut DiffOutcome,
) {
    let Some((offset, _)) = entry.span else {
        out.notes.push(format!(
            "{label}: the reader recorded no offset for it, so it cannot be addressed"
        ));
        return;
    };
    let op = match whole {
        Whole::Value => EditOp::Set { text },
        Whole::Element(index) => EditOp::SetElement { index, text },
        Whole::Key(index) => EditOp::SetKey { index, text },
    };
    out.edits.values.push(ValueEdit {
        offset,
        expect_name: entry.name.clone(),
        expect_element: entry.element,
        expect_kind: kind_of(&entry.value),
        op,
    });
}

/// What a text reads as from its edited parts: a string table entry's `Table:Key`, or a
/// transformed text's source.
fn parts_read_as(parts: &[Json]) -> Option<String> {
    let part = |name: &str| {
        parts
            .iter()
            .find(|part| part.get("name").and_then(Json::as_str) == Some(name))
            .and_then(|part| part.get("value"))
    };
    if let (Some(table), Some(key)) = (part("TableId"), part("Key")) {
        return Some(format!("{}:{}", text_of(table)?, text_of(key)?));
    }
    part("SourceText")?
        .get("value")
        .and_then(Json::as_str)
        .map(str::to_string)
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
    // An array nothing stores yet takes the dump's elements in the same save, each filled in
    // through its index.
    if matches!(was, "unset" | "default")
        && is == "array"
        && let Some(items) = edited
            .get("items")
            .and_then(Json::as_array)
            .filter(|items| !items.is_empty())
    {
        for position in 0..items.len() {
            out.edits.values.push(ValueEdit {
                offset,
                expect_name: entry.name.clone(),
                expect_element: entry.element,
                expect_kind: was.to_string(),
                op: EditOp::Insert {
                    index: position as u32,
                    key: None,
                },
            });
        }
        for (position, item) in items.iter().enumerate() {
            element_sets(entry, None, item, vec![format!("[{position}]")], label, out);
        }
        return;
    }
    // A struct or a container nothing stores yet is filled in from what the dump gives it, each
    // edit waiting for the one that makes its place, all in one save.
    if was == "unset" && text_of(edited).is_none() {
        out.follow.push(FollowUp::Fill {
            anchor: Anchor::Value {
                offset,
                name: entry.name.clone(),
                element: entry.element,
            },
            steps: Vec::new(),
            edited: edited.clone(),
            label: label.to_string(),
        });
        return;
    }
    // Fields given values inside a zero struct are set through it, which stores it on the way.
    if was == "default" && text_of(edited).is_none() {
        let preview = match &entry.value {
            PropertyValue::Unset { fields, .. } | PropertyValue::Default { fields, .. } => {
                &fields[..]
            }
            _ => &[],
        };
        if let Some(fields) = edited.get("fields").and_then(Json::as_array)
            && field_sets_in(entry, preview, fields, &[], label, out) > 0
        {
            return;
        }
    }
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

/// The values the edited dump gives fields inside a struct that stores nothing yet, as field sets
/// addressed through the struct. `preview` is what the reader showed of the struct's fields, which
/// is how a deeper unset struct is told from a value. Returns how many it found.
fn field_sets_in(
    entry: &PropertyEntry,
    preview: &[PropertyEntry],
    edited: &[Json],
    path: &[String],
    label: &str,
    out: &mut DiffOutcome,
) -> usize {
    let Some((offset, _)) = entry.span else {
        return 0;
    };
    let mut found = 0;
    for field in edited {
        let Some(name) = field.get("name").and_then(Json::as_str) else {
            continue;
        };
        let element = field
            .get("element")
            .and_then(Json::as_u64)
            .map(|at| at as u32);
        let Some(value) = field.get("value") else {
            continue;
        };
        let segment = match element {
            Some(at) => format!("{name}[{at}]"),
            None => name.to_string(),
        };
        let mut here = path.to_vec();
        here.push(segment);
        let held = preview
            .iter()
            .find(|held| held.name == name && held.element == element);
        // A field the dump shows as it already reads is not a change.
        if held.is_some_and(|held| !differs(&held.value, value)) {
            continue;
        }
        let inner = held
            .and_then(|held| match &held.value {
                PropertyValue::Unset { fields, .. } | PropertyValue::Default { fields, .. } => {
                    Some(&fields[..])
                }
                _ => None,
            })
            .unwrap_or(&[]);
        match kind_in(value) {
            Some("unset" | "struct" | "default") => {
                if let Some(nested) = value.get("fields").and_then(Json::as_array) {
                    found += field_sets_in(entry, inner, nested, &here, label, out);
                }
            }
            None => {}
            Some(_) => match text_of(value) {
                Some(text) => {
                    out.edits.field_sets.push(rivals_uasset::FieldSet {
                        offset,
                        expect_name: entry.name.clone(),
                        expect_element: entry.element,
                        path: here,
                        text,
                    });
                    found += 1;
                }
                None => out.notes.push(format!(
                    "{label}.{}: a value inside a struct not stored yet can only be set from text",
                    here.join(".")
                )),
            },
        }
    }
    found
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
    let was: Vec<Json> = items.iter().map(dumped).collect();
    if let Some(order) = reordered(&was, now) {
        record_becomes(entry, edited, out);
        return push_reorder(entry, order, label, out);
    }
    if now.len() != items.len() {
        record_becomes(entry, edited, out);
    }
    for (position, (before, after)) in items.iter().zip(now).enumerate() {
        let at = Whole::Element(position as u32);
        let here = format!("{label}[{position}]");
        diff_in_place(entry, at, before, after, export, &here, out);
    }
    let Some(offset) = entry.span.map(|(start, _)| start) else {
        return;
    };
    let kind = kind_of(&entry.value);
    let is_set = matches!(entry.value, PropertyValue::Set { .. });
    // An element added to the end of an array starts as a copy of the last one read.
    let source = items.last();
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
        if is_set && key.is_none() {
            out.notes.push(format!(
                "{label}[{position}]: a new element takes the element type's default; dump the \
                 saved copy and diff again to give it a value"
            ));
        } else if !is_set {
            element_sets(
                entry,
                source,
                added,
                vec![format!("[{position}]")],
                label,
                out,
            );
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

/// Field sets that give an element the same save adds what the edited dump holds, addressed
/// through `path` from the container. `source` is what the element starts as: a copy of the one
/// read before it, or the type's default when there is none. Returns whether every change could be
/// expressed; a note says why for any that could not.
fn element_sets(
    entry: &PropertyEntry,
    source: Option<&PropertyValue>,
    edited: &Json,
    path: Vec<String>,
    label: &str,
    out: &mut DiffOutcome,
) -> bool {
    let Some((offset, _)) = entry.span else {
        return false;
    };
    let kind = kind_in(edited).unwrap_or_default();
    let stored = source.filter(|was| {
        !matches!(
            was,
            PropertyValue::Unset { .. } | PropertyValue::Default { .. }
        )
    });
    let at = || {
        let rest: String = path
            .iter()
            .map(|segment| match segment.starts_with('[') {
                true => segment.clone(),
                false => format!(".{segment}"),
            })
            .collect();
        format!("{label}{rest}")
    };
    if matches!(kind, "unset" | "default")
        && let Some(was) = stored
    {
        out.notes.push(format!(
            "{}: a new element starts as a copy holding {}, and a value can be given but not \
             taken away in the save that adds it",
            at(),
            was.summary()
        ));
        return false;
    }
    if matches!(kind, "struct" | "unset" | "default")
        && let Some(fields) = edited.get("fields").and_then(Json::as_array)
    {
        let before: &[PropertyEntry] = match source {
            Some(
                PropertyValue::Struct { fields, .. }
                | PropertyValue::Unset { fields, .. }
                | PropertyValue::Default { fields, .. },
            ) => fields,
            _ => &[],
        };
        let mut complete = true;
        for field in fields {
            let (Some(name), Some(value)) =
                (field.get("name").and_then(Json::as_str), field.get("value"))
            else {
                continue;
            };
            let element = field
                .get("element")
                .and_then(Json::as_u64)
                .map(|at| at as u32);
            let was = before
                .iter()
                .find(|held| held.name == name && held.element == element)
                .map(|held| &held.value);
            let mut here = path.clone();
            here.push(match element {
                Some(at) => format!("{name}[{at}]"),
                None => name.to_string(),
            });
            complete &= element_sets(entry, was, value, here, label, out);
        }
        return complete;
    }
    if matches!(kind, "unset" | "default") {
        return true;
    }
    if let Some(text) = text_of(edited) {
        if stored.is_none_or(|was| text != was.summary()) {
            out.edits.field_sets.push(rivals_uasset::FieldSet {
                offset,
                expect_name: entry.name.clone(),
                expect_element: entry.element,
                path,
                text,
            });
        }
        return true;
    }
    // An array inside a new element starts as the copy's. Its elements are made over where they
    // are, and the ones it gains or loses are added or dropped at its end once the element is
    // there.
    if kind == "array"
        && let Some(now) = edited.get("items").and_then(Json::as_array)
        && let Some(anchor) = anchor_of(entry)
    {
        let copied: &[PropertyValue] = match stored {
            Some(PropertyValue::Array { items }) => items,
            _ => &[],
        };
        let steps = steps_of(&path);
        let becomes = elements_in(edited);
        let mut complete = true;
        for (index, item) in now.iter().enumerate() {
            let mut here = path.clone();
            here.push(format!("[{index}]"));
            match copied.get(index) {
                Some(was) if differs(was, item) => {
                    complete &= element_sets(entry, Some(was), item, here, label, out);
                }
                Some(_) => {}
                None => {
                    out.follow.push(FollowUp::Edit {
                        anchor: anchor.clone(),
                        steps: steps.clone(),
                        op: PathOp::Insert {
                            index: Some(index as u32),
                            key: None,
                        },
                        becomes: becomes.clone(),
                        label: at(),
                    });
                    // A new element is a copy of the last one the array held.
                    complete &= element_sets(entry, copied.last(), item, here, label, out);
                }
            }
        }
        for index in (now.len()..copied.len()).rev() {
            let mut steps = steps.clone();
            steps.push(Segment::Index(index as u32));
            out.follow.push(FollowUp::Edit {
                anchor: anchor.clone(),
                steps,
                op: PathOp::Remove,
                becomes: becomes.clone(),
                label: at(),
            });
        }
        return complete;
    }
    // A set or a map, or a value the loader binds, which a field set cannot write.
    let empty = ["items", "entries"].iter().any(|key| {
        edited
            .get(*key)
            .and_then(Json::as_array)
            .is_some_and(Vec::is_empty)
    });
    let same = match source {
        Some(was) => serde_json::to_value(was).is_ok_and(|was| &was == edited),
        None => empty,
    };
    if !same {
        out.notes.push(format!(
            "{}: a {kind} inside a new element starts as the copy's; dump the saved copy and diff \
             again to change it",
            at()
        ));
    }
    same
}

/// A map against its edited form, pair by pair: each key and each value keeps its place.
fn diff_map(
    entry: &PropertyEntry,
    entries: &[rivals_uasset::MapEntry],
    edited: &Json,
    export: u32,
    label: &str,
    out: &mut DiffOutcome,
) {
    let Some(items) = edited.get("entries").and_then(Json::as_array) else {
        return;
    };
    if items.len() != entries.len() {
        out.notes.push(format!(
            "{label}: the map holds {} entries and the edited dump {}; adding or dropping one is \
             keyed, so make it in the editor rather than in the dump",
            entries.len(),
            items.len()
        ));
        return;
    }
    // Pairs that only moved are one reorder. Keys that moved while values changed are a reorder
    // too, since a value set by position would land on the wrong key; the values follow by key once
    // the pairs have moved.
    let pairs: Vec<Json> = entries.iter().map(dumped).collect();
    if let Some(order) = reordered(&pairs, items) {
        record_becomes(entry, edited, out);
        return push_reorder(entry, order, label, out);
    }
    let keys: Vec<Json> = entries.iter().map(|pair| dumped(&pair.key)).collect();
    let now_keys: Vec<Json> = items
        .iter()
        .map(|item| item.get("key").cloned().unwrap_or_default())
        .collect();
    if let Some(order) = reordered(&keys, &now_keys) {
        record_becomes(entry, edited, out);
        let by_key = (0..entries.len() as u32).all(|index| {
            matches!(
                rivals_uasset::element_segment(&entry.value, index),
                Segment::Key(_)
            )
        });
        if !by_key {
            push_reorder(entry, order, label, out);
            out.notes.push(format!(
                "{label}: its pairs moved and some of their values changed, and not every key has \
                 a text form to name its pair by; this save moves them, and a dump of the saved \
                 copy diffed again changes the values"
            ));
            return;
        }
        let mut moved = DiffOutcome::default();
        push_reorder(entry, order.clone(), label, &mut moved);
        for (position, from) in order.iter().enumerate() {
            let (Some(pair), Some(value)) = (
                entries.get(*from as usize),
                items.get(position).and_then(|item| item.get("value")),
            ) else {
                continue;
            };
            let here = format!("{label}[{position}]");
            diff_in_place(
                entry,
                Whole::Element(*from),
                &pair.value,
                value,
                export,
                &here,
                &mut moved,
            );
        }
        out.notes.append(&mut moved.notes);
        out.follow.append(&mut moved.follow);
        let keyed = moved
            .edits
            .values
            .into_iter()
            .map(Keyed::Value)
            .chain(moved.edits.field_sets.into_iter().map(Keyed::Field));
        for edit in keyed {
            out.follow.push(FollowUp::Keyed {
                edit,
                label: label.to_string(),
            });
        }
        return;
    }
    let held: Vec<Json> = now_keys.iter().map(content).collect();
    let keys_unique = !(1..held.len()).any(|at| held[..at].contains(&held[at]));
    if !keys_unique {
        out.notes.push(format!(
            "{label}: two pairs hold the same key in the edited dump, which a map cannot; its \
             keys are left as they are"
        ));
    }
    for (position, (before, now)) in entries.iter().zip(items).enumerate() {
        let at = position as u32;
        if let Some(key) = now.get("key").filter(|_| keys_unique) {
            let here = format!("{label}[{position}].key");
            diff_in_place(entry, Whole::Key(at), &before.key, key, export, &here, out);
        }
        if let Some(value) = now.get("value") {
            let here = format!("{label}[{position}]");
            diff_in_place(
                entry,
                Whole::Element(at),
                &before.value,
                value,
                export,
                &here,
                out,
            );
        }
    }
}

/// Whether the edited value says something the original does not: through the text an edit
/// would carry where it has one, and otherwise through everything the dump says of it.
fn differs(before: &PropertyValue, edited: &Json) -> bool {
    match edited_text(before, edited) {
        Some(text) => text != before.summary(),
        None => dumped(before) != content(edited),
    }
}

/// A value as the dump wrote it, less what it shows only for reading.
fn dumped(value: &impl serde::Serialize) -> Json {
    content(&serde_json::to_value(value).unwrap_or_default())
}

/// The order the edited list holds the original elements in, when it holds exactly them and has
/// moved some: `order[new_position]` is where the element there was read. A repeated element keeps
/// its place among its twins. `None` when anything changed besides the order, or nothing did.
fn reordered(was: &[Json], now: &[Json]) -> Option<Vec<u32>> {
    if was.len() != now.len() {
        return None;
    }
    let mut taken = vec![false; was.len()];
    let mut order = Vec::with_capacity(now.len());
    for item in now {
        let item = content(item);
        let at = (0..was.len()).find(|&at| !taken[at] && was[at] == item)?;
        taken[at] = true;
        order.push(at as u32);
    }
    let moved = order.iter().enumerate().any(|(at, old)| at as u32 != *old);
    moved.then_some(order)
}

/// A container's elements put in another order, in one edit at its own offset.
fn push_reorder(entry: &PropertyEntry, order: Vec<u32>, label: &str, out: &mut DiffOutcome) {
    let Some((offset, _)) = entry.span else {
        out.notes.push(format!(
            "{label}: the reader recorded no offset for it, so it cannot be addressed"
        ));
        return;
    };
    out.edits.values.push(ValueEdit {
        offset,
        expect_name: entry.name.clone(),
        expect_element: entry.element,
        expect_kind: kind_of(&entry.value),
        op: EditOp::Reorder { order },
    });
}

/// The text an edited value is typed as. Where the dump shows a value two ways, the one that
/// changed is the one meant: an enum's number under the name it had, or an object's index under
/// the path it had.
fn edited_text(before: &PropertyValue, edited: &Json) -> Option<String> {
    let field = |name: &str| edited.get(name);
    match before {
        PropertyValue::Enum { value, name, .. } => {
            let now = field("value").and_then(Json::as_i64);
            if field("name").and_then(Json::as_str) == name.as_deref()
                && let Some(now) = now.filter(|now| now != value)
            {
                return Some(now.to_string());
            }
        }
        PropertyValue::Object { index, path } => {
            let now = field("index").and_then(Json::as_i64);
            if field("path").and_then(Json::as_str) == path.as_deref()
                && let Some(now) = now.filter(|now| *now != i64::from(*index))
            {
                return Some(match now {
                    0 => "None".to_string(),
                    now => now.to_string(),
                });
            }
        }
        _ => {}
    }
    text_of(edited)
}

/// What a dump says of a value, less what it shows only for reading: where it sits and what a
/// lookup displays.
fn content(value: &Json) -> Json {
    match value {
        Json::Object(fields) => Json::Object(
            fields
                .iter()
                .filter(|(name, _)| !matches!(name.as_str(), "span" | "display"))
                .map(|(name, value)| (name.clone(), content(value)))
                .collect(),
        ),
        Json::Array(items) => Json::Array(items.iter().map(content).collect()),
        other => other.clone(),
    }
}

/// The kind a dump names a value by. Dumps from before unsigned integers were named as one word
/// call them `u_int`.
fn kind_in(edited: &Json) -> Option<&str> {
    match edited.get("kind").and_then(Json::as_str)? {
        "u_int" => Some("uint"),
        kind => Some(kind),
    }
}

/// The text form of an edited value, the way a `Set` carries it. `None` for a value with no single
/// text form, which is every container and struct.
fn text_of(edited: &Json) -> Option<String> {
    let kind = kind_in(edited)?;
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
        "delegate" => {
            let function = field("function")?.as_str()?;
            match field("object").and_then(Json::as_str) {
                Some(object) => format!("{object}::{function}"),
                None if function == "None" => "None".to_string(),
                None => format!("None::{function}"),
            }
        }
        "field_path" => {
            let path = field("path")?.as_str()?;
            match field("owner").and_then(Json::as_str) {
                Some(owner) => format!("{path} in {owner}"),
                None => path.to_string(),
            }
        }
        "lazy_object" => field("guid")?.as_str()?.to_string(),
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

        let defaulted = entry(
            "Count",
            PropertyValue::Default {
                declared: None,
                fields: Vec::new(),
            },
            0x40,
        );
        let out = one(defaulted, json!({"kind": "int", "value": 5}));
        assert!(matches!(&value(&out).op, EditOp::Set { text } if text == "5"));

        let unset = entry(
            "Count",
            PropertyValue::Unset {
                declared: "Int",
                enum_type: None,
                fields: Vec::new(),
            },
            0x40,
        );
        let out = one(unset, json!({"kind": "int", "value": 5}));
        assert_eq!(value(&out).expect_kind, "unset");
    }

    /// An unset struct has to be stored before its fields can be filled in, so the diff says so
    /// in the same save, so one given nothing inside is stored as it is.
    #[test]
    fn an_unset_struct_given_its_stored_form_is_filled_in() {
        let out = one(
            entry(
                "Where",
                PropertyValue::Unset {
                    declared: "Struct",
                    enum_type: None,
                    fields: Vec::new(),
                },
                0x40,
            ),
            json!({"kind": "struct", "name": "Vector", "fields": []}),
        );
        assert!(out.edits.is_empty(), "{:?}", out.edits);
        assert!(out.notes.is_empty(), "{:?}", out.notes);
        assert_eq!(waiting(&out), [(String::new(), "fill struct".to_string())]);
        let (paths, notes) = filled(json!({"kind": "struct", "name": "Vector", "fields": []}));
        assert_eq!(paths, [("Col".to_string(), PathOp::Store)]);
        assert!(notes.is_empty());
    }

    /// A value the same save makes starts unset all the way down, and an element of a container at
    /// its type's zero, so a fill sets what has text, clears a zero and leaves the rest. A
    /// container's elements go in first and are filled once they are there, a map's by key.
    #[test]
    fn a_value_made_in_the_same_save_is_filled_from_its_dump() {
        let set = |text: &str| PathOp::Set { text: text.into() };
        let insert = |index: u32, key: Option<&str>| PathOp::Insert {
            index: Some(index),
            key: key.map(str::to_string),
        };
        let (paths, notes) = filled(json!({"kind": "struct", "name": "Row", "fields": [
            {"name": "A", "value": {"kind": "int", "value": 3}},
            {"name": "B", "value": {"kind": "default", "declared": "Int"}},
            {"name": "C", "value": {"kind": "unset", "declared": "Int"}},
            {"name": "D", "value": {"kind": "text", "value": "Hi", "namespace": "NS", "key": "K"}},
            {"name": "E", "value": {"kind": "array", "items": [
                {"kind": "int", "value": 0},
                {"kind": "int", "value": 5},
            ]}},
            {"name": "F", "value": {"kind": "map", "entries": [
                {"key": {"kind": "name", "value": "A.B"}, "value": {"kind": "int", "value": 1}},
            ]}},
            {"name": "G", "value": {"kind": "set", "items": [{"kind": "name", "value": "X"}]}},
            {"name": "H", "value": {"kind": "map", "entries": [
                {"key": {"kind": "struct", "name": "K", "fields": []}, "value": {"kind": "int", "value": 1}},
            ]}},
            {"name": "I", "element": 1, "value": {"kind": "struct", "name": "Empty", "fields": []}},
            {"name": "J", "value": {"kind": "array", "items": [
                {"kind": "struct", "name": "P", "fields": [
                    {"name": "X", "value": {"kind": "int", "value": 2}},
                    {"name": "Y", "value": {"kind": "unset", "declared": "Int"}},
                ]},
            ]}},
        ]}));
        let expected = [
            ("Col.A", set("3")),
            ("Col.B", PathOp::Clear),
            ("Col.D", set(r#"NSLOCTEXT("NS", "K", "Hi")"#)),
            ("Col.E", insert(0, None)),
            ("Col.E", insert(1, None)),
            ("Col.E[1]", set("5")),
            ("Col.F", insert(0, Some("A.B"))),
            ("Col.F{A.B}", set("1")),
            ("Col.G", insert(0, Some("X"))),
            ("Col.H", PathOp::Store),
            ("Col.I[1]", PathOp::Store),
            ("Col.J", insert(0, None)),
            ("Col.J[0].X", set("2")),
        ];
        assert_eq!(
            paths,
            expected
                .into_iter()
                .map(|(path, op)| (path.to_string(), op))
                .collect::<Vec<_>>()
        );
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].starts_with("Col.H[0]"), "{notes:?}");
    }

    /// An array inside a new element starts as the copy's: an element that differs is set where it
    /// is, and one the dump adds goes in once the new element is there, then takes its value.
    #[test]
    fn an_array_inside_a_new_element_is_made_over_in_the_same_save() {
        let holder = |inner: Vec<i64>| PropertyValue::Struct {
            name: "Holder".into(),
            fields: vec![entry(
                "Inner",
                PropertyValue::Array {
                    items: inner
                        .into_iter()
                        .map(|value| PropertyValue::Int { value })
                        .collect(),
                },
                0x60,
            )],
        };
        let dumped_holder = |inner: &[i64]| {
            json!({"kind": "struct", "name": "Holder", "fields": [
                {"name": "Inner", "value": {"kind": "array", "items": inner
                    .iter()
                    .map(|value| json!({"kind": "int", "value": value}))
                    .collect::<Vec<_>>()}},
            ]})
        };
        let out = one(
            array(vec![holder(vec![1, 2])]),
            json!({"kind": "array", "items": [dumped_holder(&[1, 2]), dumped_holder(&[1, 7, 9])]}),
        );
        assert!(out.notes.is_empty(), "{:?}", out.notes);
        assert!(matches!(
            value(&out).op,
            EditOp::Insert {
                index: 1,
                key: None
            }
        ));
        assert_eq!(
            sets_of(&out),
            [
                ("[1].Inner.[1]".to_string(), "7"),
                ("[1].Inner.[2]".to_string(), "9")
            ]
        );
        assert_eq!(
            waiting(&out),
            [(
                "[1].Inner".to_string(),
                format!(
                    "{:?}",
                    PathOp::Insert {
                        index: Some(2),
                        key: None
                    }
                )
            )]
        );

        // One that loses elements drops them from its end.
        let out = one(
            array(vec![holder(vec![1, 2, 3])]),
            json!({"kind": "array", "items": [dumped_holder(&[1, 2, 3]), dumped_holder(&[1])]}),
        );
        assert!(out.notes.is_empty(), "{:?}", out.notes);
        let removed: Vec<String> = waiting(&out).into_iter().map(|(path, _)| path).collect();
        assert_eq!(removed, ["[1].Inner[2]", "[1].Inner[1]"]);
    }

    /// A name listed twice, as an object whose class writes values of its own after its properties
    /// lists it, pairs each with its own, so an untouched list is no change.
    #[test]
    fn a_name_listed_twice_pairs_each_with_its_own() {
        let was = [
            entry(
                "NavListStart",
                PropertyValue::Unset {
                    declared: "Object",
                    enum_type: None,
                    fields: Vec::new(),
                },
                0x40,
            ),
            entry(
                "NavListStart",
                PropertyValue::Object {
                    index: 3,
                    path: Some("/Game/Map.Map:PersistentLevel.Start".into()),
                },
                0x80,
            ),
        ];
        let listed: Vec<Json> = was
            .iter()
            .map(|entry| json!({"name": entry.name, "value": dumped(&entry.value)}))
            .collect();
        let mut out = DiffOutcome::default();
        diff_entries(&was, &listed, 0, "/Game/Map.Map", &mut out);
        assert!(out.edits.is_empty(), "{:?}", out.edits);
        assert!(out.notes.is_empty(), "{:?}", out.notes);

        // A third one is more than the package holds.
        let mut more = listed.clone();
        more.push(listed[1].clone());
        let mut out = DiffOutcome::default();
        diff_entries(&was, &more, 0, "/Game/Map.Map", &mut out);
        assert_eq!(out.notes.len(), 1, "{:?}", out.notes);
        assert!(out.notes[0].contains("no such property"), "{:?}", out.notes);
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

    /// A longer list inserts at the end, each new element given its value through its index in the
    /// same save; a shorter one removes from the end down, so an earlier removal does not move a
    /// later one.
    #[test]
    fn a_container_grows_by_inserts_and_shrinks_by_removes() {
        let out = one(
            array(vec![PropertyValue::Str { value: "a".into() }]),
            json!({"kind": "array", "items": [
                {"kind": "str", "value": "a"},
                {"kind": "str", "value": "b"},
                {"kind": "str", "value": "a"},
            ]}),
        );
        assert!(matches!(
            out.edits.values[0].op,
            EditOp::Insert { index: 1, .. }
        ));
        assert!(out.notes.is_empty(), "{:?}", out.notes);
        let sets: Vec<(&[String], &str)> = out
            .edits
            .field_sets
            .iter()
            .map(|set| (&set.path[..], set.text.as_str()))
            .collect();
        assert_eq!(
            sets,
            [(&["[1]".to_string()][..], "b")],
            "the third starts as a copy of the last element read, which it already equals"
        );

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

    fn my_struct(x: i64, y: PropertyValue) -> PropertyValue {
        PropertyValue::Struct {
            name: "MyStruct".into(),
            fields: vec![
                entry("X", PropertyValue::Int { value: x }, 0x50),
                entry("Y", y, 0x54),
            ],
        }
    }

    /// The edits that wait to be placed by path, as their ops.
    fn keyed(out: &DiffOutcome) -> Vec<&EditOp> {
        out.follow
            .iter()
            .filter_map(|follow| match follow {
                FollowUp::Keyed {
                    edit: Keyed::Value(edit),
                    ..
                } => Some(&edit.op),
                _ => None,
            })
            .collect()
    }

    /// The fills and single edits that wait, as the steps below their anchor and what each does.
    fn waiting(out: &DiffOutcome) -> Vec<(String, String)> {
        out.follow
            .iter()
            .filter_map(|follow| match follow {
                FollowUp::Fill { steps, edited, .. } => Some((
                    rivals_uasset::format_path(steps),
                    format!("fill {}", kind_in(edited).unwrap_or_default()),
                )),
                FollowUp::Edit { steps, op, .. } => {
                    Some((rivals_uasset::format_path(steps), format!("{op:?}")))
                }
                FollowUp::Keyed { .. } => None,
            })
            .collect()
    }

    /// What [`fill`] makes of a dumped value at `Col` in a row the same save adds.
    fn filled(edited: Json) -> (Vec<(String, PathOp)>, Vec<String>) {
        let head = Head {
            export: "Table".into(),
            row: Some("New".into()),
            defaults: false,
        };
        let (mut paths, mut notes) = (Vec::new(), Vec::new());
        fill(
            &head,
            vec![Segment::Field("Col".into())],
            &edited,
            "Col",
            false,
            &mut paths,
            &mut notes,
        );
        assert!(paths.iter().all(|path| path.export == "Table"
            && path.row.as_deref() == Some("New")
            && path.was.is_none()));
        (
            paths.into_iter().map(|path| (path.path, path.op)).collect(),
            notes,
        )
    }

    fn sets_of(out: &DiffOutcome) -> Vec<(String, &str)> {
        out.edits
            .field_sets
            .iter()
            .map(|set| (set.path.join("."), set.text.as_str()))
            .collect()
    }

    /// A struct element added to an array is filled in through its index, for the fields that
    /// differ from the element it starts as a copy of.
    #[test]
    fn a_new_struct_element_is_filled_in_through_its_index() {
        let out = one(
            array(vec![my_struct(1, PropertyValue::Int { value: 2 })]),
            json!({"kind": "array", "items": [
                {"kind": "struct", "fields": [
                    {"name": "X", "value": {"kind": "int", "value": 1}},
                    {"name": "Y", "value": {"kind": "int", "value": 2}},
                ]},
                {"kind": "struct", "fields": [
                    {"name": "X", "value": {"kind": "int", "value": 1}},
                    {"name": "Y", "value": {"kind": "int", "value": 5}},
                ]},
            ]}),
        );
        assert!(matches!(
            value(&out).op,
            EditOp::Insert {
                index: 1,
                key: None
            }
        ));
        assert_eq!(sets_of(&out), [("[1].Y".to_string(), "5")]);
        assert!(out.notes.is_empty(), "{:?}", out.notes);
    }

    /// A field set only carries a value, so a new element's field the dump leaves unset while its
    /// copy stores one is noted rather than dropped silently.
    #[test]
    fn a_new_elements_field_left_unset_is_noted() {
        let out = one(
            array(vec![my_struct(1, PropertyValue::Int { value: 2 })]),
            json!({"kind": "array", "items": [
                {"kind": "struct", "fields": [
                    {"name": "X", "value": {"kind": "int", "value": 1}},
                    {"name": "Y", "value": {"kind": "int", "value": 2}},
                ]},
                {"kind": "struct", "fields": [
                    {"name": "X", "value": {"kind": "int", "value": 3}},
                    {"name": "Y", "value": {"kind": "unset", "declared": "Int"}},
                ]},
            ]}),
        );
        assert_eq!(sets_of(&out), [("[1].X".to_string(), "3")]);
        assert_eq!(out.notes.len(), 1, "{:?}", out.notes);
        assert!(out.notes[0].contains("Tags[1].Y"), "{}", out.notes[0]);
    }

    /// An array nothing stores yet takes the dump's elements in one save: an insert for each, and
    /// every value they hold, since they start as the type's default.
    #[test]
    fn an_unset_array_given_elements_inserts_and_fills_them() {
        let out = one(
            entry(
                "Tags",
                PropertyValue::Unset {
                    declared: "Array",
                    enum_type: None,
                    fields: Vec::new(),
                },
                0x30,
            ),
            json!({"kind": "array", "items": [
                {"kind": "struct", "fields": [
                    {"name": "X", "value": {"kind": "int", "value": 7}},
                    {"name": "Y", "value": {"kind": "unset", "declared": "Int"}},
                ]},
                {"kind": "struct", "fields": [
                    {"name": "X", "value": {"kind": "int", "value": 8}},
                ]},
            ]}),
        );
        let inserts: Vec<u32> = out
            .edits
            .values
            .iter()
            .map(|edit| match (&edit.op, edit.expect_kind.as_str()) {
                (EditOp::Insert { index, key: None }, "unset") => *index,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(inserts, [0, 1]);
        assert_eq!(
            sets_of(&out),
            [("[0].X".to_string(), "7"), ("[1].X".to_string(), "8")]
        );
        assert!(out.notes.is_empty(), "{:?}", out.notes);
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
                {"kind": "struct", "name": "Point"},
            ]}),
        );
        assert!(out.edits.values.is_empty(), "{:?}", out.edits.values);
        assert_eq!(out.notes.len(), 1);
    }

    /// A value given to a field inside an unset struct is set through the struct, which stores it
    /// on the way, rather than stored first and noted.
    #[test]
    fn a_field_inside_an_unset_struct_becomes_a_field_set() {
        let preview = PropertyEntry {
            name: "Amplitude".into(),
            element: None,
            value: PropertyValue::Unset {
                declared: "Float",
                enum_type: None,
                fields: Vec::new(),
            },
            span: None,
            slot: None,
        };
        let out = one(
            entry(
                "FOVOscillation",
                PropertyValue::Unset {
                    declared: "Struct",
                    enum_type: None,
                    fields: vec![preview],
                },
                0x40,
            ),
            json!({"kind": "unset", "declared": "Struct", "fields": [
                {"name": "Amplitude", "value": {"kind": "float", "value": 2.5}},
            ]}),
        );
        assert!(out.edits.values.is_empty(), "{:?}", out.edits.values);
        assert_eq!(
            out.edits.field_sets,
            vec![rivals_uasset::FieldSet {
                offset: 0x40,
                expect_name: "FOVOscillation".into(),
                expect_element: None,
                path: vec!["Amplitude".into()],
                text: "2.5".into(),
            }]
        );
        assert!(out.notes.is_empty(), "{:?}", out.notes);

        // A zero struct shows its fields the same way, and one given a value is set through it.
        let zero = PropertyEntry {
            name: "X".into(),
            element: None,
            value: PropertyValue::Float { value: 0.0 },
            span: None,
            slot: None,
        };
        let out = one(
            entry(
                "Offset",
                PropertyValue::Default {
                    declared: None,
                    fields: vec![zero],
                },
                0x50,
            ),
            json!({"kind": "default", "fields": [
                {"name": "X", "value": {"kind": "float", "value": 1.5}},
            ]}),
        );
        assert_eq!(out.edits.field_sets.len(), 1, "{:?}", out.edits);
        assert_eq!(out.edits.field_sets[0].path, vec!["X".to_string()]);
    }

    /// A delegate is bound by the loader rather than stored as text, so a changed one is reported
    /// and not written.
    const TABLE: &str = "/Game/UI/Menu_ST.Menu_ST";

    fn table_text(key: &str) -> PropertyEntry {
        let part = |name: &str, value: PropertyValue, at: u64| PropertyEntry {
            name: name.into(),
            element: None,
            span: Some((at, at + 8)),
            value,
            slot: None,
        };
        PropertyEntry {
            name: "Label".into(),
            element: None,
            span: Some((0x40, 0x60)),
            value: PropertyValue::Text {
                value: Some(format!("{TABLE}:{key}")),
                parts: vec![
                    part(
                        "TableId",
                        PropertyValue::Name {
                            value: TABLE.into(),
                        },
                        0x45,
                    ),
                    part("Key", PropertyValue::Str { value: key.into() }, 0x4D),
                ],
                namespace: None,
                key: None,
                display: None,
            },
            slot: None,
        }
    }

    fn table_json(value: &str, key: &str) -> Json {
        json!({"kind": "text", "value": value, "parts": [
            {"name": "TableId", "value": {"kind": "name", "value": TABLE}},
            {"name": "Key", "value": {"kind": "str", "value": key}},
        ]})
    }

    /// A string table text whose value was typed again is set whole at its own offset, so the
    /// encoder can repoint it or give it fixed text.
    #[test]
    fn a_table_text_s_changed_value_is_set_whole() {
        let out = one(table_text("Play"), table_json("Fixed label", "Play"));
        let edit = value(&out);
        assert_eq!(edit.offset, 0x40);
        assert_eq!(edit.expect_kind, "text");
        assert!(matches!(&edit.op, EditOp::Set { text } if text == "Fixed label"));
    }

    /// Parts and a value that say the same thing leave the part edits alone; ones that disagree
    /// keep the part edits and say so.
    #[test]
    fn a_table_text_s_parts_and_value_are_reconciled() {
        let agree = one(
            table_text("Play"),
            table_json(&format!("{TABLE}:Quit"), "Quit"),
        );
        let edit = value(&agree);
        assert_eq!(edit.offset, 0x4D);
        assert!(agree.notes.is_empty(), "{:?}", agree.notes);

        let disagree = one(table_text("Play"), table_json("Something else", "Quit"));
        assert_eq!(value(&disagree).offset, 0x4D);
        assert!(
            disagree.notes.iter().any(|note| note.contains("disagree")),
            "{:?}",
            disagree.notes
        );
    }

    /// A localized text given another namespace or key is a text with another identity, set as
    /// its literal; a changed source alone stays a plain edit.
    #[test]
    fn a_localized_text_s_new_key_is_set_as_its_literal() {
        let localized = entry(
            "Title",
            PropertyValue::Text {
                value: Some("Play".into()),
                parts: Vec::new(),
                namespace: Some("Menu".into()),
                key: Some("Play".into()),
                display: None,
            },
            0x10,
        );
        let out = one(
            localized.clone(),
            json!({"kind": "text", "value": "Play", "namespace": "Menu", "key": "Start"}),
        );
        assert!(matches!(
            &value(&out).op,
            EditOp::Set { text } if text == r#"NSLOCTEXT("Menu", "Start", "Play")"#
        ));
        let out = one(
            localized,
            json!({"kind": "text", "value": "Go", "namespace": "Menu", "key": "Play"}),
        );
        assert!(matches!(&value(&out).op, EditOp::Set { text } if text == "Go"));
    }

    /// A delegate, a field path and a lazy object are set from the text the dump renders them as.
    #[test]
    fn a_delegate_field_path_and_lazy_object_are_set_from_text() {
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
        assert!(matches!(&value(&out).op, EditOp::Set { text } if text == "/Game/A.A_C::Other"));
        let out = one(
            entry(
                "OnFired",
                PropertyValue::Delegate {
                    object: Some("/Game/A.A_C".into()),
                    function: "Handler".into(),
                },
                0x60,
            ),
            json!({"kind": "delegate", "function": "None"}),
        );
        assert!(matches!(&value(&out).op, EditOp::Set { text } if text == "None"));
        let out = one(
            entry(
                "Watched",
                PropertyValue::FieldPath {
                    path: "A".into(),
                    owner: Some("/Game/A.A_C".into()),
                },
                0x60,
            ),
            json!({"kind": "field_path", "path": "B", "owner": "/Game/B.B_C"}),
        );
        assert!(matches!(&value(&out).op, EditOp::Set { text } if text == "B in /Game/B.B_C"));
        let out = one(
            entry(
                "Lazy",
                PropertyValue::LazyObject {
                    guid: "0".repeat(32),
                },
                0x60,
            ),
            json!({"kind": "lazy_object", "guid": "1".repeat(32)}),
        );
        assert!(matches!(&value(&out).op, EditOp::Set { text } if *text == "1".repeat(32)));
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

    /// A dump written before unsigned integers were named as one word still diffs as the value it
    /// was: an unchanged one is no retype, and a changed one is set.
    #[test]
    fn an_old_dump_s_u_int_reads_as_uint() {
        let count = entry("Count", PropertyValue::UInt { value: 3 }, 0x40);
        let out = one(count.clone(), json!({"kind": "u_int", "value": 3}));
        assert!(out.edits.is_empty(), "{:?}", out.edits);
        assert!(out.notes.is_empty(), "{:?}", out.notes);
        let out = one(count, json!({"kind": "u_int", "value": 4}));
        assert!(matches!(&value(&out).op, EditOp::Set { text } if text == "4"));
        assert_eq!(value(&out).expect_kind, "uint");
    }

    /// Every kind of value, nested the ways a package nests them.
    fn every_kind() -> Vec<PropertyEntry> {
        let scalars = vec![
            PropertyValue::Bool { value: true },
            PropertyValue::Int { value: -3 },
            PropertyValue::UInt { value: 3 },
            PropertyValue::Float { value: 2.5 },
            PropertyValue::Byte { value: 7 },
            PropertyValue::Str { value: "a".into() },
            PropertyValue::Name { value: "A".into() },
            PropertyValue::Text {
                value: Some("Play".into()),
                parts: Vec::new(),
                namespace: Some("Menu".into()),
                key: Some("Play".into()),
                display: None,
            },
            PropertyValue::Enum {
                value: 1,
                name: Some("Tank".into()),
                enum_type: Some("EHeroRole".into()),
            },
            PropertyValue::Object {
                index: -1,
                path: Some("/Script/Engine.Actor".into()),
            },
            PropertyValue::SoftObject {
                path: "/Game/A.A".into(),
            },
            PropertyValue::Delegate {
                object: Some("/Game/A.A_C".into()),
                function: "OnFired".into(),
            },
            PropertyValue::FieldPath {
                path: "A.B".into(),
                owner: Some("/Game/A.A_C".into()),
            },
            PropertyValue::LazyObject {
                guid: "00000000000000000000000000000000".into(),
            },
            PropertyValue::Undecoded {
                reason: "no schema".into(),
                bytes: 4,
            },
            PropertyValue::Default {
                declared: None,
                fields: Vec::new(),
            },
            PropertyValue::Unset {
                declared: "Int",
                enum_type: None,
                fields: Vec::new(),
            },
        ];
        let mut entries: Vec<PropertyEntry> = scalars
            .iter()
            .enumerate()
            .map(|(at, value)| entry(&format!("P{at}"), value.clone(), at as u64 * 0x10))
            .collect();
        let point = || PropertyValue::Struct {
            name: "IntPoint".into(),
            fields: vec![
                entry("X", PropertyValue::Int { value: 1 }, 0x400),
                entry("Y", PropertyValue::Int { value: 2 }, 0x404),
            ],
        };
        entries.push(entry("Where", point(), 0x400));
        entries.push(entry(
            "List",
            PropertyValue::Array {
                items: vec![point(), point()],
            },
            0x500,
        ));
        entries.push(entry(
            "Names",
            PropertyValue::Set {
                items: vec![PropertyValue::Name { value: "A".into() }],
            },
            0x600,
        ));
        entries.push(entry(
            "Lookup",
            PropertyValue::Map {
                entries: scalars
                    .iter()
                    .filter(|value| {
                        !matches!(
                            value,
                            PropertyValue::Default { .. } | PropertyValue::Unset { .. }
                        )
                    })
                    .enumerate()
                    .map(|(at, value)| rivals_uasset::MapEntry {
                        key: PropertyValue::Name {
                            value: format!("K{at}"),
                        },
                        value: value.clone(),
                    })
                    .collect(),
            },
            0x700,
        ));
        entries.push(entry(
            "ByPoint",
            PropertyValue::Map {
                entries: vec![rivals_uasset::MapEntry {
                    key: point(),
                    value: PropertyValue::Int { value: 1 },
                }],
            },
            0x800,
        ));
        entries.push(table_text("Play"));
        // A float held as a 32-bit value, whose decimal form only reads back exactly when the
        // parser is exact.
        entries.push(entry(
            "Fraction",
            PropertyValue::Float {
                value: f64::from(0.968_249_9_f32),
            },
            0x900,
        ));
        // A struct stored as all zero shows its fields, which say nothing new.
        let zero = |name: &str| PropertyEntry {
            span: None,
            ..entry(name, PropertyValue::Float { value: 0.0 }, 0)
        };
        entries.push(entry(
            "Rotation",
            PropertyValue::Default {
                declared: Some("Struct"),
                fields: vec![zero("Pitch"), zero("Yaw"), zero("Roll")],
            },
            0x910,
        ));
        entries
    }

    /// A dump read back untouched says nothing at all, for every kind of value however it nests,
    /// once it has been written out as text and read in again.
    #[test]
    fn an_untouched_dump_of_every_kind_diffs_to_nothing() {
        let entries = every_kind();
        let json: Vec<Json> = entries
            .iter()
            .map(|entry| {
                let text = serde_json::to_string(entry).unwrap();
                serde_json::from_str(&text).unwrap()
            })
            .collect();
        let mut out = DiffOutcome::default();
        diff_entries(&entries, &json, 0, "/Game/Thing.Thing", &mut out);
        assert!(out.edits.is_empty(), "{:?}", out.edits);
        assert!(out.notes.is_empty(), "{:?}", out.notes);
    }

    /// An enum whose number changed under the name it had is set by the number, which is the half
    /// of the dump that was edited.
    #[test]
    fn an_enum_given_only_a_new_number_is_set_by_it() {
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
            json!({"kind": "enum", "value": 2, "name": "Tank", "enum_type": "EHeroRole"}),
        );
        assert!(matches!(&value(&out).op, EditOp::Set { text } if text == "2"));
    }

    /// An object whose index changed under the path it had is set by the index.
    #[test]
    fn an_object_given_only_a_new_index_is_set_by_it() {
        let out = one(
            entry(
                "Mesh",
                PropertyValue::Object {
                    index: -3,
                    path: Some("/Game/A.A".into()),
                },
                0x20,
            ),
            json!({"kind": "object", "index": -4, "path": "/Game/A.A"}),
        );
        assert!(matches!(&value(&out).op, EditOp::Set { text } if text == "-4"));
    }

    /// A struct element the dump lists without its fields, or as another type, says so rather
    /// than changing nothing in silence.
    #[test]
    fn a_struct_element_without_fields_is_noted() {
        let out = one(
            array(vec![my_struct(1, PropertyValue::Int { value: 2 })]),
            json!({"kind": "array", "items": [{"kind": "struct", "name": "Other"}]}),
        );
        assert!(out.edits.is_empty(), "{:?}", out.edits);
        assert_eq!(out.notes.len(), 2, "{:?}", out.notes);
        assert!(
            out.notes
                .iter()
                .any(|note| note.contains("lists no fields"))
        );
        assert!(
            out.notes
                .iter()
                .any(|note| note.contains("stays a MyStruct"))
        );
    }

    /// An element is always its container's type, so one given another kind is a note.
    #[test]
    fn an_element_given_another_kind_is_noted() {
        let out = one(
            array(vec![PropertyValue::Str { value: "a".into() }]),
            json!({"kind": "array", "items": [{"kind": "int", "value": 1}]}),
        );
        assert!(out.edits.is_empty(), "{:?}", out.edits);
        assert_eq!(out.notes.len(), 1, "{:?}", out.notes);
        assert!(out.notes[0].contains("Tags[0]"), "{}", out.notes[0]);
    }

    /// A multicast delegate's bindings are a list, and a changed one is set through its index.
    #[test]
    fn a_multicast_delegate_s_changed_element_is_set_by_index() {
        let delegate = |function: &str| PropertyValue::Delegate {
            object: Some("/Game/A.A_C".into()),
            function: function.into(),
        };
        let out = one(
            array(vec![delegate("Handler"), delegate("Other")]),
            json!({"kind": "array", "items": [
                {"kind": "delegate", "object": "/Game/A.A_C", "function": "Handler"},
                {"kind": "delegate", "object": null, "function": "Later"},
            ]}),
        );
        assert!(
            matches!(&value(&out).op, EditOp::SetElement { index: 1, text } if text == "None::Later"),
            "{:?}",
            out.edits
        );
    }

    /// A property the edited dump adds is not something a save can declare, so it is a note.
    #[test]
    fn a_property_the_package_does_not_declare_is_noted() {
        let mut out = DiffOutcome::default();
        diff_entries(
            &[entry("Count", PropertyValue::Int { value: 1 }, 0)],
            &[
                json!({"name": "Count", "value": {"kind": "int", "value": 1}}),
                json!({"name": "Extra", "value": {"kind": "int", "value": 2}}),
            ],
            0,
            "/Game/Thing.Thing",
            &mut out,
        );
        assert!(out.edits.is_empty(), "{:?}", out.edits);
        assert_eq!(out.notes.len(), 1, "{:?}", out.notes);
        assert!(out.notes[0].contains("Thing.Extra"), "{}", out.notes[0]);
    }

    /// A text inside a container is diffed the way a text property is: its parts at their own
    /// offsets, and a value typed again set whole through its index.
    #[test]
    fn a_text_element_s_parts_recurse_and_its_value_is_set_whole() {
        let labels = entry(
            "Labels",
            PropertyValue::Array {
                items: vec![table_text("Play").value],
            },
            0x30,
        );
        let out = one(
            labels.clone(),
            json!({"kind": "array", "items": [table_json(&format!("{TABLE}:Quit"), "Quit")]}),
        );
        let edit = value(&out);
        assert_eq!((edit.offset, edit.expect_name.as_str()), (0x4D, "Key"));

        let out = one(
            labels,
            json!({"kind": "array", "items": [table_json("Fixed", "Play")]}),
        );
        let edit = value(&out);
        assert_eq!(edit.offset, 0x30);
        assert!(
            matches!(&edit.op, EditOp::SetElement { index: 0, text } if text == "Fixed"),
            "{:?}",
            edit.op
        );
    }

    /// A localized text held as a map's value, given another key, is set as its literal through
    /// the pair's index.
    #[test]
    fn a_text_map_value_with_a_new_key_is_set_as_its_literal() {
        let localized = PropertyValue::Text {
            value: Some("Play".into()),
            parts: Vec::new(),
            namespace: Some("Menu".into()),
            key: Some("Play".into()),
            display: None,
        };
        let out = one(
            entry(
                "Titles",
                PropertyValue::Map {
                    entries: vec![rivals_uasset::MapEntry {
                        key: PropertyValue::Name { value: "A".into() },
                        value: localized,
                    }],
                },
                0x30,
            ),
            json!({"kind": "map", "entries": [{
                "key": {"kind": "name", "value": "A"},
                "value": {"kind": "text", "value": "Play", "namespace": "Menu", "key": "Start"},
            }]}),
        );
        assert!(matches!(
            &value(&out).op,
            EditOp::SetElement { index: 0, text } if text == r#"NSLOCTEXT("Menu", "Start", "Play")"#
        ));
    }

    /// A field of a map's struct key is edited at its own offset; the key as a whole keeps its
    /// place.
    #[test]
    fn a_struct_key_s_field_is_set_at_its_own_offset() {
        let out = one(
            entry(
                "ByPoint",
                PropertyValue::Map {
                    entries: vec![rivals_uasset::MapEntry {
                        key: my_struct(1, PropertyValue::Int { value: 2 }),
                        value: PropertyValue::Int { value: 7 },
                    }],
                },
                0x40,
            ),
            json!({"kind": "map", "entries": [{
                "key": {"kind": "struct", "name": "MyStruct", "fields": [
                    {"name": "X", "value": {"kind": "int", "value": 1}},
                    {"name": "Y", "value": {"kind": "int", "value": 3}},
                ]},
                "value": {"kind": "int", "value": 7},
            }]}),
        );
        let edit = value(&out);
        assert_eq!((edit.offset, edit.expect_name.as_str()), (0x54, "Y"));
        assert!(out.notes.is_empty(), "{:?}", out.notes);
    }

    /// A key that changed is set through its pair's index.
    #[test]
    fn a_changed_map_key_is_set_by_index() {
        let out = one(
            entry(
                "Scores",
                PropertyValue::Map {
                    entries: vec![rivals_uasset::MapEntry {
                        key: PropertyValue::Name { value: "A".into() },
                        value: PropertyValue::Int { value: 7 },
                    }],
                },
                0x40,
            ),
            json!({"kind": "map", "entries": [{
                "key": {"kind": "name", "value": "B"},
                "value": {"kind": "int", "value": 7},
            }]}),
        );
        assert!(
            matches!(&value(&out).op, EditOp::SetKey { index: 0, text } if text == "B"),
            "{:?}",
            out.edits
        );
        assert!(out.notes.is_empty(), "{:?}", out.notes);
    }

    fn scores(pairs: &[(&str, i64)]) -> PropertyEntry {
        entry(
            "Scores",
            PropertyValue::Map {
                entries: pairs
                    .iter()
                    .map(|(key, value)| rivals_uasset::MapEntry {
                        key: PropertyValue::Name {
                            value: (*key).into(),
                        },
                        value: PropertyValue::Int { value: *value },
                    })
                    .collect(),
            },
            0x40,
        )
    }

    fn scores_json(pairs: &[(&str, i64)]) -> Json {
        json!({"kind": "map", "entries": pairs
            .iter()
            .map(|(key, value)| json!({
                "key": {"kind": "name", "value": key},
                "value": {"kind": "int", "value": value},
            }))
            .collect::<Vec<_>>()})
    }

    /// Pairs that only moved are one reorder, never values set by position, which would land on
    /// the wrong keys. Pairs that moved and changed move, then their values are set by key.
    #[test]
    fn a_reordered_map_is_one_reorder_not_swapped_values() {
        let out = one(
            scores(&[("A", 1), ("B", 2)]),
            scores_json(&[("B", 2), ("A", 1)]),
        );
        assert!(matches!(&value(&out).op, EditOp::Reorder { order } if *order == [1, 0]));
        assert!(out.notes.is_empty(), "{:?}", out.notes);

        let out = one(
            scores(&[("A", 1), ("B", 2)]),
            scores_json(&[("B", 5), ("A", 1)]),
        );
        assert!(out.edits.is_empty(), "{:?}", out.edits);
        assert!(out.notes.is_empty(), "{:?}", out.notes);
        assert!(
            matches!(
                keyed(&out).as_slice(),
                [
                    EditOp::Reorder { order },
                    EditOp::SetElement { index: 1, text },
                ] if *order == [1, 0] && text == "5"
            ),
            "{:?}",
            keyed(&out)
        );

        let out = one(
            scores(&[("A", 1), ("B", 2)]),
            scores_json(&[("B", 1), ("B", 2)]),
        );
        assert!(out.edits.is_empty(), "{:?}", out.edits);
        assert!(out.notes[0].contains("same key"), "{:?}", out.notes);
    }

    /// Repeated elements keep their order among themselves, so a reorder moves only what moved.
    #[test]
    fn repeated_elements_keep_their_order_in_a_reorder() {
        let names = |values: &[&str]| -> Vec<PropertyValue> {
            values
                .iter()
                .map(|value| PropertyValue::Str {
                    value: (*value).into(),
                })
                .collect()
        };
        let out = one(
            array(names(&["a", "b", "a"])),
            json!({"kind": "array", "items": [
                {"kind": "str", "value": "b"},
                {"kind": "str", "value": "a"},
                {"kind": "str", "value": "a"},
            ]}),
        );
        assert!(
            matches!(&value(&out).op, EditOp::Reorder { order } if *order == [1, 0, 2]),
            "{:?}",
            out.edits
        );
    }

    /// An instanced struct given another type is one object set on its type field; the fields
    /// the dump gives the new type are filled in once it has landed.
    #[test]
    fn an_instanced_struct_s_type_is_set_and_its_fields_follow() {
        let out = one(
            entry(
                "Payload",
                PropertyValue::Struct {
                    name: "Point".into(),
                    fields: vec![
                        entry(
                            TYPE_FIELD,
                            PropertyValue::Object {
                                index: -5,
                                path: Some("/Script/Test.Point".into()),
                            },
                            0x60,
                        ),
                        entry("X", PropertyValue::Int { value: 7 }, 0x6A),
                    ],
                },
                0x60,
            ),
            json!({"kind": "struct", "name": "Vector", "fields": [
                {"name": TYPE_FIELD, "value": {"kind": "object", "index": -7, "path": "/Script/CoreUObject.Vector"}},
                {"name": "X", "value": {"kind": "float", "value": 1.0}},
            ]}),
        );
        assert!(out.edits.is_empty(), "{:?}", out.edits);
        assert!(out.notes.is_empty(), "{:?}", out.notes);
        assert!(
            matches!(keyed(&out).as_slice(), [EditOp::Set { text }] if text == "/Script/CoreUObject.Vector"),
            "{:?}",
            keyed(&out)
        );
        assert_eq!(waiting(&out), [("X".to_string(), "fill float".to_string())]);
    }

    /// A format text changes through its parts, each at its own offset; what it shows changes
    /// with them, and typed on its own is a note.
    #[test]
    fn a_format_text_s_part_is_set_at_its_own_offset() {
        let part = |name: &str, value: PropertyValue, at: u64| PropertyEntry {
            name: name.into(),
            element: None,
            span: Some((at, at + 8)),
            value,
            slot: None,
        };
        let pattern = |text: &str| PropertyValue::Text {
            value: Some(text.into()),
            parts: Vec::new(),
            namespace: None,
            key: None,
            display: None,
        };
        let named = entry(
            "Title",
            PropertyValue::Text {
                value: Some("Hulk wins".into()),
                parts: vec![part("SourceFmt", pattern("{Who} wins"), 0x45)],
                namespace: None,
                key: None,
                display: None,
            },
            0x40,
        );
        let out = one(
            named.clone(),
            json!({"kind": "text", "value": "Hulk loses", "parts": [
                {"name": "SourceFmt", "value": {"kind": "text", "value": "{Who} loses"}},
            ]}),
        );
        let edit = value(&out);
        assert_eq!(
            (edit.offset, edit.expect_name.as_str()),
            (0x45, "SourceFmt")
        );
        assert!(out.notes.is_empty(), "{:?}", out.notes);

        let out = one(
            named,
            json!({"kind": "text", "value": "Hulk loses", "parts": [
                {"name": "SourceFmt", "value": {"kind": "text", "value": "{Who} wins"}},
            ]}),
        );
        assert!(out.edits.is_empty(), "{:?}", out.edits);
        assert!(out.notes[0].contains("built from parts"), "{:?}", out.notes);
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
