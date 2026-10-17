//! What two copies of a package hold differently, value by value: each field, row cell or element
//! one stores otherwise than the other, by the export, the row and the path inside it.
//!
//! Objects compare by path rather than by index, since two packages number their imports and
//! exports each in their own way. A field one copy leaves at its default and the other stores, even
//! as zero, counts as a change, since that is a difference in what the package holds.

use std::collections::BTreeMap;

use rivals_uasset::{
    DataTableRow, ExportStatus, MapEntry, ParsedExport, ParsedPackage, PropertyEntry, PropertyValue,
};
use serde::Serialize;

/// One value the two copies hold differently.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ValueChange {
    /// The export's path.
    pub export: String,
    /// The table row, for a value in one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row: Option<String>,
    /// Where in the export or row, as `A.B`, `A[2]` or `A[Key]`; empty for a whole row.
    pub path: String,
    pub game: String,
    pub ours: String,
}

/// Every value `ours` holds differently from `game`, in the exports both have at the same path.
/// Rows only `ours` has are listed as added; what only `game` has is the override check's to say.
pub fn value_changes(game: &ParsedPackage, ours: &ParsedPackage) -> Vec<ValueChange> {
    let key = |path: &str| path.to_ascii_lowercase();
    let our_exports: BTreeMap<String, usize> = ours
        .exports
        .iter()
        .enumerate()
        .map(|(at, export)| (key(&export.path), at))
        .collect();
    let mut out = Vec::new();
    for export in &game.exports {
        let Some(&at) = our_exports.get(&key(&export.path)) else {
            continue;
        };
        let theirs = &ours.exports[at];
        let mut place = Place {
            export: &export.path,
            row: None,
            out: &mut out,
        };
        place.entries("", &export.properties, &theirs.properties);
        // A Blueprint struct's default values, as `defaults.Field`, unless one copy's did not read.
        let read = |export: &ParsedExport| {
            !export.defaults.is_empty() || matches!(export.status, ExportStatus::Complete)
        };
        if !(export.defaults.is_empty() && theirs.defaults.is_empty())
            && read(export)
            && read(theirs)
        {
            place.entries("defaults", &export.defaults, &theirs.defaults);
        }
        if let (Some(table), Some(our_table)) = (&export.data_table, &theirs.data_table) {
            out.extend(row_changes(&export.path, &table.rows, &our_table.rows));
        }
    }
    out
}

/// Every cell `ours` holds differently from `base`, rows paired by name without case, and each
/// row only `ours` has as added.
pub fn row_changes(export: &str, base: &[DataTableRow], ours: &[DataTableRow]) -> Vec<ValueChange> {
    let by_name: BTreeMap<String, &DataTableRow> = base
        .iter()
        .map(|row| (row.name.to_ascii_lowercase(), row))
        .collect();
    let mut out = Vec::new();
    for row in ours {
        match by_name.get(&row.name.to_ascii_lowercase()) {
            Some(was) => Place {
                export,
                row: Some(&row.name),
                out: &mut out,
            }
            .entries("", &was.fields, &row.fields),
            None => out.push(ValueChange {
                export: export.to_string(),
                row: Some(row.name.clone()),
                path: String::new(),
                game: "(no row)".into(),
                ours: "added".into(),
            }),
        }
    }
    out
}

/// Where the changes being found sit, and where they go.
struct Place<'a> {
    export: &'a str,
    row: Option<&'a str>,
    out: &'a mut Vec<ValueChange>,
}

impl Place<'_> {
    fn push(&mut self, path: &str, game: String, ours: String) {
        self.out.push(ValueChange {
            export: self.export.to_string(),
            row: self.row.map(str::to_string),
            path: path.to_string(),
            game,
            ours,
        });
    }

    /// Entries pair by name without case and by element, so a static array's slots pair one by one.
    fn entries(&mut self, prefix: &str, game: &[PropertyEntry], ours: &[PropertyEntry]) {
        let key = |entry: &PropertyEntry| (entry.name.to_ascii_lowercase(), entry.element);
        let theirs: BTreeMap<_, &PropertyEntry> = ours.iter().map(|e| (key(e), e)).collect();
        let mine: BTreeMap<_, &PropertyEntry> = game.iter().map(|e| (key(e), e)).collect();
        let field = |entry: &PropertyEntry| {
            if prefix.is_empty() {
                entry.label()
            } else {
                format!("{prefix}.{}", entry.label())
            }
        };
        for entry in game {
            let path = field(entry);
            match theirs.get(&key(entry)) {
                Some(other) => self.value(&path, &entry.value, &other.value),
                None => self.push(&path, brief(&entry.value), "(not stored)".into()),
            }
        }
        for entry in ours.iter().filter(|entry| !mine.contains_key(&key(entry))) {
            self.push(&field(entry), "(not stored)".into(), brief(&entry.value));
        }
    }

    fn value(&mut self, path: &str, game: &PropertyValue, ours: &PropertyValue) {
        use PropertyValue as V;
        match (game, ours) {
            (
                V::Struct { name, fields },
                V::Struct {
                    name: other,
                    fields: theirs,
                },
            ) if name.eq_ignore_ascii_case(other) => {
                self.entries(path, fields, theirs);
            }
            (V::Array { items }, V::Array { items: theirs })
            | (V::Set { items }, V::Set { items: theirs }) => {
                if items.len() != theirs.len() {
                    self.push(
                        path,
                        format!("[{} items]", items.len()),
                        format!("[{} items]", theirs.len()),
                    );
                }
                for (at, (item, other)) in items.iter().zip(theirs).enumerate() {
                    self.value(&format!("{path}[{at}]"), item, other);
                }
            }
            (V::Map { entries }, V::Map { entries: theirs }) => self.map(path, entries, theirs),
            _ if same(game, ours) => {}
            _ => self.push(path, brief(game), brief(ours)),
        }
    }

    fn map(&mut self, path: &str, game: &[MapEntry], ours: &[MapEntry]) {
        let theirs: BTreeMap<String, &MapEntry> =
            ours.iter().map(|e| (e.key.summary(), e)).collect();
        let mine: BTreeMap<String, &MapEntry> = game.iter().map(|e| (e.key.summary(), e)).collect();
        for entry in game {
            let key = entry.key.summary();
            let at = format!("{path}[{key}]");
            match theirs.get(&key) {
                Some(other) => self.value(&at, &entry.value, &other.value),
                None => self.push(&at, brief(&entry.value), "(no entry)".into()),
            }
        }
        for entry in ours.iter().filter(|e| !mine.contains_key(&e.key.summary())) {
            let at = format!("{path}[{}]", entry.key.summary());
            self.push(&at, "(no entry)".into(), brief(&entry.value));
        }
    }
}

/// Whether two leaf values are the same: objects by path, a panel's slot whatever number an
/// editor's save gave it, enumerators with or without their enum's name before them, texts by what
/// they say and where their translations are filed, undecoded payloads by their bytes, anything
/// else by how it reads.
fn same(game: &PropertyValue, ours: &PropertyValue) -> bool {
    use PropertyValue as V;
    if let (Some(game), Some(ours)) = (enumerator(game), enumerator(ours)) {
        return game.eq_ignore_ascii_case(ours);
    }
    match (game, ours) {
        (
            V::Object { index, path },
            V::Object {
                index: other,
                path: theirs,
            },
        ) => match (path, theirs) {
            (Some(path), Some(theirs)) => {
                path.eq_ignore_ascii_case(theirs) || same_slot(path, theirs)
            }
            (None, None) => index == other || (*index > 0 && *other > 0),
            _ => false,
        },
        (
            V::Text {
                value,
                namespace,
                key,
                ..
            },
            V::Text {
                value: other,
                namespace: other_namespace,
                key: other_key,
                ..
            },
        ) => value == other && namespace == other_namespace && key == other_key,
        (
            V::Undecoded { bytes, digest, .. },
            V::Undecoded {
                bytes: other,
                digest: theirs,
                ..
            },
        ) => bytes == other && digest == theirs,
        _ => {
            std::mem::discriminant(game) == std::mem::discriminant(ours)
                && game.summary() == ours.summary()
        }
    }
}

/// An enumerator's name without its enum's, as `UserSpecified` for `EStretch::UserSpecified`: a
/// cooked package and an editor's save spell one each way.
fn enumerator(value: &PropertyValue) -> Option<&str> {
    let name = match value {
        PropertyValue::Enum {
            name: Some(name), ..
        } => name,
        PropertyValue::Name { value } => value,
        _ => return None,
    };
    Some(name.rsplit("::").next().unwrap_or(name))
}

/// Two paths to a panel's slot that differ only in the number an editor's save gave it, which
/// nothing reaches the slot by.
fn same_slot(game: &str, ours: &str) -> bool {
    fn split(path: &str) -> Option<(String, &str)> {
        path.rsplit_once([':', '.'])
            .map(|(outer, last)| (outer.to_ascii_lowercase(), last))
    }
    let unnumbered = |name: &str| {
        name.trim_end_matches(|c: char| c.is_ascii_digit())
            .trim_end_matches('_')
            .to_string()
    };
    match (split(game), split(ours)) {
        (Some((outer, last)), Some((other, theirs))) => {
            outer == other
                && unnumbered(last).ends_with("Slot")
                && unnumbered(last) == unnumbered(theirs)
        }
        _ => false,
    }
}

fn brief(value: &PropertyValue) -> String {
    crate::mod_search::brief(value)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn entry(name: &str, value: PropertyValue) -> PropertyEntry {
        PropertyEntry {
            name: name.into(),
            element: None,
            value,
            span: None,
            slot: None,
        }
    }

    fn int(value: i64) -> PropertyValue {
        PropertyValue::Int { value }
    }

    fn row(name: &str, fields: Vec<PropertyEntry>) -> DataTableRow {
        DataTableRow {
            name: name.into(),
            fields,
        }
    }

    /// A struct's changed leaf is named by its path, an array that grew says so as well as which
    /// element changed, the same object at another index is no change, and a default against a
    /// stored zero is one.
    #[test]
    fn values_compare_leaf_by_leaf() {
        let object = |index, path: &str| PropertyValue::Object {
            index,
            path: Some(path.into()),
        };
        let game = vec![row(
            "Hulk",
            vec![
                entry(
                    "Stats",
                    PropertyValue::Struct {
                        name: "Stats".into(),
                        fields: vec![entry("Health", int(100)), entry("Speed", int(5))],
                    },
                ),
                entry(
                    "Tags",
                    PropertyValue::Array {
                        items: vec![int(1), int(2)],
                    },
                ),
                entry("Icon", object(-3, "/Game/UI/Hulk.Hulk")),
                entry(
                    "Armor",
                    PropertyValue::Default {
                        declared: None,
                        fields: Vec::new(),
                    },
                ),
            ],
        )];
        let ours = vec![
            row(
                "hulk",
                vec![
                    entry(
                        "Stats",
                        PropertyValue::Struct {
                            name: "Stats".into(),
                            fields: vec![entry("Health", int(250)), entry("Speed", int(5))],
                        },
                    ),
                    entry(
                        "Tags",
                        PropertyValue::Array {
                            items: vec![int(1), int(7), int(3)],
                        },
                    ),
                    entry("Icon", object(-9, "/Game/UI/Hulk.Hulk")),
                    entry("Armor", int(0)),
                ],
            ),
            row("Thor", Vec::new()),
        ];
        let changes = row_changes("/Game/DT.DT", &game, &ours);
        let said: Vec<(String, String, String)> = changes
            .iter()
            .map(|c| (c.path.clone(), c.game.clone(), c.ours.clone()))
            .collect();
        assert_eq!(
            said,
            vec![
                ("Stats.Health".into(), "100".into(), "250".into()),
                ("Tags".into(), "[2 items]".into(), "[3 items]".into()),
                ("Tags[1]".into(), "2".into(), "7".into()),
                ("Armor".into(), "(default)".into(), "0".into()),
                (String::new(), "(no row)".into(), "added".into()),
            ]
        );
        assert_eq!(changes[0].row.as_deref(), Some("hulk"));
        assert_eq!(changes[4].row.as_deref(), Some("Thor"));
    }

    /// An enumerator spelled with its enum's name or without, and a panel's slot an editor's save
    /// numbered afresh, are the same value.
    #[test]
    fn spellings_of_one_value_are_no_change() {
        let enumerator = |name: &str| PropertyValue::Enum {
            value: 0,
            name: Some(name.into()),
            enum_type: None,
        };
        let slot = |name: &str| PropertyValue::Object {
            index: 4,
            path: Some(format!("/Game/W.W_C:WidgetTree:Canvas:{name}")),
        };
        let game = vec![row(
            "A",
            vec![
                entry("Stretch", enumerator("UserSpecified")),
                entry("Slot", slot("CanvasPanelSlot_1")),
                entry("Other", slot("Button_1")),
            ],
        )];
        let ours = vec![row(
            "A",
            vec![
                entry("Stretch", enumerator("EStretch::UserSpecified")),
                entry("Slot", slot("CanvasPanelSlot_4")),
                entry("Other", slot("Button_2")),
            ],
        )];
        let changes = row_changes("/Game/DT.DT", &game, &ours);
        assert_eq!(changes.len(), 1, "{changes:?}");
        assert_eq!(changes[0].path, "Other", "only a slot goes by any number");
    }

    /// A map pairs its entries by key, so one entry's value change is that entry alone.
    #[test]
    fn a_map_pairs_its_entries_by_key() {
        let map = |pairs: &[(&str, i64)]| PropertyValue::Map {
            entries: pairs
                .iter()
                .map(|(key, value)| MapEntry {
                    key: PropertyValue::Name {
                        value: key.to_string(),
                    },
                    value: int(*value),
                })
                .collect(),
        };
        let game = vec![row(
            "A",
            vec![entry("Scores", map(&[("Ranked", 1), ("Quick", 2)]))],
        )];
        let ours = vec![row(
            "A",
            vec![entry("Scores", map(&[("Quick", 2), ("Ranked", 5)]))],
        )];
        let changes = row_changes("/Game/DT.DT", &game, &ours);
        assert_eq!(changes.len(), 1, "{changes:?}");
        assert_eq!(changes[0].path, "Scores[Ranked]");
        assert_eq!(
            (changes[0].game.as_str(), changes[0].ours.as_str()),
            ("1", "5")
        );
    }
}
