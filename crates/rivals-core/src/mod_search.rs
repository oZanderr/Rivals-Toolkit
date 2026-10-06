//! Finds where a mod's packages name a string, a function, a variable or an object, across every
//! function's bytecode and every export's own values.

use std::path::Path;

use rivals_uasset::{
    AssetBundle, DataTable, Expr, Mappings, ParseOptions, ParsedPackage, PropertyEntry,
    PropertyValue, ScriptPrinter, StringTable, Term, TermKind,
};
use serde::{Deserialize, Serialize};

use crate::asset::{AssetSource, PackageConverter, list_packages};
use crate::schema_synth::{self, LayoutReader, PackageSource};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HitKind {
    String,
    Call,
    /// A variable read.
    Read,
    /// A variable assigned or changed in place.
    Write,
    Object,
    Name,
    Delegate,
    /// Text an export stores as a value: a string, a name, a text, an object or asset path, an
    /// enumerator, a delegate's function or a property path, at any depth, a DataTable's rows and
    /// a StringTable's entries included.
    Value,
}

impl HitKind {
    /// The kind as the JSON and the command line spell it.
    pub fn name(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Call => "call",
            Self::Read => "read",
            Self::Write => "write",
            Self::Object => "object",
            Self::Name => "name",
            Self::Delegate => "delegate",
            Self::Value => "value",
        }
    }

    /// Which of a statement's matching terms names it: a call says the most about a line, a
    /// variable the least, and a variable stored into more than one read.
    fn rank(self) -> u8 {
        match self {
            Self::Call => 0,
            Self::Delegate => 1,
            Self::String => 2,
            Self::Name => 3,
            Self::Object => 4,
            Self::Write => 5,
            Self::Read => 6,
            Self::Value => 7,
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
    /// Match the text only where it stands as a word of its own: not inside a longer name, so
    /// `Delay` finds `KismetSystemLibrary:Delay` but not `DelayUntilNextTick` or `bDelayed`.
    whole_word: bool,
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
            whole_word: false,
        })
    }

    /// The same query, matching only whole words when `whole_word` is set.
    pub fn whole_word(self, whole_word: bool) -> Self {
        Self { whole_word, ..self }
    }

    fn matches(&self, text: &str) -> bool {
        // Most text is ASCII, which is matched where it lies. Anything wider is lowercased whole,
        // since a character outside ASCII can lowercase into one inside it, as the Kelvin sign
        // does into `k`.
        if text.is_ascii() && self.needle.is_ascii() {
            return self.matches_ascii(text.as_bytes());
        }
        let text = text.to_lowercase();
        if !self.whole_word {
            return text.contains(&self.needle);
        }
        // A word is a run of letters, digits and underscores, as an identifier is.
        let word = |c: char| c.is_alphanumeric() || c == '_';
        text.match_indices(&self.needle).any(|(at, found)| {
            !text[..at].chars().next_back().is_some_and(word)
                && !text[at + found.len()..].chars().next().is_some_and(word)
        })
    }

    /// [`Query::matches`] for ASCII text and an ASCII needle, without a lowercased copy. A whole
    /// word is looked for among the same occurrences `match_indices` gives, which do not overlap.
    fn matches_ascii(&self, text: &[u8]) -> bool {
        let needle = self.needle.as_bytes();
        let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        let mut at = 0;
        while at + needle.len() <= text.len() {
            if !text[at..at + needle.len()].eq_ignore_ascii_case(needle) {
                at += 1;
                continue;
            }
            let end = at + needle.len();
            if !self.whole_word
                || (!at.checked_sub(1).is_some_and(|before| word(text[before]))
                    && !text.get(end).is_some_and(|&after| word(after)))
            {
                return true;
            }
            at = end;
        }
        false
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
            TermKind::Read => Self::Read,
            TermKind::Write => Self::Write,
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
    /// The container a game-wide search read the package from, which is where it opens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    /// The enabled mod whose copy of the package the game loads, for a game-wide search.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_mod: Option<String>,
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
    // The store holds the mod and the base game, which is where its Blueprints' layouts come from.
    let layouts = LayoutReader::through(store.as_ref(), &converter);
    let mut result = SearchResult::default();
    for (id, path) in &packages {
        let parsed = converter.convert(*id, path).and_then(|bundle| {
            schema_synth::parse_package_through(
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
                    mod_container: None,
                },
                &layouts,
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
                        container: None,
                        in_mod: None,
                    });
                }
            }
        }
        if !query.values {
            continue;
        }
        let tables = (export.data_table.as_ref(), export.string_table.as_ref());
        let lists = [&export.properties[..], &export.defaults[..]];
        values_in(&lists, tables, query, &mut |at, text| {
            out.push(SearchHit {
                package: package.to_string(),
                export: export.object_name.clone(),
                export_index: export.index,
                offset: None,
                kind: HitKind::Value,
                term: text.to_string(),
                line: format!("{at} = {text}"),
                container: None,
                in_mod: None,
            });
        });
    }
}

/// Every stored value `query` matches, with where it sits: a property by its path through
/// structs (`A.B`), elements (`A[2]`) and map pairs (`A[Key]`), a DataTable row's value under the
/// row's name, and a StringTable entry under its key.
fn values_in(
    lists: &[&[PropertyEntry]],
    (table, strings): (Option<&DataTable>, Option<&StringTable>),
    query: &Query,
    hit: &mut dyn FnMut(&str, &str),
) {
    let mut path = Vec::new();
    for list in lists {
        walk_entries(list, &mut path, query, hit);
    }
    for row in table.map_or(&[][..], |table| &table.rows) {
        path.push(Step::Row(&row.name));
        walk_entries(&row.fields, &mut path, query, hit);
        path.pop();
    }
    for entry in strings.map_or(&[][..], |strings| &strings.entries) {
        for text in [&entry.key, &entry.source] {
            if query.matches(text) {
                hit(&entry.key, text);
            }
        }
    }
}

/// One step on the way to a value, kept as what it borrows: most values match nothing, so the
/// path is only written out for one that does.
enum Step<'a> {
    Field(&'a PropertyEntry),
    Row(&'a str),
    Index(usize),
    Key(&'a PropertyValue),
}

/// `A.B`, `A[2]`, `A[Key]`, with a DataTable row's name first.
fn path_text(path: &[Step<'_>]) -> String {
    let mut out = String::new();
    for step in path {
        match step {
            Step::Field(entry) => {
                if !out.is_empty() {
                    out.push('.');
                }
                out.push_str(&entry.label());
            }
            Step::Row(name) => out.push_str(name),
            Step::Index(index) => out.push_str(&format!("[{index}]")),
            Step::Key(key) => out.push_str(&format!("[{}]", key.summary())),
        }
    }
    out
}

fn walk_entries<'a>(
    entries: &'a [PropertyEntry],
    path: &mut Vec<Step<'a>>,
    query: &Query,
    hit: &mut dyn FnMut(&str, &str),
) {
    for entry in entries {
        path.push(Step::Field(entry));
        walk_value(&entry.value, path, query, hit);
        path.pop();
    }
}

/// One value: its own text, when it has text, then whatever it holds. Numbers are never searched,
/// and neither is a value that is not stored.
fn walk_value<'a>(
    value: &'a PropertyValue,
    path: &mut Vec<Step<'a>>,
    query: &Query,
    hit: &mut dyn FnMut(&str, &str),
) {
    let text = match value {
        PropertyValue::Str { value } | PropertyValue::Name { value } => Some(value.as_str()),
        // A text built from parts shows what they make; one that matches is found once, as
        // itself, rather than once more through each part.
        PropertyValue::Text { value, parts, .. } => match value.as_deref() {
            Some(shown) if query.matches(shown) => Some(shown),
            _ => {
                walk_entries(parts, path, query, hit);
                None
            }
        },
        PropertyValue::Object {
            path: Some(path), ..
        }
        | PropertyValue::SoftObject { path }
        | PropertyValue::FieldPath { path, .. } => Some(path.as_str()),
        PropertyValue::Enum {
            name: Some(name), ..
        } => Some(name.as_str()),
        PropertyValue::Delegate { function, .. } => Some(function.as_str()),
        PropertyValue::Struct { fields, .. } => {
            walk_entries(fields, path, query, hit);
            None
        }
        PropertyValue::Array { items } | PropertyValue::Set { items } => {
            for (index, item) in items.iter().enumerate() {
                path.push(Step::Index(index));
                walk_value(item, path, query, hit);
                path.pop();
            }
            None
        }
        PropertyValue::Map { entries } => {
            for pair in entries {
                path.push(Step::Key(&pair.key));
                walk_value(&pair.key, path, query, hit);
                walk_value(&pair.value, path, query, hit);
                path.pop();
            }
            None
        }
        _ => None,
    };
    if let Some(text) = text
        && query.matches(text)
    {
        hit(&path_text(path), text);
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use rivals_uasset::{ObjectRef, PropertyRef};

    /// A whole word is bounded by anything but letters, digits and underscores, so a path's
    /// separators bound it and a longer name does not.
    #[test]
    fn a_whole_word_stands_apart_from_longer_names() {
        let query = Query::new("Delay", Vec::new(), false)
            .unwrap()
            .whole_word(true);
        assert!(query.matches("/Script/Engine.KismetSystemLibrary:Delay"));
        assert!(query.matches("delay"));
        assert!(query.matches("Delay or DelayUntilNextTick"));
        assert!(!query.matches("DelayUntilNextTick"));
        assert!(!query.matches("bDelayed"));
        assert!(!query.matches("CallFunc_Delay_ReturnValue"));
        let anywhere = Query::new("Delay", Vec::new(), false).unwrap();
        assert!(anywhere.matches("bDelayed"));
    }

    /// ASCII text is matched where it lies and wider text after lowercasing it, with the same
    /// answers either way, a character that lowercases into ASCII included.
    #[test]
    fn ascii_and_wider_text_match_alike() {
        let hulk = Query::new("Hulk", Vec::new(), false).unwrap();
        assert!(hulk.matches("THE HULK"));
        assert!(hulk.matches("Über Hulk"));
        assert!(!hulk.matches("Hul"));
        assert!(!hulk.matches("Über Hul"));
        let kelvin = Query::new("kelvin", Vec::new(), false).unwrap();
        assert!(kelvin.matches("\u{212A}elvin"));
        let word = Query::new("Hulk", Vec::new(), false)
            .unwrap()
            .whole_word(true);
        assert!(word.matches("/Game/Hulk.Hulk_C"));
        assert!(word.matches("Über Hulk"));
        assert!(!word.matches("Hulk_Body"));
        assert!(!word.matches("SmartHulk"));
        // The occurrences looked at do not overlap, as `match_indices` finds them, so the second
        // `a.a`, which would stand alone, is never reached either way.
        let dotted = Query::new("a.a", Vec::new(), false)
            .unwrap()
            .whole_word(true);
        assert!(!dotted.matches("xa.a.a"));
        assert!(!dotted.matches("\u{e9}xa.a.a"));
    }

    #[test]
    fn an_empty_query_is_refused() {
        assert!(Query::new("  ", Vec::new(), false).is_err());
    }

    /// `Let Count = Helper(Count)`: a call says more about the line than the variable, unless
    /// only variables are asked for, and the line both writes the variable and reads it.
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
        let writes = Query::new("HELPER", vec![HitKind::Write], false).unwrap();
        let best = writes.best_term(&expr).expect("a hit");
        assert_eq!(
            (best.kind, best.text.as_str()),
            (TermKind::Write, "CountHelper")
        );
        let reads = Query::new("HELPER", vec![HitKind::Read], false).unwrap();
        let best = reads.best_term(&expr).expect("a hit");
        assert_eq!(
            (best.kind, best.text.as_str()),
            (TermKind::Read, "CountHelper")
        );
        let names = Query::new("helper", vec![HitKind::Name], false).unwrap();
        assert!(names.best_term(&expr).is_none());
    }

    fn field(name: &str, value: PropertyValue) -> PropertyEntry {
        PropertyEntry {
            name: name.into(),
            element: None,
            value,
            span: None,
            slot: None,
        }
    }

    fn text(value: &str) -> PropertyValue {
        PropertyValue::Str {
            value: value.into(),
        }
    }

    /// Every value `query` finds in `lists` and the tables, as `where = text`.
    fn found(
        lists: &[&[PropertyEntry]],
        tables: (Option<&DataTable>, Option<&StringTable>),
        query: &str,
    ) -> Vec<String> {
        let query = Query::new(query, Vec::new(), true).unwrap();
        let mut out = Vec::new();
        values_in(lists, tables, &query, &mut |at, text| {
            out.push(format!("{at} = {text}"));
        });
        out
    }

    #[test]
    fn a_value_nested_in_structs_arrays_and_maps_is_found_by_its_path() {
        let settings = field(
            "Settings",
            PropertyValue::Struct {
                name: "SaveSettings".into(),
                fields: vec![
                    field(
                        "Files",
                        PropertyValue::Array {
                            items: vec![PropertyValue::Struct {
                                name: "SaveFile".into(),
                                fields: vec![field("Name", text("Keys.txt"))],
                            }],
                        },
                    ),
                    field(
                        "Slots",
                        PropertyValue::Map {
                            entries: vec![rivals_uasset::MapEntry {
                                key: PropertyValue::Name {
                                    value: "Main".into(),
                                },
                                value: text("keys_backup.txt"),
                            }],
                        },
                    ),
                ],
            },
        );
        assert_eq!(
            found(&[&[settings]], (None, None), "KEYS"),
            [
                "Settings.Files[0].Name = Keys.txt",
                "Settings.Slots[Main] = keys_backup.txt"
            ]
        );
    }

    #[test]
    fn object_soft_enum_delegate_and_field_path_values_are_searched() {
        let values = [
            field(
                "Mesh",
                PropertyValue::Object {
                    index: -1,
                    path: Some("/Game/Hulk/Mesh.Mesh".into()),
                },
            ),
            field(
                "Icon",
                PropertyValue::SoftObject {
                    path: "/Game/Hulk/Icon.Icon".into(),
                },
            ),
            field(
                "Role",
                PropertyValue::Enum {
                    value: 1,
                    name: Some("ERole::Hulk".into()),
                    enum_type: None,
                },
            ),
            field(
                "OnSmash",
                PropertyValue::Delegate {
                    object: None,
                    function: "HulkSmash".into(),
                },
            ),
            field(
                "Watched",
                PropertyValue::FieldPath {
                    path: "HulkRage".into(),
                    owner: None,
                },
            ),
        ];
        assert_eq!(found(&[&values], (None, None), "hulk").len(), 5);
    }

    #[test]
    fn a_data_table_row_and_a_string_table_entry_are_searched() {
        let table = DataTable {
            row_struct: "HeroRow".into(),
            columns: vec!["Title".into()],
            rows: vec![rivals_uasset::DataTableRow {
                name: "Hero_1011".into(),
                fields: vec![field("Title", text("Bruce Banner"))],
            }],
            declared_rows: 1,
            truncated: None,
        };
        let strings = StringTable {
            namespace: "Heroes".into(),
            entries: vec![rivals_uasset::StringTableEntry {
                key: "Hero_Banner".into(),
                source: "The Hulk".into(),
                tag: String::new(),
                metadata: Vec::new(),
            }],
            loose_metadata: Vec::new(),
        };
        assert_eq!(
            found(&[], (Some(&table), Some(&strings)), "banner"),
            [
                "Hero_1011.Title = Bruce Banner",
                "Hero_Banner = Hero_Banner"
            ]
        );
    }

    #[test]
    fn a_number_is_never_a_hit() {
        let values = [
            field("Count", PropertyValue::Int { value: 1011 }),
            field("Rate", PropertyValue::Float { value: 1011.0 }),
        ];
        assert!(found(&[&values], (None, None), "1011").is_empty());
    }
}
