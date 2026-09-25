//! Edits applied to a package cooked with tagged properties, read back, and compared.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use crate::edit::{EditOp, PackageEdits, ValueEdit, kind_of, patch_package, verify_patch};
use crate::mappings::Mappings;
use crate::package::{AssetBundle, ExportStatus, ParsedPackage, parse_package};
use crate::tagged_fixture::{
    head, int, my_struct, name, package_of, sparse_mappings, sparse_package, string,
};
use crate::value::{PropertyEntry, PropertyValue};

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
    package_of(e)
}

fn parse(asset: &[u8], exports: &[u8]) -> ParsedPackage {
    parse_with(asset, exports, None)
}

fn parse_with(asset: &[u8], exports: &[u8], mappings: Option<&Mappings>) -> ParsedPackage {
    let parsed = parse_package(&AssetBundle { asset, exports }, mappings).expect("parses");
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

/// Storing a tagged property that is already there is refused plainly; so is clearing one, which
/// has no zero flag to set.
#[test]
fn storing_a_held_tagged_property_or_clearing_one_is_refused_plainly() {
    let err = refused(|p| vec![edit_of(find(top(p), "Damage"), EditOp::Store)]);
    assert!(err.contains("already stored"), "{err}");
    let err = refused(|p| vec![edit_of(find(top(p), "Damage"), EditOp::Clear)]);
    assert!(err.contains("tagged"), "{err}");
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

/// A struct `MyHolder` whose map and set hold `MyStruct` elements, which a tag alone names only as
/// structs.
fn holder_package() -> (Vec<u8>, Vec<u8>) {
    let mut pairs = 0i32.to_le_bytes().to_vec();
    pairs.extend_from_slice(&1i32.to_le_bytes());
    name(&mut pairs, "Foo");
    pairs.extend_from_slice(&my_struct(5));
    let mut structs = 0i32.to_le_bytes().to_vec();
    structs.extend_from_slice(&1i32.to_le_bytes());
    structs.extend_from_slice(&my_struct(7));

    let mut holder = Vec::new();
    head(&mut holder, "Pairs", "MapProperty", pairs.len());
    name(&mut holder, "NameProperty");
    name(&mut holder, "StructProperty");
    holder.push(0);
    holder.extend_from_slice(&pairs);
    head(&mut holder, "Structs", "SetProperty", structs.len());
    name(&mut holder, "StructProperty");
    holder.push(0);
    holder.extend_from_slice(&structs);
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

fn holder_mappings() -> Mappings {
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
                property(
                    "Pairs",
                    0,
                    PropertyInner::Map {
                        key: Box::new(PropertyInner::Name),
                        value: Box::new(my_struct()),
                    },
                ),
                property(
                    "Structs",
                    1,
                    PropertyInner::Set {
                        key: Box::new(my_struct()),
                    },
                ),
            ],
        },
        Struct {
            name: "MyStruct".into(),
            super_struct: None,
            properties: vec![property("X", 0, PropertyInner::Int)],
        },
    ])
}

fn holder_fields(parsed: &ParsedPackage) -> &[PropertyEntry] {
    match &find(top(parsed), "Holder").value {
        PropertyValue::Struct { fields, .. } => fields,
        other => panic!("{other:?}"),
    }
}

/// With the owner's schema to say which struct a tag's elements are, a map of structs and a set
/// of them take field edits, new elements and removals like any other container.
#[test]
fn tagged_containers_of_structs_edit_through_the_owners_schema() {
    let (asset, exports) = holder_package();
    let mappings = holder_mappings();
    let before = parse_with(&asset, &exports, Some(&mappings));
    let pairs = find(holder_fields(&before), "Pairs");
    let structs = find(holder_fields(&before), "Structs");
    let x = match &pairs.value {
        PropertyValue::Map { entries } => match &entries[0].value {
            PropertyValue::Struct { fields, .. } => find(fields, "X").clone(),
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    };
    let changes = PackageEdits {
        values: vec![
            edit_of(&x, set("6")),
            edit_of(
                pairs,
                EditOp::Insert {
                    index: 1,
                    key: Some("Tag".into()),
                },
            ),
            edit_of(structs, EditOp::Remove { index: 0 }),
        ],
        ..Default::default()
    };
    let bundle = AssetBundle {
        asset: &asset,
        exports: &exports,
    };
    let patched = patch_package(&bundle, &before, &changes, Some(&mappings)).expect("patch");
    let after = parse_with(&patched.asset, &patched.exports, Some(&mappings));
    verify_patch(&before, &after, &changes, &patched.applied).expect("verifies");

    match &find(holder_fields(&after), "Pairs").value {
        PropertyValue::Map { entries } => {
            assert_eq!(entries.len(), 2);
            assert_eq!(entries[1].key.summary(), "Tag");
            let PropertyValue::Struct { fields, .. } = &entries[0].value else {
                panic!("{:?}", entries[0].value);
            };
            assert!(matches!(
                find(fields, "X").value,
                PropertyValue::Int { value: 6 }
            ));
            assert!(
                matches!(&entries[1].value, PropertyValue::Struct { fields, .. } if fields.is_empty()),
                "a new value is the empty tagged block"
            );
        }
        other => panic!("{other:?}"),
    }
    assert!(items_of(find(holder_fields(&after), "Structs")).is_empty());
}

/// Without a schema the tag cannot say which struct the elements are, so they stay undecoded
/// and the rest of the package still reads.
#[test]
fn tagged_containers_of_structs_stay_undecoded_without_a_schema() {
    let (asset, exports) = holder_package();
    let parsed = parse(&asset, &exports);
    let pairs = find(holder_fields(&parsed), "Pairs");
    assert!(
        matches!(&pairs.value, PropertyValue::Struct { fields, .. } if fields[0].name == "(undecoded)"),
        "{:?}",
        pairs.value
    );
}

/// The editor's reading: with the schema, what a block lacks is listed as not stored.
fn parse_declared(asset: &[u8], exports: &[u8], mappings: Option<&Mappings>) -> ParsedPackage {
    let parsed = crate::package::parse_package_opts(
        &AssetBundle { asset, exports },
        mappings,
        None,
        crate::package::ParseOptions {
            declared_slots: true,
            ..Default::default()
        },
    )
    .expect("parses");
    assert!(
        matches!(parsed.exports[0].status, ExportStatus::Complete),
        "every byte accounted for, got {:?}",
        parsed.exports[0].status
    );
    parsed
}

/// Patches, reads back the way a save does, and checks the result.
fn apply_declared(
    asset: &[u8],
    exports: &[u8],
    mappings: Option<&Mappings>,
    edits: impl FnOnce(&ParsedPackage) -> Vec<ValueEdit>,
) -> Result<(ParsedPackage, Vec<u8>, Vec<u8>), String> {
    let before = parse_declared(asset, exports, mappings);
    let changes = PackageEdits {
        values: edits(&before),
        ..Default::default()
    };
    let patched = patch_package(&AssetBundle { asset, exports }, &before, &changes, mappings)?;
    let after = parse_declared(&patched.asset, &patched.exports, mappings);
    verify_patch(&before, &after, &changes, &patched.applied)?;
    Ok((after, patched.asset, patched.exports))
}

fn holder(parsed: &ParsedPackage) -> &[PropertyEntry] {
    match &find(top(parsed), "Holder").value {
        PropertyValue::Struct { fields, .. } => fields,
        other => panic!("{other:?}"),
    }
}

fn is_absent(entry: &PropertyEntry) -> bool {
    matches!(entry.value, PropertyValue::Unset { .. })
}

/// What the schema declares and the block lacks is listed at the block's `None`, nested blocks
/// included; without a schema nothing is listed.
#[test]
fn a_tagged_block_lists_what_its_schema_declares_and_it_lacks() {
    let (asset, exports) = sparse_package();
    let mappings = sparse_mappings();
    let parsed = parse_declared(&asset, &exports, Some(&mappings));
    let absent: Vec<&str> = holder(&parsed)
        .iter()
        .filter(|entry| is_absent(entry))
        .map(|entry| entry.name.as_str())
        .collect();
    assert_eq!(
        absent,
        [
            "Extra", "Label", "On", "Mode", "Nested", "Items", "Scores", "Counts"
        ]
    );
    let inner = find(holder(&parsed), "Inner");
    assert!(is_absent(find(fields_of(inner), "Y")));

    let bare = parse_declared(&asset, &exports, None);
    assert!(!holder(&bare).iter().any(is_absent));
}

/// A value typed for an absent property adds its tag, in its block and in a nested one, in one
/// save; the blocks and the tags around them grow to hold them.
#[test]
fn values_typed_for_absent_tagged_properties_add_their_tags() {
    let (asset, exports) = sparse_package();
    let mappings = sparse_mappings();
    let (after, ..) = apply_declared(&asset, &exports, Some(&mappings), |p| {
        let inner = find(holder(p), "Inner");
        vec![
            edit_of(find(holder(p), "Extra"), set("7")),
            edit_of(find(holder(p), "Label"), set("hello")),
            edit_of(find(holder(p), "On"), set("true")),
            edit_of(find(holder(p), "Mode"), set("EMode::B")),
            edit_of(find(fields_of(inner), "Y"), set("4")),
        ]
    })
    .expect("added");
    let got = |name: &str| find(holder(&after), name).value.summary();
    assert_eq!(got("Extra"), "7");
    assert_eq!(got("Label"), "hello");
    assert_eq!(got("On"), "true");
    assert_eq!(got("Mode"), "EMode::B");
    assert_eq!(
        find(fields_of(find(holder(&after), "Inner")), "Y")
            .value
            .summary(),
        "4"
    );
    assert_eq!(got("Count"), "3", "what was there reads as it did");
}

/// Storing an absent struct, array or map adds its tag holding the empty form, which reads back
/// empty.
#[test]
fn absent_tagged_structs_and_containers_store_empty() {
    let (asset, exports) = sparse_package();
    let mappings = sparse_mappings();
    let (after, ..) = apply_declared(&asset, &exports, Some(&mappings), |p| {
        ["Nested", "Items", "Scores"]
            .iter()
            .map(|name| edit_of(find(holder(p), name), EditOp::Store))
            .collect()
    })
    .expect("stored");
    let nested = find(holder(&after), "Nested");
    assert!(
        fields_of(nested).iter().all(is_absent),
        "{:?}",
        nested.value
    );
    assert!(items_of(find(holder(&after), "Items")).is_empty());
    assert!(matches!(
        &find(holder(&after), "Scores").value,
        PropertyValue::Map { entries } if entries.is_empty()
    ));
}

/// Unsetting a stored tagged property takes its tag out, a bool and a struct included, with or
/// without a schema to list it afterwards.
#[test]
fn unsetting_a_tagged_property_takes_its_tag_out() {
    let (asset, exports) = sparse_package();
    let mappings = sparse_mappings();
    let take_out = |p: &ParsedPackage| -> Vec<ValueEdit> {
        ["Count", "Flag", "Inner"]
            .iter()
            .map(|name| edit_of(find(holder(p), name), EditOp::Unset))
            .collect()
    };
    let (after, ..) = apply_declared(&asset, &exports, Some(&mappings), take_out).expect("out");
    for name in ["Count", "Flag", "Inner"] {
        assert!(is_absent(find(holder(&after), name)), "{name}");
    }
    let (bare, ..) = apply_declared(&asset, &exports, None, take_out).expect("out");
    assert!(holder(&bare).is_empty(), "{:?}", holder(&bare));
}

/// A tag can go in and another come out of the same block in one save.
#[test]
fn a_tagged_block_takes_an_addition_and_a_removal_together() {
    let (asset, exports) = sparse_package();
    let mappings = sparse_mappings();
    let (after, ..) = apply_declared(&asset, &exports, Some(&mappings), |p| {
        vec![
            edit_of(find(holder(p), "Extra"), set("1")),
            edit_of(find(holder(p), "Count"), EditOp::Unset),
        ]
    })
    .expect("both");
    assert_eq!(find(holder(&after), "Extra").value.summary(), "1");
    assert!(is_absent(find(holder(&after), "Count")));
}

/// A tagged value has no zero flag, so clearing one is refused in words that say what to do.
#[test]
fn clearing_a_tagged_property_says_what_to_do_instead() {
    let (asset, exports) = sparse_package();
    let mappings = sparse_mappings();
    let Err(err) = apply_declared(&asset, &exports, Some(&mappings), |p| {
        vec![edit_of(find(holder(p), "Count"), EditOp::Clear)]
    }) else {
        panic!("refused");
    };
    assert!(err.contains("unset it"), "{err}");
}
