//! Reads the tagged property form, where every property carries its own name, type and byte size.
//!
//! The size field is what makes this mode safe. Whatever happens while decoding a value, the next
//! property starts at a position the file itself declared, so a type this crate does not handle
//! costs one undecoded value rather than the rest of the export. Unversioned parsing has no such
//! anchor, which is why it fails hard instead.
//!
//! Because the tags carry their own type information, a tagged package needs no mappings file.

use usmap::PropertyInner;

use crate::props::{Ctx, Diagnostics, InstancedLayout, read_value, record_container};
use crate::reader::Cursor;
use crate::structs;
use crate::value::{PropertyEntry, PropertyValue};

/// Guards against a stream that never produces the terminating `None`.
const MAX_PROPERTIES: usize = 65536;

/// What a tag says about the value that follows.
struct Tag {
    name: String,
    type_name: String,
    size: i32,
    /// Where the size sits, so an edit that changes the value's width can move it.
    size_at: usize,
    array_index: i32,
    /// Set for structs, and for the element type of a container.
    inner_name: Option<String>,
    value_type_name: Option<String>,
    /// A bool stores its value in the tag itself and occupies no value bytes.
    bool_value: bool,
    /// Where that byte sits, which is the only place a bool can be rewritten.
    bool_at: Option<usize>,
}

/// `owner` is the struct or class whose properties the block holds. A tag names a container's
/// struct elements only as structs, so the owner's schema is what says which.
pub(crate) fn read_tagged_block(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    depth: u32,
    entries: &mut Vec<PropertyEntry>,
    owner: Option<&str>,
) -> Result<(), String> {
    // What the block holds, by name and array index, so the owner's other properties can be
    // listed as absent.
    let mut held: Vec<(String, i32)> = Vec::new();
    // This block's own tags among the recorded bounds, which learn where the block ends last.
    let mut bounds: Vec<usize> = Vec::new();
    for _ in 0..MAX_PROPERTIES {
        let name_at = cursor.file_offset();
        let name = cursor.read_name(ctx.names())?;
        if name == "None" {
            for at in bounds {
                diagnostics.tag_bounds[at].none_at = name_at;
            }
            list_absent(ctx, diagnostics, owner, &held, name_at, entries);
            return Ok(());
        }
        let tag = read_tag(name, cursor, ctx)?;
        held.push((tag.name.clone(), tag.array_index));
        let start = cursor.position();
        let end = start
            .checked_add(tag.size.max(0) as usize)
            .ok_or_else(|| cursor.err("tagged property size overflows"))?;
        if end > start + cursor.remaining() {
            return Err(cursor.err(format!(
                "property {} declares {} bytes but only {} remain",
                tag.name,
                tag.size,
                cursor.remaining()
            )));
        }
        // The size is a length prefix like an instanced struct's, so the writer moves it by
        // whatever an edit inside the value adds or removes.
        let base = cursor.file_offset() - cursor.position() as u64;
        diagnostics.instanced.push(InstancedLayout {
            size_at: base + tag.size_at as u64,
            payload_start: base + start as u64,
            payload_end: base + end as u64,
        });

        let value = match read_tagged_value(&tag, cursor, ctx, diagnostics, depth, owner) {
            Ok(value) if cursor.position() == end => value,
            Ok(_) => {
                diagnostics.unsupported.insert(tag.type_name.clone());
                undecoded(&tag, "decoder did not consume the declared size")
            }
            Err(reason) => {
                diagnostics.unsupported.insert(tag.type_name.clone());
                undecoded(&tag, &reason)
            }
        };

        // The tag's own size is the anchor: resynchronise whatever the value reader did.
        cursor.seek_to(end)?;
        bounds.push(diagnostics.tag_bounds.len());
        diagnostics.tag_bounds.push(crate::props::TagBounds {
            key: base + tag.bool_at.unwrap_or(start) as u64,
            tag_at: name_at,
            tag_end: base + end as u64,
            none_at: 0,
        });
        // A tagged property declares its own size, so the span is exact without tracking reads.
        entries.push(PropertyEntry {
            name: tag.name,
            element: (tag.array_index > 0).then_some(tag.array_index as u32),
            value,
            // A bool keeps its value in the tag and stores nothing after it, so its one byte there
            // is where an edit writes.
            span: Some(match tag.bool_at {
                Some(at) => (base + at as u64, base + at as u64 + 1),
                None => (base + start as u64, base + end as u64),
            }),
            slot: None,
        });
    }
    Err(cursor.err("tagged property list has no terminating None"))
}

/// The properties the owner's schema declares that the block does not hold, listed as not stored at
/// its terminating `None`. Only the editor's reading lists them, and only with a schema to ask.
fn list_absent(
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    owner: Option<&str>,
    held: &[(String, i32)],
    none_at: u64,
    entries: &mut Vec<PropertyEntry>,
) {
    if !diagnostics.declared_slots {
        return;
    }
    let Some(schema) = owner.and_then(|owner| ctx.schema(owner)) else {
        return;
    };
    for slot in (0..schema.len()).filter_map(|index| schema.slot(index)) {
        let index = slot.element as i32;
        if held
            .iter()
            .any(|(name, at)| *name == slot.property.name && *at == index)
        {
            continue;
        }
        let inner = &slot.property.inner;
        entries.push(PropertyEntry {
            name: slot.property.name.clone(),
            element: (index > 0).then_some(slot.element),
            value: PropertyValue::Unset {
                declared: crate::props::typed_as(inner),
                enum_type: match inner {
                    PropertyInner::Enum { name, .. } => Some(name.clone()),
                    _ => None,
                },
                fields: crate::props::unset_fields(inner, ctx, 0),
            },
            span: Some((none_at, none_at)),
            slot: None,
        });
        let native_default = match inner {
            PropertyInner::Struct { name } => crate::props::native_parts(name, ctx).flatten(),
            _ => None,
        };
        diagnostics.tagged_absent.push(crate::props::TaggedAbsent {
            none_at,
            name: slot.property.name.clone(),
            array_index: slot.element,
            inner: inner.clone(),
            native_default,
        });
    }
}

fn undecoded(tag: &Tag, reason: &str) -> PropertyValue {
    PropertyValue::Struct {
        name: tag.type_name.clone(),
        fields: vec![PropertyEntry {
            name: "(undecoded)".into(),
            element: None,
            span: None,
            slot: None,
            value: PropertyValue::Str {
                value: reason.to_string(),
            },
        }],
    }
}

fn read_tag(name: String, cursor: &mut Cursor<'_>, ctx: &Ctx<'_>) -> Result<Tag, String> {
    let type_name = cursor.read_name(ctx.names())?;
    let size_at = cursor.position();
    let size = cursor.read_i32()?;
    let array_index = cursor.read_i32()?;

    let mut inner_name = None;
    let mut value_type_name = None;
    let mut bool_value = false;
    let mut bool_at = None;
    match type_name.as_str() {
        "StructProperty" => {
            inner_name = Some(cursor.read_name(ctx.names())?);
            cursor.skip(16)?;
        }
        "BoolProperty" => {
            bool_at = Some(cursor.position());
            bool_value = cursor.read_u8()? != 0;
        }
        "ByteProperty" | "EnumProperty" => inner_name = Some(cursor.read_name(ctx.names())?),
        "ArrayProperty" | "SetProperty" => inner_name = Some(cursor.read_name(ctx.names())?),
        "MapProperty" => {
            inner_name = Some(cursor.read_name(ctx.names())?);
            value_type_name = Some(cursor.read_name(ctx.names())?);
        }
        _ => {}
    }

    if cursor.read_u8()? != 0 {
        cursor.skip(16)?;
    }

    Ok(Tag {
        name,
        type_name,
        size,
        size_at,
        array_index,
        inner_name,
        value_type_name,
        bool_value,
        bool_at,
    })
}

fn read_tagged_value(
    tag: &Tag,
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    depth: u32,
    owner: Option<&str>,
) -> Result<PropertyValue, String> {
    let declared = || declared_inner(ctx, owner, &tag.name);
    match tag.type_name.as_str() {
        "BoolProperty" => Ok(PropertyValue::Bool {
            value: tag.bool_value,
        }),
        "StructProperty" => {
            let name = tag.inner_name.as_deref().unwrap_or_default();
            read_tagged_struct(name, cursor, ctx, diagnostics, depth)
        }
        // The declared size settles what an engine version would otherwise leave ambiguous: a
        // byte-backed enum is one byte, a name-backed one is an eight byte FName.
        "ByteProperty" | "EnumProperty" if tag.size == 8 => {
            let name = cursor.read_name(ctx.names())?;
            Ok(PropertyValue::Enum {
                value: -1,
                name: Some(name),
                enum_type: None,
            })
        }
        "ArrayProperty" => read_tagged_array(tag, cursor, ctx, diagnostics, depth),
        "SetProperty" => {
            let element = Element::of(
                tag.inner_name.as_deref(),
                declared().and_then(|inner| match inner {
                    PropertyInner::Set { key } => struct_name(&key),
                    _ => None,
                }),
                cursor,
            )?;
            let at = cursor.file_offset();
            // The keys removed from the inherited defaults come first, each a full key.
            for _ in 0..read_count(cursor)? {
                element.read(cursor, ctx, diagnostics, depth + 1)?;
            }
            let count_at = cursor.file_offset();
            let count = read_count(cursor)?;
            let mut items = Vec::with_capacity(count);
            let mut spans = Vec::with_capacity(count);
            for _ in 0..count {
                let from = cursor.file_offset();
                items.push(element.read(cursor, ctx, diagnostics, depth + 1)?);
                spans.push((from, cursor.file_offset()));
            }
            record_container(
                diagnostics,
                ctx,
                at,
                count_at,
                spans,
                &element.inner(),
                None,
            );
            element.settle(diagnostics, false);
            Ok(PropertyValue::Set { items })
        }
        "MapProperty" => {
            let (key_struct, value_struct) = match declared() {
                Some(PropertyInner::Map { key, value }) => (struct_name(&key), struct_name(&value)),
                _ => (None, None),
            };
            let key_type = Element::of(tag.inner_name.as_deref(), key_struct, cursor)?;
            let value_type = Element::of(tag.value_type_name.as_deref(), value_struct, cursor)?;
            let at = cursor.file_offset();
            for _ in 0..read_count(cursor)? {
                key_type.read(cursor, ctx, diagnostics, depth + 1)?;
            }
            let count_at = cursor.file_offset();
            let count = read_count(cursor)?;
            let mut pairs = Vec::with_capacity(count);
            let mut spans = Vec::with_capacity(count);
            let mut keys = Vec::with_capacity(count);
            for _ in 0..count {
                let from = cursor.file_offset();
                let key = key_type.read(cursor, ctx, diagnostics, depth + 1)?;
                keys.push((from, cursor.file_offset()));
                pairs.push(crate::value::MapEntry {
                    key,
                    value: value_type.read(cursor, ctx, diagnostics, depth + 1)?,
                });
                spans.push((from, cursor.file_offset()));
            }
            record_container(
                diagnostics,
                ctx,
                at,
                count_at,
                spans,
                &value_type.inner(),
                Some((keys, &key_type.inner())),
            );
            value_type.settle(diagnostics, false);
            key_type.settle(diagnostics, true);
            Ok(PropertyValue::Map { entries: pairs })
        }
        other => {
            let inner = simple_inner(other)
                .ok_or_else(|| cursor.err(format!("{other} is not decoded in tagged mode")))?;
            read_value(&inner, cursor, ctx, diagnostics, depth)
        }
    }
}

/// Native structs keep their binary layout; everything else nests another tagged block, which is
/// why tagged parsing needs no schema.
fn read_tagged_struct(
    name: &str,
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    depth: u32,
) -> Result<PropertyValue, String> {
    if let Some(result) = structs::read_native(name, cursor, ctx, diagnostics, depth) {
        return result;
    }
    let mut fields = Vec::new();
    read_tagged_block(cursor, ctx, diagnostics, depth + 1, &mut fields, Some(name))?;
    Ok(PropertyValue::Struct {
        name: name.to_string(),
        fields,
    })
}

/// An array of structs writes one inner tag naming the element type, then the raw elements.
fn read_tagged_array(
    tag: &Tag,
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    depth: u32,
) -> Result<PropertyValue, String> {
    let at = cursor.file_offset();
    let count = cursor.read_i32()?;
    if count < 0 || count as usize > cursor.remaining() {
        return Err(cursor.err(format!("implausible tagged array count {count}")));
    }
    let inner_type = tag.inner_name.as_deref().unwrap_or_default();

    if inner_type == "StructProperty" {
        let element_name = cursor.read_name(ctx.names())?;
        let element_tag = read_tag(element_name, cursor, ctx)?;
        // The element tag's size covers every element, so it follows edits inside them too.
        let base = cursor.file_offset() - cursor.position() as u64;
        let elements_start = base + cursor.position() as u64;
        diagnostics.instanced.push(InstancedLayout {
            size_at: base + element_tag.size_at as u64,
            payload_start: elements_start,
            payload_end: elements_start + element_tag.size.max(0) as u64,
        });
        let struct_name = element_tag.inner_name.unwrap_or_default();
        let mut items = Vec::with_capacity(count as usize);
        let mut spans = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let from = cursor.file_offset();
            items.push(read_tagged_struct(
                &struct_name,
                cursor,
                ctx,
                diagnostics,
                depth,
            )?);
            spans.push((from, cursor.file_offset()));
        }
        record_container(
            diagnostics,
            ctx,
            at,
            at,
            spans,
            &PropertyInner::Struct { name: struct_name },
            None,
        );
        // A tagged struct with nothing stored is its terminating `None` alone; the schema-built
        // default is an unversioned header, which would not read here.
        if let Some(layout) = diagnostics.containers.last_mut() {
            layout.default_element = None;
            layout.default_recipe = None;
            layout.default_name = Some("None".into());
            layout.elements_at = Some(elements_start);
        }
        return Ok(PropertyValue::Array { items });
    }

    let element = Element::of(Some(inner_type), None, cursor)?;
    let mut items = Vec::with_capacity(count as usize);
    let mut spans = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let from = cursor.file_offset();
        items.push(element.read(cursor, ctx, diagnostics, depth + 1)?);
        spans.push((from, cursor.file_offset()));
    }
    record_container(diagnostics, ctx, at, at, spans, &element.inner(), None);
    element.settle(diagnostics, false);
    Ok(PropertyValue::Array { items })
}

/// A container element as its tag names its type. Inside a container a bool is a byte of its own
/// and an enum its enumerator's name, where a lone property keeps the one in its tag and the other
/// behind its enum's name. A struct element carries no struct name there, so those are left
/// undecoded rather than guessed at.
enum Element {
    Simple(PropertyInner),
    Bool,
    Enum,
    /// A struct element, named by the owner's schema since the tag does not say which.
    Struct(String),
}

/// The type the owner's schema declares for `property`, when there is a schema to ask.
fn declared_inner(ctx: &Ctx<'_>, owner: Option<&str>, property: &str) -> Option<PropertyInner> {
    let schema = ctx.schema(owner?)?;
    (0..schema.len())
        .filter_map(|index| schema.slot(index))
        .find(|slot| slot.property.name == property)
        .map(|slot| slot.property.inner.clone())
}

fn struct_name(inner: &PropertyInner) -> Option<String> {
    match inner {
        PropertyInner::Struct { name } => Some(name.clone()),
        _ => None,
    }
}

impl Element {
    /// `declared_struct` is the struct the owner's schema names for this element, when it does.
    fn of(
        type_name: Option<&str>,
        declared_struct: Option<String>,
        cursor: &Cursor<'_>,
    ) -> Result<Self, String> {
        let type_name = type_name.unwrap_or_default();
        match (type_name, declared_struct) {
            ("StructProperty", Some(name)) => Ok(Self::Struct(name)),
            ("BoolProperty", _) => Ok(Self::Bool),
            ("EnumProperty", _) => Ok(Self::Enum),
            _ => simple_inner(type_name).map(Self::Simple).ok_or_else(|| {
                cursor.err(format!(
                    "container elements of type {type_name} are not decoded in tagged mode"
                ))
            }),
        }
    }

    fn read(
        &self,
        cursor: &mut Cursor<'_>,
        ctx: &Ctx<'_>,
        diagnostics: &mut Diagnostics,
        depth: u32,
    ) -> Result<PropertyValue, String> {
        match self {
            Self::Simple(inner) => read_value(inner, cursor, ctx, diagnostics, depth),
            Self::Bool => Ok(PropertyValue::Bool {
                value: cursor.read_u8()? != 0,
            }),
            Self::Enum => Ok(PropertyValue::Enum {
                value: -1,
                name: Some(cursor.read_name(ctx.names())?),
                enum_type: None,
            }),
            Self::Struct(name) => read_tagged_struct(name, cursor, ctx, diagnostics, depth),
        }
    }

    /// The schema type an edit writes an element as.
    fn inner(&self) -> PropertyInner {
        match self {
            Self::Simple(inner) => inner.clone(),
            Self::Bool => PropertyInner::Bool,
            Self::Enum => PropertyInner::Enum {
                inner: Box::new(PropertyInner::Name),
                name: String::new(),
            },
            Self::Struct(name) => PropertyInner::Struct { name: name.clone() },
        }
    }

    /// A tag does not say which enum an element belongs to, so the layout just recorded names
    /// none rather than an empty one.
    fn settle(&self, diagnostics: &mut Diagnostics, key: bool) {
        // A reflected struct element is a tagged block of its own, so a fresh one is the empty
        // block, its terminating `None`, not the schema's unversioned header.
        if let Self::Struct(name) = self {
            if !matches!(
                structs::native_default(name),
                structs::NativeDefault::NotNative
            ) {
                return;
            }
            let Some(layout) = diagnostics.containers.last_mut() else {
                return;
            };
            if key {
                if let Some(keys) = layout.keys.as_mut() {
                    keys.default = None;
                    keys.default_recipe = None;
                    keys.default_name = Some("None".into());
                }
            } else {
                layout.default_element = None;
                layout.default_recipe = None;
                layout.default_name = Some("None".into());
            }
            return;
        }
        if !matches!(self, Self::Enum) {
            return;
        }
        if let Some(layout) = diagnostics.containers.last_mut() {
            if key {
                if let Some(keys) = layout.keys.as_mut() {
                    keys.enum_type = None;
                }
            } else {
                layout.element_enum = None;
            }
        }
    }
}

fn read_count(cursor: &mut Cursor<'_>) -> Result<usize, String> {
    let count = cursor.read_i32()?;
    if count < 0 || count as usize > cursor.remaining() {
        return Err(cursor.err(format!("implausible tagged container count {count}")));
    }
    Ok(count as usize)
}

/// Tag type names that map straight onto a schema property type, so the existing value reader can
/// be reused rather than duplicated.
fn simple_inner(type_name: &str) -> Option<PropertyInner> {
    Some(match type_name {
        "ByteProperty" => PropertyInner::Byte,
        "IntProperty" => PropertyInner::Int,
        "Int8Property" => PropertyInner::Int8,
        "Int16Property" => PropertyInner::Int16,
        "Int64Property" => PropertyInner::Int64,
        "UInt16Property" => PropertyInner::UInt16,
        "UInt32Property" => PropertyInner::UInt32,
        "UInt64Property" => PropertyInner::UInt64,
        "FloatProperty" => PropertyInner::Float,
        "DoubleProperty" => PropertyInner::Double,
        "StrProperty" => PropertyInner::Str,
        "NameProperty" => PropertyInner::Name,
        "TextProperty" => PropertyInner::Text,
        "ObjectProperty" => PropertyInner::Object,
        "WeakObjectProperty" => PropertyInner::WeakObject,
        "LazyObjectProperty" => PropertyInner::LazyObject,
        "SoftObjectProperty" => PropertyInner::SoftObject,
        "AssetObjectProperty" => PropertyInner::AssetObject,
        "InterfaceProperty" => PropertyInner::Interface,
        "DelegateProperty" => PropertyInner::Delegate,
        "MulticastDelegateProperty" | "MulticastInlineDelegateProperty" => {
            PropertyInner::MulticastDelegate
        }
        "FieldPathProperty" => PropertyInner::FieldPath,
        _ => return None,
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use retoc::legacy_asset::{FLegacyPackageHeader, FPackageNameMap};

    const NAMES: &[&str] = &[
        "None",
        "Damage",
        "IntProperty",
        "bEnabled",
        "BoolProperty",
        "Mystery",
        "SomeFutureProperty",
        "Label",
        "StrProperty",
    ];

    fn header() -> FLegacyPackageHeader {
        FLegacyPackageHeader {
            name_map: FPackageNameMap::create_from_names(
                NAMES.iter().map(|n| (*n).to_string()).collect(),
            ),
            ..Default::default()
        }
    }

    fn name(out: &mut Vec<u8>, value: &str) {
        let index = NAMES
            .iter()
            .position(|n| *n == value)
            .expect("name is in the test name map") as i32;
        out.extend_from_slice(&index.to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
    }

    /// Writes a tag with no type-specific payload and no property guid.
    fn tag(out: &mut Vec<u8>, property: &str, type_name: &str, size: i32) {
        name(out, property);
        name(out, type_name);
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
        out.push(0);
    }

    fn parse(data: &[u8]) -> Result<Vec<PropertyEntry>, String> {
        parse_recording(data).map(|(entries, _)| entries)
    }

    fn parse_recording(data: &[u8]) -> Result<(Vec<PropertyEntry>, Diagnostics), String> {
        let header = header();
        let ctx = Ctx {
            mappings: None,
            header: &header,
            fixups: None,
            synth: None,
            local: None,
        };
        let mut cursor = Cursor::new(data, 0x100);
        let mut entries = Vec::new();
        let mut diagnostics = Diagnostics::default();
        read_tagged_block(&mut cursor, &ctx, &mut diagnostics, 0, &mut entries, None)?;
        Ok((entries, diagnostics))
    }

    /// Every tag's size is recorded as a length prefix over its value, so a value edited to
    /// another width carries the tag with it.
    #[test]
    fn a_tags_size_is_recorded_as_the_length_prefix_of_its_value() {
        let mut data = Vec::new();
        tag(&mut data, "Label", "StrProperty", 9);
        data.extend_from_slice(&5i32.to_le_bytes());
        data.extend_from_slice(b"abcd\0");
        tag(&mut data, "Damage", "IntProperty", 4);
        data.extend_from_slice(&7i32.to_le_bytes());
        name(&mut data, "None");
        let (entries, diagnostics) = parse_recording(&data).expect("parse");
        assert_eq!(entries.len(), 2);
        assert_eq!(diagnostics.instanced.len(), 2);
        let label = &diagnostics.instanced[0];
        assert_eq!(label.size_at, 0x100 + 16, "after the two names");
        assert_eq!(label.payload_start, 0x100 + 25);
        assert_eq!(label.payload_end, 0x100 + 25 + 9);
        assert_eq!(
            entries[0].span,
            Some((label.payload_start, label.payload_end))
        );
        assert_eq!(
            diagnostics.instanced[1].payload_end,
            0x100 + 25 + 9 + 25 + 4
        );
    }

    #[test]
    fn a_tagged_int_reads_without_any_mappings_file() {
        let mut data = Vec::new();
        tag(&mut data, "Damage", "IntProperty", 4);
        data.extend_from_slice(&42i32.to_le_bytes());
        name(&mut data, "None");

        let entries = parse(&data).expect("should parse");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "Damage");
        assert!(matches!(entries[0].value, PropertyValue::Int { value: 42 }));
    }

    #[test]
    fn a_bool_takes_its_value_from_the_tag_and_consumes_no_value_bytes() {
        let mut data = Vec::new();
        name(&mut data, "bEnabled");
        name(&mut data, "BoolProperty");
        data.extend_from_slice(&0i32.to_le_bytes());
        data.extend_from_slice(&0i32.to_le_bytes());
        data.push(1);
        data.push(0);
        name(&mut data, "None");

        let entries = parse(&data).expect("should parse");
        assert!(matches!(
            entries[0].value,
            PropertyValue::Bool { value: true }
        ));
    }

    /// The property this crate cannot decode must cost exactly one value, not the rest of the
    /// export. This is the guarantee that makes tagged parsing safe.
    #[test]
    fn an_unknown_property_type_is_skipped_by_its_declared_size() {
        let mut data = Vec::new();
        tag(&mut data, "Mystery", "SomeFutureProperty", 6);
        data.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x11]);
        tag(&mut data, "Damage", "IntProperty", 4);
        data.extend_from_slice(&7i32.to_le_bytes());
        name(&mut data, "None");

        let entries = parse(&data).expect("should parse");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].name, "Damage");
        assert!(
            matches!(entries[1].value, PropertyValue::Int { value: 7 }),
            "the property after an undecodable one still has to read correctly"
        );
    }

    /// A decoder that reads the wrong number of bytes must be caught by the size check rather
    /// than silently shifting everything after it.
    #[test]
    fn a_value_that_does_not_match_its_declared_size_is_rejected_and_resynchronised() {
        let mut data = Vec::new();
        tag(&mut data, "Damage", "IntProperty", 8);
        data.extend_from_slice(&1i32.to_le_bytes());
        data.extend_from_slice(&2i32.to_le_bytes());
        tag(&mut data, "Label", "StrProperty", 9);
        data.extend_from_slice(&5i32.to_le_bytes());
        data.extend_from_slice(b"abcd\0");
        name(&mut data, "None");

        let entries = parse(&data).expect("should parse");
        assert_eq!(entries.len(), 2);
        let PropertyValue::Struct { fields, .. } = &entries[0].value else {
            panic!("a size mismatch should be reported, not decoded");
        };
        assert_eq!(fields[0].name, "(undecoded)");
        assert!(
            matches!(&entries[1].value, PropertyValue::Str { value } if value == "abcd"),
            "the next property must still line up"
        );
    }

    #[test]
    fn a_missing_terminator_is_an_error_rather_than_an_endless_read() {
        let mut data = Vec::new();
        tag(&mut data, "Damage", "IntProperty", 4);
        data.extend_from_slice(&1i32.to_le_bytes());
        assert!(parse(&data).is_err());
    }

    #[test]
    fn a_property_claiming_more_bytes_than_remain_is_refused() {
        let mut data = Vec::new();
        tag(&mut data, "Damage", "IntProperty", 4096);
        data.extend_from_slice(&1i32.to_le_bytes());
        let err = parse(&data).expect_err("should refuse");
        assert!(err.contains("declares 4096 bytes"), "{err}");
    }

    #[test]
    fn every_simple_tag_name_maps_to_a_property_type() {
        for name in ["IntProperty", "StrProperty", "ObjectProperty"] {
            assert!(simple_inner(name).is_some(), "{name} should map");
        }
        assert!(
            simple_inner("StructProperty").is_none(),
            "structs carry a name in the tag and need the dedicated path"
        );
        assert!(simple_inner("NotAThing").is_none());
    }
}
