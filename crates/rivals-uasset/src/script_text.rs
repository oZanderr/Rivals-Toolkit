//! The text a function's bytecode prints as and assembles from.
//!
//! One statement a line, in the order the bytes run, with a label wherever something jumps or
//! resumes. Every operand the bytes hold is written unless the text already says it: a jump names
//! a label instead of an offset, a context's skip and a switch's offsets are measured from the code
//! they cover, and a variable leaves out the object that owns it when that is the function or class
//! it plainly belongs to. The printer and the assembler share one resolver, so a short form is
//! written only where the assembler reads it back to the same bytes, and the raw form, marked `#`,
//! everywhere else.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;

use serde::Serialize;

use crate::header_edit::Tables;
use crate::kismet::{self, Expr, NameId, ObjectRef, PropertyRef, Script, Span, TextLiteral};
use crate::package::ParsedPackage;

/// A problem with assembler text, where it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    pub line: u32,
    pub column: u32,
    pub message: String,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}:{}: {}", self.line, self.column, self.message)
    }
}

/// Where a token starts: a line and a column, both counted from 1.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Pos {
    pub line: u32,
    pub column: u32,
}

impl Pos {
    pub(crate) fn error(self, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            line: self.line,
            column: self.column,
            message: message.into(),
        }
    }
}

// ----------------------------------------------------------------------------------------------
// Names and objects
// ----------------------------------------------------------------------------------------------

/// Splits a trailing `_N` into the number UE stores with a name, one higher than it prints, the
/// way the name table does when it stores one. `Rocket_04` keeps its digits: a number written with
/// a leading zero is part of the name.
pub(crate) fn split_number(name: &str) -> (&str, i32) {
    if let Some((left, right)) = name.rsplit_once('_')
        && let Ok(number) = right.parse::<i32>()
        && number >= 0
        && number.to_string() == right
        && let Some(stored) = number.checked_add(1)
    {
        return (left, stored);
    }
    (name, 0)
}

/// The names and objects a package resolves text against.
#[derive(Debug, Clone)]
pub(crate) struct Symbols {
    names: Vec<String>,
    /// Each name to the entry the name table stores it under: the last one, as the table's own
    /// lookup has it.
    lookup: HashMap<String, usize>,
    /// Each path to the first import, else export, that answers to it.
    objects: HashMap<String, i32>,
    /// What each object prints as, for the note a raw index carries.
    paths: HashMap<i32, String>,
    imports: usize,
    exports: usize,
}

impl Symbols {
    pub(crate) fn of(parsed: &ParsedPackage) -> Self {
        Self::with_names(parsed, parsed.names.clone())
    }

    /// The package's objects, with the names a save in progress has grown the table to.
    pub(crate) fn with_names(parsed: &ParsedPackage, names: Vec<String>) -> Self {
        let mut lookup = HashMap::with_capacity(names.len());
        for (index, name) in names.iter().enumerate() {
            lookup.insert(name.clone(), index);
        }
        let mut objects = HashMap::new();
        let mut paths = HashMap::new();
        for import in &parsed.imports {
            if !import.path.is_empty() {
                objects.entry(import.path.clone()).or_insert(import.index);
            }
            paths.insert(import.index, import.path.clone());
        }
        for export in &parsed.exports {
            let index = export.index as i32 + 1;
            if !export.path.is_empty() {
                objects.entry(export.path.clone()).or_insert(index);
            }
            paths.insert(index, export.path.clone());
        }
        Symbols {
            names,
            lookup,
            objects,
            paths,
            imports: parsed.imports.len(),
            exports: parsed.exports.len(),
        }
    }

    /// The entry and number the name table would store `text` as, when it has the entry.
    pub(crate) fn name_id(&self, text: &str) -> Option<NameId> {
        let (base, number) = split_number(text);
        self.lookup.get(base).map(|index| NameId {
            index: *index as i32,
            number,
        })
    }

    /// A name as UE prints it: the entry, then the number less one when there is one.
    pub(crate) fn name_text(&self, id: NameId) -> Option<String> {
        let entry = self.names.get(usize::try_from(id.index).ok()?)?;
        Some(if id.number != 0 {
            format!("{entry}_{}", i64::from(id.number) - 1)
        } else {
            entry.clone()
        })
    }

    /// The name `text` is when it is pinned to entry `index`: the entry itself, or the entry with
    /// a number after it.
    pub(crate) fn pinned(&self, index: i32, text: &str) -> Option<NameId> {
        let entry = self.names.get(usize::try_from(index).ok()?)?;
        if text == entry {
            return Some(NameId { index, number: 0 });
        }
        let rest = text.strip_prefix(entry.as_str())?.strip_prefix('_')?;
        let number: i64 = rest.parse().ok()?;
        if number.to_string() != rest {
            return None;
        }
        Some(NameId {
            index,
            number: i32::try_from(number + 1).ok()?,
        })
    }

    fn entry(&self, index: i32) -> Option<&str> {
        self.names
            .get(usize::try_from(index).ok()?)
            .map(String::as_str)
    }

    /// The object a path names: the first import that answers to it, else the first export.
    pub(crate) fn object(&self, path: &str) -> Option<i32> {
        self.objects.get(path).copied()
    }

    fn path_of(&self, index: i32) -> Option<&str> {
        self.paths
            .get(&index)
            .map(String::as_str)
            .filter(|p| !p.is_empty())
    }

    /// Whether a raw index names an import or export the package has.
    fn holds(&self, index: i32) -> bool {
        match index {
            0 => true,
            i if i < 0 => ((-i - 1) as usize) < self.imports,
            i => ((i - 1) as usize) < self.exports,
        }
    }
}

/// How the owner of a variable is written when the text leaves it out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// A parameter or local of the function: the function owns it.
    Local,
    /// A member of the class: the class owns it.
    Member,
    /// A local of the class's event graph, kept between its runs.
    Persistent,
    /// Nothing to go by: the owner is always written.
    Other,
}

fn role_of(token: &str) -> Role {
    match token {
        "LocalVariable" | "LocalOutVariable" => Role::Local,
        "InstanceVariable" | "DefaultVariable" | "ClassSparseDataVariable" => Role::Member,
        "LetValueOnPersistentFrame" => Role::Persistent,
        _ => Role::Other,
    }
}

/// What the text knows about the function it is written for.
#[derive(Debug, Clone)]
pub(crate) struct FunctionCx {
    pub(crate) name: String,
    /// The function as the package's bytecode names it.
    pub(crate) own: i32,
    /// The class the function belongs to, as the package names it.
    class: i32,
    class_name: String,
    /// The function's parameters and locals. `None` when its field records were not read, which
    /// leaves nothing to check a local against.
    locals: Option<HashSet<String>>,
    /// What the class declares itself.
    members: HashSet<String>,
    /// The class's event graph and its locals.
    persistent: Option<(i32, HashSet<String>)>,
}

impl FunctionCx {
    pub(crate) fn of(parsed: &ParsedPackage, export: u32) -> Option<Self> {
        let function = parsed.exports.get(export as usize)?;
        let names_of = |export: &crate::package::ParsedExport| {
            export.signature.as_ref().map(|signature| {
                signature
                    .params
                    .iter()
                    .chain(&signature.locals)
                    .map(|field| field.name.clone())
                    .collect::<HashSet<String>>()
            })
        };
        let class = function.outer_index;
        let class_export = usize::try_from(class - 1)
            .ok()
            .and_then(|at| parsed.exports.get(at))
            .filter(|_| class > 0);
        let members = class_export
            .and_then(|export| export.struct_definition.as_ref())
            .map(|definition| {
                definition
                    .properties
                    .iter()
                    .map(|property| property.name.clone())
                    .collect()
            })
            .unwrap_or_default();
        let persistent = parsed
            .exports
            .iter()
            .find(|other| {
                other.outer_index == class && other.object_name.starts_with("ExecuteUbergraph")
            })
            .and_then(|graph| Some((graph.index as i32 + 1, names_of(graph)?)));
        Some(FunctionCx {
            name: function.object_name.clone(),
            own: export as i32 + 1,
            class,
            class_name: class_export
                .map(|export| export.object_name.clone())
                .unwrap_or_default(),
            locals: names_of(function),
            members,
            persistent,
        })
    }

    /// Lets the text name a local the function is about to be given.
    pub(crate) fn declare(&mut self, name: &str) {
        if let Some(locals) = &mut self.locals {
            locals.insert(name.to_string());
        }
    }

    /// Whether `name` is one of the function's parameters or locals, as far as they were read.
    fn has_local(&self, name: &str) -> bool {
        self.locals
            .as_ref()
            .is_none_or(|locals| locals.contains(name))
    }

    /// The owner a variable takes when the text leaves it out, when it can take one at all.
    fn default_owner(&self, role: Role, first: &str) -> Option<i32> {
        match role {
            Role::Local => match &self.locals {
                Some(locals) if !locals.contains(first) => None,
                _ => Some(self.own),
            },
            Role::Member => (self.class != 0 && self.members.contains(first)).then_some(self.class),
            Role::Persistent => self
                .persistent
                .as_ref()
                .filter(|(_, locals)| locals.contains(first))
                .map(|(graph, _)| *graph),
            Role::Other => None,
        }
    }
}

/// The field a `Let` assigns through, which is what it names unless the text says otherwise.
fn let_field(variable: &Expr) -> Option<&PropertyRef> {
    match variable {
        Expr::Variable { property, .. } | Expr::Member { property, .. } => Some(property),
        Expr::Context { member, .. } => let_field(member),
        _ => None,
    }
}

/// The field a plain `Let` names when the text leaves it out. The compiler writes the variable
/// assigned, except for a cast, whose `Let` names nothing.
fn let_default(variable: &Expr, value: &Expr) -> Option<PropertyRef> {
    if matches!(value, Expr::Cast { .. } | Expr::Conversion { .. }) {
        return None;
    }
    let_field(variable).cloned()
}

fn is_call(expr: &Expr) -> bool {
    matches!(expr, Expr::FinalCall { .. } | Expr::VirtualCall { .. })
}

/// The value a context hands back, when the text leaves it out: its member's variable when the
/// member is one, and for a call, the variable a `Let` keeps its result in, which is `kept`.
fn rvalue_default<'a>(member: &'a Expr, kept: Option<&'a PropertyRef>) -> Option<&'a PropertyRef> {
    match member {
        Expr::Variable { property, .. } => Some(property),
        call if is_call(call) => kept,
        _ => None,
    }
}

/// The owner a context's value field takes when the text gives the field but not its owner.
fn rvalue_owner(member: &Expr) -> Option<i32> {
    match member {
        Expr::Variable { property, .. } => Some(property.owner.index),
        Expr::FinalCall { function, .. } => Some(function.index),
        _ => None,
    }
}

fn same_field(a: &PropertyRef, b: &PropertyRef) -> bool {
    a.path == b.path && a.owner.index == b.owner.index && a.names == b.names
}

fn empty_field() -> PropertyRef {
    PropertyRef {
        path: String::new(),
        owner: ObjectRef {
            index: 0,
            path: None,
        },
        names: Vec::new(),
    }
}

fn is_empty_field(field: &PropertyRef) -> bool {
    field.path.is_empty() && field.names.is_empty() && field.owner.index == 0
}

// ----------------------------------------------------------------------------------------------
// Writing the text
// ----------------------------------------------------------------------------------------------

/// Words the text gives a meaning of its own, which a name has to be quoted to be.
const RESERVED: &[&str] = &["in", "none", "None", "default", "unless"];

fn is_ident(text: &str) -> bool {
    let mut chars = text.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn is_path_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '.' | ':' | '-')
}

/// Whether a path can stand bare: it starts with `/` and holds nothing a delimiter could be
/// mistaken for.
fn is_bare_path(path: &str) -> bool {
    path.starts_with('/') && path.chars().all(is_path_char) && !path.contains("->")
}

fn escape_into(out: &mut String, c: char, quote: char, wide: bool) {
    match c {
        '\\' => out.push_str("\\\\"),
        '\n' => out.push_str("\\n"),
        '\r' => out.push_str("\\r"),
        '\t' => out.push_str("\\t"),
        c if c == quote => {
            out.push('\\');
            out.push(c);
        }
        c if u32::from(c) < 0x20 || (0x7F..=0x9F).contains(&u32::from(c)) => {
            if wide {
                out.push_str(&format!("\\u{{{:X}}}", u32::from(c)));
            } else {
                out.push_str(&format!("\\x{:02X}", u32::from(c)));
            }
        }
        c => out.push(c),
    }
}

/// A one-byte-per-character string as the text writes it.
fn quote_ansi(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        escape_into(&mut out, c, '"', false);
    }
    out.push('"');
    out
}

fn quote_unicode(value: &str) -> String {
    let mut out = String::from("u\"");
    for c in value.chars() {
        escape_into(&mut out, c, '"', true);
    }
    out.push('"');
    out
}

fn quote_name(value: &str) -> String {
    let mut out = String::from("'");
    for c in value.chars() {
        escape_into(&mut out, c, '\'', true);
    }
    out.push('\'');
    out
}

fn f64_text(value: f64) -> String {
    if value.is_nan() {
        format!("nan(0x{:016X})", value.to_bits())
    } else if value.is_infinite() {
        (if value < 0.0 { "-inf" } else { "inf" }).to_string()
    } else {
        format!("{value:?}")
    }
}

fn f32_text(value: f32) -> String {
    if value.is_nan() {
        format!("nan(0x{:08X})", value.to_bits())
    } else if value.is_infinite() {
        (if value < 0.0 { "-inf" } else { "inf" }).to_string()
    } else {
        format!("{value:?}")
    }
}

fn label_name(offset: u32) -> String {
    format!("{offset:04X}")
}

/// Whether a label stands for an offset in the code as it was printed: four or more upper-case
/// hexadecimal digits, which is how the printer names every label.
pub(crate) fn original_offset(label: &str) -> Option<u32> {
    (label.len() >= 4
        && label
            .chars()
            .all(|c| c.is_ascii_digit() || ('A'..='F').contains(&c)))
    .then(|| u32::from_str_radix(label, 16).ok())
    .flatten()
}

/// A label line the printer writes before a statement, with what enters there from outside.
#[derive(Debug, Clone, Serialize)]
pub struct TextLabel {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One statement as the text has it.
#[derive(Debug, Clone, Serialize)]
pub struct TextLine {
    /// The loaded offset the statement starts at.
    pub offset: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<TextLabel>,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Where each expression the statement holds sits in `text`, by the loaded offset it starts
    /// at: a byte range.
    #[serde(skip)]
    pub ranges: Vec<(u32, (usize, usize))>,
}

/// A function's script as text.
#[derive(Debug, Clone, Serialize)]
pub struct ScriptText {
    pub lines: Vec<TextLine>,
    /// Labels at the very end of the script, which is where a jump past the last statement lands.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub end_labels: Vec<TextLabel>,
    /// Why the script stopped decoding, when it did: such a script prints, but does not assemble.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stopped: Option<String>,
}

impl ScriptText {
    /// The whole text, a label or statement a line.
    pub fn text(&self) -> String {
        let mut out = String::new();
        let label = |out: &mut String, label: &TextLabel| {
            out.push('@');
            out.push_str(&label.name);
            out.push(':');
            if let Some(note) = &label.note {
                out.push_str("  ; ");
                out.push_str(note);
            }
            out.push('\n');
        };
        for line in &self.lines {
            for each in &line.labels {
                label(&mut out, each);
            }
            out.push_str(&line.text);
            if let Some(note) = &line.note {
                out.push_str("  ; ");
                out.push_str(note);
            }
            out.push('\n');
        }
        for each in &self.end_labels {
            label(&mut out, each);
        }
        if let Some(stopped) = &self.stopped {
            out.push_str("!! ");
            out.push_str(stopped);
            out.push('\n');
        }
        out
    }
}

struct Printer<'a> {
    symbols: Option<&'a Symbols>,
    cx: Option<&'a FunctionCx>,
    spans: HashMap<*const Expr, Span>,
    /// The offsets that carry a label.
    labels: HashSet<u32>,
    /// Statement starts, whose labels the line above the statement defines.
    statement_starts: HashSet<u32>,
    /// Where a latent action's resume point counts in another function's code.
    foreign: HashSet<u64>,
    out: String,
    ranges: Vec<(u32, (usize, usize))>,
    notes: Vec<String>,
    /// The value a `Let` is about to print, and the variable it keeps it in: a call made through
    /// a context hands its result back through that variable.
    kept: Option<(*const Expr, Option<PropertyRef>)>,
}

impl Printer<'_> {
    fn detached() -> Self {
        Printer {
            symbols: None,
            cx: None,
            spans: HashMap::new(),
            labels: HashSet::new(),
            statement_starts: HashSet::new(),
            foreign: HashSet::new(),
            out: String::new(),
            ranges: Vec::new(),
            notes: Vec::new(),
            kept: None,
        }
    }

    fn push(&mut self, text: &str) {
        self.out.push_str(text);
    }

    fn note(&mut self, note: String) {
        if !self.notes.contains(&note) {
            self.notes.push(note);
        }
    }

    /// What an unresolved call probably was, as a note on its line.
    fn hint(&mut self, function: &ObjectRef, params: &[Expr], variable: Option<&Expr>) {
        if self.symbols.is_some()
            && let Some(hint) = kismet::probable_call(function, params, variable)
        {
            self.note(format!("probably {hint}"));
        }
    }

    fn span(&self, expr: &Expr) -> Option<Span> {
        self.spans.get(&(expr as *const Expr)).copied()
    }

    fn loaded_len(&self, expr: &Expr) -> Option<u32> {
        self.span(expr).map(|span| span.end_offset - span.offset)
    }

    fn target(&self, target: u32) -> String {
        if self.symbols.is_none() || self.labels.contains(&target) {
            format!("@{}", label_name(target))
        } else {
            format!("#0x{target:04X}")
        }
    }

    fn name_token(&self, text: &str, id: Option<NameId>) -> String {
        let mut out = if is_ident(text) && !RESERVED.contains(&text) {
            text.to_string()
        } else {
            quote_name(text)
        };
        self.pin(&mut out, text, id);
        out
    }

    fn name_literal(&self, text: &str, id: NameId) -> String {
        let mut out = quote_name(text);
        self.pin(&mut out, text, Some(id));
        out
    }

    fn pin(&self, out: &mut String, text: &str, id: Option<NameId>) {
        if let (Some(symbols), Some(id)) = (self.symbols, id)
            && symbols.name_id(text) != Some(id)
        {
            out.push_str(&format!("#{}", id.index));
        }
    }

    fn object(&mut self, object: &ObjectRef) -> String {
        if object.index == 0 {
            return "None".to_string();
        }
        let path = match self.symbols {
            Some(symbols) => symbols.path_of(object.index).map(str::to_string),
            None => object.path.clone(),
        };
        match (self.symbols, path) {
            (Some(symbols), Some(path)) if symbols.object(&path) == Some(object.index) => {
                path_token(&path)
            }
            (None, Some(path)) => path_token(&path),
            (_, path) => {
                if let Some(path) = path {
                    self.note(format!("#{} is {path}", object.index));
                }
                format!("#{}", object.index)
            }
        }
    }

    /// A field path, with its owner when the text would not give it that owner unasked.
    fn field(&mut self, field: &PropertyRef, default_owner: Option<i32>) {
        if is_empty_field(field) {
            self.push("none");
            return;
        }
        let segments: Vec<String> = match (self.symbols, field.names.is_empty()) {
            (Some(symbols), false) => field
                .names
                .iter()
                .map(|id| {
                    let text = symbols.name_text(*id).unwrap_or_default();
                    self.name_token(&text, Some(*id))
                })
                .collect(),
            _ => field
                .path
                .split('.')
                .map(|segment| self.name_token(segment, None))
                .collect(),
        };
        if segments.is_empty() {
            self.push("none");
        } else {
            self.push(&segments.join("."));
        }
        if self.cx.is_some() && default_owner != Some(field.owner.index) {
            let owner = self.object(&field.owner);
            self.push(" in ");
            self.push(&owner);
        }
    }

    fn first_segment(&self, field: &PropertyRef) -> String {
        match (self.symbols, field.names.first()) {
            (Some(symbols), Some(id)) => symbols.name_text(*id).unwrap_or_default(),
            _ => field.path.split('.').next().unwrap_or_default().to_string(),
        }
    }

    fn role_owner(&self, role: Role, field: &PropertyRef) -> Option<i32> {
        let first = self.first_segment(field);
        self.cx.and_then(|cx| cx.default_owner(role, &first))
    }

    fn list(&mut self, items: &[Expr]) {
        for (position, item) in items.iter().enumerate() {
            if position > 0 {
                self.push(", ");
            }
            self.expr(item);
        }
    }

    fn expr(&mut self, expr: &Expr) {
        let span = self.span(expr);
        if let Some(span) = span
            && self.labels.contains(&span.offset)
            && !self.statement_starts.contains(&span.offset)
        {
            let label = format!("@{}: ", label_name(span.offset));
            self.push(&label);
        }
        let start = self.out.len();
        self.body(expr);
        if let Some(span) = span {
            self.ranges.push((span.offset, (start, self.out.len())));
        }
    }

    /// An expression that ends in one of its operands reads into whatever follows it, so as a
    /// context's object it goes in parentheses.
    fn open_ended(expr: &Expr) -> bool {
        matches!(
            expr,
            Expr::Return { .. }
                | Expr::JumpIfNot { .. }
                | Expr::ComputedJump { .. }
                | Expr::Let { .. }
        )
    }

    fn body(&mut self, expr: &Expr) {
        match expr {
            Expr::Simple { name } => self.push(name),
            Expr::SelfRef => self.push("Self"),
            Expr::Variable { name, property } => {
                self.push(name);
                self.push("(");
                let owner = self.role_owner(role_of(name), property);
                self.field(property, owner);
                self.push(")");
            }
            Expr::Return { value } => {
                self.push("Return ");
                self.expr(value);
            }
            Expr::Jump { target } => {
                let target = self.target(*target);
                self.push("Jump ");
                self.push(&target);
            }
            Expr::JumpIfNot { target, condition } => {
                let target = self.target(*target);
                self.push("Jump ");
                self.push(&target);
                self.push(" unless ");
                self.expr(condition);
            }
            Expr::ComputedJump { target } => {
                self.push("Jump ");
                self.expr(target);
            }
            Expr::PushExecutionFlow { target } => {
                let target = self.target(*target);
                self.push("PushExecutionFlow ");
                self.push(&target);
            }
            Expr::Assert {
                line,
                debug,
                condition,
            } => {
                self.push(&format!("Assert<{line}, {debug}>("));
                self.expr(condition);
                self.push(")");
            }
            Expr::NothingInt32 { value } => self.push(&format!("NothingInt32({value})")),
            Expr::Let {
                name,
                property,
                variable,
                value,
            } => {
                self.push(name);
                if let Some(property) = property {
                    let implied = let_default(variable, value);
                    let same = match &implied {
                        Some(implied) => same_field(implied, property),
                        None => is_empty_field(property),
                    };
                    if !same {
                        self.push("<");
                        let owner = let_field(variable).map(|field| field.owner.index);
                        self.field(property, owner);
                        self.push(">");
                    }
                }
                self.push(" ");
                self.expr(variable);
                self.push(" = ");
                if let Expr::FinalCall {
                    function, params, ..
                } = value.as_ref()
                {
                    self.hint(function, params, Some(variable));
                }
                self.kept = Some((value.as_ref() as *const Expr, let_field(variable).cloned()));
                self.expr(value);
                self.kept = None;
            }
            Expr::BitFieldConst { property, value } => {
                self.push("BitFieldConst(");
                self.field(property, None);
                self.push(&format!(", {value})"));
            }
            Expr::Context {
                name,
                object,
                skip,
                property,
                member,
            } => {
                let wrap = Self::open_ended(object);
                if wrap {
                    self.push("(");
                }
                self.expr(object);
                if wrap {
                    self.push(")");
                }
                self.push(match *name {
                    "Context_FailSilent" => "?->",
                    "ClassContext" => "::",
                    _ => "->",
                });
                let raw = self.loaded_len(member).is_some_and(|len| len != *skip);
                if raw {
                    self.push(&format!("#{skip}"));
                }
                let kept = match self.kept.take() {
                    Some((value, kept)) if std::ptr::eq(value, expr) => kept,
                    _ => None,
                };
                let shown = match rvalue_default(member, kept.as_ref()) {
                    Some(implied) => !same_field(implied, property),
                    None => !is_empty_field(property),
                };
                if raw && !shown {
                    self.push(" ");
                }
                if shown {
                    self.push("[");
                    let owner = rvalue_owner(member);
                    self.field(property, owner);
                    self.push("] ");
                }
                let nested = matches!(member.as_ref(), Expr::Context { .. });
                if nested {
                    self.push("(");
                }
                self.expr(member);
                if nested {
                    self.push(")");
                }
            }
            Expr::Cast { name, class, value } => {
                let class = self.object(class);
                self.push(&format!("{name}<{class}>("));
                self.expr(value);
                self.push(")");
            }
            Expr::Skip { skip, value } => {
                self.push(&format!("Skip<#{skip}>("));
                self.expr(value);
                self.push(")");
            }
            Expr::VirtualCall {
                name,
                function,
                params,
                id,
            } => {
                let callee = self.name_token(function, Some(*id));
                self.push(&format!("{name} {callee}("));
                self.list(params);
                self.push(")");
            }
            Expr::FinalCall {
                name,
                function,
                params,
            } => {
                let callee = self.object(function);
                if !self.notes.iter().any(|note| note.starts_with("probably ")) {
                    self.hint(function, params, None);
                }
                self.push(&format!("{name} {callee}("));
                self.list(params);
                self.push(")");
            }
            Expr::IntConst { value, .. } => self.push(&value.to_string()),
            Expr::Int64Const { value, .. } => self.push(&format!("Int64Const({value})")),
            Expr::UInt64Const { value, .. } => self.push(&format!("UInt64Const({value})")),
            Expr::FloatConst { value, .. } => {
                if value.is_finite() {
                    self.push(&format!("{}f", f32_text(*value)));
                } else {
                    self.push(&format!("FloatConst({})", f32_text(*value)));
                }
            }
            Expr::DoubleConst { value, .. } => {
                if value.is_finite() {
                    self.push(&f64_text(*value));
                } else {
                    self.push(&format!("DoubleConst({})", f64_text(*value)));
                }
            }
            Expr::ByteConst { name, value, .. } => self.push(&format!("{name}({value})")),
            Expr::StringConst { value, .. } => self.push(&quote_ansi(value)),
            Expr::UnicodeStringConst { value, .. } => self.push(&quote_unicode(value)),
            Expr::ObjectConst { object } => {
                let object = self.object(object);
                self.push(&format!("ObjectConst({object})"));
            }
            Expr::NameConst {
                name, value, id, ..
            } => {
                let literal = self.name_literal(value, *id);
                if *name == "NameConst" {
                    self.push(&literal);
                } else {
                    self.push(&format!("{name}({literal})"));
                }
            }
            Expr::Numbers { name, values, .. } => {
                let single = *name == "Vector3fConst";
                let values: Vec<String> = values
                    .iter()
                    .map(|value| {
                        if single {
                            f32_text(*value as f32)
                        } else {
                            f64_text(*value)
                        }
                    })
                    .collect();
                self.push(&format!("{name}({})", values.join(", ")));
            }
            Expr::TextConst { text } => match text {
                TextLiteral::Empty => self.push("TextConst<Empty>"),
                TextLiteral::Localized {
                    source,
                    key,
                    namespace,
                } => {
                    self.push("TextConst<Localized>(");
                    self.expr(source);
                    self.push(", ");
                    self.expr(key);
                    self.push(", ");
                    self.expr(namespace);
                    self.push(")");
                }
                TextLiteral::Invariant { source } => {
                    self.push("TextConst<Invariant>(");
                    self.expr(source);
                    self.push(")");
                }
                TextLiteral::Literal { source } => {
                    self.push("TextConst<Literal>(");
                    self.expr(source);
                    self.push(")");
                }
                TextLiteral::StringTable {
                    table,
                    table_id,
                    key,
                } => {
                    let table = self.object(table);
                    self.push(&format!("TextConst<StringTable, {table}>("));
                    self.expr(table_id);
                    self.push(", ");
                    self.expr(key);
                    self.push(")");
                }
            },
            Expr::StructConst {
                struct_type,
                size,
                fields,
            } => {
                let struct_type = self.object(struct_type);
                self.push(&format!("StructConst<{struct_type}, {size}>("));
                self.list(fields);
                self.push(")");
            }
            Expr::SetArray { array, items } => {
                self.push("SetArray(");
                self.expr(array);
                for item in items {
                    self.push(", ");
                    self.expr(item);
                }
                self.push(")");
            }
            Expr::PropertyConst { property } => {
                self.push("PropertyConst(");
                self.field(property, None);
                self.push(")");
            }
            Expr::Conversion { conversion, value } => {
                let kind = kismet::conversion_name(*conversion)
                    .map_or_else(|| format!("0x{conversion:02X}"), str::to_string);
                self.push(&format!("Cast<{kind}>("));
                self.expr(value);
                self.push(")");
            }
            Expr::SetContainer {
                name,
                target,
                count,
                items,
            } => {
                self.push(name);
                if *count != container_count(name, items.len()) {
                    self.push(&format!("<#{count}>"));
                }
                self.push("(");
                self.expr(target);
                for item in items {
                    self.push(", ");
                    self.expr(item);
                }
                self.push(")");
            }
            Expr::ContainerConst {
                name,
                property,
                count,
                items,
            } => {
                self.push(name);
                self.push("<");
                self.field(property, None);
                if *count != container_count(name, items.len()) {
                    self.push(&format!(", #{count}"));
                }
                self.push(">(");
                self.list(items);
                self.push(")");
            }
            Expr::MapConst {
                key,
                value,
                count,
                items,
            } => {
                self.push("MapConst<");
                self.field(key, None);
                self.push(", ");
                self.field(value, None);
                if *count != container_count("MapConst", items.len()) {
                    self.push(&format!(", #{count}"));
                }
                self.push(">(");
                self.list(items);
                self.push(")");
            }
            Expr::Member {
                name,
                property,
                value,
            } => {
                self.push(name);
                self.push("<");
                let owner = self.role_owner(role_of(name), property);
                self.field(property, owner);
                self.push(">(");
                self.expr(value);
                self.push(")");
            }
            Expr::Unary { name, value } => {
                self.push(&format!("{name}("));
                self.expr(value);
                self.push(")");
            }
            Expr::SkipOffsetConst { value } => {
                let foreign = self
                    .span(expr)
                    .is_some_and(|span| self.foreign.contains(&(span.at + 1)));
                let target = if foreign {
                    format!("#0x{value:04X}")
                } else {
                    self.target(*value)
                };
                self.push(&format!("SkipOffsetConst({target})"));
            }
            Expr::DelegateOp {
                name,
                delegate,
                value,
            } => {
                self.push(&format!("{name}("));
                self.expr(delegate);
                self.push(", ");
                self.expr(value);
                self.push(")");
            }
            Expr::BindDelegate {
                function,
                delegate,
                object,
                id,
            } => {
                let function = self.name_token(function, Some(*id));
                self.push(&format!("BindDelegate<{function}>("));
                self.expr(delegate);
                self.push(", ");
                self.expr(object);
                self.push(")");
            }
            Expr::CallMulticastDelegate {
                signature,
                delegate,
                params,
            } => {
                let signature = self.object(signature);
                self.push(&format!("CallMulticastDelegate {signature}("));
                self.expr(delegate);
                for param in params {
                    self.push(", ");
                    self.expr(param);
                }
                self.push(")");
            }
            Expr::SwitchValue {
                end,
                index,
                cases,
                default,
            } => {
                self.push("SwitchValue");
                if self
                    .span(default)
                    .is_some_and(|span| span.end_offset != *end)
                {
                    self.push(&format!("<#0x{end:04X}>"));
                }
                self.push("(");
                self.expr(index);
                for case in cases {
                    self.push(", ");
                    self.expr(&case.value);
                    self.push(" =>");
                    if self
                        .span(&case.result)
                        .is_some_and(|span| span.end_offset != case.next)
                    {
                        self.push(&format!("#0x{:04X}", case.next));
                    }
                    self.push(" ");
                    self.expr(&case.result);
                }
                self.push(", default => ");
                self.expr(default);
                self.push(")");
            }
            Expr::InstrumentationEvent { event, name, id } => match name {
                Some(name) => {
                    let literal = self.name_literal(name, *id);
                    self.push(&format!("InstrumentationEvent({event}, {literal})"));
                }
                None => self.push(&format!("InstrumentationEvent({event})")),
            },
            Expr::ArrayGetByRef { array, index } => {
                self.push("ArrayGetByRef(");
                self.expr(array);
                self.push(", ");
                self.expr(index);
                self.push(")");
            }
            Expr::Unknown { token } => self.push(&format!("?? 0x{token:02X}")),
        }
    }
}

fn path_token(path: &str) -> String {
    if is_bare_path(path) {
        path.to_string()
    } else {
        quote_name(path)
    }
}

/// The count a container's items make: one per element, and a map's run key, value.
fn container_count(name: &str, items: usize) -> i32 {
    let count = if matches!(name, "SetMap" | "MapConst") {
        items / 2
    } else {
        items
    };
    i32::try_from(count).unwrap_or(i32::MAX)
}

/// An expression as the text writes it, without the package around it: for messages, where the
/// short forms are close enough.
pub fn print_expr(expr: &Expr) -> String {
    let mut printer = Printer::detached();
    printer.expr(expr);
    printer.out
}

/// The offsets in a function's code that something jumps or resumes to, with what enters there
/// from outside the function.
fn targets(
    parsed: &ParsedPackage,
    export: u32,
    script: &Script,
) -> (BTreeMap<u32, Vec<String>>, HashSet<u64>) {
    let name = parsed
        .exports
        .get(export as usize)
        .map(|export| export.object_name.as_str())
        .unwrap_or_default();
    let mut found: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    let mut foreign = HashSet::new();
    for fixup in &script.fixups {
        let kismet::FixupKind::Absolute { target, resumes } = &fixup.kind else {
            continue;
        };
        match (fixup.source, resumes) {
            (kismet::FixupSource::SwitchEnd | kismet::FixupSource::SwitchNext, _) => {}
            (kismet::FixupSource::Linkage, Some(resumes)) if resumes != name => {
                foreign.insert(fixup.at);
            }
            _ => {
                found.entry(*target).or_default();
            }
        }
    }
    let functions = kismet::functions_of(&parsed.exports);
    if let Some(function) = functions.iter().find(|f| f.export == export) {
        let inbound = kismet::inbound(&functions, function);
        let holder = |export: u32| {
            parsed
                .exports
                .get(export as usize)
                .map_or_else(|| format!("export {export}"), |e| e.object_name.clone())
        };
        for entry in &inbound.entries {
            found
                .entry(entry.offset)
                .or_default()
                .push(format!("{} enters here", holder(entry.export)));
        }
        for (export, fixup) in &inbound.linkages {
            if let kismet::FixupKind::Absolute { target, .. } = fixup.kind {
                found.entry(target).or_default().push(format!(
                    "a latent action in {} resumes here",
                    holder(*export)
                ));
            }
        }
        for other in &functions {
            if other.event_graph.0 == function.export as i32 + 1 && other.event_graph.1 > 0 {
                found
                    .entry(other.event_graph.1 as u32)
                    .or_default()
                    .push(format!("{} enters here directly", other.name));
            }
        }
    }
    (found, foreign)
}

/// A function's script as assembler text: every statement, and a label wherever code is jumped
/// or resumed to. `None` when the export holds no script.
pub fn print_script(parsed: &ParsedPackage, export: u32) -> Option<ScriptText> {
    ScriptPrinter::new(parsed).print(export)
}

/// Prints any of a package's scripts against one table of its names and objects, built once,
/// which is what a caller printing several functions of a package uses rather than
/// [`print_script`] for each.
pub struct ScriptPrinter<'a> {
    parsed: &'a ParsedPackage,
    symbols: Symbols,
}

impl<'a> ScriptPrinter<'a> {
    pub fn new(parsed: &'a ParsedPackage) -> Self {
        Self {
            parsed,
            symbols: Symbols::of(parsed),
        }
    }

    /// The script of export `export` as text, or `None` when it has none.
    pub fn print(&self, export: u32) -> Option<ScriptText> {
        let script = self.parsed.exports.get(export as usize)?.script.as_ref()?;
        let cx = FunctionCx::of(self.parsed, export);
        Some(print_with(
            self.parsed,
            export,
            script,
            &self.symbols,
            cx.as_ref(),
        ))
    }
}

pub(crate) fn print_with(
    parsed: &ParsedPackage,
    export: u32,
    script: &Script,
    symbols: &Symbols,
    cx: Option<&FunctionCx>,
) -> ScriptText {
    let mut spans = HashMap::new();
    kismet::visit_spans(script, &mut |expr: &Expr, span: &Span, _| {
        spans.insert(expr as *const Expr, *span);
    });
    let starts: HashSet<u32> = script.spans.iter().map(|span| span.offset).collect();
    let statement_starts: HashSet<u32> = script.statements.iter().map(|s| s.offset).collect();
    let end = script.decoded_size;
    let (found, foreign) = targets(parsed, export, script);
    let found: BTreeMap<u32, Vec<String>> = if script.complete() {
        found
            .into_iter()
            .filter(|(offset, _)| *offset == end || starts.contains(offset))
            .collect()
    } else {
        BTreeMap::new()
    };
    let mut printer = Printer {
        symbols: Some(symbols),
        cx,
        spans,
        labels: found.keys().copied().collect(),
        statement_starts: statement_starts.clone(),
        foreign,
        out: String::new(),
        ranges: Vec::new(),
        notes: Vec::new(),
        kept: None,
    };
    let label_of = |offset: u32| TextLabel {
        name: label_name(offset),
        note: found
            .get(&offset)
            .filter(|notes| !notes.is_empty())
            .map(|notes| notes.join("; ")),
    };
    let mut lines = Vec::with_capacity(script.statements.len());
    for statement in &script.statements {
        printer.out.clear();
        printer.ranges.clear();
        printer.notes.clear();
        printer.expr(&statement.expr);
        let labels = if found.contains_key(&statement.offset) {
            vec![label_of(statement.offset)]
        } else {
            Vec::new()
        };
        lines.push(TextLine {
            offset: statement.offset,
            labels,
            text: std::mem::take(&mut printer.out),
            note: (!printer.notes.is_empty()).then(|| printer.notes.join("; ")),
            ranges: std::mem::take(&mut printer.ranges),
        });
    }
    let end_labels = if found.contains_key(&end) && !statement_starts.contains(&end) {
        vec![label_of(end)]
    } else {
        Vec::new()
    };
    let stopped = script.stopped.as_ref().map(|stop| {
        format!(
            "stopped at 0x{:04X} (file {:#X}) on token {:#04X} {}: {}",
            stop.offset,
            stop.at,
            stop.token,
            kismet::token_name(stop.token).unwrap_or("unknown"),
            stop.reason
        )
    });
    ScriptText {
        lines,
        end_labels,
        stopped,
    }
}

// ----------------------------------------------------------------------------------------------
// Reading the text
// ----------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Int(i128),
    /// A number with a point or an exponent, and whether it carries the `f` of a single.
    Float(String, bool),
    Str(String),
    UStr(String),
    Quoted(String),
    Path(String),
    Label(String),
    /// `#` and an integer: a value written exactly as the bytes hold it.
    Raw(i128),
    Punct(&'static str),
    Newline,
}

impl Tok {
    fn describe(&self) -> String {
        match self {
            Tok::Ident(name) => format!("`{name}`"),
            Tok::Int(value) => format!("the number {value}"),
            Tok::Float(text, _) => format!("the number {text}"),
            Tok::Str(_) | Tok::UStr(_) => "a string".to_string(),
            Tok::Quoted(text) => format!("'{text}'"),
            Tok::Path(path) => format!("the path {path}"),
            Tok::Label(label) => format!("@{label}"),
            Tok::Raw(value) => format!("#{value}"),
            Tok::Punct(p) => format!("`{p}`"),
            Tok::Newline => "the end of the line".to_string(),
        }
    }
}

#[derive(Debug, Clone)]
struct Token {
    tok: Tok,
    pos: Pos,
}

const PUNCTS: &[&str] = &[
    "?->", "->", "=>", "::", "(", ")", "<", ">", "[", "]", ",", "=", ":", ".",
];

fn lex(text: &str) -> Result<Vec<Token>, Diagnostic> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let (mut i, mut line, mut column) = (0usize, 1u32, 1u32);
    let mut depth = 0u32;
    let advance = |i: &mut usize, column: &mut u32, by: usize| {
        *i += by;
        *column += by as u32;
    };
    while i < chars.len() {
        let c = chars[i];
        let pos = Pos { line, column };
        match c {
            '\n' => {
                if depth == 0
                    && !matches!(
                        out.last(),
                        Some(Token {
                            tok: Tok::Newline,
                            ..
                        })
                    )
                {
                    out.push(Token {
                        tok: Tok::Newline,
                        pos,
                    });
                }
                i += 1;
                line += 1;
                column = 1;
            }
            c if c.is_whitespace() => advance(&mut i, &mut column, 1),
            ';' => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '"' | '\'' => {
                let (value, used) = quoted(&chars[i..], c, pos)?;
                advance(&mut i, &mut column, used);
                out.push(Token {
                    tok: if c == '"' {
                        Tok::Str(value)
                    } else {
                        Tok::Quoted(value)
                    },
                    pos,
                });
            }
            'u' if chars.get(i + 1) == Some(&'"') => {
                let (value, used) = quoted(&chars[i + 1..], '"', pos)?;
                advance(&mut i, &mut column, used + 1);
                out.push(Token {
                    tok: Tok::UStr(value),
                    pos,
                });
            }
            '@' => {
                let start = i + 1;
                let mut end = start;
                while end < chars.len() && (chars[end].is_ascii_alphanumeric() || chars[end] == '_')
                {
                    end += 1;
                }
                if end == start {
                    return Err(pos.error("a label needs a name after its @"));
                }
                out.push(Token {
                    tok: Tok::Label(chars[start..end].iter().collect()),
                    pos,
                });
                let by = end - i;
                advance(&mut i, &mut column, by);
            }
            '#' => {
                let (value, used) = integer(&chars[i + 1..])
                    .ok_or_else(|| pos.error("a # takes a whole number right after it"))?;
                out.push(Token {
                    tok: Tok::Raw(value),
                    pos,
                });
                advance(&mut i, &mut column, used + 1);
            }
            '/' => {
                let mut end = i;
                while end < chars.len()
                    && is_path_char(chars[end])
                    && !(chars[end] == '-' && chars.get(end + 1) == Some(&'>'))
                {
                    end += 1;
                }
                out.push(Token {
                    tok: Tok::Path(chars[i..end].iter().collect()),
                    pos,
                });
                let by = end - i;
                advance(&mut i, &mut column, by);
            }
            c if c.is_ascii_digit()
                || (c == '-' && chars.get(i + 1).is_some_and(|n| n.is_ascii_digit())) =>
            {
                let (tok, used) = number(&chars[i..])
                    .ok_or_else(|| pos.error("this number does not read as one"))?;
                out.push(Token { tok, pos });
                advance(&mut i, &mut column, used);
            }
            '-' if chars[i + 1..].starts_with(&['i', 'n', 'f']) => {
                out.push(Token {
                    tok: Tok::Float("-inf".into(), false),
                    pos,
                });
                advance(&mut i, &mut column, 4);
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let mut end = i;
                while end < chars.len() && (chars[end].is_ascii_alphanumeric() || chars[end] == '_')
                {
                    end += 1;
                }
                out.push(Token {
                    tok: Tok::Ident(chars[i..end].iter().collect()),
                    pos,
                });
                let by = end - i;
                advance(&mut i, &mut column, by);
            }
            _ => {
                let rest: String = chars[i..chars.len().min(i + 3)].iter().collect();
                let Some(punct) = PUNCTS.iter().find(|p| rest.starts_with(**p)) else {
                    return Err(pos.error(format!("`{c}` has no meaning here")));
                };
                match *punct {
                    "(" | "<" | "[" => depth += 1,
                    ")" | ">" | "]" => depth = depth.saturating_sub(1),
                    _ => {}
                }
                out.push(Token {
                    tok: Tok::Punct(punct),
                    pos,
                });
                advance(&mut i, &mut column, punct.len());
            }
        }
    }
    Ok(out)
}

/// A whole number, decimal or `0x` hexadecimal, with an optional sign.
fn integer(chars: &[char]) -> Option<(i128, usize)> {
    let (negative, start) = match chars.first() {
        Some('-') => (true, 1),
        _ => (false, 0),
    };
    let rest = &chars[start..];
    let (radix, skip) = if rest.len() > 2 && rest[0] == '0' && matches!(rest[1], 'x' | 'X') {
        (16, 2)
    } else {
        (10, 0)
    };
    let digits: String = rest[skip..]
        .iter()
        .take_while(|c| c.is_digit(radix))
        .collect();
    if digits.is_empty() {
        return None;
    }
    let value = i128::from_str_radix(&digits, radix).ok()?;
    Some((
        if negative { -value } else { value },
        start + skip + digits.len(),
    ))
}

fn number(chars: &[char]) -> Option<(Tok, usize)> {
    let digits = usize::from(chars.first() == Some(&'-'));
    let hex = chars.get(digits) == Some(&'0') && matches!(chars.get(digits + 1), Some('x' | 'X'));
    if hex {
        let (value, used) = integer(chars)?;
        return Some((Tok::Int(value), used));
    }
    let mut end = usize::from(chars[0] == '-');
    while end < chars.len() && chars[end].is_ascii_digit() {
        end += 1;
    }
    let mut float = false;
    if chars.get(end) == Some(&'.') && chars.get(end + 1).is_some_and(|c| c.is_ascii_digit()) {
        float = true;
        end += 1;
        while end < chars.len() && chars[end].is_ascii_digit() {
            end += 1;
        }
    }
    if matches!(chars.get(end), Some('e' | 'E')) {
        let mut at = end + 1;
        if matches!(chars.get(at), Some('-' | '+')) {
            at += 1;
        }
        if chars.get(at).is_some_and(|c| c.is_ascii_digit()) {
            float = true;
            end = at;
            while end < chars.len() && chars[end].is_ascii_digit() {
                end += 1;
            }
        }
    }
    let text: String = chars[..end].iter().collect();
    let single = chars.get(end) == Some(&'f')
        && !chars
            .get(end + 1)
            .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_');
    if single {
        return Some((Tok::Float(text, true), end + 1));
    }
    if float {
        Some((Tok::Float(text, false), end))
    } else {
        Some((Tok::Int(text.parse().ok()?), end))
    }
}

/// A quoted string from its opening quote, with how many characters it took.
fn quoted(chars: &[char], quote: char, pos: Pos) -> Result<(String, usize), Diagnostic> {
    let mut out = String::new();
    let mut i = 1;
    loop {
        match chars.get(i) {
            None | Some('\n') => return Err(pos.error("this string is never closed")),
            Some(c) if *c == quote => return Ok((out, i + 1)),
            Some('\\') => {
                let escaped = chars
                    .get(i + 1)
                    .ok_or_else(|| pos.error("this string is never closed"))?;
                match escaped {
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    '\\' | '"' | '\'' => out.push(*escaped),
                    'x' => {
                        let hex: String = chars.iter().skip(i + 2).take(2).collect();
                        let value = u8::from_str_radix(&hex, 16)
                            .ok()
                            .filter(|_| hex.len() == 2)
                            .ok_or_else(|| pos.error("\\x takes two hexadecimal digits"))?;
                        out.push(char::from(value));
                        i += 2;
                    }
                    'u' => {
                        let close = chars[i..]
                            .iter()
                            .position(|c| *c == '}')
                            .filter(|_| chars.get(i + 2) == Some(&'{'))
                            .ok_or_else(|| pos.error("\\u takes a code point in braces"))?;
                        let hex: String = chars[i + 3..i + close].iter().collect();
                        let value = u32::from_str_radix(&hex, 16)
                            .ok()
                            .and_then(char::from_u32)
                            .ok_or_else(|| {
                                pos.error(format!("\\u{{{hex}}} is not a character a string holds"))
                            })?;
                        out.push(value);
                        i += close - 1;
                    }
                    other => return Err(pos.error(format!("\\{other} is not an escape"))),
                }
                i += 2;
            }
            Some(c) => {
                out.push(*c);
                i += 1;
            }
        }
    }
}

/// What the parser keeps about an expression that the expression itself has no place for: the
/// labels defined at it, the label its offset operand names, and which lengths were written out.
#[derive(Debug, Clone, Default)]
pub(crate) struct Note {
    pub(crate) pos: Pos,
    pub(crate) labels: Vec<(String, Pos)>,
    pub(crate) target: Option<(String, Pos)>,
    /// A context's skip, or a switch's end, given as written rather than measured.
    pub(crate) raw: bool,
    /// A context whose value field the text left out.
    pub(crate) implicit: bool,
    /// Each switch case's next offset, likewise.
    pub(crate) raw_next: Vec<bool>,
    /// The notes of the expression's own expressions, in the order [`kismet::children`] lists
    /// them.
    pub(crate) children: Vec<Note>,
}

/// One statement of assembler text, parsed.
#[derive(Debug, Clone)]
pub(crate) struct ParsedStatement {
    pub(crate) labels: Vec<(String, Pos)>,
    pub(crate) expr: Expr,
    pub(crate) note: Note,
}

/// A parsed script: its statements and the labels after the last of them.
#[derive(Debug, Clone, Default)]
pub(crate) struct ParsedScript {
    pub(crate) statements: Vec<ParsedStatement>,
    pub(crate) end_labels: Vec<(String, Pos)>,
}

/// Whether the text may grow the package's tables, and the tables it grows.
pub(crate) struct Resolver<'a> {
    pub(crate) symbols: Symbols,
    pub(crate) cx: Option<FunctionCx>,
    pub(crate) tables: &'a mut Tables,
    pub(crate) add_names: bool,
    pub(crate) add_imports: bool,
    pub(crate) warnings: Vec<Diagnostic>,
}

impl Resolver<'_> {
    fn name(&mut self, text: &str, pin: Option<i128>, pos: Pos) -> Result<NameId, Diagnostic> {
        if let Some(pin) = pin {
            let index = i32::try_from(pin)
                .map_err(|_| pos.error(format!("#{pin} is no entry of the name table")))?;
            return self.symbols.pinned(index, text).ok_or_else(|| {
                pos.error(match self.symbols.entry(index) {
                    Some(entry) => format!(
                        "'{text}' is not entry #{index} ('{entry}'), with or without a number after it"
                    ),
                    None => format!("#{index} is no entry of the name table"),
                })
            });
        }
        if let Some(id) = self.symbols.name_id(text) {
            return Ok(id);
        }
        if text.is_empty() {
            return Err(pos.error("a name cannot be empty"));
        }
        if !self.add_names {
            return Err(pos.error(format!("this package has no name '{text}'")));
        }
        let stored = self.tables.names.store(text);
        let (base, _) = split_number(text);
        if stored.index as usize >= self.symbols.names.len() {
            self.symbols.names.push(base.to_string());
            self.symbols
                .lookup
                .insert(base.to_string(), stored.index as usize);
        }
        Ok(stored.into())
    }

    fn object(
        &mut self,
        object: &ObjectSyntax,
        class: Option<(&str, &str)>,
    ) -> Result<ObjectRef, Diagnostic> {
        let pos = object.pos;
        let (path, typed) = match &object.kind {
            ObjectKind::None => {
                return Ok(ObjectRef {
                    index: 0,
                    path: None,
                });
            }
            ObjectKind::Raw(index) => {
                let index = i32::try_from(*index)
                    .ok()
                    .filter(|index| self.symbols.holds(*index))
                    .ok_or_else(|| {
                        pos.error(format!(
                            "#{index} is outside the {} imports and {} exports this package has",
                            self.symbols.imports, self.symbols.exports
                        ))
                    })?;
                return Ok(ObjectRef {
                    index,
                    path: self.symbols.path_of(index).map(str::to_string),
                });
            }
            ObjectKind::Path { path, class } => (path.clone(), class.clone()),
        };
        if let Some(index) = self.symbols.object(&path) {
            return Ok(ObjectRef {
                index,
                path: Some(path),
            });
        }
        if !self.add_imports {
            return Err(pos.error(format!("this package names no object {path}")));
        }
        let class = match typed {
            Some(class) => Some(split_class(&class).ok_or_else(|| {
                pos.error(format!(
                    "{class} is not a class path such as /Script/Engine.Texture2D"
                ))
            })?),
            None => {
                if class.is_none() {
                    self.warnings.push(pos.error(format!(
                        "{path} is imported without a class; write its class before it, as /Script/Engine.Texture2D'{path}'"
                    )));
                }
                class.map(|(package, name)| (package.to_string(), name.to_string()))
            }
        };
        let index = crate::header_edit::add_import(self.tables, &path, class)
            .map_err(|reason| pos.error(format!("{path} cannot be imported: {reason}")))?;
        self.symbols.objects.insert(path.clone(), index);
        self.symbols.paths.insert(index, path.clone());
        self.symbols.imports = self.tables.imports.len();
        Ok(ObjectRef {
            index,
            path: Some(path),
        })
    }

    /// A field path, with the owner the text gave it or the one its place implies.
    fn field(
        &mut self,
        field: &FieldSyntax,
        role: Role,
        implied: Option<i32>,
    ) -> Result<PropertyRef, Diagnostic> {
        if field.none {
            let owner = match &field.owner {
                Some(owner) => self.object(owner, None)?,
                None => ObjectRef {
                    index: 0,
                    path: None,
                },
            };
            return Ok(PropertyRef {
                owner,
                ..empty_field()
            });
        }
        let mut names = Vec::with_capacity(field.segments.len());
        for (text, pin, pos) in &field.segments {
            names.push(self.name(text, *pin, *pos)?);
        }
        let path = field
            .segments
            .iter()
            .map(|(text, _, _)| text.as_str())
            .collect::<Vec<_>>()
            .join(".");
        let owner = match &field.owner {
            Some(owner) => {
                let owner = self.object(owner, None)?;
                let first = field
                    .segments
                    .first()
                    .map(|(text, _, _)| text.as_str())
                    .unwrap_or_default();
                if role == Role::Local
                    && let Some(cx) = &self.cx
                    && owner.index == cx.own
                    && !cx.has_local(first)
                {
                    return Err(self.missing_owner(role, first, field.pos));
                }
                owner
            }
            None => {
                let first = field
                    .segments
                    .first()
                    .map(|(text, _, _)| text.as_str())
                    .unwrap_or_default();
                let index = match role {
                    Role::Other => implied,
                    role => self
                        .cx
                        .as_ref()
                        .and_then(|cx| cx.default_owner(role, first)),
                }
                .ok_or_else(|| self.missing_owner(role, first, field.pos))?;
                ObjectRef {
                    index,
                    path: self.symbols.path_of(index).map(str::to_string),
                }
            }
        };
        Ok(PropertyRef { path, owner, names })
    }

    fn missing_owner(&self, role: Role, first: &str, pos: Pos) -> Diagnostic {
        let function = self
            .cx
            .as_ref()
            .map_or("the function", |cx| cx.name.as_str());
        pos.error(match role {
            Role::Local => format!(
                "{first} is not a parameter or local of {function}; the text can only use the ones it has"
            ),
            Role::Member => format!(
                "{first} is not a member {} declares; write the class that does, as InstanceVariable({first} in /Script/Engine.Actor)",
                self.cx
                    .as_ref()
                    .map(|cx| cx.class_name.as_str())
                    .filter(|name| !name.is_empty())
                    .unwrap_or("the class")
            ),
            Role::Persistent => format!(
                "{first} is not a local of the event graph; write its owner after `in`"
            ),
            Role::Other => format!("{first} needs its owner written after it, as `{first} in <object>`"),
        })
    }
}

/// `/Script/Engine.Texture2D` as the package and name of the class.
fn split_class(class: &str) -> Option<(String, String)> {
    let (package, name) = class.rsplit_once('.')?;
    (package.starts_with('/') && !name.is_empty()).then(|| (package.to_string(), name.to_string()))
}

#[derive(Debug, Clone)]
enum ObjectKind {
    None,
    Raw(i128),
    Path { path: String, class: Option<String> },
}

#[derive(Debug, Clone)]
struct ObjectSyntax {
    kind: ObjectKind,
    pos: Pos,
}

/// A field path as written, before anything resolves it.
#[derive(Debug, Clone, Default)]
struct FieldSyntax {
    segments: Vec<(String, Option<i128>, Pos)>,
    owner: Option<ObjectSyntax>,
    none: bool,
    pos: Pos,
}

/// The tokens [`kismet::token_name`] gives the one-byte forms, which take no operand.
fn is_simple(token: u8) -> bool {
    matches!(
        token,
        0x0B | 0x15
            | 0x16
            | 0x25
            | 0x26
            | 0x27
            | 0x28
            | 0x2A
            | 0x2D
            | 0x30
            | 0x32
            | 0x3A
            | 0x3C
            | 0x3E
            | 0x40
            | 0x4A
            | 0x4D
            | 0x50
            | 0x53
            | 0x5A
            | 0x5E
            | 0x66
    )
}

/// The name a token's variant keeps, which is the static string the reader gives it.
fn static_name(token: u8) -> &'static str {
    kismet::token_name(token).unwrap_or("Unknown")
}

/// An expression with what the text said about it beyond the expression itself.
type Noted = (Expr, Note);
type Parsed = Result<Noted, Diagnostic>;

struct Parser<'r, 'a> {
    tokens: Vec<Token>,
    at: usize,
    resolver: &'r mut Resolver<'a>,
}

impl Parser<'_, '_> {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.at).map(|token| &token.tok)
    }

    fn peek_at(&self, ahead: usize) -> Option<&Tok> {
        self.tokens.get(self.at + ahead).map(|token| &token.tok)
    }

    fn pos(&self) -> Pos {
        self.tokens
            .get(self.at)
            .or_else(|| self.tokens.last())
            .map_or(Pos { line: 1, column: 1 }, |token| token.pos)
    }

    fn next(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.at).cloned();
        self.at += 1;
        token
    }

    fn is(&self, punct: &str) -> bool {
        matches!(self.peek(), Some(Tok::Punct(p)) if *p == punct)
    }

    fn is_word(&self, word: &str) -> bool {
        matches!(self.peek(), Some(Tok::Ident(w)) if w == word)
    }

    fn unexpected(&self, wanted: &str) -> Diagnostic {
        let found = self
            .peek()
            .map_or_else(|| "the end of the text".to_string(), Tok::describe);
        self.pos()
            .error(format!("expected {wanted}, found {found}"))
    }

    fn expect(&mut self, punct: &str) -> Result<(), Diagnostic> {
        if self.is(punct) {
            self.at += 1;
            Ok(())
        } else {
            Err(self.unexpected(&format!("`{punct}`")))
        }
    }

    fn eat(&mut self, punct: &str) -> bool {
        let found = self.is(punct);
        if found {
            self.at += 1;
        }
        found
    }

    fn int(&mut self, what: &str) -> Result<(i128, Pos), Diagnostic> {
        let pos = self.pos();
        match self.peek() {
            Some(Tok::Int(value)) => {
                let value = *value;
                self.at += 1;
                Ok((value, pos))
            }
            _ => Err(self.unexpected(what)),
        }
    }

    fn raw(&mut self) -> Result<(i128, Pos), Diagnostic> {
        let pos = self.pos();
        match self.peek() {
            Some(Tok::Raw(value)) => {
                let value = *value;
                self.at += 1;
                Ok((value, pos))
            }
            _ => Err(self.unexpected("a raw value such as #12")),
        }
    }

    fn fits<T: TryFrom<i128>>(value: i128, pos: Pos, what: &str) -> Result<T, Diagnostic> {
        T::try_from(value).map_err(|_| pos.error(format!("{value} does not fit {what}")))
    }

    // --- statements ---------------------------------------------------------------------------

    fn script(&mut self) -> (ParsedScript, Vec<Diagnostic>) {
        let mut out = ParsedScript::default();
        let mut errors = Vec::new();
        let mut pending: Vec<(String, Pos)> = Vec::new();
        while self.at < self.tokens.len() {
            if matches!(self.peek(), Some(Tok::Newline)) {
                self.at += 1;
                continue;
            }
            let labels = self.label_defs();
            pending.extend(labels);
            if matches!(self.peek(), Some(Tok::Newline) | None) {
                continue;
            }
            match self.statement() {
                Ok((expr, note)) => {
                    if !matches!(self.peek(), Some(Tok::Newline) | None) {
                        errors.push(self.unexpected("the end of the statement"));
                        self.skip_line();
                    }
                    out.statements.push(ParsedStatement {
                        labels: std::mem::take(&mut pending),
                        expr,
                        note,
                    });
                }
                Err(error) => {
                    errors.push(error);
                    pending.clear();
                    self.skip_line();
                }
            }
        }
        out.end_labels = pending;
        (out, errors)
    }

    fn skip_line(&mut self) {
        while let Some(tok) = self.peek() {
            if *tok == Tok::Newline {
                break;
            }
            self.at += 1;
        }
    }

    /// Labels defined here: `@name:`.
    fn label_defs(&mut self) -> Vec<(String, Pos)> {
        let mut out = Vec::new();
        while let (Some(Tok::Label(name)), Some(Tok::Punct(":"))) = (self.peek(), self.peek_at(1)) {
            out.push((name.clone(), self.pos()));
            self.at += 2;
        }
        out
    }

    fn statement(&mut self) -> Parsed {
        self.expr()
    }

    // --- expressions --------------------------------------------------------------------------

    fn expr(&mut self) -> Parsed {
        let labels = self.label_defs();
        let (expr, mut note) = self.postfix()?;
        if !labels.is_empty() {
            let mut all = labels;
            all.append(&mut note.labels);
            note.labels = all;
        }
        Ok((expr, note))
    }

    fn postfix(&mut self) -> Parsed {
        let (mut expr, mut note) = self.primary()?;
        loop {
            let arrow = match self.peek() {
                Some(Tok::Punct("->")) => "Context",
                Some(Tok::Punct("?->")) => "Context_FailSilent",
                Some(Tok::Punct("::")) => "ClassContext",
                _ => break,
            };
            let pos = self.pos();
            self.at += 1;
            let skip = match self.peek() {
                Some(Tok::Raw(_)) => Some(self.raw()?),
                _ => None,
            };
            let rvalue = if self.eat("[") {
                let field = self.field_syntax()?;
                self.expect("]")?;
                Some(field)
            } else {
                None
            };
            let (member, member_note) = self.primary()?;
            let property = match &rvalue {
                None => rvalue_default(&member, None)
                    .cloned()
                    .unwrap_or_else(empty_field),
                Some(field) => self
                    .resolver
                    .field(field, Role::Other, rvalue_owner(&member))?,
            };
            let skip_value = match skip {
                Some((value, pos)) => Self::fits(value, pos, "a context's skip")?,
                None => 0,
            };
            expr = Expr::Context {
                name: arrow,
                object: Box::new(expr),
                skip: skip_value,
                property,
                member: Box::new(member),
            };
            note = Note {
                pos,
                raw: skip.is_some(),
                implicit: rvalue.is_none(),
                children: vec![note, member_note],
                ..Note::default()
            };
        }
        Ok((expr, note))
    }

    fn leaf(expr: Expr, pos: Pos) -> Parsed {
        Ok((
            expr,
            Note {
                pos,
                ..Note::default()
            },
        ))
    }

    fn with(expr: Expr, pos: Pos, children: Vec<Note>) -> Parsed {
        Ok((
            expr,
            Note {
                pos,
                children,
                ..Note::default()
            },
        ))
    }

    fn primary(&mut self) -> Parsed {
        let pos = self.pos();
        let Some(token) = self.next() else {
            return Err(pos.error("expected an expression, found the end of the text"));
        };
        match token.tok {
            Tok::Int(value) => Self::leaf(
                Expr::IntConst {
                    value: Self::fits(
                        value,
                        pos,
                        "an IntConst; write Int64Const(…) for a wider one",
                    )?,
                    at: 0,
                },
                pos,
            ),
            Tok::Float(text, single) => {
                if single {
                    Self::leaf(
                        Expr::FloatConst {
                            value: parse_f32(&text, pos)?,
                            at: 0,
                        },
                        pos,
                    )
                } else {
                    Self::leaf(
                        Expr::DoubleConst {
                            value: parse_f64(&text, pos)?,
                            at: 0,
                        },
                        pos,
                    )
                }
            }
            Tok::Str(value) => {
                if let Some(wide) = value.chars().find(|c| u32::from(*c) > 0xFF) {
                    return Err(pos.error(format!(
                        "{wide:?} does not fit a StringConst, which holds one byte a character; write the string as u\"…\""
                    )));
                }
                if value.contains('\0') {
                    return Err(pos.error("a string cannot hold a NUL, which would end it"));
                }
                Self::leaf(Expr::StringConst { value, at: 0 }, pos)
            }
            Tok::UStr(value) => {
                if value.contains('\0') {
                    return Err(pos.error("a string cannot hold a NUL, which would end it"));
                }
                Self::leaf(Expr::UnicodeStringConst { value, at: 0 }, pos)
            }
            Tok::Quoted(value) => {
                let id = self.pinned_name(&value, pos)?;
                Self::leaf(
                    Expr::NameConst {
                        name: "NameConst",
                        value,
                        at: 0,
                        id,
                    },
                    pos,
                )
            }
            Tok::Punct("(") => {
                let inner = self.expr()?;
                self.expect(")")?;
                Ok(inner)
            }
            Tok::Ident(word) => self.word(&word, pos),
            other => Err(pos.error(format!(
                "expected an expression, found {}",
                other.describe()
            ))),
        }
    }

    /// A name, then a pin when one follows it.
    fn pinned_name(&mut self, text: &str, pos: Pos) -> Result<NameId, Diagnostic> {
        let pin = match self.peek() {
            Some(Tok::Raw(pin)) => {
                let pin = *pin;
                self.at += 1;
                Some(pin)
            }
            _ => None,
        };
        self.resolver.name(text, pin, pos)
    }

    /// A name operand: bare or quoted, with an optional pin.
    fn name_operand(&mut self) -> Result<(String, NameId), Diagnostic> {
        let pos = self.pos();
        let text = match self.next().map(|token| token.tok) {
            Some(Tok::Ident(text) | Tok::Quoted(text)) => text,
            _ => {
                self.at -= 1;
                return Err(self.unexpected("a name"));
            }
        };
        let id = self.pinned_name(&text, pos)?;
        Ok((text, id))
    }

    fn name_literal(&mut self) -> Result<(String, NameId), Diagnostic> {
        let pos = self.pos();
        match self.next().map(|token| token.tok) {
            Some(Tok::Quoted(text)) => {
                let id = self.pinned_name(&text, pos)?;
                Ok((text, id))
            }
            _ => {
                self.at -= 1;
                Err(self.unexpected("a quoted name such as 'Tick'"))
            }
        }
    }

    fn object_syntax(&mut self) -> Result<ObjectSyntax, Diagnostic> {
        let pos = self.pos();
        let kind = match self.next().map(|token| token.tok) {
            Some(Tok::Ident(word)) if word == "None" => ObjectKind::None,
            Some(Tok::Raw(index)) => ObjectKind::Raw(index),
            Some(Tok::Path(path) | Tok::Quoted(path) | Tok::Ident(path)) => {
                if let Some(Tok::Quoted(object)) = self.peek() {
                    let object = object.clone();
                    self.at += 1;
                    ObjectKind::Path {
                        path: object,
                        class: Some(path),
                    }
                } else {
                    ObjectKind::Path { path, class: None }
                }
            }
            _ => {
                self.at -= 1;
                return Err(self.unexpected("an object: a path, None or #index"));
            }
        };
        Ok(ObjectSyntax { kind, pos })
    }

    fn object(&mut self, class: Option<(&str, &str)>) -> Result<ObjectRef, Diagnostic> {
        let syntax = self.object_syntax()?;
        self.resolver.object(&syntax, class)
    }

    fn field_syntax(&mut self) -> Result<FieldSyntax, Diagnostic> {
        let pos = self.pos();
        let mut field = FieldSyntax {
            pos,
            ..FieldSyntax::default()
        };
        if self.is_word("none") {
            self.at += 1;
            field.none = true;
        } else {
            loop {
                let segment_pos = self.pos();
                let text = match self.next().map(|token| token.tok) {
                    Some(Tok::Ident(text) | Tok::Quoted(text)) => text,
                    _ => {
                        self.at -= 1;
                        return Err(self.unexpected("a variable's name"));
                    }
                };
                let pin = match self.peek() {
                    Some(Tok::Raw(pin)) => {
                        let pin = *pin;
                        self.at += 1;
                        Some(pin)
                    }
                    _ => None,
                };
                field.segments.push((text, pin, segment_pos));
                if !self.eat(".") {
                    break;
                }
            }
        }
        if self.is_word("in") {
            self.at += 1;
            field.owner = Some(self.object_syntax()?);
        }
        Ok(field)
    }

    fn target(&mut self) -> Result<(u32, Option<(String, Pos)>), Diagnostic> {
        let pos = self.pos();
        match self.next().map(|token| token.tok) {
            Some(Tok::Label(label)) => Ok((0, Some((label, pos)))),
            Some(Tok::Raw(value)) => Ok((Self::fits(value, pos, "a code offset")?, None)),
            _ => {
                self.at -= 1;
                Err(self.unexpected("a label such as @0045"))
            }
        }
    }

    /// `(a, b, …)`, each an expression.
    fn args(&mut self) -> Result<(Vec<Expr>, Vec<Note>), Diagnostic> {
        self.expect("(")?;
        let mut exprs = Vec::new();
        let mut notes = Vec::new();
        if self.eat(")") {
            return Ok((exprs, notes));
        }
        loop {
            let (expr, note) = self.expr()?;
            exprs.push(expr);
            notes.push(note);
            if self.eat(")") {
                return Ok((exprs, notes));
            }
            self.expect(",")?;
        }
    }

    fn one_arg(&mut self) -> Parsed {
        self.expect("(")?;
        let inner = self.expr()?;
        self.expect(")")?;
        Ok(inner)
    }

    fn two_args(&mut self) -> Result<(Noted, Noted), Diagnostic> {
        self.expect("(")?;
        let first = self.expr()?;
        self.expect(",")?;
        let second = self.expr()?;
        self.expect(")")?;
        Ok((first, second))
    }

    fn float_operand(&mut self) -> Result<(String, Pos), Diagnostic> {
        let pos = self.pos();
        match self.next().map(|token| token.tok) {
            Some(Tok::Float(text, _)) => Ok((text, pos)),
            Some(Tok::Int(value)) => Ok((value.to_string(), pos)),
            Some(Tok::Ident(word)) if word == "inf" => Ok((word, pos)),
            Some(Tok::Ident(word)) if word == "nan" => {
                self.expect("(")?;
                let (bits, _) = self.int("the NaN's bits, as nan(0x7FC00000)")?;
                self.expect(")")?;
                Ok((format!("nan:{bits}"), pos))
            }
            _ => {
                self.at -= 1;
                Err(self.unexpected("a number"))
            }
        }
    }

    fn word(&mut self, word: &str, pos: Pos) -> Parsed {
        if word == "Self" {
            return Self::leaf(Expr::SelfRef, pos);
        }
        let token = kismet::token_named(word);
        if let Some(token) = token
            && is_simple(token)
        {
            return Self::leaf(
                Expr::Simple {
                    name: static_name(token),
                },
                pos,
            );
        }
        let Some(token) = token else {
            return Err(pos.error(format!("{word} is not an instruction")));
        };
        let name = static_name(token);
        match token {
            0x00 | 0x01 | 0x02 | 0x48 | 0x6C => {
                self.expect("(")?;
                let field = self.field_syntax()?;
                self.expect(")")?;
                let property = self.resolver.field(&field, role_of(name), None)?;
                Self::leaf(Expr::Variable { name, property }, pos)
            }
            0x04 => {
                let (value, note) = self.expr()?;
                Self::with(
                    Expr::Return {
                        value: Box::new(value),
                    },
                    pos,
                    vec![note],
                )
            }
            0x06 => {
                let jumps = matches!(self.peek(), Some(Tok::Label(_) | Tok::Raw(_)));
                if !jumps {
                    let (target, note) = self.expr()?;
                    return Self::with(
                        Expr::ComputedJump {
                            target: Box::new(target),
                        },
                        pos,
                        vec![note],
                    );
                }
                let (target, label) = self.target()?;
                if self.is_word("unless") {
                    self.at += 1;
                    let (condition, note) = self.expr()?;
                    return Ok((
                        Expr::JumpIfNot {
                            target,
                            condition: Box::new(condition),
                        },
                        Note {
                            pos,
                            target: label,
                            children: vec![note],
                            ..Note::default()
                        },
                    ));
                }
                Ok((
                    Expr::Jump { target },
                    Note {
                        pos,
                        target: label,
                        ..Note::default()
                    },
                ))
            }
            0x07 | 0x4E => Err(pos.error(format!(
                "{word} is written as Jump: `Jump @label unless <condition>`, or `Jump <expression>`"
            ))),
            0x4C => {
                let (target, label) = self.target()?;
                Ok((
                    Expr::PushExecutionFlow { target },
                    Note {
                        pos,
                        target: label,
                        ..Note::default()
                    },
                ))
            }
            0x09 => {
                self.expect("<")?;
                let (line, line_pos) = self.int("the assert's line")?;
                self.expect(",")?;
                let (debug, debug_pos) = self.int("the assert's debug flag")?;
                self.expect(">")?;
                let (condition, note) = self.one_arg()?;
                Self::with(
                    Expr::Assert {
                        line: Self::fits(line, line_pos, "an assert's line")?,
                        debug: Self::fits(debug, debug_pos, "an assert's flag")?,
                        condition: Box::new(condition),
                    },
                    pos,
                    vec![note],
                )
            }
            0x0C => {
                self.expect("(")?;
                let (value, value_pos) = self.int("a number")?;
                self.expect(")")?;
                Self::leaf(
                    Expr::NothingInt32 {
                        value: Self::fits(value, value_pos, "NothingInt32")?,
                    },
                    pos,
                )
            }
            0x0F | 0x14 | 0x43 | 0x44 | 0x5F | 0x60 => {
                let field = if token == 0x0F && self.eat("<") {
                    let field = self.field_syntax()?;
                    self.expect(">")?;
                    Some(field)
                } else {
                    None
                };
                let (variable, variable_note) = self.expr()?;
                self.expect("=")?;
                let (mut value, value_note) = self.expr()?;
                // A call made through a context hands its result back through the variable the
                // `Let` keeps it in, unless the text says otherwise.
                if value_note.implicit
                    && let Expr::Context {
                        member, property, ..
                    } = &mut value
                    && is_call(member)
                {
                    *property = let_field(&variable).cloned().unwrap_or_else(empty_field);
                }
                let property = if token == 0x0F {
                    Some(match &field {
                        Some(field) => {
                            let implied = let_field(&variable).map(|field| field.owner.index);
                            self.resolver.field(field, Role::Other, implied)?
                        }
                        None => let_default(&variable, &value).unwrap_or_else(empty_field),
                    })
                } else {
                    None
                };
                Self::with(
                    Expr::Let {
                        name,
                        property,
                        variable: Box::new(variable),
                        value: Box::new(value),
                    },
                    pos,
                    vec![variable_note, value_note],
                )
            }
            0x11 => {
                self.expect("(")?;
                let field = self.field_syntax()?;
                self.expect(",")?;
                let (value, value_pos) = self.int("the bit's value")?;
                self.expect(")")?;
                let property = self.resolver.field(&field, Role::Other, None)?;
                Self::leaf(
                    Expr::BitFieldConst {
                        property,
                        value: Self::fits(value, value_pos, "a byte")?,
                    },
                    pos,
                )
            }
            0x12 | 0x19 | 0x1A => Err(pos.error(format!(
                "{word} is written between its object and member: `object->member`, `object?->member` or `object::member`"
            ))),
            0x13 | 0x2E | 0x52 | 0x54 | 0x55 => {
                self.expect("<")?;
                let class = self.object(Some(("/Script/CoreUObject", "Class")))?;
                self.expect(">")?;
                let (value, note) = self.one_arg()?;
                Self::with(
                    Expr::Cast {
                        name,
                        class,
                        value: Box::new(value),
                    },
                    pos,
                    vec![note],
                )
            }
            0x18 => {
                self.expect("<")?;
                let (skip, skip_pos) = self.raw()?;
                self.expect(">")?;
                let (value, note) = self.one_arg()?;
                Self::with(
                    Expr::Skip {
                        skip: Self::fits(skip, skip_pos, "a skip")?,
                        value: Box::new(value),
                    },
                    pos,
                    vec![note],
                )
            }
            0x1B | 0x45 => {
                let (function, id) = self.name_operand()?;
                let (params, notes) = self.args()?;
                Self::with(
                    Expr::VirtualCall {
                        name,
                        function,
                        params,
                        id,
                    },
                    pos,
                    notes,
                )
            }
            0x1C | 0x46 | 0x68 => {
                let function = self.object(Some(("/Script/CoreUObject", "Function")))?;
                let (params, notes) = self.args()?;
                Self::with(
                    Expr::FinalCall {
                        name,
                        function,
                        params,
                    },
                    pos,
                    notes,
                )
            }
            0x1D => {
                self.expect("(")?;
                let (value, value_pos) = self.int("a number")?;
                self.expect(")")?;
                Self::leaf(
                    Expr::IntConst {
                        value: Self::fits(value, value_pos, "an IntConst")?,
                        at: 0,
                    },
                    pos,
                )
            }
            0x35 | 0x36 | 0x24 | 0x2C => {
                self.expect("(")?;
                let (value, value_pos) = self.int("a number")?;
                self.expect(")")?;
                let expr = match token {
                    0x35 => Expr::Int64Const {
                        value: Self::fits(value, value_pos, "an Int64Const")?,
                        at: 0,
                    },
                    0x36 => Expr::UInt64Const {
                        value: Self::fits(value, value_pos, "a UInt64Const")?,
                        at: 0,
                    },
                    _ => Expr::ByteConst {
                        name,
                        value: Self::fits(value, value_pos, "a byte")?,
                        at: 0,
                    },
                };
                Self::leaf(expr, pos)
            }
            0x1E | 0x37 => {
                self.expect("(")?;
                let (text, value_pos) = self.float_operand()?;
                self.expect(")")?;
                let expr = if token == 0x1E {
                    Expr::FloatConst {
                        value: parse_f32(&text, value_pos)?,
                        at: 0,
                    }
                } else {
                    Expr::DoubleConst {
                        value: parse_f64(&text, value_pos)?,
                        at: 0,
                    }
                };
                Self::leaf(expr, pos)
            }
            0x1F | 0x34 => Err(pos.error(format!(
                "{word} is written as its string: \"…\", or u\"…\" for a UnicodeStringConst"
            ))),
            0x20 => {
                self.expect("(")?;
                let object = self.object(None)?;
                self.expect(")")?;
                Self::leaf(Expr::ObjectConst { object }, pos)
            }
            0x21 => Err(pos.error("a NameConst is written as its name in quotes, as 'Tick'")),
            0x4B => {
                self.expect("(")?;
                let (value, id) = self.name_literal()?;
                self.expect(")")?;
                Self::leaf(
                    Expr::NameConst {
                        name,
                        value,
                        at: 0,
                        id,
                    },
                    pos,
                )
            }
            0x22 | 0x23 | 0x2B | 0x41 => {
                let wanted = if token == 0x2B { 10 } else { 3 };
                self.expect("(")?;
                let mut values = Vec::with_capacity(wanted);
                loop {
                    let (text, value_pos) = self.float_operand()?;
                    values.push(if token == 0x41 {
                        f64::from(parse_f32(&text, value_pos)?)
                    } else {
                        parse_f64(&text, value_pos)?
                    });
                    if self.eat(")") {
                        break;
                    }
                    self.expect(",")?;
                }
                if values.len() != wanted {
                    return Err(pos.error(format!(
                        "{word} takes {wanted} numbers, not {}",
                        values.len()
                    )));
                }
                Self::leaf(
                    Expr::Numbers {
                        name,
                        values,
                        at: 0,
                    },
                    pos,
                )
            }
            0x29 => self.text_const(pos),
            0x2F => {
                self.expect("<")?;
                let struct_type = self.object(Some(("/Script/CoreUObject", "ScriptStruct")))?;
                self.expect(",")?;
                let (size, size_pos) = self.int("the struct's size")?;
                self.expect(">")?;
                let (fields, notes) = self.args()?;
                Self::with(
                    Expr::StructConst {
                        struct_type,
                        size: Self::fits(size, size_pos, "a struct's size")?,
                        fields,
                    },
                    pos,
                    notes,
                )
            }
            0x31 => {
                let (mut items, notes) = self.args()?;
                if items.is_empty() {
                    return Err(pos.error("SetArray takes the array, then its items"));
                }
                let array = items.remove(0);
                Self::with(
                    Expr::SetArray {
                        array: Box::new(array),
                        items,
                    },
                    pos,
                    notes,
                )
            }
            0x33 => {
                self.expect("(")?;
                let field = self.field_syntax()?;
                self.expect(")")?;
                let property = self.resolver.field(&field, Role::Other, None)?;
                Self::leaf(Expr::PropertyConst { property }, pos)
            }
            0x38 => {
                self.expect("<")?;
                let kind_pos = self.pos();
                let conversion = match self.next().map(|token| token.tok) {
                    Some(Tok::Ident(kind)) => kismet::conversion_kind(&kind).ok_or_else(|| {
                        kind_pos.error(format!("{kind} is not a conversion; write its byte as 0x05"))
                    })?,
                    Some(Tok::Int(value)) => Self::fits(value, kind_pos, "a conversion byte")?,
                    _ => {
                        self.at -= 1;
                        return Err(self.unexpected("a conversion such as DoubleToFloat"));
                    }
                };
                self.expect(">")?;
                let (value, note) = self.one_arg()?;
                Self::with(
                    Expr::Conversion {
                        conversion,
                        value: Box::new(value),
                    },
                    pos,
                    vec![note],
                )
            }
            0x39 | 0x3B => {
                let raw = if self.eat("<") {
                    let raw = self.raw()?;
                    self.expect(">")?;
                    Some(raw)
                } else {
                    None
                };
                let (mut items, notes) = self.args()?;
                if items.is_empty() {
                    return Err(pos.error(format!("{word} takes its target, then its items")));
                }
                let target = items.remove(0);
                let count = match raw {
                    Some((count, count_pos)) => Self::fits(count, count_pos, "a count")?,
                    None => container_count(name, items.len()),
                };
                Self::with(
                    Expr::SetContainer {
                        name,
                        target: Box::new(target),
                        count,
                        items,
                    },
                    pos,
                    notes,
                )
            }
            0x3D | 0x65 => {
                self.expect("<")?;
                let field = self.field_syntax()?;
                let raw = if self.eat(",") { Some(self.raw()?) } else { None };
                self.expect(">")?;
                let (items, notes) = self.args()?;
                let property = self.resolver.field(&field, Role::Other, None)?;
                let count = match raw {
                    Some((count, count_pos)) => Self::fits(count, count_pos, "a count")?,
                    None => container_count(name, items.len()),
                };
                Self::with(
                    Expr::ContainerConst {
                        name,
                        property,
                        count,
                        items,
                    },
                    pos,
                    notes,
                )
            }
            0x3F => {
                self.expect("<")?;
                let key = self.field_syntax()?;
                self.expect(",")?;
                let value = self.field_syntax()?;
                let raw = if self.eat(",") { Some(self.raw()?) } else { None };
                self.expect(">")?;
                let (items, notes) = self.args()?;
                let key = self.resolver.field(&key, Role::Other, None)?;
                let value = self.resolver.field(&value, Role::Other, None)?;
                let count = match raw {
                    Some((count, count_pos)) => Self::fits(count, count_pos, "a count")?,
                    None => container_count(name, items.len()),
                };
                Self::with(
                    Expr::MapConst {
                        key,
                        value,
                        count,
                        items,
                    },
                    pos,
                    notes,
                )
            }
            0x42 | 0x64 => {
                self.expect("<")?;
                let field = self.field_syntax()?;
                self.expect(">")?;
                let property = self.resolver.field(&field, role_of(name), None)?;
                let (value, note) = self.one_arg()?;
                Self::with(
                    Expr::Member {
                        name,
                        property,
                        value: Box::new(value),
                    },
                    pos,
                    vec![note],
                )
            }
            0x4F | 0x51 | 0x5D | 0x67 | 0x6D => {
                let (value, note) = self.one_arg()?;
                Self::with(
                    Expr::Unary {
                        name,
                        value: Box::new(value),
                    },
                    pos,
                    vec![note],
                )
            }
            0x5B => {
                self.expect("(")?;
                let (value, label) = self.target()?;
                self.expect(")")?;
                Ok((
                    Expr::SkipOffsetConst { value },
                    Note {
                        pos,
                        target: label,
                        ..Note::default()
                    },
                ))
            }
            0x5C | 0x62 => {
                let ((delegate, first), (value, second)) = self.two_args()?;
                Self::with(
                    Expr::DelegateOp {
                        name,
                        delegate: Box::new(delegate),
                        value: Box::new(value),
                    },
                    pos,
                    vec![first, second],
                )
            }
            0x61 => {
                self.expect("<")?;
                let (function, id) = self.name_operand()?;
                self.expect(">")?;
                let ((delegate, first), (object, second)) = self.two_args()?;
                Self::with(
                    Expr::BindDelegate {
                        function,
                        delegate: Box::new(delegate),
                        object: Box::new(object),
                        id,
                    },
                    pos,
                    vec![first, second],
                )
            }
            0x63 => {
                let signature = self.object(Some(("/Script/CoreUObject", "Function")))?;
                let (mut params, notes) = self.args()?;
                if params.is_empty() {
                    return Err(pos.error("CallMulticastDelegate takes the delegate, then its arguments"));
                }
                let delegate = params.remove(0);
                Self::with(
                    Expr::CallMulticastDelegate {
                        signature,
                        delegate: Box::new(delegate),
                        params,
                    },
                    pos,
                    notes,
                )
            }
            0x69 => self.switch(pos),
            0x6A => {
                self.expect("(")?;
                let (event, event_pos) = self.int("the event's kind")?;
                let event: u8 = Self::fits(event, event_pos, "an event kind")?;
                let named = if self.eat(",") {
                    Some(self.name_literal()?)
                } else {
                    None
                };
                self.expect(")")?;
                if (event == 4) != named.is_some() {
                    return Err(pos.error("an InstrumentationEvent carries a name exactly when its kind is 4"));
                }
                let id = named.as_ref().map(|(_, id)| *id).unwrap_or_default();
                Self::leaf(
                    Expr::InstrumentationEvent {
                        event,
                        name: named.map(|(name, _)| name),
                        id,
                    },
                    pos,
                )
            }
            0x6B => {
                let ((array, first), (index, second)) = self.two_args()?;
                Self::with(
                    Expr::ArrayGetByRef {
                        array: Box::new(array),
                        index: Box::new(index),
                    },
                    pos,
                    vec![first, second],
                )
            }
            0x17 => Self::leaf(Expr::SelfRef, pos),
            _ => Err(pos.error(format!("{word} is not an instruction the assembler writes"))),
        }
    }

    fn text_const(&mut self, pos: Pos) -> Parsed {
        self.expect("<")?;
        let kind_pos = self.pos();
        let kind = match self.next().map(|token| token.tok) {
            Some(Tok::Ident(kind)) => kind,
            _ => {
                self.at -= 1;
                return Err(self.unexpected(
                    "a text kind: Empty, Localized, Invariant, Literal or StringTable",
                ));
            }
        };
        let table = if kind == "StringTable" {
            self.expect(",")?;
            Some(self.object(Some(("/Script/Engine", "StringTable")))?)
        } else {
            None
        };
        self.expect(">")?;
        let (parts, notes) = if self.is("(") {
            self.args()?
        } else {
            (Vec::new(), Vec::new())
        };
        let wanted = match kind.as_str() {
            "Empty" => 0,
            "Invariant" | "Literal" => 1,
            "StringTable" => 2,
            "Localized" => 3,
            other => {
                return Err(kind_pos.error(format!(
                    "{other} is not a text kind: Empty, Localized, Invariant, Literal or StringTable"
                )));
            }
        };
        if parts.len() != wanted {
            return Err(pos.error(format!(
                "a {kind} text takes {wanted} part(s), not {}",
                parts.len()
            )));
        }
        let mut parts = parts.into_iter().map(Box::new);
        let mut part = || {
            parts
                .next()
                .ok_or_else(|| pos.error("a text part is missing"))
        };
        let text = match kind.as_str() {
            "Empty" => TextLiteral::Empty,
            "Invariant" => TextLiteral::Invariant { source: part()? },
            "Literal" => TextLiteral::Literal { source: part()? },
            "Localized" => TextLiteral::Localized {
                source: part()?,
                key: part()?,
                namespace: part()?,
            },
            _ => TextLiteral::StringTable {
                table: table.unwrap_or(ObjectRef {
                    index: 0,
                    path: None,
                }),
                table_id: part()?,
                key: part()?,
            },
        };
        Self::with(Expr::TextConst { text }, pos, notes)
    }

    fn switch(&mut self, pos: Pos) -> Parsed {
        let raw_end = if self.eat("<") {
            let raw = self.raw()?;
            self.expect(">")?;
            Some(raw)
        } else {
            None
        };
        self.expect("(")?;
        let (index, index_note) = self.expr()?;
        let mut children = vec![index_note];
        let mut cases = Vec::new();
        let mut raw_next = Vec::new();
        loop {
            self.expect(",")?;
            if self.is_word("default") {
                self.at += 1;
                self.expect("=>")?;
                let (default, default_note) = self.expr()?;
                self.expect(")")?;
                children.push(default_note);
                let end = match raw_end {
                    Some((end, end_pos)) => Self::fits(end, end_pos, "a code offset")?,
                    None => 0,
                };
                return Ok((
                    Expr::SwitchValue {
                        end,
                        index: Box::new(index),
                        cases,
                        default: Box::new(default),
                    },
                    Note {
                        pos,
                        raw: raw_end.is_some(),
                        raw_next,
                        children,
                        ..Note::default()
                    },
                ));
            }
            let (value, value_note) = self.expr()?;
            self.expect("=>")?;
            let next = match self.peek() {
                Some(Tok::Raw(_)) => {
                    let (next, next_pos) = self.raw()?;
                    Some(Self::fits::<u32>(next, next_pos, "a code offset")?)
                }
                _ => None,
            };
            let (result, result_note) = self.expr()?;
            children.push(value_note);
            children.push(result_note);
            raw_next.push(next.is_some());
            cases.push(kismet::SwitchCase {
                value,
                next: next.unwrap_or(0),
                result,
            });
        }
    }
}

fn parse_f64(text: &str, pos: Pos) -> Result<f64, Diagnostic> {
    if let Some(bits) = text.strip_prefix("nan:") {
        let bits: i128 = bits.parse().map_err(|_| pos.error("a NaN's bits"))?;
        return u64::try_from(bits)
            .map(f64::from_bits)
            .map_err(|_| pos.error("a double's NaN takes 64 bits"));
    }
    text.parse()
        .map_err(|_| pos.error(format!("{text} is not a number")))
}

fn parse_f32(text: &str, pos: Pos) -> Result<f32, Diagnostic> {
    if let Some(bits) = text.strip_prefix("nan:") {
        let bits: i128 = bits.parse().map_err(|_| pos.error("a NaN's bits"))?;
        return u32::try_from(bits)
            .map(f32::from_bits)
            .map_err(|_| pos.error("a float's NaN takes 32 bits"));
    }
    text.parse()
        .map_err(|_| pos.error(format!("{text} is not a number")))
}

/// A local the text declares, `local Name: Type`, ahead of its first statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Declaration {
    pub(crate) name: String,
    pub(crate) ty: crate::field_record::FieldType,
    pub(crate) pos: Pos,
}

/// Takes the `local` lines off the head of a text, leaving a blank line in each one's place so
/// every later line keeps its number. A `local` line after the first statement is an error.
pub(crate) fn split_declarations(
    text: &str,
) -> Result<(String, Vec<Declaration>), Vec<Diagnostic>> {
    let mut kept = Vec::new();
    let mut declared: Vec<Declaration> = Vec::new();
    let mut errors = Vec::new();
    let mut statements = false;
    for (at, line) in text.lines().enumerate() {
        let number = at as u32 + 1;
        let code = line.split(';').next().unwrap_or_default();
        let trimmed = code.trim_start();
        let Some(rest) = trimmed.strip_prefix("local ") else {
            statements |= !trimmed.trim().is_empty();
            kept.push(line);
            continue;
        };
        kept.push("");
        let pos = Pos {
            line: number,
            column: (code.len() - trimmed.len()) as u32 + 1,
        };
        if statements {
            errors.push(pos.error("declare a local before the first statement"));
            continue;
        }
        let Some((name, ty)) = rest.split_once(':') else {
            errors.push(pos.error("a local is declared as `local Name: Type`"));
            continue;
        };
        let name = name.trim();
        if !is_ident(name) {
            errors.push(pos.error(format!("{name:?} is not a name a local can take")));
            continue;
        }
        if declared.iter().any(|other| other.name == name) {
            errors.push(pos.error(format!("{name} is declared twice")));
            continue;
        }
        match crate::field_record::parse_field_type(ty) {
            Ok(ty) => declared.push(Declaration {
                name: name.to_string(),
                ty,
                pos,
            }),
            Err(reason) => errors.push(pos.error(reason)),
        }
    }
    if errors.is_empty() {
        Ok((kept.join("\n"), declared))
    } else {
        Err(errors)
    }
}

/// Parses assembler text against the package it was printed from. Names and imports the text
/// adds go into the resolver's tables when it allows them.
pub(crate) fn parse_script(
    text: &str,
    resolver: &mut Resolver<'_>,
) -> Result<ParsedScript, Vec<Diagnostic>> {
    let tokens = lex(text).map_err(|error| vec![error])?;
    let mut parser = Parser {
        tokens,
        at: 0,
        resolver,
    };
    let (script, errors) = parser.script();
    if errors.is_empty() {
        Ok(script)
    } else {
        Err(errors)
    }
}
