//! A package cooked with unversioned properties, built byte by byte, and the mappings it was cooked
//! against, for the edits only such a package takes: values a header slot governs, read through a
//! schema rather than through tags.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use retoc::legacy_asset::{
    EPackageFlags, FLegacyPackageFileSummary, FLegacyPackageHeader, FMinimalName, FObjectExport,
    FObjectImport, FPackageNameMap,
};
use retoc::zen::FPackageIndex;
use usmap::PropertyInner;

use crate::mappings::Mappings;
use crate::package::FALLBACK_ENGINE_VERSION;

const HEADER_SIZE: usize = 1024;

/// The package's name map, which the builders index into.
pub const NAMES: &[&str] = &[
    "None",
    "/Script/CoreUObject",
    "Package",
    "Class",
    "/Script/Test",
    "TestClass",
    "/Game/Helpers",
    "Helper",
    "TestObject",
    "Handler",
    "OnFired",
    "Count",
];

/// Where the package's imports sit, as the package indices values point at.
pub const TEST_CLASS: i32 = -2;
pub const HELPER: i32 = -4;
pub const HELPER_PATH: &str = "/Game/Helpers.Helper";
pub const TEST_CLASS_PATH: &str = "/Script/Test.TestClass";

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

/// What the export holds for a slot of its class.
pub enum Held {
    Bytes(Vec<u8>),
    /// Flagged zero in the header, so nothing is written for it.
    Zero,
    /// Skipped by the header, so it inherits.
    Skipped,
}

/// One property the fixture's class declares, and what the export holds for it.
pub struct Slot {
    pub name: &'static str,
    pub inner: PropertyInner,
    pub held: Held,
}

fn slot(name: &'static str, inner: PropertyInner, held: Held) -> Slot {
    Slot { name, inner, held }
}

fn delegate(object: i32, function: &str) -> Vec<u8> {
    let mut out = object.to_le_bytes().to_vec();
    name(&mut out, function);
    out
}

/// The class's slots, in schema order.
pub fn slots() -> Vec<Slot> {
    let mut bindings = 1i32.to_le_bytes().to_vec();
    bindings.extend(delegate(HELPER, "OnFired"));
    let mut watched = 1i32.to_le_bytes().to_vec();
    name(&mut watched, "Count");
    watched.extend_from_slice(&TEST_CLASS.to_le_bytes());
    let lazy: Vec<u8> = (1u32..=4).flat_map(u32::to_le_bytes).collect();
    vec![
        slot(
            "OnFired",
            PropertyInner::Delegate,
            Held::Bytes(delegate(HELPER, "Handler")),
        ),
        slot(
            "OnChanged",
            PropertyInner::MulticastDelegate,
            Held::Bytes(bindings),
        ),
        slot("Watched", PropertyInner::FieldPath, Held::Bytes(watched)),
        slot("Lazy", PropertyInner::LazyObject, Held::Bytes(lazy)),
        slot(
            "Count",
            PropertyInner::Int,
            Held::Bytes(7i32.to_le_bytes().to_vec()),
        ),
        slot("Spare", PropertyInner::Delegate, Held::Skipped),
        slot("Listeners", PropertyInner::MulticastDelegate, Held::Skipped),
        slot("Path", PropertyInner::FieldPath, Held::Skipped),
        slot("ZeroLazy", PropertyInner::LazyObject, Held::Zero),
        native("When", "DateTime", 100i64.to_le_bytes().to_vec()),
        native("Transform", "Matrix", doubles(&[1.0; 16])),
        native("Bounds", "Box", {
            let mut bounds = doubles(&[0.0, 0.0, 0.0, 1.0, 1.0, 1.0]);
            bounds.push(1);
            bounds
        }),
        native("Ball", "Sphere", doubles(&[0.0, 0.0, 0.0, 4.0])),
        native("Key", "RichCurveKey", {
            let mut key = vec![0u8, 1, 2];
            for value in [0.5f32, 1.0, 0.0, 0.0, 0.0, 0.0] {
                key.extend_from_slice(&value.to_le_bytes());
            }
            key
        }),
        native("Range", "MovieSceneFrameRange", {
            let mut range = Vec::new();
            for (kind, frame) in [(1i8, 0i32), (2, 30)] {
                range.push(kind as u8);
                range.extend_from_slice(&frame.to_le_bytes());
            }
            range
        }),
        native(
            "Agents",
            "NavAgentSelector",
            0b1001u32.to_le_bytes().to_vec(),
        ),
        slot(
            "Lookup",
            PropertyInner::Map {
                key: Box::new(PropertyInner::Name),
                value: Box::new(PropertyInner::Int),
            },
            Held::Bytes({
                let mut pairs = 0i32.to_le_bytes().to_vec();
                pairs.extend_from_slice(&3i32.to_le_bytes());
                for (key, value) in [("OnFired", 1i32), ("Handler", 2), ("Count", 3)] {
                    name(&mut pairs, key);
                    pairs.extend_from_slice(&value.to_le_bytes());
                }
                pairs
            }),
        ),
        slot(
            "Numbers",
            PropertyInner::Array {
                inner: Box::new(PropertyInner::Int),
            },
            Held::Bytes(
                [3i32, 10, 20, 30]
                    .iter()
                    .flat_map(|value| value.to_le_bytes())
                    .collect(),
            ),
        ),
        slot(
            "Targets",
            PropertyInner::Array {
                inner: Box::new(PropertyInner::Object),
            },
            Held::Bytes(
                [2i32, HELPER, 0]
                    .iter()
                    .flat_map(|value| value.to_le_bytes())
                    .collect(),
            ),
        ),
    ]
}

fn native(name: &'static str, kind: &str, bytes: Vec<u8>) -> Slot {
    let inner = PropertyInner::Struct { name: kind.into() };
    slot(name, inner, Held::Bytes(bytes))
}

fn doubles(values: &[f64]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

/// The export's bytes: the header naming what it stores, the values, and no object guid.
fn export_bytes(slots: &[Slot]) -> Vec<u8> {
    let empty = crate::unversioned::empty_header(slots.len());
    let mut header =
        crate::unversioned::read_header(&mut crate::reader::Cursor::new(&empty, 0)).unwrap();
    for (index, slot) in slots.iter().enumerate() {
        match slot.held {
            Held::Bytes(_) => header.insert_value(index as u32, false).unwrap(),
            Held::Zero => header.insert_value(index as u32, true).unwrap(),
            Held::Skipped => {}
        }
    }
    let mut out = header.write().unwrap();
    for slot in slots {
        if let Held::Bytes(bytes) = &slot.held {
            out.extend_from_slice(bytes);
        }
    }
    out.extend_from_slice(&0i32.to_le_bytes());
    out
}

/// A package whose one export, of class `TestClass`, holds what [`slots`] says.
pub fn unversioned_package() -> (Vec<u8>, Vec<u8>) {
    let exports = export_bytes(&slots());
    let mut summary = FLegacyPackageFileSummary {
        package_name: "/Game/TestPackage".to_string(),
        ..Default::default()
    };
    summary.versioning_info.package_file_version = FALLBACK_ENGINE_VERSION.package_file_version();
    summary.versioning_info.total_header_size = HEADER_SIZE as i32;
    summary.package_flags = EPackageFlags::Cooked as u32
        | EPackageFlags::FilterEditorOnly as u32
        | EPackageFlags::UsesUnversionedProperties as u32;
    let minimal = |value: &str| FMinimalName {
        index: index_of(value),
        number: 0,
    };
    let import =
        |class_package: &str, class_name: &str, outer: FPackageIndex, object: &str| FObjectImport {
            class_package: minimal(class_package),
            class_name: minimal(class_name),
            outer_index: outer,
            object_name: minimal(object),
            is_optional: false,
        };
    let header = FLegacyPackageHeader {
        summary,
        name_map: FPackageNameMap::create_from_names(
            NAMES.iter().map(|n| (*n).to_string()).collect(),
        ),
        imports: vec![
            import(
                "/Script/CoreUObject",
                "Package",
                FPackageIndex::create_null(),
                "/Script/Test",
            ),
            import(
                "/Script/CoreUObject",
                "Class",
                FPackageIndex::create_import(0),
                "TestClass",
            ),
            import(
                "/Script/CoreUObject",
                "Package",
                FPackageIndex::create_null(),
                "/Game/Helpers",
            ),
            import(
                "/Script/Test",
                "TestClass",
                FPackageIndex::create_import(2),
                "Helper",
            ),
        ],
        exports: vec![FObjectExport {
            object_name: minimal("TestObject"),
            class_index: FPackageIndex::create_import(1),
            serial_offset: 0,
            serial_size: exports.len() as i64,
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
    (asset.into_inner(), exports)
}

/// The schema [`unversioned_package`] was cooked against.
pub fn unversioned_mappings() -> Mappings {
    use usmap::{Property, Struct};
    let properties = slots()
        .into_iter()
        .enumerate()
        .map(|(index, slot)| Property {
            name: slot.name.into(),
            array_dim: 1,
            index: index as u16,
            inner: slot.inner,
        })
        .collect();
    // A class is laid out only when its chain roots at `Object`.
    Mappings::from_structs(vec![
        Struct {
            name: "Object".into(),
            super_struct: None,
            properties: Vec::new(),
        },
        Struct {
            name: "TestClass".into(),
            super_struct: Some("Object".into()),
            properties,
        },
    ])
}
