//! A Blueprint class's variables and functions, and everything in a package that names one.
//!
//! A member is named in many ways: by the field record and the function map that declare it, by
//! the field paths and the calls of the bytecode, by a literal an engine function takes a name in,
//! by the tags its objects store values under, and by the binding objects and delegates that point
//! at a function by name. Every name a package holds is on record with its offset once the package
//! reads whole, so each one with the member's name is either one of those uses, a use of something
//! else that shares the name, or one this cannot tell, which is what an edit has to be told about.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::kismet::{Expr, NameRole, PropertyRef, Script, callee_name};
use crate::mappings::Mappings;
use crate::package::{AssetBundle, ParsedExport, ParsedPackage, header_size};
use crate::ustruct::RecordTail;
use crate::value::{PropertyEntry, PropertyValue};

/// Which member of a class: a variable or a function the class declares, or a parameter or local
/// of one of its functions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberTarget {
    /// The class, by full path: `/Game/X/BP.BP_C`.
    pub class: String,
    /// For a parameter or local, the function declaring it, by full path.
    pub function: Option<String>,
    pub name: String,
    pub kind: MemberKind,
    /// The class's ancestors by short name, nearest first: a call or a literal acting on an
    /// object of one of these may reach the member, on an object that is one of the class.
    pub ancestors: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberKind {
    Variable,
    Function,
    /// A parameter or local of a function.
    Local,
}

/// How a use names the member.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum UseRole {
    /// The field record declaring a variable, parameter or local, its inner records included.
    Declared,
    /// The class's function map entry for a function.
    FunctionMap,
    /// The export holding a function, or the widget a widget variable is bound to by name.
    Export,
    /// Another package's import of the function.
    Import,
    /// A field path in a script, owned by the class or the function declaring the member.
    FieldPath,
    /// A field path an assignment statement only stores into, which reads nothing out of it.
    Assigned,
    /// A field path an assignment stores a call's result into. The call writes its value there, so
    /// the store keeps the variable, read or not: it is how a result nothing reads is compiled.
    CallResult,
    /// A call by name.
    Call,
    /// A call or a reference by the function's object, which a rename leaves as it is.
    Object,
    /// A delegate bound by name.
    Bind,
    /// An `InstanceDelegate` naming the function.
    InstanceDelegate,
    /// A latent action's `ExecutionFunction`, which it resumes into by name.
    Resume,
    /// A literal an engine function takes a member's name in.
    ByName { callee: String },
    /// The tag a tagged object stores the variable's value under.
    Tag,
    /// A name a binding object, a widget binding or a construction script node stores.
    Binding { field: String },
    /// A stored delegate naming the function.
    Delegate,
    /// The Python class the declaring class extends, whose code reaches the class's variables and
    /// functions by name: nothing a package holds says whether it does.
    PythonParent { class: String },
}

/// Where a use sits, which is how an edit reaches it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "site", rename_all = "snake_case")]
pub enum UseSite {
    /// An FName's eight bytes at this file offset.
    Name { at: u64 },
    /// An export's object name in the export table.
    Export { index: u32 },
    /// An import's object name in the import table.
    Import { index: i32 },
    /// A string literal in a script, by where its token sits.
    Script { export: u32, at: u64 },
    /// A stored string, by where its value starts.
    Value { export: u32, at: u64 },
    /// A package index naming the object, by where it sits.
    Index { at: u64 },
}

#[derive(Debug, Clone, Serialize)]
pub struct MemberUse {
    #[serde(flatten)]
    pub role: UseRole,
    #[serde(flatten)]
    pub site: UseSite,
    /// What holds it, as a reader would find it.
    pub label: String,
}

/// Every use of a member in one package.
#[derive(Debug, Clone, Default, Serialize)]
pub struct MemberUses {
    pub uses: Vec<MemberUse>,
    /// Uses by name on an object the package does not say is of the class: one of an ancestor's,
    /// or one whose class it does not record. Each reaches the member only on an object that is
    /// one of the class.
    pub possible: Vec<MemberUse>,
    /// Names and strings holding the member's name this cannot place, labelled.
    pub unclassified: Vec<String>,
    /// Exports with bytes nobody read, which may name the member where nothing records it.
    pub unread: Vec<String>,
}

impl MemberUses {
    /// Uses other than the member's own declaration.
    pub fn references(&self) -> impl Iterator<Item = &MemberUse> {
        self.uses.iter().filter(|u| {
            !matches!(
                u.role,
                UseRole::Declared | UseRole::FunctionMap | UseRole::Export
            )
        })
    }

    /// Uses that run or read the member, possible ones included: neither its declaration nor a
    /// value its objects store for it, which goes with it.
    pub fn live(&self) -> impl Iterator<Item = &MemberUse> {
        self.references()
            .filter(|u| u.role != UseRole::Tag)
            .chain(&self.possible)
    }

    /// The live uses that read or run the member, leaving out the assignments that only store into
    /// it.
    pub fn reads(&self) -> impl Iterator<Item = &MemberUse> {
        self.live().filter(|u| u.role != UseRole::Assigned)
    }
}

/// A variable or function a class declares, as `members_of` lists it.
#[derive(Debug, Clone, Serialize)]
pub struct Member {
    pub name: String,
    pub kind: MemberKind,
    /// The variable's type, or what kind of function it is.
    pub detail: String,
    /// The function's export, or for a variable, the widget export bound to it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub export: Option<u32>,
    /// A function overriding one of the parent class's, which the engine calls by its name.
    pub overrides: bool,
    /// An event stub, entering the event graph at the offset it passes.
    pub event: bool,
    /// The class's event graph.
    pub graph: bool,
    /// A function's parameter, which its callers pass.
    pub parameter: bool,
}

/// `FUNC_BlueprintEvent`, `FUNC_Event` and `FUNC_Net`: flags that say something outside the
/// package may call the function by its name.
const CALLED_FROM_OUTSIDE: u32 = 0x0800_0000 | 0x0000_0800 | 0x0000_0040;

/// The ancestors of the class at `class` by short name, nearest first: its parents in this
/// package, the first one it imports, and that one's own as the mappings know them.
pub fn ancestors_of(
    parsed: &ParsedPackage,
    class: u32,
    mappings: Option<&Mappings>,
) -> Vec<String> {
    let mut out = Vec::new();
    let mut at = parsed
        .exports
        .get(class as usize)
        .map_or(0, |export| export.super_index);
    for _ in 0..64 {
        let Some(path) = path_of(parsed, at) else {
            break;
        };
        let short = short_name(path).to_string();
        if at < 0 {
            out.push(short.clone());
            if let Some(chain) = mappings.map(|m| m.ancestry(&short)) {
                out.extend(chain.iter().map(|name| name.to_string()));
            }
            break;
        }
        out.push(short);
        at = parsed.exports[at as usize - 1].super_index;
    }
    let mut seen = BTreeSet::new();
    out.retain(|name| seen.insert(name.to_ascii_lowercase()));
    out
}

/// What a path names last: the class `BP_C` of `/Game/X/BP.BP_C`.
fn short_name(path: &str) -> &str {
    path.rsplit(['.', ':', '/']).next().unwrap_or(path)
}

/// How sure a use by name on an object is to reach the member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Receiver {
    /// The object is one of the class.
    Member,
    /// It may be: of an ancestor, or of a class the package does not record.
    Possible,
    /// It is of a class unrelated to the member's.
    Other,
}

/// The class an export belongs to: itself when it is one, else the nearest up its outers.
fn owning_class_of<'a>(parsed: &'a ParsedPackage, export: &'a ParsedExport) -> Option<&'a str> {
    let mut held = export;
    for _ in 0..64 {
        if held.layout.is_some() && held.class_name.ends_with("Class") {
            return Some(held.path.as_str());
        }
        held = parsed
            .exports
            .get((held.outer_index as usize).checked_sub(1)?)?;
    }
    None
}

/// The class the function at `function` belongs to, by path.
fn class_of_function<'a>(parsed: &'a ParsedPackage, function: &ParsedExport) -> Option<&'a str> {
    path_of(parsed, function.outer_index)
}

/// The class an object variable's field record says it holds, when the package declares it.
fn property_class<'a>(parsed: &'a ParsedPackage, property: &PropertyRef) -> Option<&'a str> {
    let owner = property.owner.index;
    if owner <= 0 {
        return None;
    }
    let layout = parsed.exports.get(owner as usize - 1)?.layout.as_ref()?;
    let name = property.path.split('.').next()?;
    let (record, _) = layout
        .records
        .iter()
        .find(|(record, _)| record.name.eq_ignore_ascii_case(name))?;
    match (&record.tail, record.kind.as_str()) {
        (RecordTail::Index(class), "ObjectProperty" | "WeakObjectProperty") => {
            path_of(parsed, *class)
        }
        _ => None,
    }
}

/// The class of the object an expression stands for, as far as the package records it.
fn static_class<'a>(
    parsed: &'a ParsedPackage,
    function: &ParsedExport,
    expr: &Expr,
) -> Option<&'a str> {
    match expr {
        Expr::SelfRef => class_of_function(parsed, function),
        Expr::Variable { property, .. } => property_class(parsed, property),
        Expr::Context { member, .. } => static_class(parsed, function, member),
        _ => None,
    }
}

/// How an object of the class at `path` relates to the member's class.
fn relate(parsed: &ParsedPackage, path: Option<&str>, target: &MemberTarget) -> Receiver {
    let Some(path) = path else {
        return Receiver::Possible;
    };
    // The class, or one this package derives from it.
    let mut at = parsed
        .exports
        .iter()
        .find(|e| e.path.eq_ignore_ascii_case(path))
        .map(|e| e.index as i32 + 1);
    let mut unknown = false;
    for _ in 0..64 {
        let Some(index) = at else {
            break;
        };
        let Some(held) = path_of(parsed, index) else {
            break;
        };
        if held.eq_ignore_ascii_case(&target.class) {
            return Receiver::Member;
        }
        if index < 0 {
            unknown = !held.starts_with("/Script/");
            break;
        }
        at = Some(parsed.exports[index as usize - 1].super_index).filter(|i| *i != 0);
    }
    if path.eq_ignore_ascii_case(&target.class) {
        return Receiver::Member;
    }
    let short = short_name(path);
    if target
        .ancestors
        .iter()
        .any(|held| held.eq_ignore_ascii_case(short))
    {
        return Receiver::Possible;
    }
    // A native class derives from no Blueprint class; another package's Blueprint might.
    if unknown || !(path.starts_with("/Script/") || parsed.exports.iter().any(|e| e.path == path)) {
        Receiver::Possible
    } else {
        Receiver::Other
    }
}

/// For each call, bind and by-name literal in the script, by where its name sits, how sure the
/// object it acts on is to be one of the member's class.
fn receivers(
    parsed: &ParsedPackage,
    function: &ParsedExport,
    script: &Script,
    target: &MemberTarget,
) -> BTreeMap<u64, Receiver> {
    let own = relate(parsed, class_of_function(parsed, function), target);
    let mut out = BTreeMap::new();
    crate::kismet::visit_spans(script, &mut |expr, span, parent| match expr {
        Expr::VirtualCall { .. } => {
            let on = match parent {
                Some(Expr::Context { object, member, .. })
                    if std::ptr::eq(member.as_ref(), expr) =>
                {
                    relate(parsed, static_class(parsed, function, object), target)
                }
                _ => own,
            };
            out.insert(span.at + 1, on);
        }
        Expr::BindDelegate { object, .. } => {
            out.insert(
                span.at + 1,
                relate(parsed, static_class(parsed, function, object), target),
            );
        }
        Expr::NameConst { at, .. }
        | Expr::StringConst { at, .. }
        | Expr::UnicodeStringConst { at, .. } => {
            let on = match parent {
                Some(Expr::FinalCall { params, .. } | Expr::VirtualCall { params, .. }) => {
                    params.first().map_or(Receiver::Possible, |object| {
                        relate(parsed, static_class(parsed, function, object), target)
                    })
                }
                // An instance delegate or a latent action resumes on the script's own object.
                _ => own,
            };
            let name_at = if matches!(expr, Expr::NameConst { .. }) {
                at + 1
            } else {
                *at
            };
            out.insert(name_at, on);
        }
        _ => {}
    });
    out
}

/// The variables and functions the class at `class` declares, in the order its layout lists them.
pub fn members_of(parsed: &ParsedPackage, class: u32) -> Result<Vec<Member>, String> {
    let export = parsed
        .exports
        .get(class as usize)
        .ok_or_else(|| format!("this package has no export {class}"))?;
    let layout = export
        .layout
        .as_ref()
        .ok_or_else(|| format!("{} is not a class whose layout was read", export.path))?;
    let entries = crate::kismet::ubergraph_entries(
        parsed
            .exports
            .iter()
            .filter_map(|e| Some((e.object_name.as_str(), e.script.as_ref()?))),
        &crate::kismet::event_graphs(&parsed.exports),
    );
    let stubs: BTreeSet<&str> = entries
        .values()
        .flatten()
        .map(|(_, name)| name.as_str())
        .collect();
    let widget = |name: &str| {
        parsed
            .exports
            .iter()
            .find(|e| e.object_name == name && is_widget_tree_child(parsed, e, class))
            .map(|e| e.index)
    };
    let types: BTreeMap<&str, String> = export
        .struct_definition
        .as_ref()
        .map(|definition| {
            definition
                .properties
                .iter()
                .map(|p| (p.name.as_str(), crate::ustruct::type_text(&p.inner)))
                .collect()
        })
        .unwrap_or_default();
    let mut members: Vec<Member> = layout
        .records
        .iter()
        .map(|(record, _)| Member {
            name: record.name.clone(),
            kind: MemberKind::Variable,
            detail: types
                .get(record.name.as_str())
                .cloned()
                .unwrap_or_else(|| record.kind.clone()),
            export: widget(&record.name),
            overrides: false,
            event: false,
            graph: false,
            parameter: false,
        })
        .collect();
    for &child in &layout.children {
        let Some(function) = child
            .checked_sub(1)
            .and_then(|at| parsed.exports.get(at as usize))
        else {
            continue;
        };
        let flags = function.signature.as_ref().map_or(0, |s| s.flags);
        let graph = function.is_event_graph();
        let event = stubs.contains(function.object_name.as_str());
        let detail = if graph {
            "event graph".to_string()
        } else if function.object_name.ends_with("__DelegateSignature") {
            "delegate signature".to_string()
        } else if event {
            "event".to_string()
        } else if flags & CALLED_FROM_OUTSIDE != 0 {
            "event or callable function".to_string()
        } else {
            "function".to_string()
        };
        members.push(Member {
            name: function.object_name.clone(),
            kind: MemberKind::Function,
            detail,
            export: Some(function.index),
            overrides: function.super_index != 0,
            event,
            graph,
            parameter: false,
        });
    }
    Ok(members)
}

/// The parameters and locals the function at `function` declares.
pub fn locals_of(parsed: &ParsedPackage, function: u32) -> Result<Vec<Member>, String> {
    let export = parsed
        .exports
        .get(function as usize)
        .ok_or_else(|| format!("this package has no export {function}"))?;
    let signature = export
        .signature
        .as_ref()
        .ok_or_else(|| format!("{} is not a function whose layout was read", export.path))?;
    Ok(signature
        .params
        .iter()
        .chain(&signature.locals)
        .map(|field| Member {
            name: field.name.clone(),
            kind: MemberKind::Local,
            detail: format!("{:?} {}", field.role, field.kind).to_lowercase(),
            export: None,
            overrides: false,
            event: false,
            graph: false,
            parameter: field.role != crate::ustruct::FieldRole::Local,
        })
        .collect())
}

/// Whether `export` is a widget in the widget tree of the class at `class`, which UMG binds to the
/// class's variable of the same name.
fn is_widget_tree_child(parsed: &ParsedPackage, export: &ParsedExport, class: u32) -> bool {
    let mut outer = export.outer_index;
    while outer > 0 {
        let Some(held) = parsed.exports.get(outer as usize - 1) else {
            return false;
        };
        if held.class_name == "WidgetTree" {
            return held.outer_index == class as i32 + 1;
        }
        outer = held.outer_index;
    }
    false
}

/// A name split the way an FName stores it: the text, and the number one higher than the `_N` it
/// prints with.
fn split_name(name: &str) -> (&str, i32) {
    match name.rsplit_once('_') {
        Some((head, digits))
            if !head.is_empty()
                && !digits.is_empty()
                && digits.bytes().all(|b| b.is_ascii_digit())
                && (digits == "0" || !digits.starts_with('0')) =>
        {
            match digits.parse::<i32>() {
                Ok(number) => (head, number + 1),
                Err(_) => (name, 0),
            }
        }
        _ => (name, 0),
    }
}

/// Engine functions that take a member's name as a literal: the callee, the argument holding it,
/// and whether it names a variable or a function.
fn by_name_argument(callee: &str, argument: usize) -> Option<MemberKind> {
    let timer = matches!(
        callee,
        "K2_SetTimer"
            | "K2_ClearTimer"
            | "K2_PauseTimer"
            | "K2_UnPauseTimer"
            | "K2_IsTimerActive"
            | "K2_IsTimerPaused"
            | "K2_TimerExists"
            | "K2_GetTimerElapsedTime"
            | "K2_GetTimerRemainingTime"
            | "K2_SetTimerForNextTick"
    );
    let property = callee.starts_with("Set") && callee.ends_with("PropertyByName");
    match argument {
        1 if timer => Some(MemberKind::Function),
        1 if property => Some(MemberKind::Variable),
        _ => None,
    }
}

/// What the literal whose token sits at an offset is passed to.
#[derive(Debug, Clone)]
enum LiteralUse {
    Argument { callee: String, kind: MemberKind },
    Resume,
}

/// Every name or string literal in `script` that an engine function takes a member's name in, or
/// that a latent action resumes into, by where its token sits.
fn literal_uses(script: &Script) -> BTreeMap<u64, LiteralUse> {
    fn literal_at(expr: &Expr) -> Option<u64> {
        match expr {
            Expr::NameConst { at, .. }
            | Expr::StringConst { at, .. }
            | Expr::UnicodeStringConst { at, .. } => Some(*at),
            _ => None,
        }
    }
    fn walk(expr: &Expr, out: &mut BTreeMap<u64, LiteralUse>) {
        match expr {
            Expr::FinalCall {
                function, params, ..
            } => {
                let callee = function.path.as_deref().map(callee_name).unwrap_or("");
                for (argument, param) in params.iter().enumerate() {
                    if let (Some(at), Some(kind)) =
                        (literal_at(param), by_name_argument(callee, argument))
                    {
                        out.insert(
                            at,
                            LiteralUse::Argument {
                                callee: callee.to_string(),
                                kind,
                            },
                        );
                    }
                }
            }
            Expr::VirtualCall {
                function, params, ..
            } => {
                for (argument, param) in params.iter().enumerate() {
                    if let (Some(at), Some(kind)) =
                        (literal_at(param), by_name_argument(function, argument))
                    {
                        out.insert(
                            at,
                            LiteralUse::Argument {
                                callee: function.clone(),
                                kind,
                            },
                        );
                    }
                }
            }
            Expr::StructConst {
                struct_type,
                fields,
                ..
            } if struct_type
                .path
                .as_deref()
                .is_some_and(|path| short_name(path) == "LatentActionInfo") =>
            {
                if let Some(at) = fields.get(2).and_then(literal_at) {
                    out.insert(at, LiteralUse::Resume);
                }
            }
            _ => {}
        }
        for child in crate::kismet::children(expr) {
            walk(child, out);
        }
    }
    let mut out = BTreeMap::new();
    for statement in &script.statements {
        walk(&statement.expr, &mut out);
    }
    out
}

/// Every string literal in `script`, by where its token sits, with its value.
fn string_literals(script: &Script) -> Vec<(u64, &str)> {
    fn walk<'a>(expr: &'a Expr, out: &mut Vec<(u64, &'a str)>) {
        match expr {
            Expr::StringConst { value, at } | Expr::UnicodeStringConst { value, at } => {
                out.push((*at, value.as_str()))
            }
            _ => {}
        }
        for child in crate::kismet::children(expr) {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    for statement in &script.statements {
        walk(&statement.expr, &mut out);
    }
    out
}

/// Fields of binding objects, widget bindings and construction script nodes that name a function.
const FUNCTION_FIELDS: &[&str] = &["FunctionNameToBind", "FunctionName"];
/// Fields of the same that name a variable: a component's, a widget's or an animation's.
const VARIABLE_FIELDS: &[&str] = &[
    "ComponentPropertyName",
    "PropertyName",
    "WidgetName",
    "SlotWidgetName",
    "AnimationToBind",
    "InternalVariableName",
];

/// The entries from the top of `entries` down to the innermost one whose span holds `at`.
fn entries_holding(entries: &[PropertyEntry], at: u64) -> Vec<&PropertyEntry> {
    let mut chain = Vec::new();
    let mut level = entries;
    while let Some(entry) = level.iter().find(|entry| {
        entry
            .span
            .is_some_and(|(start, end)| start <= at && at < end)
    }) {
        chain.push(entry);
        level = match &entry.value {
            PropertyValue::Struct { fields, .. } | PropertyValue::Default { fields, .. } => fields,
            PropertyValue::Array { items } | PropertyValue::Set { items } => {
                // Struct elements keep their fields' spans; reach into whichever holds `at`.
                match items.iter().find_map(|item| match item {
                    PropertyValue::Struct { fields, .. }
                        if fields.iter().any(|field| {
                            field
                                .span
                                .is_some_and(|(start, end)| start <= at && at < end)
                        }) =>
                    {
                        Some(fields.as_slice())
                    }
                    _ => None,
                }) {
                    Some(fields) => fields,
                    None => break,
                }
            }
            PropertyValue::Map { entries } => {
                match entries.iter().find_map(|pair| {
                    [&pair.key, &pair.value]
                        .into_iter()
                        .find_map(|half| match half {
                            PropertyValue::Struct { fields, .. }
                                if fields.iter().any(|field| {
                                    field
                                        .span
                                        .is_some_and(|(start, end)| start <= at && at < end)
                                }) =>
                            {
                                Some(fields.as_slice())
                            }
                            _ => None,
                        })
                }) {
                    Some(fields) => fields,
                    None => break,
                }
            }
            _ => break,
        };
    }
    chain
}

fn chain_label(export: &ParsedExport, chain: &[&PropertyEntry]) -> String {
    let mut label = export.object_name.clone();
    for entry in chain {
        label.push('.');
        label.push_str(&entry.label());
    }
    label
}

/// The package indices that stand for `path` in `parsed`: its own export, or an import of it.
fn indices_of(parsed: &ParsedPackage, path: &str) -> BTreeSet<i32> {
    let exports = parsed
        .exports
        .iter()
        .filter(|e| e.path.eq_ignore_ascii_case(path))
        .map(|e| e.index as i32 + 1);
    let imports = parsed
        .imports
        .iter()
        .filter(|i| i.path.eq_ignore_ascii_case(path))
        .map(|i| i.index);
    exports.chain(imports).collect()
}

/// The path of the object at a package index.
fn path_of(parsed: &ParsedPackage, index: i32) -> Option<&str> {
    if index > 0 {
        parsed
            .exports
            .get(index as usize - 1)
            .map(|e| e.path.as_str())
    } else if index < 0 {
        parsed
            .imports
            .iter()
            .find(|i| i.index == index)
            .map(|i| i.path.as_str())
    } else {
        None
    }
}

/// Whether the export is an object of the target's class, or of a class this package derives from
/// it, whose stored values are tagged by the class's variable names.
fn is_instance(parsed: &ParsedPackage, export: &ParsedExport, class: &str) -> bool {
    let mut at = export.class_index;
    for _ in 0..64 {
        let Some(path) = path_of(parsed, at) else {
            return false;
        };
        if path.eq_ignore_ascii_case(class) {
            return true;
        }
        if at <= 0 {
            return false;
        }
        at = parsed.exports[at as usize - 1].super_index;
    }
    false
}

/// Every use of `target` in the package: what holds the member's name and what each is.
pub fn member_uses(
    bundle: &AssetBundle<'_>,
    parsed: &ParsedPackage,
    target: &MemberTarget,
) -> Result<MemberUses, String> {
    let total = header_size(bundle)?;
    let (text, number) = split_name(&target.name);
    let index = parsed
        .names
        .iter()
        .position(|held| held.eq_ignore_ascii_case(text))
        .map(|at| at as i32);
    let name_at = |at: u64| -> Option<(i32, i32)> {
        let local = at.checked_sub(total)? as usize;
        let bytes = bundle.exports.get(local..local + 8)?;
        Some((
            i32::from_le_bytes(bytes[0..4].try_into().ok()?),
            i32::from_le_bytes(bytes[4..8].try_into().ok()?),
        ))
    };
    let names_member = |at: u64| index.is_some() && name_at(at) == index.map(|i| (i, number));

    let owners = match &target.function {
        Some(function) => indices_of(parsed, function),
        None => indices_of(parsed, &target.class),
    };
    let class_indices = indices_of(parsed, &target.class);
    let function_kind = target.kind == MemberKind::Function;

    let mut out = MemberUses::default();
    // Offsets placed, whether as a use of the member or of something sharing its name.
    let mut placed: BTreeSet<u64> = BTreeSet::new();
    let add = |out: &mut MemberUses, role: UseRole, site: UseSite, label: String| {
        out.uses.push(MemberUse { role, site, label });
    };

    for export in &parsed.exports {
        if !export.names_complete {
            out.unread.push(export.path.clone());
        }
        let declares_member = match &target.function {
            Some(function) => export.path.eq_ignore_ascii_case(function),
            None => export.path.eq_ignore_ascii_case(&target.class),
        };

        // Field records: the member's own, and others' sharing its name, which are left alone.
        if let Some(layout) = &export.layout {
            for (record, (start, end)) in &layout.records {
                let inside: Vec<u64> = export
                    .name_refs
                    .iter()
                    .copied()
                    .filter(|at| (*start..*end).contains(at))
                    .collect();
                let own = declares_member
                    && target.kind != MemberKind::Function
                    && record.name.eq_ignore_ascii_case(&target.name);
                for at in inside {
                    // A map's value record is named after the map, `<name>_Value`.
                    let value_name = own
                        && name_at(at).is_some_and(|(held, n)| {
                            n == 0
                                && parsed.names.get(held as usize).is_some_and(|held| {
                                    held.eq_ignore_ascii_case(&format!("{}_Value", target.name))
                                })
                        });
                    if names_member(at) || value_name {
                        placed.insert(at);
                        if own {
                            add(
                                &mut out,
                                UseRole::Declared,
                                UseSite::Name { at },
                                format!("{}.{}", export.object_name, record.name),
                            );
                        }
                    }
                }
            }
            // The function map, an FName and an index per entry.
            if let Some(map_at) = layout.function_map_at {
                for (position, (name, _)) in layout.function_map.iter().enumerate() {
                    let at = map_at + 4 + 12 * position as u64;
                    if names_member(at) {
                        placed.insert(at);
                        if declares_member && function_kind {
                            add(
                                &mut out,
                                UseRole::FunctionMap,
                                UseSite::Name { at },
                                format!("{}'s function map entry {name}", export.object_name),
                            );
                        }
                    }
                }
            }
        }

        // The script's names, each by what the reader read it as.
        if let Some(script) = &export.script {
            let literals = literal_uses(script);
            let assigned = crate::kismet::assigned_names(script);
            let on = receivers(parsed, export, script, target);
            for named in &script.name_roles {
                if !names_member(named.at) {
                    continue;
                }
                let label = format!(
                    "{} at {}",
                    export.object_name,
                    statement_label(script, named.at)
                );
                let role = match named.role {
                    NameRole::FieldPath { segment, owner } => {
                        if segment == 0 && owners.contains(&owner) && !function_kind {
                            Some(match assigned.get(&named.at) {
                                Some(true) => UseRole::CallResult,
                                Some(false) => UseRole::Assigned,
                                None => UseRole::FieldPath,
                            })
                        } else {
                            // A field another struct, function or class declares.
                            placed.insert(named.at);
                            None
                        }
                    }
                    NameRole::Call if function_kind => Some(UseRole::Call),
                    NameRole::Bind if function_kind => Some(UseRole::Bind),
                    NameRole::Literal { token: 0x4B } if function_kind => {
                        Some(UseRole::InstanceDelegate)
                    }
                    NameRole::Literal { .. } => match literals.get(&(named.at - 1)) {
                        Some(LiteralUse::Resume) if function_kind => Some(UseRole::Resume),
                        Some(LiteralUse::Argument { callee, kind }) if *kind == target.kind => {
                            Some(UseRole::ByName {
                                callee: callee.clone(),
                            })
                        }
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(role) = role {
                    placed.insert(named.at);
                    let site = UseSite::Name { at: named.at };
                    match role {
                        UseRole::FieldPath | UseRole::Assigned | UseRole::CallResult => {
                            add(&mut out, role, site, label)
                        }
                        _ => match on.get(&named.at).copied().unwrap_or(Receiver::Possible) {
                            Receiver::Member => add(&mut out, role, site, label),
                            Receiver::Possible => {
                                out.possible.push(MemberUse { role, site, label })
                            }
                            Receiver::Other => {}
                        },
                    }
                }
            }
            // A string literal naming a function for a timer, which is not an FName.
            for (at, value) in string_literals(script) {
                if !value.eq_ignore_ascii_case(&target.name) {
                    continue;
                }
                let label = format!("{} at {}", export.object_name, statement_label(script, at));
                match literals.get(&at) {
                    Some(LiteralUse::Argument { callee, kind }) if *kind == target.kind => {
                        let one = MemberUse {
                            role: UseRole::ByName {
                                callee: callee.clone(),
                            },
                            site: UseSite::Script {
                                export: export.index,
                                at,
                            },
                            label,
                        };
                        match on.get(&at).copied().unwrap_or(Receiver::Possible) {
                            Receiver::Member => out.uses.push(one),
                            Receiver::Possible => out.possible.push(one),
                            Receiver::Other => {}
                        }
                    }
                    _ => out
                        .unclassified
                        .push(format!("{label}: a string reading {value:?}")),
                }
            }
        }

        // The export table: the function's own export, the widget bound to a widget variable, and
        // anything else of the name, which is not the member.
        if export.object_name.eq_ignore_ascii_case(&target.name) {
            let function_export = function_kind && class_indices.contains(&export.outer_index);
            let widget = target.kind == MemberKind::Variable
                && parsed.exports.iter().any(|class| {
                    class.path.eq_ignore_ascii_case(&target.class)
                        && is_widget_tree_child(parsed, export, class.index)
                });
            if function_export || widget {
                add(
                    &mut out,
                    UseRole::Export,
                    UseSite::Export {
                        index: export.index,
                    },
                    export.path.clone(),
                );
            }
        }

        // Stored values: tags under the variable's name, and names and delegates pointing at the
        // member.
        let instance = !function_kind && is_instance(parsed, export, &target.class);
        let start = export.serial_offset.max(0) as u64;
        let end = start + export.serial_size.max(0) as u64;
        for tag in parsed
            .tag_bounds
            .iter()
            .filter(|tag| (start..end).contains(&tag.tag_at))
        {
            if !names_member(tag.tag_at) {
                continue;
            }
            placed.insert(tag.tag_at);
            let top = export
                .properties
                .iter()
                .any(|entry| entry.span.is_some_and(|(at, _)| at == tag.key));
            if instance && top && target.kind == MemberKind::Variable {
                add(
                    &mut out,
                    UseRole::Tag,
                    UseSite::Name { at: tag.tag_at },
                    format!("{}.{}", export.object_name, target.name),
                );
            }
        }
        // A binding object, a construction script node or a widget binding names a member of the
        // class it belongs to: the class itself, or the one up its chain of outers.
        let owning_class = relate(parsed, owning_class_of(parsed, export), target);
        for &at in &export.name_refs {
            if placed.contains(&at) || !names_member(at) {
                continue;
            }
            let chain = entries_holding(&export.properties, at);
            let Some(innermost) = chain.last() else {
                continue;
            };
            let (role, on) = match &innermost.value {
                PropertyValue::Name { .. }
                    if function_kind && FUNCTION_FIELDS.contains(&innermost.name.as_str()) =>
                {
                    (
                        Some(UseRole::Binding {
                            field: innermost.name.clone(),
                        }),
                        owning_class,
                    )
                }
                PropertyValue::Name { .. }
                    if !function_kind && VARIABLE_FIELDS.contains(&innermost.name.as_str()) =>
                {
                    (
                        Some(UseRole::Binding {
                            field: innermost.name.clone(),
                        }),
                        owning_class,
                    )
                }
                // A stored delegate names its object by path, which says nothing of its class.
                PropertyValue::Delegate { .. } | PropertyValue::Array { .. } if function_kind => {
                    (Some(UseRole::Delegate), Receiver::Possible)
                }
                _ => (None, Receiver::Possible),
            };
            if let Some(role) = role {
                placed.insert(at);
                let one = MemberUse {
                    role,
                    site: UseSite::Name { at },
                    label: chain_label(export, &chain),
                };
                match on {
                    Receiver::Member => out.uses.push(one),
                    Receiver::Possible => out.possible.push(one),
                    Receiver::Other => {}
                }
            }
        }
        // A widget binding names its widget by a string.
        if target.kind == MemberKind::Variable {
            collect_strings(&export.properties, &target.name, &mut |entry, chain| {
                let label = chain_label(export, &chain);
                match entry.span {
                    Some((at, _)) if entry.name == "ObjectName" => add(
                        &mut out,
                        UseRole::Binding {
                            field: entry.name.clone(),
                        },
                        UseSite::Value {
                            export: export.index,
                            at,
                        },
                        label,
                    ),
                    _ => out
                        .unclassified
                        .push(format!("{label}: a string reading {:?}", target.name)),
                }
            });
        }
    }

    // The function's object, named by a call or a reference by index: not in the class's own
    // function list and map, which declare it, nor in the function itself.
    if function_kind {
        let function_path = format!("{}:{}", target.class, target.name);
        let functions = indices_of(parsed, &function_path);
        let within = |path: &str, at: u64| {
            parsed.exports.iter().any(|e| {
                e.path.eq_ignore_ascii_case(path) && {
                    let start = e.serial_offset.max(0) as u64;
                    (start..start + e.serial_size.max(0) as u64).contains(&at)
                }
            })
        };
        for reference in &parsed.references {
            if functions.contains(&reference.index)
                && !within(&target.class, reference.at)
                && !within(&function_path, reference.at)
            {
                let label = parsed
                    .exports
                    .iter()
                    .find(|e| {
                        let start = e.serial_offset.max(0) as u64;
                        (start..start + e.serial_size.max(0) as u64).contains(&reference.at)
                    })
                    .map(|e| match &e.script {
                        Some(script) if (script.start..script.end).contains(&reference.at) => {
                            format!(
                                "{} at {}",
                                e.object_name,
                                statement_label(script, reference.at)
                            )
                        }
                        _ => e.object_name.clone(),
                    })
                    .unwrap_or_else(|| format!("{:#X}", reference.at));
                add(
                    &mut out,
                    UseRole::Object,
                    UseSite::Index { at: reference.at },
                    label,
                );
            }
        }
    }

    // Another package's import of the function, under an import of the class.
    if function_kind {
        for import in &parsed.imports {
            if import.object_name.eq_ignore_ascii_case(&target.name)
                && class_indices.contains(&import.outer_index)
            {
                add(
                    &mut out,
                    UseRole::Import,
                    UseSite::Import {
                        index: import.index,
                    },
                    import.path.clone(),
                );
            }
        }
    }

    // Whatever holds the name and is none of the above.
    for export in &parsed.exports {
        for &at in &export.name_refs {
            if placed.contains(&at) || !names_member(at) {
                continue;
            }
            let label = match &export.script {
                Some(script) if (script.start..script.end).contains(&at) => {
                    format!("{} at {}", export.object_name, statement_label(script, at))
                }
                _ => {
                    let chain = entries_holding(&export.properties, at);
                    if chain.is_empty() {
                        format!("{} at {at:#X}", export.object_name)
                    } else {
                        chain_label(export, &chain)
                    }
                }
            };
            out.unclassified.push(format!("{label}: a name"));
        }
    }
    // A Python parent's code reaches the members of the class by name, which no package records.
    if matches!(target.kind, MemberKind::Variable | MemberKind::Function)
        && let Some((index, python)) = python_parent(parsed, &target.class)
    {
        out.possible.push(MemberUse {
            role: UseRole::PythonParent {
                class: python.to_string(),
            },
            site: UseSite::Import { index },
            label: format!("{python}'s code reaches members by name, which no package records"),
        });
    }
    out.unclassified.sort();
    out.unclassified.dedup();
    Ok(out)
}

/// The import of the Python class the class at `class` extends, when this package declares the
/// class and the first ancestor it imports is one: the class's own Blueprint parents are walked
/// within the package first.
fn python_parent<'a>(parsed: &'a ParsedPackage, class: &str) -> Option<(i32, &'a str)> {
    let mut at = parsed
        .exports
        .iter()
        .find(|export| export.path == class)?
        .super_index;
    for _ in 0..64 {
        if at > 0 {
            at = parsed.exports.get(at as usize - 1)?.super_index;
            continue;
        }
        let path = path_of(parsed, at)?;
        return path
            .starts_with("/Script/UnrealEnginePython/")
            .then(|| (at, short_name(path)));
    }
    None
}

/// The array of structs at the top of an export holding `at`, and which of its elements does: how
/// a binding object's entry naming a member is taken out.
pub fn element_holding(parsed: &ParsedPackage, at: u64) -> Option<(u32, &PropertyEntry, u32)> {
    for export in &parsed.exports {
        for entry in &export.properties {
            let PropertyValue::Array { items } = &entry.value else {
                continue;
            };
            if !entry
                .span
                .is_some_and(|(start, end)| start <= at && at < end)
            {
                continue;
            }
            let index = items.iter().position(|item| match item {
                PropertyValue::Struct { fields, .. } => fields.iter().any(|field| {
                    field
                        .span
                        .is_some_and(|(start, end)| start <= at && at < end)
                }),
                _ => false,
            })?;
            return Some((export.index, entry, index as u32));
        }
    }
    None
}

/// Every stored string reading `text`, with the entries leading to it.
fn collect_strings<'a>(
    entries: &'a [PropertyEntry],
    text: &str,
    found: &mut dyn FnMut(&'a PropertyEntry, Vec<&'a PropertyEntry>),
) {
    fn walk<'a>(
        entries: &'a [PropertyEntry],
        text: &str,
        chain: &mut Vec<&'a PropertyEntry>,
        found: &mut dyn FnMut(&'a PropertyEntry, Vec<&'a PropertyEntry>),
    ) {
        for entry in entries {
            chain.push(entry);
            match &entry.value {
                PropertyValue::Str { value } if value.eq_ignore_ascii_case(text) => {
                    found(entry, chain.clone())
                }
                PropertyValue::Struct { fields, .. } | PropertyValue::Default { fields, .. } => {
                    walk(fields, text, chain, found)
                }
                PropertyValue::Array { items } | PropertyValue::Set { items } => {
                    for item in items {
                        if let PropertyValue::Struct { fields, .. } = item {
                            walk(fields, text, chain, found);
                        }
                    }
                }
                _ => {}
            }
            chain.pop();
        }
    }
    walk(entries, text, &mut Vec::new(), found);
}

/// The statement holding `at`, as a jump would name it.
fn statement_label(script: &Script, at: u64) -> String {
    script
        .statements
        .iter()
        .rev()
        .find(|statement| statement.at <= at)
        .map(|statement| format!("{:#06X}", statement.offset))
        .unwrap_or_else(|| format!("{at:#X}"))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::script_fixture::{Code, Function, Import, Package, event_graph};

    fn target(parsed: &ParsedPackage, name: &str, kind: MemberKind) -> MemberTarget {
        MemberTarget {
            class: parsed.exports[0].path.clone(),
            function: None,
            name: name.into(),
            kind,
            ancestors: Vec::new(),
        }
    }

    fn roles(uses: &[MemberUse]) -> Vec<String> {
        let mut out: Vec<String> = uses.iter().map(|u| format!("{:?}", u.role)).collect();
        out.sort();
        out
    }

    /// The event graph fixture's class lists its graph and its stub, the stub is named by the
    /// graph's call by object, the graph by the stub's call and its latent action's resume, and
    /// a local of the graph by its field paths.
    #[test]
    fn a_classes_members_and_their_uses_are_found() {
        let built = event_graph().build();
        let parsed = built.parsed();
        let members = members_of(&parsed, 0).expect("members");
        let graph = members
            .iter()
            .find(|m| m.name == "ExecuteUbergraph_BP_Test")
            .expect("graph");
        assert!(graph.graph && graph.kind == MemberKind::Function);
        let stub = members
            .iter()
            .find(|m| m.name == "ReceiveBeginPlay")
            .expect("stub");
        assert!(stub.event);

        let uses = member_uses(
            &built.bundle(),
            &parsed,
            &target(&parsed, "ReceiveBeginPlay", MemberKind::Function),
        )
        .expect("uses");
        assert_eq!(roles(&uses.uses), ["Export", "FunctionMap", "Object"]);
        assert!(uses.unclassified.is_empty(), "{:?}", uses.unclassified);

        let uses = member_uses(
            &built.bundle(),
            &parsed,
            &target(&parsed, "ExecuteUbergraph_BP_Test", MemberKind::Function),
        )
        .expect("uses");
        assert_eq!(
            roles(&uses.uses),
            ["Export", "FunctionMap", "Object", "Resume"]
        );
        assert!(uses.unclassified.is_empty(), "{:?}", uses.unclassified);

        let local = MemberTarget {
            function: Some(parsed.exports[1].path.clone()),
            ..target(&parsed, "Count", MemberKind::Local)
        };
        let uses = member_uses(&built.bundle(), &parsed, &local).expect("uses");
        assert!(roles(&uses.uses).contains(&"Declared".to_string()));
        let count = |role: UseRole| uses.uses.iter().filter(|u| u.role == role).count();
        assert!(
            count(UseRole::FieldPath) + count(UseRole::Assigned) >= 3,
            "{:?}",
            uses.uses
        );
        assert!(count(UseRole::Assigned) > 0 && count(UseRole::FieldPath) > 0);
        assert!(uses.unclassified.is_empty(), "{:?}", uses.unclassified);
    }

    /// A class `BP_Test_C` with an `Int` variable `Health` and a function `Fire` running `text`.
    fn health_stored_by(text: &str) -> (crate::script_fixture::Built, ParsedPackage) {
        let mut package = Package::new("BP_Test_C", &["Fire", "Health"]);
        package
            .members
            .push(crate::script_fixture::local("Health", "IntProperty"));
        let code = Code::new(&package).ops(&[0x04, 0x0B, 0x53]).bytes;
        package.functions.push(Function {
            name: "Fire",
            fields: Vec::new(),
            script: code,
            flags: 0,
        });
        let built = package.build();
        let edits = crate::edit::PackageEdits {
            script_texts: vec![crate::edit::ScriptTextEdit {
                export: 1,
                text: text.to_string(),
                was: None,
                object: None,
            }],
            ..Default::default()
        };
        let patched = crate::edit::patch_package(&built.bundle(), &built.parsed(), &edits, None)
            .expect("saved");
        let built = crate::script_fixture::Built {
            asset: patched.asset,
            exports: patched.exports,
        };
        let parsed = built.parsed();
        (built, parsed)
    }

    /// A variable an assignment only stores into is named by it as assigned, both the statement's
    /// property and the variable it assigns, while one it reads from is a field path.
    #[test]
    fn an_assignment_stores_into_a_variable_and_reads_its_value() {
        let (built, parsed) =
            health_stored_by("Let InstanceVariable(Health) = 5\nReturn Nothing\nEndOfScript\n");
        let health = target(&parsed, "Health", MemberKind::Variable);
        let uses = member_uses(&built.bundle(), &parsed, &health).expect("uses");
        assert_eq!(roles(&uses.uses), ["Assigned", "Assigned", "Declared"]);
        assert!(uses.reads().next().is_none());

        let (built, parsed) = health_stored_by(
            "Let InstanceVariable(Health) = InstanceVariable(Health)\nReturn Nothing\nEndOfScript\n",
        );
        let uses = member_uses(&built.bundle(), &parsed, &health).expect("uses");
        assert_eq!(
            roles(&uses.uses),
            ["Assigned", "Assigned", "Declared", "FieldPath"]
        );
        assert_eq!(uses.reads().count(), 1);
    }

    /// A call by name and a timer naming the function on the object itself are its uses; a name
    /// literal nothing takes a member's name in is reported as unplaced.
    #[test]
    fn calls_by_name_on_the_object_itself_are_uses() {
        let mut package = Package::new(
            "BP_Test_C",
            &[
                "Fire",
                "/Script/Engine",
                "KismetSystemLibrary",
                "K2_SetTimer",
            ],
        );
        let engine = package.import(Import {
            class_package: "/Script/CoreUObject",
            class: "Package",
            outer: 0,
            name: "/Script/Engine",
        });
        let library = package.import(Import {
            class_package: "/Script/CoreUObject",
            class: "Class",
            outer: engine,
            name: "KismetSystemLibrary",
        });
        let timer = package.import(Import {
            class_package: "/Script/CoreUObject",
            class: "Function",
            outer: library,
            name: "K2_SetTimer",
        });
        let code = Code::new(&package)
            .op(0x45)
            .name("Fire")
            .op(0x16)
            .op(0x68)
            .int(timer)
            .op(0x17)
            .op(0x1F)
            .ops(b"Fire ")
            .op(0x16)
            .op(0x21)
            .name("Fire")
            .ops(&[0x04, 0x0B, 0x53])
            .bytes;
        package.functions.push(Function {
            name: "Fire",
            fields: Vec::new(),
            script: code,
            flags: 0,
        });
        let built = package.build();
        let parsed = built.parsed();
        let uses = member_uses(
            &built.bundle(),
            &parsed,
            &target(&parsed, "Fire", MemberKind::Function),
        )
        .expect("uses");
        assert_eq!(
            roles(&uses.uses),
            [
                "ByName { callee: \"K2_SetTimer\" }",
                "Call",
                "Export",
                "FunctionMap"
            ]
        );
        assert_eq!(uses.unclassified.len(), 1, "{:?}", uses.unclassified);
        assert!(uses.possible.is_empty());
    }

    #[test]
    fn a_name_splits_as_an_fname_stores_it() {
        assert_eq!(split_name("Count"), ("Count", 0));
        assert_eq!(split_name("Count_2"), ("Count", 3));
        assert_eq!(split_name("Count_0"), ("Count", 1));
        assert_eq!(split_name("Count_02"), ("Count_02", 0));
        assert_eq!(split_name("Gal Msg UI"), ("Gal Msg UI", 0));
        assert_eq!(split_name("_3"), ("_3", 0));
    }

    #[test]
    fn engine_functions_taking_a_name_are_known_by_their_argument() {
        assert_eq!(
            by_name_argument("SetObjectPropertyByName", 1),
            Some(MemberKind::Variable)
        );
        assert_eq!(
            by_name_argument("K2_SetTimer", 1),
            Some(MemberKind::Function)
        );
        assert_eq!(by_name_argument("K2_SetTimer", 0), None);
        assert_eq!(by_name_argument("PrintString", 1), None);
    }
}
