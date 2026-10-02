//! Edits applied to a package cooked with unversioned properties, read back, and compared.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use crate::edit::{
    EditOp, PackageEdits, ValueEdit, check_expectations, expectations, kind_of, patch_package,
    verify_patch,
};
use crate::package::{AssetBundle, ExportStatus, ParseOptions, ParsedPackage, parse_package_opts};
use crate::props::TYPE_FIELD;
use crate::unversioned_fixture::{
    HELPER_PATH, POINT_PATH, TEST_CLASS_PATH, unversioned_mappings, unversioned_package,
};
use crate::value::{PropertyEntry, PropertyValue};

/// The editor's own parse, which lists every slot the header skips.
fn parse(asset: &[u8], exports: &[u8]) -> ParsedPackage {
    let mappings = unversioned_mappings();
    let parsed = parse_package_opts(
        &AssetBundle { asset, exports },
        Some(&mappings),
        None,
        ParseOptions {
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

fn fixture() -> (ParsedPackage, Vec<u8>, Vec<u8>) {
    let (asset, exports) = unversioned_package();
    (parse(&asset, &exports), asset, exports)
}

fn find<'a>(entries: &'a [PropertyEntry], name: &str) -> &'a PropertyEntry {
    entries
        .iter()
        .find(|e| e.name == name)
        .unwrap_or_else(|| panic!("no {name}"))
}

fn top(parsed: &ParsedPackage) -> &[PropertyEntry] {
    &parsed.exports[0].properties
}

fn value<'a>(parsed: &'a ParsedPackage, name: &str) -> &'a PropertyValue {
    &find(top(parsed), name).value
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

/// Applies `edits` to `asset`, checked the way a save checks itself, and hands the bytes back so a
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
    let mappings = unversioned_mappings();
    let patched = patch_package(
        &AssetBundle { asset, exports },
        &before,
        &changes,
        Some(&mappings),
    )
    .expect("patch");
    let after = parse(&patched.asset, &patched.exports);
    verify_patch(&before, &after, &changes, &patched.applied).expect("verifies");
    (after, patched.asset, patched.exports)
}

fn apply(edits: impl FnOnce(&ParsedPackage) -> Vec<ValueEdit>) -> ParsedPackage {
    let (asset, exports) = unversioned_package();
    apply_to(&asset, &exports, edits).0
}

/// Why `edits` is refused against the fixture.
fn refused(edits: impl FnOnce(&ParsedPackage) -> Vec<ValueEdit>) -> String {
    let (before, asset, exports) = fixture();
    let mappings = unversioned_mappings();
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
        Some(&mappings),
    )
    .err()
    .expect("refused")
}

/// The import the package holds at `path`, which the export waits on before it serializes.
fn waited_on(asset: &[u8], exports: &[u8], parsed: &ParsedPackage, path: &str) -> bool {
    let import = parsed
        .imports
        .iter()
        .find(|import| import.path == path)
        .unwrap_or_else(|| panic!("{path} is imported"));
    let header = crate::read_header(&AssetBundle { asset, exports }).expect("header");
    crate::runs_of(&header).expect("runs")[0]
        .create_before_serialize
        .contains(&import.index)
}

#[test]
fn the_fixture_reads_every_slot() {
    let (parsed, ..) = fixture();
    assert_eq!(
        value(&parsed, "OnFired").summary(),
        format!("{HELPER_PATH}::Handler")
    );
    assert_eq!(value(&parsed, "Count").summary(), "7");
    assert!(matches!(
        value(&parsed, "Spare"),
        PropertyValue::Unset {
            declared: "Delegate",
            ..
        }
    ));
}

/// A multicast delegate's bindings are a list like an array's, laid out so one can be added or
/// dropped.
#[test]
fn a_multicast_delegate_records_where_each_binding_sits() {
    let (parsed, ..) = fixture();
    let entry = find(top(&parsed), "OnChanged");
    let layout = parsed
        .containers
        .iter()
        .find(|layout| layout.at == entry.span.unwrap().0)
        .expect("a layout at the list");
    assert_eq!(layout.elements.len(), 1);
    assert_eq!(layout.element_kind, "Delegate");
    assert_eq!(
        entry.value.summary(),
        "[1 items]",
        "the bindings read as a list"
    );
}

#[test]
fn a_field_path_keeps_its_owner() {
    let (parsed, ..) = fixture();
    assert!(matches!(
        value(&parsed, "Watched"),
        PropertyValue::FieldPath { path, owner: Some(owner) }
            if path == "Count" && owner == TEST_CLASS_PATH
    ));
}

#[test]
fn a_zeroed_lazy_object_reads_as_an_empty_guid() {
    let (parsed, ..) = fixture();
    assert!(matches!(
        value(&parsed, "ZeroLazy"),
        PropertyValue::LazyObject { guid } if guid == &"0".repeat(32)
    ));
}

/// A delegate bound to an object the package does not import yet imports it, with the class the
/// object it replaces had, and waits on it; the edit holds the delegate to what it read.
#[test]
fn a_delegate_is_bound_to_another_object_and_waits_on_it() {
    const OTHER: &str = "/Game/Others.Other";
    let (before, asset, exports) = fixture();
    let changes = PackageEdits {
        values: vec![edit_of(
            find(top(&before), "OnFired"),
            set(&format!("{OTHER}::Run")),
        )],
        ..Default::default()
    };
    let expect = expectations(&before, &changes);
    assert_eq!(
        expect.values.values().collect::<Vec<_>>(),
        [&format!("{HELPER_PATH}::Handler")]
    );
    let (after, asset, exports) = apply_to(&asset, &exports, |_| changes.values.clone());
    assert!(matches!(
        value(&after, "OnFired"),
        PropertyValue::Delegate { object: Some(object), function }
            if object == OTHER && function == "Run"
    ));
    let import = after
        .imports
        .iter()
        .find(|import| import.path == OTHER)
        .expect("imported");
    assert_eq!(import.class_name, "TestClass");
    assert!(waited_on(&asset, &exports, &after, OTHER));
    let drifted = PackageEdits { expect, ..changes };
    assert!(check_expectations(&after, &drifted).is_err());
}

/// `None` alone empties a delegate; `None::Function` names a function bound to no object.
#[test]
fn a_delegate_is_cleared_with_none() {
    let after = apply(|p| vec![edit_of(find(top(p), "OnFired"), set("None"))]);
    assert_eq!(value(&after, "OnFired").summary(), "None");
    let after = apply(|p| vec![edit_of(find(top(p), "OnFired"), set("None::Later"))]);
    assert!(matches!(
        value(&after, "OnFired"),
        PropertyValue::Delegate { object: None, function } if function == "Later"
    ));
}

/// A delegate typed without its object, or with an index for one, is refused rather than guessed.
#[test]
fn a_delegate_names_its_object_by_path() {
    let error = refused(|p| vec![edit_of(find(top(p), "OnFired"), set("Handler"))]);
    assert!(error.contains("Object::Function"), "{error}");
    let error = refused(|p| vec![edit_of(find(top(p), "OnFired"), set("-4::Handler"))]);
    assert!(error.contains("by its path"), "{error}");
}

/// A multicast delegate takes a binding copied from the last, and in a later save a changed one
/// and one fewer.
#[test]
fn a_multicast_delegate_gains_changes_and_loses_bindings() {
    let (asset, exports) = unversioned_package();
    let (grown, asset, exports) = apply_to(&asset, &exports, |p| {
        vec![edit_of(
            find(top(p), "OnChanged"),
            EditOp::Insert {
                index: 1,
                key: None,
            },
        )]
    });
    let PropertyValue::Array { items } = value(&grown, "OnChanged") else {
        panic!("not a list");
    };
    assert_eq!(items.len(), 2);
    assert_eq!(items[1].summary(), format!("{HELPER_PATH}::OnFired"));

    let (after, ..) = apply_to(&asset, &exports, |p| {
        let entry = find(top(p), "OnChanged");
        vec![
            edit_of(
                entry,
                EditOp::SetElement {
                    index: 1,
                    text: format!("{HELPER_PATH}::Handler"),
                },
            ),
            edit_of(entry, EditOp::Remove { index: 0 }),
        ]
    });
    let PropertyValue::Array { items } = value(&after, "OnChanged") else {
        panic!("not a list");
    };
    let bound: Vec<String> = items.iter().map(PropertyValue::summary).collect();
    assert_eq!(bound, [format!("{HELPER_PATH}::Handler")]);
}

/// A multicast delegate nothing stores yet is stored with its first binding, which starts empty
/// and is bound in the next save.
#[test]
fn an_unset_multicast_delegate_grows_its_first_binding() {
    let (asset, exports) = unversioned_package();
    let (grown, asset, exports) = apply_to(&asset, &exports, |p| {
        vec![edit_of(
            find(top(p), "Listeners"),
            EditOp::Insert {
                index: 0,
                key: None,
            },
        )]
    });
    let PropertyValue::Array { items } = value(&grown, "Listeners") else {
        panic!("not a list");
    };
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].summary(), "None");
    let (after, ..) = apply_to(&asset, &exports, |p| {
        vec![edit_of(
            find(top(p), "Listeners"),
            EditOp::SetElement {
                index: 0,
                text: format!("{HELPER_PATH}::Handler"),
            },
        )]
    });
    let PropertyValue::Array { items } = value(&after, "Listeners") else {
        panic!("not a list");
    };
    assert_eq!(items[0].summary(), format!("{HELPER_PATH}::Handler"));
}

/// An unset delegate takes a value typed for it, or its empty form.
#[test]
fn an_unset_delegate_is_typed_or_stored_empty() {
    let typed = apply(|p| {
        vec![edit_of(
            find(top(p), "Spare"),
            set(&format!("{HELPER_PATH}::Handler")),
        )]
    });
    assert_eq!(
        value(&typed, "Spare").summary(),
        format!("{HELPER_PATH}::Handler")
    );
    let stored = apply(|p| vec![edit_of(find(top(p), "Spare"), EditOp::Store)]);
    assert_eq!(value(&stored, "Spare").summary(), "None");
}

#[test]
fn a_field_path_names_another_property_and_owner() {
    let after = apply(|p| {
        vec![edit_of(
            find(top(p), "Watched"),
            set(&format!("Lazy.Inner in {HELPER_PATH}")),
        )]
    });
    assert!(matches!(
        value(&after, "Watched"),
        PropertyValue::FieldPath { path, owner: Some(owner) }
            if path == "Lazy.Inner" && owner == HELPER_PATH
    ));
}

#[test]
fn a_field_path_without_an_owner_keeps_the_one_it_had() {
    let after = apply(|p| vec![edit_of(find(top(p), "Watched"), set("OnFired"))]);
    assert_eq!(
        value(&after, "Watched").summary(),
        format!("OnFired in {TEST_CLASS_PATH}")
    );
    let after = apply(|p| vec![edit_of(find(top(p), "Path"), set("Count"))]);
    assert_eq!(value(&after, "Path").summary(), "Count");
}

#[test]
fn a_lazy_object_takes_a_guid_in_any_spelling() {
    let after = apply(|p| {
        vec![edit_of(
            find(top(p), "Lazy"),
            set("{0000000a-0000-000b-0000-000c0000000d}"),
        )]
    });
    assert_eq!(
        value(&after, "Lazy").summary(),
        "0000000A0000000B0000000C0000000D"
    );
}

fn summaries(value: &PropertyValue) -> Vec<String> {
    match value {
        PropertyValue::Array { items } | PropertyValue::Set { items } => {
            items.iter().map(PropertyValue::summary).collect()
        }
        PropertyValue::Map { entries } => entries
            .iter()
            .map(|pair| format!("{}={}", pair.key.summary(), pair.value.summary()))
            .collect(),
        other => panic!("{other:?} holds no elements"),
    }
}

fn set_key(index: u32, text: &str) -> EditOp {
    EditOp::SetKey {
        index,
        text: text.into(),
    }
}

fn reorder(order: &[u32]) -> EditOp {
    EditOp::Reorder {
        order: order.to_vec(),
    }
}

#[test]
fn a_map_key_changes_in_place() {
    let after = apply(|p| vec![edit_of(find(top(p), "Lookup"), set_key(1, "Fresh"))]);
    assert_eq!(
        summaries(value(&after, "Lookup")),
        ["OnFired=1", "Fresh=2", "Count=3"]
    );
}

/// Two pairs trade keys in one save: each key is free once its own pair takes another.
#[test]
fn two_map_keys_swap_in_one_save() {
    let after = apply(|p| {
        let lookup = find(top(p), "Lookup");
        vec![
            edit_of(lookup, set_key(0, "Handler")),
            edit_of(lookup, set_key(1, "OnFired")),
        ]
    });
    assert_eq!(
        summaries(value(&after, "Lookup")),
        ["Handler=1", "OnFired=2", "Count=3"]
    );
}

#[test]
fn a_key_another_pair_holds_is_refused() {
    let error = refused(|p| vec![edit_of(find(top(p), "Lookup"), set_key(0, "Handler"))]);
    assert!(error.contains("already holds the key Handler"), "{error}");
    let error = refused(|p| {
        let lookup = find(top(p), "Lookup");
        vec![
            edit_of(lookup, set_key(0, "Fresh")),
            edit_of(lookup, set_key(2, "Fresh")),
        ]
    });
    assert!(error.contains("same key twice"), "{error}");
}

/// A pair the save removes takes no key, and its own key is free for another pair to take.
#[test]
fn a_removed_pair_takes_no_new_key() {
    let error = refused(|p| {
        let lookup = find(top(p), "Lookup");
        vec![
            edit_of(lookup, EditOp::Remove { index: 1 }),
            edit_of(lookup, set_key(1, "Fresh")),
        ]
    });
    assert!(error.contains("removed and given a new key"), "{error}");
    let after = apply(|p| {
        let lookup = find(top(p), "Lookup");
        vec![
            edit_of(lookup, EditOp::Remove { index: 1 }),
            edit_of(lookup, set_key(2, "Handler")),
        ]
    });
    assert_eq!(
        summaries(value(&after, "Lookup")),
        ["OnFired=1", "Handler=3"]
    );
}

/// A key edit is held to the key it read, under a drift key of its own.
#[test]
fn a_key_edit_is_held_to_the_key_it_read() {
    let (before, ..) = fixture();
    let lookup = find(top(&before), "Lookup");
    let changes = PackageEdits {
        values: vec![edit_of(lookup, set_key(1, "Fresh"))],
        ..Default::default()
    };
    let expect = expectations(&before, &changes);
    let at = lookup.span.unwrap().0;
    assert_eq!(
        expect
            .values
            .get(&format!("{at}[1].key"))
            .map(String::as_str),
        Some("Handler")
    );
    let after = apply(|_| changes.values.clone());
    assert!(check_expectations(&after, &PackageEdits { expect, ..changes }).is_err());
}

#[test]
fn an_array_is_reordered_in_one_splice() {
    let (before, asset, exports) = fixture();
    let numbers = find(top(&before), "Numbers");
    let changes = PackageEdits {
        values: vec![edit_of(numbers, reorder(&[2, 0, 1]))],
        ..Default::default()
    };
    assert_eq!(
        expectations(&before, &changes)
            .values
            .values()
            .collect::<Vec<_>>(),
        ["[10, 20, 30]"]
    );
    let (after, _, patched) = apply_to(&asset, &exports, |_| changes.values.clone());
    assert_eq!(summaries(value(&after, "Numbers")), ["30", "10", "20"]);
    assert_eq!(patched.len(), exports.len(), "nothing grew or shrank");
}

#[test]
fn a_map_s_pairs_move_with_their_values() {
    let after = apply(|p| vec![edit_of(find(top(p), "Lookup"), reorder(&[2, 1, 0]))]);
    assert_eq!(
        summaries(value(&after, "Lookup")),
        ["Count=3", "Handler=2", "OnFired=1"]
    );
}

/// A reorder writes the container's elements whole, so nothing else may act on them in the same
/// save, and an order has to name each element once.
#[test]
fn a_reorder_rides_alone_in_its_container() {
    let error = refused(|p| {
        let numbers = find(top(p), "Numbers");
        vec![
            edit_of(numbers, reorder(&[2, 0, 1])),
            edit_of(
                numbers,
                EditOp::SetElement {
                    index: 0,
                    text: "5".into(),
                },
            ),
        ]
    });
    assert!(error.contains("a save of its own"), "{error}");
    let error = refused(|p| vec![edit_of(find(top(p), "Numbers"), reorder(&[0, 0, 1]))]);
    assert!(error.contains("twice"), "{error}");
    let error = refused(|p| vec![edit_of(find(top(p), "Numbers"), reorder(&[0, 1]))]);
    assert!(error.contains("names 2"), "{error}");
}

/// The objects a reordered array points at are still waited on: the references moved, and the
/// export still needs them.
#[test]
fn a_reordered_array_of_references_keeps_its_dependencies() {
    const OTHER: &str = "/Game/Others.Other";
    let (asset, exports) = unversioned_package();
    let (pointed, asset, exports) = apply_to(&asset, &exports, |p| {
        vec![edit_of(
            find(top(p), "Targets"),
            EditOp::SetElement {
                index: 1,
                text: OTHER.into(),
            },
        )]
    });
    assert!(waited_on(&asset, &exports, &pointed, OTHER));
    let (after, asset, exports) = apply_to(&asset, &exports, |p| {
        vec![edit_of(find(top(p), "Targets"), reorder(&[1, 0]))]
    });
    assert_eq!(
        summaries(value(&after, "Targets")),
        [OTHER.to_string(), HELPER_PATH.to_string()]
    );
    assert!(waited_on(&asset, &exports, &after, OTHER));
}

/// The field reached from `parsed`'s property `name` down through `path`.
fn field<'a>(parsed: &'a ParsedPackage, name: &str, path: &[&str]) -> &'a PropertyEntry {
    let mut entry = find(top(parsed), name);
    for step in path {
        let PropertyValue::Struct { fields, .. } = &entry.value else {
            panic!("{} is not a struct", entry.name);
        };
        entry = find(fields, step);
    }
    entry
}

/// The fields of native layouts that used to be read-only edit in place, all in one save.
#[test]
fn read_only_native_fields_now_edit_in_place() {
    let edits: [(&str, &[&str], &str); 7] = [
        ("When", &["Ticks"], "200"),
        ("Transform", &["M23"], "2.5"),
        ("Bounds", &["IsValid"], "false"),
        ("Ball", &["W"], "9.5"),
        ("Key", &["Time"], "1.5"),
        ("Key", &["InterpMode"], "2"),
        ("Range", &["UpperBound", "Value"], "60"),
    ];
    let after = apply(|p| {
        edits
            .iter()
            .map(|(name, path, text)| edit_of(field(p, name, path), set(text)))
            .collect()
    });
    for (name, path, text) in edits {
        assert_eq!(
            field(&after, name, path).value.summary(),
            text,
            "{name}.{path:?}"
        );
    }
    assert_eq!(
        field(&after, "Bounds", &["Max", "X"]).value.summary(),
        "1.0"
    );
}

/// Two of a `NavAgentSelector`'s bits share one word, and change together in one save.
#[test]
fn two_nav_agent_bits_change_in_one_save() {
    let after = apply(|p| {
        vec![
            edit_of(field(p, "Agents", &["bSupportsAgent0"]), set("false")),
            edit_of(field(p, "Agents", &["bSupportsAgent1"]), set("true")),
        ]
    });
    let on: Vec<String> = match &find(top(&after), "Agents").value {
        PropertyValue::Struct { fields, .. } => fields
            .iter()
            .filter(|bit| matches!(bit.value, PropertyValue::Bool { value: true }))
            .map(|bit| bit.name.clone())
            .collect(),
        other => panic!("{other:?}"),
    };
    assert_eq!(on, ["bSupportsAgent1", "bSupportsAgent3"]);
}

/// The names of an instanced struct's fields, its type first, and what that type reads as.
fn instanced_fields(value: &PropertyValue) -> (String, Vec<String>) {
    let PropertyValue::Struct { name, fields } = value else {
        panic!("{value:?} is not a struct");
    };
    assert_eq!(fields[0].name, TYPE_FIELD, "{fields:?}");
    (
        format!("{name} {}", fields[0].value.summary()),
        fields[1..]
            .iter()
            .map(|field| format!("{}={}", field.name, field.value.summary()))
            .collect(),
    )
}

#[test]
fn an_instanced_struct_shows_its_type_as_a_field() {
    let (parsed, ..) = fixture();
    let (typed, fields) = instanced_fields(value(&parsed, "Payload"));
    assert_eq!(typed, format!("Point {POINT_PATH}"));
    assert_eq!(fields, ["X=7", "Y=(not stored)"]);
    let (typed, fields) = instanced_fields(value(&parsed, "Holder"));
    assert_eq!(typed, "InstancedStruct None");
    assert!(fields.is_empty());
}

/// A zero instanced struct is stored as one with no type, not as a block of zero fields.
#[test]
fn a_zero_instanced_struct_stores_as_an_empty_one() {
    let after = apply(|p| vec![edit_of(find(top(p), "ZeroSpot"), EditOp::Store)]);
    assert_eq!(
        instanced_fields(value(&after, "ZeroSpot")).0,
        "InstancedStruct None"
    );
}

fn retype(parsed: &ParsedPackage, name: &str, to: &str) -> ValueEdit {
    edit_of(field(parsed, name, &[TYPE_FIELD]), set(to))
}

#[test]
fn an_instanced_struct_is_retyped_to_a_reflected_struct() {
    let after = apply(|p| vec![retype(p, "Holder", POINT_PATH)]);
    let (typed, fields) = instanced_fields(value(&after, "Holder"));
    assert_eq!(typed, format!("Point {POINT_PATH}"));
    assert_eq!(fields, ["X=(not stored)", "Y=(not stored)"]);
    assert_eq!(
        value(&after, "Count").summary(),
        "7",
        "what follows reads on"
    );
}

#[test]
fn an_instanced_struct_is_retyped_to_a_native_struct() {
    const VECTOR: &str = "/Script/CoreUObject.Vector";
    let after = apply(|p| vec![retype(p, "Payload", VECTOR)]);
    let (typed, fields) = instanced_fields(value(&after, "Payload"));
    assert_eq!(typed, format!("Vector {VECTOR}"));
    assert_eq!(fields, ["X=0.0", "Y=0.0", "Z=0.0"]);
    let import = after
        .imports
        .iter()
        .find(|import| import.path == VECTOR)
        .expect("imported");
    assert_eq!(import.class_name, "ScriptStruct");
}

#[test]
fn an_instanced_struct_is_retyped_to_none() {
    let after = apply(|p| vec![retype(p, "Payload", "None")]);
    assert_eq!(
        instanced_fields(value(&after, "Payload")),
        ("InstancedStruct None".to_string(), Vec::new())
    );
}

/// One nothing stores yet is stored empty, then given a type in the next save.
#[test]
fn an_unset_instanced_struct_is_stored_empty_then_typed() {
    let (asset, exports) = unversioned_package();
    let (stored, asset, exports) = apply_to(&asset, &exports, |p| {
        vec![edit_of(find(top(p), "Spot"), EditOp::Store)]
    });
    assert_eq!(
        instanced_fields(value(&stored, "Spot")).0,
        "InstancedStruct None"
    );
    let (typed, ..) = apply_to(&asset, &exports, |p| vec![retype(p, "Spot", POINT_PATH)]);
    assert_eq!(
        instanced_fields(value(&typed, "Spot")).0,
        format!("Point {POINT_PATH}")
    );
}

#[test]
fn a_type_the_mappings_do_not_describe_is_refused() {
    let error = refused(|p| vec![retype(p, "Payload", "/Script/Test.Missing")]);
    assert!(
        error.contains("not a struct the mappings file describes"),
        "{error}"
    );
}

/// A retype writes the payload anew, so a field of the old payload cannot be edited beside it.
#[test]
fn fields_inside_a_retyped_payload_are_refused_in_the_same_save() {
    let error = refused(|p| {
        vec![
            retype(p, "Payload", "None"),
            edit_of(field(p, "Payload", &["X"]), set("9")),
        ]
    });
    assert!(error.contains("given another type"), "{error}");
}

/// A retype inside another instanced struct moves both lengths: its own, which it writes, and the
/// one around it.
#[test]
fn a_nested_retype_moves_both_sizes() {
    let after = apply(|p| {
        vec![edit_of(
            field(p, "Nest", &["Inner", TYPE_FIELD]),
            set("/Script/CoreUObject.Vector"),
        )]
    });
    let (outer, _) = instanced_fields(value(&after, "Nest"));
    assert!(outer.starts_with("Wrapper "), "{outer}");
    let (inner, fields) = instanced_fields(&field(&after, "Nest", &["Inner"]).value);
    assert!(inner.starts_with("Vector "), "{inner}");
    assert_eq!(fields, ["X=0.0", "Y=0.0", "Z=0.0"]);
}
