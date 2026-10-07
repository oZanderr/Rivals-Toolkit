//! A new field at the end of a Blueprint struct.
//!
//! An unversioned value of a struct starts with a header saying which of the struct's fields it
//! stores, numbered as the struct declares them, so a field declared after the others leaves every
//! value stored before reading as it did, the new field unset: a table's rows, a property's value
//! and the struct's own defaults alike. What lays the struct out by position instead, a script
//! constant listing every field or a value sent over the network, is the caller's to check: see
//! `rivals_core::asset_edit`.

use retoc::legacy_asset::FLegacyPackageHeader;
use serde::{Deserialize, Serialize};

use crate::class_variable::is_ident;
use crate::edit::AppliedEdit;
use crate::field_record::{FieldType, NewField, encode_field_record, new_record, parse_field_type};
use crate::header_edit::Tables;
use crate::package::{ParsedExport, ParsedPackage};
use crate::write::Splice;

/// A field to add to a Blueprint struct.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AddField {
    /// The struct's export; the package's only Blueprint struct when absent.
    #[serde(rename = "struct", default, skip_serializing_if = "Option::is_none")]
    pub strukt: Option<u32>,
    /// The name the editor shows, which the field's own name starts with.
    pub name: String,
    /// As a `local` line takes it: `Int`, `Name`, `Struct</Script/CoreUObject.Vector>`.
    #[serde(rename = "type")]
    pub ty: String,
}

/// What adding fields writes.
pub(crate) struct FieldAddition {
    pub tables: Tables,
    pub splices: Vec<Splice>,
    pub applied: Vec<AppliedEdit>,
}

/// The struct flags the compiler sets from what the members are, which a member of another kind
/// makes untrue: `STRUCT_ZeroConstructor`, `STRUCT_IsPlainOldData` and `STRUCT_NoDestructor`.
/// Without them the struct is built, copied and destroyed member by member, which suits any member.
const DERIVED_FLAGS: u32 = 0x8000 | 0x2000 | 0x4000;

/// The Blueprint struct a save adds fields to: the export named, or the package's only one.
pub fn struct_of(parsed: &ParsedPackage, export: Option<u32>) -> Result<&ParsedExport, String> {
    const CLASS: &str = "UserDefinedStruct";
    let found = match export {
        Some(index) => parsed
            .exports
            .get(index as usize)
            .ok_or_else(|| format!("this package has no export {index}"))?,
        None => {
            let held: Vec<&ParsedExport> = parsed
                .exports
                .iter()
                .filter(|export| export.class_name == CLASS)
                .collect();
            match held.as_slice() {
                [one] => *one,
                [] => return Err("this package holds no Blueprint struct".into()),
                _ => return Err("this package holds several Blueprint structs: name one".into()),
            }
        }
    };
    if found.class_name != CLASS {
        return Err(format!(
            "{} is a {}, not a Blueprint struct",
            found.object_name, found.class_name
        ));
    }
    Ok(found)
}

fn members(export: &ParsedExport) -> Vec<&str> {
    export
        .struct_definition
        .as_ref()
        .map(|definition| {
            definition
                .properties
                .iter()
                .map(|property| property.name.as_str())
                .collect()
        })
        .unwrap_or_default()
}

/// A member's name as the Blueprint editor makes it, `<name>_<n>_<GUID>`, as the name it shows
/// and the number.
fn member_parts(name: &str) -> Option<(&str, u32)> {
    let (rest, guid) = name.rsplit_once('_')?;
    if guid.len() != 32 || !guid.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let (shown, number) = rest.rsplit_once('_')?;
    Some((shown, number.parse().ok()?))
}

/// The member of `export` shown as `name`, by its own name.
pub fn member_shown_as<'a>(export: &'a ParsedExport, name: &str) -> Option<&'a str> {
    members(export).into_iter().find(|member| {
        member_parts(member)
            .map_or(*member, |(shown, _)| shown)
            .eq_ignore_ascii_case(name.trim())
    })
}

/// The names the fields `adds` asks for take, in order, as the editor names a member: the name
/// shown, a number past the highest the members carry, and a guid made from the struct and the
/// name, so the same field added again is named the same.
pub fn new_field_names(export: &ParsedExport, adds: &[AddField]) -> Vec<String> {
    let next = members(export)
        .into_iter()
        .filter_map(member_parts)
        .map(|(_, number)| number + 1)
        .max()
        .unwrap_or(0);
    adds.iter()
        .enumerate()
        .map(|(at, add)| {
            let shown = add.name.trim();
            format!(
                "{shown}_{}_{}",
                next + at as u32,
                crate::component::derived_guid(&[&export.path, shown])
            )
        })
        .collect()
}

/// Holds a member's type to what needs nothing loaded before the struct: one value of a type
/// `/Script` defines, or of a plain kind. A Blueprint type would have to load first, which takes
/// a preload dependency a save does not add yet.
fn member_type(ty: &FieldType) -> Result<(), String> {
    match ty {
        FieldType::Array(_) | FieldType::Set(_) | FieldType::Map(..) => {
            Err("a container field is not added to a Blueprint struct yet".into())
        }
        FieldType::Object(path)
        | FieldType::WeakObject(path)
        | FieldType::SoftObject(path)
        | FieldType::Interface(path)
        | FieldType::Class(path)
        | FieldType::SoftClass(path)
        | FieldType::Struct(path)
        | FieldType::Enum(path)
            if !path.starts_with("/Script/") =>
        {
            Err(format!(
                "{path} is a Blueprint type, which would have to load before the struct, and a \
                 save does not arrange that yet"
            ))
        }
        _ => Ok(()),
    }
}

/// Which of `adds` the struct has already: a member shown as its name, of its type. One shown as
/// its name with another type is refused, as adding it cannot be what was meant.
pub fn fields_held(
    parsed: &ParsedPackage,
    header: &FLegacyPackageHeader,
    adds: &[AddField],
) -> Result<Vec<bool>, String> {
    let mut tables = Tables {
        names: header.name_map.clone(),
        imports: header.imports.clone(),
    };
    adds.iter()
        .map(|add| {
            let export = struct_of(parsed, add.strukt)?;
            let Some(member) = member_shown_as(export, &add.name) else {
                return Ok(false);
            };
            let held = export
                .layout
                .iter()
                .flat_map(|layout| &layout.records)
                .find(|(record, _)| record.name == member)
                .ok_or_else(|| format!("{member}'s record did not read"))?;
            let ty = parse_field_type(&add.ty).map_err(|reason| format!("{member}: {reason}"))?;
            let wanted = new_record(member, &ty, NewField::Member, &mut tables)?;
            if (&wanted.kind, &wanted.tail) == (&held.0.kind, &held.0.tail) {
                Ok(true)
            } else {
                Err(format!(
                    "{} has a field {} of another type than {} already",
                    export.object_name,
                    add.name.trim(),
                    ty.printed()
                ))
            }
        })
        .collect()
}

/// The fields, each a record after the struct's own, with the flags their kinds can make untrue
/// cleared.
pub(crate) fn add_fields(
    parsed: &ParsedPackage,
    header: &FLegacyPackageHeader,
    adds: &[AddField],
) -> Result<FieldAddition, String> {
    let Some(first) = adds.first() else {
        return Err("no field to add".into());
    };
    let export = struct_of(parsed, first.strukt)?;
    if adds
        .iter()
        .any(|add| struct_of(parsed, add.strukt).map(|other| other.index) != Ok(export.index))
    {
        return Err("fields are added to one struct in a save".into());
    }
    if export.super_index != 0 {
        return Err(format!(
            "{} derives from another struct, whose fields its values store first",
            export.object_name
        ));
    }
    let layout = export
        .layout
        .as_ref()
        .ok_or_else(|| format!("{}'s fields did not read", export.object_name))?;
    let (flags_at, flags) = layout
        .struct_flags
        .ok_or_else(|| format!("{}'s flags did not read", export.object_name))?;
    let mut tables = Tables {
        names: header.name_map.clone(),
        imports: header.imports.clone(),
    };
    let mut bytes = Vec::new();
    let mut applied = Vec::new();
    let mut shown: Vec<&str> = Vec::new();
    for (add, name) in adds.iter().zip(new_field_names(export, adds)) {
        let bare = add.name.trim();
        if !is_ident(bare) {
            return Err(format!("{bare:?} is not a name a field can take"));
        }
        if member_shown_as(export, bare).is_some()
            || shown.iter().any(|other| other.eq_ignore_ascii_case(bare))
        {
            return Err(format!("{} already has a field {bare}", export.object_name));
        }
        let ty = parse_field_type(&add.ty).map_err(|reason| format!("{bare}: {reason}"))?;
        member_type(&ty).map_err(|reason| format!("{bare}: {reason}"))?;
        let record = new_record(&name, &ty, NewField::Member, &mut tables)
            .map_err(|reason| format!("{bare}: {reason}"))?;
        bytes.extend(encode_field_record(&record, &mut tables.names));
        applied.push(AppliedEdit {
            name: format!("{} field {name}", export.object_name),
            offset: layout.records_end,
            offset_after: layout.records_end,
            element: None,
            elements_after: None,
            before: "(none)".into(),
            after: ty.printed(),
        });
        shown.push(bare);
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
    let cleared = flags & !DERIVED_FLAGS;
    if cleared != flags {
        splices.push(Splice {
            start: flags_at,
            end: flags_at + 4,
            bytes: cleared.to_le_bytes().to_vec(),
        });
        applied.push(AppliedEdit {
            name: format!("{} StructFlags", export.object_name),
            offset: flags_at,
            offset_after: flags_at,
            element: None,
            elements_after: None,
            before: format!("{flags:#x}"),
            after: format!("{cleared:#x}"),
        });
    }
    Ok(FieldAddition {
        tables,
        splices,
        applied,
    })
}

/// Holds a save adding fields to what it asked: the struct declares each after its own, none of
/// the flags a member can make untrue is left, the struct reads whole with its defaults as they
/// were, and every other export reads as before.
pub(crate) fn verify(
    before: &ParsedPackage,
    after: &ParsedPackage,
    adds: &[AddField],
) -> Result<(), String> {
    let Some(first) = adds.first() else {
        return Ok(());
    };
    let was = struct_of(before, first.strukt)?;
    let wanted = new_field_names(was, adds);
    let now = after
        .exports
        .get(was.index as usize)
        .ok_or("the struct is gone after saving")?;
    if !matches!(now.status, crate::ExportStatus::Complete) {
        return Err(format!(
            "{} does not read whole after saving: {:?}",
            now.object_name, now.status
        ));
    }
    let mut declared = members(was);
    declared.extend(wanted.iter().map(String::as_str));
    if members(now) != declared {
        return Err(format!(
            "{} does not declare {} after its own fields after saving",
            now.object_name,
            wanted.join(", ")
        ));
    }
    if now
        .layout
        .as_ref()
        .and_then(|layout| layout.struct_flags)
        .is_none_or(|(_, flags)| flags & DERIVED_FLAGS != 0)
    {
        return Err(format!(
            "{} still holds flags a new field makes untrue",
            now.object_name
        ));
    }
    let values = |entries: &[crate::PropertyEntry]| -> Vec<(String, String)> {
        entries
            .iter()
            .filter(|entry| !wanted.contains(&entry.name))
            .map(|entry| (entry.label(), entry.value.summary()))
            .collect()
    };
    if values(&was.defaults) != values(&now.defaults) {
        return Err(format!(
            "{}'s defaults read otherwise after saving",
            now.object_name
        ));
    }
    for (old, new) in before.exports.iter().zip(&after.exports) {
        if values(&old.properties) != values(&new.properties) {
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

    fn card(members: &[&str]) -> ParsedExport {
        let mut export = ParsedExport::blank(0);
        export.object_name = "CardData".into();
        export.class_name = "UserDefinedStruct".into();
        export.path = "/Game/Data/CardData.CardData".into();
        export.struct_definition = Some(usmap::Struct {
            name: "CardData".into(),
            super_struct: None,
            properties: members
                .iter()
                .enumerate()
                .map(|(at, name)| usmap::Property {
                    name: (*name).into(),
                    array_dim: 1,
                    index: at as u16,
                    inner: usmap::PropertyInner::Int,
                })
                .collect(),
        });
        export
    }

    /// A new field is named as the editor names a member: the name shown, a number past the
    /// highest the members carry, and a guid that is the same each time.
    #[test]
    fn a_new_field_is_named_as_the_editor_names_a_member() {
        let export = card(&[
            "CardID_64_CC9CEDDF4E4198FAEAC83788C5B70386",
            "CardName_76_8CB9C6B240C6981FE4BCE298018684C6",
            "Legacy",
        ]);
        let add = |name: &str| AddField {
            name: name.into(),
            ty: "Int".into(),
            ..Default::default()
        };
        let names = new_field_names(&export, &[add("Rarity"), add(" Tag ")]);
        let guid = crate::component::derived_guid(&[&export.path, "Rarity"]);
        assert_eq!(names[0], format!("Rarity_77_{guid}"));
        assert!(names[1].starts_with("Tag_78_"), "{}", names[1]);
        assert_eq!(
            names,
            new_field_names(&export, &[add("Rarity"), add("Tag")])
        );
        assert_eq!(
            member_shown_as(&export, "cardname"),
            Some("CardName_76_8CB9C6B240C6981FE4BCE298018684C6")
        );
        assert_eq!(member_shown_as(&export, "Legacy"), Some("Legacy"));
        assert_eq!(member_shown_as(&export, "Card"), None);
    }

    /// A member is one value of a native type or a plain kind; a container, or a type a package
    /// defines, is refused with why.
    #[test]
    fn a_member_needs_nothing_loaded_first() {
        let held = |text: &str| member_type(&parse_field_type(text).unwrap());
        for fine in [
            "Int",
            "Text",
            "Struct</Script/CoreUObject.Vector>",
            "SoftObject</Script/Engine.Texture2D>",
            "Enum</Script/Marvel.ESkinQuality>",
        ] {
            assert_eq!(held(fine), Ok(()), "{fine}");
        }
        assert!(held("Array<Int>").is_err_and(|why| why.contains("container")));
        assert!(
            held("Struct</Game/Data/Other.Other>").is_err_and(|why| why.contains("Blueprint type"))
        );
    }
}
