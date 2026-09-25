//! Reads the field records a `UStruct` export carries, so a Blueprint struct the mappings file
//! never saw can still be decoded, and walks the rest of a class or function export so the object
//! references in its layout can be followed.

use retoc::legacy_asset::FLegacyPackageHeader;
use serde::Serialize;
use usmap::{Property, PropertyInner, Struct};

use crate::mappings::Mappings;
use crate::package::{AssetBundle, parse_package};
use crate::props::IndexRef;
use crate::reader::Cursor;

/// `FUNC_Net`: the function carries a replication offset after its flags.
const FUNC_NET: u32 = 0x0000_0040;

/// `FField::FlagsPrivate`, which sits between the field's type and its name.
const FIELD_FLAGS_BYTES: usize = 4;

/// `ElementSize`, `PropertyFlags` and `RepIndex`, which follow `ArrayDim`. Only the flags are kept,
/// for what they say about a function's parameters.
#[cfg(test)]
const FIELD_TAIL_BYTES: usize = 4 + 8 + 2;
const ELEMENT_SIZE_BYTES: usize = 4;
const REP_INDEX_BYTES: usize = 2;

/// `CPF_Parm`, `CPF_OutParm`, `CPF_ReturnParm` and `CPF_ReferenceParm`.
const CPF_PARM: u64 = 0x80;
const CPF_OUT_PARM: u64 = 0x100;
const CPF_RETURN_PARM: u64 = 0x400;
const CPF_REFERENCE_PARM: u64 = 0x0800_0000;

/// A function's parameters and locals, read from the field records its export declares.
#[derive(Debug, Clone, Serialize)]
pub struct FunctionSignature {
    pub params: Vec<FunctionField>,
    pub locals: Vec<FunctionField>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FunctionField {
    pub name: String,
    /// The type as a reader would write it: `Str`, `Array<Str>`, `S_MeshEntry`.
    pub kind: String,
    pub role: FieldRole,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldRole {
    In,
    /// Passed by reference: the caller's variable, which the function may change.
    Ref,
    Out,
    Return,
    Local,
}

impl FunctionSignature {
    fn of(fields: &[Property], flags: &[u64]) -> Self {
        let mut params = Vec::new();
        let mut locals = Vec::new();
        for (field, &flag) in fields.iter().zip(flags) {
            let role = if flag & CPF_PARM == 0 {
                FieldRole::Local
            } else if flag & CPF_RETURN_PARM != 0 {
                FieldRole::Return
            } else if flag & CPF_REFERENCE_PARM != 0 {
                FieldRole::Ref
            } else if flag & CPF_OUT_PARM != 0 {
                FieldRole::Out
            } else {
                FieldRole::In
            };
            let entry = FunctionField {
                name: field.name.clone(),
                kind: type_text(&field.inner),
                role,
            };
            if role == FieldRole::Local {
                locals.push(entry);
            } else {
                params.push(entry);
            }
        }
        Self { params, locals }
    }

    /// `Name(In: T, ref R: T) -> Out: T`, with several outputs in parentheses.
    pub fn render(&self, name: &str) -> String {
        let inputs: Vec<String> = self
            .params
            .iter()
            .filter(|p| matches!(p.role, FieldRole::In | FieldRole::Ref))
            .map(|p| {
                let prefix = if p.role == FieldRole::Ref { "ref " } else { "" };
                format!("{prefix}{}: {}", p.name, p.kind)
            })
            .collect();
        let outputs: Vec<String> = self
            .params
            .iter()
            .filter(|p| matches!(p.role, FieldRole::Out | FieldRole::Return))
            .map(|p| format!("{}: {}", p.name, p.kind))
            .collect();
        let head = format!("{name}({})", inputs.join(", "));
        match outputs.len() {
            0 => head,
            1 => format!("{head} -> {}", outputs[0]),
            _ => format!("{head} -> ({})", outputs.join(", ")),
        }
    }
}

/// A field's type written out in full, containers included.
fn type_text(inner: &PropertyInner) -> String {
    match inner {
        PropertyInner::Struct { name } => name.clone(),
        PropertyInner::Enum { name, inner } if name.is_empty() => type_text(inner),
        PropertyInner::Enum { name, .. } => name.clone(),
        PropertyInner::Array { inner } => format!("Array<{}>", type_text(inner)),
        PropertyInner::Set { key } => format!("Set<{}>", type_text(key)),
        PropertyInner::Map { key, value } => {
            format!("Map<{}, {}>", type_text(key), type_text(value))
        }
        PropertyInner::Optional { inner } => format!("Optional<{}>", type_text(inner)),
        other => crate::mappings::kind_name(other).to_string(),
    }
}

/// `FBoolProperty` stores its packing here. The width is measured against real assets; the
/// breakdown of the six bytes is not confirmed, so they are skipped rather than named.
const BOOL_PACKING_BYTES: usize = 6;

/// The shipped classes carry one or two metadata entries; a count past this is a misread layout.
const MAX_CLASS_METADATA: usize = 16;

/// A struct definition recovered from a package, ready to be turned into a schema.
pub struct StructDefinition {
    inner: Struct,
}

impl StructDefinition {
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    pub fn super_struct(&self) -> Option<&str> {
        self.inner.super_struct.as_deref()
    }

    pub fn field_count(&self) -> usize {
        self.inner.properties.len()
    }

    pub fn properties(&self) -> &[Property] {
        &self.inner.properties
    }
}

/// Every struct defined by an export of this package. Blueprint structs live one per package, but
/// nothing here assumes that. The parse records each definition while it walks the export's tail.
pub fn read_struct_definitions(
    bundle: &AssetBundle<'_>,
    mappings: Option<&Mappings>,
) -> Result<Vec<StructDefinition>, String> {
    Ok(collect_definitions(
        &parse_package(bundle, mappings)?,
        false,
    ))
}

/// [`read_struct_definitions`] with every class named by its object path, `/Pkg/Path.Class_C`,
/// so two Blueprint classes that share a name stay apart. Structs keep their plain name: rows and
/// properties refer to them by it.
pub fn read_struct_definitions_pathed(
    bundle: &AssetBundle<'_>,
    mappings: Option<&Mappings>,
) -> Result<Vec<StructDefinition>, String> {
    Ok(collect_definitions(&parse_package(bundle, mappings)?, true))
}

/// The struct and class definitions a parsed package carries, classes named by object path, for a
/// second parse of the same package whose own Blueprint classes the mappings file describes wrongly.
pub fn definitions_of(parsed: &crate::package::ParsedPackage) -> Vec<StructDefinition> {
    collect_definitions(parsed, true)
}

fn collect_definitions(
    parsed: &crate::package::ParsedPackage,
    classes_by_path: bool,
) -> Vec<StructDefinition> {
    parsed
        .exports
        .iter()
        .filter_map(|export| {
            let mut inner = export.struct_definition.clone()?;
            // Only a class is named by path: a struct is looked up by the name its properties
            // carry, and a generated one (an animation Blueprint's mutable data) has a parent too.
            if classes_by_path && export.class_name.ends_with("Class") {
                inner.name = format!("{}.{}", parsed.info.package_name, inner.name);
            }
            Some(StructDefinition { inner })
        })
        .collect()
}

/// What a `UStruct`-derived export stores after its properties, as far as the scanner follows it:
/// where every object reference sits, and where bytecode the scanner cannot read into lies.
#[derive(Debug)]
pub(crate) struct StructTail {
    pub references: Vec<IndexRef>,
    pub bytecode: Option<(u64, u64)>,
    /// Where the two size words in front of the bytecode sit, so a replacement of another length
    /// can rewrite them.
    pub sizes_at: u64,
    /// `BytecodeBufferSize`: the script's size once loaded, which is the space its jump offsets
    /// index. Object pointers and names are wider in memory than on disk, so it is not the
    /// stored size.
    pub buffer_size: u32,
    /// The field records of a script struct or a class, which are the schema for its own values.
    pub definition: Option<Vec<Property>>,
    /// A function's parameters and locals, from the same records and their flags.
    pub signature: Option<FunctionSignature>,
    /// `SuperStruct` as written, zero when there is none.
    pub super_struct: i32,
    /// Where that index sits, so a reparent can splice over it.
    pub super_struct_at: u64,
}

/// Walks a `UStruct`, `UClass` or `UFunction` export from after its object guid to its end, in
/// `Super::Serialize` order, recording every `FPackageIndex` on the way. Anything but landing
/// exactly on the export's end is an error: a layout guessed wrong would misplace every reference
/// after the mistake, and the caller is better off leaving the tail opaque.
pub(crate) fn scan_struct_tail(
    cursor: &mut Cursor<'_>,
    header: &FLegacyPackageHeader,
    chain: &[&str],
) -> Result<StructTail, String> {
    let mut references = Vec::new();
    let super_struct_at = cursor.file_offset();
    let super_struct = take_index(cursor, &mut references)?;
    for _ in 0..read_count(cursor, "UStruct children")? {
        take_index(cursor, &mut references)?;
    }
    let mut properties = Vec::new();
    let mut flags = Vec::new();
    for ordinal in 0..read_count(cursor, "UStruct child properties")? {
        let (field, flag) = read_flagged_field(cursor, header, ordinal, &mut references)?;
        properties.push(field);
        flags.push(flag);
    }
    // Bytecode is stored behind its size, so it can be stepped over without being understood.
    let sizes_at = cursor.file_offset();
    let buffer_size = cursor.read_i32()?.max(0) as u32;
    let storage = read_count(cursor, "bytecode storage")?;
    let bytecode = (storage > 0).then(|| {
        let at = cursor.file_offset();
        (at, at + storage as u64)
    });
    cursor.skip(storage)?;

    if chain.contains(&"Class") {
        for _ in 0..read_count(cursor, "function map")? {
            cursor.skip(8)?;
            take_index(cursor, &mut references)?;
        }
        // ClassFlags, ClassWithin, ClassConfigName, ClassGeneratedBy. The generated-by reference
        // precedes the interfaces: the two are indistinguishable for a class without interfaces
        // (a null index and an empty count are the same four bytes), and every class with them
        // desynced while the order was the other way round.
        cursor.skip(4)?;
        take_index(cursor, &mut references)?;
        cursor.skip(8)?;
        take_index(cursor, &mut references)?;
        // `FImplementedInterface`: the class, its pointer offset and whether Blueprint implements it.
        for _ in 0..read_count(cursor, "interfaces")? {
            take_index(cursor, &mut references)?;
            cursor.skip(8)?;
        }
        // bDeprecatedForceScriptOrder, an unused name, bCooked.
        cursor.skip(4 + 8 + 4)?;
        take_index(cursor, &mut references)?;
        if chain.contains(&"BlueprintGeneratedClass") && cursor.remaining() > 0 {
            read_class_metadata(cursor, header)?;
        }
        if chain.contains(&"RigVMMemoryStorageGeneratorClass") {
            read_memory_storage_class_tail(cursor)?;
        }
        // A RigVM Blueprint class serializes its default object's VM again after the layout, an
        // opaque blob the caller names as a payload; the layout before it is still the definition.
        if chain.contains(&"RigVMBlueprintGeneratedClass") {
            return Ok(StructTail {
                references,
                bytecode,
                sizes_at,
                buffer_size,
                definition: Some(properties),
                signature: None,
                super_struct,
                super_struct_at,
            });
        }
    } else if chain.contains(&"Function") {
        let flags = cursor.read_u32()?;
        if flags & FUNC_NET != 0 {
            cursor.skip(2)?;
        }
        take_index(cursor, &mut references)?;
        cursor.skip(4)?;
    } else if chain.contains(&"ScriptStruct") {
        // `UScriptStruct::Serialize` adds the struct flags. A Blueprint struct then writes its
        // default instance, which the caller reads with the fields scanned above; its guid is one
        // of its properties, not part of this tail.
        cursor.skip(4)?;
        return Ok(StructTail {
            references,
            bytecode,
            sizes_at,
            buffer_size,
            definition: Some(properties),
            signature: None,
            super_struct,
            super_struct_at,
        });
    }
    if cursor.remaining() != 0 {
        return Err(cursor.err(format!(
            "{} bytes left after the struct tail",
            cursor.remaining()
        )));
    }
    let signature = chain
        .contains(&"Function")
        .then(|| FunctionSignature::of(&properties, &flags));
    Ok(StructTail {
        references,
        bytecode,
        sizes_at,
        buffer_size,
        // A class's own fields are worth keeping: a Blueprint class revised after the mappings
        // were dumped is only readable from here.
        definition: chain.contains(&"Class").then_some(properties),
        signature,
        super_struct,
        super_struct_at,
    })
}

/// `URigVMMemoryStorageGeneratorClass::Serialize`: the property path descriptions (property
/// index, head type, segment path), then the memory type byte.
fn read_memory_storage_class_tail(cursor: &mut Cursor<'_>) -> Result<(), String> {
    for _ in 0..read_count(cursor, "RigVM property path")? {
        cursor.read_i32()?;
        cursor.read_string()?;
        cursor.read_string()?;
    }
    cursor.read_u8()?;
    Ok(())
}

/// This game's engine follows a cooked Blueprint class with a `TMap<FName, FString>` of class
/// metadata: the Blueprint type, and the interfaces the class implements when it has any.
fn read_class_metadata(
    cursor: &mut Cursor<'_>,
    header: &FLegacyPackageHeader,
) -> Result<(), String> {
    let count = read_count(cursor, "class metadata")?;
    if count > MAX_CLASS_METADATA {
        return Err(cursor.err(format!("implausible class metadata count {count}")));
    }
    for _ in 0..count {
        let key = cursor.read_name(&header.name_map)?;
        let value = cursor.read_string()?;
        if key == "BlueprintType" && !value.starts_with("BPTYPE_") {
            return Err(cursor.err(format!(
                "expected a Blueprint type after the class, found {value:?}"
            )));
        }
    }
    Ok(())
}

fn take_index(cursor: &mut Cursor<'_>, references: &mut Vec<IndexRef>) -> Result<i32, String> {
    let at = cursor.file_offset();
    let index = cursor.read_i32()?;
    if index != 0 {
        references.push(IndexRef { at, index });
    }
    Ok(index)
}

/// A schema source holding only synthesised structs, consulted when the mappings file has no entry.
pub fn mappings_from_definitions(definitions: Vec<StructDefinition>) -> Mappings {
    mappings_from_definitions_with(definitions, None)
}

/// [`mappings_from_definitions`] with each definition's ancestry copied in from `parents`, so a
/// Blueprint class recovered from its package still decodes the properties it inherits.
pub fn mappings_from_definitions_with(
    definitions: Vec<StructDefinition>,
    parents: Option<&Mappings>,
) -> Mappings {
    mappings_from_structs_with(definitions.into_iter().map(|d| d.inner).collect(), parents)
}

/// A synthesised set built against `onto` instead of the mappings it was built against: what it
/// recovered itself stays, and the parent chains are copied again, so a twin chosen in `onto`
/// reaches the recovered classes too.
pub(crate) fn rebase_synth(synth: &Mappings, onto: &Mappings) -> Mappings {
    mappings_from_structs_with(synth.recovered(onto), Some(onto))
}

fn mappings_from_structs_with(mut structs: Vec<Struct>, parents: Option<&Mappings>) -> Mappings {
    if let Some(parents) = parents {
        let mut known: std::collections::HashSet<String> =
            structs.iter().map(|s| s.name.clone()).collect();
        let wanted: Vec<String> = structs
            .iter()
            .filter_map(|s| s.super_struct.clone())
            .collect();
        for parent in wanted {
            for entry in parents.chain_entries(&parent) {
                if known.insert(entry.name.clone()) {
                    structs.push(entry);
                }
            }
        }
    }
    Mappings::from_structs(structs)
}

/// Records appear in the order UE rebuilds `PropertyLink`, which is the order the unversioned
/// header indexes into, so the running ordinal is the schema index.
fn read_field(
    cursor: &mut Cursor<'_>,
    header: &FLegacyPackageHeader,
    ordinal: usize,
    references: &mut Vec<IndexRef>,
) -> Result<Property, String> {
    Ok(read_flagged_field(cursor, header, ordinal, references)?.0)
}

/// A field record with its `PropertyFlags`.
fn read_flagged_field(
    cursor: &mut Cursor<'_>,
    header: &FLegacyPackageHeader,
    ordinal: usize,
    references: &mut Vec<IndexRef>,
) -> Result<(Property, u64), String> {
    let kind = cursor.read_name(&header.name_map)?;
    let name = cursor.read_name(&header.name_map)?;
    cursor.skip(FIELD_FLAGS_BYTES)?;
    let array_dim = cursor.read_i32()?;
    cursor.skip(ELEMENT_SIZE_BYTES)?;
    let flags = cursor.read_u64()?;
    cursor.skip(REP_INDEX_BYTES)?;
    let _rep_notify = cursor.read_name(&header.name_map)?;
    let _replication_condition = cursor.read_u8()?;
    let inner = read_kind(&kind, cursor, header, references)?;
    let property = Property {
        name,
        array_dim: u8::try_from(array_dim.clamp(1, i32::from(u8::MAX)))
            .map_err(|_| cursor.err("implausible ArrayDim"))?,
        index: u16::try_from(ordinal).map_err(|_| cursor.err("too many fields"))?,
        inner,
    };
    Ok((property, flags))
}

/// The per-type tail. An unknown type name is fatal rather than guessed: reading the wrong width
/// would silently shift every field after it.
fn read_kind(
    kind: &str,
    cursor: &mut Cursor<'_>,
    header: &FLegacyPackageHeader,
    references: &mut Vec<IndexRef>,
) -> Result<PropertyInner, String> {
    let inner = match kind {
        // A `TEnumAsByte` is a byte property carrying an enum, and the mappings represent it as an
        // enum too. The distinction is not cosmetic: inside a container an enum is written as its
        // enumerator's `FName`, eight bytes, where a bare byte is one.
        "ByteProperty" => match object_name(header, take_index(cursor, references)?) {
            Some(name) => PropertyInner::Enum {
                inner: Box::new(PropertyInner::Byte),
                name,
            },
            None => PropertyInner::Byte,
        },
        "BoolProperty" => {
            cursor.skip(BOOL_PACKING_BYTES)?;
            PropertyInner::Bool
        }
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
        "ObjectProperty" => {
            take_index(cursor, references)?;
            PropertyInner::Object
        }
        "WeakObjectProperty" => {
            take_index(cursor, references)?;
            PropertyInner::WeakObject
        }
        "LazyObjectProperty" => {
            take_index(cursor, references)?;
            PropertyInner::LazyObject
        }
        "InterfaceProperty" => {
            take_index(cursor, references)?;
            PropertyInner::Interface
        }
        "SoftObjectProperty" => {
            take_index(cursor, references)?;
            PropertyInner::SoftObject
        }
        // A class reference names a second type, the metaclass, after the property class. Measured
        // on `SoftClassProperty`; `ClassProperty` is the same shape one level up the hierarchy.
        "ClassProperty" => {
            take_index(cursor, references)?;
            take_index(cursor, references)?;
            PropertyInner::Object
        }
        "SoftClassProperty" => {
            take_index(cursor, references)?;
            take_index(cursor, references)?;
            PropertyInner::SoftObject
        }
        // Delegates name the function whose signature they carry.
        "DelegateProperty" => {
            take_index(cursor, references)?;
            PropertyInner::Delegate
        }
        "MulticastInlineDelegateProperty" | "MulticastSparseDelegateProperty" => {
            take_index(cursor, references)?;
            PropertyInner::MulticastDelegate
        }
        // The property class is an `FFieldClass`, written as its name.
        "FieldPathProperty" => {
            cursor.skip(8)?;
            PropertyInner::FieldPath
        }
        "StructProperty" => {
            let index = take_index(cursor, references)?;
            PropertyInner::Struct {
                name: object_name(header, index)
                    .ok_or_else(|| cursor.err("struct property names no type"))?,
            }
        }
        "EnumProperty" => {
            let index = take_index(cursor, references)?;
            let name = object_name(header, index).unwrap_or_default();
            let underlying = read_nested(cursor, header, references)?;
            PropertyInner::Enum {
                inner: Box::new(underlying),
                name,
            }
        }
        "ArrayProperty" => PropertyInner::Array {
            inner: Box::new(read_nested(cursor, header, references)?),
        },
        "SetProperty" => PropertyInner::Set {
            key: Box::new(read_nested(cursor, header, references)?),
        },
        "MapProperty" => {
            let key = read_nested(cursor, header, references)?;
            let value = read_nested(cursor, header, references)?;
            PropertyInner::Map {
                key: Box::new(key),
                value: Box::new(value),
            }
        }
        other => return Err(cursor.err(format!("unknown field type {other}"))),
    };
    Ok(inner)
}

/// Container inners and an enum's underlying type are whole field records of their own.
fn read_nested(
    cursor: &mut Cursor<'_>,
    header: &FLegacyPackageHeader,
    references: &mut Vec<IndexRef>,
) -> Result<PropertyInner, String> {
    Ok(read_field(cursor, header, 0, references)?.inner)
}

fn object_name(header: &FLegacyPackageHeader, index: i32) -> Option<String> {
    crate::package::object_short_name(header, retoc::zen::FPackageIndex { index })
}

fn read_count(cursor: &mut Cursor<'_>, what: &str) -> Result<usize, String> {
    let count = cursor.read_i32()?;
    if count < 0 || count as usize > cursor.remaining() {
        return Err(cursor.err(format!("implausible {what} count {count}")));
    }
    Ok(count as usize)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use retoc::legacy_asset::FPackageNameMap;

    use super::*;

    const NAMES: &[&str] = &[
        "None",
        "ObjectProperty",
        "Target",
        "Run",
        "BlueprintType",
        "ImplementedInterfaces",
        "IntProperty",
        "StrProperty",
        "Index",
        "Label",
        "Scratch",
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
        let index = NAMES.iter().position(|n| *n == value).expect("named") as i32;
        out.extend_from_slice(&index.to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
    }

    fn i32s(out: &mut Vec<u8>, values: &[i32]) {
        for value in values {
            out.extend_from_slice(&value.to_le_bytes());
        }
    }

    /// A field record with no type tail beyond its common part, carrying `flags`.
    fn plain_field(out: &mut Vec<u8>, kind: &str, field: &str, flags: u64) {
        name(out, kind);
        name(out, field);
        i32s(out, &[0, 1, 4]); // object flags, ArrayDim, ElementSize
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&[0, 0]); // RepIndex
        name(out, "None"); // RepNotifyFunc
        out.push(0); // replication condition
    }

    /// Parameters and locals share one list of records; only the flags tell them apart.
    #[test]
    fn a_function_tail_sorts_its_fields_into_parameters_and_locals() {
        let mut out = Vec::new();
        i32s(&mut out, &[0, 0, 3]); // no super, no children, three fields
        plain_field(&mut out, "IntProperty", "Index", CPF_PARM);
        plain_field(&mut out, "StrProperty", "Label", CPF_PARM | CPF_OUT_PARM);
        plain_field(&mut out, "IntProperty", "Scratch", 0);
        i32s(&mut out, &[0, 0]); // no bytecode
        i32s(&mut out, &[0, 0, 0]); // FunctionFlags, EventGraphFunction, EventGraphCallOffset
        let mut cursor = Cursor::new(&out, 0);
        let tail = scan_struct_tail(&mut cursor, &header(), &["Function"]).expect("tail");
        assert!(tail.definition.is_none(), "a function is not a schema");
        let signature = tail.signature.expect("signature");
        assert_eq!(signature.render("Pick"), "Pick(Index: Int) -> Label: Str");
        assert_eq!(signature.locals.len(), 1);
        assert_eq!(signature.locals[0].name, "Scratch");
    }

    #[test]
    fn a_reference_parameter_is_an_input_marked_ref() {
        let field = |name: &str, role| FunctionField {
            name: name.into(),
            kind: "Array<Str>".into(),
            role,
        };
        let signature = FunctionSignature {
            params: vec![
                field("Paths", FieldRole::Ref),
                field("Sorted", FieldRole::Out),
                field("Count", FieldRole::Return),
            ],
            locals: Vec::new(),
        };
        assert_eq!(
            signature.render("SortPaks"),
            "SortPaks(ref Paths: Array<Str>) -> (Sorted: Array<Str>, Count: Array<Str>)"
        );
    }

    /// A class with one function child, one object property typed by an import, two bytes of
    /// bytecode, a function map entry and a default object, laid out as `UClass::Serialize` writes
    /// them, then this game's metadata trailer.
    fn class_tail() -> Vec<u8> {
        class_tail_with(&[])
    }

    /// [`class_tail`] implementing `interfaces`, which the trailer then lists too.
    fn class_tail_with(interfaces: &[i32]) -> Vec<u8> {
        let mut out = Vec::new();
        i32s(&mut out, &[-1]); // SuperStruct
        i32s(&mut out, &[1, 3]); // Children: one function export
        i32s(&mut out, &[1]); // one property
        name(&mut out, "ObjectProperty");
        name(&mut out, "Target");
        i32s(&mut out, &[0, 1]); // flags, ArrayDim
        out.extend_from_slice(&[0u8; FIELD_TAIL_BYTES]);
        name(&mut out, "None"); // RepNotifyFunc
        out.push(0); // replication condition
        i32s(&mut out, &[-2]); // PropertyClass
        i32s(&mut out, &[2, 2]); // bytecode size, storage
        out.extend_from_slice(&[0xAA, 0xBB]);
        i32s(&mut out, &[1]); // one function map entry
        name(&mut out, "Run");
        i32s(&mut out, &[3]);
        i32s(&mut out, &[0]); // ClassFlags
        i32s(&mut out, &[-3]); // ClassWithin
        name(&mut out, "None"); // ClassConfigName
        i32s(&mut out, &[-4]); // ClassGeneratedBy
        i32s(&mut out, &[interfaces.len() as i32]);
        for interface in interfaces {
            i32s(&mut out, &[*interface, 0x40, 1]); // class, PointerOffset, bImplementedByK2
        }
        i32s(&mut out, &[0]); // bDeprecatedForceScriptOrder
        name(&mut out, "None");
        i32s(&mut out, &[1]); // bCooked
        i32s(&mut out, &[2]); // ClassDefaultObject
        i32s(&mut out, &[if interfaces.is_empty() { 1 } else { 2 }]);
        name(&mut out, "BlueprintType");
        string(&mut out, "BPTYPE_Normal");
        if !interfaces.is_empty() {
            name(&mut out, "ImplementedInterfaces");
            string(&mut out, INTERFACES);
        }
        out
    }

    const INTERFACES: &str =
        "((Interface=\"/Script/CoreUObject.Class'/Script/UMG.UserObjectListEntry'\"))";

    fn string(out: &mut Vec<u8>, value: &str) {
        i32s(out, &[value.len() as i32 + 1]);
        out.extend_from_slice(value.as_bytes());
        out.push(0);
    }

    #[test]
    fn a_class_tail_yields_every_reference_and_the_bytecode_range() {
        let bytes = class_tail();
        let mut cursor = Cursor::new(&bytes, 0x100);
        let tail = scan_struct_tail(
            &mut cursor,
            &header(),
            &["BlueprintGeneratedClass", "Class"],
        )
        .expect("scan");
        let indices: Vec<i32> = tail.references.iter().map(|r| r.index).collect();
        assert_eq!(indices, vec![-1, 3, -2, 3, -3, -4, 2]);
        assert_eq!(
            tail.references[1].at,
            0x100 + 8,
            "the child sits after the count"
        );
        let bytecode = tail.bytecode.expect("bytecode");
        assert_eq!(bytecode.1 - bytecode.0, 2);
        assert_eq!(
            &bytes[(bytecode.0 - 0x100) as usize..(bytecode.1 - 0x100) as usize],
            &[0xAA, 0xBB]
        );
    }

    /// Interface records follow the generated-by reference, and the trailer then lists the
    /// interfaces as a second metadata entry.
    #[test]
    fn a_class_implementing_interfaces_records_them_after_its_generated_by_reference() {
        let bytes = class_tail_with(&[-5, -6]);
        let mut cursor = Cursor::new(&bytes, 0);
        let tail = scan_struct_tail(
            &mut cursor,
            &header(),
            &[
                "WidgetBlueprintGeneratedClass",
                "BlueprintGeneratedClass",
                "Class",
            ],
        )
        .expect("scan");
        let indices: Vec<i32> = tail.references.iter().map(|r| r.index).collect();
        assert_eq!(indices, vec![-1, 3, -2, 3, -3, -4, -5, -6, 2]);
        assert_eq!(cursor.remaining(), 0);
    }

    /// A memory storage generator class closes with its property paths and a memory type byte; a
    /// RigVM Blueprint class leaves its inline VM for the caller to name, definition in hand.
    #[test]
    fn rigvm_class_tails_are_read_or_left_for_the_payload() {
        let mut bytes = class_tail();
        let trailer = bytes.len() - (4 + 8 + 4 + 14);
        bytes.truncate(trailer);
        i32s(&mut bytes, &[1, 10, 8]);
        bytes.extend_from_slice(b"FVector\0");
        i32s(&mut bytes, &[2]);
        bytes.extend_from_slice(b"X\0");
        bytes.push(0);
        let mut cursor = Cursor::new(&bytes, 0);
        let tail = scan_struct_tail(
            &mut cursor,
            &header(),
            &["Class", "RigVMMemoryStorageGeneratorClass"],
        )
        .expect("scan");
        assert_eq!(cursor.remaining(), 0);
        assert!(tail.definition.is_some());

        let mut bytes = class_tail();
        bytes.extend_from_slice(&[0xAA; 40]);
        let mut cursor = Cursor::new(&bytes, 0);
        let tail = scan_struct_tail(
            &mut cursor,
            &header(),
            &[
                "Class",
                "BlueprintGeneratedClass",
                "RigVMBlueprintGeneratedClass",
            ],
        )
        .expect("scan");
        assert_eq!(cursor.remaining(), 40, "the VM blob is left unread");
        assert_eq!(tail.definition.map(|d| d.len()), Some(1));
    }

    #[test]
    fn an_implausible_metadata_count_is_refused() {
        let mut bytes = class_tail();
        let trailer = bytes.len() - (4 + 8 + 4 + 14);
        bytes[trailer..trailer + 4].copy_from_slice(&17i32.to_le_bytes());
        bytes.extend(std::iter::repeat_n(0u8, 64));
        let mut cursor = Cursor::new(&bytes, 0);
        let error = scan_struct_tail(
            &mut cursor,
            &header(),
            &["BlueprintGeneratedClass", "Class"],
        )
        .expect_err("refused");
        assert!(error.contains("class metadata count 17"), "{error}");
    }

    #[test]
    fn a_tail_that_does_not_end_on_the_export_is_refused() {
        let mut bytes = class_tail();
        bytes.push(0);
        let mut cursor = Cursor::new(&bytes, 0);
        let chain = ["BlueprintGeneratedClass", "Class"];
        let error = scan_struct_tail(&mut cursor, &header(), &chain).expect_err("refused");
        assert!(error.contains("1 bytes left"), "{error}");
        // A plain class has no trailer, so the same bytes do not parse as one.
        let mut cursor = Cursor::new(&bytes, 0);
        assert!(scan_struct_tail(&mut cursor, &header(), &["Class"]).is_err());
    }

    /// A Blueprint struct's tail hands back its fields and stops before the default instance,
    /// whose layout only those fields describe.
    #[test]
    fn a_script_struct_tail_returns_its_fields_and_leaves_the_default_instance() {
        let mut out = Vec::new();
        i32s(&mut out, &[0, 0, 1]); // no super, no children, one property
        name(&mut out, "ObjectProperty");
        name(&mut out, "Target");
        i32s(&mut out, &[0, 1]);
        out.extend_from_slice(&[0u8; FIELD_TAIL_BYTES]);
        name(&mut out, "None");
        out.push(0);
        i32s(&mut out, &[-2]);
        i32s(&mut out, &[0, 0]); // no bytecode
        i32s(&mut out, &[0x10]); // StructFlags
        out.extend_from_slice(&[0xAA, 0xBB]); // the default instance, left for the caller
        let mut cursor = Cursor::new(&out, 0);
        let tail = scan_struct_tail(
            &mut cursor,
            &header(),
            &["UserDefinedStruct", "ScriptStruct", "Struct"],
        )
        .expect("scan");
        let fields = tail.definition.expect("definition");
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].name, "Target");
        assert!(matches!(fields[0].inner, PropertyInner::Object));
        assert_eq!(
            cursor.remaining(),
            2,
            "the default instance is not consumed here"
        );
    }

    #[test]
    fn a_function_tail_reads_its_flags_and_event_graph_link() {
        let mut out = Vec::new();
        i32s(&mut out, &[-1, 0, 0, 0, 0]); // super, no children, no properties, no bytecode
        out.extend_from_slice(&(FUNC_NET | 0x1).to_le_bytes());
        out.extend_from_slice(&[0u8; 2]); // RepOffset
        i32s(&mut out, &[4, 12]); // EventGraphFunction, EventGraphCallOffset
        let mut cursor = Cursor::new(&out, 0);
        let tail = scan_struct_tail(&mut cursor, &header(), &["Function", "Struct"]).expect("scan");
        let indices: Vec<i32> = tail.references.iter().map(|r| r.index).collect();
        assert_eq!(indices, vec![-1, 4]);
        assert!(tail.bytecode.is_none());
    }
}
