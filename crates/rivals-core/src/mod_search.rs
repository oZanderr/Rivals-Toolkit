//! Finds where a mod's packages name a string, a function, a variable or an object, across every
//! function's bytecode and every export's own values.

use std::path::Path;

use rivals_uasset::{AssetBundle, Mappings, ParseOptions, ParsedPackage, PropertyValue, TermKind};
use serde::Serialize;

use crate::asset::{AssetSource, PackageConverter, list_packages};
use crate::schema_synth::{self, PackageSource};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HitKind {
    String,
    Call,
    Variable,
    Object,
    /// A string or name an export stores as a property value, such as a save slot's default.
    Value,
}

impl From<TermKind> for HitKind {
    fn from(kind: TermKind) -> Self {
        match kind {
            TermKind::String => Self::String,
            TermKind::Call => Self::Call,
            TermKind::Variable => Self::Variable,
            TermKind::Object => Self::Object,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub package: String,
    /// The function whose bytecode holds the hit, or the export whose value it is.
    pub export: String,
    /// The export's index in its package, which is what a viewer opens it by.
    pub export_index: u32,
    /// The statement offset, for a hit in bytecode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,
    pub kind: HitKind,
    /// The whole term that matched: the full string, call path or variable path.
    pub term: String,
    /// The statement rendered, or `Property = value` for a stored value.
    pub line: String,
}

#[derive(Debug, Default, Serialize)]
pub struct SearchResult {
    pub hits: Vec<SearchHit>,
    /// Packages that could not be read, with why, so an empty result is not taken as proof.
    pub unreadable: Vec<(String, String)>,
}

/// Every place a package in `utoc_path` names something containing `query`, ignoring case.
/// Unversioned packages need `mappings`; any that cannot be read are listed rather than skipped
/// silently.
pub fn mod_search(
    game_root: &str,
    utoc_path: &Path,
    mappings: Option<&Mappings>,
    query: &str,
) -> Result<SearchResult, String> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Err("nothing to search for".into());
    }
    let utoc = utoc_path.to_string_lossy();
    let (store, packages) = list_packages(game_root, &utoc)?;
    let converter = PackageConverter::new(store.as_ref());
    let mut result = SearchResult::default();
    for (id, path) in &packages {
        let parsed = converter.convert(*id, path).and_then(|bundle| {
            schema_synth::parse_package_opts(
                &AssetBundle {
                    asset: &bundle.asset_file_buffer,
                    exports: &bundle.exports_file_buffer,
                },
                mappings,
                &PackageSource {
                    game_root,
                    container: &utoc,
                    entry: path,
                    kind: AssetSource::Utoc,
                },
                ParseOptions::default(),
            )
        });
        match parsed {
            Ok(parsed) => search_package(path, &parsed, &needle, &mut result.hits),
            Err(reason) => result.unreadable.push((path.clone(), reason)),
        }
    }
    Ok(result)
}

fn search_package(package: &str, parsed: &ParsedPackage, needle: &str, out: &mut Vec<SearchHit>) {
    for export in &parsed.exports {
        if let Some(script) = &export.script {
            let lines = rivals_uasset::script_lines(script);
            // One hit per statement: a call and the variable holding its result often both match,
            // and the call is the more telling of the two.
            for (statement, line) in script.statements.iter().zip(&lines) {
                let Some(term) = rivals_uasset::statement_terms(&statement.expr)
                    .into_iter()
                    .filter(|t| t.text.to_lowercase().contains(needle))
                    .min_by_key(|t| match t.kind {
                        TermKind::Call => 0,
                        TermKind::String => 1,
                        TermKind::Object => 2,
                        TermKind::Variable => 3,
                    })
                else {
                    continue;
                };
                out.push(SearchHit {
                    package: package.to_string(),
                    export: export.object_name.clone(),
                    export_index: export.index,
                    offset: Some(statement.offset),
                    kind: term.kind.into(),
                    term: term.text,
                    line: line.text.clone(),
                });
            }
        }
        for property in &export.properties {
            let text = match &property.value {
                PropertyValue::Str { value } | PropertyValue::Name { value } => value,
                PropertyValue::Text {
                    value: Some(value), ..
                } => value,
                _ => continue,
            };
            if text.to_lowercase().contains(needle) {
                out.push(SearchHit {
                    package: package.to_string(),
                    export: export.object_name.clone(),
                    export_index: export.index,
                    offset: None,
                    kind: HitKind::Value,
                    term: text.clone(),
                    line: format!("{} = {text}", property.label()),
                });
            }
        }
    }
}
