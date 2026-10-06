//! A new entry on a Blueprint enum, just before its `_MAX`.
//!
//! Another package stores a value of the enum as its number in a slot of its own and as the
//! entry's name inside a container, so neither reads otherwise once an entry goes in before
//! `_MAX`, which moves up one. What other packages hold to besides, such as how many bits a
//! replicated value takes, is the caller's to check: see `rivals_core::asset_edit`. The entry's
//! display name is a value of the enum's `DisplayNameMap`, which the caller sets in the same save
//! once the entry is there.

use serde::{Deserialize, Serialize};

use crate::edit::{AppliedEdit, encode_name};
use crate::package::{ParsedExport, ParsedPackage};
use crate::tails::{EnumEntry, EnumTail};
use crate::write::Splice;

/// An entry to add to a Blueprint enum, shown as `display`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AddEnumEntry {
    /// The enum's export; the package's only Blueprint enum when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export: Option<u32>,
    pub display: String,
}

/// The largest value a Blueprint enum can hold, as it is stored in a byte.
const LARGEST: i64 = 255;

/// The Blueprint enum a save adds entries to: the export named, or the package's only one.
pub fn enum_of(parsed: &ParsedPackage, export: Option<u32>) -> Result<&ParsedExport, String> {
    crate::struct_field::definition_of(parsed, export, "UserDefinedEnum", "Blueprint enum")
}

/// How a Blueprint enum lays out its entries: values 0 to N-1, then `<P>_MAX` at N.
pub struct EnumLayout<'a> {
    pub tail: &'a EnumTail,
    /// What each entry's name starts with: `<P>::` for a namespaced enum.
    pub prefix: String,
    pub max: &'a EnumEntry,
    /// The number the next `NewEnumerator<n>` takes.
    pub next: u32,
}

/// The layout of an enum's entries, refused unless it is a Blueprint enum's.
pub fn enum_layout(export: &ParsedExport) -> Result<EnumLayout<'_>, String> {
    let tail = export
        .enum_tail
        .as_ref()
        .ok_or_else(|| format!("{}'s entries did not read", export.object_name))?;
    let Some((max, entries)) = tail.entries.split_last() else {
        return Err(format!("{} holds no entries", export.object_name));
    };
    let prefix = match max.name.split_once("::") {
        Some((space, _)) => format!("{space}::"),
        None => String::new(),
    };
    let laid_out = max.name.ends_with("_MAX")
        && max.value == entries.len() as i64
        && entries
            .iter()
            .enumerate()
            .all(|(at, entry)| entry.value == at as i64 && entry.name.starts_with(&prefix));
    if !laid_out {
        return Err(format!(
            "{}'s entries are not a Blueprint enum's, numbered from 0 with _MAX last after them",
            export.object_name
        ));
    }
    let next = entries
        .iter()
        .filter_map(|entry| {
            entry.name[prefix.len()..]
                .strip_prefix("NewEnumerator")?
                .parse::<u32>()
                .ok()
        })
        .max()
        .map_or(entries.len() as u32, |highest| highest + 1);
    Ok(EnumLayout {
        tail,
        prefix,
        max,
        next,
    })
}

/// The bare names the entries `adds` asks for take, in order: `NewEnumerator<n>` on from `next`.
pub fn new_entry_names(layout: &EnumLayout<'_>, count: usize) -> Vec<String> {
    (0..count)
        .map(|at| format!("NewEnumerator{}", layout.next + at as u32))
        .collect()
}

/// What adding enum entries writes.
pub(crate) struct EnumAddition {
    pub names: retoc::legacy_asset::FPackageNameMap,
    pub splices: Vec<Splice>,
    pub applied: Vec<AppliedEdit>,
}

/// The entries, each before `_MAX` with the value `_MAX` had, `_MAX` moved up past them.
pub(crate) fn add_enum_entries(
    parsed: &ParsedPackage,
    header: &retoc::legacy_asset::FLegacyPackageHeader,
    adds: &[AddEnumEntry],
) -> Result<EnumAddition, String> {
    let Some(first) = adds.first() else {
        return Err("no enum entry to add".into());
    };
    let export = enum_of(parsed, first.export)?;
    if adds
        .iter()
        .any(|add| enum_of(parsed, add.export).map(|other| other.index) != Ok(export.index))
    {
        return Err("entries are added to one enum in a save".into());
    }
    let layout = enum_layout(export)?;
    let top = layout.max.value + adds.len() as i64;
    if top > LARGEST {
        return Err(format!(
            "{} would hold values up to {top}, and a Blueprint enum is stored in a byte",
            export.object_name
        ));
    }
    let mut names = header.name_map.clone();
    let mut pairs = Vec::new();
    let mut applied = Vec::new();
    for (at, (add, bare)) in adds
        .iter()
        .zip(new_entry_names(&layout, adds.len()))
        .enumerate()
    {
        let value = layout.max.value + at as i64;
        let full = format!("{}{bare}", layout.prefix);
        pairs.extend(encode_name(&full, &mut names));
        pairs.extend_from_slice(&value.to_le_bytes());
        applied.push(AppliedEdit {
            name: format!("{} entry {full}", export.object_name),
            offset: layout.max.span.0,
            offset_after: layout.max.span.0,
            element: None,
            elements_after: None,
            before: "(none)".into(),
            after: format!("{value}, shown as {}", add.display.trim()),
        });
    }
    let count = layout.tail.entries.len() + adds.len();
    let value_at = layout.max.span.0 + 8;
    let splices = vec![
        Splice {
            start: layout.tail.count_at,
            end: layout.tail.count_at + 4,
            bytes: (count as i32).to_le_bytes().to_vec(),
        },
        Splice {
            start: layout.max.span.0,
            end: layout.max.span.0,
            bytes: pairs,
        },
        Splice {
            start: value_at,
            end: value_at + 8,
            bytes: top.to_le_bytes().to_vec(),
        },
    ];
    applied.push(AppliedEdit {
        name: layout.max.name.clone(),
        offset: value_at,
        offset_after: value_at,
        element: None,
        elements_after: None,
        before: layout.max.value.to_string(),
        after: top.to_string(),
    });
    Ok(EnumAddition {
        names,
        splices,
        applied,
    })
}

/// Holds a save adding enum entries to what it asked: the entries before as they were, the new
/// ones before `_MAX` with the values it had and on, `_MAX` past them, the enum read whole, and
/// every other export reading as before.
pub(crate) fn verify(
    before: &ParsedPackage,
    after: &ParsedPackage,
    adds: &[AddEnumEntry],
) -> Result<(), String> {
    let Some(first) = adds.first() else {
        return Ok(());
    };
    let was = enum_of(before, first.export)?;
    let layout = enum_layout(was)?;
    let now = after
        .exports
        .get(was.index as usize)
        .ok_or("the enum is gone after saving")?;
    if !matches!(now.status, crate::ExportStatus::Complete) {
        return Err(format!(
            "{} does not read whole after saving: {:?}",
            now.object_name, now.status
        ));
    }
    let read = |tail: &EnumTail| -> Vec<(String, i64)> {
        tail.entries
            .iter()
            .map(|entry| (entry.name.clone(), entry.value))
            .collect()
    };
    let mut wanted = read(layout.tail);
    let max = wanted.pop().ok_or("the enum held no entries")?;
    for (at, bare) in new_entry_names(&layout, adds.len()).into_iter().enumerate() {
        wanted.push((format!("{}{bare}", layout.prefix), max.1 + at as i64));
    }
    wanted.push((max.0, max.1 + adds.len() as i64));
    let held = now.enum_tail.as_ref().map(read).unwrap_or_default();
    if held != wanted {
        return Err(format!(
            "{} holds {held:?} after saving, not {wanted:?}",
            now.object_name
        ));
    }
    for (old, new) in before.exports.iter().zip(&after.exports) {
        let values = |export: &ParsedExport| -> Vec<(String, String)> {
            export
                .properties
                .iter()
                .map(|entry| (entry.label(), entry.value.summary()))
                .collect()
        };
        if values(old) != values(new) {
            return Err(format!(
                "{} reads other values after saving than it held",
                new.object_name
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn enum_with(entries: &[(&str, i64)]) -> ParsedExport {
        let mut export = ParsedExport::blank(0);
        export.object_name = "EShape".into();
        export.class_name = "UserDefinedEnum".into();
        export.enum_tail = Some(EnumTail {
            entries: entries
                .iter()
                .enumerate()
                .map(|(at, (name, value))| EnumEntry {
                    name: (*name).into(),
                    value: *value,
                    span: (8 + 16 * at as u64, 24 + 16 * at as u64),
                })
                .collect(),
            count_at: 4,
        });
        export
    }

    /// A Blueprint enum's entries are numbered from 0 with `_MAX` last; the next entry takes the
    /// number after the highest `NewEnumerator` and the prefix the others carry.
    #[test]
    fn a_blueprint_enum_s_layout_is_held_to_its_shape() {
        let export = enum_with(&[
            ("EShape::NewEnumerator0", 0),
            ("EShape::NewEnumerator4", 1),
            ("EShape::EShape_MAX", 2),
        ]);
        let layout = enum_layout(&export).expect("laid out");
        assert_eq!(layout.prefix, "EShape::");
        assert_eq!(layout.next, 5);
        assert_eq!(
            new_entry_names(&layout, 2),
            ["NewEnumerator5", "NewEnumerator6"]
        );

        let bare = enum_with(&[("Box", 0), ("EShape_MAX", 1)]);
        let layout = enum_layout(&bare).expect("laid out");
        assert_eq!((layout.prefix.as_str(), layout.next), ("", 1));

        for wrong in [
            &[("EShape::A", 0), ("EShape::B", 1)][..],
            &[
                ("EShape::A", 0),
                ("EShape::B", 5),
                ("EShape::EShape_MAX", 6),
            ][..],
            &[("EShape::A", 0), ("EShape::EShape_MAX", 7)][..],
            &[][..],
        ] {
            assert!(enum_layout(&enum_with(wrong)).is_err(), "{wrong:?}");
        }
    }
}
