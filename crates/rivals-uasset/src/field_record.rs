//! Field records: what `FProperty::Serialize` writes for each local, parameter and variable a
//! function or class declares. A record read from a package writes back byte for byte, and a new
//! one is written the way the cook writes every record of its kind.

use retoc::legacy_asset::FPackageNameMap;

use crate::header_edit::{self, Tables};
use crate::ustruct::{FieldRecord, RecordTail};

/// `RF_Public`, the only object flag on a function's field records and on a container's element.
const PUBLIC: u32 = 0x1;
/// What the cook leaves on a class's own variables and a Blueprint struct's members: `RF_Public`
/// and one more object flag.
const CLASS_VARIABLE: u32 = 0x0020_0001;

/// `FBoolProperty`'s packing, the same in every bool record the game ships: a one-byte field at
/// offset zero, masked whole, held as a native bool.
const BOOL_PACKING: [u8; 6] = [1, 0, 1, 0xFF, 1, 1];

/// The `PropertyFlags` the cook leaves on a function's fields, by what each is to the function.
/// An input is `CPF_Parm | CPF_BlueprintVisible | CPF_BlueprintReadOnly`.
const INPUT: u64 = 0x94;
const BY_REFERENCE: u64 = 0x0800_0194;
const OUTPUT: u64 = 0x180;
const RETURN: u64 = 0x580;
/// `CPF_ReferenceParm`, which the compiler leaves on the array locals of a function.
const ARRAY_LOCAL: u64 = 0x0800_0000;
/// `CPF_Edit | CPF_BlueprintVisible | CPF_DisableEditOnInstance`, a variable as the Blueprint editor
/// makes one: settable on the class's defaults and readable and writable from its graphs.
const VARIABLE: u64 = 0x0001_0005;
/// `CPF_Edit | CPF_BlueprintVisible`, which every member of every Blueprint struct the game ships
/// carries.
const MEMBER: u64 = 0x5;

/// What a new field holds, as a text declares it. Classes, structs and enums are named by their
/// full path: a short name can stand for more than one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldType {
    Bool,
    Byte,
    Int8,
    Int16,
    Int,
    Int64,
    UInt16,
    UInt32,
    UInt64,
    Float,
    Double,
    Name,
    Str,
    Text,
    Object(String),
    WeakObject(String),
    SoftObject(String),
    Interface(String),
    /// A class reference, by the class it may hold.
    Class(String),
    SoftClass(String),
    /// A delegate, by the function whose signature it carries:
    /// `/Script/Engine.TimerDynamicDelegate__DelegateSignature`.
    Delegate(String),
    Struct(String),
    /// An enum held in a byte.
    Enum(String),
    Array(Box<FieldType>),
    Set(Box<FieldType>),
    Map(Box<FieldType>, Box<FieldType>),
}

/// What a new field is to the function or class that declares it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewField {
    Local,
    Input,
    ByReference,
    Output,
    Return,
    /// A variable of the class itself.
    Variable,
    /// A member of a Blueprint struct.
    Member,
}

/// Engine structs a value of which can be hashed, each defining `GetTypeHash`, so a map can be
/// keyed or a set held by a struct with a field of one.
const HASHED_STRUCTS: &[&str] = &[
    "/Script/CoreUObject.Guid",
    "/Script/CoreUObject.IntPoint",
    "/Script/CoreUObject.IntVector",
    "/Script/CoreUObject.IntVector4",
    "/Script/CoreUObject.Vector",
    "/Script/CoreUObject.Vector2D",
    "/Script/CoreUObject.Color",
    "/Script/CoreUObject.LinearColor",
    "/Script/CoreUObject.DateTime",
    "/Script/CoreUObject.Timespan",
    "/Script/CoreUObject.SoftObjectPath",
    "/Script/CoreUObject.SoftClassPath",
    "/Script/CoreUObject.TopLevelAssetPath",
    "/Script/CoreUObject.PrimaryAssetId",
    "/Script/CoreUObject.PrimaryAssetType",
    "/Script/GameplayTags.GameplayTag",
];

impl FieldType {
    /// Whether a value of the type can be hashed, which every field of a struct keying a map or
    /// held in a set needs. How a struct hashes is not known from here, so one counts as not.
    pub fn hashes(&self) -> bool {
        match self {
            Self::Struct(path) => HASHED_STRUCTS.contains(&path.as_str()),
            _ => !matches!(
                self,
                Self::Text | Self::Delegate(_) | Self::Array(_) | Self::Set(_) | Self::Map(..)
            ),
        }
    }

    /// Every object path the type names, its elements' included.
    pub fn paths(&self) -> Vec<&str> {
        match self {
            Self::Object(path)
            | Self::WeakObject(path)
            | Self::SoftObject(path)
            | Self::Interface(path)
            | Self::Class(path)
            | Self::SoftClass(path)
            | Self::Delegate(path)
            | Self::Struct(path)
            | Self::Enum(path) => vec![path.as_str()],
            Self::Array(inner) | Self::Set(inner) => inner.paths(),
            Self::Map(key, value) => {
                let mut paths = key.paths();
                paths.extend(value.paths());
                paths
            }
            _ => Vec::new(),
        }
    }

    /// The type as a function's signature prints it, so a declaration can be held to a field the
    /// function already has.
    pub fn printed(&self) -> String {
        let class = |path: &str| {
            let name = path.rsplit(['.', ':']).next().unwrap_or(path);
            name.to_string()
        };
        match self {
            Self::Bool => "Bool".into(),
            Self::Byte => "Byte".into(),
            Self::Int8 => "Int8".into(),
            Self::Int16 => "Int16".into(),
            Self::Int => "Int".into(),
            Self::Int64 => "Int64".into(),
            Self::UInt16 => "UInt16".into(),
            Self::UInt32 => "UInt32".into(),
            Self::UInt64 => "UInt64".into(),
            Self::Float => "Float".into(),
            Self::Double => "Double".into(),
            Self::Name => "Name".into(),
            Self::Str => "Str".into(),
            Self::Text => "Text".into(),
            Self::Object(path) => format!("Object<{}>", class(path)),
            Self::WeakObject(path) => format!("WeakObject<{}>", class(path)),
            Self::SoftObject(path) => format!("SoftObject<{}>", class(path)),
            Self::Interface(path) => format!("Interface<{}>", class(path)),
            Self::Class(path) => format!("Class<{}>", class(path)),
            Self::SoftClass(path) => format!("SoftClass<{}>", class(path)),
            // A signature prints a delegate without the function it takes its signature from.
            Self::Delegate(_) => "Delegate".into(),
            Self::Struct(path) | Self::Enum(path) => class(path),
            Self::Array(inner) => format!("Array<{}>", inner.printed()),
            Self::Set(inner) => format!("Set<{}>", inner.printed()),
            Self::Map(key, value) => format!("Map<{}, {}>", key.printed(), value.printed()),
        }
    }
}

/// Reads a type as a declaration writes it: `Int`, `Object</Script/Engine.Actor>`,
/// `Struct</Script/CoreUObject.Vector>`, `Map<Name, Array<Int>>`.
pub fn parse_field_type(text: &str) -> Result<FieldType, String> {
    let mut reader = TypeReader {
        chars: text.chars().collect(),
        at: 0,
    };
    let ty = reader.ty()?;
    reader.blanks();
    if reader.at < reader.chars.len() {
        return Err(format!(
            "{text} has more after its type: {}",
            reader.chars[reader.at..].iter().collect::<String>()
        ));
    }
    Ok(ty)
}

struct TypeReader {
    chars: Vec<char>,
    at: usize,
}

impl TypeReader {
    fn blanks(&mut self) {
        while self.chars.get(self.at).is_some_and(|c| c.is_whitespace()) {
            self.at += 1;
        }
    }

    /// A word or a path: anything up to a bracket, a comma or a space.
    fn word(&mut self) -> String {
        self.blanks();
        let start = self.at;
        while self
            .chars
            .get(self.at)
            .is_some_and(|c| !matches!(c, '<' | '>' | ',') && !c.is_whitespace())
        {
            self.at += 1;
        }
        self.chars[start..self.at].iter().collect()
    }

    fn eat(&mut self, c: char) -> bool {
        self.blanks();
        if self.chars.get(self.at) == Some(&c) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, c: char, after: &str) -> Result<(), String> {
        if self.eat(c) {
            Ok(())
        } else {
            Err(format!("expected '{c}' after {after}"))
        }
    }

    fn path(&mut self, what: &str) -> Result<String, String> {
        let path = self.word();
        if !path.starts_with('/') || !path.contains('.') {
            return Err(format!(
                "{what} takes a full path, such as {}: {path:?} is not one",
                match what {
                    "Struct" => "/Script/CoreUObject.Vector",
                    "Enum" => "/Script/Engine.ECollisionChannel",
                    "Delegate" => "/Script/Engine.TimerDynamicDelegate__DelegateSignature",
                    _ => "/Script/Engine.Actor",
                }
            ));
        }
        Ok(path)
    }

    fn ty(&mut self) -> Result<FieldType, String> {
        let head = self.word();
        let simple = match head.as_str() {
            "Bool" => Some(FieldType::Bool),
            "Byte" => Some(FieldType::Byte),
            "Int8" => Some(FieldType::Int8),
            "Int16" => Some(FieldType::Int16),
            "Int" => Some(FieldType::Int),
            "Int64" => Some(FieldType::Int64),
            "UInt16" => Some(FieldType::UInt16),
            "UInt32" => Some(FieldType::UInt32),
            "UInt64" => Some(FieldType::UInt64),
            "Float" => Some(FieldType::Float),
            "Double" => Some(FieldType::Double),
            "Name" => Some(FieldType::Name),
            "Str" => Some(FieldType::Str),
            "Text" => Some(FieldType::Text),
            _ => None,
        };
        if let Some(simple) = simple {
            return Ok(simple);
        }
        let known = [
            "Object",
            "WeakObject",
            "SoftObject",
            "Interface",
            "Class",
            "SoftClass",
            "Delegate",
            "Struct",
            "Enum",
            "Array",
            "Set",
            "Map",
        ];
        if !known.contains(&head.as_str()) {
            return Err(if head.is_empty() {
                "a type is missing".to_string()
            } else {
                format!(
                    "{head} is not a type a field can be declared with; use Bool, Byte, Int, Int64, Float, Double, Name, Str, Text, Object<path>, Class<path>, SoftObject<path>, SoftClass<path>, Interface<path>, Delegate<path>, Struct<path>, Enum<path>, Array<T>, Set<T> or Map<K, V>"
                )
            });
        }
        self.expect('<', &head)?;
        let ty = match head.as_str() {
            "Array" => FieldType::Array(Box::new(self.ty()?)),
            "Set" => FieldType::Set(Box::new(self.ty()?)),
            "Map" => {
                let key = self.ty()?;
                self.expect(',', "a map's key type")?;
                FieldType::Map(Box::new(key), Box::new(self.ty()?))
            }
            "Struct" => FieldType::Struct(self.path("Struct")?),
            "Enum" => FieldType::Enum(self.path("Enum")?),
            "Delegate" => FieldType::Delegate(self.path("Delegate")?),
            object => {
                let path = self.path(object)?;
                match object {
                    "Object" => FieldType::Object(path),
                    "WeakObject" => FieldType::WeakObject(path),
                    "SoftObject" => FieldType::SoftObject(path),
                    "Interface" => FieldType::Interface(path),
                    "Class" => FieldType::Class(path),
                    _ => FieldType::SoftClass(path),
                }
            }
        };
        self.expect('>', &head)?;
        Ok(ty)
    }
}

/// The record for a new field, written the way the cook writes every record of its kind. Whatever
/// it names is imported into `tables` when the package does not have it yet.
pub(crate) fn new_record(
    name: &str,
    ty: &FieldType,
    role: NewField,
    tables: &mut Tables,
) -> Result<FieldRecord, String> {
    let flags = match role {
        NewField::Local if matches!(ty, FieldType::Array(_)) => ARRAY_LOCAL,
        NewField::Local => 0,
        NewField::Input => INPUT,
        NewField::ByReference => BY_REFERENCE,
        NewField::Output => OUTPUT,
        NewField::Return => RETURN,
        NewField::Variable => VARIABLE,
        NewField::Member => MEMBER,
    };
    let mut record = record(name, ty, flags, tables)?;
    if matches!(role, NewField::Variable | NewField::Member) {
        record.field_flags = CLASS_VARIABLE;
    }
    Ok(record)
}

fn record(
    name: &str,
    ty: &FieldType,
    property_flags: u64,
    tables: &mut Tables,
) -> Result<FieldRecord, String> {
    let plain = |kind: &str, size: i32| (kind.to_string(), size, RecordTail::None);
    let (kind, element_size, tail) = match ty {
        FieldType::Bool => (
            "BoolProperty".to_string(),
            1,
            RecordTail::Bool(BOOL_PACKING),
        ),
        FieldType::Byte => ("ByteProperty".to_string(), 1, RecordTail::Index(0)),
        FieldType::Int8 => plain("Int8Property", 1),
        FieldType::Int16 => plain("Int16Property", 2),
        FieldType::Int => plain("IntProperty", 4),
        FieldType::Int64 => plain("Int64Property", 8),
        FieldType::UInt16 => plain("UInt16Property", 2),
        FieldType::UInt32 => plain("UInt32Property", 4),
        FieldType::UInt64 => plain("UInt64Property", 8),
        FieldType::Float => plain("FloatProperty", 4),
        FieldType::Double => plain("DoubleProperty", 8),
        FieldType::Name => plain("NameProperty", 12),
        FieldType::Str => plain("StrProperty", 16),
        FieldType::Text => plain("TextProperty", 24),
        FieldType::Object(path) => (
            "ObjectProperty".into(),
            8,
            RecordTail::Index(class_import(tables, path)?),
        ),
        FieldType::WeakObject(path) => (
            "WeakObjectProperty".into(),
            8,
            RecordTail::Index(class_import(tables, path)?),
        ),
        FieldType::SoftObject(path) => (
            "SoftObjectProperty".into(),
            48,
            RecordTail::Index(class_import(tables, path)?),
        ),
        FieldType::Interface(path) => (
            "InterfaceProperty".into(),
            16,
            RecordTail::Index(class_import(tables, path)?),
        ),
        // A class reference's own class is `Class`; what it may hold is its metaclass.
        FieldType::Class(path) => (
            "ClassProperty".into(),
            8,
            RecordTail::Indices(
                class_import(tables, "/Script/CoreUObject.Class")?,
                class_import(tables, path)?,
            ),
        ),
        FieldType::SoftClass(path) => (
            "SoftClassProperty".into(),
            48,
            RecordTail::Indices(
                class_import(tables, "/Script/CoreUObject.Class")?,
                class_import(tables, path)?,
            ),
        ),
        // A delegate names the function whose signature it carries: a native one is a delegate
        // function, a Blueprint's dispatcher signature a plain one. Its size is the cook's.
        FieldType::Delegate(path) => {
            let class = if path.starts_with("/Script/") {
                ("/Script/CoreUObject", "DelegateFunction")
            } else {
                ("/Script/CoreUObject", "Function")
            };
            (
                "DelegateProperty".into(),
                32,
                RecordTail::Index(import(tables, path, class)?),
            )
        }
        // The engine sets a struct field's size from the struct when it links the function or
        // class, whatever the record says, so a new one leaves it at zero.
        FieldType::Struct(path) => {
            let class = if path.starts_with("/Script/") {
                ("/Script/CoreUObject", "ScriptStruct")
            } else {
                ("/Script/Engine", "UserDefinedStruct")
            };
            (
                "StructProperty".into(),
                0,
                RecordTail::Index(import(tables, path, class)?),
            )
        }
        // A Blueprint enum is namespaced, and the editor holds a namespaced enum in a byte that names
        // it, as every Blueprint enum field the game ships is.
        FieldType::Enum(path) if !path.starts_with("/Script/") => (
            "ByteProperty".into(),
            1,
            RecordTail::Index(import(tables, path, ("/Script/Engine", "UserDefinedEnum"))?),
        ),
        FieldType::Enum(path) => {
            let class = ("/Script/CoreUObject", "Enum");
            let underlying = FieldRecord {
                kind: "ByteProperty".into(),
                name: "UnderlyingType".into(),
                field_flags: PUBLIC,
                array_dim: 1,
                element_size: 1,
                property_flags: 0,
                rep_index: 0,
                rep_notify: "None".into(),
                condition: 0,
                tail: RecordTail::Index(0),
            };
            (
                "EnumProperty".into(),
                1,
                RecordTail::Enum(import(tables, path, class)?, Box::new(underlying)),
            )
        }
        // A container's element takes the container's name, and a map's value the name with
        // `_Value` after it, as the compiler names them.
        FieldType::Array(inner) => (
            "ArrayProperty".into(),
            16,
            RecordTail::One(Box::new(record(name, inner, 0, tables)?)),
        ),
        FieldType::Set(inner) => (
            "SetProperty".into(),
            80,
            RecordTail::One(Box::new(record(name, inner, 0, tables)?)),
        ),
        FieldType::Map(key, value) => (
            "MapProperty".into(),
            80,
            RecordTail::Two(
                Box::new(record(name, key, 0, tables)?),
                Box::new(record(&format!("{name}_Value"), value, 0, tables)?),
            ),
        ),
    };
    Ok(FieldRecord {
        kind,
        name: name.to_string(),
        field_flags: PUBLIC,
        array_dim: 1,
        element_size,
        property_flags,
        rep_index: 0,
        rep_notify: "None".into(),
        condition: 0,
        tail,
    })
}

/// A class, imported as a native class or a Blueprint's generated one by where its path lies.
fn class_import(tables: &mut Tables, path: &str) -> Result<i32, String> {
    let class = if path.starts_with("/Script/") {
        ("/Script/CoreUObject", "Class")
    } else {
        ("/Script/Engine", "BlueprintGeneratedClass")
    };
    import(tables, path, class)
}

fn import(tables: &mut Tables, path: &str, (package, class): (&str, &str)) -> Result<i32, String> {
    header_edit::add_import(tables, path, Some((package.to_string(), class.to_string())))
        .map_err(|reason| format!("{path} cannot be imported: {reason}"))
}

/// What the loader has to see before the struct or class declaring `record`, as the cook lists
/// it: each struct or enum the record holds values of is read first (serialize before
/// serialize), and each class an object field of it may hold is created first (create before
/// serialize). Raw indices, deduplicated; the caller keeps those an IoStore package can hold.
pub(crate) fn record_dependencies(record: &FieldRecord) -> (Vec<i32>, Vec<i32>) {
    fn walk(record: &FieldRecord, serialize: &mut Vec<i32>, create: &mut Vec<i32>) {
        match (record.kind.as_str(), &record.tail) {
            ("StructProperty" | "ByteProperty", RecordTail::Index(index)) => serialize.push(*index),
            ("EnumProperty", RecordTail::Enum(index, _)) => serialize.push(*index),
            (
                "ObjectProperty" | "WeakObjectProperty" | "SoftObjectProperty"
                | "LazyObjectProperty" | "InterfaceProperty",
                RecordTail::Index(index),
            ) => create.push(*index),
            ("ClassProperty" | "SoftClassProperty", RecordTail::Indices(_, meta)) => {
                create.push(*meta)
            }
            // The function a delegate's signature is made before what holds the delegate is read.
            (
                "DelegateProperty"
                | "MulticastInlineDelegateProperty"
                | "MulticastSparseDelegateProperty",
                RecordTail::Index(signature),
            ) => create.push(*signature),
            (_, RecordTail::One(inner)) => walk(inner, serialize, create),
            (_, RecordTail::Two(key, value)) => {
                walk(key, serialize, create);
                walk(value, serialize, create);
            }
            _ => {}
        }
    }
    let (mut serialize, mut create) = (Vec::new(), Vec::new());
    walk(record, &mut serialize, &mut create);
    let distinct = |list: &mut Vec<i32>| {
        let mut seen = std::collections::HashSet::new();
        list.retain(|index| *index != 0 && seen.insert(*index));
    };
    distinct(&mut serialize);
    distinct(&mut create);
    (serialize, create)
}

/// Every index a record names, for the dependencies a save adds with it.
pub(crate) fn record_indices(record: &FieldRecord, out: &mut Vec<i32>) {
    match &record.tail {
        RecordTail::Index(index) => out.push(*index),
        RecordTail::Indices(first, second) => out.extend([*first, *second]),
        RecordTail::Enum(index, inner) => {
            out.push(*index);
            record_indices(inner, out);
        }
        RecordTail::One(inner) => record_indices(inner, out),
        RecordTail::Two(key, value) => {
            record_indices(key, out);
            record_indices(value, out);
        }
        RecordTail::None | RecordTail::Bool(_) | RecordTail::Name(_) => {}
    }
    out.retain(|index| *index != 0);
}

/// The bytes of `record`, its names stored in `names`.
pub fn encode_field_record(record: &FieldRecord, names: &mut FPackageNameMap) -> Vec<u8> {
    let mut out = Vec::new();
    write(record, names, &mut out);
    out
}

fn write(record: &FieldRecord, names: &mut FPackageNameMap, out: &mut Vec<u8>) {
    name(names, &record.kind, out);
    name(names, &record.name, out);
    out.extend_from_slice(&record.field_flags.to_le_bytes());
    out.extend_from_slice(&record.array_dim.to_le_bytes());
    out.extend_from_slice(&record.element_size.to_le_bytes());
    out.extend_from_slice(&record.property_flags.to_le_bytes());
    out.extend_from_slice(&record.rep_index.to_le_bytes());
    name(names, &record.rep_notify, out);
    out.push(record.condition);
    match &record.tail {
        RecordTail::None => {}
        RecordTail::Bool(packing) => out.extend_from_slice(packing),
        RecordTail::Index(index) => out.extend_from_slice(&index.to_le_bytes()),
        RecordTail::Indices(first, second) => {
            out.extend_from_slice(&first.to_le_bytes());
            out.extend_from_slice(&second.to_le_bytes());
        }
        RecordTail::Name(text) => name(names, text, out),
        RecordTail::Enum(index, inner) => {
            out.extend_from_slice(&index.to_le_bytes());
            write(inner, names, out);
        }
        RecordTail::One(inner) => write(inner, names, out),
        RecordTail::Two(key, value) => {
            write(key, names, out);
            write(value, names, out);
        }
    }
}

fn name(names: &mut FPackageNameMap, text: &str, out: &mut Vec<u8>) {
    let id = names.store(text);
    out.extend_from_slice(&id.index.to_le_bytes());
    out.extend_from_slice(&id.number.to_le_bytes());
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    /// An engine struct defining a hash keeps a struct holding it hashable; any other struct, a
    /// text or a container does not.
    #[test]
    fn a_hashed_engine_struct_hashes() {
        for (ty, hashes) in [
            ("Struct</Script/CoreUObject.Guid>", true),
            ("Struct</Script/GameplayTags.GameplayTag>", true),
            ("Struct</Script/CoreUObject.Transform>", false),
            ("Struct</Game/Mods/X.S>", false),
            ("Text", false),
            ("Array<Int>", false),
            ("Name", true),
        ] {
            assert_eq!(parse_field_type(ty).unwrap().hashes(), hashes, "{ty}");
        }
    }

    #[test]
    fn a_declared_type_reads_every_kind_and_its_paths() {
        let cases = [
            ("Int", FieldType::Int),
            ("Bool", FieldType::Bool),
            (
                "Object</Script/Engine.Actor>",
                FieldType::Object("/Script/Engine.Actor".into()),
            ),
            (
                "Class< /Game/A/BP_X.BP_X_C >",
                FieldType::Class("/Game/A/BP_X.BP_X_C".into()),
            ),
            (
                "Struct</Script/CoreUObject.Vector>",
                FieldType::Struct("/Script/CoreUObject.Vector".into()),
            ),
            (
                "Struct</Game/S_Entry.S_Entry>",
                FieldType::Struct("/Game/S_Entry.S_Entry".into()),
            ),
            (
                "Delegate</Script/Engine.TimerDynamicDelegate__DelegateSignature>",
                FieldType::Delegate(
                    "/Script/Engine.TimerDynamicDelegate__DelegateSignature".into(),
                ),
            ),
            (
                "Map<Name, Array<Int>>",
                FieldType::Map(
                    Box::new(FieldType::Name),
                    Box::new(FieldType::Array(Box::new(FieldType::Int))),
                ),
            ),
        ];
        for (text, want) in cases {
            assert_eq!(parse_field_type(text).unwrap(), want, "{text}");
        }
        assert_eq!(
            parse_field_type("Map<Name, Array<Object</Script/Engine.Actor>>>")
                .unwrap()
                .printed(),
            "Map<Name, Array<Object<Actor>>>"
        );
    }

    #[test]
    fn a_short_class_name_or_an_unknown_type_is_refused() {
        let short = parse_field_type("Object<Actor>").unwrap_err();
        assert!(short.contains("full path"), "{short}");
        let unknown = parse_field_type("Vector").unwrap_err();
        assert!(unknown.contains("not a type"), "{unknown}");
        assert!(parse_field_type("Array<Int").is_err());
        assert!(parse_field_type("Int extra").is_err());
        assert!(parse_field_type("Struct</Script/CoreUObject.Vector, 0>").is_err());
    }

    /// Every record a package declares writes back to exactly the bytes it was read from, its
    /// names already in the table.
    #[test]
    fn every_record_writes_back_byte_for_byte() {
        let built = crate::script_fixture::event_graph().build();
        let parsed = built.parsed();
        let header = built.header();
        let bytes = [built.asset.as_slice(), built.exports.as_slice()].concat();
        let mut names = header.name_map.clone();
        let mut seen = 0;
        for export in &parsed.exports {
            for (record, (start, end)) in export.layout.iter().flat_map(|l| &l.records) {
                assert_eq!(
                    encode_field_record(record, &mut names),
                    bytes[*start as usize..*end as usize],
                    "{}",
                    record.name
                );
                seen += 1;
            }
        }
        assert!(seen > 0);
        assert_eq!(names.num_names(), header.name_map.num_names());
    }

    fn tables() -> Tables {
        Tables {
            names: FPackageNameMap::create_from_names(vec!["None".into()]),
            imports: Vec::new(),
        }
    }

    /// A Blueprint enum is held in a byte naming it, as the editor holds a namespaced enum.
    #[test]
    fn a_blueprint_enum_field_is_a_byte_naming_it() {
        let mut tables = tables();
        let ty = parse_field_type("Enum</Game/Data/EMode.EMode>").unwrap();
        let mode = new_record("Mode", &ty, NewField::Member, &mut tables).unwrap();
        let RecordTail::Index(index) = mode.tail else {
            panic!("{:?}", mode.tail);
        };
        assert_eq!((mode.kind.as_str(), mode.element_size), ("ByteProperty", 1));
        assert_eq!(
            tables.import_class(index),
            Some(("/Script/Engine".into(), "UserDefinedEnum".into()))
        );
    }

    /// A field's struct and enum types are read before what declares it, and the classes its object
    /// fields may hold are created before it, its elements' included.
    #[test]
    fn a_field_s_types_load_as_the_cook_orders_them() {
        let mut tables = tables();
        let depends = |text: &str, tables: &mut Tables| {
            let ty = parse_field_type(text).unwrap();
            record_dependencies(&new_record("Field", &ty, NewField::Member, tables).unwrap())
        };
        let (read, made) = depends("Struct</Game/Data/Card.Card>", &mut tables);
        assert_eq!((read.len(), made.len()), (1, 0));
        let (read, made) = depends("Array<Enum</Game/Data/EMode.EMode>>", &mut tables);
        assert_eq!((read.len(), made.len()), (1, 0));
        let (read, made) = depends("SoftObject</Game/Data/BP_Thing.BP_Thing_C>", &mut tables);
        assert_eq!((read.len(), made.len()), (0, 1));
        let (read, made) = depends(
            "Map<Name, Class</Game/Data/BP_Thing.BP_Thing_C>>",
            &mut tables,
        );
        assert_eq!((read.len(), made.len()), (0, 1));
        assert_eq!(depends("Int", &mut tables), (Vec::new(), Vec::new()));
    }

    /// A new local is written as the cook writes every local of its kind.
    #[test]
    fn a_new_local_takes_the_cooks_flags_and_sizes() {
        let mut tables = tables();
        let count = new_record("Count", &FieldType::Int, NewField::Local, &mut tables).unwrap();
        assert_eq!(
            (
                count.kind.as_str(),
                count.element_size,
                count.property_flags
            ),
            ("IntProperty", 4, 0)
        );
        assert_eq!(
            (
                count.field_flags,
                count.array_dim,
                count.rep_notify.as_str()
            ),
            (PUBLIC, 1, "None")
        );
        let flag = new_record("On", &FieldType::Bool, NewField::Input, &mut tables).unwrap();
        assert_eq!(flag.tail, RecordTail::Bool(BOOL_PACKING));
        assert_eq!(flag.property_flags, INPUT);
        let list = new_record(
            "Items",
            &FieldType::Array(Box::new(FieldType::Name)),
            NewField::Local,
            &mut tables,
        )
        .unwrap();
        assert_eq!(list.property_flags, ARRAY_LOCAL);
        let RecordTail::One(element) = &list.tail else {
            panic!("an array holds its element's record");
        };
        assert_eq!((element.name.as_str(), element.element_size), ("Items", 12));
        let map = new_record(
            "Lookup",
            &FieldType::Map(Box::new(FieldType::Int), Box::new(FieldType::Str)),
            NewField::Return,
            &mut tables,
        )
        .unwrap();
        let RecordTail::Two(key, value) = &map.tail else {
            panic!("a map holds its key's and value's records");
        };
        assert_eq!(
            (key.name.as_str(), value.name.as_str()),
            ("Lookup", "Lookup_Value")
        );
        assert_eq!(map.property_flags, RETURN);
    }

    /// A delegate local is written as the cook writes the one a Create Event node leaves: the
    /// function its signature comes from named after the record, imported as a delegate function
    /// when native and as a plain function when a Blueprint's dispatcher declares it, and made
    /// before what declares the local. A signature prints it as no more than a delegate.
    #[test]
    fn a_delegate_local_names_the_function_its_signature_comes_from() {
        let mut tables = tables();
        let native = parse_field_type(
            "Delegate</Script/Marvel.StackPanel:OnStackPanelTopChanged__DelegateSignature>",
        )
        .unwrap();
        assert_eq!(native.printed(), "Delegate");
        let local = new_record("OnTop", &native, NewField::Local, &mut tables).unwrap();
        assert_eq!(
            (
                local.kind.as_str(),
                local.element_size,
                local.property_flags,
                local.field_flags,
            ),
            ("DelegateProperty", 32, 0, PUBLIC)
        );
        let RecordTail::Index(index) = local.tail else {
            panic!("a delegate names its signature: {:?}", local.tail);
        };
        assert_eq!(
            tables.import_class(index),
            Some(("/Script/CoreUObject".into(), "DelegateFunction".into()))
        );
        assert_eq!(record_dependencies(&local), (Vec::new(), vec![index]));
        let dispatcher = parse_field_type(
            "Delegate</Game/Mods/BP_Loader.BP_Loader_C:OnDone__DelegateSignature>",
        )
        .unwrap();
        let local = new_record("OnDone", &dispatcher, NewField::Local, &mut tables).unwrap();
        let RecordTail::Index(index) = local.tail else {
            panic!("a delegate names its signature: {:?}", local.tail);
        };
        assert_eq!(
            tables.import_class(index),
            Some(("/Script/CoreUObject".into(), "Function".into()))
        );
        assert!(parse_field_type("Delegate<OnDone__DelegateSignature>").is_err());
    }

    /// An object type imports its class, and a struct type its struct, with the size left to the
    /// engine.
    #[test]
    fn a_new_field_imports_what_it_names() {
        let mut tables = tables();
        let actor = new_record(
            "Target",
            &FieldType::Object("/Script/Engine.Actor".into()),
            NewField::Local,
            &mut tables,
        )
        .unwrap();
        let RecordTail::Index(index) = actor.tail else {
            panic!("an object field names its class");
        };
        assert!(index < 0);
        assert_eq!(
            tables.import_class(index),
            Some(("/Script/CoreUObject".into(), "Class".into()))
        );
        let vector = FieldType::Struct("/Script/CoreUObject.Vector".into());
        let at = new_record("At", &vector, NewField::Local, &mut tables).unwrap();
        let RecordTail::Index(index) = at.tail else {
            panic!("a struct field names its struct");
        };
        assert_eq!(
            tables.import_class(index),
            Some(("/Script/CoreUObject".into(), "ScriptStruct".into()))
        );
        assert_eq!((at.kind.as_str(), at.element_size), ("StructProperty", 0));
    }
}
