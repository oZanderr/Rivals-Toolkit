//! Replacing text across a mod: the strings its scripts hold, the strings and texts its objects
//! store and its string tables' entries, in one write.
//!
//! Names are left alone, since one is a member, a row or an object rather than text, and so are
//! paths unless asked for, since one names something another package has to find: a rename or a
//! move changes those. A text's key and namespace stay as they are, and a text showing a string
//! table's entry is changed in the table.

use rivals_uasset::text_literal::{self, TextLiteral};
use rivals_uasset::{
    EditOp, Expr, Mappings, PackageEdits, ParsedPackage, PropertyEntry, PropertyValue,
    ScriptConstEdit, StringEdit, StringOp, TextLiteral as ScriptText, ValueEdit, kind_of,
    visit_spans,
};
use serde::Serialize;

use crate::asset::{self, AssetSource};
use crate::asset_edit::{AssetEditRequest, read_package};

#[derive(Debug, Clone, Default)]
pub struct ReplaceOptions {
    pub find: String,
    pub to: String,
    /// Match any case, and write the replacement in the case of what it replaces.
    pub ignore_case: bool,
    /// Match only where the text stands as a word of its own.
    pub whole_word: bool,
    /// Also change strings that are paths, `/Game/...` and the like.
    pub paths: bool,
    /// Only the packages whose path holds one of these; none keeps every package.
    pub filters: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Replaced {
    pub label: String,
    pub before: String,
    pub after: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReplacedPackage {
    pub entry: String,
    pub changes: Vec<Replaced>,
    /// Matches left as they are, and why.
    pub left: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReplaceReport {
    pub packages: Vec<ReplacedPackage>,
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pak: Option<String>,
}

/// `text` with every match replaced, or `None` when nothing matched.
pub fn replace_in(text: &str, options: &ReplaceOptions) -> Option<String> {
    let find = options.find.as_str();
    if find.is_empty() || text.len() < find.len() {
        return None;
    }
    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    let mut matched = false;
    while at < text.len() {
        let rest = &text[at..];
        let here = rest.len() >= find.len()
            && rest.is_char_boundary(find.len())
            && if options.ignore_case {
                rest[..find.len()].eq_ignore_ascii_case(find)
            } else {
                rest.starts_with(find)
            }
            && (!options.whole_word
                || (!word(text[..at].chars().next_back())
                    && !word(rest[find.len()..].chars().next())));
        if here {
            let found = &rest[..find.len()];
            out.push_str(&in_case_of(found, &options.to, options.ignore_case));
            at += find.len();
            matched = true;
        } else {
            let c = rest.chars().next().unwrap_or_default();
            out.push(c);
            at += c.len_utf8().max(1);
        }
    }
    matched.then_some(out)
}

/// The replacement written in the case of the text it replaces: all capitals, all lower case or
/// capitalised, as `found` is, when matching ignored case; as given otherwise.
fn in_case_of(found: &str, to: &str, ignore_case: bool) -> String {
    if !ignore_case {
        return to.to_string();
    }
    let letters: Vec<char> = found.chars().filter(|c| c.is_alphabetic()).collect();
    if letters.is_empty() {
        return to.to_string();
    }
    if letters.iter().all(|c| c.is_uppercase()) && letters.len() > 1 {
        return to.to_uppercase();
    }
    if letters.iter().all(|c| c.is_lowercase()) {
        return to.to_lowercase();
    }
    if letters[0].is_uppercase() && letters[1..].iter().all(|c| c.is_lowercase()) {
        let mut chars = to.chars();
        return match chars.next() {
            Some(first) => first
                .to_uppercase()
                .chain(chars.flat_map(char::to_lowercase))
                .collect(),
            None => String::new(),
        };
    }
    to.to_string()
}

fn is_path(text: &str) -> bool {
    ["/Game/", "/Script/", "/Engine/", "/Marvel/"]
        .iter()
        .any(|root| text.starts_with(root))
}

/// The edits replacing text in one package, with what each changes and what it leaves.
pub fn package_replacements(
    parsed: &ParsedPackage,
    options: &ReplaceOptions,
) -> (PackageEdits, Vec<Replaced>, Vec<String>) {
    let mut edits = PackageEdits::default();
    let mut changes = Vec::new();
    let mut left = Vec::new();
    for export in &parsed.exports {
        if let Some(script) = export.script.as_ref().filter(|s| s.complete()) {
            let locked = script.resize_lock();
            visit_spans(script, &mut |expr, span, parent| {
                let value = match expr {
                    Expr::StringConst { value, .. } | Expr::UnicodeStringConst { value, .. } => {
                        value
                    }
                    _ => return,
                };
                // A text's key, namespace and table are what it is looked up by.
                if let Some(Expr::TextConst { text }) = parent {
                    let keeps = match text {
                        ScriptText::Localized { key, namespace, .. } => {
                            std::ptr::eq(key.as_ref(), expr)
                                || std::ptr::eq(namespace.as_ref(), expr)
                        }
                        ScriptText::StringTable { .. } => true,
                        _ => false,
                    };
                    if keeps {
                        return;
                    }
                }
                if is_path(value) && !options.paths {
                    return;
                }
                let Some(new) = replace_in(value, options) else {
                    return;
                };
                let statement = script
                    .statements
                    .iter()
                    .rev()
                    .find(|statement| statement.at <= span.at);
                let label = format!(
                    "{} at {}",
                    export.object_name,
                    statement.map_or_else(String::new, |s| format!("{:#06X}", s.offset))
                );
                if let Some(lock) = &locked
                    && new.len() != value.len()
                {
                    left.push(format!(
                        "{label}: {value:?} would change length, and the script has to keep its size ({lock})"
                    ));
                    return;
                }
                let Some(statement) = statement else {
                    return;
                };
                edits.scripts.push(ScriptConstEdit {
                    export: export.index,
                    statement: statement.offset,
                    at: Some(span.offset),
                    value: new.clone(),
                    ..ScriptConstEdit::default()
                });
                changes.push(Replaced {
                    label,
                    before: value.clone(),
                    after: new,
                });
            });
        }
        let mut walk = |entries: &[PropertyEntry], prefix: &str| {
            replace_entries(
                entries,
                prefix,
                options,
                &mut edits,
                &mut changes,
                &mut left,
            )
        };
        walk(&export.properties, &export.object_name);
        walk(&export.defaults, &export.object_name);
        if let Some(table) = &export.data_table {
            for row in &table.rows {
                walk(
                    &row.fields,
                    &format!("{}[{}]", export.object_name, row.name),
                );
            }
        }
        if let Some(table) = &export.string_table {
            for (index, entry) in table.entries.iter().enumerate() {
                if let Some(new) = replace_in(&entry.source, options) {
                    edits.strings.push(StringEdit {
                        export: export.index,
                        object: None,
                        op: StringOp::SetSource {
                            index: index as u32,
                            key: entry.key.clone(),
                            to: new.clone(),
                        },
                    });
                    changes.push(Replaced {
                        label: format!("{} entry {}", export.object_name, entry.key),
                        before: entry.source.clone(),
                        after: new,
                    });
                }
            }
        }
    }
    (edits, changes, left)
}

/// A text with its source strings replaced, as the literal a value edit takes.
fn replaced_text(literal: &TextLiteral, options: &ReplaceOptions) -> Option<TextLiteral> {
    match literal {
        TextLiteral::Invariant(text) => replace_in(text, options).map(TextLiteral::Invariant),
        TextLiteral::Localized {
            namespace,
            key,
            source,
        } => replace_in(source, options).map(|source| TextLiteral::Localized {
            namespace: namespace.clone(),
            key: key.clone(),
            source,
        }),
        TextLiteral::Transform { upper, inner } => {
            replaced_text(inner, options).map(|inner| TextLiteral::Transform {
                upper: *upper,
                inner: Box::new(inner),
            })
        }
        TextLiteral::Table { .. } => None,
    }
}

fn replace_entries(
    entries: &[PropertyEntry],
    prefix: &str,
    options: &ReplaceOptions,
    edits: &mut PackageEdits,
    changes: &mut Vec<Replaced>,
    left: &mut Vec<String>,
) {
    for entry in entries {
        let label = format!("{prefix}.{}", entry.label());
        let Some((offset, _)) = entry.span else {
            continue;
        };
        let edit = |op: EditOp| ValueEdit {
            offset,
            expect_name: entry.name.clone(),
            expect_element: entry.element,
            expect_kind: kind_of(&entry.value),
            op,
        };
        match &entry.value {
            PropertyValue::Str { value } if options.paths || !is_path(value) => {
                if let Some(new) = replace_in(value, options) {
                    edits.values.push(edit(EditOp::Set { text: new.clone() }));
                    changes.push(Replaced {
                        label,
                        before: value.clone(),
                        after: new,
                    });
                }
            }
            PropertyValue::SoftObject { path } if options.paths => {
                if let Some(new) = replace_in(path, options) {
                    edits.values.push(edit(EditOp::Set { text: new.clone() }));
                    changes.push(Replaced {
                        label,
                        before: path.clone(),
                        after: new,
                    });
                }
            }
            PropertyValue::Text { .. } => {
                let Some(literal) = text_literal::of_value(&entry.value) else {
                    continue;
                };
                match replaced_text(&literal, options) {
                    Some(new) => {
                        let text = text_literal::format(&new);
                        edits.values.push(edit(EditOp::Set { text: text.clone() }));
                        changes.push(Replaced {
                            label,
                            before: text_literal::format(&literal),
                            after: text,
                        });
                    }
                    None if matches!(literal, TextLiteral::Table { .. })
                        && entry
                            .value
                            .summary()
                            .to_lowercase()
                            .contains(&options.find.to_lowercase()) =>
                    {
                        left.push(format!(
                            "{label} shows a string table's entry, which the table's own package holds"
                        ));
                    }
                    None => {}
                }
            }
            PropertyValue::Struct { fields, .. } | PropertyValue::Default { fields, .. } => {
                replace_entries(fields, &label, options, edits, changes, left);
            }
            PropertyValue::Array { items } => {
                for (index, item) in items.iter().enumerate() {
                    match item {
                        PropertyValue::Str { value } if options.paths || !is_path(value) => {
                            if let Some(new) = replace_in(value, options) {
                                edits.values.push(edit(EditOp::SetElement {
                                    index: index as u32,
                                    text: new.clone(),
                                }));
                                changes.push(Replaced {
                                    label: format!("{label}[{index}]"),
                                    before: value.clone(),
                                    after: new,
                                });
                            }
                        }
                        PropertyValue::Struct { fields, .. } => replace_entries(
                            fields,
                            &format!("{label}[{index}]"),
                            options,
                            edits,
                            changes,
                            left,
                        ),
                        _ => {}
                    }
                }
            }
            PropertyValue::Map { entries: pairs } => {
                for (index, pair) in pairs.iter().enumerate() {
                    if let PropertyValue::Str { value } = &pair.key
                        && (options.paths || !is_path(value))
                        && let Some(new) = replace_in(value, options)
                    {
                        edits.values.push(edit(EditOp::SetKey {
                            index: index as u32,
                            text: new.clone(),
                        }));
                        changes.push(Replaced {
                            label: format!("{label} key {index}"),
                            before: value.clone(),
                            after: new,
                        });
                    }
                    match &pair.value {
                        PropertyValue::Struct { fields, .. } => replace_entries(
                            fields,
                            &format!("{label}[{index}]"),
                            options,
                            edits,
                            changes,
                            left,
                        ),
                        PropertyValue::Str { value } if replace_in(value, options).is_some() => {
                            left.push(format!(
                                "{label}[{index}] is a string a map holds, which an edit cannot reach yet"
                            ));
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

/// Replaces text across every package of the mod at `request.container`, writing the result back
/// into it in one write.
pub fn replace_in_mod(
    request: &AssetEditRequest<'_>,
    mappings: Option<&Mappings>,
    options: &ReplaceOptions,
    iostore: &crate::pak::iostore_out::IoStoreOptions,
    dry_run: bool,
) -> Result<ReplaceReport, String> {
    if options.find.is_empty() {
        return Err("say what to find".into());
    }
    let filter = asset::PathFilter::any(options.filters.iter().map(String::as_str));
    let (_, packages) = asset::list_packages(request.game_root, request.container)?;
    let mut report = ReplaceReport {
        packages: Vec::new(),
        warnings: Vec::new(),
        pak: None,
    };
    let mut written = Vec::new();
    for (_, entry) in &packages {
        if !filter.matches(entry) {
            continue;
        }
        let one = AssetEditRequest {
            game_root: request.game_root,
            container: request.container,
            entry,
            kind: AssetSource::Utoc,
            mod_name: request.mod_name,
            changes: Default::default(),
        };
        let (loaded, parsed) = match read_package(&one, mappings) {
            Ok(read) => read,
            Err(e) => {
                report.warnings.push(format!(
                    "{entry} could not be read, so nothing in it changed: {e}"
                ));
                continue;
            }
        };
        let (mut edits, changes, left) = package_replacements(&parsed, options);
        if changes.is_empty() {
            if !left.is_empty() {
                report.packages.push(ReplacedPackage {
                    entry: entry.clone(),
                    changes,
                    left,
                });
            }
            continue;
        }
        edits.allow_unchecked = request.changes.allow_unchecked;
        let (patched, _) = crate::asset_edit::patch_pass(
            &one,
            mappings,
            &crate::asset_edit::bundle_of(&loaded),
            rivals_uasset::Sidecars {
                bulk: loaded.bulk_data_buffer.as_deref(),
                optional_bulk: loaded.optional_bulk_data_buffer.as_deref(),
            },
            &parsed,
            &edits,
        )
        .and_then(|patched| {
            crate::asset_edit::check_load_order(&edits, &parsed, &patched.1)?;
            Ok(patched)
        })
        .map_err(|e| format!("{entry}: {e}"))?;
        report.warnings.extend(patched.notes.iter().cloned());
        written.push(crate::pak::iostore_out::PackageBytes {
            entry: entry.clone(),
            asset: patched.asset,
            exports: patched.exports,
            bulk: patched.bulk.or(loaded.bulk_data_buffer),
            optional_bulk: patched.optional_bulk.or(loaded.optional_bulk_data_buffer),
            memory_mapped_bulk: loaded.memory_mapped_bulk_data_buffer,
            shader_map_hashes: crate::asset_edit::source_shader_maps(
                &one,
                &loaded.asset_file_buffer,
            ),
        });
        report.packages.push(ReplacedPackage {
            entry: entry.clone(),
            changes,
            left,
        });
    }
    if dry_run || written.is_empty() {
        return Ok(report);
    }
    let utoc = std::path::PathBuf::from(request.container);
    let mut written = written.into_iter();
    let Some(first) = written.next() else {
        return Ok(report);
    };
    crate::pak::containers::drop_cached_store();
    let done = crate::pak::iostore_out::write_replacing_in_iostore(
        &utoc,
        crate::pak::iostore_out::PackageFiles {
            entry: &first.entry,
            asset: &first.asset,
            exports: &first.exports,
            bulk: first.bulk.as_deref(),
            optional_bulk: first.optional_bulk.as_deref(),
            memory_mapped_bulk: first.memory_mapped_bulk.as_deref(),
            shader_map_hashes: first.shader_map_hashes.clone(),
        },
        written.collect(),
        &std::collections::HashSet::new(),
        iostore,
    )?;
    report.pak = Some(done.utoc.to_string_lossy().into_owned());
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(find: &str, to: &str, ignore_case: bool) -> ReplaceOptions {
        ReplaceOptions {
            find: find.into(),
            to: to.into(),
            ignore_case,
            ..Default::default()
        }
    }

    #[test]
    fn a_match_is_replaced_in_the_case_it_was_written_in() {
        let ignoring = options("Galacta", "Sigeon", true);
        assert_eq!(
            replace_in(
                "PROJECTGALACTA and Project Galacta and galacta.log",
                &ignoring
            )
            .as_deref(),
            Some("PROJECTSIGEON and Project Sigeon and sigeon.log")
        );
        let exact = options("Galacta", "Sigeon", false);
        assert_eq!(
            replace_in("PROJECTGALACTA and Galacta", &exact).as_deref(),
            Some("PROJECTGALACTA and Sigeon")
        );
        assert_eq!(replace_in("nothing here", &exact), None);
    }

    #[test]
    fn a_whole_word_match_stands_alone() {
        let whole = ReplaceOptions {
            whole_word: true,
            ..options("Gal", "Sig", false)
        };
        assert_eq!(
            replace_in("Gal Msg UI, Galacta, [Gal]", &whole).as_deref(),
            Some("Sig Msg UI, Galacta, [Sig]")
        );
    }

    #[test]
    fn a_path_is_left_unless_asked_for() {
        assert!(is_path("/Game/Marvel/ProjectGalacta/X"));
        assert!(!is_path("Galacta_NativeKeys.txt"));
    }
}
