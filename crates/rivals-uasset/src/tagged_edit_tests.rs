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

    let mut words = 2i32.to_le_bytes().to_vec();
    string(&mut words, "a");
    string(&mut words, "bb");
    head(&mut e, "Words", "ArrayProperty", words.len());
    name(&mut e, "StrProperty");
    e.push(0);
    e.extend_from_slice(&words);

    // A set of names, a map from name to int, and arrays of bools and of enums: each element is
    // written the way the property writes it inside a container.
    let mut names = 0i32.to_le_bytes().to_vec();
    names.extend_from_slice(&2i32.to_le_bytes());
    name(&mut names, "Foo");
    name(&mut names, "Label");
    head(&mut e, "Names", "SetProperty", names.len());
    name(&mut e, "NameProperty");
    e.push(0);
    e.extend_from_slice(&names);

    let mut scores = 0i32.to_le_bytes().to_vec();
    scores.extend_from_slice(&1i32.to_le_bytes());
    name(&mut scores, "Foo");
    scores.extend_from_slice(&5i32.to_le_bytes());
    head(&mut e, "Scores", "MapProperty", scores.len());
    name(&mut e, "NameProperty");
    name(&mut e, "IntProperty");
    e.push(0);
    e.extend_from_slice(&scores);

    let flags = [2, 0, 0, 0, 1, 0];
    head(&mut e, "Flags", "ArrayProperty", flags.len());
    name(&mut e, "BoolProperty");
    e.push(0);
    e.extend_from_slice(&flags);

    let mut modes = 1i32.to_le_bytes().to_vec();
    name(&mut modes, "EMode::A");
    head(&mut e, "Modes", "ArrayProperty", modes.len());
    name(&mut e, "EnumProperty");
    e.push(0);
    e.extend_from_slice(&modes);

    // Three native structs whose one value is not written the way it reads.
    let native = |e: &mut Vec<u8>, property: &str, kind: &str, value: &[u8]| {
        head(e, property, "StructProperty", value.len());
        name(e, kind);
        e.extend_from_slice(&[0; 16]);
        e.push(0);
        e.extend_from_slice(value);
    };
    let id: Vec<u8> = (1u32..=4).flat_map(u32::to_le_bytes).collect();
    native(&mut e, "Id", "Guid", &id);
    let mut asset_path = Vec::new();
    name(&mut asset_path, "/Game/A");
    name(&mut asset_path, "B");
    native(&mut e, "Asset", "TopLevelAssetPath", &asset_path);
    let mut path = Vec::new();
    string(&mut path, "/Game/A.B");
    native(&mut e, "Path", "MarvelSoftObjectPath", &path);

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
            "Damage", "Label", "Enabled", "Tag", "Title", "Pos", "Values", "Points", "Mode",
            "Words", "Names", "Scores", "Flags", "Modes", "Id", "Asset", "Path"
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
        PropertyValue::Array { items, .. } | PropertyValue::Set { items } => items.iter().collect(),
        other => panic!("not an array or a set: {other:?}"),
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

/// A guid is sixteen raw bytes, though it reads as its hex digits, and any spelling of it is taken.
#[test]
fn a_guid_is_written_as_its_bytes() {
    let wanted = "0000000A-0000000B-0000000C-0000000D".to_lowercase();
    let after = apply(|p| vec![edit_of(find(top(p), "Id"), set(&wanted))]);
    match &find(top(&after), "Id").value {
        PropertyValue::Str { value } => assert_eq!(value, "0000000A0000000B0000000C0000000D"),
        other => panic!("{other:?}"),
    }
    let err = refused(|p| vec![edit_of(find(top(p), "Id"), set("not a guid"))]);
    assert!(err.contains("32 hex digits"), "{err}");
}

/// A top level asset path is two names and nothing after them.
#[test]
fn a_top_level_asset_path_is_written_as_two_names() {
    let after = apply(|p| vec![edit_of(find(top(p), "Asset"), set("/Game/Other.Thing"))]);
    match &find(top(&after), "Asset").value {
        PropertyValue::SoftObject { path } => assert_eq!(path, "/Game/Other.Thing"),
        other => panic!("{other:?}"),
    }
    let err = refused(|p| vec![edit_of(find(top(p), "Asset"), set("/Game/A.B:Sub"))]);
    assert!(err.contains("subobject"), "{err}");
}

/// The game's soft path wrapper writes the whole path as one string.
#[test]
fn a_marvel_soft_path_is_written_as_one_string() {
    let after = apply(|p| {
        vec![edit_of(
            find(top(p), "Path"),
            set("/Game/Longer/Path/Here.Here"),
        )]
    });
    match &find(top(&after), "Path").value {
        PropertyValue::SoftObject { path } => assert_eq!(path, "/Game/Longer/Path/Here.Here"),
        other => panic!("{other:?}"),
    }
}

/// What an edit was written against is taken from the package it was made on, and applying it once
/// the package reads differently is refused, unless the caller says to apply anyway.
#[test]
fn edits_on_a_changed_package_are_refused_as_drift() {
    let (asset, exports) = tagged_package();
    let before = parse(&asset, &exports);
    let damage = find(top(&before), "Damage").clone();
    let mut edits = PackageEdits {
        values: vec![edit_of(&damage, set("5"))],
        reset_exports: Vec::new(),
        ..Default::default()
    };
    edits.expect = crate::edit::expectations(&before, &edits);
    assert_eq!(
        edits
            .expect
            .values
            .get(&damage.span.expect("span").0.to_string()),
        Some(&"99".to_string())
    );
    crate::edit::check_expectations(&before, &edits).expect("unchanged");

    // A later build stored another value there.
    let (changed, asset, exports) = apply_to(&asset, &exports, |p| {
        vec![edit_of(find(top(p), "Damage"), set("98"))]
    });
    let err = crate::edit::check_expectations(&changed, &edits).expect_err("refused");
    assert!(err.starts_with(crate::edit::DRIFT), "{err}");
    assert!(err.contains("Damage was 99, and is now 98"), "{err}");

    // An export index that now names another object is caught the same way.
    let mut moved = edits.clone();
    moved.expect.values.clear();
    moved.expect.exports.insert(0, "/Game/Other.Thing".into());
    let err = crate::edit::check_expectations(&changed, &moved).expect_err("refused");
    assert!(err.contains("export 0 was /Game/Other.Thing"), "{err}");

    edits.allow_drift = true;
    patch_package(
        &AssetBundle {
            asset: &asset,
            exports: &exports,
        },
        &changed,
        &edits,
        None,
    )
    .expect("applied anyway");
}

/// Several elements go in and out of one container in a single save, each addressed by its index
/// as read, and the count moves once by the net change.
#[test]
fn a_container_takes_several_inserts_and_removals_in_one_save() {
    let after = apply(|p| {
        let values = find(top(p), "Values");
        vec![
            edit_of(
                values,
                EditOp::Insert {
                    index: 0,
                    key: None,
                },
            ),
            edit_of(
                values,
                EditOp::Insert {
                    index: 2,
                    key: None,
                },
            ),
            edit_of(values, EditOp::Remove { index: 1 }),
        ]
    });
    let items: Vec<i64> = items_of(find(top(&after), "Values"))
        .iter()
        .map(|item| match item {
            PropertyValue::Int { value } => *value,
            other => panic!("{other:?}"),
        })
        .collect();
    // [1, 7]: a copy of 1 before it, 7 dropped, a copy of 7 appended.
    assert_eq!(items, vec![1, 1, 7]);
}

/// Two elements of one container change width in the same save; each is found where it ends up.
#[test]
fn two_elements_of_one_container_change_width_together() {
    let after = apply(|p| {
        let words = find(top(p), "Words");
        vec![
            edit_of(
                words,
                EditOp::SetElement {
                    index: 0,
                    text: "a good deal longer".into(),
                },
            ),
            edit_of(
                words,
                EditOp::SetElement {
                    index: 1,
                    text: "x".into(),
                },
            ),
        ]
    });
    let items = items_of(find(top(&after), "Words"));
    assert!(
        matches!(&items[..], [PropertyValue::Str { value: a }, PropertyValue::Str { value: b }]
            if a == "a good deal longer" && b == "x"),
        "{items:?}"
    );
}

/// An element is removed once, and one being removed takes no new value.
#[test]
fn a_removal_twice_or_a_value_for_a_removed_element_is_refused() {
    let err = refused(|p| {
        let values = find(top(p), "Values");
        vec![
            edit_of(values, EditOp::Remove { index: 0 }),
            edit_of(values, EditOp::Remove { index: 0 }),
        ]
    });
    assert!(err.contains("removed twice"), "{err}");
    let err = refused(|p| {
        let values = find(top(p), "Values");
        vec![
            edit_of(values, EditOp::Remove { index: 1 }),
            edit_of(
                values,
                EditOp::SetElement {
                    index: 1,
                    text: "3".into(),
                },
            ),
        ]
    });
    assert!(err.contains("both removed and given a value"), "{err}");
}

/// A tagged set takes a new key, loses one and changes another, all in one save.
#[test]
fn a_tagged_set_takes_keys_and_loses_them() {
    let after = apply(|p| {
        let names = find(top(p), "Names");
        vec![
            edit_of(
                names,
                EditOp::Insert {
                    index: 2,
                    key: Some("Tag".into()),
                },
            ),
            edit_of(names, EditOp::Remove { index: 0 }),
            edit_of(
                names,
                EditOp::SetElement {
                    index: 1,
                    text: "Title".into(),
                },
            ),
        ]
    });
    let items: Vec<String> = items_of(find(top(&after), "Names"))
        .iter()
        .map(|item| item.summary())
        .collect();
    assert_eq!(items, vec!["Title", "Tag"]);
}

/// A tagged map's value changes in place, and a pair goes in under a new key.
#[test]
fn a_tagged_map_changes_a_value_and_takes_a_pair() {
    let after = apply(|p| {
        let scores = find(top(p), "Scores");
        vec![
            edit_of(
                scores,
                EditOp::SetElement {
                    index: 0,
                    text: "9".into(),
                },
            ),
            edit_of(
                scores,
                EditOp::Insert {
                    index: 1,
                    key: Some("Tag".into()),
                },
            ),
        ]
    });
    match &find(top(&after), "Scores").value {
        PropertyValue::Map { entries } => {
            let pairs: Vec<(String, String)> = entries
                .iter()
                .map(|pair| (pair.key.summary(), pair.value.summary()))
                .collect();
            assert_eq!(
                pairs,
                vec![("Foo".into(), "9".into()), ("Tag".into(), "0".into())]
            );
        }
        other => panic!("{other:?}"),
    }
}

/// Bool and enum elements are a byte and a name each, and edit as such.
#[test]
fn tagged_bool_and_enum_arrays_edit_their_elements() {
    let after = apply(|p| {
        vec![
            edit_of(
                find(top(p), "Flags"),
                EditOp::SetElement {
                    index: 1,
                    text: "true".into(),
                },
            ),
            edit_of(
                find(top(p), "Modes"),
                EditOp::SetElement {
                    index: 0,
                    text: "EMode::B".into(),
                },
            ),
            edit_of(
                find(top(p), "Modes"),
                EditOp::Insert {
                    index: 1,
                    key: None,
                },
            ),
        ]
    });
    let flags: Vec<String> = items_of(find(top(&after), "Flags"))
        .iter()
        .map(|item| item.summary())
        .collect();
    assert_eq!(flags, vec!["true", "true"]);
    let modes: Vec<String> = items_of(find(top(&after), "Modes"))
        .iter()
        .map(|item| item.summary())
        .collect();
    assert_eq!(modes, vec!["EMode::B", "EMode::A"]);
}
