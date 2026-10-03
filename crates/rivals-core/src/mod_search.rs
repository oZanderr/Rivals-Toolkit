//! Finds where a mod's packages name a string, a function, a variable or an object, across every
//! function's bytecode and every export's own values.

use std::path::Path;

use rivals_uasset::{
    AssetBundle, Expr, Mappings, ParseOptions, ParsedPackage, PropertyValue, ScriptPrinter, Term,
    TermKind,
};
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
    Name,
    Delegate,
    /// A string or name an export stores as a property value, such as a save slot's default.
    Value,
}

impl HitKind {
    /// Which of a statement's matching terms names it: a call says the most about a line, a
    /// variable the least.
    fn rank(self) -> u8 {
        match self {
            Self::Call => 0,
            Self::Delegate => 1,
            Self::String => 2,
            Self::Name => 3,
            Self::Object => 4,
            Self::Variable => 5,
            Self::Value => 6,
        }
    }
}

/// What a search looks for: text found anywhere in a term, case aside, among the kinds asked for,
/// every kind when none is, and in stored values when asked.
#[derive(Debug, Clone)]
pub struct Query {
    needle: String,
    kinds: Vec<HitKind>,
    pub values: bool,
}

impl Query {
    pub fn new(text: &str, kinds: Vec<HitKind>, values: bool) -> Result<Self, String> {
        let needle = text.trim().to_lowercase();
        if needle.is_empty() {
            return Err("nothing to search for".into());
        }
        Ok(Self {
            needle,
            kinds,
            values,
        })
    }

    fn matches(&self, text: &str) -> bool {
        text.to_lowercase().contains(&self.needle)
    }

    /// The term a statement is found by: of the ones of a kind asked for that match, the one
    /// that says the most. The kinds are filtered first, so a variable asked for is not passed
    /// over for a call on the same line.
    fn best_term(&self, expr: &Expr) -> Option<Term> {
        rivals_uasset::statement_terms(expr)
            .into_iter()
            .filter(|term| {
                let kind = HitKind::from(term.kind);
                (self.kinds.is_empty() || self.kinds.contains(&kind)) && self.matches(&term.text)
            })
            .min_by_key(|term| HitKind::from(term.kind).rank())
    }
}

impl From<TermKind> for HitKind {
    fn from(kind: TermKind) -> Self {
        match kind {
            TermKind::String => Self::String,
            TermKind::Call => Self::Call,
            TermKind::Variable => Self::Variable,
            TermKind::Object => Self::Object,
            TermKind::Name => Self::Name,
            TermKind::Delegate => Self::Delegate,
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
    let query = Query::new(query, Vec::new(), true)?;
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
            Ok(parsed) => search_package(path, &parsed, &query, &mut result.hits),
            Err(reason) => result.unreadable.push((path.clone(), reason)),
        }
    }
    Ok(result)
}

/// Every place `parsed` names what `query` looks for. Terms are cheap to collect, so a function's
/// text is printed only when one of its statements hits, against one table of the package's names
/// built the first time one does.
pub(crate) fn search_package(
    package: &str,
    parsed: &ParsedPackage,
    query: &Query,
    out: &mut Vec<SearchHit>,
) {
    let mut printer: Option<ScriptPrinter<'_>> = None;
    for export in &parsed.exports {
        if let Some(script) = &export.script {
            // One hit per statement: a call and the variable holding its result often both match,
            // and the call is the more telling of the two.
            let hits: Vec<(usize, Term)> = script
                .statements
                .iter()
                .enumerate()
                .filter_map(|(at, statement)| query.best_term(&statement.expr).map(|t| (at, t)))
                .collect();
            if !hits.is_empty() {
                let lines = printer
                    .get_or_insert_with(|| ScriptPrinter::new(parsed))
                    .print(export.index)
                    .map(|text| text.lines)
                    .unwrap_or_default();
                for (at, term) in hits {
                    let statement = &script.statements[at];
                    out.push(SearchHit {
                        package: package.to_string(),
                        export: export.object_name.clone(),
                        export_index: export.index,
                        offset: Some(statement.offset),
                        kind: term.kind.into(),
                        term: term.text,
                        line: lines.get(at).map_or_else(
                            || rivals_uasset::print_expr(&statement.expr),
                            |line| line.text.clone(),
                        ),
                    });
                }
            }
        }
        if !query.values {
            continue;
        }
        for property in &export.properties {
            let text = match &property.value {
                PropertyValue::Str { value } | PropertyValue::Name { value } => value,
                PropertyValue::Text {
                    value: Some(value), ..
                } => value,
                _ => continue,
            };
            if query.matches(text) {
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

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use rivals_uasset::{ObjectRef, PropertyRef};

    #[test]
    fn an_empty_query_is_refused() {
        assert!(Query::new("  ", Vec::new(), false).is_err());
    }

    /// `Let Count = Helper(Count)`: a call says more about the line than the variable, unless
    /// only variables are asked for.
    #[test]
    fn the_best_term_wins_and_a_kind_filter_can_pass_it_over() {
        let count = || Expr::Variable {
            name: "LocalVariable",
            property: PropertyRef {
                names: Vec::new(),
                path: "CountHelper".into(),
                owner: ObjectRef {
                    index: 0,
                    path: None,
                },
            },
        };
        let expr = Expr::Let {
            name: "Let",
            property: None,
            variable: Box::new(count()),
            value: Box::new(Expr::VirtualCall {
                name: "VirtualFunction",
                function: "Helper".into(),
                params: vec![count()],
                id: Default::default(),
            }),
        };
        let any = Query::new("helper", Vec::new(), false).unwrap();
        let best = any.best_term(&expr).expect("a hit");
        assert_eq!((best.kind, best.text.as_str()), (TermKind::Call, "Helper"));
        let variables = Query::new("HELPER", vec![HitKind::Variable], false).unwrap();
        let best = variables.best_term(&expr).expect("a hit");
        assert_eq!(best.kind, TermKind::Variable);
        let names = Query::new("helper", vec![HitKind::Name], false).unwrap();
        assert!(names.best_term(&expr).is_none());
    }
}
