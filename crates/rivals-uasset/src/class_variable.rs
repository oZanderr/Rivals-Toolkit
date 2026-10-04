//! A new variable on a Blueprint class whose every instance lives in its own package.
//!
//! A class's own properties come first in an object's layout, ahead of the ones it inherits, so a
//! new one moves every inherited slot up. Each instance in the package has its header renumbered
//! to match, its values left as they are. Instances in other packages would read wrong, which this
//! cannot see: the caller holds the class to having none.

use serde::{Deserialize, Serialize};

use crate::edit::AppliedEdit;
use crate::field_record::{
    NewField, encode_field_record, new_record, parse_field_type, struct_sizes,
};
use crate::header_edit::Tables;
use crate::new_function::class_of;
use crate::package::{AssetBundle, ParsedExport, ParsedPackage};
use crate::reader::Cursor;
use crate::write::Splice;

/// A variable to add to a Blueprint class.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AddVariable {
    /// The class's export; the package's only Blueprint class when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<u32>,
    pub name: String,
    /// As a `local` line takes it: `Int`, `Object</Script/Engine.Actor>`, `Array<Name>`.
    #[serde(rename = "type")]
    pub ty: String,
}

/// What adding variables writes.
pub(crate) struct Addition {
    pub tables: Tables,
    pub splices: Vec<Splice>,
    pub applied: Vec<AppliedEdit>,
}

/// How many schema slots a class's own properties take: one per element of each.
fn own_slots(class: &ParsedExport) -> u32 {
    class.struct_definition.as_ref().map_or(0, |definition| {
        definition
            .properties
            .iter()
            .map(|property| u32::from(property.array_dim.max(1)))
            .sum()
    })
}

/// Where `class`'s own slots start in the layout of an object of `of`, when `of` is `class` or a
/// class of the package below it.
fn offset_in(parsed: &ParsedPackage, of: i32, class: u32) -> Option<u32> {
    let mut offset = 0;
    let mut at = of;
    for _ in 0..parsed.exports.len() {
        let current = parsed.exports.get(usize::try_from(at - 1).ok()?)?;
        if current.index == class {
            return Some(offset);
        }
        offset += own_slots(current);
        at = current.super_index;
        if at <= 0 {
            return None;
        }
    }
    None
}

/// The variables, each a record after the class's own, and every instance of the class in the
/// package renumbered to the layout they make.
pub(crate) fn add_variables(
    parsed: &ParsedPackage,
    header: &retoc::legacy_asset::FLegacyPackageHeader,
    bundle: &AssetBundle<'_>,
    adds: &[AddVariable],
) -> Result<Addition, String> {
    let mut tables = Tables {
        names: header.name_map.clone(),
        imports: header.imports.clone(),
    };
    let sizes = struct_sizes(parsed);
    let Some(first) = adds.first() else {
        return Err("no variable to add".into());
    };
    let (class, layout) = class_of(parsed, first.class)?;
    if adds
        .iter()
        .any(|add| class_of(parsed, add.class).map(|(other, _)| other.index) != Ok(class.index))
    {
        return Err("variables are added to one class in a save".into());
    }
    let own: Vec<&str> = class
        .struct_definition
        .as_ref()
        .map(|definition| {
            definition
                .properties
                .iter()
                .map(|property| property.name.as_str())
                .collect()
        })
        .unwrap_or_default();
    let mut bytes = Vec::new();
    let mut applied = Vec::new();
    let mut added: Vec<String> = Vec::new();
    for add in adds {
        let name = add.name.trim();
        if !is_ident(name) {
            return Err(format!("{name:?} is not a name a variable can take"));
        }
        if own
            .iter()
            .chain(added.iter().map(String::as_str).collect::<Vec<_>>().iter())
            .any(|existing| existing.eq_ignore_ascii_case(name))
        {
            return Err(format!(
                "{} already has a variable {name}",
                class.object_name
            ));
        }
        let ty = parse_field_type(&add.ty).map_err(|reason| format!("{name}: {reason}"))?;
        let record = new_record(name, &ty, NewField::Variable, &mut tables, &sizes)
            .map_err(|reason| format!("{name}: {reason}"))?;
        bytes.extend(encode_field_record(&record, &mut tables.names));
        applied.push(AppliedEdit {
            name: format!("{} variable {name}", class.object_name),
            offset: layout.records_end,
            offset_after: layout.records_end,
            element: None,
            elements_after: None,
            before: "(none)".into(),
            after: ty.printed(),
        });
        added.push(name.to_string());
    }
    let mut splices = vec![
        Splice {
            start: layout.child_properties_at,
            end: layout.child_properties_at + 4,
            bytes: ((layout.records.len() + adds.len()) as i32)
                .to_le_bytes()
                .to_vec(),
        },
        Splice {
            start: layout.records_end,
            end: layout.records_end,
            bytes,
        },
    ];
    // A tagged package names every value it stores, so nothing in it moves.
    if parsed.info.unversioned_properties {
        let base = crate::package::header_size(bundle)?;
        let first_inherited = own_slots(class);
        for export in &parsed.exports {
            let Some(offset) = offset_in(parsed, export.class_index, class.index) else {
                continue;
            };
            let Some(end) = export.properties_end else {
                return Err(format!(
                    "{} is an instance of {} whose values did not read, so they cannot be renumbered",
                    export.object_name, class.object_name
                ));
            };
            let start = export.serial_offset as u64;
            let held = crate::edit::bytes_at(bundle, base, start, end)?;
            let mut cursor = Cursor::new(held, start);
            let mut read = crate::unversioned::read_header(&mut cursor)?;
            let length = cursor.position() as u64;
            let before = read.write()?;
            read.shift_from(offset + first_inherited, adds.len() as u32);
            let after = read.write()?;
            if after != before {
                splices.push(Splice {
                    start,
                    end: start + length,
                    bytes: after,
                });
                applied.push(AppliedEdit {
                    name: format!("{} slots", export.object_name),
                    offset: start,
                    offset_after: start,
                    element: None,
                    elements_after: None,
                    before: "as the class was".into(),
                    after: format!("with {} more of {}'s own", adds.len(), class.object_name),
                });
            }
        }
    }
    splices.sort_by_key(|splice| (splice.start, splice.end));
    Ok(Addition {
        tables,
        splices,
        applied,
    })
}

fn is_ident(text: &str) -> bool {
    let mut chars = text.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Holds the save to what was asked: the class declares each new variable after its own, and every
/// instance in the package reads back the values it held.
pub(crate) fn verify(
    before: &ParsedPackage,
    after: &ParsedPackage,
    adds: &[AddVariable],
) -> Result<(), String> {
    let Some(first) = adds.first() else {
        return Ok(());
    };
    let (class, _) = class_of(after, first.class)?;
    let declared: Vec<&str> = class
        .struct_definition
        .as_ref()
        .map(|definition| {
            definition
                .properties
                .iter()
                .map(|property| property.name.as_str())
                .collect()
        })
        .unwrap_or_default();
    let wanted: Vec<&str> = adds.iter().map(|add| add.name.trim()).collect();
    if !declared.ends_with(&wanted) {
        return Err(format!(
            "{} does not declare {} after its own variables after saving",
            class.object_name,
            wanted.join(", ")
        ));
    }
    let stored = |export: &ParsedExport| -> Vec<(String, String)> {
        export
            .properties
            .iter()
            .filter(|entry| !wanted.contains(&entry.name.as_str()))
            .map(|entry| (entry.label(), entry.value.summary()))
            .collect()
    };
    for (was, now) in before.exports.iter().zip(&after.exports) {
        if offset_in(before, was.class_index, class.index).is_none() {
            continue;
        }
        if !matches!(now.status, crate::ExportStatus::Complete) {
            return Err(format!(
                "{} does not read whole after saving: {:?}",
                now.object_name, now.status
            ));
        }
        if stored(was) != stored(now) {
            return Err(format!(
                "{} reads other values after saving than it held",
                now.object_name
            ));
        }
    }
    Ok(())
}
