//! What a call needs to fit the function it names: how many arguments, of what types, and what
//! comes back.
//!
//! A retarget holds what the call itself passes and keeps, read off the call, against the new
//! function's own field records. Only a Blueprint function has those: cooked data says nothing of
//! a native function's parameters, so a call pointed at one is not checked. A class the hierarchy
//! cannot place, or an enum that may be laid out like what it meets, leaves the answer unknown
//! rather than wrong, since a wrong answer refuses a save outright.

use std::collections::HashMap;

use crate::kismet::{self, CallUse, Expr, PropertyRef};
use crate::mappings::Mappings;
use crate::package::{ParsedExport, ParsedPackage};
use crate::ustruct::{FieldRole, FunctionSignature};

/// What `NoObject` passes: no object at all, which any object-like input takes.
const NONE: &str = "None";

/// The type words that name an object, a class or an interface, each followed by its class.
const OBJECTS: [&str; 7] = [
    "Object",
    "WeakObject",
    "LazyObject",
    "SoftObject",
    "Interface",
    "Class",
    "SoftClass",
];

/// The type words that are neither a struct nor an enum, and so mean the same in every package.
const PRIMITIVES: [&str; 22] = [
    "Bool",
    "Byte",
    "Int8",
    "Int16",
    "Int",
    "Int64",
    "UInt16",
    "UInt32",
    "UInt64",
    "Float",
    "Double",
    "Str",
    "Name",
    "Text",
    "Delegate",
    "MulticastDelegate",
    "FieldPath",
    "Array",
    "Set",
    "Map",
    "Optional",
    "Utf8Str",
];

/// One function's shape, or what one call passes: an argument type per parameter, `None` where
/// nothing said, and what it returns, `None` when nothing said.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallShape {
    pub args: Vec<Option<String>>,
    /// Whether each argument is known to be read rather than written: a call passed it something
    /// that is not this function's own result variable, or a signature says it is an input.
    pub inputs: Vec<bool>,
    pub returns: Option<String>,
}

/// What one call in a script passes the function it names, and keeps of what comes back.
#[derive(Debug, Clone)]
pub struct CallSite {
    /// A final call's function path, or the bare name a virtual call looks up.
    pub function: String,
    pub args: Vec<Option<String>>,
    pub inputs: Vec<bool>,
    /// The type of the variable the returned value is kept in, when a `Let` keeps it.
    pub returns: Option<String>,
}

/// Where the declared types of what a function's script reads come from: its own parameters and
/// locals, the class it belongs to, and the members of the classes and structs it reads from.
struct Types<'a> {
    locals: HashMap<&'a str, &'a str>,
    /// What `Self` is: the function's class, or the nearest of its ancestors the mappings know.
    own_class: Option<String>,
    parsed: &'a ParsedPackage,
    mappings: Option<&'a Mappings>,
}

impl<'a> Types<'a> {
    fn of(
        parsed: &'a ParsedPackage,
        export: &'a ParsedExport,
        mappings: Option<&'a Mappings>,
    ) -> Self {
        Self {
            locals: export
                .signature
                .iter()
                .flat_map(|signature| signature.params.iter().chain(&signature.locals))
                .map(|field| (field.name.as_str(), field.kind.as_str()))
                .collect(),
            own_class: own_class(parsed, export, mappings),
            parsed,
            mappings,
        }
    }

    /// A member's declared type, from the package's own definition of the class or struct that
    /// declares it, then from the mappings. Neither says which class an object member holds. A
    /// weak or lazy pointer reaches a script as the object it points at, which is what a call
    /// passing one takes.
    fn member(&self, property: &PropertyRef) -> Option<String> {
        let owner = property.owner.path.as_deref()?.rsplit(['.', ':']).next()?;
        let name = property.path.as_str();
        let own = self
            .parsed
            .exports
            .iter()
            .filter_map(|export| export.struct_definition.as_ref())
            .find(|definition| definition.name == owner)
            .and_then(|definition| {
                definition
                    .properties
                    .iter()
                    .find(|property| property.name == name)
            });
        let found = own.or_else(|| self.mappings?.member(owner, name))?;
        Some(held_as(&found.inner))
    }

    /// The type of each field a struct holds, in the order a struct literal gives them: from the
    /// mappings, or from the package's own definition of a Blueprint struct.
    fn fields_of(&self, name: &str) -> Option<Vec<String>> {
        if let Some(schema) = self.mappings.and_then(|mappings| mappings.schema(name)) {
            return Some(
                schema
                    .iter()
                    .map(|slot| held_as(&slot.property.inner))
                    .collect(),
            );
        }
        let definition = self
            .parsed
            .exports
            .iter()
            .filter_map(|export| export.struct_definition.as_ref())
            .find(|definition| definition.name == name)?;
        Some(
            definition
                .properties
                .iter()
                .map(|property| held_as(&property.inner))
                .collect(),
        )
    }

    /// The type of an object a script names outright: `Class<Name>` for a class, else an object
    /// of its class.
    fn object(&self, index: i32) -> Option<String> {
        let (class, name) = if index < 0 {
            let import = self.parsed.imports.iter().find(|i| i.index == index)?;
            (import.class_name.as_str(), import.object_name.as_str())
        } else if index > 0 {
            let export = self
                .parsed
                .exports
                .iter()
                .find(|e| e.index as i64 == i64::from(index) - 1)?;
            (export.class_name.as_str(), export.object_name.as_str())
        } else {
            return None;
        };
        Some(if class.ends_with("Class") {
            format!("Class<{name}>")
        } else {
            format!("Object<{class}>")
        })
    }
}

/// A declared type as a script reads it: a weak or lazy pointer reaches a script as the object it
/// points at.
fn held_as(inner: &usmap::PropertyInner) -> String {
    match inner {
        usmap::PropertyInner::WeakObject | usmap::PropertyInner::LazyObject => "Object".into(),
        _ => crate::ustruct::type_text(inner),
    }
}

/// The class a function's `Self` is, as the mappings can place it: the function's own class when
/// they know it, else the nearest ancestor its package names that they do.
fn own_class(
    parsed: &ParsedPackage,
    export: &ParsedExport,
    mappings: Option<&Mappings>,
) -> Option<String> {
    let class = parsed
        .exports
        .iter()
        .find(|e| e.index as i64 == i64::from(export.outer_index) - 1)?;
    let start = class.object_name.clone();
    let Some(mappings) = mappings else {
        return Some(start);
    };
    let mut name = start.clone();
    for _ in 0..32 {
        if mappings.has_struct(&name) {
            return Some(name);
        }
        let parent = parsed
            .exports
            .iter()
            .filter_map(|e| e.struct_definition.as_ref())
            .find(|definition| definition.name == name)
            .and_then(|definition| definition.super_struct.clone());
        match parent {
            Some(parent) => name = parent,
            None => break,
        }
    }
    Some(start)
}

/// The type a literal, a variable, a member or an object passed as an argument has.
fn arg_type(expr: &Expr, types: &Types<'_>) -> Option<String> {
    Some(match expr {
        Expr::IntConst { .. }
        | Expr::Simple {
            name: "IntZero" | "IntOne",
        }
        | Expr::ByteConst {
            name: "IntConstByte",
            ..
        } => "Int".into(),
        Expr::ByteConst { .. } => "Byte".into(),
        Expr::Int64Const { .. } => "Int64".into(),
        Expr::UInt64Const { .. } => "UInt64".into(),
        Expr::FloatConst { .. } => "Float".into(),
        Expr::DoubleConst { .. } => "Double".into(),
        Expr::Simple {
            name: "True" | "False",
        } => "Bool".into(),
        Expr::StringConst { .. } | Expr::UnicodeStringConst { .. } => "Str".into(),
        Expr::NameConst {
            name: "NameConst", ..
        } => "Name".into(),
        Expr::TextConst { .. } => "Text".into(),
        Expr::SelfRef => match &types.own_class {
            Some(class) => format!("Object<{class}>"),
            None => "Object".into(),
        },
        Expr::ObjectConst { object } => types
            .object(object.index)
            .unwrap_or_else(|| "Object".into()),
        Expr::Simple { name: "NoObject" } => NONE.into(),
        Expr::Cast { name, class, .. } => {
            let leaf = class.path.as_deref()?.rsplit(['.', ':']).next()?;
            match *name {
                "DynamicCast" | "InterfaceToObjCast" => format!("Object<{leaf}>"),
                "MetaCast" => format!("Class<{leaf}>"),
                "ObjToInterfaceCast" | "CrossInterfaceCast" => format!("Interface<{leaf}>"),
                _ => return None,
            }
        }
        Expr::Numbers { name, .. } => match *name {
            "VectorConst" => "Vector",
            "RotationConst" => "Rotator",
            "TransformConst" => "Transform",
            "Vector3fConst" => "Vector3f",
            _ => return None,
        }
        .into(),
        Expr::StructConst { struct_type, .. } => struct_type
            .path
            .as_deref()
            .and_then(|path| path.rsplit(['.', ':']).next())?
            .to_string(),
        Expr::Variable {
            name: "LocalVariable" | "LocalOutVariable",
            property,
        } => (*types.locals.get(property.path.as_str())?).to_string(),
        Expr::Variable {
            name: "InstanceVariable" | "DefaultVariable",
            property,
        }
        | Expr::Member {
            name: "StructMemberContext",
            property,
            ..
        } => types.member(property)?,
        // What another object holds is the member read from it.
        Expr::Context { member, .. } => return arg_type(member, types),
        Expr::Conversion { conversion, .. } => match kismet::conversion_name(*conversion)? {
            "DoubleToFloat" => "Float".into(),
            "FloatToDouble" => "Double".into(),
            _ => return None,
        },
        // A choice is what each of its results is, when they all agree.
        Expr::SwitchValue { cases, default, .. } => {
            let first = arg_type(default, types)?;
            for case in cases {
                if arg_type(&case.result, types)? != first {
                    return None;
                }
            }
            first
        }
        _ => return None,
    })
}

/// The name of the variable an expression stands for, for a message.
fn variable_name(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Variable { property, .. } | Expr::Member { property, .. } => {
            Some(property.path.as_str())
        }
        Expr::Context { member, .. } => variable_name(member),
        _ => None,
    }
}

/// What each statement of export `export` stores, held to where it stores it: the value a `Let`
/// puts in a variable, and each field of a struct literal. The VM copies a value's bytes as they
/// are, so a Float stored in a Double, or a Bool in an Int, is a wrong value in game rather than a
/// conversion. Only what is known on both sides is judged: anything a native call returns, an
/// enum, or a class the hierarchy cannot place is left alone. One finding, by the statement's
/// offset, for each statement that does not fit.
pub fn stores_in(
    parsed: &ParsedPackage,
    export: u32,
    mappings: Option<&Mappings>,
) -> Vec<(u32, Fit)> {
    let Some(found) = parsed.exports.iter().find(|e| e.index == export) else {
        return Vec::new();
    };
    let Some(script) = found.script.as_ref() else {
        return Vec::new();
    };
    let types = Types::of(parsed, found, mappings);
    script
        .statements
        .iter()
        .filter_map(|statement| {
            let mut fit = Fit::Fits;
            let mut pending = vec![&statement.expr];
            while let Some(expr) = pending.pop() {
                fit = fit.or(store_fit(expr, &types));
                pending.extend(kismet::children(expr));
            }
            (fit != Fit::Fits).then_some((statement.offset, fit))
        })
        .collect()
}

/// Whether one expression stores what fits where it stores it.
fn store_fit(expr: &Expr, types: &Types<'_>) -> Fit {
    match expr {
        Expr::Let {
            name,
            variable,
            value,
            ..
        } => {
            let Some(to) = arg_type(variable, types) else {
                return Fit::Fits;
            };
            let target = variable_name(variable).unwrap_or("the variable");
            // `LetBool` writes a bool and `LetObj` an object pointer, whatever they are given.
            let known = known_kind(&to, types.mappings);
            match *name {
                "LetBool" if known && to != "Bool" => {
                    return Fit::Mismatch(format!(
                        "stores a Bool with LetBool in {target}, which is a {to}"
                    ));
                }
                "LetObj" if known && !object_like(&to) => {
                    return Fit::Mismatch(format!(
                        "stores an object with LetObj in {target}, which is a {to}"
                    ));
                }
                _ => {}
            }
            let Some(from) = arg_type(value, types) else {
                return Fit::Fits;
            };
            match relate(&from, &to, true, types.mappings) {
                Err(Why::Differs) => {
                    Fit::Mismatch(format!("stores a {from} in {target}, which is a {to}"))
                }
                _ => Fit::Fits,
            }
        }
        Expr::StructConst {
            struct_type,
            fields,
            ..
        } => {
            let Some(name) = struct_type
                .path
                .as_deref()
                .and_then(|path| path.rsplit(['.', ':']).next())
            else {
                return Fit::Fits;
            };
            let Some(held) = types.fields_of(name) else {
                return Fit::Fits;
            };
            // A struct's transient fields are in its layout and not in a literal of it, so a count
            // that differs may still be right.
            if held.len() != fields.len() {
                return Fit::Unknown(format!(
                    "a {name} literal gives {} field(s), and {name} has {}",
                    fields.len(),
                    held.len()
                ));
            }
            for (at, (field, to)) in fields.iter().zip(&held).enumerate() {
                let Some(from) = arg_type(field, types) else {
                    continue;
                };
                if relate(&from, to, true, types.mappings) == Err(Why::Differs) {
                    return Fit::Mismatch(format!(
                        "field {at} of a {name} literal is a {from}, and {name} holds a {to} there"
                    ));
                }
            }
            Fit::Fits
        }
        _ => Fit::Fits,
    }
}

/// Whether a type is one whose bytes are known: a primitive, an object or a struct the mappings
/// hold, rather than an enum or a name nothing places.
fn known_kind(text: &str, mappings: Option<&Mappings>) -> bool {
    let head = without_classes(text);
    let head = head.split('<').next().unwrap_or_default();
    PRIMITIVES.contains(&head)
        || OBJECTS.contains(&head)
        || mappings.is_some_and(|m| m.has_struct(head) && !m.has_enum(head))
}

/// Whether a call reads `arg` rather than writing into it. A Blueprint wires a function's output
/// pins to temporaries named for that function, `CallFunc_<Function>_<Pin>`, and passes a
/// function's own output through as `LocalOutVariable`; anything else feeds an input. Its own
/// `ReturnValue` temporary is an earlier call's result being passed on.
fn is_input(arg: &Expr, callee: &str) -> bool {
    match arg {
        Expr::Variable {
            name: "LocalOutVariable",
            ..
        } => false,
        Expr::Variable {
            name: "LocalVariable",
            property,
        } => property
            .path
            .strip_prefix("CallFunc_")
            .and_then(|rest| rest.strip_prefix(callee))
            .and_then(|rest| rest.strip_prefix('_'))
            .is_none_or(|pin| pin.starts_with("ReturnValue")),
        _ => true,
    }
}

/// A function path's last name, which is what a virtual call and a result temporary use.
fn leaf(function: &str) -> &str {
    function.rsplit(['.', ':']).next().unwrap_or(function)
}

/// What the call `call` tells, given the variable a `Let` keeps its value in.
fn site_of(call: &Expr, kept_in: Option<&Expr>, types: &Types<'_>) -> Option<CallSite> {
    let (function, params) = match call {
        Expr::FinalCall {
            function, params, ..
        } => (function.path.clone()?, params),
        Expr::VirtualCall {
            function, params, ..
        } => (function.clone(), params),
        _ => return None,
    };
    let callee = leaf(&function).to_string();
    Some(CallSite {
        args: params.iter().map(|param| arg_type(param, types)).collect(),
        inputs: params
            .iter()
            .map(|param| is_input(param, &callee))
            .collect(),
        returns: kept_in.and_then(|variable| arg_type(variable, types)),
        function,
    })
}

/// The call starting at loaded offset `at` in the statement at `statement` of export `export`:
/// what it passes and keeps, and what becomes of its value.
pub fn site_at(
    parsed: &ParsedPackage,
    export: u32,
    statement: u32,
    at: u32,
    mappings: Option<&Mappings>,
) -> Option<(CallSite, CallUse)> {
    let found = parsed.exports.iter().find(|e| e.index == export)?;
    let script = found.script.as_ref()?;
    let types = Types::of(parsed, found, mappings);
    let (call, use_, kept_in) = kismet::call_expr_at(script, statement, at)?;
    Some((site_of(call, kept_in, &types)?, use_))
}

/// An object type at the top, `Object<Actor>` or a bare `Object`, as its word and its class.
fn top_class(text: &str) -> Option<(&str, Option<&str>)> {
    if OBJECTS.contains(&text) {
        return Some((text, None));
    }
    let (kind, rest) = text.split_once('<')?;
    let class = rest.strip_suffix('>')?;
    (OBJECTS.contains(&kind) && !class.contains('<')).then_some((kind, Some(class)))
}

fn object_like(text: &str) -> bool {
    OBJECTS.contains(&without_classes(text).as_str())
}

/// Whether an object type anywhere in `text` names no class, which leaves what it holds open.
fn names_no_class(text: &str) -> bool {
    let bare = without_classes(text);
    let classes = text.matches('<').count() - bare.matches('<').count();
    let objects = bare
        .split(['<', '>', ',', ' '])
        .filter(|word| OBJECTS.contains(word))
        .count();
    classes < objects
}

/// A type with the class after each object type dropped: `Array<Object<Actor>>` is
/// `Array<Object>`.
fn without_classes(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find('<') {
        let (head, tail) = rest.split_at(at);
        out.push_str(head);
        let word = head.rsplit(['<', ',', ' ']).next().unwrap_or(head);
        if OBJECTS.contains(&word) {
            // Skip the class, up to its closing bracket.
            let mut depth = 0;
            let mut end = tail.len();
            for (i, c) in tail.char_indices() {
                match c {
                    '<' => depth += 1,
                    '>' => {
                        depth -= 1;
                        if depth == 0 {
                            end = i + 1;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            rest = &tail[end..];
        } else {
            out.push('<');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

/// A shape written out, `(Int, Float) -> Bool`, with `?` for what nothing said. A return nothing
/// said is left out.
pub fn render(shape: &CallShape) -> String {
    let args: Vec<&str> = shape
        .args
        .iter()
        .map(|arg| arg.as_deref().unwrap_or("?"))
        .collect();
    match &shape.returns {
        Some(returns) => format!("({}) -> {returns}", args.join(", ")),
        None => format!("({})", args.join(", ")),
    }
}

/// A Blueprint function's shape, from its own field records.
pub fn shape_of_signature(signature: &FunctionSignature) -> CallShape {
    let params: Vec<_> = signature
        .params
        .iter()
        .filter(|param| param.role != FieldRole::Return)
        .collect();
    let returns = signature
        .params
        .iter()
        .find(|param| param.role == FieldRole::Return)
        .map(|param| param.kind.clone());
    CallShape {
        args: params
            .iter()
            .map(|param| Some(param.kind.clone()))
            .collect(),
        inputs: params
            .iter()
            .map(|param| param.role == FieldRole::In)
            .collect(),
        returns,
    }
}

/// How a call's new function compares with what the call passes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fit {
    Fits,
    /// Known to differ, with how.
    Mismatch(String),
    /// Something that matters is not known.
    Unknown(String),
}

impl Fit {
    /// The weightier of two findings: a mismatch over a doubt, a doubt over a fit.
    fn or(self, other: Self) -> Self {
        match (&self, &other) {
            (Self::Mismatch(_), _) => self,
            (_, Self::Mismatch(_)) => other,
            (Self::Unknown(_), _) => self,
            (_, Self::Unknown(_)) => other,
            _ => self,
        }
    }
}

/// Whether `new` can stand where the call was made. `passed` is what the call passes, one type
/// per argument, with `returns` the type its value is kept in when `use_` is `Kept`, or the old
/// function's return otherwise.
pub fn compare(
    passed: &CallShape,
    new: &CallShape,
    use_: CallUse,
    mappings: Option<&Mappings>,
) -> Fit {
    let arity = passed.args.len();
    if new.args.len() != arity {
        return Fit::Mismatch(format!(
            "the call passes {arity} argument(s), and the new function takes {}",
            new.args.len()
        ));
    }
    let mut fit = Fit::Fits;
    for position in 0..arity {
        let reads = new.inputs.get(position).copied().unwrap_or(false);
        let passes_value = passed.inputs.get(position).copied().unwrap_or(false);
        let found = match (
            passed.args[position].as_deref(),
            new.args[position].as_deref(),
        ) {
            (Some(was), Some(now)) => explain(
                relate(was, now, passes_value && reads, mappings),
                Side::Argument(position),
                was,
                now,
            ),
            _ => Fit::Unknown(format!(
                "what argument {position} has to be is not known for both functions"
            )),
        };
        fit = fit.or(found);
        if passes_value && !reads {
            fit = fit.or(Fit::Unknown(format!(
                "whether the new function only reads argument {position} is not known"
            )));
        }
    }
    let returned = match (use_, passed.returns.as_deref(), new.returns.as_deref()) {
        (CallUse::Discarded, None, Some(now)) => Fit::Unknown(format!(
            "the new function returns a {now}, and this call keeps no value for it"
        )),
        (CallUse::Discarded, _, _) => Fit::Fits,
        (_, _, None) => Fit::Unknown("what the new function returns is not known".into()),
        (_, None, Some(_)) => {
            Fit::Unknown("what this call does with the value is not known".into())
        }
        // A kept value goes into a variable that takes any subclass of what it holds; a value used
        // in place is read as exactly the old function's.
        (CallUse::Kept, Some(was), Some(now)) => {
            explain(relate(now, was, true, mappings), Side::Kept, now, was)
        }
        (CallUse::Consumed, Some(was), Some(now)) => {
            explain(relate(now, was, false, mappings), Side::Used, now, was)
        }
    };
    fit.or(returned)
}

/// Why a value of one type may not stand where another is taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Why {
    /// An object type names no class, so what it holds is open.
    NoClass,
    /// `None` is passed where it may not be taken.
    MaybeNone,
    /// Two classes the hierarchy cannot put one under the other, or a place a subclass may not go.
    Unplaced,
    /// An enum, or a name the mappings cannot place, which may be laid out like the other.
    SameBytes,
    /// Two different kinds of value.
    Differs,
}

/// Where in a call a type is compared.
#[derive(Debug, Clone, Copy)]
enum Side {
    Argument(usize),
    /// The value a `Let` keeps.
    Kept,
    /// The value used where the call stands, without being kept.
    Used,
}

/// A finding about `from` standing where `to` is taken, in the words for where it stands.
fn explain(found: Result<(), Why>, side: Side, from: &str, to: &str) -> Fit {
    let Err(why) = found else {
        return Fit::Fits;
    };
    let text = match (side, why) {
        (Side::Argument(at), Why::NoClass) => {
            format!("argument {at} is a {from} whose class is not known")
        }
        (Side::Argument(at), Why::MaybeNone) => {
            format!(
                "argument {at} is None, and whether the new function takes None there is not known"
            )
        }
        (Side::Argument(at), Why::Unplaced) => format!(
            "argument {at} is a {from}, and whether the new function takes it as a {to} is not known"
        ),
        (Side::Argument(at), Why::SameBytes) => format!(
            "argument {at} is a {from}, and the new function takes a {to}, which may hold the same bytes"
        ),
        (Side::Argument(at), Why::Differs) => {
            format!("argument {at} is a {from}, and the new function takes a {to} there")
        }
        (_, Why::NoClass) => format!("the new function returns a {from} whose class is not known"),
        (Side::Kept, Why::Unplaced | Why::MaybeNone) => format!(
            "the new function returns a {from}, and whether that fits the {to} the call keeps it in is not known"
        ),
        (_, Why::Unplaced | Why::MaybeNone) => format!(
            "the new function returns a {from}, where the old one returned the {to} the code after it reads"
        ),
        (_, Why::SameBytes) => format!(
            "the new function returns a {from} where a {to} is taken, which may hold the same bytes"
        ),
        (Side::Kept, Why::Differs) => {
            format!("the call keeps a {to}, and the new function returns a {from}")
        }
        (_, Why::Differs) => {
            format!("the code after the call reads a {to}, and the new function returns a {from}")
        }
    };
    match why {
        Why::Differs => Fit::Mismatch(text),
        _ => Fit::Unknown(text),
    }
}

/// Whether a value of type `from` fits where `to` is taken. `subclass` says whether a value of a
/// derived class may stand in, which holds where the value is only read or is stored into a
/// variable.
fn relate(from: &str, to: &str, subclass: bool, mappings: Option<&Mappings>) -> Result<(), Why> {
    if from == to {
        return if names_no_class(from) {
            Err(Why::NoClass)
        } else {
            Ok(())
        };
    }
    if from == NONE && object_like(to) {
        return if subclass {
            Ok(())
        } else {
            Err(Why::MaybeNone)
        };
    }
    if without_classes(from) == without_classes(to) {
        if let (Some((from_kind, Some(a))), Some((to_kind, Some(b)))) =
            (top_class(from), top_class(to))
            && from_kind == to_kind
            && subclass
            && mappings.is_some_and(|m| m.inherits_from(a, b))
        {
            return Ok(());
        }
        return Err(Why::Unplaced);
    }
    let head = |text: &str| {
        without_classes(text)
            .split('<')
            .next()
            .unwrap_or_default()
            .to_string()
    };
    // A class is an object held the same way, and the mappings record a class reference as an
    // object one naming no class: a class may be what such a reference holds.
    let any_object = |text: &str, word: &str| matches!(top_class(text), Some((kind, class)) if kind == word && class.is_none_or(|class| class == "Object"));
    for (class, object) in [("Class", "Object"), ("SoftClass", "SoftObject")] {
        if (head(from) == class && any_object(to, object))
            || (head(to) == class && any_object(from, object))
        {
            return Err(Why::Unplaced);
        }
    }
    // An enum is laid out as the integer under it, and a name the mappings cannot place may be
    // one, so neither is known to differ from what it meets.
    let plain = |word: &str| {
        PRIMITIVES.contains(&word)
            || OBJECTS.contains(&word)
            || mappings.is_some_and(|m| m.has_struct(word) && !m.has_enum(word))
    };
    if !plain(&head(from)) || !plain(&head(to)) {
        return Err(Why::SameBytes);
    }
    Err(Why::Differs)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    /// A shape from a signature or from a call itself, every argument read.
    fn exact(args: &[&str], returns: Option<&str>) -> CallShape {
        open(
            &args.iter().map(|arg| Some(*arg)).collect::<Vec<_>>(),
            returns,
        )
    }

    /// The same with arguments nothing said anything about.
    fn open(args: &[Option<&str>], returns: Option<&str>) -> CallShape {
        CallShape {
            args: args.iter().map(|arg| arg.map(str::to_string)).collect(),
            inputs: vec![true; args.len()],
            returns: returns.map(str::to_string),
        }
    }

    fn hierarchy() -> Mappings {
        let class = |name: &str, parent: Option<&str>| usmap::Struct {
            name: name.into(),
            super_struct: parent.map(str::to_string),
            properties: vec![],
        };
        Mappings::from_structs_and_enums(
            vec![
                class("Object", None),
                class("Actor", Some("Object")),
                class("Pawn", Some("Actor")),
                class("Character", Some("Pawn")),
                class("Light", Some("Actor")),
                class("Vector", None),
            ],
            vec![usmap::Enum {
                name: "EMode".into(),
                entries: Default::default(),
            }],
        )
    }

    #[test]
    fn a_new_function_fits_only_where_both_shapes_say_the_same() {
        let m = hierarchy();
        let call = exact(&["Int", "Float"], Some("Bool"));
        let fit = |new: &CallShape| compare(&call, new, CallUse::Kept, Some(&m));
        assert_eq!(fit(&exact(&["Int", "Float"], Some("Bool"))), Fit::Fits);
        assert!(matches!(
            fit(&exact(&["Int", "Str"], Some("Bool"))),
            Fit::Mismatch(_)
        ));
        assert!(matches!(
            fit(&exact(&["Int"], Some("Bool"))),
            Fit::Mismatch(_)
        ));
        assert!(matches!(
            fit(&open(&[Some("Int"), None], Some("Bool"))),
            Fit::Unknown(_)
        ));
        assert!(matches!(
            fit(&exact(&["Int", "Float"], Some("Int"))),
            Fit::Mismatch(_)
        ));
        let discards = exact(&["Int", "Float"], None);
        assert!(matches!(
            compare(
                &discards,
                &exact(&["Int", "Float"], Some("Bool")),
                CallUse::Discarded,
                Some(&m)
            ),
            Fit::Unknown(_)
        ));
    }

    /// A subclass stands in for its ancestor where the value is only read, and nowhere it could be
    /// written back; a class the call leaves open, or one inside a container, decides nothing.
    #[test]
    fn a_class_fits_where_it_derives_and_is_only_read() {
        let m = hierarchy();
        let fits = |from: &str, to: &str| {
            compare(
                &exact(&[from], None),
                &exact(&[to], None),
                CallUse::Discarded,
                Some(&m),
            )
        };
        assert_eq!(fits("Object<Character>", "Object<Actor>"), Fit::Fits);
        assert!(matches!(
            fits("Object<Actor>", "Object<Character>"),
            Fit::Unknown(_)
        ));
        assert!(matches!(fits("Object", "Object"), Fit::Unknown(_)));
        assert!(matches!(fits("Object", "Object<Actor>"), Fit::Unknown(_)));
        assert!(matches!(
            fits("Array<Object<Character>>", "Array<Object<Actor>>"),
            Fit::Unknown(_)
        ));
        assert!(matches!(
            fits("Class<Pawn>", "Object<Actor>"),
            Fit::Mismatch(_)
        ));
        assert_eq!(fits(NONE, "Object<Actor>"), Fit::Fits);
        assert_eq!(without_classes("Array<Object<Actor>>"), "Array<Object>");
        assert_eq!(
            without_classes("Map<Name, Class<Actor>>"),
            "Map<Name, Class>"
        );
        let mut written = exact(&["Object<Actor>"], None);
        written.inputs = vec![false];
        assert!(matches!(
            compare(
                &exact(&["Object<Character>"], None),
                &written,
                CallUse::Discarded,
                Some(&m)
            ),
            Fit::Unknown(_)
        ));
    }

    /// An enum and a byte are the same width, and a name the mappings cannot place may be either.
    #[test]
    fn an_enum_or_an_unknown_name_is_never_a_mismatch() {
        let m = hierarchy();
        let fits = |from: &str, to: &str| {
            compare(
                &exact(&[from], None),
                &exact(&[to], None),
                CallUse::Discarded,
                Some(&m),
            )
        };
        assert!(matches!(fits("EMode", "Byte"), Fit::Unknown(_)));
        assert!(matches!(fits("Int", "S_Unheard"), Fit::Unknown(_)));
        assert!(matches!(fits("Vector", "Int"), Fit::Mismatch(_)));
    }

    /// A kept value may come back as a subclass of the variable keeping it, and not the other way.
    #[test]
    fn a_returned_value_fits_a_variable_of_its_class_or_an_ancestor() {
        let m = hierarchy();
        let keeps = exact(&[], Some("Object<Actor>"));
        let returns = |class: &str| exact(&[], Some(class));
        assert_eq!(
            compare(&keeps, &returns("Object<Pawn>"), CallUse::Kept, Some(&m)),
            Fit::Fits
        );
        assert!(matches!(
            compare(
                &returns("Object<Pawn>"),
                &returns("Object<Actor>"),
                CallUse::Kept,
                Some(&m)
            ),
            Fit::Unknown(_)
        ));
        assert!(matches!(
            compare(
                &keeps,
                &returns("Object<Pawn>"),
                CallUse::Consumed,
                Some(&m)
            ),
            Fit::Unknown(_)
        ));
    }

    /// A call reports what it passes, which arguments it only reads, and the variable it keeps
    /// its value in.
    #[test]
    fn a_call_tells_what_it_passes_and_keeps() {
        let parsed = ParsedPackage::of_exports(Vec::new());
        let mut locals = HashMap::new();
        locals.insert("Offset", "Vector");
        locals.insert("CallFunc_BreakVector_X", "Double");
        locals.insert("Hit", "Bool");
        let types = Types {
            locals,
            own_class: Some("Actor".into()),
            parsed: &parsed,
            mappings: None,
        };
        let local = |name: &str| Expr::Variable {
            name: "LocalVariable",
            property: PropertyRef {
                names: Vec::new(),
                path: name.into(),
                owner: kismet::ObjectRef {
                    index: 0,
                    path: None,
                },
            },
        };
        let call = Expr::FinalCall {
            name: "CallMath",
            function: kismet::ObjectRef {
                index: -1,
                path: Some("/Script/Engine.KismetMathLibrary:BreakVector".into()),
            },
            params: vec![
                local("Offset"),
                local("CallFunc_BreakVector_X"),
                Expr::SelfRef,
            ],
        };
        let site = site_of(&call, Some(&local("Hit")), &types).expect("a call");
        assert_eq!(
            site.function,
            "/Script/Engine.KismetMathLibrary:BreakVector"
        );
        assert_eq!(
            site.args,
            vec![
                Some("Vector".to_string()),
                Some("Double".to_string()),
                Some("Object<Actor>".to_string())
            ]
        );
        assert_eq!(site.inputs, vec![true, false, true]);
        assert_eq!(site.returns.as_deref(), Some("Bool"));
    }

    #[test]
    fn a_result_temporary_of_the_function_itself_is_an_output() {
        let local = |name: &str| Expr::Variable {
            name: "LocalVariable",
            property: PropertyRef {
                names: Vec::new(),
                path: name.into(),
                owner: kismet::ObjectRef {
                    index: 0,
                    path: None,
                },
            },
        };
        assert!(!is_input(&local("CallFunc_BreakVector_X"), "BreakVector"));
        assert!(is_input(
            &local("CallFunc_BreakVector_ReturnValue"),
            "BreakVector"
        ));
        assert!(is_input(
            &local("CallFunc_GetActorLocation_ReturnValue"),
            "BreakVector"
        ));
        assert!(is_input(&local("Speed"), "BreakVector"));
        assert!(is_input(&Expr::SelfRef, "BreakVector"));
    }

    /// A member passed as an argument takes the type its class or struct declares: the package's
    /// own class first, the mappings for anything else, through a context as well.
    #[test]
    fn a_member_passed_takes_the_type_its_owner_declares() {
        use crate::kismet::ObjectRef;
        use usmap::{Property, PropertyInner, Struct};

        let field = |name: &str, inner: PropertyInner| Property {
            name: name.into(),
            array_dim: 1,
            index: 0,
            inner,
        };
        let class = ParsedExport {
            object_name: "BP_Thing_C".into(),
            struct_definition: Some(Struct {
                name: "BP_Thing_C".into(),
                super_struct: None,
                properties: vec![
                    field("Count", PropertyInner::Int),
                    field("Owner", PropertyInner::WeakObject),
                ],
            }),
            ..ParsedExport::blank(0)
        };
        let parsed = ParsedPackage::of_exports(vec![class]);
        let mappings = Mappings::from_structs(vec![Struct {
            name: "Vector".into(),
            super_struct: None,
            properties: vec![field("X", PropertyInner::Double)],
        }]);
        let types = Types {
            locals: HashMap::new(),
            own_class: None,
            parsed: &parsed,
            mappings: Some(&mappings),
        };
        let member = |name: &'static str, owner: &str, field: &str| Expr::Variable {
            name,
            property: PropertyRef {
                names: Vec::new(),
                path: field.into(),
                owner: ObjectRef {
                    index: 0,
                    path: Some(owner.into()),
                },
            },
        };
        let own = member("InstanceVariable", "/Game/BP_Thing.BP_Thing_C", "Count");
        assert_eq!(arg_type(&own, &types).as_deref(), Some("Int"));
        let through = Expr::Context {
            name: "Context",
            object: Box::new(Expr::SelfRef),
            skip: 0,
            property: PropertyRef {
                names: Vec::new(),
                path: String::new(),
                owner: ObjectRef {
                    index: 0,
                    path: None,
                },
            },
            member: Box::new(own),
        };
        assert_eq!(arg_type(&through, &types).as_deref(), Some("Int"));
        let Expr::Variable { property, .. } =
            member("InstanceVariable", "/Script/CoreUObject.Vector", "X")
        else {
            unreachable!()
        };
        let x = Expr::Member {
            name: "StructMemberContext",
            property,
            value: Box::new(Expr::SelfRef),
        };
        assert_eq!(arg_type(&x, &types).as_deref(), Some("Double"));
        let weak = member("InstanceVariable", "/Game/BP_Thing.BP_Thing_C", "Owner");
        assert_eq!(arg_type(&weak, &types).as_deref(), Some("Object"));
        let missing = member("InstanceVariable", "/Script/Engine.Actor", "Nope");
        assert_eq!(arg_type(&missing, &types), None);
        assert_eq!(arg_type(&Expr::SelfRef, &types).as_deref(), Some("Object"));
        assert_eq!(
            arg_type(&Expr::Simple { name: "NoObject" }, &types).as_deref(),
            Some(NONE)
        );
    }
}
