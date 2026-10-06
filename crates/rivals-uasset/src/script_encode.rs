//! Writes assembler text back into bytecode.
//!
//! The encoder mirrors the reader token for token. Every operand has a fixed width, so one pass
//! lays the code out: each expression's loaded offset is known as it is written, a context's skip
//! and a switch's offsets are measured from the code they cover, and labels used before they are
//! defined are filled in once the pass is done. What it writes is then read back with the reader
//! itself, which has to see the same statements at the same offsets.

use std::collections::{BTreeMap, HashMap};

use serde::Serialize;

use crate::header_edit::Tables;
use crate::kismet::{self, Expr, NameId, ObjectRef, PropertyRef, TextLiteral};
use crate::package::{AssetBundle, ParsedPackage};
use crate::props::{Ctx, Diagnostics};
use crate::script_text::{
    self, Declaration, Diagnostic, FunctionCx, Note, ParsedScript, Pos, Resolver, Symbols,
    parse_script, split_declarations,
};

/// The note an expression built without one stands for: nothing written out, nothing labelled.
static EMPTY: Note = Note {
    pos: Pos { line: 0, column: 0 },
    labels: Vec::new(),
    target: None,
    raw: false,
    implicit: false,
    raw_next: Vec::new(),
    children: Vec::new(),
};

/// The token an expression is written with.
fn token_of(expr: &Expr) -> Option<u8> {
    Some(match expr {
        Expr::Simple { name }
        | Expr::Variable { name, .. }
        | Expr::Let { name, .. }
        | Expr::Context { name, .. }
        | Expr::Cast { name, .. }
        | Expr::VirtualCall { name, .. }
        | Expr::FinalCall { name, .. }
        | Expr::ByteConst { name, .. }
        | Expr::NameConst { name, .. }
        | Expr::Numbers { name, .. }
        | Expr::SetContainer { name, .. }
        | Expr::ContainerConst { name, .. }
        | Expr::Member { name, .. }
        | Expr::Unary { name, .. }
        | Expr::DelegateOp { name, .. } => return kismet::token_named(name),
        Expr::Return { .. } => 0x04,
        Expr::Jump { .. } => 0x06,
        Expr::JumpIfNot { .. } => 0x07,
        Expr::Assert { .. } => 0x09,
        Expr::NothingInt32 { .. } => 0x0C,
        Expr::BitFieldConst { .. } => 0x11,
        Expr::SelfRef => 0x17,
        Expr::Skip { .. } => 0x18,
        Expr::IntConst { .. } => 0x1D,
        Expr::FloatConst { .. } => 0x1E,
        Expr::StringConst { .. } => 0x1F,
        Expr::ObjectConst { .. } => 0x20,
        Expr::TextConst { .. } => 0x29,
        Expr::StructConst { .. } => 0x2F,
        Expr::SetArray { .. } => 0x31,
        Expr::PropertyConst { .. } => 0x33,
        Expr::UnicodeStringConst { .. } => 0x34,
        Expr::Int64Const { .. } => 0x35,
        Expr::UInt64Const { .. } => 0x36,
        Expr::DoubleConst { .. } => 0x37,
        Expr::Conversion { .. } => 0x38,
        Expr::MapConst { .. } => 0x3F,
        Expr::PushExecutionFlow { .. } => 0x4C,
        Expr::ComputedJump { .. } => 0x4E,
        Expr::SkipOffsetConst { .. } => 0x5B,
        Expr::BindDelegate { .. } => 0x61,
        Expr::CallMulticastDelegate { .. } => 0x63,
        Expr::SwitchValue { .. } => 0x69,
        Expr::InstrumentationEvent { .. } => 0x6A,
        Expr::ArrayGetByRef { .. } => 0x6B,
        Expr::Unknown { .. } => return None,
    })
}

/// What one pass over a script wrote.
#[derive(Debug, Clone)]
pub(crate) struct Encoded {
    pub(crate) bytes: Vec<u8>,
    pub(crate) loaded: u32,
    /// Where each statement starts, in loaded bytes.
    pub(crate) statements: Vec<u32>,
    pub(crate) labels: HashMap<String, u32>,
    /// Every object index written, with where it sits in `bytes`.
    pub(crate) links: Vec<(usize, i32)>,
}

struct Encoder {
    out: Vec<u8>,
    offset: u32,
    labels: HashMap<String, (u32, Pos)>,
    uses: Vec<(usize, String, Pos)>,
    links: Vec<(usize, i32)>,
    errors: Vec<Diagnostic>,
}

impl Encoder {
    fn byte(&mut self, value: u8) {
        self.out.push(value);
        self.offset += 1;
    }

    fn raw(&mut self, bytes: &[u8]) {
        self.out.extend_from_slice(bytes);
        self.offset += bytes.len() as u32;
    }

    fn name(&mut self, id: NameId) {
        self.out.extend_from_slice(&id.index.to_le_bytes());
        self.out.extend_from_slice(&id.number.to_le_bytes());
        self.offset += 12;
    }

    fn index(&mut self, index: i32) {
        if index != 0 {
            self.links.push((self.out.len(), index));
        }
        self.out.extend_from_slice(&index.to_le_bytes());
    }

    fn object(&mut self, object: &ObjectRef) {
        self.index(object.index);
        self.offset += 8;
    }

    fn property(&mut self, property: &PropertyRef, pos: Pos) {
        if property.names.is_empty() && !property.path.is_empty() {
            self.errors.push(pos.error(format!(
                "{} has no names to write; it was built without the package's name table",
                property.path
            )));
        }
        self.out
            .extend_from_slice(&(property.names.len() as i32).to_le_bytes());
        for id in &property.names {
            self.out.extend_from_slice(&id.index.to_le_bytes());
            self.out.extend_from_slice(&id.number.to_le_bytes());
        }
        self.index(property.owner.index);
        self.offset += 8;
    }

    fn define(&mut self, label: &str, pos: Pos) {
        if let Some((_, first)) = self.labels.get(label) {
            self.errors.push(pos.error(format!(
                "@{label} is already defined on line {}",
                first.line
            )));
            return;
        }
        self.labels.insert(label.to_string(), (self.offset, pos));
    }

    /// A four-byte code offset: the label's, once known, else the value as written.
    fn code_offset(&mut self, value: u32, target: &Option<(String, Pos)>) {
        if let Some((label, pos)) = target {
            self.uses.push((self.out.len(), label.clone(), *pos));
        }
        self.raw(&value.to_le_bytes());
    }

    fn placeholder(&mut self) -> usize {
        let at = self.out.len();
        self.raw(&[0; 4]);
        at
    }

    fn patch(&mut self, at: usize, value: u32) {
        self.out[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    /// Expressions until `terminator`, which is written after them. An item written with the
    /// terminator's own token would end the list early, so it is refused.
    fn list<'n>(
        &mut self,
        items: &mut [Expr],
        notes: &mut impl Iterator<Item = &'n Note>,
        terminator: u8,
        pos: Pos,
    ) {
        for item in items {
            let note = notes.next().unwrap_or(&EMPTY);
            if token_of(item) == Some(terminator) {
                let at = if note.pos.line == 0 { pos } else { note.pos };
                self.errors.push(at.error(format!(
                    "{} would end its list here",
                    kismet::token_name(terminator).unwrap_or("this token")
                )));
            }
            self.expr(item, note);
        }
        self.byte(terminator);
    }

    fn expr(&mut self, expr: &mut Expr, note: &Note) {
        for (label, pos) in &note.labels {
            self.define(label, *pos);
        }
        let pos = note.pos;
        let Some(token) = token_of(expr) else {
            self.errors
                .push(pos.error("this instruction has no token to write"));
            return;
        };
        self.byte(token);
        let mut kids = note.children.iter();
        let mut kid = || kids.next().unwrap_or(&EMPTY);
        match expr {
            Expr::Simple { .. } | Expr::SelfRef => {}
            Expr::Variable { property, .. } | Expr::PropertyConst { property } => {
                self.property(property, pos);
            }
            Expr::Return { value }
            | Expr::ComputedJump { target: value }
            | Expr::Unary { value, .. } => self.expr(value, kid()),
            Expr::Jump { target } | Expr::PushExecutionFlow { target } => {
                self.code_offset(*target, &note.target);
            }
            Expr::SkipOffsetConst { value } => self.code_offset(*value, &note.target),
            Expr::JumpIfNot { target, condition } => {
                self.code_offset(*target, &note.target);
                self.expr(condition, kid());
            }
            Expr::Assert {
                line,
                debug,
                condition,
            } => {
                self.raw(&line.to_le_bytes());
                self.byte(*debug);
                self.expr(condition, kid());
            }
            Expr::NothingInt32 { value } => self.raw(&value.to_le_bytes()),
            Expr::Let {
                property,
                variable,
                value,
                ..
            } => {
                match (token == 0x0F, property.as_ref()) {
                    (true, Some(property)) => self.property(property, pos),
                    (true, None) => self
                        .errors
                        .push(pos.error("a Let names the field it assigns through")),
                    (false, Some(_)) => self
                        .errors
                        .push(pos.error("only a plain Let names the field it assigns through")),
                    (false, None) => {}
                }
                self.expr(variable, kid());
                self.expr(value, kid());
            }
            Expr::BitFieldConst { property, value } => {
                self.property(property, pos);
                self.byte(*value);
            }
            Expr::Context {
                object,
                skip,
                property,
                member,
                ..
            } => {
                self.expr(object, kid());
                let skip_at = self.placeholder();
                self.property(property, pos);
                let from = self.offset;
                self.expr(member, kid());
                if !note.raw {
                    *skip = self.offset - from;
                }
                self.patch(skip_at, *skip);
            }
            Expr::Cast { class, value, .. } => {
                self.object(class);
                self.expr(value, kid());
            }
            Expr::Skip { skip, value } => {
                self.raw(&skip.to_le_bytes());
                self.expr(value, kid());
            }
            Expr::VirtualCall { params, id, .. } => {
                self.name(*id);
                self.list(params, &mut kids, 0x16, pos);
            }
            Expr::FinalCall {
                function, params, ..
            } => {
                self.object(function);
                self.list(params, &mut kids, 0x16, pos);
            }
            Expr::IntConst { value, .. } => self.raw(&value.to_le_bytes()),
            Expr::Int64Const { value, .. } => self.raw(&value.to_le_bytes()),
            Expr::UInt64Const { value, .. } => self.raw(&value.to_le_bytes()),
            Expr::FloatConst { value, .. } => self.raw(&value.to_le_bytes()),
            Expr::DoubleConst { value, .. } => self.raw(&value.to_le_bytes()),
            Expr::ByteConst { value, .. } => self.byte(*value),
            Expr::StringConst { value, .. } => {
                for c in value.chars() {
                    match u8::try_from(u32::from(c)) {
                        Ok(byte) if byte != 0 => self.byte(byte),
                        _ => {
                            self.errors
                                .push(pos.error(format!("{c:?} does not fit a StringConst")));
                        }
                    }
                }
                self.byte(0);
            }
            Expr::UnicodeStringConst { value, .. } => {
                for unit in value.encode_utf16() {
                    self.raw(&unit.to_le_bytes());
                }
                self.raw(&[0, 0]);
            }
            Expr::ObjectConst { object } => self.object(object),
            Expr::NameConst { id, .. } => self.name(*id),
            Expr::Numbers { name, values, .. } => {
                let wanted = if *name == "TransformConst" { 10 } else { 3 };
                if values.len() != wanted {
                    self.errors.push(pos.error(format!(
                        "{name} takes {wanted} numbers, not {}",
                        values.len()
                    )));
                }
                for value in values.iter() {
                    if *name == "Vector3fConst" {
                        self.raw(&(*value as f32).to_le_bytes());
                    } else {
                        self.raw(&value.to_le_bytes());
                    }
                }
            }
            Expr::TextConst { text } => match text {
                TextLiteral::Empty => self.byte(0),
                TextLiteral::Localized {
                    source,
                    key,
                    namespace,
                } => {
                    self.byte(1);
                    self.expr(source, kid());
                    self.expr(key, kid());
                    self.expr(namespace, kid());
                }
                TextLiteral::Invariant { source } => {
                    self.byte(2);
                    self.expr(source, kid());
                }
                TextLiteral::Literal { source } => {
                    self.byte(3);
                    self.expr(source, kid());
                }
                TextLiteral::StringTable {
                    table,
                    table_id,
                    key,
                } => {
                    self.byte(4);
                    self.object(table);
                    self.expr(table_id, kid());
                    self.expr(key, kid());
                }
            },
            Expr::StructConst {
                struct_type,
                size,
                fields,
            } => {
                self.object(struct_type);
                self.raw(&size.to_le_bytes());
                self.list(fields, &mut kids, 0x30, pos);
            }
            Expr::SetArray { array, items } => {
                self.expr(array, kid());
                self.list(items, &mut kids, 0x32, pos);
            }
            Expr::Conversion { conversion, value } => {
                self.byte(*conversion);
                self.expr(value, kid());
            }
            Expr::SetContainer {
                target,
                count,
                items,
                ..
            } => {
                self.expr(target, kid());
                self.raw(&count.to_le_bytes());
                let end = if token == 0x39 { 0x3A } else { 0x3C };
                self.list(items, &mut kids, end, pos);
            }
            Expr::ContainerConst {
                property,
                count,
                items,
                ..
            } => {
                self.property(property, pos);
                self.raw(&count.to_le_bytes());
                let end = if token == 0x3D { 0x3E } else { 0x66 };
                self.list(items, &mut kids, end, pos);
            }
            Expr::MapConst {
                key,
                value,
                count,
                items,
            } => {
                self.property(key, pos);
                self.property(value, pos);
                self.raw(&count.to_le_bytes());
                self.list(items, &mut kids, 0x40, pos);
            }
            Expr::Member {
                property, value, ..
            } => {
                self.property(property, pos);
                self.expr(value, kid());
            }
            Expr::DelegateOp {
                delegate, value, ..
            } => {
                self.expr(delegate, kid());
                self.expr(value, kid());
            }
            Expr::BindDelegate {
                delegate,
                object,
                id,
                ..
            } => {
                self.name(*id);
                self.expr(delegate, kid());
                self.expr(object, kid());
            }
            Expr::CallMulticastDelegate {
                signature,
                delegate,
                params,
            } => {
                self.object(signature);
                self.expr(delegate, kid());
                self.list(params, &mut kids, 0x16, pos);
            }
            Expr::SwitchValue {
                end,
                index,
                cases,
                default,
            } => {
                let Ok(count) = u16::try_from(cases.len()) else {
                    self.errors
                        .push(pos.error("a switch holds at most 65535 cases"));
                    return;
                };
                self.raw(&count.to_le_bytes());
                let end_at = self.placeholder();
                self.expr(index, kid());
                for (position, case) in cases.iter_mut().enumerate() {
                    self.expr(&mut case.value, kid());
                    let next_at = self.placeholder();
                    self.expr(&mut case.result, kid());
                    if !note.raw_next.get(position).copied().unwrap_or(false) {
                        case.next = self.offset;
                    }
                    self.patch(next_at, case.next);
                }
                self.expr(default, kid());
                if !note.raw {
                    *end = self.offset;
                }
                self.patch(end_at, *end);
            }
            Expr::InstrumentationEvent { event, name, id } => {
                self.byte(*event);
                if *event == 4 {
                    if name.is_none() {
                        self.errors
                            .push(pos.error("an inline InstrumentationEvent carries a name"));
                    }
                    self.name(*id);
                }
            }
            Expr::ArrayGetByRef { array, index } => {
                self.expr(array, kid());
                self.expr(index, kid());
            }
            Expr::Unknown { .. } => {}
        }
    }
}

/// Writes each label's offset into the expression that names it, so it compares whole against
/// the same code read back.
fn resolve_targets(expr: &mut Expr, note: &Note, labels: &HashMap<String, u32>) {
    if let Some((label, _)) = &note.target
        && let Some(offset) = labels.get(label)
    {
        match expr {
            Expr::Jump { target }
            | Expr::JumpIfNot { target, .. }
            | Expr::PushExecutionFlow { target } => *target = *offset,
            Expr::SkipOffsetConst { value } => *value = *offset,
            _ => {}
        }
    }
    let mut kids = note.children.iter();
    for child in kismet::children_mut(expr) {
        resolve_targets(child, kids.next().unwrap_or(&EMPTY), labels);
    }
}

/// Lays a parsed script out as bytecode.
pub(crate) fn encode(script: &mut ParsedScript) -> Result<Encoded, Vec<Diagnostic>> {
    let mut encoder = Encoder {
        out: Vec::new(),
        offset: 0,
        labels: HashMap::new(),
        uses: Vec::new(),
        links: Vec::new(),
        errors: Vec::new(),
    };
    let mut statements = Vec::with_capacity(script.statements.len());
    for statement in &mut script.statements {
        for (label, pos) in &statement.labels {
            encoder.define(label, *pos);
        }
        statements.push(encoder.offset);
        encoder.expr(&mut statement.expr, &statement.note);
    }
    for (label, pos) in &script.end_labels {
        encoder.define(label, *pos);
    }
    let labels: HashMap<String, u32> = encoder
        .labels
        .iter()
        .map(|(label, (offset, _))| (label.clone(), *offset))
        .collect();
    for (at, label, pos) in std::mem::take(&mut encoder.uses) {
        match labels.get(&label) {
            Some(offset) => encoder.patch(at, *offset),
            None => encoder
                .errors
                .push(pos.error(format!("@{label} is not defined anywhere in the text"))),
        }
    }
    if !encoder.errors.is_empty() {
        return Err(encoder.errors);
    }
    for statement in &mut script.statements {
        resolve_targets(&mut statement.expr, &statement.note, &labels);
    }
    Ok(Encoded {
        bytes: encoder.out,
        loaded: encoder.offset,
        statements,
        labels,
        links: encoder.links,
    })
}

/// Whether the text may add names or imports to the package.
#[derive(Debug, Clone, Copy, Default)]
pub struct AssembleOptions {
    pub add_names: bool,
    pub add_imports: bool,
}

/// A script written from text.
#[derive(Debug, Clone)]
pub struct Assembled {
    pub bytes: Vec<u8>,
    pub loaded_size: u32,
    /// Where each statement starts, in loaded bytes.
    pub statements: Vec<u32>,
    /// The offsets the script had where the text kept a label for them, to where they are now:
    /// every label named after an offset, and the script's old end to its new one.
    pub moved: BTreeMap<u32, u32>,
    /// Every object index the script names, with where it sits in `bytes`.
    pub links: Vec<(u64, i32)>,
    pub warnings: Vec<Diagnostic>,
    /// The statements as written, every offset they name resolved.
    pub expressions: Vec<Expr>,
    /// The locals the text declares that the function does not have yet.
    pub(crate) locals: Vec<Declaration>,
}

/// The locals a text declares that the function does not have yet, each held to the fields it does
/// have: a parameter's name is refused, and a local it has already is no change when the type is
/// the same. The new ones become names the text can use.
fn new_locals(
    declared: Vec<Declaration>,
    parsed: &ParsedPackage,
    export: u32,
    cx: &mut Option<FunctionCx>,
) -> Result<Vec<Declaration>, Vec<Diagnostic>> {
    if declared.is_empty() {
        return Ok(declared);
    }
    let function = parsed.exports.get(export as usize);
    let name = function.map_or("the function", |f| f.object_name.as_str());
    let (Some(signature), Some(cx)) = (function.and_then(|f| f.signature.as_ref()), cx.as_mut())
    else {
        return Err(declared
            .iter()
            .map(|decl| {
                decl.pos.error(format!(
                    "{name}'s fields were not read, so it can take no new local"
                ))
            })
            .collect());
    };
    let mut errors = Vec::new();
    let mut new = Vec::new();
    for decl in declared {
        if signature.params.iter().any(|param| param.name == decl.name) {
            errors.push(
                decl.pos
                    .error(format!("{} is a parameter of {name}", decl.name)),
            );
        } else if let Some(local) = signature.locals.iter().find(|l| l.name == decl.name) {
            if local.kind != decl.ty.printed() {
                errors.push(decl.pos.error(format!(
                    "{} is already a local of {name}, of type {}",
                    decl.name, local.kind
                )));
            }
        } else {
            cx.declare(&decl.name);
            new.push(decl);
        }
    }
    if errors.is_empty() {
        Ok(new)
    } else {
        Err(errors)
    }
}

/// Assembles `text` as the script of `export`, against the package's tables as they stand in
/// `tables`. The tables take the names and imports the text adds only when it assembles.
pub(crate) fn assemble(
    text: &str,
    parsed: &ParsedPackage,
    export: u32,
    header: &retoc::legacy_asset::FLegacyPackageHeader,
    tables: &mut Tables,
    options: AssembleOptions,
) -> Result<Assembled, Vec<Diagnostic>> {
    let (text, declared) = split_declarations(text)?;
    let mut cx = FunctionCx::of(parsed, export);
    let locals = new_locals(declared, parsed, export, &mut cx)?;
    let mut work = tables.clone();
    let symbols = Symbols::with_names(parsed, work.names.raw_names().to_vec());
    let mut resolver = Resolver {
        symbols,
        cx,
        tables: &mut work,
        add_names: options.add_names,
        add_imports: options.add_imports,
        warnings: Vec::new(),
    };
    let mut script = parse_script(&text, &mut resolver)?;
    let warnings = std::mem::take(&mut resolver.warnings);
    let encoded = encode(&mut script)?;
    read_back(&encoded, &script, header, &work).map_err(|error| vec![error])?;
    let moved = moved_of(&encoded, parsed, export);
    *tables = work;
    Ok(Assembled {
        expressions: script
            .statements
            .iter()
            .map(|statement| statement.expr.clone())
            .collect(),
        loaded_size: encoded.loaded,
        statements: encoded.statements,
        moved,
        links: encoded
            .links
            .iter()
            .map(|(at, index)| (*at as u64, *index))
            .collect(),
        bytes: encoded.bytes,
        warnings,
        locals,
    })
}

/// What is wrong with `text` as the script of `export`, line by line, and what is only worth
/// knowing: both empty for a text that assembles cleanly.
pub fn text_diagnostics(
    bundle: &AssetBundle<'_>,
    parsed: &ParsedPackage,
    export: u32,
    text: &str,
) -> Result<(Vec<Diagnostic>, Vec<Diagnostic>), String> {
    let header = crate::package::read_header(bundle)?;
    let mut tables = Tables {
        names: header.name_map.clone(),
        imports: header.imports.clone(),
    };
    let options = AssembleOptions {
        add_names: true,
        add_imports: true,
    };
    Ok(
        match assemble(text, parsed, export, &header, &mut tables, options) {
            Ok(assembled) => (Vec::new(), assembled.warnings),
            Err(errors) => (errors, Vec::new()),
        },
    )
}

/// Where the offsets a script had land in the code written from a text: every label named after an
/// offset, and the script's old end at its new one.
pub(crate) fn moved_of(
    encoded: &Encoded,
    parsed: &ParsedPackage,
    export: u32,
) -> BTreeMap<u32, u32> {
    let mut moved: BTreeMap<u32, u32> = encoded
        .labels
        .iter()
        .filter_map(|(label, offset)| Some((script_text::original_offset(label)?, *offset)))
        .collect();
    if let Some(script) = parsed
        .exports
        .get(export as usize)
        .and_then(|export| export.script.as_ref())
    {
        moved.entry(script.decoded_size).or_insert(encoded.loaded);
    }
    moved
}

/// Parses and lays out `text` against a package as it was saved, adding nothing to it: the
/// statements and the code a saved script is held to.
pub(crate) fn lay_out(
    text: &str,
    parsed: &ParsedPackage,
    export: u32,
) -> Result<(Vec<Expr>, Encoded), Vec<Diagnostic>> {
    let (text, declared) = split_declarations(text)?;
    let mut cx = FunctionCx::of(parsed, export);
    // Every local the text declares is one the saved function has.
    if let Some(missing) = new_locals(declared, parsed, export, &mut cx)?.first() {
        return Err(vec![missing.pos.error(format!(
            "the saved function has no local {}",
            missing.name
        ))]);
    }
    let mut tables = Tables {
        names: retoc::legacy_asset::FPackageNameMap::create_from_names(parsed.names.clone()),
        imports: Vec::new(),
    };
    let mut resolver = Resolver {
        symbols: Symbols::of(parsed),
        cx,
        tables: &mut tables,
        add_names: false,
        add_imports: false,
        warnings: Vec::new(),
    };
    let mut script = parse_script(&text, &mut resolver)?;
    let encoded = encode(&mut script)?;
    Ok((
        script
            .statements
            .into_iter()
            .map(|statement| statement.expr)
            .collect(),
        encoded,
    ))
}

/// Reads the written bytes back with the reader, which has to find the statements the text says
/// at the offsets the encoder gave them.
fn read_back(
    encoded: &Encoded,
    script: &ParsedScript,
    header: &retoc::legacy_asset::FLegacyPackageHeader,
    tables: &Tables,
) -> Result<(), Diagnostic> {
    let mut measured = header.clone();
    measured.name_map = tables.names.clone();
    measured.imports = tables.imports.clone();
    let ctx = Ctx {
        mappings: None,
        header: &measured,
        fixups: None,
        synth: None,
        local: None,
    };
    let mut scratch = Diagnostics::default();
    let read = kismet::read_script(
        &encoded.bytes,
        0,
        0,
        Some(encoded.loaded),
        encoded.bytes.len() as u32,
        &ctx,
        &mut scratch,
    );
    let pos_of = |offset: u32| {
        let at = encoded
            .statements
            .iter()
            .rposition(|start| *start <= offset)
            .unwrap_or(0);
        script
            .statements
            .get(at)
            .map_or(Pos::default(), |statement| statement.note.pos)
    };
    if let Some(stop) = &read.stopped {
        return Err(pos_of(stop.offset).error(format!(
            "the bytes this writes do not read back: {}",
            stop.reason
        )));
    }
    let offsets: Vec<u32> = read.statements.iter().map(|s| s.offset).collect();
    if offsets != encoded.statements {
        let at = offsets
            .iter()
            .zip(&encoded.statements)
            .position(|(a, b)| a != b)
            .unwrap_or(offsets.len().min(encoded.statements.len()));
        return Err(pos_of(encoded.statements.get(at).copied().unwrap_or(0))
            .error("the bytes this writes read back as other statements than the text has"));
    }
    for (statement, written) in read.statements.iter().zip(&script.statements) {
        let (was, is) = (kismet::shape(&written.expr), kismet::shape(&statement.expr));
        if was != is {
            return Err(written.note.pos.error(format!(
                "this statement reads back as `{}`",
                script_text::print_expr(&statement.expr)
            )));
        }
    }
    Ok(())
}

/// Why a function's script does not come back byte for byte from its own text.
#[derive(Debug, Clone, Serialize)]
pub struct RoundTripFailure {
    /// A short category the audit counts by.
    pub cause: String,
    pub detail: String,
}

fn failure(cause: impl Into<String>, detail: impl Into<String>) -> RoundTripFailure {
    RoundTripFailure {
        cause: cause.into(),
        detail: detail.into(),
    }
}

/// A message with its numbers and quoted parts taken out, so failures of one kind count together.
fn category(message: &str) -> String {
    let mut out = String::new();
    let mut quoted = None;
    for c in message.chars() {
        match quoted {
            Some(close) if c == close => quoted = None,
            Some(_) => {}
            None if matches!(c, '\'' | '"' | '`') => {
                quoted = Some(c);
                out.push('…');
            }
            None if c.is_ascii_digit() => {
                if !out.ends_with('N') {
                    out.push('N');
                }
            }
            None => out.push(c),
        }
    }
    out.chars().take(100).collect()
}

/// Each function's export, and whether its script came back byte for byte from its own text.
pub type RoundTrips = Vec<(u32, Result<(), RoundTripFailure>)>;

/// Prints every whole script in the package and assembles each text again, which has to give back
/// exactly the bytes it was printed from: each function's export with what became of it.
pub fn script_round_trips(
    bundle: &AssetBundle<'_>,
    parsed: &ParsedPackage,
) -> Result<RoundTrips, String> {
    let header = crate::package::read_header(bundle)?;
    let base = crate::package::header_size(bundle)?;
    let tables = Tables {
        names: header.name_map.clone(),
        imports: header.imports.clone(),
    };
    let mut out = Vec::new();
    for export in &parsed.exports {
        let Some(script) = export.script.as_ref() else {
            continue;
        };
        let result = match &script.stopped {
            Some(stop) => Err(failure("did not decode", stop.reason.clone())),
            None => crate::edit::bytes_at(bundle, base, script.start, script.end)
                .map_err(|e| failure("bytes", e))
                .and_then(|original| {
                    round_trip(parsed, export.index, script, original, &header, &tables)
                }),
        };
        out.push((export.index, result));
    }
    Ok(out)
}

fn round_trip(
    parsed: &ParsedPackage,
    export: u32,
    script: &kismet::Script,
    original: &[u8],
    header: &retoc::legacy_asset::FLegacyPackageHeader,
    tables: &Tables,
) -> Result<(), RoundTripFailure> {
    let text = script_text::print_script(parsed, export)
        .ok_or_else(|| failure("no script", "the export holds no bytecode"))?
        .text();
    let mut tables = tables.clone();
    let assembled = assemble(
        &text,
        parsed,
        export,
        header,
        &mut tables,
        AssembleOptions::default(),
    )
    .map_err(|errors| {
        let first = errors
            .first()
            .map(|e| e.message.clone())
            .unwrap_or_default();
        let line = errors
            .first()
            .and_then(|e| text.lines().nth(e.line.saturating_sub(1) as usize))
            .unwrap_or_default();
        failure(
            format!("does not assemble: {}", category(&first)),
            format!("{first} in `{line}`"),
        )
    })?;
    if assembled.loaded_size != script.buffer_size {
        return Err(failure(
            "loaded size differs",
            format!("{} for {}", assembled.loaded_size, script.buffer_size),
        ));
    }
    let offsets: Vec<u32> = script.statements.iter().map(|s| s.offset).collect();
    if assembled.statements != offsets {
        return Err(failure("statement offsets differ", String::new()));
    }
    if assembled.bytes != original {
        let at = assembled
            .bytes
            .iter()
            .zip(original)
            .position(|(a, b)| a != b)
            .unwrap_or(assembled.bytes.len().min(original.len()));
        let file = script.start + at as u64;
        let inside = script
            .spans
            .iter()
            .filter(|span| span.at <= file && file < span.end_at)
            .min_by_key(|span| span.end_at - span.at)
            .and_then(|span| kismet::token_name(span.token))
            .unwrap_or("the script");
        return Err(failure(
            format!("bytes differ inside {inside}"),
            format!("at file {file:#X}"),
        ));
    }
    if let Some((old, new)) = assembled.moved.iter().find(|(old, new)| old != new) {
        return Err(failure(
            "a label moved",
            format!("@{old:04X} landed at 0x{new:04X}"),
        ));
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::script_fixture::{Code, Function, Import, Package, event_graph, local};

    fn text_of(parsed: &ParsedPackage, export: u32) -> String {
        script_text::print_script(parsed, export)
            .expect("a script")
            .text()
    }

    #[test]
    fn the_event_graph_prints_as_the_text_says() {
        let built = event_graph().build();
        let parsed = built.parsed();
        let graph = &parsed.exports[1];
        let script = graph.script.as_ref().expect("a script");
        assert!(script.complete(), "{:?}", script.stopped);
        assert_eq!(
            text_of(&parsed, 1),
            "Jump LocalVariable(EntryPoint)\n\
             @000A:  ; ReceiveBeginPlay enters here\n\
             Jump @0092 unless LocalVariable(Flag)\n\
             Let LocalVariable(Count) = 5\n\
             Self->LocalFinalFunction /Game/Test.BP_Test_C:ReceiveBeginPlay()\n\
             SwitchValue(LocalVariable(Count), 1 => IntOne, default => IntZero)\n\
             FinalFunction /Script/Engine.KismetSystemLibrary:Delay(StructConst</Script/Engine.LatentActionInfo, 24>(SkipOffsetConst(@0092), 7, 'ExecuteUbergraph_BP_Test', Self))\n\
             @0092:\n\
             Return Nothing\n\
             EndOfScript\n"
        );
        assert_eq!(
            text_of(&parsed, 2),
            "LocalFinalFunction /Game/Test.BP_Test_C:ExecuteUbergraph_BP_Test(10)\n\
             Return Nothing\n\
             EndOfScript\n"
        );
    }

    /// The VM never reads a struct literal's size, so a text may leave it out and gets 0 there,
    /// with every other byte as the compiler wrote it.
    #[test]
    fn a_struct_literal_may_leave_out_its_size() {
        let built = event_graph().build();
        let printed = text_of(&built.parsed(), 1);
        let sized = assemble_over(&built, 1, &printed, ADD)
            .0
            .expect("assembles");
        let bare = printed.replace("LatentActionInfo, 24>", "LatentActionInfo>");
        assert_ne!(bare, printed);
        let bare = assemble_over(&built, 1, &bare, ADD).0.expect("assembles");
        assert_eq!(sized.bytes.len(), bare.bytes.len());
        let differing: Vec<(u8, u8)> = sized
            .bytes
            .iter()
            .zip(&bare.bytes)
            .filter(|(a, b)| a != b)
            .map(|(a, b)| (*a, *b))
            .collect();
        assert_eq!(differing, vec![(24, 0)]);
    }

    #[test]
    fn every_function_comes_back_byte_for_byte() {
        let built = event_graph().build();
        let parsed = built.parsed();
        let results = script_round_trips(&built.bundle(), &parsed).expect("round trips");
        assert_eq!(results.len(), 2);
        for (export, result) in results {
            assert!(
                result.is_ok(),
                "export {export}: {result:?}\n{}",
                text_of(&parsed, export)
            );
        }
    }

    /// One printer prints every function of a package exactly as printing each alone does.
    #[test]
    fn one_printer_prints_every_function_as_print_script_does() {
        for built in [event_graph().build(), catalogue().build()] {
            let parsed = built.parsed();
            let printer = script_text::ScriptPrinter::new(&parsed);
            let scripted: Vec<u32> = parsed
                .exports
                .iter()
                .filter(|export| export.script.is_some())
                .map(|export| export.index)
                .collect();
            assert!(!scripted.is_empty());
            for export in scripted {
                assert_eq!(
                    printer.print(export).expect("printed").text(),
                    text_of(&parsed, export)
                );
            }
        }
    }

    /// A function using every instruction the reader models, in the forms the text has to keep
    /// apart: the literal tokens, numbered names, every text kind, containers, delegates, casts,
    /// contexts with their values, and a NaN.
    fn catalogue() -> Package {
        let mut package = Package::new(
            "BP_Cat_C",
            &[
                "Catalogue",
                "Flag",
                "Count",
                "Values",
                "Damage",
                "Tick",
                "Handler",
                "Health",
                "X",
                "/Script/Engine",
                "Actor",
                "Vector",
                "ScriptStruct",
                "/Game/ST",
                "ST",
                "StringTable",
                "OnHit__DelegateSignature",
                "Other",
            ],
        );
        let engine = package.import(Import {
            class_package: "/Script/CoreUObject",
            class: "Package",
            outer: 0,
            name: "/Script/Engine",
        });
        let actor = package.import(Import {
            class_package: "/Script/CoreUObject",
            class: "Class",
            outer: engine,
            name: "Actor",
        });
        let vector = package.import(Import {
            class_package: "/Script/CoreUObject",
            class: "ScriptStruct",
            outer: -1,
            name: "Vector",
        });
        let table_package = package.import(Import {
            class_package: "/Script/CoreUObject",
            class: "Package",
            outer: 0,
            name: "/Game/ST",
        });
        let table = package.import(Import {
            class_package: "/Script/Engine",
            class: "StringTable",
            outer: table_package,
            name: "ST",
        });
        let signature = package.import(Import {
            class_package: "/Script/CoreUObject",
            class: "Function",
            outer: actor,
            name: "OnHit__DelegateSignature",
        });
        package.members.push(local("Health", "IntProperty"));
        let own = package.function_index(0);
        let class = package.class_index();
        // The function is the package's second export, after its class.
        assert_eq!(own, 2);
        fn var<'p>(code: Code<'p>, name: &str) -> Code<'p> {
            code.op(0x00).field(name, 2)
        }
        fn string<'p>(code: Code<'p>, value: &[u8]) -> Code<'p> {
            code.op(0x1F).ops(value).op(0)
        }
        let mut code = Code::new(&package)
            // Assert<12, 7>(True), NothingInt32(5)
            .ops(&[0x09, 0x0C, 0x00, 0x07, 0x27])
            .op(0x0C)
            .int(5)
            // LetBool LocalVariable(Flag) = False
            .op(0x14);
        code = var(code, "Flag").op(0x28);
        // BitFieldConst(Flag in …, 1), ObjectConst(None)
        code = code.op(0x11).field("Flag", own).op(1).op(0x20).int(0);
        // the wide and narrow numbers
        code = code
            .op(0x35)
            .ops(&5_000_000_000i64.to_le_bytes())
            .op(0x36)
            .ops(&u64::MAX.to_le_bytes())
            .op(0x1E)
            .ops(&1.5f32.to_le_bytes())
            .op(0x37)
            .ops(&0.1f64.to_le_bytes())
            .op(0x37)
            .ops(&(-0.0f64).to_le_bytes())
            .op(0x1E)
            .ops(&f32::from_bits(0x7FC0_0001).to_le_bytes())
            .op(0x37)
            .ops(&f64::INFINITY.to_le_bytes())
            .ops(&[0x24, 3, 0x2C, 24]);
        // strings: quotes, a line break and a Latin-1 byte; then UTF-16
        code = string(code, &[b'a', b'"', b'b', b'\n', 0xE9, 0x7F]);
        code = code.op(0x34);
        for unit in "h\u{e9}llo \u{1F600}".encode_utf16() {
            code = code.ops(&unit.to_le_bytes());
        }
        code = code.ops(&[0, 0]);
        // names: numbered, and one only an instance delegate holds
        code = code.op(0x21).numbered("Damage", 3).op(0x4B).name("Tick");
        // vectors and transforms
        code = code.op(0x23);
        for value in [1.0f64, -2.5, 0.0] {
            code = code.ops(&value.to_le_bytes());
        }
        code = code.op(0x22);
        for value in [0.0f64, 90.0, 180.0] {
            code = code.ops(&value.to_le_bytes());
        }
        code = code.op(0x2B);
        for value in 0..10 {
            code = code.ops(&f64::from(value).to_le_bytes());
        }
        code = code.op(0x41);
        for value in [1.0f32, 0.1, -3.0] {
            code = code.ops(&value.to_le_bytes());
        }
        // every text kind
        code = code.ops(&[0x29, 0x00, 0x29, 0x02]);
        code = string(code, b"Hi");
        code = code.ops(&[0x29, 0x03]);
        code = string(code, b"Lit");
        code = code.ops(&[0x29, 0x01]);
        code = string(code, b"Source");
        code = string(code, b"KEY");
        code = string(code, b"NS");
        code = code.ops(&[0x29, 0x04]).int(table);
        code = string(code, b"/Game/ST.ST");
        code = string(code, b"Play");
        // containers
        code = var(code.op(0x31), "Values");
        code = code.op(0x1D).int(1).op(0x1D).int(2).op(0x32);
        code = var(code.op(0x39), "Values").int(1).op(0x26).op(0x3A);
        code = var(code.op(0x3B), "Values").int(1).ops(&[0x25, 0x26, 0x3C]);
        code = code
            .op(0x65)
            .field("Values", own)
            .int(2)
            .ops(&[0x25, 0x26, 0x66]);
        code = code.op(0x3D).field("Values", own).int(5).ops(&[0x25, 0x3E]);
        code = code
            .op(0x3F)
            .field("Values", own)
            .field("Count", own)
            .int(1)
            .ops(&[0x25, 0x26, 0x40]);
        // members, casts and conversions
        code = var(code.op(0x42).field("X", vector), "Values");
        code = var(code.op(0x64).field("Count", own), "Count");
        code = var(code.ops(&[0x38, 0x03]), "Values");
        code = code.ops(&[0x38, 0x41, 0x17]);
        code = code.op(0x2E).int(actor).op(0x17);
        code = code.op(0x13).int(actor).op(0x17);
        // the one-operand forms
        code = code.ops(&[0x51, 0x17, 0x4F, 0x27]);
        code = var(code.op(0x5D), "Values");
        code = string(code.op(0x67), b"/Game/A.A");
        code = code.op(0x6D).op(0x33).field("Count", own);
        // delegates
        code = var(var(code.op(0x5C), "Values"), "Count");
        code = var(var(code.op(0x62), "Values"), "Count");
        code = var(code.op(0x61).name("Handler"), "Values").op(0x17);
        code = var(code.op(0x63).int(signature), "Values")
            .op(0x1D)
            .int(4)
            .op(0x16);
        // instrumentation, array access, the one-byte forms
        code = code.ops(&[0x6A, 0x02, 0x6A, 0x04]).name("Tick");
        code = var(code.op(0x6B), "Values").op(0x25);
        code = code.ops(&[0x2A, 0x2D, 0x50, 0x5E, 0x5A, 0x4A, 0x4D]);
        // variables of every role, one owned elsewhere
        code = code.op(0x48).field("Count", own);
        code = code.op(0x01).field("Health", class);
        code = code.op(0x02).field("Health", class);
        code = code.op(0x6C).field("Health", class);
        code = code.op(0x01).field("Other", actor);
        // contexts: fail-silent with its value, a class context, and a Let naming another field
        code = code.ops(&[0x1A, 0x17]).word(9).field("Health", class);
        code = code.op(0x01).field("Health", class);
        code = code.ops(&[0x12, 0x17]).word(10).int(0).int(0);
        code = code.op(0x1B).name("Tick").op(0x16);
        code = code.op(0x0F).field("Flag", own);
        code = var(code, "Count").op(0x26);
        // calls
        code = code.op(0x45).name("Tick").op(0x16);
        code = code.op(0x68).int(own).op(0x25).op(0x16);
        // a skip, and the end
        code = code.op(0x18).word(1).op(0x27);
        let script = code.ops(&[0x04, 0x0B, 0x53]).bytes;
        package.functions.push(Function {
            name: "Catalogue",
            fields: vec![
                local("Flag", "BoolProperty"),
                local("Count", "IntProperty"),
                local("Values", "IntProperty"),
            ],
            script,
        });
        package
    }

    #[test]
    fn every_modelled_instruction_comes_back_byte_for_byte() {
        let built = catalogue().build();
        let parsed = built.parsed();
        let script = parsed.exports[1].script.as_ref().expect("a script");
        assert!(script.complete(), "{:?}", script.stopped);
        let results = script_round_trips(&built.bundle(), &parsed).expect("round trips");
        for (export, result) in &results {
            assert!(
                result.is_ok(),
                "export {export}: {result:?}\n{}",
                text_of(&parsed, *export)
            );
        }
        let text = text_of(&parsed, 1);
        for line in [
            "Assert<12, 7>(True)",
            "NothingInt32(5)",
            "LetBool LocalVariable(Flag) = False",
            "BitFieldConst(Flag in /Game/Test.BP_Cat_C:Catalogue, 1)",
            "ObjectConst(None)",
            "Int64Const(5000000000)",
            "UInt64Const(18446744073709551615)",
            "1.5f",
            "0.1",
            "-0.0",
            "FloatConst(nan(0x7FC00001))",
            "DoubleConst(inf)",
            "ByteConst(3)",
            "IntConstByte(24)",
            "\"a\\\"b\\n\u{e9}\\x7F\"",
            "u\"h\u{e9}llo \u{1F600}\"",
            "'Damage_2'",
            "InstanceDelegate('Tick')",
            "VectorConst(1.0, -2.5, 0.0)",
            "Vector3fConst(1.0, 0.1, -3.0)",
            "TextConst<Empty>",
            "TextConst<Invariant>(\"Hi\")",
            "TextConst<Literal>(\"Lit\")",
            "TextConst<Localized>(\"Source\", \"KEY\", \"NS\")",
            "TextConst<StringTable, /Game/ST.ST>(\"/Game/ST.ST\", \"Play\")",
            "SetArray(LocalVariable(Values), 1, 2)",
            "SetSet(LocalVariable(Values), IntOne)",
            "SetMap(LocalVariable(Values), IntZero, IntOne)",
            "ArrayConst<Values in /Game/Test.BP_Cat_C:Catalogue>(IntZero, IntOne)",
            "SetConst<Values in /Game/Test.BP_Cat_C:Catalogue, #5>(IntZero)",
            "StructMemberContext<X in /Script/CoreUObject.Vector>(LocalVariable(Values))",
            "Cast<DoubleToFloat>(LocalVariable(Values))",
            "Cast<0x41>(Self)",
            "DynamicCast</Script/Engine.Actor>(Self)",
            "MetaCast</Script/Engine.Actor>(Self)",
            "InterfaceContext(Self)",
            "SoftObjectConst(\"/Game/A.A\")",
            "BindDelegate<Handler>(LocalVariable(Values), Self)",
            "CallMulticastDelegate /Script/Engine.Actor:OnHit__DelegateSignature(LocalVariable(Values), 4)",
            "InstrumentationEvent(4, 'Tick')",
            "ArrayGetByRef(LocalVariable(Values), IntZero)",
            "LocalOutVariable(Count)",
            "InstanceVariable(Health)",
            "InstanceVariable(Other in /Script/Engine.Actor)",
            "Self?->InstanceVariable(Health)",
            "Self::#10 VirtualFunction Tick()",
            "Let<Flag> LocalVariable(Count) = IntOne",
            "CallMath /Game/Test.BP_Cat_C:Catalogue(IntZero)",
            "Skip<#1>(True)",
        ] {
            assert!(
                text.lines().any(|l| l == line),
                "no line `{line}` in\n{text}"
            );
        }
    }

    fn assemble_over(
        built: &crate::script_fixture::Built,
        export: u32,
        text: &str,
        options: AssembleOptions,
    ) -> (Result<Assembled, Vec<Diagnostic>>, Tables) {
        let header = built.header();
        let parsed = built.parsed();
        let mut tables = Tables {
            names: header.name_map.clone(),
            imports: header.imports.clone(),
        };
        let result = assemble(text, &parsed, export, &header, &mut tables, options);
        (result, tables)
    }

    const ADD: AssembleOptions = AssembleOptions {
        add_names: true,
        add_imports: true,
    };

    #[test]
    fn an_inserted_statement_moves_every_label_after_it() {
        let built = event_graph().build();
        let text = text_of(&built.parsed(), 1).replace(
            "Let LocalVariable(Count) = 5\n",
            "Let LocalVariable(Count) = 5\nLet LocalVariable(Count) = 6\n",
        );
        let (assembled, _) = assemble_over(&built, 1, &text, AssembleOptions::default());
        let assembled = assembled.expect("assembles");
        // One more `Let`: 1 + 8 + 9 + 5 loaded bytes.
        assert_eq!(assembled.loaded_size, 0x95 + 23);
        assert_eq!(
            assembled.moved,
            BTreeMap::from([(0x0A, 0x0A), (0x92, 0x92 + 23), (0x95, 0x95 + 23)])
        );
        assert_eq!(assembled.statements.len(), 9);
        // The branch and the latent action's resume point both follow the label.
        let target = (0x92u32 + 23).to_le_bytes();
        let pointing = assembled
            .bytes
            .windows(5)
            .filter(|w| (w[0] == 0x07 || w[0] == 0x5B) && w[1..] == target)
            .count();
        assert_eq!(pointing, 2, "both offsets point at the return");
    }

    #[test]
    fn reordered_statements_keep_their_labels() {
        let built = event_graph().build();
        let text = text_of(&built.parsed(), 1).replace(
            "Let LocalVariable(Count) = 5\nSelf->LocalFinalFunction /Game/Test.BP_Test_C:ReceiveBeginPlay()\n",
            "Self->LocalFinalFunction /Game/Test.BP_Test_C:ReceiveBeginPlay()\nLet LocalVariable(Count) = 5\n",
        );
        let (assembled, _) = assemble_over(&built, 1, &text, AssembleOptions::default());
        let assembled = assembled.expect("assembles");
        assert_eq!(assembled.loaded_size, 0x95);
        assert_eq!(assembled.statements[2], 0x18);
        assert_eq!(assembled.statements[3], 0x18 + 24);
        assert_eq!(assembled.moved.get(&0x92), Some(&0x92));
    }

    #[test]
    fn a_parse_error_names_its_line_and_column() {
        let built = event_graph().build();
        let (result, _) = assemble_over(
            &built,
            2,
            "LocalFinalFunction /Game/Test.BP_Test_C:ExecuteUbergraph_BP_Test(10\n\
             Return Nothing\n\
             EndOfScript\n",
            AssembleOptions::default(),
        );
        let errors = result.expect_err("refused");
        assert_eq!((errors[0].line, errors[0].column), (2, 1), "{errors:?}");
    }

    #[test]
    fn labels_must_be_defined_once_and_used_where_defined() {
        let built = event_graph().build();
        let (result, _) = assemble_over(
            &built,
            2,
            "Jump @nowhere\nEndOfScript\n",
            AssembleOptions::default(),
        );
        let errors = result.expect_err("refused");
        assert!(
            errors[0].message.contains("@nowhere is not defined"),
            "{errors:?}"
        );
        let (result, _) = assemble_over(
            &built,
            2,
            "@here:\nReturn Nothing\n@here:\nEndOfScript\n",
            AssembleOptions::default(),
        );
        let errors = result.expect_err("refused");
        assert!(
            errors[0].message.contains("already defined on line 1"),
            "{errors:?}"
        );
    }

    /// Where the VM evaluates an operand with nowhere to put a result and reads the variable it
    /// leaves behind, a call or a literal would be written through a null pointer in game.
    #[test]
    fn an_operand_the_vm_reads_as_a_variable_has_to_be_one() {
        let built = event_graph().build();
        let add = "CallMath /Script/Engine.KismetMathLibrary:Add_IntInt(1, 2)";
        for (text, column) in [
            ("Cast<DoubleToFloat>(1.0)".to_string(), 48),
            (format!("Cast<FloatToDouble>({add})"), 48),
            (format!("SwitchValue({add}, default => 1)"), 40),
        ] {
            let text = format!("Let LocalVariable(Count) = {text}\n");
            let (result, _) = assemble_over(&built, 1, &text, ADD);
            let errors = result.expect_err("refused");
            assert_eq!(
                (errors[0].line, errors[0].column),
                (1, column),
                "{errors:?}"
            );
            assert!(
                errors[0].message.contains("has to be a variable"),
                "{errors:?}"
            );
        }
        for text in [
            "Cast<DoubleToFloat>(LocalVariable(Count))",
            "SwitchValue(LocalVariable(Flag), default => 1)",
        ] {
            let text = format!("Let LocalVariable(Count) = {text}\n");
            let (result, _) = assemble_over(&built, 1, &text, ADD);
            if let Err(errors) = result {
                assert!(
                    errors
                        .iter()
                        .all(|error| !error.message.contains("has to be a variable")),
                    "{errors:?}"
                );
            }
        }
    }

    #[test]
    fn a_local_the_function_does_not_have_is_refused() {
        let built = event_graph().build();
        let (result, _) = assemble_over(&built, 1, "Let LocalVariable(Missing) = 5\n", ADD);
        let errors = result.expect_err("refused");
        assert_eq!((errors[0].line, errors[0].column), (1, 19), "{errors:?}");
        assert!(
            errors[0]
                .message
                .contains("Missing is not a parameter or local of ExecuteUbergraph_BP_Test"),
            "{errors:?}"
        );
    }

    #[test]
    fn a_new_name_or_object_needs_leave_to_add_it() {
        let built = event_graph().build();
        let text = "VirtualFunction Brand_New()\n\
                    LocalFinalFunction /Script/Engine.KismetMathLibrary:Add_IntInt(1, 2)\n\
                    EndOfScript\n";
        let (result, _) = assemble_over(&built, 2, text, AssembleOptions::default());
        let errors = result.expect_err("refused");
        assert!(
            errors[0].message.contains("no name 'Brand_New'"),
            "{errors:?}"
        );
        let names = built.header().name_map.num_names();

        let (result, tables) = assemble_over(&built, 2, text, ADD);
        let assembled = result.expect("assembles");
        assert!(tables.names.num_names() > names, "the names grew");
        let callee = tables.imports.last().expect("an import was added");
        let name = |id| tables.names.get(id).expect("a name").into_owned();
        assert_eq!(name(callee.object_name), "Add_IntInt");
        assert_eq!(name(callee.class_name), "Function");
        // `Brand_New` is stored as `Brand` numbered 1: the number is split off as UE does.
        assert_eq!(&assembled.bytes[1..5], &(names as i32).to_le_bytes());
        assert_eq!(&assembled.bytes[5..9], &0i32.to_le_bytes());
        assert_eq!(
            name(retoc::legacy_asset::FMinimalName {
                index: names as i32,
                number: 0,
            }),
            "Brand_New"
        );
    }

    #[test]
    fn tables_change_only_when_the_text_assembles() {
        let built = event_graph().build();
        let (result, tables) = assemble_over(
            &built,
            2,
            "VirtualFunction Brand_New()\nJump @nowhere\n",
            ADD,
        );
        assert!(result.is_err());
        assert_eq!(
            tables.names.num_names(),
            built.header().name_map.num_names()
        );
    }
}
