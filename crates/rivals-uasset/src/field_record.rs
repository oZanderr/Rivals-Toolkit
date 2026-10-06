//! Field records: what `FProperty::Serialize` writes for each local, parameter and variable a
//! function or class declares. A record read from a package writes back byte for byte, and a new
//! one is written the way the cook writes every record of its kind.

use retoc::legacy_asset::FPackageNameMap;

use crate::header_edit::{self, Tables};
use crate::ustruct::{FieldRecord, RecordTail};

/// `RF_Public`, the only object flag on a function's field records and on a container's element.
const PUBLIC: u32 = 0x1;
/// What the cook leaves on a class's own variables: `RF_Public` and one more object flag.
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
}

impl FieldType {
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
                    "{head} is not a type a field can be declared with; use Bool, Byte, Int, Int64, Float, Double, Name, Str, Text, Object<path>, Class<path>, SoftObject<path>, SoftClass<path>, Interface<path>, Struct<path>, Enum<path>, Array<T>, Set<T> or Map<K, V>"
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
    };
    let mut record = record(name, ty, flags, tables)?;
    if role == NewField::Variable {
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
        FieldType::Enum(path) => {
            let class = if path.starts_with("/Script/") {
                ("/Script/CoreUObject", "Enum")
            } else {
                ("/Script/Engine", "UserDefinedEnum")
            };
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
