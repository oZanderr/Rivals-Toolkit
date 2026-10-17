//! Finds where a mod's packages name a string, a function, a variable or an object, across every
//! function's bytecode and every export's own values.

use std::path::Path;

use rivals_uasset::{
    AssetBundle, DataTable, Expr, Mappings, ParseOptions, ParsedExport, ParsedPackage,
    PropertyEntry, PropertyValue, ScriptPrinter, StringTable, Term, TermKind,
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
    /// A delegate, bound or broadcast: asked for, it stands for both.
    Delegate,
    /// Who listens: a function bound to a delegate, or the delegate it joins.
    Bind,
    /// Who fires: a multicast delegate broadcast, by its signature or its delegate.
    Broadcast,
    /// Text an export stores as a value: a string, a name, a text, an object or asset path, an
    /// enumerator, a delegate's function or a property path, at any depth, a DataTable's rows and
    /// a StringTable's entries included.
    Value,
    /// A stored value found by its field's name, showing what it holds: numbers included.
    Field,
    /// An entry of the package's name table, used or not: what a name only the table still holds
    /// is found by.
    NameMap,
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
            Self::Bind => "bind",
            Self::Broadcast => "broadcast",
            Self::Value => "value",
            Self::Field => "field",
            Self::NameMap => "name_map",
        }
    }

    /// Which of a statement's matching terms names it: a call says the most about a line, a
    /// variable the least, and a variable stored into more than one read.
    fn rank(self) -> u8 {
        match self {
            Self::Call => 0,
            Self::Delegate | Self::Bind | Self::Broadcast => 1,
            Self::String => 2,
            Self::Name => 3,
            Self::Object => 4,
            Self::Write => 5,
            Self::Read => 6,
            Self::Value => 7,
            Self::Field => 8,
            Self::NameMap => 9,
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
    /// With `values`, also find a stored value by its field's name, showing what it holds.
    fields: bool,
    /// Leave out a package's paths to its own objects where the package's own name is what
    /// matched, so a search for an id finds the packages that name it and not the one it is.
    skip_self: bool,
}

impl Query {
    pub fn new(text: &str, mut kinds: Vec<HitKind>, values: bool) -> Result<Self, String> {
        let needle = text.trim().to_lowercase();
        if needle.is_empty() {
            return Err("nothing to search for".into());
        }
        // A delegate is bound or broadcast, and asking for delegates asks for both.
        if kinds.contains(&HitKind::Delegate) {
            kinds.extend([HitKind::Bind, HitKind::Broadcast]);
        }
        Ok(Self {
            needle,
            kinds,
            values,
            whole_word: false,
            fields: false,
            skip_self: false,
        })
    }

    /// The same query, matching only whole words when `whole_word` is set.
    pub fn whole_word(self, whole_word: bool) -> Self {
        Self { whole_word, ..self }
    }

    /// The same query, finding stored values by their fields' names as well when `fields` is set.
    pub fn fields(self, fields: bool) -> Self {
        Self { fields, ..self }
    }

    /// The same query, leaving out a package's paths to its own objects when `skip_self` is set.
    pub fn skip_self(self, skip_self: bool) -> Self {
        Self { skip_self, ..self }
    }

    /// Whether hits of `kind` are asked for: every kind when none is named.
    fn wants(&self, kind: HitKind) -> bool {
        self.kinds.is_empty() || self.kinds.contains(&kind)
    }

    /// Whether `term`, found in the package named `own`, is a path to the package's own objects
    /// that matched through the package's name.
    fn names_itself(&self, own: &str, term: &str) -> bool {
        self.skip_self
            && self.matches(own)
            && term.len() >= own.len()
            && term.is_char_boundary(own.len())
            && term[..own.len()].eq_ignore_ascii_case(own)
            && matches!(term[own.len()..].chars().next(), None | Some('.' | ':'))
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
            .filter(|term| self.wants(HitKind::from(term.kind)) && self.matches(&term.text))
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
            TermKind::Bind => Self::Bind,
            TermKind::Broadcast => Self::Broadcast,
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
    /// The call a call hit is found by, with its arguments, as its statement reads it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call: Option<String>,
    /// The container a game-wide search read the package from, which is where it opens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    /// The enabled mod whose copy of the package the game loads, for a game-wide search.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_mod: Option<String>,
    /// The DataTable row or StringTable entry a stored value is in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row: Option<String>,
    /// A stored value's path from its row or export: `A.B`, `A[2]`, `A[Key]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// The row's identifying field and its value, as `GamePlayModeId 600`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The hero and ability the package belongs to, and which version of it, for a game-wide
    /// search: from its path, or a Python class's module it names.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub place: Option<crate::ability_ids::AbilityPlace>,
}

impl SearchHit {
    fn new(
        package: &str,
        export: &ParsedExport,
        kind: HitKind,
        term: String,
        line: String,
    ) -> Self {
        Self {
            package: package.to_string(),
            export: export.object_name.clone(),
            export_index: export.index,
            offset: None,
            kind,
            term,
            line,
            call: None,
            container: None,
            in_mod: None,
            row: None,
            field: None,
            id: None,
            place: None,
        }
    }
}

#[derive(Debug, Default, Serialize)]
pub struct SearchResult {
    pub hits: Vec<SearchHit>,
    /// Packages that could not be read, with why, so an empty result is not taken as proof.
    pub unreadable: Vec<(String, String)>,
}

/// What a mod search looks in beyond scripts and stored values.
#[derive(Debug, Clone, Copy, Default)]
pub struct SearchOptions {
    /// Each package's name table too, where a name nothing uses any more is still found.
    pub names: bool,
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
    mod_search_with(
        game_root,
        utoc_path,
        mappings,
        query,
        SearchOptions::default(),
    )
}

/// [`mod_search`], looking where `options` say as well.
pub fn mod_search_with(
    game_root: &str,
    utoc_path: &Path,
    mappings: Option<&Mappings>,
    query: &str,
    options: SearchOptions,
) -> Result<SearchResult, String> {
    let query = Query::new(query, Vec::new(), true)?;
    let utoc = utoc_path.to_string_lossy();
    let (store, packages) = list_packages(game_root, &utoc)?;
    let converter = PackageConverter::new(store.as_ref());
    // The store holds the mod and the base game, which is where its Blueprints' layouts come from.
    let layouts = LayoutReader::through(store.as_ref(), &converter);
    let mut result = SearchResult::default();
    for (id, path) in &packages {
        let loaded = match converter.convert(*id, path) {
            Ok(loaded) => loaded,
            Err(reason) => {
                result.unreadable.push((path.clone(), reason));
                continue;
            }
        };
        let bundle = AssetBundle {
            asset: &loaded.asset_file_buffer,
            exports: &loaded.exports_file_buffer,
        };
        let parsed = schema_synth::parse_package_through(
            &bundle,
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
        );
        match parsed {
            Ok(parsed) => {
                search_package(path, &parsed, &query, &mut result.hits);
                if options.names {
                    name_map_hits(path, &bundle, &parsed, &query, &mut result.hits);
                }
            }
            Err(reason) => result.unreadable.push((path.clone(), reason)),
        }
    }
    Ok(result)
}

/// Each entry of the package's name table holding what `query` looks for, said to be unused when
/// nothing in the package uses it, or why that cannot be known.
fn name_map_hits(
    package: &str,
    bundle: &AssetBundle<'_>,
    parsed: &ParsedPackage,
    query: &Query,
    out: &mut Vec<SearchHit>,
) {
    let Ok(header) = rivals_uasset::read_header(bundle) else {
        return;
    };
    let names: Vec<&String> = header
        .name_map
        .raw_names()
        .iter()
        .filter(|name| query.matches(name))
        .collect();
    if names.is_empty() {
        return;
    }
    let unused = rivals_uasset::header_size(bundle)
        .and_then(|total| rivals_uasset::unused_names(parsed, &header, bundle.exports, total));
    for name in names {
        let state = match &unused {
            Ok(unused) if unused.contains(name) => " (unused)".to_string(),
            Ok(_) => String::new(),
            Err(reason) => format!(" (use not known: {reason})"),
        };
        out.push(SearchHit {
            package: package.to_string(),
            export: "(name map)".into(),
            export_index: 0,
            offset: None,
            kind: HitKind::NameMap,
            term: name.clone(),
            line: format!("{name}{state}"),
            call: None,
            container: None,
            in_mod: None,
            row: None,
            field: None,
            id: None,
            place: None,
        });
    }
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
    let own = parsed.info.package_name.as_str();
    for export in &parsed.exports {
        if let Some(script) = &export.script {
            // One hit per statement: a call and the variable holding its result often both match,
            // and the call is the more telling of the two.
            let hits: Vec<(usize, Term)> = script
                .statements
                .iter()
                .enumerate()
                .filter_map(|(at, statement)| query.best_term(&statement.expr).map(|t| (at, t)))
                .filter(|(_, term)| !query.names_itself(own, &term.text))
                .collect();
            if !hits.is_empty() {
                let lines = printer
                    .get_or_insert_with(|| ScriptPrinter::new(parsed))
                    .print(export.index)
                    .map(|text| text.lines)
                    .unwrap_or_default();
                let calls = hits
                    .iter()
                    .any(|(_, term)| term.kind == TermKind::Call)
                    .then(|| rivals_uasset::CallTexts::new(script));
                for (at, term) in hits {
                    let statement = &script.statements[at];
                    let line = lines.get(at).map_or_else(
                        || rivals_uasset::print_expr(&statement.expr),
                        |line| line.text.clone(),
                    );
                    let call = match (&calls, term.kind) {
                        (Some(calls), TermKind::Call) => calls.text(at, &term.text, lines.get(at)),
                        _ => None,
                    };
                    let mut hit =
                        SearchHit::new(package, export, term.kind.into(), term.text, line);
                    hit.offset = Some(statement.offset);
                    hit.call = call;
                    out.push(hit);
                }
            }
        }
        if !query.values {
            continue;
        }
        let tables = (export.data_table.as_ref(), export.string_table.as_ref());
        let lists = [&export.properties[..], &export.defaults[..]];
        values_in(&lists, tables, query, &mut |found| {
            if query.names_itself(own, &found.term) {
                return;
            }
            let line = found.line();
            let mut hit = SearchHit::new(package, export, found.kind, found.term, line);
            hit.row = found.row;
            hit.field = found.field;
            hit.id = found.id;
            out.push(hit);
        });
    }
}

/// A stored value a search finds, with where it sits.
struct Found {
    kind: HitKind,
    /// The DataTable row or StringTable entry it is in.
    row: Option<String>,
    /// Its path from the row or the export.
    field: Option<String>,
    /// The row's identifying field and its value.
    id: Option<String>,
    term: String,
    /// What it holds, as the line shows it.
    shown: String,
}

impl Found {
    /// `where = what`: the row and the path through it, then the value.
    fn line(&self) -> String {
        let at = match (&self.row, &self.field) {
            (Some(row), Some(field)) => format!("{row}.{field}"),
            (Some(row), None) => row.clone(),
            (None, Some(field)) => field.clone(),
            (None, None) => String::new(),
        };
        format!("{at} = {}", self.shown)
    }
}

/// The row a walk is inside, with its identifying field.
#[derive(Clone, Copy, Default)]
struct Place<'a> {
    row: Option<&'a str>,
    id: Option<&'a str>,
}

impl Place<'_> {
    fn found(&self, kind: HitKind, field: Option<String>, term: &str, shown: String) -> Found {
        Found {
            kind,
            row: self.row.map(str::to_string),
            field,
            id: self.id.map(str::to_string),
            term: term.to_string(),
            shown,
        }
    }
}

/// Every stored value `query` matches, with where it sits: a property by its path through
/// structs (`A.B`), elements (`A[2]`) and map pairs (`A[Key]`), a DataTable row's value under the
/// row and the row by its name, and a StringTable entry under its key.
fn values_in(
    lists: &[&[PropertyEntry]],
    (table, strings): (Option<&DataTable>, Option<&StringTable>),
    query: &Query,
    hit: &mut dyn FnMut(Found),
) {
    let mut path = Vec::new();
    for list in lists {
        walk_entries(list, Place::default(), &mut path, query, hit);
    }
    for row in table.map_or(&[][..], |table| &table.rows) {
        let id = row_id(&row.fields);
        let place = Place {
            row: Some(&row.name),
            id: id.as_deref(),
        };
        if query.wants(HitKind::Value) && query.matches(&row.name) {
            hit(place.found(HitKind::Value, None, &row.name, row.name.clone()));
        }
        walk_entries(&row.fields, place, &mut path, query, hit);
    }
    for entry in strings.map_or(&[][..], |strings| &strings.entries) {
        let place = Place {
            row: Some(&entry.key),
            id: None,
        };
        for text in [&entry.key, &entry.source] {
            if query.wants(HitKind::Value) && query.matches(text) {
                hit(place.found(HitKind::Value, None, text, text.clone()));
            }
        }
    }
}

/// A row's identifying field, as `GamePlayModeId 600`: its first field, which every row stores in
/// schema order, when the field's name says it is an id. One left at its default reads as 0.
fn row_id(fields: &[PropertyEntry]) -> Option<String> {
    let first = fields.first()?;
    let name = shown_name(&first.name);
    if !(name.ends_with("Id") || name.ends_with("ID")) {
        return None;
    }
    let value = match &first.value {
        PropertyValue::Default { .. } => "0".to_string(),
        PropertyValue::Struct { .. }
        | PropertyValue::Array { .. }
        | PropertyValue::Set { .. }
        | PropertyValue::Map { .. }
        | PropertyValue::Unset { .. }
        | PropertyValue::Undecoded { .. } => return None,
        other => other.summary(),
    };
    Some(format!("{name} {value}"))
}

/// A field's name as the editor shows it: a Blueprint struct member without the `_<n>_<GUID>` it
/// carries.
fn shown_name(name: &str) -> &str {
    let mut parts = name.rsplitn(3, '_');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(guid), Some(number), Some(shown))
            if guid.len() == 32
                && guid.chars().all(|c| c.is_ascii_hexdigit())
                && number.chars().all(|c| c.is_ascii_digit()) =>
        {
            shown
        }
        _ => name,
    }
}

/// What a value holds, kept short: a struct, an array or a map by its size alone, since a field
/// found by its name can hold any of them, and every element of an array is a field of its own.
pub(crate) fn brief(value: &PropertyValue) -> String {
    /// Enough of a long value to tell it by.
    const LONGEST: usize = 120;
    let capped = |text: String| match text.char_indices().nth(LONGEST) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text,
    };
    match value {
        PropertyValue::Struct { .. } => "{…}".to_string(),
        // A multicast delegate's bindings say what handles it, which its count would not.
        PropertyValue::Array { items }
            if !items.is_empty()
                && items
                    .iter()
                    .all(|item| matches!(item, PropertyValue::Delegate { .. })) =>
        {
            capped(items.iter().map(bound).collect::<Vec<_>>().join(", "))
        }
        PropertyValue::Array { items } | PropertyValue::Set { items } => {
            format!("[{} items]", items.len())
        }
        PropertyValue::Map { entries } => format!("{{{} pairs}}", entries.len()),
        PropertyValue::Delegate { .. } => capped(bound(value)),
        other => capped(other.summary()),
    }
}

/// A delegate as `Object::Function`, the object by its own name.
fn bound(value: &PropertyValue) -> String {
    match value {
        PropertyValue::Delegate {
            object: Some(object),
            function,
        } => {
            let name = object.rsplit([':', '.']).next().unwrap_or(object);
            format!("{name}::{function}")
        }
        PropertyValue::Delegate { function, .. } => function.clone(),
        other => other.summary(),
    }
}

/// One step on the way to a value, kept as what it borrows: most values match nothing, so the
/// path is only written out for one that does.
enum Step<'a> {
    Field(&'a PropertyEntry),
    Index(usize),
    Key(&'a PropertyValue),
}

/// `A.B`, `A[2]`, `A[Key]`.
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
            Step::Index(index) => out.push_str(&format!("[{index}]")),
            Step::Key(key) => out.push_str(&format!("[{}]", key.summary())),
        }
    }
    out
}

/// Each entry's value, then the entry itself by its name when the query finds fields by name and
/// the value was not already found as itself.
fn walk_entries<'a>(
    entries: &'a [PropertyEntry],
    place: Place<'_>,
    path: &mut Vec<Step<'a>>,
    query: &Query,
    hit: &mut dyn FnMut(Found),
) {
    for entry in entries {
        path.push(Step::Field(entry));
        let found_itself = walk_value(&entry.value, place, path, query, hit);
        if query.fields
            && !found_itself
            && query.wants(HitKind::Field)
            && query.matches(&entry.name)
        {
            hit(place.found(
                HitKind::Field,
                Some(path_text(path)),
                &entry.name,
                brief(&entry.value),
            ));
        }
        path.pop();
    }
}

/// One value: its own text, when it has text, then whatever it holds. Numbers are never searched
/// as text, and neither is a value that is not stored. Whether the value's own text was found.
fn walk_value<'a>(
    value: &'a PropertyValue,
    place: Place<'_>,
    path: &mut Vec<Step<'a>>,
    query: &Query,
    hit: &mut dyn FnMut(Found),
) -> bool {
    let text = match value {
        PropertyValue::Str { value } | PropertyValue::Name { value } => Some(value.as_str()),
        // A text built from parts shows what they make; one that matches is found once, as
        // itself, rather than once more through each part. One that does not is still found by
        // where its translations are filed, its namespace and key, which no screen shows.
        PropertyValue::Text {
            value: shown,
            parts,
            namespace,
            key,
            ..
        } => match shown.as_deref() {
            Some(shown) if query.matches(shown) => Some(shown),
            _ => {
                let filed = [namespace, key]
                    .into_iter()
                    .flatten()
                    .find(|filed| query.matches(filed));
                if let Some(filed) = filed {
                    if query.wants(HitKind::Value) {
                        let literal = rivals_uasset::text_literal::of_value(value).map_or_else(
                            || brief(value),
                            |l| rivals_uasset::text_literal::format(&l),
                        );
                        hit(place.found(HitKind::Value, Some(path_text(path)), filed, literal));
                    }
                    return true;
                }
                walk_entries(parts, place, path, query, hit);
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
            walk_entries(fields, place, path, query, hit);
            None
        }
        PropertyValue::Array { items } | PropertyValue::Set { items } => {
            for (index, item) in items.iter().enumerate() {
                path.push(Step::Index(index));
                walk_value(item, place, path, query, hit);
                path.pop();
            }
            None
        }
        PropertyValue::Map { entries } => {
            for pair in entries {
                path.push(Step::Key(&pair.key));
                walk_value(&pair.key, place, path, query, hit);
                walk_value(&pair.value, place, path, query, hit);
                path.pop();
            }
            None
        }
        _ => None,
    };
    let Some(text) = text.filter(|text| query.matches(text)) else {
        return false;
    };
    if query.wants(HitKind::Value) {
        hit(place.found(
            HitKind::Value,
            Some(path_text(path)),
            text,
            text.to_string(),
        ));
    }
    true
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
        values_in(lists, tables, &query, &mut |found| out.push(found.line()));
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

    /// Everything `query` finds in `lists` and the tables, as found.
    fn finds(
        lists: &[&[PropertyEntry]],
        tables: (Option<&DataTable>, Option<&StringTable>),
        query: &Query,
    ) -> Vec<Found> {
        let mut out = Vec::new();
        values_in(lists, tables, query, &mut |found| out.push(found));
        out
    }

    /// A match mode table's row: its id first, as every such table stores it.
    fn mode_table() -> DataTable {
        DataTable {
            row_struct: "MarvelGameMatchModeTable".into(),
            columns: vec!["GamePlayModeId".into(), "InactiveFightJudgeTime".into()],
            rows: vec![rivals_uasset::DataTableRow {
                name: "600".into(),
                fields: vec![
                    field("GamePlayModeId", PropertyValue::Int { value: 600 }),
                    field("InactiveFightJudgeTime", PropertyValue::Int { value: 600 }),
                    field("InactiveTips", text("Inactive for too long")),
                    field(
                        "MapIds",
                        PropertyValue::Array {
                            items: vec![PropertyValue::Int { value: 1032 }; 3],
                        },
                    ),
                ],
            }],
            declared_rows: 1,
            truncated: None,
        }
    }

    /// A value in a table's row says which row and field it is, and the row's id beside them; the
    /// row is found by its name too, numbers being matched there as text.
    #[test]
    fn a_table_value_names_its_row_its_field_and_the_row_s_id() {
        let table = mode_table();
        let query = Query::new("inactive for", Vec::new(), true).unwrap();
        let found = finds(&[], (Some(&table), None), &query);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].row.as_deref(), Some("600"));
        assert_eq!(found[0].field.as_deref(), Some("InactiveTips"));
        assert_eq!(found[0].id.as_deref(), Some("GamePlayModeId 600"));
        assert_eq!(found[0].line(), "600.InactiveTips = Inactive for too long");
        let by_name = Query::new("600", Vec::new(), true).unwrap();
        let rows: Vec<String> = finds(&[], (Some(&table), None), &by_name)
            .iter()
            .map(Found::line)
            .collect();
        assert_eq!(rows, ["600 = 600"]);
    }

    /// A field found by its name shows what it holds, a number as itself and a container by its
    /// size; one whose value is found as itself is found once, as the value.
    #[test]
    fn a_field_is_found_by_its_name_with_what_it_holds() {
        let table = mode_table();
        let query = |text: &str| Query::new(text, Vec::new(), true).unwrap().fields(true);
        let judge = finds(&[], (Some(&table), None), &query("InactiveFightJudgeTime"));
        assert_eq!(judge.len(), 1);
        assert_eq!(judge[0].kind, HitKind::Field);
        assert_eq!(judge[0].shown, "600");
        assert_eq!(judge[0].id.as_deref(), Some("GamePlayModeId 600"));
        let maps = finds(&[], (Some(&table), None), &query("MapIds"));
        assert_eq!(maps[0].shown, "[3 items]");
        let hover = |object: Option<&str>, function: &str| PropertyValue::Delegate {
            object: object.map(Into::into),
            function: function.into(),
        };
        let button = [field(
            "OnHovered",
            PropertyValue::Array {
                items: vec![
                    hover(Some("/Game/UI/W.W_C:WidgetTree:Btn_Reset"), "HoverText"),
                    hover(None, "OnAnyHover"),
                ],
            },
        )];
        let hovered = finds(&[&button], (None, None), &query("OnHovered"));
        assert_eq!(hovered[0].shown, "Btn_Reset::HoverText, OnAnyHover");
        let tips = finds(&[], (Some(&table), None), &query("inactive"));
        assert_eq!(
            tips.iter().map(|found| found.kind).collect::<Vec<_>>(),
            [HitKind::Field, HitKind::Value],
            "{:?}",
            tips.iter().map(Found::line).collect::<Vec<_>>()
        );
        let off = Query::new("InactiveFightJudgeTime", Vec::new(), true).unwrap();
        assert!(finds(&[], (Some(&table), None), &off).is_empty());
    }

    /// The kinds asked for hold for stored values too: a call search finds none, and `field`
    /// alone finds fields and not values.
    #[test]
    fn the_kinds_asked_for_narrow_stored_values() {
        let table = mode_table();
        let calls = Query::new("inactive", vec![HitKind::Call], true)
            .unwrap()
            .fields(true);
        assert!(finds(&[], (Some(&table), None), &calls).is_empty());
        let fields = Query::new("inactive", vec![HitKind::Field], true)
            .unwrap()
            .fields(true);
        assert!(
            finds(&[], (Some(&table), None), &fields)
                .iter()
                .all(|found| found.kind == HitKind::Field)
        );
    }

    /// A Blueprint struct member's id is told by the name the editor shows, not the one it is
    /// stored under.
    #[test]
    fn a_blueprint_member_s_id_goes_by_its_shown_name() {
        let fields = [field(
            "CardID_64_CC9CEDDF4E4198FAEAC83788C5B70386",
            PropertyValue::Default {
                declared: None,
                fields: Vec::new(),
            },
        )];
        assert_eq!(row_id(&fields).as_deref(), Some("CardID 0"));
        assert_eq!(row_id(&[field("Title", text("x"))]), None);
    }

    /// Asking for delegates finds who listens and who fires, and asking for either finds only it.
    #[test]
    fn a_delegate_search_tells_binding_from_broadcasting() {
        let delegate = || {
            Box::new(Expr::Variable {
                name: "InstanceVariable",
                property: PropertyRef {
                    names: Vec::new(),
                    path: "OnClicked".into(),
                    owner: ObjectRef {
                        index: 0,
                        path: None,
                    },
                },
            })
        };
        let added = Expr::DelegateOp {
            name: "AddMulticastDelegate",
            delegate: delegate(),
            value: Box::new(Expr::NameConst {
                name: "InstanceDelegate",
                value: "OnPressed".into(),
                at: 0,
                id: Default::default(),
            }),
        };
        let fired = Expr::CallMulticastDelegate {
            signature: ObjectRef {
                index: -1,
                path: Some("/Game/X.X_C:OnClicked__DelegateSignature".into()),
            },
            delegate: delegate(),
            params: Vec::new(),
        };
        let kind = |kinds: Vec<HitKind>, expr: &Expr| {
            Query::new("OnClicked", kinds, false)
                .unwrap()
                .best_term(expr)
                .map(|term| HitKind::from(term.kind))
        };
        assert_eq!(kind(vec![HitKind::Delegate], &added), Some(HitKind::Bind));
        assert_eq!(
            kind(vec![HitKind::Delegate], &fired),
            Some(HitKind::Broadcast)
        );
        assert_eq!(kind(vec![HitKind::Bind], &fired), None);
        assert_eq!(kind(vec![HitKind::Broadcast], &added), None);
        assert_eq!(kind(Vec::new(), &added), Some(HitKind::Bind));
    }

    /// A package's paths to its own objects are left out when its own name is what matched, and
    /// kept when only the object part did or the path is another package's.
    #[test]
    fn a_package_naming_itself_is_left_out_only_through_its_own_name() {
        let own = "/Game/Ability/Summoned_10146101_BP";
        let by_id = Query::new("10146101", Vec::new(), false)
            .unwrap()
            .skip_self(true);
        assert!(by_id.names_itself(own, &format!("{own}.Summoned_10146101_BP_C")));
        assert!(by_id.names_itself(own, &format!("{own}.Summoned_10146101_BP_C:Fire")));
        assert!(!by_id.names_itself(own, "/Game/Ability/Ability_101461_BP.Summoned_10146101"));
        let by_function = Query::new("Fire", Vec::new(), false)
            .unwrap()
            .skip_self(true);
        assert!(!by_function.names_itself(own, &format!("{own}.Summoned_10146101_BP_C:Fire")));
        let kept = Query::new("10146101", Vec::new(), false).unwrap();
        assert!(!kept.names_itself(own, &format!("{own}.Summoned_10146101_BP_C")));
    }

    #[test]
    fn a_number_is_never_a_hit() {
        let values = [
            field("Count", PropertyValue::Int { value: 1011 }),
            field("Rate", PropertyValue::Float { value: 1011.0 }),
        ];
        assert!(found(&[&values], (None, None), "1011").is_empty());
    }

    /// A stored text is found by what it shows, and otherwise by its namespace or key, once, shown
    /// as the literal that files it; a text inside another's parts is searched the same way.
    #[test]
    fn a_text_is_found_by_where_its_translations_are_filed() {
        let localized = |namespace: &str, key: &str, shown: &str| PropertyValue::Text {
            value: Some(shown.into()),
            parts: Vec::new(),
            namespace: Some(namespace.into()),
            key: Some(key.into()),
            display: None,
        };
        let values = [
            field(
                "EntryName",
                localized(
                    "GAL_Other_Additions",
                    "GAL_Title_EntryName",
                    "PROJECT SIGEON",
                ),
            ),
            field(
                "Upper",
                PropertyValue::Text {
                    value: Some("MODS".into()),
                    parts: vec![field(
                        "SourceText",
                        localized("GAL_Other_Additions", "GAL_Mods_EntryName", "Mods"),
                    )],
                    namespace: None,
                    key: None,
                    display: None,
                },
            ),
        ];
        assert_eq!(
            found(&[&values], (None, None), "GAL_"),
            vec![
                r#"EntryName = NSLOCTEXT("GAL_Other_Additions", "GAL_Title_EntryName", "PROJECT SIGEON")"#
                    .to_string(),
                r#"Upper.SourceText = NSLOCTEXT("GAL_Other_Additions", "GAL_Mods_EntryName", "Mods")"#
                    .to_string(),
            ]
        );
        assert_eq!(
            found(&[&values], (None, None), "sigeon"),
            vec!["EntryName = PROJECT SIGEON".to_string()],
            "a text matching by what it shows is found as itself, once"
        );
        assert!(found(&[&values], (None, None), "Additions_X").is_empty());
    }
}

/// Set `RIVALS_GAME_ROOT`, `RIVALS_USMAP` and `OODLE_LIB_PATH` to run these. Skipped otherwise.
#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod game_data_tests {
    use super::*;

    const TABLE: &str = "Marvel/Content/Marvel/Data/DataTable/UI/CardTable.uasset";

    /// A row renamed leaves its old name in the table of names with nothing using it: a search of
    /// the names finds it there, said to be unused, and a search without them does not. The new
    /// name is found there too, in use.
    #[test]
    fn a_name_only_the_name_table_holds_is_found_with_the_names() {
        let (Ok(root), Ok(usmap)) = (
            std::env::var("RIVALS_GAME_ROOT"),
            std::env::var("RIVALS_USMAP"),
        ) else {
            return;
        };
        if std::env::var_os("OODLE_LIB_PATH").is_none() {
            return;
        }
        let mappings = crate::mappings::load(Path::new(&usmap)).expect("mappings");
        let container = format!(
            "{}/MarvelGame/Marvel/Content/Paks/pakchunk0-Windows.utoc",
            root.replace('\\', "/")
        );
        let loaded =
            crate::asset::load_bundle(&root, &container, TABLE, AssetSource::Utoc).expect("load");
        let bundle = AssetBundle {
            asset: &loaded.asset_file_buffer,
            exports: &loaded.exports_file_buffer,
        };
        let source = PackageSource {
            game_root: &root,
            container: &container,
            entry: TABLE,
            kind: AssetSource::Utoc,
            mod_container: None,
        };
        let parsed = schema_synth::parse_package(&bundle, Some(&mappings), &source).expect("parse");
        let (export, old) = parsed
            .exports
            .iter()
            .find_map(|export| {
                let row = export.data_table.as_ref()?.rows.first()?;
                Some((export.index, row.name.clone()))
            })
            .expect("a row");
        const RENAMED: &str = "RivalsProbeRenamedRow";
        let patched = rivals_uasset::patch_package(
            &bundle,
            &parsed,
            &rivals_uasset::PackageEdits {
                rows: vec![rivals_uasset::RowEdit {
                    export,
                    object: None,
                    op: rivals_uasset::RowOp::Rename {
                        name: old.clone(),
                        to: RENAMED.into(),
                    },
                }],
                ..Default::default()
            },
            Some(&mappings),
        )
        .expect("rename the row");
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rivals-search-names-{stamp}"));
        std::fs::create_dir_all(&dir).expect("scratch");
        let utoc = dir.join("NamesProbe_P.utoc");
        let mut package = Some(crate::pak::iostore_out::PackageBytes {
            entry: TABLE.into(),
            asset: patched.asset,
            exports: patched.exports,
            bulk: None,
            optional_bulk: None,
            memory_mapped_bulk: None,
            shader_map_hashes: Vec::new(),
        });
        crate::pak::iostore_out::write_batch_into_iostore(
            &utoc,
            1,
            |_| package.take(),
            &crate::pak::iostore_out::IoStoreOptions {
                compression: crate::pak::profile::PackCompression::Zlib,
                ..Default::default()
            },
        )
        .expect("write the probe");

        let search = |text: &str, names: bool| {
            mod_search_with(&root, &utoc, Some(&mappings), text, SearchOptions { names })
                .expect("search")
                .hits
                .into_iter()
                .filter(|hit| hit.kind == HitKind::NameMap)
                .map(|hit| (hit.term, hit.line))
                .collect::<Vec<_>>()
        };
        let without = search(&old, false);
        let with = search(&old, true);
        let renamed = search(RENAMED, true);
        crate::pak::containers::drop_cached_store();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(without.is_empty(), "{without:?}");
        assert!(
            with.contains(&(old.clone(), format!("{old} (unused)"))),
            "{with:?}"
        );
        assert_eq!(renamed, vec![(RENAMED.to_string(), RENAMED.to_string())]);
    }
}
