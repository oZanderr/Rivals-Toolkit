//! Edits applied to a package cooked with tagged properties, read back, and compared.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use retoc::legacy_asset::{
    EPackageFlags, FLegacyPackageFileSummary, FLegacyPackageHeader, FObjectExport, FPackageNameMap,
};

use crate::edit::{EditOp, PackageEdits, ValueEdit, kind_of, patch_package, verify_patch};
use crate::package::{
    AssetBundle, ExportStatus, FALLBACK_ENGINE_VERSION, ParsedPackage, parse_package,
};
use crate::value::{PropertyEntry, PropertyValue};

const HEADER_SIZE: usize = 1024;
const NAMES: &[&str] = &[
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
];

fn index_of(value: &str) -> i32 {
    NAMES
        .iter()
        .position(|n| *n == value)
        .expect("name is in the test name map") as i32
}

fn name(out: &mut Vec<u8>, value: &str) {
    out.extend_from_slice(&index_of(value).to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
}

fn string(out: &mut Vec<u8>, value: &str) {
    out.extend_from_slice(&(value.len() as i32 + 1).to_le_bytes());
    out.extend_from_slice(value.as_bytes());
    out.push(0);
}

/// A tag's common head: the property's name, its type, its value's size and an array index of 0.
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

/// `MyStruct { X }` as a tagged block of its own.
fn my_struct(x: i32) -> Vec<u8> {
    let mut out = Vec::new();
    int(&mut out, "X", x);
    name(&mut out, "None");
    out
}

/// One export holding one property of every shape an edit treats differently.
fn tagged_package() -> (Vec<u8>, Vec<u8>) {
    let mut e = Vec::new();
    int(&mut e, "Damage", 99);

    let mut label = Vec::new();
    string(&mut label, "hi");
    head(&mut e, "Label", "StrProperty", label.len());
    e.push(0);
    e.extend_from_slice(&label);

    head(&mut e, "Enabled", "BoolProperty", 0);
    e.push(1);
    e.push(0);

    head(&mut e, "Tag", "NameProperty", 8);
    e.push(0);
    name(&mut e, "Foo");

    // A culture-invariant text: flags, history `None`, then the string it holds.
    let mut title = Vec::new();
    title.extend_from_slice(&0u32.to_le_bytes());
    title.push(0xFF);
    title.extend_from_slice(&1i32.to_le_bytes());
    string(&mut title, "Hello");
    head(&mut e, "Title", "TextProperty", title.len());
    e.push(0);
    e.extend_from_slice(&title);

    let pos = my_struct(1);
    head(&mut e, "Pos", "StructProperty", pos.len());
    name(&mut e, "MyStruct");
    e.extend_from_slice(&[0; 16]);
    e.push(0);
    e.extend_from_slice(&pos);

    head(&mut e, "Values", "ArrayProperty", 4 + 8);
    name(&mut e, "IntProperty");
    e.push(0);
    e.extend_from_slice(&2i32.to_le_bytes()); // count
    for value in [1i32, 7] {
        e.extend_from_slice(&value.to_le_bytes());
    }

    // An array of structs writes one inner tag, covering every element, before the elements.
    let point = my_struct(5);
    let mut points = 1i32.to_le_bytes().to_vec();
    head(&mut points, "Points", "StructProperty", point.len());
    name(&mut points, "MyStruct");
    points.extend_from_slice(&[0; 16]);
    points.push(0);
    points.extend_from_slice(&point);
    head(&mut e, "Points", "ArrayProperty", points.len());
    name(&mut e, "StructProperty");
    e.push(0);
    e.extend_from_slice(&points);

    head(&mut e, "Mode", "EnumProperty", 8);
    name(&mut e, "EMode");
    e.push(0);
    name(&mut e, "EMode::A");

    name(&mut e, "None");
    e.extend_from_slice(&0i32.to_le_bytes());

    let mut summary = FLegacyPackageFileSummary {
        package_name: "/Game/TestPackage".to_string(),
        ..Default::default()
    };
    summary.versioning_info.package_file_version = FALLBACK_ENGINE_VERSION.package_file_version();
    summary.versioning_info.total_header_size = HEADER_SIZE as i32;
    summary.package_flags = EPackageFlags::Cooked as u32;
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

fn parse(asset: &[u8], exports: &[u8]) -> ParsedPackage {
    let parsed = parse_package(&AssetBundle { asset, exports }, None).expect("parses");
    assert!(
        matches!(parsed.exports[0].status, ExportStatus::Complete),
        "every byte accounted for, got {:?}",
        parsed.exports[0].status
    );
    parsed
}

fn find<'a>(entries: &'a [PropertyEntry], name: &str) -> &'a PropertyEntry {
    entries
        .iter()
        .find(|e| e.name == name)
        .unwrap_or_else(|| panic!("no {name}"))
}

fn edit_of(entry: &PropertyEntry, op: EditOp) -> ValueEdit {
    ValueEdit {
        offset: entry.span.expect("span").0,
        expect_name: entry.name.clone(),
        expect_element: entry.element,
        expect_kind: kind_of(&entry.value),
        op,
    }
}

fn set(text: &str) -> EditOp {
    EditOp::Set { text: text.into() }
}

/// Applies `edits` to the fixture and reads the result, which must still read to its end.
fn apply(edits: impl FnOnce(&ParsedPackage) -> Vec<ValueEdit>) -> ParsedPackage {
    let (asset, exports) = tagged_package();
    apply_to(&asset, &exports, edits).0
}

/// The same on any bytes, checked the way a save checks itself, and handing the bytes back so a
/// second edit can follow.
fn apply_to(
    asset: &[u8],
    exports: &[u8],
    edits: impl FnOnce(&ParsedPackage) -> Vec<ValueEdit>,
) -> (ParsedPackage, Vec<u8>, Vec<u8>) {
    let before = parse(asset, exports);
    let changes = PackageEdits {
        values: edits(&before),
        ..Default::default()
    };
    let patched =
        patch_package(&AssetBundle { asset, exports }, &before, &changes, None).expect("patch");
    let after = parse(&patched.asset, &patched.exports);
    verify_patch(&before, &after, &changes, &patched.applied).expect("verifies");
    (after, patched.asset, patched.exports)
}

fn refused(edits: impl FnOnce(&ParsedPackage) -> Vec<ValueEdit>) -> String {
    let (asset, exports) = tagged_package();
    let before = parse(&asset, &exports);
    patch_package(
        &AssetBundle {
            asset: &asset,
            exports: &exports,
        },
        &before,
        &PackageEdits {
            values: edits(&before),
            ..Default::default()
        },
        None,
    )
    .err()
    .expect("refused")
}

fn top(parsed: &ParsedPackage) -> &[PropertyEntry] {
    &parsed.exports[0].properties
}

#[test]
fn the_fixture_reads_every_property() {
    let (asset, exports) = tagged_package();
    let parsed = parse(&asset, &exports);
    let names: Vec<&str> = top(&parsed).iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "Damage", "Label", "Enabled", "Tag", "Title", "Pos", "Values", "Points", "Mode"
        ]
    );
}

#[test]
fn an_int_changes_in_place() {
    let after = apply(|p| vec![edit_of(find(top(p), "Damage"), set("250"))]);
    assert!(matches!(
        find(top(&after), "Damage").value,
        PropertyValue::Int { value: 250 }
    ));
    assert!(matches!(
        find(top(&after), "Enabled").value,
        PropertyValue::Bool { value: true }
    ));
}

#[test]
fn a_string_that_grows_or_shrinks_moves_its_tag_size() {
    for text in ["a much longer label than before", ""] {
        let after = apply(|p| vec![edit_of(find(top(p), "Label"), set(text))]);
        match &find(top(&after), "Label").value {
            PropertyValue::Str { value } => assert_eq!(value, text),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            find(top(&after), "Tag").value,
            PropertyValue::Name { .. }
        ));
    }
}

#[test]
fn a_bool_is_rewritten_in_its_tag() {
    let after = apply(|p| vec![edit_of(find(top(p), "Enabled"), set("false"))]);
    assert!(matches!(
        find(top(&after), "Enabled").value,
        PropertyValue::Bool { value: false }
    ));
    assert!(matches!(
        find(top(&after), "Damage").value,
        PropertyValue::Int { value: 99 }
    ));
}

#[test]
fn a_name_the_package_lacks_is_added_to_its_name_map() {
    let after = apply(|p| vec![edit_of(find(top(p), "Tag"), set("Brand_New"))]);
    match &find(top(&after), "Tag").value {
        PropertyValue::Name { value } => assert_eq!(value, "Brand_New"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_text_changes_its_string() {
    let after = apply(|p| vec![edit_of(find(top(p), "Title"), set("Goodbye, world"))]);
    match &find(top(&after), "Title").value {
        PropertyValue::Text { value, .. } => assert_eq!(value.as_deref(), Some("Goodbye, world")),
        other => panic!("{other:?}"),
    }
}

fn fields_of(entry: &PropertyEntry) -> &[PropertyEntry] {
    match &entry.value {
        PropertyValue::Struct { fields, .. } => fields,
        other => panic!("not a struct: {other:?}"),
    }
}

#[test]
fn a_field_inside_a_struct_moves_both_tag_sizes() {
    let after = apply(|p| {
        let x = find(fields_of(find(top(p), "Pos")), "X");
        vec![edit_of(x, set("42"))]
    });
    let x = find(fields_of(find(top(&after), "Pos")), "X");
    assert!(matches!(x.value, PropertyValue::Int { value: 42 }));
}

fn items_of(entry: &PropertyEntry) -> Vec<&PropertyValue> {
    match &entry.value {
        PropertyValue::Array { items, .. } => items.iter().collect(),
        other => panic!("not an array: {other:?}"),
    }
}

#[test]
fn an_array_grows_shrinks_and_changes_an_element() {
    let after = apply(|p| {
        vec![edit_of(
            find(top(p), "Values"),
            EditOp::Insert {
                index: 1,
                key: None,
            },
        )]
    });
    assert_eq!(items_of(find(top(&after), "Values")).len(), 3);

    let after = apply(|p| vec![edit_of(find(top(p), "Values"), EditOp::Remove { index: 0 })]);
    assert_eq!(items_of(find(top(&after), "Values")).len(), 1);

    let after = apply(|p| {
        vec![edit_of(
            find(top(p), "Values"),
            EditOp::SetElement {
                index: 1,
                text: "13".into(),
            },
        )]
    });
    assert!(matches!(
        items_of(find(top(&after), "Values"))[1],
        PropertyValue::Int { value: 13 }
    ));
}

#[test]
fn an_array_of_structs_grows_and_its_elements_edit() {
    let after = apply(|p| {
        vec![edit_of(
            find(top(p), "Points"),
            EditOp::Insert {
                index: 1,
                key: None,
            },
        )]
    });
    assert_eq!(items_of(find(top(&after), "Points")).len(), 2);
    assert!(matches!(
        find(top(&after), "Mode").value,
        PropertyValue::Enum { .. }
    ));
}

#[test]
fn an_enum_takes_another_enumerator() {
    let after = apply(|p| vec![edit_of(find(top(p), "Mode"), set("EMode::B"))]);
    match &find(top(&after), "Mode").value {
        PropertyValue::Enum { name, .. } => assert_eq!(name.as_deref(), Some("EMode::B")),
        other => panic!("{other:?}"),
    }
}

#[test]
fn clearing_or_unsetting_a_tagged_property_is_refused_plainly() {
    for op in [EditOp::Clear, EditOp::Unset] {
        let err = refused(|p| vec![edit_of(find(top(p), "Damage"), op.clone())]);
        assert!(err.contains("tagged"), "{err}");
    }
}

/// An emptied struct array has no element to copy, so a new one is the empty tagged block.
#[test]
fn an_emptied_array_of_structs_grows_by_an_empty_struct() {
    let (asset, exports) = tagged_package();
    let (emptied, asset, exports) = apply_to(&asset, &exports, |p| {
        vec![edit_of(find(top(p), "Points"), EditOp::Remove { index: 0 })]
    });
    assert!(items_of(find(top(&emptied), "Points")).is_empty());
    let (grown, ..) = apply_to(&asset, &exports, |p| {
        vec![edit_of(
            find(top(p), "Points"),
            EditOp::Insert {
                index: 0,
                key: None,
            },
        )]
    });
    let items = items_of(find(top(&grown), "Points"));
    assert_eq!(items.len(), 1);
    assert!(matches!(items[0], PropertyValue::Struct { fields, .. } if fields.is_empty()));
}
