//! Edits inside a function's bytecode, each addressed by the expression it changes: a literal
//! given a value, an object pointed elsewhere, or a branch's condition fixed.
//!
//! Each edit replaces one run of a script's bytes. Where the new bytes take another length, the
//! script is relocated: everything pointing into it, in it or from other functions, moves with the
//! code it named. Verification rebuilds the script the edits should leave from the one that was
//! read, with every offset mapped, then holds the saved script to it statement by statement.

use retoc::zen::FPackageIndex;

use crate::edit::{
    AppliedEdit, DRIFT, PackageEdits, ScriptConstEdit, encode_object, object_index, payload_range,
};
use crate::header_edit::{Tables, add_import};
use crate::kismet::{self, Expr, FixupKind, Script, Span};
use crate::package::{AssetBundle, ParsedExport, ParsedPackage, path_from};
use crate::relocate::{Change, OffsetMap, Relocated, relocate};
use crate::write::Splice;

/// The expression an edit lands on.
struct Located<'a> {
    export: &'a ParsedExport,
    script: &'a Script,
    /// Which statement holds it, by position.
    statement: usize,
    /// The expression's position among the script's spans, which is pre-order.
    index: usize,
    expr: &'a Expr,
    span: Span,
}

/// What an edit writes in place of the bytes it covers.
#[derive(Debug, Clone)]
enum Replacement {
    /// A literal's value bytes after its token, in its own kind and at any width.
    Value(Expr),
    /// The whole expression written anew as this literal, under its own token.
    Literal(Expr),
    /// The token byte alone.
    Token(u8),
    /// The four-byte offset after the token, as an offset in the script as it was read.
    Offset(u32),
    /// An object index named by its path: after the token of an object constant, or with a token
    /// of its own in place of a `NoObject`.
    Object { path: String, widen: bool },
    /// A whole text constant, with the string table its entry names when it names one.
    Text { text: Expr, table: Option<String> },
    /// The function a final call names, by its path.
    Callee(String),
    /// The name a virtual call looks its function up by.
    Name(String),
}

/// How the edited script should read once saved.
#[derive(Debug, Clone)]
enum Expect {
    /// The expression reads as this.
    Node(Expr),
    /// The statement reads as these, the first where it was and the rest after it.
    Statements(Vec<Expr>),
    /// An object constant names this path, or nothing.
    Object(String),
    /// A text constant reads as this, its table, when it names one, taken from what was saved.
    Text(Expr),
    /// A final call reads as this one, naming the function at the path.
    Callee(Expr, String),
}

/// The run of bytes an edit replaces, in the file and once loaded, and the room its replacement
/// takes in each.
#[derive(Debug, Clone, Copy)]
struct Extent {
    at: u64,
    end_at: u64,
    offset: u32,
    end_offset: u32,
    file_len: u64,
    loaded_len: u32,
}

/// One edit, worked out against the script it was written for.
struct Planned {
    export: u32,
    statement: usize,
    index: usize,
    span: Span,
    replacement: Replacement,
    expect: Expect,
    extent: Extent,
    name: String,
    before: String,
    after: String,
    /// What the save should say about this edit, such as a text no longer following its table.
    note: Option<String>,
}

/// The token bytes the forms an edit writes take.
const TOKEN_JUMP: u8 = 0x06;
const TOKEN_NOTHING: u8 = 0x0B;
const TOKEN_INT_CONST: u8 = 0x1D;
const TOKEN_STRING_CONST: u8 = 0x1F;
const TOKEN_OBJECT_CONST: u8 = 0x20;
const TOKEN_INT_ZERO: u8 = 0x25;
const TOKEN_INT_ONE: u8 = 0x26;
const TOKEN_NO_OBJECT: u8 = 0x2A;
const TOKEN_INT_CONST_BYTE: u8 = 0x2C;
const TOKEN_TRUE: u8 = 0x27;
const TOKEN_FALSE: u8 = 0x28;
const TOKEN_UNICODE_STRING_CONST: u8 = 0x34;
const TOKEN_POP_FLOW: u8 = 0x4D;
const TOKEN_TEXT_CONST: u8 = 0x29;

/// An object index is four bytes on disk and a pointer once loaded.
const LOADED_POINTER: u32 = 8;

/// The splices and dependency links a save's script edits make.
pub(crate) struct ScriptSplices {
    pub splices: Vec<Splice>,
    pub applied: Vec<AppliedEdit>,
    /// `(file offset of an object index, the index)`, for the edges the cook would write.
    pub links: Vec<(u64, i32)>,
    pub notes: Vec<String>,
}

/// The line a save reports a script's change of size with, when it changed size.
fn layout_line(
    parsed: &ParsedPackage,
    export: u32,
    owner: &str,
    relocated: &Relocated,
) -> Option<AppliedEdit> {
    let ((loaded, stored), (now_loaded, now_stored)) = relocated.sizes?;
    let sizes_at = parsed
        .exports
        .iter()
        .find(|e| e.index == export)
        .and_then(|e| e.script.as_ref())
        .map_or(0, |script| script.sizes_at);
    Some(AppliedEdit {
        name: format!("{owner} script layout"),
        offset: sizes_at,
        offset_after: sizes_at,
        element: None,
        elements_after: None,
        before: format!("{loaded} bytes loaded, {stored} stored"),
        after: format!(
            "{now_loaded} bytes loaded, {now_stored} stored; {} offset(s), {} event entr{} and {} latent resume point(s) moved",
            relocated.jumps,
            relocated.entries,
            if relocated.entries == 1 { "y" } else { "ies" },
            relocated.linkages
        ),
    })
}

/// Works out every script edit in `edits`, the bytes each writes, and everything that moves.
pub(crate) fn script_splices(
    parsed: &ParsedPackage,
    edits: &PackageEdits,
    tables: &mut Tables,
    package: &retoc::legacy_asset::FLegacyPackageHeader,
    bundle: &AssetBundle<'_>,
    base: u64,
) -> Result<ScriptSplices, String> {
    let mut out = ScriptSplices {
        splices: Vec::new(),
        applied: Vec::new(),
        links: Vec::new(),
        notes: Vec::new(),
    };
    let plans = plan_all(parsed, edits, edits.allow_drift)?;
    let mut labelled: Vec<(Splice, String)> = Vec::new();
    let mut exports: Vec<u32> = plans.iter().map(|plan| plan.export).collect();
    exports.sort_unstable();
    exports.dedup();
    for export in exports {
        let mine: Vec<&Planned> = plans.iter().filter(|plan| plan.export == export).collect();
        // An offset an edit writes counts in the script as it will be, which only the extents of
        // every edit in it decide.
        let map = map_of(&mine);
        let mut changes = Vec::with_capacity(mine.len());
        for plan in &mine {
            let bytes = bytes_for(parsed, plan, &map, tables, package, &mut out.links)?;
            if bytes.len() as u64 != plan.extent.file_len {
                return Err(format!(
                    "{}: the new bytes take {} where {} were worked out",
                    plan.name,
                    bytes.len(),
                    plan.extent.file_len
                ));
            }
            changes.push(Change {
                at: plan.extent.at,
                end_at: plan.extent.end_at,
                offset: plan.extent.offset,
                end_offset: plan.extent.end_offset,
                bytes,
                loaded_len: plan.extent.loaded_len,
                label: plan.name.clone(),
            });
            out.notes.extend(plan.note.clone());
            out.applied.push(AppliedEdit {
                name: plan.name.clone(),
                offset: plan.span.at,
                offset_after: plan.span.at,
                element: None,
                elements_after: None,
                before: plan.before.clone(),
                after: plan.after.clone(),
            });
        }
        let relocated = relocate(parsed, export, changes)?;
        let owner = mine.first().map_or("", |plan| plan.name.as_str());
        let owner = owner.split(" script").next().unwrap_or(owner);
        out.applied
            .extend(layout_line(parsed, export, owner, &relocated));
        labelled.extend(relocated.splices);
    }
    for edit in &edits.script_texts {
        let text =
            crate::script_text_edit::plan_text(parsed, edits, edit, tables, package, bundle, base)?;
        out.links.extend(text.links);
        out.notes.extend(text.notes);
        out.applied.extend(text.applied);
        let owner = parsed
            .exports
            .get(edit.export as usize)
            .map_or_else(|| edit.export.to_string(), |e| e.object_name.clone());
        out.applied
            .extend(layout_line(parsed, edit.export, &owner, &text.relocated));
        labelled.extend(text.relocated.splices);
    }
    // An edit in one script can land on bytes another script's relocation rewrites, such as the
    // entry a stub passes an event graph this save moves, or inside a stub given a whole new payload.
    let mut claimed: Vec<(u64, u64, String)> = labelled
        .iter()
        .map(|(splice, label)| (splice.start, splice.end, label.clone()))
        .collect();
    for payload in &edits.payloads {
        if let Some(export) = parsed.exports.iter().find(|e| e.index == payload.export)
            && let Some((start, end)) = payload_range(export)
        {
            claimed.push((
                start,
                end,
                format!("the new payload for {}", export.object_name),
            ));
        }
    }
    claimed.sort_by_key(|(start, end, _)| (*start, *end));
    for pair in claimed.windows(2) {
        if pair[0].1 > pair[1].0 {
            return Err(format!(
                "{} and {} are the same bytes, and this save writes both",
                pair[0].2, pair[1].2
            ));
        }
    }
    labelled.sort_by_key(|(splice, _)| (splice.start, splice.end));
    out.splices = labelled.into_iter().map(|(splice, _)| splice).collect();
    Ok(out)
}

/// The offset map a script's planned edits make, from their extents alone.
fn map_of(plans: &[&Planned]) -> OffsetMap {
    OffsetMap::from_moves(
        plans
            .iter()
            .map(|plan| {
                let extent = plan.extent;
                (
                    extent.offset,
                    extent.end_offset,
                    i64::from(extent.loaded_len) - i64::from(extent.end_offset - extent.offset),
                )
            })
            .collect(),
    )
}

/// The bytes one planned edit writes.
fn bytes_for(
    parsed: &ParsedPackage,
    plan: &Planned,
    map: &OffsetMap,
    tables: &mut Tables,
    package: &retoc::legacy_asset::FLegacyPackageHeader,
    links: &mut Vec<(u64, i32)>,
) -> Result<Vec<u8>, String> {
    Ok(match &plan.replacement {
        Replacement::Value(new) => kismet::literal_bytes(new, &mut tables.names)?,
        Replacement::Literal(new) => {
            let mut bytes = vec![literal_token(new)?];
            if !matches!(new, Expr::Simple { .. }) {
                bytes.extend(kismet::literal_bytes(new, &mut tables.names)?);
            }
            bytes
        }
        Replacement::Token(token) => vec![*token],
        Replacement::Offset(target) => map
            .map(*target)
            .map_err(|reason| format!("{}: {reason}", plan.name))?
            .to_le_bytes()
            .to_vec(),
        Replacement::Object { path, widen } => {
            let was = match located_expr(parsed, plan) {
                Some(Expr::ObjectConst { object }) => object.index,
                _ => 0,
            };
            let index_bytes = encode_object(path, package, tables, Some(was))?;
            let index = i32::from_le_bytes(
                index_bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| "an object index is four bytes".to_string())?,
            );
            check_object(parsed, package, tables, was, index, path)?;
            let at = if *widen {
                plan.extent.at + 1
            } else {
                plan.extent.at
            };
            if index != 0 {
                links.push((at, index));
            }
            if *widen {
                let mut bytes = vec![TOKEN_OBJECT_CONST];
                bytes.extend(index_bytes);
                bytes
            } else {
                index_bytes
            }
        }
        Replacement::Callee(path) => {
            let index = match object_index(package, tables, path) {
                Some(index) => index,
                None => add_import(
                    tables,
                    path,
                    Some(("/Script/CoreUObject".to_string(), "Function".to_string())),
                )?,
            };
            let canonical = path_from(
                &tables.names,
                &tables.imports,
                &package.exports,
                &package.summary.package_name,
                FPackageIndex { index },
            )
            .unwrap_or_default();
            if canonical != *path {
                return Err(format!(
                    "write the function as the disassembly names it, {canonical}, rather than {path}"
                ));
            }
            links.push((plan.extent.at, index));
            index.to_le_bytes().to_vec()
        }
        Replacement::Name(name) => kismet::literal_bytes(
            &Expr::NameConst {
                name: "NameConst",
                value: name.clone(),
                at: 0,
                id: kismet::NameId::default(),
            },
            &mut tables.names,
        )?,
        Replacement::Text { text, table } => {
            let index = match table {
                Some(path) => {
                    let index = match object_index(package, tables, path) {
                        Some(index) => index,
                        None => add_import(
                            tables,
                            path,
                            Some(("/Script/Engine".to_string(), "StringTable".to_string())),
                        )?,
                    };
                    links.push((plan.extent.at + 2, index));
                    Some(index)
                }
                None => None,
            };
            text_bytes(text, index, &mut tables.names)?
        }
    })
}

/// The bytes of a text constant: its token, the form it takes, and each of its parts.
fn text_bytes(
    text: &Expr,
    table: Option<i32>,
    names: &mut retoc::legacy_asset::FPackageNameMap,
) -> Result<Vec<u8>, String> {
    let Expr::TextConst { text } = text else {
        return Err("not a text constant".to_string());
    };
    let mut out = vec![TOKEN_TEXT_CONST];
    let mut string = |out: &mut Vec<u8>, expr: &Expr| -> Result<(), String> {
        out.push(literal_token(expr)?);
        out.extend(kismet::literal_bytes(expr, names)?);
        Ok(())
    };
    match text {
        kismet::TextLiteral::Empty => out.push(0),
        kismet::TextLiteral::Localized {
            source,
            key,
            namespace,
        } => {
            out.push(1);
            string(&mut out, source)?;
            string(&mut out, key)?;
            string(&mut out, namespace)?;
        }
        kismet::TextLiteral::Invariant { source } => {
            out.push(2);
            string(&mut out, source)?;
        }
        kismet::TextLiteral::Literal { source } => {
            out.push(3);
            string(&mut out, source)?;
        }
        kismet::TextLiteral::StringTable { table_id, key, .. } => {
            out.push(4);
            out.extend_from_slice(&table.unwrap_or(0).to_le_bytes());
            string(&mut out, table_id)?;
            string(&mut out, key)?;
        }
    }
    Ok(out)
}

/// How long a text constant is on disk and once loaded: the same, but for the table it names,
/// which is an object index stored and a pointer loaded.
fn text_length(text: &Expr) -> (u64, u32) {
    let Expr::TextConst { text } = text else {
        return (0, 0);
    };
    let string = |expr: &Expr| 1 + kismet::stored_width(expr);
    let (parts, table) = match text {
        kismet::TextLiteral::Empty => (0, false),
        kismet::TextLiteral::Localized {
            source,
            key,
            namespace,
        } => (string(source) + string(key) + string(namespace), false),
        kismet::TextLiteral::Invariant { source } | kismet::TextLiteral::Literal { source } => {
            (string(source), false)
        }
        kismet::TextLiteral::StringTable { table_id, key, .. } => {
            (string(table_id) + string(key), true)
        }
    };
    let file = 2 + parts + if table { 4 } else { 0 };
    let loaded = 2 + parts + if table { u64::from(LOADED_POINTER) } else { 0 };
    (file, loaded as u32)
}

/// A string as a script stores one: one byte per character while every one is ASCII.
fn string_expr(value: &str) -> Expr {
    if value.chars().all(|c| u32::from(c) <= 0x7F) {
        Expr::StringConst {
            value: value.to_string(),
            at: 0,
        }
    } else {
        Expr::UnicodeStringConst {
            value: value.to_string(),
            at: 0,
        }
    }
}

/// What a text constant becomes when `value` is typed at it. A text literal in UE's own syntax is
/// taken whole; a plain string edits the text's source where it has one, repoints a string table
/// text given `Table:Key`, and otherwise makes a table text show exactly what was typed.
fn text_change(
    old: &kismet::TextLiteral,
    value: &str,
) -> Result<(kismet::TextLiteral, Option<String>), String> {
    use crate::text_literal::TextLiteral as Typed;
    if value.contains('\0') {
        return Err(
            "a string in a script ends at its first NUL, so it cannot hold one".to_string(),
        );
    }
    let typed = match crate::text_literal::parse(value) {
        Some(parsed) => Some(parsed?),
        None => None,
    };
    let source = |text: &str| Box::new(string_expr(text));
    let table = |table_id: &str, key: &str| kismet::TextLiteral::StringTable {
        table: kismet::ObjectRef {
            index: 0,
            path: Some(table_id.to_string()),
        },
        table_id: source(table_id),
        key: source(key),
    };
    Ok(match typed {
        Some(Typed::Invariant(text)) => (
            kismet::TextLiteral::Invariant {
                source: source(&text),
            },
            None,
        ),
        Some(Typed::Localized {
            namespace,
            key,
            source: text,
        }) => (
            kismet::TextLiteral::Localized {
                source: source(&text),
                key: source(&key),
                namespace: source(&namespace),
            },
            None,
        ),
        Some(Typed::Table { table_id, key }) => (table(&table_id, &key), None),
        Some(Typed::Transform { .. }) => {
            return Err(
                "a text in a script has no transformed form: LOCGEN_TOUPPER and LOCGEN_TOLOWER only exist in properties"
                    .to_string(),
            );
        }
        None => match old {
            kismet::TextLiteral::Empty | kismet::TextLiteral::Invariant { .. } => (
                kismet::TextLiteral::Invariant {
                    source: source(value),
                },
                None,
            ),
            kismet::TextLiteral::Literal { .. } => (
                kismet::TextLiteral::Literal {
                    source: source(value),
                },
                None,
            ),
            kismet::TextLiteral::Localized { key, namespace, .. } => (
                kismet::TextLiteral::Localized {
                    source: source(value),
                    key: key.clone(),
                    namespace: namespace.clone(),
                },
                None,
            ),
            kismet::TextLiteral::StringTable { table_id, key, .. } => {
                let current = kismet::string_of(table_id);
                match crate::text_literal::table_reference(value, current) {
                    Some((table_id, key)) => (table(&table_id, &key), None),
                    None => (
                        kismet::TextLiteral::Invariant {
                            source: source(value),
                        },
                        Some(format!(
                            "the text no longer follows {current}:{}; it shows {value:?} in every language",
                            kismet::string_of(key)
                        )),
                    ),
                }
            }
        },
    })
}

/// The token a literal is written under.
fn literal_token(expr: &Expr) -> Result<u8, String> {
    Ok(match expr {
        Expr::IntConst { .. } => TOKEN_INT_CONST,
        Expr::StringConst { .. } => TOKEN_STRING_CONST,
        Expr::UnicodeStringConst { .. } => TOKEN_UNICODE_STRING_CONST,
        Expr::Simple { name: "True" } => TOKEN_TRUE,
        Expr::Simple { name: "IntZero" } => TOKEN_INT_ZERO,
        Expr::Simple { name: "IntOne" } => TOKEN_INT_ONE,
        Expr::Simple { name: "NoObject" } => TOKEN_NO_OBJECT,
        Expr::ByteConst {
            name: "IntConstByte",
            ..
        } => TOKEN_INT_CONST_BYTE,
        other => {
            return Err(format!(
                "{} is not written as a literal of its own",
                kismet::render(other)
            ));
        }
    })
}

/// The expression a plan was made against, read again from the package.
fn located_expr<'a>(parsed: &'a ParsedPackage, plan: &Planned) -> Option<&'a Expr> {
    let script = parsed
        .exports
        .iter()
        .find(|export| export.index == plan.export)?
        .script
        .as_ref()?;
    nodes(script)
        .into_iter()
        .nth(plan.index)
        .map(|(expr, _)| expr)
}

/// A script constant points at the same kind of thing it pointed at before: the class of the new
/// object has to be the old one's, where both are known, and the path has to be written the way
/// the disassembly prints it so the saved script can be held to it.
fn check_object(
    parsed: &ParsedPackage,
    package: &retoc::legacy_asset::FLegacyPackageHeader,
    tables: &Tables,
    was: i32,
    index: i32,
    typed: &str,
) -> Result<(), String> {
    if index == 0 {
        return Ok(());
    }
    let canonical = path_from(
        &tables.names,
        &tables.imports,
        &package.exports,
        &package.summary.package_name,
        FPackageIndex { index },
    )
    .unwrap_or_default();
    if canonical != typed.trim() {
        return Err(format!(
            "write the object as the disassembly names it, {canonical}, rather than {}",
            typed.trim()
        ));
    }
    let class_of = |index: i32| -> Option<String> {
        let at = FPackageIndex { index };
        if at.is_import() {
            tables.import_class(index).map(|(_, name)| name)
        } else if at.is_export() {
            parsed
                .exports
                .get(at.to_export_index() as usize)
                .map(|export| export.class_name.clone())
        } else {
            None
        }
    };
    if let (Some(old), Some(new)) = (class_of(was), class_of(index))
        && old != new
        && new != crate::header_edit::placeholder_class_name()
    {
        return Err(format!(
            "{typed} is a {new}, where the script holds a {old} there"
        ));
    }
    Ok(())
}

/// Every edit planned against `parsed`, refusing two that change the same bytes.
fn plan_all(
    parsed: &ParsedPackage,
    edits: &PackageEdits,
    allow_drift: bool,
) -> Result<Vec<Planned>, String> {
    let mut plans: Vec<Planned> = Vec::with_capacity(edits.scripts.len());
    for edit in &edits.scripts {
        if edits.payloads.iter().any(|p| p.export == edit.export) {
            let name = parsed
                .exports
                .iter()
                .find(|e| e.index == edit.export)
                .map_or_else(|| edit.export.to_string(), |e| e.object_name.clone());
            return Err(format!(
                "{name} is given a whole new payload in this save, so nothing inside its script can also be set"
            ));
        }
        let plan = plan(parsed, edit, allow_drift)?;
        if let Some(other) = plans
            .iter()
            .find(|other| other.export == plan.export && overlaps(other, &plan))
        {
            return Err(format!(
                "{} and {} change the same bytes of one script in one save",
                other.name, plan.name
            ));
        }
        plans.push(plan);
    }
    Ok(plans)
}

/// Two edits collide when their bytes do, or when one's expression holds the other's: a
/// condition being dropped takes everything inside it with it.
fn overlaps(one: &Planned, other: &Planned) -> bool {
    let reach = |plan: &Planned| match plan.expect {
        Expect::Statements(_) => (plan.span.at, plan.span.end_at),
        _ => (plan.extent.at, plan.extent.end_at.max(plan.extent.at + 1)),
    };
    let (a, b) = (reach(one), reach(other));
    a.0 < b.1 && b.0 < a.1
}

fn plan(
    parsed: &ParsedPackage,
    edit: &ScriptConstEdit,
    allow_drift: bool,
) -> Result<Planned, String> {
    let mut found = locate(parsed, edit)?;
    if !allow_drift
        && let Some(was) = &edit.was
        && kismet::render(found.expr) != *was
    {
        return Err(format!(
            "{DRIFT}: the expression at 0x{:04X} in {} reads {} now, not {was}. Re-read the asset and make the edit again, or apply it anyway",
            found.span.offset,
            found.export.object_name,
            kismet::render(found.expr)
        ));
    }
    let name = match edit.at {
        Some(at) => format!(
            "{} script 0x{:04X} at 0x{at:04X}",
            found.export.object_name, edit.statement
        ),
        None => format!(
            "{} script constant {} at 0x{:04X}",
            found.export.object_name, edit.constant, edit.statement
        ),
    };
    let before = kismet::render(found.expr);
    check_entry(parsed, &found, edit)?;
    let at_statement = found.span.offset == found.script.statements[found.statement].offset;
    let (replacement, expect, after, note) = match found.expr {
        _ if edit.widen && edit.narrow => {
            return Err(format!("{name} is set to widen and to narrow at once"));
        }
        _ if edit.widen => {
            let (replacement, expect, after) = widen(found.expr)?;
            (replacement, expect, after, None)
        }
        _ if edit.narrow => {
            let (replacement, expect, after) = narrow(found.expr)?;
            (replacement, expect, after, None)
        }
        Expr::TextConst { text } => {
            let (text, note) = text_change(text, &edit.value)?;
            let new = Expr::TextConst { text };
            if kismet::render(&new) == before
                && matches!(&new, Expr::TextConst { text } if !matches!(text, kismet::TextLiteral::StringTable { .. }))
            {
                return Err(already(found.expr, &edit.value));
            }
            let table = match &new {
                Expr::TextConst {
                    text: kismet::TextLiteral::StringTable { table, .. },
                } => table.path.clone(),
                _ => None,
            };
            let after = kismet::text_form(&new);
            (
                Replacement::Text {
                    text: new.clone(),
                    table,
                },
                Expect::Text(new),
                after,
                note,
            )
        }
        // Never popping on a condition that does more than read a variable cannot leave it to run
        // as a statement of its own, so the condition goes and `True` takes its place.
        Expr::Unary {
            name: "PopExecutionFlowIfNot",
            value: condition,
        } if at_statement
            && !matches!(condition.as_ref(), Expr::Variable { .. })
            && parse_condition(&edit.value)? =>
        {
            let (expr, span) = nodes(found.script)
                .into_iter()
                .nth(found.index + 1)
                .ok_or("the condition is missing")?;
            found = Located {
                expr,
                span,
                index: found.index + 1,
                ..found
            };
            let truth = Expr::Simple { name: "True" };
            let after = kismet::render(&Expr::Unary {
                name: "PopExecutionFlowIfNot",
                value: Box::new(truth.clone()),
            });
            let note = format!(
                "{name}: the condition {} is no longer evaluated",
                kismet::render(condition)
            );
            (
                Replacement::Literal(truth.clone()),
                Expect::Node(truth),
                after,
                Some(note),
            )
        }
        _ => {
            let (replacement, expect, after) = change_for(&found, &edit.value)?;
            (replacement, expect, after, None)
        }
    };
    let extent = extent_of(&found.span, &replacement, found.expr);
    Ok(Planned {
        export: edit.export,
        statement: found.statement,
        index: found.index,
        span: found.span,
        replacement,
        expect,
        extent,
        name,
        before,
        after,
        note,
    })
}

/// The run of bytes a replacement covers and the room it takes, from the expression it replaces.
fn extent_of(span: &Span, replacement: &Replacement, old: &Expr) -> Extent {
    let operand = |file_len: u64, loaded_len: u32| Extent {
        at: span.at + 1,
        end_at: span.at + 1 + file_len,
        offset: span.offset + 1,
        end_offset: span.offset + 1 + loaded_len,
        file_len,
        loaded_len,
    };
    let whole = |file_len: u64, loaded_len: u32| Extent {
        at: span.at,
        end_at: span.end_at,
        offset: span.offset,
        end_offset: span.end_offset,
        file_len,
        loaded_len,
    };
    match replacement {
        Replacement::Value(new) => {
            let was = kismet::stored_width(old);
            let now = kismet::stored_width(new);
            // A name is the one value wider loaded than stored, and it keeps its width.
            let extra = (span.end_offset - span.offset - 1) as u64 - was;
            Extent {
                end_at: span.end_at,
                end_offset: span.end_offset,
                ..operand(now, (now + extra) as u32)
            }
        }
        Replacement::Literal(new) => {
            let len = 1 + kismet::stored_width(new);
            whole(len, len as u32)
        }
        Replacement::Token(_) => Extent {
            at: span.at,
            end_at: span.at + 1,
            offset: span.offset,
            end_offset: span.offset + 1,
            file_len: 1,
            loaded_len: 1,
        },
        Replacement::Offset(_) => operand(4, 4),
        Replacement::Object { widen: false, .. } => operand(4, LOADED_POINTER),
        Replacement::Object { widen: true, .. } => whole(5, 1 + LOADED_POINTER),
        Replacement::Text { text, .. } => {
            let (file, loaded) = text_length(text);
            whole(file, loaded)
        }
        Replacement::Callee(_) => operand(4, LOADED_POINTER),
        // A name is eight bytes stored and twelve once loaded.
        Replacement::Name(_) => operand(8, 12),
    }
}

/// What `value` does to the expression it is typed at.
fn change_for(found: &Located<'_>, value: &str) -> Result<(Replacement, Expect, String), String> {
    let at_statement = found.span.offset == found.script.statements[found.statement].offset;
    let expr = found.expr;
    match expr {
        Expr::JumpIfNot { target, condition } if at_statement => {
            let jump = match parse_condition(value)? {
                // Never jumping is the same jump aimed at where the branch falls through anyway.
                true => {
                    let fall_through = found.span.end_offset;
                    if *target == fall_through {
                        return Err(already(expr, value));
                    }
                    let new = Expr::JumpIfNot {
                        target: fall_through,
                        condition: condition.clone(),
                    };
                    let after = kismet::render(&new);
                    (Replacement::Offset(fall_through), Expect::Node(new), after)
                }
                // Always jumping drops the test: the condition's bytes stay behind the jump, where
                // nothing reaches them.
                false => {
                    let jump = Expr::Jump { target: *target };
                    let after = kismet::render(&jump);
                    (
                        Replacement::Token(TOKEN_JUMP),
                        Expect::Statements(vec![jump, (**condition).clone()]),
                        after,
                    )
                }
            };
            Ok(jump)
        }
        Expr::Unary {
            name: "PopExecutionFlowIfNot",
            value: condition,
        } if at_statement => match parse_condition(value)? {
            false => {
                let pop = Expr::Simple {
                    name: "PopExecutionFlow",
                };
                let after = kismet::render(&pop);
                Ok((
                    Replacement::Token(TOKEN_POP_FLOW),
                    Expect::Statements(vec![pop, (**condition).clone()]),
                    after,
                ))
            }
            // Never popping leaves a variable read to run on its own as a statement, which keeps the
            // script's size; `plan` replaces any other condition.
            true => {
                let nothing = Expr::Simple { name: "Nothing" };
                Ok((
                    Replacement::Token(TOKEN_NOTHING),
                    Expect::Statements(vec![nothing, (**condition).clone()]),
                    "Nothing".to_string(),
                ))
            }
        },
        Expr::Simple {
            name: "True" | "False",
        } => {
            let new = if parse_condition(value)? {
                "True"
            } else {
                "False"
            };
            if matches!(expr, Expr::Simple { name } if *name == new) {
                return Err(already(expr, value));
            }
            let token = if new == "True" {
                TOKEN_TRUE
            } else {
                TOKEN_FALSE
            };
            Ok((
                Replacement::Token(token),
                Expect::Node(Expr::Simple { name: new }),
                new.to_string(),
            ))
        }
        Expr::Simple {
            name: "IntZero" | "IntOne",
        } => {
            let number: i32 = value.trim().parse().map_err(|_| {
                format!(
                    "{value:?} is not a 32-bit integer, which is what {} holds",
                    kismet::render(expr)
                )
            })?;
            let one_byte = match number {
                0 => Some((TOKEN_INT_ZERO, "IntZero")),
                1 => Some((TOKEN_INT_ONE, "IntOne")),
                _ => None,
            };
            match one_byte {
                Some((_, name)) if matches!(expr, Expr::Simple { name: was } if *was == name) => {
                    Err(already(expr, value))
                }
                Some((token, name)) => Ok((
                    Replacement::Token(token),
                    Expect::Node(Expr::Simple { name }),
                    name.to_string(),
                )),
                // Only a full constant holds anything else, which is four bytes longer.
                None => {
                    let new = Expr::IntConst {
                        value: number,
                        at: 0,
                    };
                    Ok((
                        Replacement::Literal(new.clone()),
                        Expect::Node(new),
                        number.to_string(),
                    ))
                }
            }
        }
        Expr::ByteConst {
            name: "IntConstByte",
            value: was,
            ..
        } if value.trim().parse::<u8>().is_err() => {
            // An integer that fits no byte takes a full constant; both write an int32 once run.
            let number: i32 = value.trim().parse().map_err(|_| {
                format!("{value:?} is not a 32-bit integer, which is what an IntConstByte holds")
            })?;
            if i64::from(number) == i64::from(*was) {
                return Err(already(expr, value));
            }
            let new = Expr::IntConst {
                value: number,
                at: 0,
            };
            Ok((
                Replacement::Literal(new.clone()),
                Expect::Node(new),
                number.to_string(),
            ))
        }
        Expr::StringConst { value: was, .. } | Expr::UnicodeStringConst { value: was, .. } => {
            if value.contains('\0') {
                return Err(
                    "a string in a script ends at its first NUL, so it cannot hold one".to_string(),
                );
            }
            if was == value {
                return Err(already(expr, value));
            }
            let ansi = matches!(expr, Expr::StringConst { .. });
            // The compiler writes a string one byte per character only when every one is ASCII.
            let new = if ansi && value.chars().all(|c| u32::from(c) <= 0x7F) {
                Expr::StringConst {
                    value: value.to_string(),
                    at: 0,
                }
            } else {
                Expr::UnicodeStringConst {
                    value: value.to_string(),
                    at: 0,
                }
            };
            let after = kismet::render(&new);
            let replacement = if ansi && matches!(new, Expr::UnicodeStringConst { .. }) {
                Replacement::Literal(new.clone())
            } else {
                Replacement::Value(new.clone())
            };
            Ok((replacement, Expect::Node(new), after))
        }
        Expr::ObjectConst { object } => {
            let path = object_path_typed(value);
            if object.path.as_deref().unwrap_or("None") == path {
                return Err(already(expr, &path));
            }
            Ok((
                Replacement::Object {
                    path: path.clone(),
                    widen: false,
                },
                Expect::Object(path.clone()),
                format!("Object({path})"),
            ))
        }
        Expr::Simple { name: "NoObject" } => {
            let path = object_path_typed(value);
            if path == "None" {
                return Err(already(expr, &path));
            }
            Ok((
                Replacement::Object {
                    path: path.clone(),
                    widen: true,
                },
                Expect::Object(path.clone()),
                format!("Object({path})"),
            ))
        }
        Expr::FinalCall {
            name,
            function,
            params,
        } => {
            let path = value.trim();
            if !path.starts_with('/') || !path.contains('.') {
                return Err(format!(
                    "a {name} names its function by the path the disassembly shows, such as {}",
                    function
                        .path
                        .as_deref()
                        .unwrap_or("/Script/Engine.KismetSystemLibrary:Delay")
                ));
            }
            if function.path.as_deref() == Some(path) {
                return Err(already(expr, path));
            }
            refuse_event_graph(function.path.as_deref().unwrap_or(""), path)?;
            let new = Expr::FinalCall {
                name,
                function: kismet::ObjectRef {
                    index: 0,
                    path: Some(path.to_string()),
                },
                params: params.clone(),
            };
            let after = kismet::render(&new);
            Ok((
                Replacement::Callee(path.to_string()),
                Expect::Callee(new, path.to_string()),
                after,
            ))
        }
        Expr::VirtualCall {
            name,
            function,
            params,
            ..
        } => {
            let new_name = value.trim();
            if new_name.is_empty() || new_name.contains(['/', '.', ':', ' ']) {
                return Err(format!(
                    "a {name} looks its function up by name on the object it is called on, so it takes a bare name such as {function}"
                ));
            }
            if function == new_name {
                return Err(already(expr, new_name));
            }
            refuse_event_graph(function, new_name)?;
            let new = Expr::VirtualCall {
                name,
                function: new_name.to_string(),
                params: params.clone(),
                id: kismet::NameId::default(),
            };
            let after = kismet::render(&new);
            Ok((
                Replacement::Name(new_name.to_string()),
                Expect::Node(new),
                after,
            ))
        }
        Expr::JumpIfNot { .. } | Expr::Unary { .. } => Err(format!(
            "{} is not a statement of its own here, so its condition cannot be fixed",
            kismet::render(expr)
        )),
        literal if kismet::literals(literal).len() == 1 && kismet::stored_width(literal) > 0 => {
            let new = kismet::with_value(literal, value)?;
            let after = kismet::render(&new);
            Ok((Replacement::Value(new.clone()), Expect::Node(new), after))
        }
        other => Err(format!(
            "{} holds nothing an edit can set: a literal, an object constant, or the condition of a JumpIfNot or PopExecutionFlowIfNot statement",
            kismet::render(other)
        )),
    }
}

/// An integer an event stub passes its event graph is where the event starts in it, so a new one
/// has to be where a statement starts there; anywhere else the event would run from the middle of
/// an instruction.
fn check_entry(
    parsed: &ParsedPackage,
    found: &Located<'_>,
    edit: &ScriptConstEdit,
) -> Result<(), String> {
    if edit.widen || kismet::entry_value(found.expr).is_none() {
        return Ok(());
    }
    let functions = kismet::functions_of(&parsed.exports);
    for graph in functions
        .iter()
        .filter(|function| function.is_event_graph())
    {
        let inbound = kismet::inbound(&functions, graph);
        if !inbound
            .entries
            .iter()
            .any(|entry| entry.export == edit.export && entry.span.at == found.span.at)
        {
            continue;
        }
        let lands = edit.value.trim().parse::<u32>().is_ok_and(|offset| {
            graph
                .script
                .statements
                .iter()
                .any(|statement| statement.offset == offset)
        });
        if !lands {
            return Err(format!(
                "{} enters {} at the offset this holds, and {} is not where a statement starts there",
                found.export.object_name,
                graph.name,
                edit.value.trim()
            ));
        }
    }
    Ok(())
}

/// A literal in its widest form with the same meaning: what the compiler writes in one byte, in
/// four, and an ASCII string as a UTF-16 one.
fn widen(expr: &Expr) -> Result<(Replacement, Expect, String), String> {
    let int = |value: i32| {
        let new = Expr::IntConst { value, at: 0 };
        Ok((
            Replacement::Literal(new.clone()),
            Expect::Node(new),
            value.to_string(),
        ))
    };
    match expr {
        Expr::Simple { name: "IntZero" } => int(0),
        Expr::Simple { name: "IntOne" } => int(1),
        Expr::ByteConst {
            name: "IntConstByte",
            value,
            ..
        } => int(i32::from(*value)),
        Expr::StringConst { value, .. } => {
            let new = Expr::UnicodeStringConst {
                value: value.clone(),
                at: 0,
            };
            let after = kismet::render(&new);
            Ok((Replacement::Literal(new.clone()), Expect::Node(new), after))
        }
        Expr::Simple { name: "NoObject" } => Ok((
            Replacement::Object {
                path: "None".to_string(),
                widen: true,
            },
            Expect::Object("None".to_string()),
            "Object(None)".to_string(),
        )),
        other => Err(format!(
            "{} has no wider form with the same meaning",
            kismet::render(other)
        )),
    }
}

/// The narrowest form with the same meaning, the reverse of `widen` for every literal whose own
/// form is already its narrowest.
fn narrow(expr: &Expr) -> Result<(Replacement, Expect, String), String> {
    let literal = |new: Expr| {
        let after = kismet::render(&new);
        Ok((Replacement::Literal(new.clone()), Expect::Node(new), after))
    };
    match expr {
        Expr::IntConst { value: 0, .. } => literal(Expr::Simple { name: "IntZero" }),
        Expr::IntConst { value: 1, .. } => literal(Expr::Simple { name: "IntOne" }),
        Expr::IntConst { value, .. } if (2..=255).contains(value) => literal(Expr::ByteConst {
            name: "IntConstByte",
            value: *value as u8,
            at: 0,
        }),
        Expr::UnicodeStringConst { value, .. }
            if value.chars().all(|c| c.is_ascii() && c != '\0') =>
        {
            literal(Expr::StringConst {
                value: value.clone(),
                at: 0,
            })
        }
        Expr::ObjectConst { object } if object.index == 0 => {
            literal(Expr::Simple { name: "NoObject" })
        }
        other => Err(format!(
            "{} has no narrower form with the same meaning",
            kismet::render(other)
        )),
    }
}

/// Whether narrowing gives back the form `expr` was widened from, which a one-byte 0 or 1 and a
/// string beyond ASCII do not.
fn narrows_back(expr: &Expr) -> bool {
    match expr {
        Expr::ByteConst {
            name: "IntConstByte",
            value,
            ..
        } => *value > 1,
        Expr::StringConst { value, .. } => value.is_ascii(),
        _ => true,
    }
}

/// An edit for every literal that has a wider form in the scripts of `parsed`, or of `only`.
/// Saved, they change no behaviour and move every offset after each, which makes them a check of
/// relocation that does not rest on the decoder's own idea of where offsets are. With `reversible`,
/// only the literals [`narrowing_edits`] can write back as they were.
pub fn widening_edits(
    parsed: &ParsedPackage,
    only: Option<u32>,
    reversible: bool,
) -> Vec<ScriptConstEdit> {
    let mut out = Vec::new();
    for export in &parsed.exports {
        if only.is_some_and(|only| only != export.index) {
            continue;
        }
        let Some(script) = export.script.as_ref() else {
            continue;
        };
        if script.resize_lock().is_some() {
            continue;
        }
        for (statement, nodes) in script
            .statements
            .iter()
            .zip(kismet::nodes_by_statement(script))
        {
            for (expr, span) in nodes {
                if widen(expr).is_ok() && (!reversible || narrows_back(expr)) {
                    out.push(ScriptConstEdit {
                        export: export.index,
                        statement: statement.offset,
                        at: Some(span.offset),
                        widen: true,
                        ..ScriptConstEdit::default()
                    });
                }
            }
        }
    }
    out
}

/// The edits that undo a save of [`widening_edits`]: every literal `widened` holds in a wider
/// form than `before` did, written back narrow. Saved over the widened package, they should give
/// back the bytes it was widened from, which holds relocation to moving everything back exactly.
pub fn narrowing_edits(before: &ParsedPackage, widened: &ParsedPackage) -> Vec<ScriptConstEdit> {
    let mut out = Vec::new();
    for export in &widened.exports {
        let Some(script) = export.script.as_ref() else {
            continue;
        };
        let Some(was) = before
            .exports
            .iter()
            .find(|e| e.index == export.index)
            .and_then(|e| e.script.as_ref())
        else {
            continue;
        };
        let old = nodes(was);
        let mut index = 0;
        for (statement, nodes) in script
            .statements
            .iter()
            .zip(kismet::nodes_by_statement(script))
        {
            for (expr, span) in nodes {
                let Some((old, _)) = old.get(index) else {
                    return out;
                };
                index += 1;
                let undone = narrow(expr).ok().and_then(|(_, expect, _)| match expect {
                    Expect::Node(new) => Some(new),
                    _ => None,
                });
                if kismet::shape(expr) != kismet::shape(old)
                    && undone.is_some_and(|new| kismet::shape(&new) == kismet::shape(old))
                {
                    out.push(ScriptConstEdit {
                        export: export.index,
                        statement: statement.offset,
                        at: Some(span.offset),
                        narrow: true,
                        ..ScriptConstEdit::default()
                    });
                }
            }
        }
    }
    out
}

/// A call into an event graph passes it an entry offset, which a call to anything else would take
/// as an argument it never asked for.
fn refuse_event_graph(was: &str, now: &str) -> Result<(), String> {
    let graph = |name: &str| kismet::callee_name(name).starts_with("ExecuteUbergraph");
    if graph(was) || graph(now) {
        return Err(
            "a call into an event graph passes it the offset an event enters at, so it is not retargeted"
                .to_string(),
        );
    }
    Ok(())
}

/// An object as typed: its path, or `None` for no object at all.
fn object_path_typed(value: &str) -> String {
    let path = value.trim();
    if path.is_empty() || path.eq_ignore_ascii_case("none") {
        "None".to_string()
    } else {
        path.to_string()
    }
}

fn already(expr: &Expr, value: &str) -> String {
    format!("{} already reads {value}", kismet::render(expr))
}

/// A condition as a person types it.
fn parse_condition(text: &str) -> Result<bool, String> {
    match text.trim().to_ascii_lowercase().as_str() {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        _ => Err(format!("{text:?} is not true or false")),
    }
}

/// The expression `edit` addresses: by its loaded offset when it names one, otherwise as the
/// statement's `constant`-th literal.
fn locate<'a>(parsed: &'a ParsedPackage, edit: &ScriptConstEdit) -> Result<Located<'a>, String> {
    let export = parsed
        .exports
        .iter()
        .find(|e| e.index == edit.export)
        .ok_or_else(|| format!("no export {}", edit.export))?;
    let script = export.script.as_ref().ok_or_else(|| {
        format!(
            "export {} ({}) carries no bytecode this reader measured",
            edit.export, export.class_name
        )
    })?;
    if let Some(stop) = &script.stopped {
        return Err(format!(
            "{}'s bytecode did not disassemble whole ({}), so nothing in it can be trusted to sit where the walk says",
            export.object_name, stop.reason
        ));
    }
    let statement = script
        .statements
        .iter()
        .position(|s| s.offset == edit.statement)
        .ok_or_else(|| {
            format!(
                "no statement starts at 0x{:04X}; the disassembly names each one by the offset at the start of its line",
                edit.statement
            )
        })?;
    let all = nodes(script);
    let range = statement_range(script, statement, &all);
    let within = &all[range.clone()];
    let position = match edit.at {
        Some(at) => within
            .iter()
            .position(|(_, span)| span.offset == at)
            .ok_or_else(|| {
                format!(
                    "no expression starts at 0x{at:04X} in the statement at 0x{:04X}",
                    edit.statement
                )
            })?,
        None => {
            let literal = kismet::literal_at(script, edit.statement, edit.constant)?;
            within
                .iter()
                .position(|(expr, _)| std::ptr::eq(*expr, literal))
                .ok_or("the literal is not among the statement's expressions")?
        }
    };
    let (expr, span) = within[position];
    Ok(Located {
        export,
        script,
        statement,
        index: range.start + position,
        expr,
        span,
    })
}

/// Every expression of a script with its span, in pre-order.
fn nodes(script: &Script) -> Vec<(&Expr, Span)> {
    let mut out = Vec::with_capacity(script.spans.len());
    kismet::visit_spans(script, &mut |expr, span, _| out.push((expr, *span)));
    out
}

/// Which of `all` belong to statement `statement`: from its own span to the next statement's.
fn statement_range(
    script: &Script,
    statement: usize,
    all: &[(&Expr, Span)],
) -> std::ops::Range<usize> {
    let start_of = |k: usize| {
        script.statements.get(k).map_or(all.len(), |s| {
            all.iter()
                .position(|(_, span)| span.at == s.at)
                .unwrap_or(all.len())
        })
    };
    start_of(statement)..start_of(statement + 1)
}

/// Holds every script in `after` to what the edits should have left: each edited one reads as the
/// old script with each change made and every offset mapped; every one entering an edited script
/// or resuming in it reads as before with those offsets mapped; and every other reads exactly as
/// it did. None may come back unable to change size when it could before.
pub(crate) fn verify(
    before: &ParsedPackage,
    after: &ParsedPackage,
    edits: &PackageEdits,
) -> Result<(), String> {
    if edits.scripts.is_empty() && edits.script_texts.is_empty() {
        return Ok(());
    }
    let plans = plan_all(before, edits, true)?;
    let mut maps: Vec<(u32, String, OffsetMap)> = Vec::new();
    for export in &before.exports {
        let mine: Vec<&Planned> = plans.iter().filter(|p| p.export == export.index).collect();
        if !mine.is_empty() {
            maps.push((export.index, export.object_name.clone(), map_of(&mine)));
        }
    }
    let texts = crate::script_text_edit::lay_out_texts(before, after, edits)?;
    for laid in &texts {
        let name = before
            .exports
            .get(laid.export as usize)
            .map_or_else(|| laid.export.to_string(), |e| e.object_name.clone());
        maps.push((laid.export, name, laid.map.clone()));
    }
    let functions = kismet::functions_of(&before.exports);
    for was in &before.exports {
        let Some(old) = was.script.as_ref() else {
            continue;
        };
        let new = after
            .exports
            .iter()
            .find(|e| e.index == was.index)
            .and_then(|e| e.script.as_ref())
            .ok_or_else(|| {
                format!(
                    "{} no longer carries bytecode after patching",
                    was.object_name
                )
            })?;
        if let Some(stop) = &new.stopped {
            return Err(format!(
                "{}'s script stopped after patching: {}",
                was.object_name, stop.reason
            ));
        }
        if old.complete() && old.resize_locks.is_empty() && !new.resize_locks.is_empty() {
            return Err(format!(
                "{}'s script no longer reads as one that can change size after patching: {}",
                was.object_name,
                new.resize_lock().unwrap_or_default()
            ));
        }
        if let Some(laid) = texts.iter().find(|laid| laid.export == was.index) {
            crate::script_text_edit::verify_text(&was.object_name, laid, new)?;
            continue;
        }
        let mine: Vec<&Planned> = plans.iter().filter(|p| p.export == was.index).collect();
        let own_map = maps
            .iter()
            .find(|(export, _, _)| *export == was.index)
            .map(|(_, _, map)| map.clone())
            .unwrap_or_default();
        let wanted_sizes = if mine.is_empty() {
            (old.buffer_size, old.storage_size)
        } else {
            let loaded: i64 = mine
                .iter()
                .map(|p| {
                    i64::from(p.extent.loaded_len)
                        - i64::from(p.extent.end_offset - p.extent.offset)
                })
                .sum();
            let stored: i64 = mine
                .iter()
                .map(|p| p.extent.file_len as i64 - (p.extent.end_at - p.extent.at) as i64)
                .sum();
            (
                (i64::from(old.buffer_size) + loaded) as u32,
                (i64::from(old.storage_size) + stored) as u32,
            )
        };
        if (new.buffer_size, new.storage_size) != wanted_sizes {
            return Err(format!(
                "{}'s script is {} bytes loaded and {} stored after patching, where {} and {} were expected",
                was.object_name, new.buffer_size, new.storage_size, wanted_sizes.0, wanted_sizes.1
            ));
        }
        // What this script holds into edited ones follows their maps, found by where it sat.
        let mut inbound_values: Vec<(usize, u32)> = Vec::new();
        let spans = nodes(old);
        for (export, name, map) in &maps {
            if *export == was.index || map.is_identity() {
                continue;
            }
            let Some(target) = functions.iter().find(|f| f.export == *export) else {
                continue;
            };
            let inbound = kismet::inbound(&functions, target);
            for entry in inbound.entries.iter().filter(|e| e.export == was.index) {
                if let Some(index) = spans.iter().position(|(_, span)| span.at == entry.span.at) {
                    inbound_values.push((index, map.map(entry.offset)?));
                }
            }
            for (_, fixup) in inbound
                .linkages
                .iter()
                .filter(|(holder, _)| *holder == was.index)
            {
                if let (Some(index), FixupKind::Absolute { target, .. }) = (
                    spans.iter().position(|(_, span)| span.at + 1 == fixup.at),
                    &fixup.kind,
                ) {
                    let mapped = map
                        .map(*target)
                        .map_err(|reason| format!("{name}: {reason}"))?;
                    inbound_values.push((index, mapped));
                }
            }
        }
        let expected =
            expected_statements(old, new, &mine, &own_map, &inbound_values, &was.object_name)?;
        if expected.len() != new.statements.len() {
            return Err(format!(
                "{} should hold {} statement(s) after patching and holds {}",
                was.object_name,
                expected.len(),
                new.statements.len()
            ));
        }
        for ((offset, expr), statement) in expected.iter().zip(&new.statements) {
            if *offset != statement.offset || kismet::shape(expr) != kismet::shape(&statement.expr)
            {
                return Err(format!(
                    "{} at 0x{:04X} reads {} after patching, where 0x{offset:04X} {} was expected",
                    was.object_name,
                    statement.offset,
                    kismet::render(&statement.expr),
                    kismet::render(expr)
                ));
            }
        }
    }
    Ok(())
}

/// The old script's statements with every planned change made and every offset into the script
/// itself mapped, each with the offset it should start at. An object an edit names is taken from
/// where the saved script holds it, once that is seen to be the path asked for.
fn expected_statements(
    old: &Script,
    new: &Script,
    plans: &[&Planned],
    map: &OffsetMap,
    inbound: &[(usize, u32)],
    owner: &str,
) -> Result<Vec<(u32, Expr)>, String> {
    let mut statements: Vec<Expr> = old.statements.iter().map(|s| s.expr.clone()).collect();
    // Offsets are mapped before the changes go in: a change's own offsets were written mapped.
    for statement in &mut statements {
        map_offsets(statement, map, owner)?;
    }
    // So is every entry and resume point it holds into another edited script, by the place it
    // held before any replacement could renumber what follows.
    for (index, value) in inbound {
        match node_mut(&mut statements, *index) {
            Some(Expr::IntConst { value: held, .. }) => *held = *value as i32,
            Some(Expr::SkipOffsetConst { value: held }) => *held = *value,
            _ => {
                return Err(format!(
                    "{owner}: an offset into an edited script is not where it was"
                ));
            }
        }
    }
    let saved = nodes(new);
    let mut ordered: Vec<&&Planned> = plans.iter().collect();
    ordered.sort_by_key(|plan| std::cmp::Reverse(plan.index));
    let mut splits: Vec<(usize, Vec<Expr>)> = Vec::new();
    for plan in ordered {
        let node = node_mut(&mut statements, plan.index)
            .ok_or_else(|| format!("{owner}: the edited expression is not in the script"))?;
        match &plan.expect {
            Expect::Node(expr) => {
                let mut expr = expr.clone();
                map_offsets(&mut expr, map, owner)?;
                *node = expr;
            }
            Expect::Statements(parts) => {
                let mut parts = parts.clone();
                for part in &mut parts {
                    map_offsets(part, map, owner)?;
                }
                splits.push((plan.statement, parts));
            }
            Expect::Text(expr) => {
                let mut expr = expr.clone();
                if let Expr::TextConst {
                    text: kismet::TextLiteral::StringTable { table, .. },
                } = &mut expr
                {
                    let wanted = table.path.clone().unwrap_or_default();
                    let offset = map.map(plan.span.offset)?;
                    let saved_table = saved.iter().find_map(|(expr, span)| match expr {
                        Expr::TextConst {
                            text: kismet::TextLiteral::StringTable { table, .. },
                        } if span.offset == offset => Some(table.clone()),
                        _ => None,
                    });
                    match saved_table {
                        Some(found) if found.path.as_deref() == Some(wanted.as_str()) => {
                            *table = found;
                        }
                        other => {
                            return Err(format!(
                                "{owner}: the text's table reads back as {:?} rather than {wanted}",
                                other.and_then(|table| table.path)
                            ));
                        }
                    }
                }
                *node = expr;
            }
            Expect::Callee(expr, path) => {
                let offset = map.map(plan.span.offset)?;
                let saved_function = saved.iter().find_map(|(expr, span)| match expr {
                    Expr::FinalCall { function, .. } if span.offset == offset => {
                        Some(function.clone())
                    }
                    _ => None,
                });
                let mut expr = expr.clone();
                match (&mut expr, saved_function) {
                    (Expr::FinalCall { function, .. }, Some(found))
                        if found.path.as_deref() == Some(path.as_str()) =>
                    {
                        *function = found;
                    }
                    (_, found) => {
                        return Err(format!(
                            "{owner}: the call reads back as calling {:?} rather than {path}",
                            found.and_then(|function| function.path)
                        ));
                    }
                }
                map_offsets(&mut expr, map, owner)?;
                *node = expr;
            }
            Expect::Object(path) => {
                let object = saved_object(&saved, map, plan, owner)?;
                let named = object.path.as_deref().unwrap_or("None");
                let wanted_none = path == "None";
                if (wanted_none && object.index != 0) || (!wanted_none && named != path) {
                    return Err(format!(
                        "{owner}: the object constant reads back as {named} rather than {path}"
                    ));
                }
                *node = Expr::ObjectConst { object };
            }
        }
    }
    let mut out = Vec::with_capacity(statements.len() + splits.len());
    for (position, (statement, expr)) in old.statements.iter().zip(statements).enumerate() {
        let start = map.map(statement.offset)?;
        match splits.iter().find(|(at, _)| *at == position) {
            Some((_, parts)) => {
                // The head is the swapped token and whatever operands it keeps; the rest follows it.
                let mut offset = start;
                for part in parts {
                    out.push((offset, part.clone()));
                    offset += head_length(part);
                }
            }
            None => out.push((start, expr)),
        }
    }
    Ok(out)
}

/// The object constant the saved script holds where a plan pointed one.
fn saved_object(
    saved: &[(&Expr, Span)],
    map: &OffsetMap,
    plan: &Planned,
    owner: &str,
) -> Result<kismet::ObjectRef, String> {
    let offset = map.map(plan.span.offset)?;
    match saved.iter().find(|(_, span)| span.offset == offset) {
        Some((Expr::ObjectConst { object }, _)) => Ok(object.clone()),
        _ => Err(format!(
            "{owner}: no object constant reads back where one was set"
        )),
    }
}

/// Every offset into the script itself that `expr` holds, mapped. The lengths a context or a
/// switch holds are left to the decoder, which measures them against the code they cover.
fn map_offsets(expr: &mut Expr, map: &OffsetMap, owner: &str) -> Result<(), String> {
    let mut failure = None;
    kismet::visit_mut(expr, &mut |node| {
        let target = match node {
            Expr::Jump { target }
            | Expr::JumpIfNot { target, .. }
            | Expr::PushExecutionFlow { target } => target,
            _ => return,
        };
        match map.map(*target) {
            Ok(mapped) => *target = mapped,
            Err(reason) => failure = Some(format!("{owner}: {reason}")),
        }
    });
    // A latent resume point counts in the function it names, which is this one for every latent
    // action the game's code holds; the decoder's own check catches any it does not.
    kismet::visit_mut(expr, &mut |node| {
        if let Expr::StructConst { fields, .. } = node
            && let [
                Expr::SkipOffsetConst { value },
                _,
                Expr::NameConst { value: resumes, .. },
                ..,
            ] = fields.as_mut_slice()
            && resumes.as_str() == owner
        {
            match map.map(*value) {
                Ok(mapped) => *value = mapped,
                Err(reason) => failure = Some(format!("{owner}: {reason}")),
            }
        }
    });
    failure.map_or(Ok(()), Err)
}

/// How long the head of a split statement is once loaded: the token and its own operands.
fn head_length(expr: &Expr) -> u32 {
    match expr {
        Expr::Jump { .. } => 5,
        _ => 1,
    }
}

/// The `index`-th expression across `statements` in pre-order.
fn node_mut(statements: &mut [Expr], index: usize) -> Option<&mut Expr> {
    let mut next = 0;
    for statement in statements {
        if let Some(found) = find_mut(statement, index, &mut next) {
            return Some(found);
        }
    }
    None
}

fn find_mut<'a>(expr: &'a mut Expr, index: usize, next: &mut usize) -> Option<&'a mut Expr> {
    if *next == index {
        return Some(expr);
    }
    *next += 1;
    for child in kismet::children_mut(expr) {
        if let Some(found) = find_mut(child, index, next) {
            return Some(found);
        }
    }
    None
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    /// The bytes a save reads a script from, for one whose edits never need them.
    const NO_BUNDLE: AssetBundle<'static> = AssetBundle {
        asset: &[],
        exports: &[],
    };
    use crate::package::{ExportStatus, ImportInfo};
    use crate::props::{Ctx, Diagnostics};
    use retoc::legacy_asset::{FLegacyPackageHeader, FMinimalName, FObjectImport, FPackageNameMap};

    const NAMES: &[&str] = &[
        "None",
        "Flag",
        "Target",
        "/Game/T_A",
        "T_A",
        "/Script/Engine",
        "Texture2D",
        "/Script/CoreUObject",
        "Package",
        "Function",
        "/Game/ST_A",
        "ST_A",
        "StringTable",
    ];

    /// The function every call names.
    const TARGET: i32 = -1;
    /// `/Game/T_A.T_A`, a texture.
    const TEXTURE: i32 = -3;

    fn name(value: &str) -> FMinimalName {
        FMinimalName {
            index: NAMES.iter().position(|n| *n == value).expect("a test name") as i32,
            number: 0,
        }
    }

    fn header() -> FLegacyPackageHeader {
        let import = |class: (&str, &str), outer: i32, object: &str| FObjectImport {
            class_package: name(class.0),
            class_name: name(class.1),
            outer_index: FPackageIndex { index: outer },
            object_name: name(object),
            is_optional: false,
        };
        let mut header = FLegacyPackageHeader {
            name_map: FPackageNameMap::create_from_names(
                NAMES.iter().map(|n| (*n).to_string()).collect(),
            ),
            imports: vec![
                import(("/Script/CoreUObject", "Function"), 0, "Target"),
                import(("/Script/CoreUObject", "Package"), 0, "/Game/T_A"),
                import(("/Script/Engine", "Texture2D"), -2, "T_A"),
                import(("/Script/CoreUObject", "Package"), 0, "/Game/ST_A"),
                import(("/Script/Engine", "StringTable"), -4, "ST_A"),
            ],
            ..Default::default()
        };
        header.summary.package_name = "/Game/Test".into();
        header
    }

    fn field_path(out: &mut Vec<u8>, value: &str) {
        out.extend_from_slice(&1i32.to_le_bytes());
        out.extend_from_slice(&name(value).index.to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
    }

    /// A call to the target with these parameter bytes.
    fn call(out: &mut Vec<u8>, params: &[u8]) {
        out.push(0x46);
        out.extend_from_slice(&TARGET.to_le_bytes());
        out.extend_from_slice(params);
        out.push(0x16);
    }

    /// Seven statements, at loaded offsets 0x00, 0x0F, 0x1D, 0x30, 0x3B, 0x45 and 0x47:
    /// `Target(411)`, `Jump @0045 unless Flag`, `Target(ObjectConst(T_A))`, `Target(True)`,
    /// `PopExecutionFlowIfNot(Flag)`, `Return Nothing` and the end marker.
    fn script_bytes() -> Vec<u8> {
        let mut out = Vec::new();
        let mut int = vec![0x1D];
        int.extend_from_slice(&411i32.to_le_bytes());
        call(&mut out, &int);
        out.push(0x07);
        out.extend_from_slice(&0x45u32.to_le_bytes());
        out.push(0x00);
        field_path(&mut out, "Flag");
        let mut object = vec![0x20];
        object.extend_from_slice(&TEXTURE.to_le_bytes());
        call(&mut out, &object);
        call(&mut out, &[0x27]);
        out.extend_from_slice(&[0x4F, 0x00]);
        field_path(&mut out, "Flag");
        out.extend_from_slice(&[0x04, 0x0B, 0x53]);
        out
    }

    /// A function's stored form as the tests lay it out: its two size words, then its script.
    fn decode(header: &FLegacyPackageHeader, file: &[u8], declared: Option<u32>) -> Script {
        let ctx = Ctx {
            mappings: None,
            header,
            fixups: None,
            synth: None,
            local: None,
        };
        let mut diagnostics = Diagnostics::default();
        kismet::read_script(
            &file[8..],
            8,
            0,
            declared,
            (file.len() - 8) as u32,
            &ctx,
            &mut diagnostics,
        )
    }

    /// `script` behind the size words it needs.
    fn file(header: &FLegacyPackageHeader, script: &[u8]) -> Vec<u8> {
        let mut out = vec![0; 8];
        out.extend_from_slice(script);
        let loaded = decode(header, &out, None).decoded_size;
        out[..4].copy_from_slice(&loaded.to_le_bytes());
        out[4..8].copy_from_slice(&(script.len() as u32).to_le_bytes());
        out
    }

    fn package(header: &FLegacyPackageHeader, bytes: &[u8]) -> ParsedPackage {
        let words = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().expect("a word"));
        assert_eq!(words(4) as usize, bytes.len() - 8, "the stored size word");
        let script = decode(header, bytes, Some(words(0)));
        assert!(script.complete(), "{:?}", script.stopped);
        let imports = (0..header.imports.len())
            .map(|at| {
                let index = FPackageIndex::create_import(at as u32);
                ImportInfo {
                    index: index.index,
                    class_package: String::new(),
                    class_name: String::new(),
                    outer_index: header.imports[at].outer_index.index,
                    object_name: String::new(),
                    path: crate::package::dotted_path(header, index).unwrap_or_default(),
                    unresolved: false,
                    usage: Default::default(),
                }
            })
            .collect();
        let mut parsed = ParsedPackage::of_exports(vec![ParsedExport {
            object_name: "Fn".into(),
            class_name: "Function".into(),
            status: ExportStatus::Payload {
                consumed: 8,
                payload_bytes: bytes.len() as u64 - 8,
                kind: "bytecode",
            },
            script: Some(script),
            ..ParsedExport::blank(0)
        }]);
        parsed.imports = imports;
        parsed
    }

    fn edit(statement: u32, at: Option<u32>, value: &str) -> ScriptConstEdit {
        ScriptConstEdit {
            export: 0,
            statement,
            at,
            value: value.into(),
            ..ScriptConstEdit::default()
        }
    }

    type Saved = (ParsedPackage, ParsedPackage, Vec<AppliedEdit>);

    /// Saves `edits` over the script and reads it back, the way a save is verified.
    fn save(edits: &PackageEdits) -> Result<Saved, String> {
        save_with(script_bytes(), edits)
    }

    fn save_with(script: Vec<u8>, edits: &PackageEdits) -> Result<Saved, String> {
        let header = header();
        let bytes = file(&header, &script);
        let before = package(&header, &bytes);
        let mut tables = Tables {
            names: header.name_map.clone(),
            imports: header.imports.clone(),
        };
        let bundle = AssetBundle {
            asset: &[],
            exports: &bytes,
        };
        let out = script_splices(&before, edits, &mut tables, &header, &bundle, 0)?;
        let mut patched = bytes.clone();
        let mut ordered: Vec<&Splice> = out.splices.iter().collect();
        ordered.sort_by_key(|splice| std::cmp::Reverse(splice.start));
        for splice in ordered {
            patched.splice(
                splice.start as usize..splice.end as usize,
                splice.bytes.clone(),
            );
        }
        let grown = FLegacyPackageHeader {
            name_map: tables.names.clone(),
            imports: tables.imports.clone(),
            ..header.clone()
        };
        let after = package(&grown, &patched);
        verify(&before, &after, edits)?;
        Ok((before, after, out.applied))
    }

    fn rendered(parsed: &ParsedPackage) -> Vec<String> {
        parsed.exports[0]
            .script
            .as_ref()
            .expect("a script")
            .statements
            .iter()
            .map(|s| format!("0x{:04X} {}", s.offset, kismet::render(&s.expr)))
            .collect()
    }

    fn edits(list: Vec<ScriptConstEdit>) -> PackageEdits {
        PackageEdits {
            scripts: list,
            ..Default::default()
        }
    }

    #[test]
    fn the_fixture_script_reads_where_the_tests_say() {
        let header = header();
        let parsed = package(&header, &file(&header, &script_bytes()));
        assert_eq!(
            rendered(&parsed),
            [
                "0x0000 LocalFinalFunction 'Target'(411)",
                "0x000F Jump @0045 unless LocalVariable(Flag)",
                "0x001D LocalFinalFunction 'Target'(ObjectConst(/Game/T_A.T_A))",
                "0x0030 LocalFinalFunction 'Target'(True)",
                "0x003B PopExecutionFlowIfNot(LocalVariable(Flag))",
                "0x0045 Return Nothing",
                "0x0047 EndOfScript",
            ]
        );
    }

    /// A literal still takes a value at its own width, by its index or by where it starts.
    #[test]
    fn a_literal_is_set_by_its_index_or_its_offset() {
        let (_, after, applied) = save(&edits(vec![edit(0, None, "1000")])).expect("saved");
        assert_eq!(
            rendered(&after)[0],
            "0x0000 LocalFinalFunction 'Target'(1000)"
        );
        assert_eq!(
            (applied[0].before.as_str(), applied[0].after.as_str()),
            ("411", "1000")
        );
        let (_, after, _) = save(&edits(vec![edit(0, Some(9), "7")])).expect("saved");
        assert_eq!(rendered(&after)[0], "0x0000 LocalFinalFunction 'Target'(7)");
        let missing = save(&edits(vec![edit(0, Some(8), "7")])).expect_err("no expression there");
        assert!(
            missing.contains("no expression starts at 0x0008"),
            "{missing}"
        );
    }

    /// Never jumping aims the jump where the branch falls through; always jumping turns it into a
    /// plain jump with the old condition left behind it, where nothing reaches.
    #[test]
    fn a_jump_condition_is_fixed_either_way_without_moving_anything() {
        let (_, after, _) = save(&edits(vec![edit(0x0F, Some(0x0F), "true")])).expect("saved");
        assert_eq!(
            rendered(&after)[1],
            "0x000F Jump @001D unless LocalVariable(Flag)"
        );
        let (_, after, _) = save(&edits(vec![edit(0x0F, Some(0x0F), "false")])).expect("saved");
        let lines = rendered(&after);
        assert_eq!(lines[1], "0x000F Jump @0045");
        assert_eq!(lines[2], "0x0014 LocalVariable(Flag)");
        assert_eq!(
            lines[3],
            "0x001D LocalFinalFunction 'Target'(ObjectConst(/Game/T_A.T_A))"
        );
        let unclear =
            save(&edits(vec![edit(0x0F, Some(0x0F), "maybe")])).expect_err("not a condition");
        assert!(unclear.contains("is not true or false"), "{unclear}");
    }

    #[test]
    fn a_pop_condition_is_fixed_either_way() {
        let (_, after, _) = save(&edits(vec![edit(0x3B, Some(0x3B), "false")])).expect("saved");
        let lines = rendered(&after);
        assert_eq!(lines[4], "0x003B PopExecutionFlow");
        assert_eq!(lines[5], "0x003C LocalVariable(Flag)");
        let (_, after, _) = save(&edits(vec![edit(0x3B, Some(0x3B), "true")])).expect("saved");
        assert_eq!(rendered(&after)[4], "0x003B Nothing");
    }

    /// A pop on a call cannot leave the call to run as a statement of its own, so never popping
    /// replaces the condition with `True` and moves the code after it.
    #[test]
    fn never_popping_on_a_call_drops_the_call() {
        let mut script = vec![0x07];
        script.extend_from_slice(&0x1Au32.to_le_bytes());
        script.push(0x00);
        field_path(&mut script, "Flag");
        script.push(0x4F);
        call(&mut script, &[0x27]);
        script.extend_from_slice(&[0x04, 0x0B, 0x53]);
        let never = edits(vec![edit(0x0E, Some(0x0E), "true")]);
        let (before, after, _) = save_with(script.clone(), &never).expect("saved");
        assert_eq!(
            rendered(&before)[1],
            "0x000E PopExecutionFlowIfNot(LocalFinalFunction 'Target'(True))"
        );
        assert_eq!(
            rendered(&after),
            [
                "0x0000 Jump @0010 unless LocalVariable(Flag)",
                "0x000E PopExecutionFlowIfNot(True)",
                "0x0010 Return Nothing",
                "0x0012 EndOfScript",
            ]
        );
        let header = header();
        let mut tables = Tables {
            names: header.name_map.clone(),
            imports: header.imports.clone(),
        };
        let out = script_splices(
            &package(&header, &file(&header, &script)),
            &never,
            &mut tables,
            &header,
            &NO_BUNDLE,
            0,
        )
        .expect("planned");
        assert!(
            out.notes.iter().any(|note| note.ends_with(
                "the condition LocalFinalFunction 'Target'(True) is no longer evaluated"
            )),
            "{:?}",
            out.notes
        );
    }

    #[test]
    fn one_byte_forms_swap_their_token() {
        let (_, after, _) = save(&edits(vec![edit(0x30, Some(0x39), "false")])).expect("saved");
        assert_eq!(
            rendered(&after)[3],
            "0x0030 LocalFinalFunction 'Target'(False)"
        );
        let same = save(&edits(vec![edit(0x30, Some(0x39), "1")])).expect_err("already true");
        assert!(same.contains("already reads"), "{same}");
    }

    /// An object constant points at another object of the same class, an import added for one the
    /// package never named, or at nothing; the path has to be the dotted one the disassembly shows.
    #[test]
    fn an_object_constant_is_pointed_elsewhere() {
        let (_, after, _) = save(&edits(vec![edit(0x1D, Some(0x26), "None")])).expect("saved");
        assert_eq!(
            rendered(&after)[2],
            "0x001D LocalFinalFunction 'Target'(ObjectConst(None))"
        );
        let (_, after, _) =
            save(&edits(vec![edit(0x1D, Some(0x26), "/Game/T_B.T_B")])).expect("saved");
        assert_eq!(
            rendered(&after)[2],
            "0x001D LocalFinalFunction 'Target'(ObjectConst(/Game/T_B.T_B))"
        );
        assert_eq!(
            after.imports.last().map(|import| import.path.as_str()),
            Some("/Game/T_B.T_B")
        );
        let slash =
            save(&edits(vec![edit(0x1D, Some(0x26), "/Game/T_A/T_A")])).expect_err("slash form");
        assert!(slash.contains("/Game/T_A.T_A"), "{slash}");
        let function =
            save(&edits(vec![edit(0x1D, Some(0x26), "Target")])).expect_err("a function");
        assert!(function.contains("is a Function"), "{function}");
    }

    /// An edit made against a script that has since changed is refused rather than landing
    /// somewhere else, unless the drift is waved through.
    #[test]
    fn an_edit_that_expects_something_else_there_is_refused() {
        let mut stale = edit(0, Some(9), "7");
        stale.was = Some("412".into());
        let refused = save(&edits(vec![stale.clone()])).expect_err("drifted");
        assert!(refused.starts_with(DRIFT), "{refused}");
        let mut waved = edits(vec![stale]);
        waved.allow_drift = true;
        assert!(save(&waved).is_ok());
    }

    #[test]
    fn one_expression_is_not_changed_twice_in_one_save() {
        let both = save(&edits(vec![
            edit(0x0F, Some(0x0F), "false"),
            edit(0x0F, Some(0x0F), "true"),
        ]))
        .expect_err("overlap");
        assert!(both.contains("change the same bytes"), "{both}");
    }

    // An event graph and a stub entering it, for edits that change a script's length.

    const GRAPH_NAMES: &[&str] = &[
        "None",
        "Flag",
        "Target",
        "EntryPoint",
        "ExecuteUbergraph_X",
        "LatentActionInfo",
        "/Script/Engine",
        "ScriptStruct",
        "/Script/CoreUObject",
        "Function",
    ];

    fn graph_name(value: &str) -> FMinimalName {
        FMinimalName {
            index: GRAPH_NAMES
                .iter()
                .position(|n| *n == value)
                .expect("a graph test name") as i32,
            number: 0,
        }
    }

    fn graph_header() -> FLegacyPackageHeader {
        let import = |class: (&str, &str), object: &str| FObjectImport {
            class_package: graph_name(class.0),
            class_name: graph_name(class.1),
            outer_index: FPackageIndex::create_null(),
            object_name: graph_name(object),
            is_optional: false,
        };
        let mut header = FLegacyPackageHeader {
            name_map: FPackageNameMap::create_from_names(
                GRAPH_NAMES.iter().map(|n| (*n).to_string()).collect(),
            ),
            imports: vec![
                import(("/Script/CoreUObject", "Function"), "Target"),
                import(("/Script/CoreUObject", "ScriptStruct"), "LatentActionInfo"),
            ],
            ..Default::default()
        };
        header.summary.package_name = "/Game/Graph".into();
        header
    }

    fn graph_field(out: &mut Vec<u8>, value: &str) {
        out.extend_from_slice(&1i32.to_le_bytes());
        out.extend_from_slice(&graph_name(value).index.to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
    }

    fn graph_name_bytes(out: &mut Vec<u8>, value: &str) {
        out.extend_from_slice(&graph_name(value).index.to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
    }

    fn ansi(out: &mut Vec<u8>, value: &str) {
        out.push(0x1F);
        out.extend_from_slice(value.as_bytes());
        out.push(0);
    }

    fn call_target(out: &mut Vec<u8>, params: &[u8]) {
        out.push(0x46);
        out.extend_from_slice(&(-1i32).to_le_bytes());
        out.extend_from_slice(params);
        out.push(0x16);
    }

    /// `ExecuteUbergraph_X`, at loaded offsets 0x00, 0x0A, 0x18, 0x26, 0x42, 0x5F, 0x8F and 0x91:
    /// its dispatch, `Target("ab")`, `Jump @008F unless Flag`, `Self->Target("cd")`, a switch
    /// on `Flag` defaulting to `IntZero`, a latent action resuming at 0x8F, `Return` and the end.
    fn graph_script() -> Vec<u8> {
        let mut out = vec![0x4E, 0x00];
        graph_field(&mut out, "EntryPoint");
        let mut ab = Vec::new();
        ansi(&mut ab, "ab");
        call_target(&mut out, &ab);
        out.push(0x07);
        out.extend_from_slice(&0x8Fu32.to_le_bytes());
        out.push(0x00);
        graph_field(&mut out, "Flag");
        out.extend_from_slice(&[0x19, 0x17]);
        out.extend_from_slice(&14u32.to_le_bytes());
        graph_field(&mut out, "Flag");
        let mut cd = Vec::new();
        ansi(&mut cd, "cd");
        call_target(&mut out, &cd);
        out.push(0x69);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&0x5Fu32.to_le_bytes());
        out.push(0x00);
        graph_field(&mut out, "Flag");
        out.push(0x1D);
        out.extend_from_slice(&7i32.to_le_bytes());
        out.extend_from_slice(&0x5Eu32.to_le_bytes());
        ansi(&mut out, "e");
        out.push(0x25);
        let mut latent = vec![0x2F];
        latent.extend_from_slice(&(-2i32).to_le_bytes());
        latent.extend_from_slice(&32i32.to_le_bytes());
        latent.push(0x5B);
        latent.extend_from_slice(&0x8Fu32.to_le_bytes());
        latent.push(0x1D);
        latent.extend_from_slice(&5i32.to_le_bytes());
        latent.push(0x21);
        graph_name_bytes(&mut latent, "ExecuteUbergraph_X");
        latent.extend_from_slice(&[0x17, 0x30]);
        call_target(&mut out, &latent);
        out.extend_from_slice(&[0x04, 0x0B, 0x53]);
        out
    }

    /// `ReceiveBeginPlay`, entering the event graph at `entry` through `literal`.
    fn stub_script(literal: &[u8]) -> Vec<u8> {
        let mut out = vec![0x45];
        graph_name_bytes(&mut out, "ExecuteUbergraph_X");
        out.extend_from_slice(literal);
        out.push(0x16);
        out.extend_from_slice(&[0x04, 0x0B, 0x53]);
        out
    }

    fn int_literal(value: i32) -> Vec<u8> {
        let mut out = vec![0x1D];
        out.extend_from_slice(&value.to_le_bytes());
        out
    }

    /// The functions laid end to end, each behind its size words, and the package read from them.
    fn graph_package(
        header: &FLegacyPackageHeader,
        whole: &[u8],
        bounds: &[(usize, usize)],
    ) -> ParsedPackage {
        let names = ["ExecuteUbergraph_X", "ReceiveBeginPlay"];
        let mut exports: Vec<ParsedExport> = bounds
            .iter()
            .enumerate()
            .map(|(index, (from, to))| {
                let part = &whole[*from..*to];
                let words =
                    |at: usize| u32::from_le_bytes(part[at..at + 4].try_into().expect("a word"));
                assert_eq!(words(4) as usize, part.len() - 8, "the stored size word");
                let ctx = Ctx {
                    mappings: None,
                    header,
                    fixups: None,
                    synth: None,
                    local: None,
                };
                let mut diagnostics = Diagnostics::default();
                let script = kismet::read_script(
                    &part[8..],
                    (*from + 8) as u64,
                    *from as u64,
                    Some(words(0)),
                    words(4),
                    &ctx,
                    &mut diagnostics,
                );
                assert!(script.complete(), "{}: {:?}", names[index], script.stopped);
                ParsedExport {
                    object_name: names[index].into(),
                    class_name: "Function".into(),
                    serial_offset: *from as i64,
                    status: ExportStatus::Payload {
                        consumed: 8,
                        payload_bytes: part.len() as u64 - 8,
                        kind: "bytecode",
                    },
                    script: Some(script),
                    ..ParsedExport::blank(index as u32)
                }
            })
            .collect();
        kismet::settle_resize_locks(&mut exports);
        ParsedPackage::of_exports(exports)
    }

    fn laid_out(
        header: &FLegacyPackageHeader,
        scripts: &[Vec<u8>],
    ) -> (Vec<u8>, Vec<(usize, usize)>) {
        let mut whole = Vec::new();
        let mut bounds = Vec::new();
        for script in scripts {
            let from = whole.len();
            whole.extend(file(header, script));
            bounds.push((from, whole.len()));
        }
        (whole, bounds)
    }

    /// Saves `edits` over the event graph and its stub, reads both back and verifies them.
    fn save_graph(
        edits: &PackageEdits,
        stub_entry: &[u8],
    ) -> Result<(ParsedPackage, Vec<AppliedEdit>), String> {
        let header = graph_header();
        let (whole, bounds) = laid_out(&header, &[graph_script(), stub_script(stub_entry)]);
        let saved = save_over(&header, &whole, &bounds, edits)?;
        Ok((saved.after, saved.applied))
    }

    struct SavedGraph {
        header: FLegacyPackageHeader,
        bytes: Vec<u8>,
        bounds: Vec<(usize, usize)>,
        before: ParsedPackage,
        after: ParsedPackage,
        applied: Vec<AppliedEdit>,
    }

    /// Saves `edits` over functions laid out at `bounds`, reads them back and verifies them.
    fn save_over(
        header: &FLegacyPackageHeader,
        whole: &[u8],
        bounds: &[(usize, usize)],
        edits: &PackageEdits,
    ) -> Result<SavedGraph, String> {
        let before = graph_package(header, whole, bounds);
        let mut tables = Tables {
            names: header.name_map.clone(),
            imports: header.imports.clone(),
        };
        let bundle = AssetBundle {
            asset: &[],
            exports: whole,
        };
        let out = script_splices(&before, edits, &mut tables, header, &bundle, 0)?;
        let mut patched = whole.to_vec();
        let mut ordered: Vec<&Splice> = out.splices.iter().collect();
        ordered.sort_by_key(|splice| std::cmp::Reverse(splice.start));
        for splice in &ordered {
            patched.splice(
                splice.start as usize..splice.end as usize,
                splice.bytes.clone(),
            );
        }
        let moved = |at: usize| {
            let shift: i64 = out
                .splices
                .iter()
                .filter(|splice| splice.end as usize <= at)
                .map(|splice| splice.bytes.len() as i64 - (splice.end - splice.start) as i64)
                .sum();
            (at as i64 + shift) as usize
        };
        let now: Vec<(usize, usize)> = bounds
            .iter()
            .map(|(from, to)| (moved(*from), moved(*to)))
            .collect();
        let grown = FLegacyPackageHeader {
            name_map: tables.names.clone(),
            imports: tables.imports.clone(),
            ..header.clone()
        };
        let after = graph_package(&grown, &patched, &now);
        verify(&before, &after, edits)?;
        Ok(SavedGraph {
            header: grown,
            bytes: patched,
            bounds: now,
            before,
            after,
            applied: out.applied,
        })
    }

    fn graph_edit(export: u32, statement: u32, at: u32, value: &str) -> ScriptConstEdit {
        ScriptConstEdit {
            export,
            statement,
            at: Some(at),
            value: value.into(),
            ..ScriptConstEdit::default()
        }
    }

    fn lines_of(parsed: &ParsedPackage, export: usize) -> Vec<String> {
        let script = parsed.exports[export].script.as_ref().expect("a script");
        script
            .statements
            .iter()
            .map(|s| format!("0x{:04X} {}", s.offset, kismet::render(&s.expr)))
            .collect()
    }

    #[test]
    fn the_event_graph_fixture_reads_where_the_tests_say() {
        let header = graph_header();
        let (whole, bounds) = laid_out(&header, &[graph_script(), stub_script(&int_literal(0x18))]);
        let parsed = graph_package(&header, &whole, &bounds);
        assert_eq!(
            lines_of(&parsed, 0),
            [
                "0x0000 Jump LocalVariable(EntryPoint)",
                "0x000A LocalFinalFunction 'Target'(\"ab\")",
                "0x0018 Jump @008F unless LocalVariable(Flag)",
                "0x0026 Self->[Flag] LocalFinalFunction 'Target'(\"cd\")",
                "0x0042 SwitchValue(LocalVariable(Flag), 7 => \"e\", default => IntZero)",
                "0x005F LocalFinalFunction 'Target'(StructConst<'LatentActionInfo', 32>(SkipOffsetConst(@008F), 5, 'ExecuteUbergraph_X', Self))",
                "0x008F Return Nothing",
                "0x0091 EndOfScript",
            ]
        );
        let graph = parsed.exports[0].script.as_ref().expect("a script");
        assert!(graph.resize_locks.is_empty(), "{:?}", graph.resize_locks);
    }

    /// A longer string moves every offset after it: the branch's target, the switch's arms, the
    /// latent resume point and the event the stub enters at, and the size words grow with it.
    #[test]
    fn a_longer_string_moves_everything_after_it() {
        let edits = edits(vec![graph_edit(0, 0x0A, 0x13, "abcdef")]);
        let (after, applied) = save_graph(&edits, &int_literal(0x18)).expect("saved");
        assert_eq!(
            lines_of(&after, 0),
            [
                "0x0000 Jump LocalVariable(EntryPoint)",
                "0x000A LocalFinalFunction 'Target'(\"abcdef\")",
                "0x001C Jump @0093 unless LocalVariable(Flag)",
                "0x002A Self->[Flag] LocalFinalFunction 'Target'(\"cd\")",
                "0x0046 SwitchValue(LocalVariable(Flag), 7 => \"e\", default => IntZero)",
                "0x0063 LocalFinalFunction 'Target'(StructConst<'LatentActionInfo', 32>(SkipOffsetConst(@0093), 5, 'ExecuteUbergraph_X', Self))",
                "0x0093 Return Nothing",
                "0x0095 EndOfScript",
            ]
        );
        assert_eq!(
            lines_of(&after, 1)[0],
            "0x0000 LocalVirtualFunction ExecuteUbergraph_X(28)"
        );
        let layout = applied.last().expect("a layout line");
        assert!(
            layout.after.starts_with("150 bytes loaded"),
            "{}",
            layout.after
        );
        assert!(layout.after.contains("1 event entry"), "{}", layout.after);
    }

    /// A context's skip is the length of its member, so a longer string inside the member grows it.
    #[test]
    fn a_change_inside_a_context_grows_its_skip() {
        let edits = edits(vec![graph_edit(0, 0x26, 0x3D, "cdxyz")]);
        let (after, _) = save_graph(&edits, &int_literal(0x18)).expect("saved");
        let graph = after.exports[0].script.as_ref().expect("a script");
        let Expr::Context { skip, .. } = &graph.statements[3].expr else {
            panic!("expected the context");
        };
        assert_eq!(*skip, 17);
        assert_eq!(
            lines_of(&after, 0)[2],
            "0x0018 Jump @0092 unless LocalVariable(Flag)"
        );
    }

    /// A one-byte form widens when it takes a value it cannot hold; two edits in one script move
    /// the code after each by what both added before it.
    #[test]
    fn two_changes_in_one_script_move_what_follows_by_both() {
        let edits = edits(vec![
            graph_edit(0, 0x0A, 0x13, "abc"),
            graph_edit(0, 0x42, 0x5E, "1000"),
        ]);
        let (after, _) = save_graph(&edits, &int_literal(0x18)).expect("saved");
        let lines = lines_of(&after, 0);
        assert_eq!(
            lines[4],
            "0x0043 SwitchValue(LocalVariable(Flag), 7 => \"e\", default => 1000)"
        );
        assert_eq!(
            lines[5],
            "0x0064 LocalFinalFunction 'Target'(StructConst<'LatentActionInfo', 32>(SkipOffsetConst(@0094), 5, 'ExecuteUbergraph_X', Self))"
        );
        assert_eq!(lines[6], "0x0094 Return Nothing");
    }

    /// An event entering through a byte-sized literal cannot be moved past what a byte holds, and
    /// a script that has to keep its size takes a change of the same width only.
    #[test]
    fn a_resize_the_stubs_or_the_script_cannot_follow_is_refused() {
        let byte_entry = [0x2C, 0x18];
        let edits_long = edits(vec![graph_edit(0, 0x0A, 0x13, "abcdef")]);
        let refused = save_graph(&edits_long, &byte_entry).expect_err("byte entry");
        assert!(refused.contains("cannot hold 0x001C"), "{refused}");

        let header = graph_header();
        let mut locked = graph_script();
        // A computed jump on anything but the entry point goes where nothing can follow.
        locked[2..6].copy_from_slice(&1i32.to_le_bytes());
        locked[6..10].copy_from_slice(&graph_name("Flag").index.to_le_bytes());
        let (whole, bounds) = laid_out(&header, &[locked, stub_script(&int_literal(0x18))]);
        let parsed = graph_package(&header, &whole, &bounds);
        let mut tables = Tables {
            names: header.name_map.clone(),
            imports: header.imports.clone(),
        };
        let refused = script_splices(&parsed, &edits_long, &mut tables, &header, &NO_BUNDLE, 0)
            .err()
            .expect("locked");
        assert!(refused.contains("has to keep its size"), "{refused}");
        let same = edits(vec![graph_edit(0, 0x0A, 0x13, "xy")]);
        assert!(script_splices(&parsed, &same, &mut tables, &header, &NO_BUNDLE, 0).is_ok());
    }

    /// A stub's entry is the event graph's to move when the graph changes size, so a save that also
    /// sets it by hand writes the same bytes twice and is refused by name.
    #[test]
    fn an_entry_the_save_also_moves_is_not_set_by_hand() {
        let both = edits(vec![
            graph_edit(1, 0x00, 0x0D, "38"),
            graph_edit(0, 0x0A, 0x13, "abcdef"),
        ]);
        let refused = save_graph(&both, &int_literal(0x18)).expect_err("collides");
        assert!(
            refused.contains("ReceiveBeginPlay script 0x0000 at 0x000D")
                && refused.contains("the offset ReceiveBeginPlay enters ExecuteUbergraph_X at"),
            "{refused}"
        );
        let mut payload = edits(vec![graph_edit(0, 0x0A, 0x13, "abcdef")]);
        payload.payloads.push(crate::edit::PayloadEdit {
            export: 1,
            bytes: stub_script(&int_literal(0x26)),
        });
        let refused = save_graph(&payload, &int_literal(0x18)).expect_err("collides");
        assert!(
            refused.contains("the new payload for ReceiveBeginPlay"),
            "{refused}"
        );
        let alone = edits(vec![graph_edit(1, 0x00, 0x0D, "38")]);
        let (after, _) = save_graph(&alone, &int_literal(0x18)).expect("saved");
        assert_eq!(
            lines_of(&after, 1)[0],
            "0x0000 LocalVirtualFunction ExecuteUbergraph_X(38)"
        );
    }

    /// Widening every literal and narrowing each back gives the bytes it started from: every offset
    /// relocation moved, it moves back to exactly where it was.
    #[test]
    fn widening_and_narrowing_back_gives_the_same_bytes() {
        let header = graph_header();
        let (whole, bounds) = laid_out(&header, &[graph_script(), stub_script(&int_literal(0x18))]);
        let parsed = graph_package(&header, &whole, &bounds);
        let widening = edits(widening_edits(&parsed, None, true));
        assert_eq!(widening.scripts.len(), 4);
        let wide = save_over(&header, &whole, &bounds, &widening).expect("widened");
        assert_ne!(wide.bytes, whole);
        let narrowing = edits(narrowing_edits(&wide.before, &wide.after));
        assert_eq!(narrowing.scripts.len(), 4);
        let back =
            save_over(&wide.header, &wide.bytes, &wide.bounds, &narrowing).expect("narrowed");
        assert_eq!(back.bytes, whole);
    }

    /// A one-byte 0 widens like any other, but narrows to `IntZero`, so the reversible widening
    /// leaves it alone.
    #[test]
    fn only_what_narrows_back_is_widened_reversibly() {
        let header = graph_header();
        let (whole, bounds) = laid_out(&header, &[graph_script(), stub_script(&[0x2C, 0x00])]);
        let parsed = graph_package(&header, &whole, &bounds);
        let in_stub = |edits: &[ScriptConstEdit]| edits.iter().filter(|e| e.export == 1).count();
        assert_eq!(in_stub(&widening_edits(&parsed, None, false)), 1);
        assert_eq!(in_stub(&widening_edits(&parsed, None, true)), 0);
    }

    #[test]
    fn a_string_beyond_ascii_becomes_a_unicode_one() {
        let edits = edits(vec![graph_edit(0, 0x0A, 0x13, "h\u{e9}")]);
        let (after, _) = save_graph(&edits, &int_literal(0x18)).expect("saved");
        let graph = after.exports[0].script.as_ref().expect("a script");
        assert!(
            kismet::literals(&graph.statements[1].expr)
                .iter()
                .any(|literal| matches!(literal, Expr::UnicodeStringConst { value, .. } if value == "h\u{e9}")),
            "{:?}",
            graph.statements[1].expr
        );
        let nul = save_graph(&edits_with("a\0b"), &int_literal(0x18)).expect_err("nul");
        assert!(nul.contains("NUL"), "{nul}");
    }

    fn edits_with(value: &str) -> PackageEdits {
        edits(vec![graph_edit(0, 0x0A, 0x13, value)])
    }

    fn ansi_bytes(out: &mut Vec<u8>, value: &str) {
        out.push(0x1F);
        out.extend_from_slice(value.as_bytes());
        out.push(0);
    }

    /// `Target(<text>)`, `Return Nothing` and the end marker.
    fn text_script(text: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        call(&mut out, text);
        out.extend_from_slice(&[0x04, 0x0B, 0x53]);
        out
    }

    fn invariant_text(value: &str) -> Vec<u8> {
        let mut out = vec![0x29, 2];
        ansi_bytes(&mut out, value);
        out
    }

    fn table_text(key: &str) -> Vec<u8> {
        let mut out = vec![0x29, 4];
        out.extend_from_slice(&(-5i32).to_le_bytes());
        ansi_bytes(&mut out, "/Game/ST_A.ST_A");
        ansi_bytes(&mut out, key);
        out
    }

    fn text_edit(value: &str) -> PackageEdits {
        edits(vec![edit(0, Some(9), value)])
    }

    /// A text takes UE's literal syntax whole, or a plain string as its new source, and the script
    /// moves to make room for whatever length that comes to.
    #[test]
    fn a_script_text_takes_a_literal_or_a_new_source() {
        let (_, after, _) =
            save_with(invariant_text_script("Hi"), &text_edit("Hello there")).expect("saved");
        assert_eq!(
            rendered(&after),
            [
                "0x0000 LocalFinalFunction 'Target'(TextConst<Invariant>(\"Hello there\"))",
                "0x0019 Return Nothing",
                "0x001B EndOfScript",
            ]
        );
        let (_, after, applied) = save_with(
            invariant_text_script("Hi"),
            &text_edit("NSLOCTEXT(\"Menu\", \"Title\", \"Play\")"),
        )
        .expect("saved");
        assert_eq!(
            rendered(&after)[0],
            "0x0000 LocalFinalFunction 'Target'(TextConst<Localized>(\"Play\", \"Title\", \"Menu\"))"
        );
        assert_eq!(applied[0].after, "NSLOCTEXT(\"Menu\", \"Title\", \"Play\")");
        let transform = save_with(
            invariant_text_script("Hi"),
            &text_edit("LOCGEN_TOUPPER(INVTEXT(\"x\"))"),
        )
        .expect_err("no transformed form");
        assert!(transform.contains("no transformed form"), "{transform}");
    }

    fn invariant_text_script(value: &str) -> Vec<u8> {
        text_script(&invariant_text(value))
    }

    /// A table text is pointed at another table through an import of it, the way the cook names
    /// the tables a script's texts read; a plain string makes it show that string everywhere.
    #[test]
    fn a_script_text_is_pointed_at_a_table_or_away_from_one() {
        let (_, after, applied) = save_with(
            invariant_text_script("Hi"),
            &text_edit("LOCTABLE(\"/Game/UI/ST_B.ST_B\", \"Play\")"),
        )
        .expect("saved");
        assert_eq!(
            rendered(&after)[0],
            "0x0000 LocalFinalFunction 'Target'(TextConst<StringTable, /Game/UI/ST_B.ST_B>(\"/Game/UI/ST_B.ST_B\", \"Play\"))"
        );
        assert_eq!(
            after.imports.last().map(|import| import.path.as_str()),
            Some("/Game/UI/ST_B.ST_B")
        );
        assert_eq!(
            applied[0].after,
            "LOCTABLE(\"/Game/UI/ST_B.ST_B\", \"Play\")"
        );

        let (_, after, _) = save_with(
            text_script(&table_text("Old")),
            &text_edit("/Game/ST_A.ST_A:New"),
        )
        .expect("saved");
        assert_eq!(
            rendered(&after)[0],
            "0x0000 LocalFinalFunction 'Target'(TextConst<StringTable, /Game/ST_A.ST_A>(\"/Game/ST_A.ST_A\", \"New\"))"
        );

        let header = header();
        let before = package(&header, &file(&header, &text_script(&table_text("Old"))));
        let mut tables = Tables {
            names: header.name_map.clone(),
            imports: header.imports.clone(),
        };
        let out = script_splices(
            &before,
            &text_edit("Plain words"),
            &mut tables,
            &header,
            &NO_BUNDLE,
            0,
        )
        .expect("planned");
        assert!(
            out.notes
                .iter()
                .any(|note| note.contains("no longer follows /Game/ST_A.ST_A:Old")),
            "{:?}",
            out.notes
        );
    }

    /// A final call is pointed at another function by its path, through an import the package
    /// gains when it never named the function; the path has to be the dotted one.
    #[test]
    fn a_final_call_is_pointed_at_another_function() {
        let (_, after, _) =
            save(&edits(vec![edit(0, Some(0), "/Game/Fn.Fn:Other")])).expect("saved");
        assert_eq!(
            rendered(&after)[0],
            "0x0000 LocalFinalFunction /Game/Fn.Fn:Other(411)"
        );
        let class = after.imports.last().expect("the new import");
        assert_eq!(class.path, "/Game/Fn.Fn:Other");
        let bare = save(&edits(vec![edit(0, Some(0), "Other")])).expect_err("a bare name");
        assert!(bare.contains("by the path the disassembly shows"), "{bare}");
        let same = save(&edits(vec![edit(0, Some(0), "Target")])).expect_err("already");
        assert!(
            same.contains("names its function by the path") || same.contains("already"),
            "{same}"
        );
    }

    /// A call into an event graph carries the offset an event enters at, so neither it nor a call
    /// made to look like one is retargeted; a virtual call takes a bare name.
    #[test]
    fn a_call_into_an_event_graph_is_not_retargeted() {
        let refused = save_graph(
            &edits(vec![graph_edit(1, 0, 0, "ReceiveTick")]),
            &int_literal(0x18),
        )
        .expect_err("an event graph call");
        assert!(refused.contains("into an event graph"), "{refused}");
        let header = graph_header();
        let mut stub = vec![0x45];
        graph_name_bytes(&mut stub, "Flag");
        stub.push(0x16);
        stub.extend_from_slice(&[0x04, 0x0B, 0x53]);
        let (whole, bounds) = laid_out(&header, &[graph_script(), stub]);
        let parsed = graph_package(&header, &whole, &bounds);
        let mut tables = Tables {
            names: header.name_map.clone(),
            imports: header.imports.clone(),
        };
        let renamed = script_splices(
            &parsed,
            &edits(vec![graph_edit(1, 0, 0, "Target")]),
            &mut tables,
            &header,
            &NO_BUNDLE,
            0,
        )
        .expect("a virtual call takes another name");
        assert_eq!(renamed.applied[0].after, "LocalVirtualFunction Target()");
        let path = script_splices(
            &parsed,
            &edits(vec![graph_edit(1, 0, 0, "/Game/X.X:Y")]),
            &mut tables,
            &header,
            &NO_BUNDLE,
            0,
        )
        .err()
        .expect("a path for a virtual call");
        assert!(path.contains("bare name"), "{path}");
    }
}
