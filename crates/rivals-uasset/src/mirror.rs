//! Values a class keeps twice, once among its properties and once in the data it serializes
//! itself, so an edit to either copy is made to both.
//!
//! The engine reads the native copy after the properties, so a change to the property alone is
//! overwritten on load, and one to the native copy alone leaves the two disagreeing.

use crate::edit::{EditOp, PackageEdits, ValueEdit};
use crate::package::ParsedPackage;
use crate::value::{PropertyEntry, PropertyValue};

/// How the elements of a list kept twice line up: the fields mirrored one for one, and the lists
/// inside each element kept twice in turn. A shape with neither is a single value kept twice.
struct Shape {
    fields: &'static [&'static str],
    lists: &'static [(&'static str, Shape)],
}

const VALUE: Shape = Shape {
    fields: &[],
    lists: &[],
};

const TIMELINE_EVENT: Shape = Shape {
    fields: &[
        "NotifyName",
        "StartTime",
        "TrackIndex",
        "NotifyColor",
        "NotifyObject",
        "NotifyStateClass",
        "Duration",
        "TriggerTimeOffset",
        "EndTriggerTimeOffset",
        "Identity",
    ],
    lists: &[],
};

/// Each value kept twice: the property, the native field holding the same value, and how the two
/// line up.
const MIRRORED: &[(&str, &str, Shape)] = &[
    (
        "StaticMaterials",
        "CookedStaticMaterials",
        Shape {
            fields: &["MaterialInterface", "MaterialSlotName"],
            lists: &[],
        },
    ),
    (
        "AnimNotifyTracks",
        "CookedNotifyTracks",
        Shape {
            fields: &["TrackName", "TrackColor"],
            lists: &[("Notifies", TIMELINE_EVENT)],
        },
    ),
    (
        "AnimSections",
        "CookedSections",
        Shape {
            fields: &["SectionName", "NextSectionName", "Time"],
            lists: &[],
        },
    ),
    ("MontagePath", "CookedMontagePath", VALUE),
];

/// `edits` with a twin for every value set on one copy of a mirrored field, or `None` when none
/// needs one.
pub(crate) fn with_mirrors(parsed: &ParsedPackage, edits: &PackageEdits) -> Option<PackageEdits> {
    let pairs: Vec<(&PropertyEntry, &PropertyEntry)> = parsed
        .exports
        .iter()
        .flat_map(|export| mirrored_fields(&export.properties))
        .collect();
    let twins = twins(&pairs, edits);
    if twins.is_empty() {
        return None;
    }
    let mut out = edits.clone();
    out.values.extend(twins);
    Some(out)
}

/// Refuses an element added to, dropped from or moved in a list kept twice, which would change one
/// copy and leave the other as it was.
pub(crate) fn refuse_one_sided(parsed: &ParsedPackage, edits: &PackageEdits) -> Result<(), String> {
    parsed
        .exports
        .iter()
        .try_for_each(|export| refuse_in(&export.properties, edits))
}

fn refuse_in(properties: &[PropertyEntry], edits: &PackageEdits) -> Result<(), String> {
    let mut lists = Vec::new();
    for (property, native, shape) in MIRRORED {
        for copy in [property, native] {
            if let Some(entry) = properties.iter().find(|e| e.name == *copy) {
                lists_of(entry, shape, &mut lists);
            }
        }
    }
    for edit in &edits.values {
        if !matches!(
            edit.op,
            EditOp::Insert { .. } | EditOp::Remove { .. } | EditOp::Reorder { .. }
        ) {
            continue;
        }
        if let Some(list) = lists.iter().find(|list| addresses(edit, list)) {
            return Err(format!(
                "{} is kept twice, among the properties and in the data its object serializes \
                 itself, and adding, dropping or moving an element would change one copy only",
                list.name
            ));
        }
    }
    Ok(())
}

/// The lists `shape` says `entry` holds, itself first when it is one.
fn lists_of<'a>(entry: &'a PropertyEntry, shape: &Shape, out: &mut Vec<&'a PropertyEntry>) {
    let PropertyValue::Array { items } = &entry.value else {
        return;
    };
    out.push(entry);
    for item in items {
        let PropertyValue::Struct { fields, .. } = item else {
            continue;
        };
        for (name, inner) in shape.lists {
            if let Some(list) = fields.iter().find(|field| field.name == *name) {
                lists_of(list, inner, out);
            }
        }
    }
}

/// An edit setting the other copy of each mirrored field `edits` sets one copy of.
fn twins(pairs: &[(&PropertyEntry, &PropertyEntry)], edits: &PackageEdits) -> Vec<ValueEdit> {
    let mut twins: Vec<ValueEdit> = Vec::new();
    for edit in &edits.values {
        if !matches!(edit.op, EditOp::Set { .. }) {
            continue;
        }
        for (a, b) in pairs {
            for (from, to) in [(a, b), (b, a)] {
                if !addresses(edit, from) {
                    continue;
                }
                let Some((at, _)) = to.span else {
                    continue;
                };
                let covered = edits
                    .values
                    .iter()
                    .chain(&twins)
                    .any(|held| held.offset == at && held.expect_name == to.name);
                if !covered {
                    twins.push(ValueEdit {
                        offset: at,
                        expect_name: to.name.clone(),
                        expect_element: to.element,
                        expect_kind: crate::edit::kind_of(&to.value),
                        op: edit.op.clone(),
                    });
                }
            }
        }
    }
    twins
}

/// A value holding its default occupies no bytes and shares its offset with the one stored next,
/// so the name is what picks it.
fn addresses(edit: &ValueEdit, entry: &PropertyEntry) -> bool {
    entry.span.is_some_and(|(start, _)| start == edit.offset)
        && entry.name == edit.expect_name
        && entry.element == edit.expect_element
}

/// Each pair of values an export's `properties` keep twice. A property copy holding its default
/// pairs too: the native copy writes every value, so a set on it reaches the property copy.
fn mirrored_fields(properties: &[PropertyEntry]) -> Vec<(&PropertyEntry, &PropertyEntry)> {
    let mut out = Vec::new();
    for (property, native, shape) in MIRRORED {
        if let (Some(held), Some(cooked)) = (
            properties.iter().find(|e| e.name == *property),
            properties.iter().find(|e| e.name == *native),
        ) {
            pair_up(held, cooked, shape, &mut out);
        }
    }
    out
}

fn pair_up<'a>(
    a: &'a PropertyEntry,
    b: &'a PropertyEntry,
    shape: &Shape,
    out: &mut Vec<(&'a PropertyEntry, &'a PropertyEntry)>,
) {
    let (PropertyValue::Array { items: left }, PropertyValue::Array { items: right }) =
        (&a.value, &b.value)
    else {
        pair_value(a, b, out);
        return;
    };
    for (l, r) in left.iter().zip(right) {
        let (PropertyValue::Struct { fields: l, .. }, PropertyValue::Struct { fields: r, .. }) =
            (l, r)
        else {
            continue;
        };
        let field = |list: &'a [PropertyEntry], name: &str| list.iter().find(|e| e.name == name);
        for name in shape.fields {
            if let (Some(l), Some(r)) = (field(l, name), field(r, name)) {
                pair_value(l, r, out);
            }
        }
        for (name, inner) in shape.lists {
            if let (Some(l), Some(r)) = (field(l, name), field(r, name)) {
                pair_up(l, r, inner, out);
            }
        }
    }
}

/// Two copies of one value, and for a struct each field both copies hold.
fn pair_value<'a>(
    a: &'a PropertyEntry,
    b: &'a PropertyEntry,
    out: &mut Vec<(&'a PropertyEntry, &'a PropertyEntry)>,
) {
    if a.span.is_none() || b.span.is_none() {
        return;
    }
    out.push((a, b));
    if let (PropertyValue::Struct { fields: l, .. }, PropertyValue::Struct { fields: r, .. }) =
        (&a.value, &b.value)
    {
        for field in l {
            if let Some(twin) = r.iter().find(|other| other.name == field.name) {
                pair_value(field, twin, out);
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn entry(name: &str, value: PropertyValue, span: (u64, u64)) -> PropertyEntry {
        PropertyEntry {
            name: name.into(),
            element: None,
            value,
            span: Some(span),
            slot: None,
        }
    }

    fn slot(material_at: u64) -> PropertyValue {
        PropertyValue::Struct {
            name: "StaticMaterial".into(),
            fields: vec![entry(
                "MaterialInterface",
                PropertyValue::Object {
                    index: -1,
                    path: Some("/Game/M.M".into()),
                },
                (material_at, material_at + 4),
            )],
        }
    }

    fn mesh() -> Vec<PropertyEntry> {
        vec![
            entry(
                "StaticMaterials",
                PropertyValue::Array {
                    items: vec![slot(0x10)],
                },
                (0x0C, 0x30),
            ),
            entry(
                "CookedStaticMaterials",
                PropertyValue::Array {
                    items: vec![slot(0x90)],
                },
                (0x8C, 0xB0),
            ),
        ]
    }

    fn set(offset: u64, text: &str) -> ValueEdit {
        ValueEdit {
            offset,
            expect_name: "MaterialInterface".into(),
            expect_element: None,
            expect_kind: "object".into(),
            op: EditOp::Set { text: text.into() },
        }
    }

    /// A set on either copy is made to the other, and one already made to both is left alone.
    #[test]
    fn a_set_on_either_copy_of_a_slot_is_made_to_the_other() {
        let properties = mesh();
        let pairs = mirrored_fields(&properties);
        assert_eq!(pairs.len(), 1);
        for (asked, twin) in [(0x10, 0x90), (0x90, 0x10)] {
            let edits = PackageEdits {
                values: vec![set(asked, "/Game/N.N")],
                ..Default::default()
            };
            let made = twins(&pairs, &edits);
            assert_eq!(made, [set(twin, "/Game/N.N")]);
        }
        let both = PackageEdits {
            values: vec![set(0x10, "/Game/N.N"), set(0x90, "/Game/N.N")],
            ..Default::default()
        };
        assert!(twins(&pairs, &both).is_empty());
        let cleared = PackageEdits {
            values: vec![ValueEdit {
                op: EditOp::Clear,
                ..set(0x10, "")
            }],
            ..Default::default()
        };
        assert!(twins(&pairs, &cleared).is_empty(), "only a set is mirrored");
    }

    fn float(name: &str, value: f64, span: (u64, u64)) -> PropertyEntry {
        entry(name, PropertyValue::Float { value }, span)
    }

    /// One track holding one notify, at `base`, with its start time stored only when `stored`.
    fn track(name: &str, base: u64, stored: bool) -> PropertyEntry {
        let start = if stored {
            (base + 8, base + 12)
        } else {
            (base + 8, base + 8)
        };
        let notify = PropertyValue::Struct {
            name: "AnimTimelineEvent".into(),
            fields: vec![float("StartTime", 0.0, start)],
        };
        let color = PropertyValue::Struct {
            name: "LinearColor".into(),
            fields: vec![float("R", 0.5, (base + 20, base + 24))],
        };
        let track = PropertyValue::Struct {
            name: "AnimTimelineNotifyTrack".into(),
            fields: vec![
                entry("TrackColor", color, (base + 20, base + 36)),
                entry(
                    "Notifies",
                    PropertyValue::Array {
                        items: vec![notify],
                    },
                    (base + 4, base + 16),
                ),
            ],
        };
        entry(
            name,
            PropertyValue::Array { items: vec![track] },
            (base, base + 40),
        )
    }

    fn set_float(offset: u64, name: &str, text: &str) -> ValueEdit {
        ValueEdit {
            offset,
            expect_name: name.into(),
            expect_element: None,
            expect_kind: "float".into(),
            op: EditOp::Set { text: text.into() },
        }
    }

    /// A notify's start time and a track colour's channel reach the other copy through the lists
    /// holding them, from a property copy holding its default too.
    #[test]
    fn a_timeline_notify_and_track_colour_are_set_on_both_copies() {
        let properties = vec![
            track("AnimNotifyTracks", 0x100, false),
            track("CookedNotifyTracks", 0x400, true),
        ];
        let pairs = mirrored_fields(&properties);
        let edits = PackageEdits {
            values: vec![
                set_float(0x408, "StartTime", "0.25"),
                set_float(0x114, "R", "1"),
            ],
            ..Default::default()
        };
        let made = twins(&pairs, &edits);
        assert!(made.contains(&set_float(0x108, "StartTime", "0.25")));
        assert!(made.contains(&set_float(0x414, "R", "1")));
    }

    #[test]
    fn adding_to_a_list_kept_twice_is_refused() {
        let properties = vec![
            track("AnimNotifyTracks", 0x100, true),
            track("CookedNotifyTracks", 0x400, true),
        ];
        let insert = |offset, name: &str| PackageEdits {
            values: vec![ValueEdit {
                offset,
                expect_name: name.into(),
                expect_element: None,
                expect_kind: "array".into(),
                op: EditOp::Insert {
                    index: 0,
                    key: None,
                },
            }],
            ..Default::default()
        };
        assert!(refuse_in(&properties, &insert(0x104, "Notifies")).is_err());
        assert!(refuse_in(&properties, &insert(0x100, "AnimNotifyTracks")).is_err());
        assert!(refuse_in(&properties, &insert(0x200, "Other")).is_ok());
    }
}
