//! Rewrites a tagged package's properties in the unversioned form, slotted against the game's
//! schema, with every export's native data kept as it was cooked.
//!
//! A value reads the same in both forms except where a tag carried part of it: a bool lives in its
//! tag, an enum field is spelled by name, a reflected struct nests a tagged block, and an array of
//! structs opens with a tag for its elements. Everything else is copied byte for byte. The result
//! is read back against the game's schema and must hold every value the tagged package held.

use retoc::legacy_asset::{EPackageFlags, FPackageNameMap};
use usmap::PropertyInner;

use crate::mappings::{Mappings, Schema, kind_name};
use crate::package::{
    AssetBundle, ExportStatus, ParsedPackage, package_info, parse_package, read_header,
};
use crate::props::DefaultPart;
use crate::reader::Cursor;
use crate::structs::{NativeDefault, native_default};
use crate::tagged::{Tag, read_tag};
use crate::unversioned::{HeaderItem, header_for};
use crate::value::{PropertyEntry, PropertyValue};
use crate::write::{HeaderDraft, Splice, reheader, rewrite};

/// A package rewritten with unversioned properties.
#[derive(Debug)]
pub struct Unversioned {
    pub asset: Vec<u8>,
    pub exports: Vec<u8>,
    /// Properties the game's schema has no slot for, as `Object.Property`, which are left out.
    pub dropped: Vec<String>,
}

/// The package with every export's properties written unversioned against `mappings`, refused
/// when a value has no faithful unversioned form or does not read back the same.
pub fn unversion_package(
    bundle: &AssetBundle<'_>,
    mappings: &Mappings,
) -> Result<Unversioned, String> {
    let header = read_header(bundle)?;
    if package_info(&header).unversioned_properties {
        return Err("the package already stores its properties unversioned".into());
    }
    let total = u64::try_from(header.summary.versioning_info.total_header_size)
        .map_err(|_| "this package reports an implausible header size".to_string())?;
    let before = parse_package(bundle, Some(mappings))?;
    let mut walk = Walk {
        names: &header.name_map,
        mappings,
        dropped: Vec::new(),
    };
    let mut splices = Vec::new();
    for export in &before.exports {
        let object = &export.object_name;
        let end = export
            .properties_end
            .ok_or_else(|| format!("{object} did not read whole, so it cannot be rewritten"))?;
        let schema = mappings
            .class_schema(&export.class_name, None)
            .ok_or_else(|| format!("the game's mappings have no class {}", export.class_name))?;
        let start = u64::try_from(export.serial_offset)
            .map_err(|_| format!("{object} has an implausible offset"))?;
        let bytes = start
            .checked_sub(total)
            .zip(end.checked_sub(total))
            .and_then(|(from, to)| bundle.exports.get(from as usize..to as usize))
            .ok_or_else(|| format!("{object}'s properties lie outside its package"))?;
        let mut cursor = Cursor::new(bytes, start);
        let block = walk.block(&mut cursor, &schema, object)?;
        if cursor.remaining() != 0 {
            return Err(format!(
                "{object}'s properties end before the block it reported"
            ));
        }
        splices.push(Splice {
            start,
            end,
            bytes: block,
        });
    }
    splices.sort_by_key(|splice| splice.start);
    let rewritten = rewrite(bundle, &splices, HeaderDraft::default())?;
    let flagged = reheader(
        &AssetBundle {
            asset: &rewritten.asset,
            exports: &rewritten.exports,
        },
        |header| header.summary.package_flags |= EPackageFlags::UsesUnversionedProperties as u32,
    )?;
    let after = parse_package(
        &AssetBundle {
            asset: &flagged.asset,
            exports: &flagged.exports,
        },
        Some(mappings),
    )?;
    let dropped = walk.dropped;
    verify(&before, &after, &dropped)?;
    Ok(Unversioned {
        asset: flagged.asset,
        exports: flagged.exports,
        dropped,
    })
}

struct Walk<'a> {
    names: &'a FPackageNameMap,
    mappings: &'a Mappings,
    dropped: Vec<String>,
}

impl Walk<'_> {
    /// A tagged block through its `None`, as the unversioned header and values `schema` lays out.
    fn block(
        &mut self,
        cursor: &mut Cursor<'_>,
        schema: &Schema<'_>,
        owner: &str,
    ) -> Result<Vec<u8>, String> {
        let mut values: Vec<(u32, Vec<u8>)> = Vec::new();
        loop {
            let name = cursor.read_name(self.names)?;
            if name == "None" {
                break;
            }
            let tag = read_tag(name, cursor, self.names)?;
            let label = format!("{owner}.{}", tag.name);
            let size =
                usize::try_from(tag.size).map_err(|_| format!("{label} has a negative size"))?;
            let end = cursor.position() + size;
            let slot = (0..schema.len()).find_map(|at| {
                let slot = schema.slot(at)?;
                (slot.property.name.eq_ignore_ascii_case(&tag.name)
                    && i64::from(slot.element) == i64::from(tag.array_index))
                .then_some((at, slot))
            });
            let Some((at, slot)) = slot else {
                self.dropped.push(label);
                cursor.seek_to(end)?;
                continue;
            };
            let bytes = self.value(&tag, &slot.property.inner, cursor, end, &label)?;
            if cursor.position() != end {
                return Err(format!(
                    "{label} came to a different length than its tag's {size} bytes"
                ));
            }
            let at = u32::try_from(at).map_err(|_| format!("{label} sits past the schema"))?;
            values.push((at, bytes));
        }
        values.sort_by_key(|(at, _)| *at);
        if values.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return Err(format!("{owner} stores one property twice"));
        }
        let items: Vec<HeaderItem> = values
            .iter()
            .map(|(at, _)| HeaderItem {
                schema_index: *at,
                is_zero: false,
            })
            .collect();
        let mut out = header_for(&items)?;
        for (_, bytes) in values {
            out.extend_from_slice(&bytes);
        }
        Ok(out)
    }

    fn value(
        &mut self,
        tag: &Tag,
        inner: &PropertyInner,
        cursor: &mut Cursor<'_>,
        end: usize,
        label: &str,
    ) -> Result<Vec<u8>, String> {
        let declared = kind_name(inner);
        let tagged = tag
            .type_name
            .strip_suffix("Property")
            .unwrap_or(&tag.type_name);
        let byte_or_enum = |kind: &str| matches!(kind, "Byte" | "Enum");
        if tagged != declared && !(byte_or_enum(tagged) && byte_or_enum(declared)) {
            return Err(format!(
                "{label} is a {tagged} in the package and a {declared} in the game's schema"
            ));
        }
        match inner {
            PropertyInner::Bool => Ok(vec![u8::from(tag.bool_value)]),
            // Spelled by name in a tag, while an unversioned field holds the number behind it.
            PropertyInner::Enum { inner, name } if tag.size == 8 => {
                let text = cursor.read_name(self.names)?;
                let value = self.mappings.enum_value(name, &text).ok_or_else(|| {
                    format!("{label} is {text}, which the game's {name} does not have")
                })?;
                integer(inner, value, label)
            }
            PropertyInner::Byte if tag.size == 8 => Err(format!(
                "{label} is spelled as an enumerator where the game stores a plain byte"
            )),
            PropertyInner::Struct { name } => match native(name, label)? {
                true => copy(cursor, end),
                false => self.structure(name, cursor, label),
            },
            PropertyInner::Array { inner } => match inner.as_ref() {
                // One tag for the elements follows the count, which the unversioned form drops.
                PropertyInner::Struct { name } => {
                    let count = cursor.read_i32()?;
                    let element = cursor.read_name(self.names)?;
                    read_tag(element, cursor, self.names)?;
                    let mut out = count.to_le_bytes().to_vec();
                    if native(name, label)? {
                        out.extend_from_slice(&copy(cursor, end)?);
                        return Ok(out);
                    }
                    for at in 0..count {
                        out.extend(self.structure(name, cursor, &format!("{label}[{at}]"))?);
                    }
                    Ok(out)
                }
                other if holds_struct(other) => Err(nested(label)),
                _ => copy(cursor, end),
            },
            PropertyInner::Set { key } if holds_struct(key) => Err(nested(label)),
            PropertyInner::Map { key, value } if holds_struct(key) || holds_struct(value) => {
                Err(nested(label))
            }
            PropertyInner::Optional { .. } => {
                Err(format!("{label} is an optional, which is not converted"))
            }
            _ => copy(cursor, end),
        }
    }

    /// A reflected struct: a tagged block of its own, laid out by its schema.
    fn structure(
        &mut self,
        name: &str,
        cursor: &mut Cursor<'_>,
        label: &str,
    ) -> Result<Vec<u8>, String> {
        let schema = self
            .mappings
            .schema(name)
            .ok_or_else(|| format!("{label} is a {name}, which the game's mappings lack"))?;
        self.block(cursor, &schema, label)
    }
}

/// Whether a struct is written whole in both forms. One that embeds reflected properties inside
/// its native layout would need those converted too, which nothing here does.
fn native(name: &str, label: &str) -> Result<bool, String> {
    if crate::niagara::reads(name)
        || crate::moviescene::reads(name)
        || matches!(name, "InstancedStruct" | "ConstStruct")
    {
        return Err(format!("{label} is a {name}, which is not converted"));
    }
    match native_default(name) {
        NativeDefault::NotNative => Ok(false),
        NativeDefault::Recipe(parts)
            if parts
                .iter()
                .any(|part| matches!(part, DefaultPart::Struct(_))) =>
        {
            Err(format!(
                "{label} is a {name}, whose reflected part is not converted"
            ))
        }
        _ => Ok(true),
    }
}

fn holds_struct(inner: &PropertyInner) -> bool {
    match inner {
        PropertyInner::Struct { .. } => true,
        PropertyInner::Array { inner } | PropertyInner::Optional { inner } => holds_struct(inner),
        PropertyInner::Set { key } => holds_struct(key),
        PropertyInner::Map { key, value } => holds_struct(key) || holds_struct(value),
        _ => false,
    }
}

fn nested(label: &str) -> String {
    format!("{label} holds structs inside a container that is not converted")
}

fn copy(cursor: &mut Cursor<'_>, end: usize) -> Result<Vec<u8>, String> {
    let count = end
        .checked_sub(cursor.position())
        .ok_or("a value ran past its tag's size")?;
    Ok(cursor.take(count)?.to_vec())
}

/// An enumerator's value at the width of the integer behind the enum.
fn integer(inner: &PropertyInner, value: i64, label: &str) -> Result<Vec<u8>, String> {
    let fits = || format!("{label} is {value}, which its enum's integer cannot hold");
    Ok(match inner {
        PropertyInner::Byte => vec![u8::try_from(value).map_err(|_| fits())?],
        PropertyInner::Int8 => i8::try_from(value)
            .map_err(|_| fits())?
            .to_le_bytes()
            .to_vec(),
        PropertyInner::Int16 => i16::try_from(value)
            .map_err(|_| fits())?
            .to_le_bytes()
            .to_vec(),
        PropertyInner::UInt16 => u16::try_from(value)
            .map_err(|_| fits())?
            .to_le_bytes()
            .to_vec(),
        PropertyInner::Int => i32::try_from(value)
            .map_err(|_| fits())?
            .to_le_bytes()
            .to_vec(),
        PropertyInner::UInt32 => u32::try_from(value)
            .map_err(|_| fits())?
            .to_le_bytes()
            .to_vec(),
        PropertyInner::Int64 => value.to_le_bytes().to_vec(),
        PropertyInner::UInt64 => u64::try_from(value)
            .map_err(|_| fits())?
            .to_le_bytes()
            .to_vec(),
        other => {
            return Err(format!(
                "{label}'s enum stands on a {}, which is not an integer",
                kind_name(other)
            ));
        }
    })
}

/// Every export reads whole after the rewrite and holds what it held, bar the dropped properties.
fn verify(before: &ParsedPackage, after: &ParsedPackage, dropped: &[String]) -> Result<(), String> {
    for (old, new) in before.exports.iter().zip(&after.exports) {
        if let ExportStatus::Failed { .. } | ExportStatus::Partial { .. } = new.status {
            return Err(format!(
                "{} does not read back whole once unversioned",
                new.object_name
            ));
        }
        let kept = |path: &str| {
            !dropped.iter().any(|gone| {
                path == gone
                    || path
                        .strip_prefix(gone.as_str())
                        .is_some_and(|rest| rest.starts_with('.') || rest.starts_with('['))
            })
        };
        let mut was = Vec::new();
        let mut now = Vec::new();
        leaves(&old.object_name, &old.properties, &mut was);
        leaves(&new.object_name, &new.properties, &mut now);
        was.retain(|(path, _)| kept(path));
        // A tagged block lists its values as stored and an unversioned one in schema order.
        let now: std::collections::BTreeMap<&str, &str> =
            now.iter().map(|(p, v)| (p.as_str(), v.as_str())).collect();
        for (path, value) in &was {
            let read = now.get(path.as_str()).copied().unwrap_or("nothing");
            if read != value {
                return Err(format!(
                    "{path} held {value} and reads {read} once unversioned"
                ));
            }
        }
        if now.len() != was.len() {
            return Err(format!(
                "{} reads more values once unversioned than it held",
                new.object_name
            ));
        }
    }
    Ok(())
}

/// Each stored value under `owner` as a path and its summary, skipping slots that store nothing.
fn leaves(owner: &str, entries: &[PropertyEntry], out: &mut Vec<(String, String)>) {
    for entry in entries {
        // The tagged reader numbers a static array's elements from the second, the unversioned
        // one every element.
        let path = match entry.element {
            Some(element) if element > 0 => format!("{owner}.{}[{element}]", entry.name),
            _ => format!("{owner}.{}", entry.name),
        };
        leaf(&path, &entry.value, out);
    }
}

fn leaf(path: &str, value: &PropertyValue, out: &mut Vec<(String, String)>) {
    match value {
        PropertyValue::Unset { .. } | PropertyValue::Default { .. } => {}
        PropertyValue::Struct { fields, .. } => leaves(path, fields, out),
        PropertyValue::Array { items } | PropertyValue::Set { items } => {
            out.push((format!("{path}#"), items.len().to_string()));
            for (at, item) in items.iter().enumerate() {
                leaf(&format!("{path}[{at}]"), item, out);
            }
        }
        PropertyValue::Map { entries } => {
            out.push((format!("{path}#"), entries.len().to_string()));
            for (at, pair) in entries.iter().enumerate() {
                leaf(&format!("{path}[{at}].key"), &pair.key, out);
                leaf(&format!("{path}[{at}].value"), &pair.value, out);
            }
        }
        // A tag spells an enumerator `EType::Name` where the schema has `Name`.
        PropertyValue::Enum { value, name, .. } => out.push((
            path.to_string(),
            name.as_deref()
                .map(|name| name.rsplit("::").next().unwrap_or(name).to_string())
                .unwrap_or_else(|| value.to_string()),
        )),
        other => out.push((path.to_string(), other.summary())),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use retoc::legacy_asset::{
        FLegacyPackageFileSummary, FLegacyPackageHeader, FMinimalName, FObjectExport, FObjectImport,
    };
    use retoc::zen::FPackageIndex;
    use usmap::{Enum, Property, Struct};

    use super::*;
    use crate::package::FALLBACK_ENGINE_VERSION;

    const NAMES: &[&str] = &[
        "None",
        "/Script/CoreUObject",
        "Package",
        "Class",
        "/Script/Test",
        "Thing",
        "TheThing",
        "IntProperty",
        "BoolProperty",
        "FloatProperty",
        "ByteProperty",
        "StructProperty",
        "ArrayProperty",
        "Damage",
        "Enabled",
        "Scale",
        "Mode",
        "EMode",
        "EMode::B",
        "Pos",
        "Point",
        "X",
        "Y",
        "Points",
        "Stale",
        "Spot",
        "Vector",
    ];

    fn index_of(value: &str) -> i32 {
        NAMES.iter().position(|n| *n == value).expect("a test name") as i32
    }

    fn name(out: &mut Vec<u8>, value: &str) {
        out.extend_from_slice(&index_of(value).to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
    }

    fn head(out: &mut Vec<u8>, property: &str, kind: &str, size: usize) {
        name(out, property);
        name(out, kind);
        out.extend_from_slice(&(size as i32).to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
    }

    fn int(out: &mut Vec<u8>, property: &str, value: i32) {
        head(out, property, "IntProperty", 4);
        out.push(0);
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn point(x: i32, y: i32) -> Vec<u8> {
        let mut out = Vec::new();
        int(&mut out, "X", x);
        int(&mut out, "Y", y);
        name(&mut out, "None");
        out
    }

    fn struct_head(out: &mut Vec<u8>, property: &str, kind: &str, size: usize) {
        head(out, property, "StructProperty", size);
        name(out, kind);
        out.extend_from_slice(&[0; 16]);
        out.push(0);
    }

    /// `Thing` with an int, a bool, a float, a named enum, a reflected struct, a native struct, an
    /// array of reflected structs and a property the schema no longer has.
    fn tagged() -> (Vec<u8>, Vec<u8>) {
        let mut e = Vec::new();
        int(&mut e, "Damage", 7);
        head(&mut e, "Enabled", "BoolProperty", 0);
        e.push(1);
        e.push(0);
        head(&mut e, "Scale", "FloatProperty", 4);
        e.push(0);
        e.extend_from_slice(&2.5f32.to_le_bytes());
        head(&mut e, "Mode", "ByteProperty", 8);
        name(&mut e, "EMode");
        e.push(0);
        name(&mut e, "EMode::B");
        let pos = point(1, 2);
        struct_head(&mut e, "Pos", "Point", pos.len());
        e.extend_from_slice(&pos);
        struct_head(&mut e, "Spot", "Vector", 24);
        for value in [1.0f64, 2.0, 3.0] {
            e.extend_from_slice(&value.to_le_bytes());
        }
        let elements = [point(3, 4), point(5, 6)].concat();
        let mut array = 2i32.to_le_bytes().to_vec();
        struct_head(&mut array, "Points", "Point", elements.len());
        array.extend_from_slice(&elements);
        head(&mut e, "Points", "ArrayProperty", array.len());
        name(&mut e, "StructProperty");
        e.push(0);
        e.extend_from_slice(&array);
        int(&mut e, "Stale", 9);
        name(&mut e, "None");
        e.extend_from_slice(&0i32.to_le_bytes());

        let minimal = |value: &str| FMinimalName {
            index: index_of(value),
            number: 0,
        };
        let mut summary = FLegacyPackageFileSummary {
            package_name: "/Game/Things".to_string(),
            ..Default::default()
        };
        summary.versioning_info.package_file_version =
            FALLBACK_ENGINE_VERSION.package_file_version();
        summary.versioning_info.total_header_size = 1024;
        summary.package_flags =
            EPackageFlags::Cooked as u32 | EPackageFlags::FilterEditorOnly as u32;
        let header = FLegacyPackageHeader {
            summary,
            name_map: FPackageNameMap::create_from_names(
                NAMES.iter().map(|n| (*n).to_string()).collect(),
            ),
            imports: vec![
                FObjectImport {
                    class_package: minimal("/Script/CoreUObject"),
                    class_name: minimal("Package"),
                    outer_index: FPackageIndex::create_null(),
                    object_name: minimal("/Script/Test"),
                    is_optional: false,
                },
                FObjectImport {
                    class_package: minimal("/Script/CoreUObject"),
                    class_name: minimal("Class"),
                    outer_index: FPackageIndex::create_import(0),
                    object_name: minimal("Thing"),
                    is_optional: false,
                },
            ],
            exports: vec![FObjectExport {
                object_name: minimal("TheThing"),
                class_index: FPackageIndex::create_import(1),
                serial_offset: 0,
                serial_size: e.len() as i64,
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut asset = std::io::Cursor::new(Vec::new());
        header
            .serialize(&mut asset, Some(1024), &retoc::logging::Log::no_log())
            .expect("serialize the test header");
        (asset.into_inner(), e)
    }

    fn mappings() -> Mappings {
        let property = |name: &str, index: u16, inner: PropertyInner| Property {
            name: name.into(),
            array_dim: 1,
            index,
            inner,
        };
        let point = || PropertyInner::Struct {
            name: "Point".into(),
        };
        Mappings::from_structs_and_enums(
            vec![
                Struct {
                    name: "Object".into(),
                    super_struct: None,
                    properties: Vec::new(),
                },
                Struct {
                    name: "Thing".into(),
                    super_struct: Some("Object".into()),
                    properties: vec![
                        property(
                            "Points",
                            0,
                            PropertyInner::Array {
                                inner: Box::new(point()),
                            },
                        ),
                        property("Damage", 1, PropertyInner::Int),
                        property("Pos", 2, point()),
                        property(
                            "Mode",
                            3,
                            PropertyInner::Enum {
                                inner: Box::new(PropertyInner::Byte),
                                name: "EMode".into(),
                            },
                        ),
                        property("Enabled", 4, PropertyInner::Bool),
                        property(
                            "Spot",
                            5,
                            PropertyInner::Struct {
                                name: "Vector".into(),
                            },
                        ),
                        property("Scale", 6, PropertyInner::Float),
                    ],
                },
                Struct {
                    name: "Point".into(),
                    super_struct: None,
                    properties: vec![
                        property("X", 0, PropertyInner::Int),
                        property("Y", 1, PropertyInner::Int),
                    ],
                },
            ],
            vec![Enum {
                name: "EMode".into(),
                entries: [(0, "A".to_string()), (1, "B".to_string())].into(),
            }],
        )
    }

    #[test]
    fn a_tagged_package_comes_out_unversioned_holding_the_same_values() {
        let (asset, exports) = tagged();
        let bundle = AssetBundle {
            asset: &asset,
            exports: &exports,
        };
        let mappings = mappings();
        let done = unversion_package(&bundle, &mappings).expect("unversioned");
        assert_eq!(done.dropped, ["TheThing.Stale"]);

        let after = AssetBundle {
            asset: &done.asset,
            exports: &done.exports,
        };
        let header = read_header(&after).expect("header");
        assert!(package_info(&header).unversioned_properties);
        let parsed = parse_package(&after, Some(&mappings)).expect("parse");
        let mut read = Vec::new();
        leaves("TheThing", &parsed.exports[0].properties, &mut read);
        let read: Vec<(&str, &str)> = read.iter().map(|(p, v)| (p.as_str(), v.as_str())).collect();
        for expected in [
            ("TheThing.Points#", "2"),
            ("TheThing.Points[1].Y", "6"),
            ("TheThing.Damage", "7"),
            ("TheThing.Pos.X", "1"),
            ("TheThing.Mode", "B"),
            ("TheThing.Enabled", "true"),
            ("TheThing.Scale", "2.5"),
        ] {
            assert!(read.contains(&expected), "{expected:?} not in {read:?}");
        }
        // The values are laid out in schema order, so the array the schema lists first leads.
        assert_eq!(read.first().map(|(p, _)| *p), Some("TheThing.Points#"));
    }

    #[test]
    fn a_value_whose_type_the_game_changed_is_refused() {
        let (asset, exports) = tagged();
        let bundle = AssetBundle {
            asset: &asset,
            exports: &exports,
        };
        let mut structs = Vec::new();
        for name in ["Object", "Thing", "Point"] {
            structs.push(
                mappings()
                    .chain_entries(name)
                    .last()
                    .cloned()
                    .expect("struct"),
            );
        }
        for property in &mut structs[1].properties {
            if property.name == "Damage" {
                property.inner = PropertyInner::Float;
            }
        }
        let changed = Mappings::from_structs_and_enums(
            structs,
            vec![Enum {
                name: "EMode".into(),
                entries: [(0, "A".to_string()), (1, "B".to_string())].into(),
            }],
        );
        let err = unversion_package(&bundle, &changed).expect_err("refused");
        assert!(err.contains("Damage") && err.contains("Float"), "{err}");
    }
}
