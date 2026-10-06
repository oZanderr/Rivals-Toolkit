//! Edits applied to a package cooked with tagged properties, read back, and compared.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use crate::edit::{EditOp, PackageEdits, ValueEdit, kind_of, patch_package, verify_patch};
use crate::mappings::Mappings;
use crate::package::{AssetBundle, ExportStatus, ParsedPackage, parse_package};
use crate::tagged_fixture::{
    head, int, my_struct, name, package_of, sparse_mappings, sparse_package, string, tagged_package,
};
use crate::value::{PropertyEntry, PropertyValue};

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

const MENU_TABLE: &str = "/Game/UI/Menu_ST.Menu_ST";

fn table_literal(key: &str) -> String {
    format!("LOCTABLE(\"{MENU_TABLE}\", \"{key}\")")
}

fn table_of(entry: &PropertyEntry) -> Option<crate::text_literal::TextLiteral> {
    crate::text_literal::of_value(&entry.value)
}

/// The fixture with its title turned into a string table text, handed back as bytes.
fn with_table_title() -> (ParsedPackage, Vec<u8>, Vec<u8>) {
    let (asset, exports) = tagged_package();
    apply_to(&asset, &exports, |p| {
        vec![edit_of(find(top(p), "Title"), set(&table_literal("Play")))]
    })
}

/// A text given a table literal becomes a string table text, and the package imports the table and
/// reads the export only after it exists, the way the cook writes a text that shows a table entry.
/// Turned back into fixed text, the tag's size follows it again.
#[test]
fn a_text_given_a_table_literal_imports_and_waits_on_its_table() {
    let (after, asset, exports) = with_table_title();
    assert_eq!(
        table_of(find(top(&after), "Title")),
        Some(crate::text_literal::TextLiteral::Table {
            table_id: MENU_TABLE.into(),
            key: "Play".into()
        })
    );
    let import = after
        .imports
        .iter()
        .find(|import| import.path == MENU_TABLE)
        .expect("the table is imported");
    assert_eq!(import.class_name, "StringTable");
    let header = crate::read_header(&AssetBundle {
        asset: &asset,
        exports: &exports,
    })
    .expect("header");
    let runs = &crate::runs_of(&header).expect("runs")[0];
    assert!(
        runs.create_before_serialize.contains(&import.index),
        "{runs:?}"
    );

    let (back, ..) = apply_to(&asset, &exports, |p| {
        vec![edit_of(find(top(p), "Title"), set("INVTEXT(\"Back\")"))]
    });
    assert_eq!(find(top(&back), "Title").value.summary(), "Back");
}

/// A plain string over a table text shows that string in every language, and the save says the
/// text no longer follows its table; the text's own `Table:Key` form names another entry instead.
#[test]
fn a_plain_string_over_a_table_text_is_noted_and_a_table_key_repoints_it() {
    let (before, asset, exports) = with_table_title();
    let changes = PackageEdits {
        values: vec![edit_of(find(top(&before), "Title"), set("Fixed"))],
        ..Default::default()
    };
    let bundle = AssetBundle {
        asset: &asset,
        exports: &exports,
    };
    let patched = patch_package(&bundle, &before, &changes, None).expect("patch");
    assert!(
        patched
            .notes
            .iter()
            .any(|note| note.contains("no longer follows") && note.contains(MENU_TABLE)),
        "{:?}",
        patched.notes
    );
    let after = parse(&patched.asset, &patched.exports);
    verify_patch(&before, &after, &changes, &patched.applied).expect("verifies");
    assert_eq!(
        table_of(find(top(&after), "Title")),
        Some(crate::text_literal::TextLiteral::Invariant("Fixed".into()))
    );

    let (repointed, ..) = apply_to(&asset, &exports, |p| {
        vec![edit_of(
            find(top(p), "Title"),
            set(&format!("{MENU_TABLE}:Quit")),
        )]
    });
    assert_eq!(
        table_of(find(top(&repointed), "Title")),
        Some(crate::text_literal::TextLiteral::Table {
            table_id: MENU_TABLE.into(),
            key: "Quit".into()
        })
    );
}

/// A table text's `TableId` pointed at another table imports that table and waits on it, the way
/// the whole text given a table literal does.
#[test]
fn a_table_id_pointed_elsewhere_imports_the_new_table() {
    const OTHER: &str = "/Game/UI/Other_ST.Other_ST";
    let (before, asset, exports) = with_table_title();
    let table = match &find(top(&before), "Title").value {
        PropertyValue::Text { parts, .. } => find(parts, "TableId").clone(),
        other => panic!("{other:?}"),
    };
    let changes = PackageEdits {
        values: vec![edit_of(&table, set(OTHER))],
        ..Default::default()
    };
    let bundle = AssetBundle {
        asset: &asset,
        exports: &exports,
    };
    let patched = patch_package(&bundle, &before, &changes, None).expect("patch");
    let after = parse(&patched.asset, &patched.exports);
    verify_patch(&before, &after, &changes, &patched.applied).expect("verifies");
    assert_eq!(
        table_of(find(top(&after), "Title")),
        Some(crate::text_literal::TextLiteral::Table {
            table_id: OTHER.into(),
            key: "Play".into()
        })
    );
    let import = after
        .imports
        .iter()
        .find(|import| import.path == OTHER)
        .expect("the new table is imported");
    assert_eq!(import.class_name, "StringTable");
    let header = crate::read_header(&AssetBundle {
        asset: &patched.asset,
        exports: &patched.exports,
    })
    .expect("header");
    let runs = &crate::runs_of(&header).expect("runs")[0];
    assert!(
        runs.create_before_serialize.contains(&import.index),
        "{runs:?}"
    );
}

/// An array of structs is written under one tag for every element, so its elements move as a
/// block after that tag, which stays where it is.
#[test]
fn an_array_of_structs_is_reordered_under_its_element_tag() {
    let (asset, exports) = tagged_package();
    let (_, asset, exports) = apply_to(&asset, &exports, |p| {
        vec![edit_of(
            find(top(p), "Points"),
            EditOp::Insert {
                index: 1,
                key: None,
            },
        )]
    });
    let (_, asset, exports) = apply_to(&asset, &exports, |p| {
        let PropertyValue::Array { items } = &find(top(p), "Points").value else {
            panic!("not an array");
        };
        let PropertyValue::Struct { fields, .. } = &items[1] else {
            panic!("not a struct");
        };
        vec![edit_of(find(fields, "X"), set("9"))]
    });
    let (after, ..) = apply_to(&asset, &exports, |p| {
        vec![edit_of(
            find(top(p), "Points"),
            EditOp::Reorder { order: vec![1, 0] },
        )]
    });
    let xs: Vec<String> = items_of(find(top(&after), "Points"))
        .iter()
        .map(|item| match item {
            PropertyValue::Struct { fields, .. } => find(fields, "X").value.summary(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(xs, ["9", "5"]);
}

/// A text set as a whole and one of its parts edited in the same save would write over each other,
/// so the save is refused saying which to choose; a part edit alone still works.
#[test]
fn a_whole_text_and_one_of_its_parts_are_not_edited_together() {
    let (before, asset, exports) = with_table_title();
    let title = find(top(&before), "Title");
    let key = match &title.value {
        PropertyValue::Text { parts, .. } => find(parts, "Key"),
        other => panic!("{other:?}"),
    };
    let changes = PackageEdits {
        values: vec![
            edit_of(title, set("INVTEXT(\"x\")")),
            edit_of(key, set("Quit")),
        ],
        ..Default::default()
    };
    let error = patch_package(
        &AssetBundle {
            asset: &asset,
            exports: &exports,
        },
        &before,
        &changes,
        None,
    )
    .err()
    .expect("refused");
    assert!(error.contains("not both"), "{error}");

    let (after, ..) = apply_to(&asset, &exports, |p| {
        let key = match &find(top(p), "Title").value {
            PropertyValue::Text { parts, .. } => find(parts, "Key").clone(),
            other => panic!("{other:?}"),
        };
        vec![edit_of(&key, set("Quit"))]
    });
    assert_eq!(
        find(top(&after), "Title").value.summary(),
        format!("{MENU_TABLE}:Quit")
    );
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
            "Extra",
            "Label",
            "On",
            "Mode",
            "Nested",
            "Items",
            "Scores",
            "Counts",
            "Chain",
            "Caption",
            "OnFired",
            "OnChanged",
            "Watched"
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

/// A table literal typed for an absent text adds its tag as a string table text, and imports the
/// table the same as an edit of a stored text does.
#[test]
fn a_table_literal_for_an_absent_text_adds_its_tag_and_table() {
    let (asset, exports) = sparse_package();
    let mappings = sparse_mappings();
    let (after, ..) = apply_declared(&asset, &exports, Some(&mappings), |p| {
        vec![edit_of(
            find(holder(p), "Caption"),
            set(&table_literal("Title")),
        )]
    })
    .expect("added");
    assert_eq!(
        table_of(find(holder(&after), "Caption")),
        Some(crate::text_literal::TextLiteral::Table {
            table_id: MENU_TABLE.into(),
            key: "Title".into()
        })
    );
    assert!(after.imports.iter().any(|import| import.path == MENU_TABLE));
    assert_eq!(find(holder(&after), "Count").value.summary(), "3");
}

/// A delegate, a multicast delegate and a field path the block lacks take tags of their own:
/// typed, or stored empty and given a binding afterwards.
#[test]
fn delegate_multicast_and_field_path_tags_are_added_for_absent_properties() {
    const HANDLER: &str = "/Game/Helpers.Helper::Handler";
    let (asset, exports) = sparse_package();
    let mappings = sparse_mappings();
    let (after, asset, exports) = apply_declared(&asset, &exports, Some(&mappings), |p| {
        vec![
            edit_of(find(holder(p), "OnFired"), set(HANDLER)),
            edit_of(find(holder(p), "OnChanged"), EditOp::Store),
            edit_of(find(holder(p), "Watched"), set("Count.Inner")),
        ]
    })
    .expect("added");
    assert_eq!(find(holder(&after), "OnFired").value.summary(), HANDLER);
    assert!(items_of(find(holder(&after), "OnChanged")).is_empty());
    assert_eq!(
        find(holder(&after), "Watched").value.summary(),
        "Count.Inner"
    );
    assert!(
        after
            .imports
            .iter()
            .any(|import| import.path == "/Game/Helpers.Helper")
    );

    let (bound, ..) = apply_declared(&asset, &exports, Some(&mappings), |p| {
        vec![edit_of(
            find(holder(p), "OnChanged"),
            EditOp::Insert {
                index: 0,
                key: None,
            },
        )]
    })
    .expect("bound");
    let bindings = items_of(find(holder(&bound), "OnChanged"));
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].summary(), "None");
}

/// A class, a soft class and a sparse delegate's tags read as the object, soft object and list of
/// bindings they hold, and edit as those.
#[test]
fn class_soft_class_and_sparse_delegate_tags_read_and_edit() {
    let mut e = Vec::new();
    head(&mut e, "Kind", "ClassProperty", 4);
    e.push(0);
    e.extend_from_slice(&0i32.to_le_bytes());
    let mut soft = Vec::new();
    name(&mut soft, "/Game/A");
    name(&mut soft, "B");
    string(&mut soft, "");
    head(&mut e, "Soft", "SoftClassProperty", soft.len());
    e.push(0);
    e.extend_from_slice(&soft);
    let mut sparse = 1i32.to_le_bytes().to_vec();
    sparse.extend_from_slice(&0i32.to_le_bytes());
    name(&mut sparse, "Foo");
    head(
        &mut e,
        "Sparse",
        "MulticastSparseDelegateProperty",
        sparse.len(),
    );
    e.push(0);
    e.extend_from_slice(&sparse);
    name(&mut e, "None");
    e.extend_from_slice(&0i32.to_le_bytes());
    let (asset, exports) = package_of(e);

    let before = parse(&asset, &exports);
    assert_eq!(find(top(&before), "Kind").value.summary(), "None");
    assert_eq!(find(top(&before), "Soft").value.summary(), "/Game/A.B");
    assert_eq!(
        items_of(find(top(&before), "Sparse"))[0].summary(),
        "None::Foo"
    );

    let (after, ..) = apply_to(&asset, &exports, |p| {
        vec![
            edit_of(find(top(p), "Kind"), set("/Game/Things.Thing_C")),
            edit_of(find(top(p), "Soft"), set("/Game/C.D")),
            edit_of(
                find(top(p), "Sparse"),
                EditOp::SetElement {
                    index: 0,
                    text: "/Game/Things.Thing_C::Fired".into(),
                },
            ),
        ]
    });
    assert_eq!(
        find(top(&after), "Kind").value.summary(),
        "/Game/Things.Thing_C"
    );
    assert_eq!(find(top(&after), "Soft").value.summary(), "/Game/C.D");
    assert_eq!(
        items_of(find(top(&after), "Sparse"))[0].summary(),
        "/Game/Things.Thing_C::Fired"
    );
}

/// A tag of a type nothing reads keeps its bytes as they are, and takes bytes typed for it at any
/// length: the tag's size follows them.
#[test]
fn an_undecoded_tag_is_replaced_from_raw_hex() {
    let mut e = Vec::new();
    head(&mut e, "Mystery", "SomeFutureProperty", 4);
    e.push(0);
    e.extend_from_slice(&[1, 2, 3, 4]);
    int(&mut e, "Damage", 99);
    name(&mut e, "None");
    e.extend_from_slice(&0i32.to_le_bytes());
    let (asset, exports) = package_of(e);
    let mystery = |p: &ParsedPackage| match &find(top(p), "Mystery").value {
        PropertyValue::Struct { fields, .. } => fields[0].clone(),
        other => panic!("{other:?}"),
    };
    let before = parse(&asset, &exports);
    assert!(matches!(
        mystery(&before).value,
        PropertyValue::Undecoded { bytes: 4, .. }
    ));
    let (after, ..) = apply_to(&asset, &exports, |p| {
        vec![edit_of(
            &mystery(p),
            EditOp::SetRaw {
                hex: "AA BB CC DD EE".into(),
            },
        )]
    });
    assert!(matches!(
        mystery(&after).value,
        PropertyValue::Undecoded { bytes: 5, .. }
    ));
    assert_eq!(find(top(&after), "Damage").value.summary(), "99");
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

/// A field set that goes through a container's elements records how many it held, so the same
/// edits applied once it has grown are drift rather than a write into another element.
#[test]
fn a_field_set_through_a_container_expects_its_length() {
    let (asset, exports) = tagged_package();
    let before = parse(&asset, &exports);
    let values = find(top(&before), "Values");
    let mut changes = PackageEdits {
        field_sets: vec![crate::edit::FieldSet {
            offset: values.span.expect("a span").0,
            expect_name: values.name.clone(),
            expect_element: values.element,
            path: vec!["[0]".into()],
            text: "5".into(),
        }],
        ..Default::default()
    };
    changes.expect = crate::edit::expectations(&before, &changes);
    assert_eq!(
        changes.expect.values.values().collect::<Vec<_>>(),
        ["[2 items]"]
    );
    crate::edit::check_expectations(&before, &changes).expect("as read");

    let grown = apply(|p| {
        vec![edit_of(
            find(top(p), "Values"),
            EditOp::Insert {
                index: 2,
                key: None,
            },
        )]
    });
    let drift = crate::edit::check_expectations(&grown, &changes).expect_err("drift");
    assert!(drift.contains("[3 items]"), "{drift}");
}

/// An unset struct's preview reaches a field three structs down, so it can be shown and diffed.
#[test]
fn a_preview_reaches_a_field_three_structs_down() {
    let (asset, exports) = sparse_package();
    let mappings = sparse_mappings();
    let parsed = parse_declared(&asset, &exports, Some(&mappings));
    let level = |entries: &[PropertyEntry], name: &str| -> Vec<PropertyEntry> {
        match &find(entries, name).value {
            PropertyValue::Unset { fields, .. } => fields.clone(),
            other => panic!("{name}: {other:?}"),
        }
    };
    let level2 = level(holder(&parsed), "Chain");
    let level3 = level(&level2, "Level2");
    let leaf = level(&level3, "Level3");
    assert!(is_absent(find(&leaf, "Leaf")), "{leaf:?}");
}

/// A set element given another's value would hold that key twice, which the check after the
/// patch refuses; a new value of its own is fine.
#[test]
fn a_set_element_given_anothers_value_is_refused() {
    let (asset, exports) = tagged_package();
    let before = parse(&asset, &exports);
    let names = find(top(&before), "Names");
    let verified = |text: &str| {
        let changes = PackageEdits {
            values: vec![edit_of(
                names,
                EditOp::SetElement {
                    index: 1,
                    text: text.into(),
                },
            )],
            ..Default::default()
        };
        let bundle = AssetBundle {
            asset: &asset,
            exports: &exports,
        };
        let patched = patch_package(&bundle, &before, &changes, None).expect("patch");
        let after = parse(&patched.asset, &patched.exports);
        verify_patch(&before, &after, &changes, &patched.applied)
    };
    let err = verified("Foo").expect_err("a repeated key");
    assert!(err.contains("same key twice, at 0 and 1"), "{err}");
    verified("Tag").expect("a key of its own");
}

/// A field edited inside a struct set element can make it equal another element, which is the
/// same repeated key reached another way.
#[test]
fn a_struct_key_edited_into_another_is_refused() {
    let (asset, exports) = holder_package();
    let mappings = holder_mappings();
    let (_, asset, exports) = apply_declared(&asset, &exports, Some(&mappings), |p| {
        vec![edit_of(
            find(holder(p), "Structs"),
            EditOp::Insert {
                index: 1,
                key: None,
            },
        )]
    })
    .expect("a default element");
    let second_x = |p: &ParsedPackage| match &find(holder(p), "Structs").value {
        PropertyValue::Set { items } => match &items[1] {
            PropertyValue::Struct { fields, .. } => find(fields, "X").clone(),
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    };
    let Err(err) = apply_declared(&asset, &exports, Some(&mappings), |p| {
        vec![edit_of(&second_x(p), set("7"))]
    }) else {
        panic!("a repeated key");
    };
    assert!(err.contains("same key twice"), "{err}");
    apply_declared(&asset, &exports, Some(&mappings), |p| {
        vec![edit_of(&second_x(p), set("8"))]
    })
    .expect("a key of its own");
}

/// A save that drops the names nothing uses keeps the ones in use, a name an earlier save added
/// among them, and every value reads the same, as the save's own check confirms.
#[test]
fn names_nothing_uses_are_dropped_and_every_value_reads_the_same() {
    let (asset, exports) = tagged_package();
    let (_, asset, exports) = apply_to(&asset, &exports, |p| {
        vec![edit_of(find(top(p), "Tag"), set("Brand_New"))]
    });
    let before = parse(&asset, &exports);
    let header = crate::package::read_header(&AssetBundle {
        asset: &asset,
        exports: &exports,
    })
    .expect("header");
    let total = crate::package::header_size(&AssetBundle {
        asset: &asset,
        exports: &exports,
    })
    .expect("size");
    let unused = crate::names::unused_names(&before, &header, &exports, total).expect("known");
    assert!(unused.contains(&"EMode::B".to_string()), "{unused:?}");
    assert!(!unused.contains(&"Brand_New".to_string()), "{unused:?}");
    assert!(!unused.contains(&"None".to_string()), "{unused:?}");

    let changes = PackageEdits {
        compact_names: true,
        ..Default::default()
    };
    let bundle = AssetBundle {
        asset: &asset,
        exports: &exports,
    };
    let patched = patch_package(&bundle, &before, &changes, None).expect("patch");
    let after = parse(&patched.asset, &patched.exports);
    verify_patch(&before, &after, &changes, &patched.applied).expect("verifies");
    assert_eq!(after.names.len(), before.names.len() - unused.len());
    assert!(!after.names.iter().any(|name| name == "EMode::B"));
    match &find(top(&after), "Tag").value {
        PropertyValue::Name { value } => assert_eq!(value, "Brand_New"),
        other => panic!("{other:?}"),
    }

    let again = patch_package(
        &AssetBundle {
            asset: &patched.asset,
            exports: &patched.exports,
        },
        &after,
        &changes,
        None,
    )
    .err()
    .expect("nothing left to drop");
    assert!(again.contains("every name"), "{again}");
}

/// The fixture as a package named `/Game/A` whose object is `A` and whose FName soft path points
/// at that object, so saving it under another name has a path into itself of each kind to follow.
fn self_referencing_package() -> (Vec<u8>, Vec<u8>) {
    let (asset, exports) = tagged_package();
    let bundle = AssetBundle {
        asset: &asset,
        exports: &exports,
    };
    let header = crate::package::read_header(&bundle).expect("header");
    let mut names = header.name_map.clone();
    let mut table = header.exports.clone();
    table[0].object_name = names.store("A");
    let named = crate::write::rewrite(
        &bundle,
        &[],
        crate::write::HeaderDraft {
            names: Some(names),
            exports: Some(table),
            package_name: Some("/Game/A".into()),
            ..Default::default()
        },
    )
    .expect("renamed");
    let (_, asset, exports) = apply_to(&named.asset, &named.exports, |p| {
        vec![edit_of(find(top(p), "Asset"), set("/Game/A.A"))]
    });
    (asset, exports)
}

/// Saving under another name moves the stored name, the object named after the package, a soft
/// path the package holds to itself as names and one it holds as a string, and leaves a path into
/// another package alone.
#[test]
fn a_package_saved_under_another_name_renames_the_paths_into_itself() {
    let (asset, exports) = self_referencing_package();
    let before = parse(&asset, &exports);
    let save_as = crate::identity::SaveAs {
        package: "/Game/Mods/C".into(),
        rename_objects: true,
    };
    let rename = crate::identity::PathRename::plan(&before, &save_as)
        .expect("plans")
        .expect("a new name");
    let bundle = AssetBundle {
        asset: &asset,
        exports: &exports,
    };
    let stage = crate::edit::patch_identity(&bundle, &before, &rename).expect("renamed");
    let mid = parse(&stage.asset, &stage.exports);
    let edits = PackageEdits {
        values: crate::identity::identity_value_edits(&mid, &rename),
        ..Default::default()
    };
    assert_eq!(
        edits.values.len(),
        1,
        "only the string-backed soft path is left"
    );
    let staged = AssetBundle {
        asset: &stage.asset,
        exports: &stage.exports,
    };
    let patched = patch_package(&staged, &mid, &edits, None).expect("patch");
    let after = parse(&patched.asset, &patched.exports);
    verify_patch(&mid, &after, &edits, &patched.applied).expect("the strings verify");
    crate::edit::verify_identity(&before, &after, &rename).expect("the rename verifies");

    assert_eq!(after.info.package_name, "/Game/Mods/C");
    assert_eq!(after.exports[0].path, "/Game/Mods/C.C");
    let soft = |name: &str| match &find(top(&after), name).value {
        PropertyValue::SoftObject { path } => path.clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(soft("Asset"), "/Game/Mods/C.C");
    assert_eq!(soft("Path"), "/Game/Mods/C.B");

    // Already that name: nothing to do.
    let same = crate::identity::SaveAs {
        package: "/Game/A".into(),
        rename_objects: true,
    };
    assert!(
        crate::identity::PathRename::plan(&before, &same)
            .expect("plans")
            .is_none()
    );
}

/// A level names its package inside its world, and a name that is not a package path is refused.
#[test]
fn a_level_or_a_malformed_name_cannot_be_saved_as() {
    let (asset, exports) = self_referencing_package();
    let mut parsed = parse(&asset, &exports);
    let save_as = |package: &str| crate::identity::SaveAs {
        package: package.into(),
        rename_objects: true,
    };
    for bad in ["Game/X", "/Game", "/Game/X.Y", "/Game/My Thing", "/Game/X/"] {
        let err = crate::identity::PathRename::plan(&parsed, &save_as(bad)).expect_err(bad);
        assert!(err.contains("not a package name"), "{err}");
    }
    parsed.exports[0].class_name = "World".into();
    let err =
        crate::identity::PathRename::plan(&parsed, &save_as("/Game/Mods/C")).expect_err("a level");
    assert!(err.contains("level"), "{err}");
}

/// A schema with one class of objects holding one number, rooted at `Object`.
fn thing_mappings() -> Mappings {
    use usmap::{Property, PropertyInner, Struct};
    Mappings::from_structs(vec![
        Struct {
            name: "Object".into(),
            super_struct: None,
            properties: Vec::new(),
        },
        Struct {
            name: "TestThing".into(),
            super_struct: Some("Object".into()),
            properties: vec![Property {
                name: "Count".into(),
                array_dim: 1,
                index: 0,
                inner: PropertyInner::Int,
            }],
        },
    ])
}

fn add_thing(outer: Option<u32>, name: &str, class: &str) -> PackageEdits {
    PackageEdits {
        add_exports: vec![crate::duplicate::AddExport {
            class: class.into(),
            outer,
            name: name.into(),
            layout: None,
        }],
        ..Default::default()
    }
}

/// An empty object of a class the schema knows reads back complete and storing nothing, under its
/// outer, and takes a value with an ordinary edit in the next save.
#[test]
fn an_empty_object_of_a_class_is_added_and_takes_a_value_next() {
    let mappings = thing_mappings();
    let (asset, exports) = tagged_package();
    let bundle = AssetBundle {
        asset: &asset,
        exports: &exports,
    };
    let before = parse_declared(&asset, &exports, Some(&mappings));
    let changes = add_thing(Some(0), "Thing", "/Script/Test.TestThing");
    let patched = patch_package(&bundle, &before, &changes, Some(&mappings)).expect("added");
    let after = parse_declared(&patched.asset, &patched.exports, Some(&mappings));
    verify_patch(&before, &after, &changes, &patched.applied).expect("verifies");
    let thing = &after.exports[1];
    assert_eq!(thing.path, "/Game/TestPackage.TestObject:Thing");
    assert_eq!(thing.class_name, "TestThing");

    let (next, ..) = apply_declared(&patched.asset, &patched.exports, Some(&mappings), |p| {
        vec![edit_of(find(&p.exports[1].properties, "Count"), set("7"))]
    })
    .expect("a value is set on it");
    assert!(matches!(
        find(&next.exports[1].properties, "Count").value,
        PropertyValue::Int { value: 7 }
    ));
}

/// A class the schema does not know, a name its outer already holds and a bad name are refused.
#[test]
fn an_object_of_an_unknown_class_or_a_taken_name_is_refused() {
    let mappings = thing_mappings();
    let (asset, exports) = tagged_package();
    let bundle = AssetBundle {
        asset: &asset,
        exports: &exports,
    };
    let before = parse_declared(&asset, &exports, Some(&mappings));
    let refused = |changes: PackageEdits| {
        patch_package(&bundle, &before, &changes, Some(&mappings))
            .err()
            .expect("refused")
    };
    let unknown = refused(add_thing(None, "Thing", "/Script/Test.Missing"));
    assert!(unknown.contains("does not know"), "{unknown}");
    let taken = refused(add_thing(None, "TestObject", "/Script/Test.TestThing"));
    assert!(taken.contains("already named"), "{taken}");
    let bad = refused(add_thing(None, "Two Words", "/Script/Test.TestThing"));
    assert!(bad.contains("not an object name"), "{bad}");
}
