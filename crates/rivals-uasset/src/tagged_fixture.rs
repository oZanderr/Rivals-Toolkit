//! A package cooked with tagged properties, built byte by byte, and a schema for it, for tests
//! here and in the crates that edit packages.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use retoc::legacy_asset::{
    EPackageFlags, FLegacyPackageFileSummary, FLegacyPackageHeader, FObjectExport, FPackageNameMap,
};

use crate::mappings::Mappings;
use crate::package::FALLBACK_ENGINE_VERSION;

pub const HEADER_SIZE: usize = 1024;
/// The package's name map, which the builders index into.
pub const NAMES: &[&str] = &[
    "None",
    "TestPackage",
    "TestObject",
    "IntProperty",
    "StrProperty",
    "BoolProperty",
    "NameProperty",
    "TextProperty",
    "StructProperty",
    "ArrayProperty",
    "EnumProperty",
    "Damage",
    "Label",
    "Enabled",
    "Tag",
    "Title",
    "Pos",
    "MyStruct",
    "X",
    "Values",
    "Points",
    "Mode",
    "EMode",
    "EMode::A",
    "EMode::B",
    "Foo",
    "Id",
    "Guid",
    "Asset",
    "TopLevelAssetPath",
    "Path",
    "MarvelSoftObjectPath",
    "/Game/A",
    "B",
    "Words",
    "Names",
    "SetProperty",
    "Scores",
    "MapProperty",
    "Flags",
    "Modes",
    "Holder",
    "MyHolder",
    "Pairs",
    "Structs",
    "Count",
    "Inner",
    "Flag",
];

pub fn index_of(value: &str) -> i32 {
    NAMES
        .iter()
        .position(|n| *n == value)
        .expect("name is in the test name map") as i32
}

pub fn name(out: &mut Vec<u8>, value: &str) {
    out.extend_from_slice(&index_of(value).to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
}

pub fn string(out: &mut Vec<u8>, value: &str) {
    out.extend_from_slice(&(value.len() as i32 + 1).to_le_bytes());
    out.extend_from_slice(value.as_bytes());
    out.push(0);
}

/// A tag's common head: the property's name, its type, its value's size and an array index of 0.
pub fn head(out: &mut Vec<u8>, property: &str, kind: &str, size: usize) {
    name(out, property);
    name(out, kind);
    out.extend_from_slice(&(size as i32).to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
}

pub fn int(out: &mut Vec<u8>, property: &str, value: i32) {
    head(out, property, "IntProperty", 4);
    out.push(0);
    out.extend_from_slice(&value.to_le_bytes());
}

/// `MyStruct { X }` as a tagged block of its own.
pub fn my_struct(x: i32) -> Vec<u8> {
    let mut out = Vec::new();
    int(&mut out, "X", x);
    name(&mut out, "None");
    out
}

/// A package whose one export holds the tagged properties in `e`.
pub fn package_of(e: Vec<u8>) -> (Vec<u8>, Vec<u8>) {
    let mut summary = FLegacyPackageFileSummary {
        package_name: "/Game/TestPackage".to_string(),
        ..Default::default()
    };
    summary.versioning_info.package_file_version = FALLBACK_ENGINE_VERSION.package_file_version();
    summary.versioning_info.total_header_size = HEADER_SIZE as i32;
    // Filtered, as every cooked package is.
    summary.package_flags = EPackageFlags::Cooked as u32 | EPackageFlags::FilterEditorOnly as u32;
    let header = FLegacyPackageHeader {
        summary,
        name_map: FPackageNameMap::create_from_names(
            NAMES.iter().map(|n| (*n).to_string()).collect(),
        ),
        exports: vec![FObjectExport {
            object_name: retoc::legacy_asset::FMinimalName {
                index: index_of("TestObject"),
                number: 0,
            },
            serial_offset: 0,
            serial_size: e.len() as i64,
            ..Default::default()
        }],
        ..Default::default()
    };
    let mut asset = std::io::Cursor::new(Vec::new());
    header
        .serialize(
            &mut asset,
            Some(HEADER_SIZE),
            &retoc::logging::Log::no_log(),
        )
        .expect("serialize the test package header");
    (asset.into_inner(), e)
}

/// `MyHolder` storing `Count`, `Flag` and `Inner` (a `MyStruct` with `X` only), where its schema
/// also declares properties it does not hold.
pub fn sparse_package() -> (Vec<u8>, Vec<u8>) {
    let mut holder = Vec::new();
    int(&mut holder, "Count", 3);
    head(&mut holder, "Flag", "BoolProperty", 0);
    holder.push(1);
    holder.push(0);
    let inner = my_struct(1);
    head(&mut holder, "Inner", "StructProperty", inner.len());
    name(&mut holder, "MyStruct");
    holder.extend_from_slice(&[0; 16]);
    holder.push(0);
    holder.extend_from_slice(&inner);
    name(&mut holder, "None");

    let mut e = Vec::new();
    head(&mut e, "Holder", "StructProperty", holder.len());
    name(&mut e, "MyHolder");
    e.extend_from_slice(&[0; 16]);
    e.push(0);
    e.extend_from_slice(&holder);
    name(&mut e, "None");
    e.extend_from_slice(&0i32.to_le_bytes());
    package_of(e)
}

/// The schema [`sparse_package`] was cooked against.
pub fn sparse_mappings() -> Mappings {
    use usmap::{Property, PropertyInner, Struct};
    let my_struct = || PropertyInner::Struct {
        name: "MyStruct".into(),
    };
    let property = |name: &str, index: u16, inner: PropertyInner| Property {
        name: name.into(),
        array_dim: 1,
        index,
        inner,
    };
    Mappings::from_structs(vec![
        Struct {
            name: "MyHolder".into(),
            super_struct: None,
            properties: vec![
                property("Count", 0, PropertyInner::Int),
                property("Flag", 1, PropertyInner::Bool),
                property("Inner", 2, my_struct()),
                property("Extra", 3, PropertyInner::Int),
                property("Label", 4, PropertyInner::Str),
                property("On", 5, PropertyInner::Bool),
                property(
                    "Mode",
                    6,
                    PropertyInner::Enum {
                        inner: Box::new(PropertyInner::Byte),
                        name: "EMode".into(),
                    },
                ),
                property("Nested", 7, my_struct()),
                property(
                    "Items",
                    8,
                    PropertyInner::Array {
                        inner: Box::new(my_struct()),
                    },
                ),
                property(
                    "Scores",
                    9,
                    PropertyInner::Map {
                        key: Box::new(PropertyInner::Name),
                        value: Box::new(PropertyInner::Int),
                    },
                ),
                property(
                    "Counts",
                    10,
                    PropertyInner::Array {
                        inner: Box::new(PropertyInner::Int),
                    },
                ),
            ],
        },
        Struct {
            name: "MyStruct".into(),
            super_struct: None,
            properties: vec![
                property("X", 0, PropertyInner::Int),
                property("Y", 1, PropertyInner::Int),
            ],
        },
    ])
}
