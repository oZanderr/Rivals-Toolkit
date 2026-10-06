//! A new function on a Blueprint class, written as the cook writes one: an export of its own under
//! the class, its parameters as field records, a script that returns at once, and a place in the
//! class's children and function map, which is where a call by name finds it. A text then writes
//! its script, in a second stage the caller makes.

use std::collections::BTreeMap;

use retoc::legacy_asset::{FLegacyPackageHeader, FObjectExport};
use retoc::zen::FPackageIndex;
use serde::{Deserialize, Serialize};

use crate::edit::AppliedEdit;
use crate::field_record::{
    FieldType, NewField, encode_field_record, new_record, parse_field_type, record_indices,
};
use crate::header_edit::{Tables, add_import};
use crate::package::{ParsedExport, ParsedPackage};
use crate::ustruct::{FieldRole, StructLayout};
use crate::write::Splice;

/// `RF_Public`, the only flag the cook gives a function's export.
const RF_PUBLIC: u32 = 0x1;
/// `FUNC_BlueprintEvent | FUNC_BlueprintCallable | FUNC_Public`, what the cook gives a Blueprint
/// function, with `FUNC_HasOutParms` for one that gives anything back.
const FUNCTION_FLAGS: u32 = 0x0C02_0000;
const HAS_OUT_PARMS: u32 = 0x0040_0000;
/// A script that returns at once: `Return Nothing`, `EndOfScript`.
const EMPTY_SCRIPT: [u8; 3] = [0x04, 0x0B, 0x53];

/// A function to add to a Blueprint class, and the text its script is written from.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NewFunctionEdit {
    /// The class's export; the package's only Blueprint class when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<u32>,
    pub name: String,
    /// What it takes and gives back, as a signature prints: `(A: Int, ref B: Array<Int>) -> Hit: Bool`,
    /// or `-> (Hit: Bool, Count: Int)` for several outputs.
    pub signature: String,
    /// The script, as assembler text.
    pub text: String,
}

/// One parameter of a new function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Param {
    pub name: String,
    pub ty: FieldType,
    pub role: NewField,
}

/// Reads a signature: its inputs in parentheses, `ref` before one passed by reference, then its
/// outputs after `->`, one alone or several in parentheses.
pub(crate) fn parse_signature(text: &str) -> Result<Vec<Param>, String> {
    let text = text.trim();
    let (inputs, outputs) = match text.split_once("->") {
        Some((inputs, outputs)) => (inputs.trim(), Some(outputs.trim())),
        None => (text, None),
    };
    let inside = inputs
        .strip_prefix('(')
        .and_then(|rest| rest.strip_suffix(')'))
        .ok_or("a signature starts with its inputs in parentheses, as (A: Int, ref B: Array<Int>) -> Hit: Bool")?;
    let mut params = Vec::new();
    for part in split_top(inside) {
        let (role, part) = match part.strip_prefix("ref ") {
            Some(rest) => (NewField::ByReference, rest.trim()),
            None => (NewField::Input, part),
        };
        let (name, ty) = field(part)?;
        params.push(Param { name, ty, role });
    }
    if let Some(outputs) = outputs {
        let list = outputs
            .strip_prefix('(')
            .and_then(|rest| rest.strip_suffix(')'))
            .unwrap_or(outputs);
        let parts = split_top(list);
        if parts.is_empty() {
            return Err("nothing follows -> in the signature".into());
        }
        for part in parts {
            let (name, ty) = field(part)?;
            params.push(Param {
                name,
                ty,
                role: NewField::Output,
            });
        }
    }
    for (at, param) in params.iter().enumerate() {
        if params[..at].iter().any(|other| other.name == param.name) {
            return Err(format!("the signature names {} twice", param.name));
        }
    }
    Ok(params)
}

/// The comma-separated parts of a list, a comma inside a type's brackets not counting.
fn split_top(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (at, c) in text.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(text[start..at].trim());
                start = at + 1;
            }
            _ => {}
        }
    }
    parts.push(text[start..].trim());
    parts.retain(|part| !part.is_empty());
    parts
}

fn field(part: &str) -> Result<(String, FieldType), String> {
    let (name, ty) = part
        .split_once(':')
        .ok_or_else(|| format!("{part:?} is not a parameter: write it as Name: Type"))?;
    let name = name.trim();
    if !is_ident(name) {
        return Err(format!("{name:?} is not a name a parameter can take"));
    }
    Ok((name.to_string(), parse_field_type(ty)?))
}

fn is_ident(text: &str) -> bool {
    let mut chars = text.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The class something new goes to: the one named, or the package's only Blueprint class.
pub(crate) fn class_of(
    parsed: &ParsedPackage,
    class: Option<u32>,
) -> Result<(&ParsedExport, &StructLayout), String> {
    let classes: Vec<&ParsedExport> = parsed
        .exports
        .iter()
        .filter(|export| {
            export
                .layout
                .as_ref()
                .is_some_and(|layout| layout.function_map_at.is_some())
        })
        .collect();
    let class = match class {
        Some(index) => parsed
            .exports
            .get(index as usize)
            .filter(|export| classes.iter().any(|class| class.index == export.index))
            .ok_or_else(|| {
                format!("export {index} is not a Blueprint class whose layout was read whole")
            })?,
        None => match classes.as_slice() {
            [only] => only,
            [] => return Err("this package holds no Blueprint class to add a function to".into()),
            several => {
                return Err(format!(
                    "this package holds {} Blueprint classes; say which, as one of {}",
                    several.len(),
                    several
                        .iter()
                        .map(|class| format!("{} ({})", class.index, class.object_name))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        },
    };
    let layout = class
        .layout
        .as_ref()
        .ok_or("the class's layout was not read")?;
    Ok((class, layout))
}

/// What adding functions writes: the grown tables, the new rows and their bytes, and the splices
/// that list them in their classes.
pub(crate) struct Addition {
    pub tables: Tables,
    pub exports: Vec<FObjectExport>,
    pub preload_dependencies: Vec<FPackageIndex>,
    pub appended: Vec<u8>,
    pub splices: Vec<Splice>,
    pub applied: Vec<AppliedEdit>,
}

/// Each function as an export of its own at the end of the table, with what its class needs to
/// list it. Their scripts return at once; a text writes each one's afterwards.
pub(crate) fn add_functions(
    parsed: &ParsedPackage,
    header: &FLegacyPackageHeader,
    adds: &[NewFunctionEdit],
) -> Result<Addition, String> {
    let total = i64::from(header.summary.versioning_info.total_header_size);
    let data_end = header
        .exports
        .iter()
        .map(|export| export.serial_offset + export.serial_size)
        .max()
        .unwrap_or(total);
    let mut tables = Tables {
        names: header.name_map.clone(),
        imports: header.imports.clone(),
    };
    let mut exports = header.exports.clone();
    let mut appended = Vec::new();
    let mut applied = Vec::new();
    // What each class gains, by its position: the functions' indices and names, in order.
    let mut listed: BTreeMap<usize, Vec<(i32, String)>> = BTreeMap::new();
    // Each new function's runs, as the table orders them: serialize before serialize, create
    // before serialize, serialize before create, create before create.
    let mut runs: BTreeMap<usize, [Vec<FPackageIndex>; 4]> = BTreeMap::new();
    for add in adds {
        let name = add.name.trim();
        if !is_ident(name) {
            return Err(format!("{name:?} is not a name a function can take"));
        }
        let (class, layout) = class_of(parsed, add.class)?;
        let position = class.index as usize;
        let taken = layout
            .function_map
            .iter()
            .map(|(existing, _)| existing.as_str())
            .chain(
                parsed
                    .exports
                    .iter()
                    .filter(|export| export.outer_index == class.index as i32 + 1)
                    .map(|export| export.object_name.as_str()),
            )
            .chain(
                listed
                    .get(&position)
                    .into_iter()
                    .flatten()
                    .map(|(_, name)| name.as_str()),
            )
            .any(|existing| existing.eq_ignore_ascii_case(name));
        if taken {
            return Err(format!(
                "{} already has something named {name}",
                class.object_name
            ));
        }
        let params = parse_signature(&add.signature)
            .map_err(|reason| format!("{name}'s signature: {reason}"))?;
        let mut records = Vec::new();
        let mut kept = Vec::new();
        for param in &params {
            let record = new_record(&param.name, &param.ty, param.role, &mut tables)
                .map_err(|reason| format!("{name}: {reason}"))?;
            let mut indices = Vec::new();
            record_indices(&record, &mut indices);
            kept.extend(
                indices
                    .into_iter()
                    .filter(|index| tables.zen_keeps_import(*index)),
            );
            records.extend(encode_field_record(&record, &mut tables.names));
        }
        let outputs = params.iter().any(|param| param.role == NewField::Output);
        // A function stores none of its own values: an unversioned header with nothing in it, or
        // a tagged list ending at once.
        let mut bytes = if parsed.info.unversioned_properties {
            crate::unversioned::empty_header(0)
        } else {
            let none = tables.names.store("None");
            let mut bytes = none.index.to_le_bytes().to_vec();
            bytes.extend_from_slice(&none.number.to_le_bytes());
            bytes
        };
        // No object guid, no parent, no children.
        bytes.extend_from_slice(&[0; 12]);
        bytes.extend_from_slice(&(params.len() as i32).to_le_bytes());
        bytes.extend_from_slice(&records);
        let script = EMPTY_SCRIPT.len() as i32;
        bytes.extend_from_slice(&script.to_le_bytes());
        bytes.extend_from_slice(&script.to_le_bytes());
        bytes.extend_from_slice(&EMPTY_SCRIPT);
        let flags = FUNCTION_FLAGS | if outputs { HAS_OUT_PARMS } else { 0 };
        bytes.extend_from_slice(&flags.to_le_bytes());
        // No event graph to enter directly.
        bytes.extend_from_slice(&[0; 8]);
        let function_class = add_import(
            &mut tables,
            "/Script/CoreUObject.Function",
            Some(("/Script/CoreUObject".into(), "Class".into())),
        )?;
        let template = add_import(
            &mut tables,
            "/Script/CoreUObject.Default__Function",
            Some(("/Script/CoreUObject".into(), "Function".into())),
        )?;
        let at = exports.len();
        let index = FPackageIndex::create_export(at as u32);
        let outer = FPackageIndex::create_export(class.index);
        exports.push(FObjectExport {
            class_index: FPackageIndex {
                index: function_class,
            },
            super_index: FPackageIndex::create_null(),
            template_index: FPackageIndex { index: template },
            outer_index: outer,
            object_name: tables.names.store(name),
            object_flags: RF_PUBLIC,
            serial_size: bytes.len() as i64,
            serial_offset: data_end + appended.len() as i64,
            first_export_dependency_index: -1,
            ..Default::default()
        });
        let index_of = |index: i32| FPackageIndex { index };
        runs.insert(
            at,
            [
                Vec::new(),
                kept.into_iter().map(index_of).collect(),
                vec![index_of(function_class), index_of(template)],
                vec![outer],
            ],
        );
        listed
            .entry(position)
            .or_default()
            .push((index.index, name.to_string()));
        applied.push(AppliedEdit {
            name: format!("{} function {name}", class.object_name),
            offset: data_end as u64,
            offset_after: data_end as u64,
            element: None,
            elements_after: None,
            before: "(none)".into(),
            after: format!("{name}{} as export {at}", signature_text(&params)),
        });
        appended.extend_from_slice(&bytes);
    }
    let mut splices = Vec::new();
    for (position, functions) in &listed {
        let layout = parsed.exports[*position]
            .layout
            .as_ref()
            .ok_or("the class's layout was not read")?;
        let map_at = layout
            .function_map_at
            .ok_or("the class's function map was not read")?;
        let children = (layout.children.len() + functions.len()) as i32;
        splices.push(count(layout.children_at, children));
        let mut indices = Vec::new();
        for (index, _) in functions {
            indices.extend_from_slice(&index.to_le_bytes());
        }
        splices.push(insert(
            layout.children_at + 4 + 4 * layout.children.len() as u64,
            indices,
        ));
        let mapped = (layout.function_map.len() + functions.len()) as i32;
        splices.push(count(map_at, mapped));
        let mut entries = Vec::new();
        for (index, name) in functions {
            let id = tables.names.store(name);
            entries.extend_from_slice(&id.index.to_le_bytes());
            entries.extend_from_slice(&id.number.to_le_bytes());
            entries.extend_from_slice(&index.to_le_bytes());
        }
        splices.push(insert(
            map_at + 4 + 12 * layout.function_map.len() as u64,
            entries,
        ));
    }
    // The class has its new functions made before it is read, as it has every other.
    let preload_dependencies =
        crate::renumber::rebuild_runs(header, &mut exports, |position, mut held| {
            if let Some(new) = runs.remove(&position) {
                return Ok(new);
            }
            if let Some(functions) = listed.get(&position) {
                held[1].extend(
                    functions
                        .iter()
                        .map(|(index, _)| FPackageIndex { index: *index }),
                );
            }
            Ok(held)
        })?;
    splices.sort_by_key(|splice| (splice.start, splice.end));
    Ok(Addition {
        tables,
        exports,
        preload_dependencies,
        appended,
        splices,
        applied,
    })
}

fn count(at: u64, value: i32) -> Splice {
    Splice {
        start: at,
        end: at + 4,
        bytes: value.to_le_bytes().to_vec(),
    }
}

fn insert(at: u64, bytes: Vec<u8>) -> Splice {
    Splice {
        start: at,
        end: at,
        bytes,
    }
}

fn signature_text(params: &[Param]) -> String {
    let inputs: Vec<String> = params
        .iter()
        .filter(|param| param.role != NewField::Output)
        .map(|param| {
            let by_ref = if param.role == NewField::ByReference {
                "ref "
            } else {
                ""
            };
            format!("{by_ref}{}: {}", param.name, param.ty.printed())
        })
        .collect();
    let outputs: Vec<String> = params
        .iter()
        .filter(|param| param.role == NewField::Output)
        .map(|param| format!("{}: {}", param.name, param.ty.printed()))
        .collect();
    let head = format!("({})", inputs.join(", "));
    match outputs.len() {
        0 => head,
        1 => format!("{head} -> {}", outputs[0]),
        _ => format!("{head} -> ({})", outputs.join(", ")),
    }
}

/// The export a new function was given, found in the package as saved by its class and name.
pub fn new_function_export(parsed: &ParsedPackage, add: &NewFunctionEdit) -> Result<u32, String> {
    let (class, _) = class_of(parsed, add.class)?;
    parsed
        .exports
        .iter()
        .find(|export| {
            export.outer_index == class.index as i32 + 1
                && export.class_name == "Function"
                && export.object_name == add.name.trim()
        })
        .map(|export| export.index)
        .ok_or_else(|| {
            format!(
                "{} has no function {} after saving",
                class.object_name, add.name
            )
        })
}

/// Holds each new function to what was asked: an export under its class reading as a function,
/// its parameters in order with their types, a script that returns at once, and a place in the
/// class's children and function map.
pub(crate) fn verify(
    before: &ParsedPackage,
    after: &ParsedPackage,
    adds: &[NewFunctionEdit],
) -> Result<(), String> {
    if after.exports.len() != before.exports.len() + adds.len() {
        return Err(format!(
            "the package holds {} exports after adding {} function(s) to {}",
            after.exports.len(),
            adds.len(),
            before.exports.len()
        ));
    }
    for add in adds {
        let index = new_function_export(after, add)?;
        let function = &after.exports[index as usize];
        let name = &function.object_name;
        let signature = function
            .signature
            .as_ref()
            .ok_or_else(|| format!("{name} does not read as a function after saving"))?;
        let wanted = parse_signature(&add.signature)?;
        let read: Vec<(String, String, FieldRole)> = signature
            .params
            .iter()
            .map(|param| (param.name.clone(), param.kind.clone(), param.role))
            .collect();
        let asked: Vec<(String, String, FieldRole)> = wanted
            .iter()
            .map(|param| {
                let role = match param.role {
                    NewField::ByReference => FieldRole::Ref,
                    NewField::Output => FieldRole::Out,
                    NewField::Return => FieldRole::Return,
                    NewField::Input | NewField::Local | NewField::Variable => FieldRole::In,
                };
                (param.name.clone(), param.ty.printed(), role)
            })
            .collect();
        if read != asked {
            return Err(format!(
                "{name} reads back taking {read:?} rather than {asked:?}"
            ));
        }
        let (class, layout) = class_of(after, add.class)?;
        let package_index = index as i32 + 1;
        if !layout.children.contains(&package_index)
            || !layout
                .function_map
                .iter()
                .any(|(listed, at)| listed == name && *at == package_index)
        {
            return Err(format!(
                "{} does not list {name} among its functions after saving",
                class.object_name
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    use crate::edit::{PackageEdits, ScriptTextEdit, Sidecars, patch_package_with, verify_patch};
    use crate::package::{AssetBundle, parse_package};
    use crate::script_text::print_script;

    /// The event graph, as the fixture lays it out after its class.
    const GRAPH: u32 = 1;

    /// Saves `edits` over `asset` and `exports`, reads the result back and verifies it.
    fn save(
        asset: &[u8],
        exports: &[u8],
        edits: &PackageEdits,
    ) -> (Vec<u8>, Vec<u8>, ParsedPackage) {
        let bundle = AssetBundle { asset, exports };
        let before = parse_package(&bundle, None).expect("reads");
        let patched =
            patch_package_with(&bundle, Sidecars::default(), &before, edits, None).expect("saved");
        let after = parse_package(
            &AssetBundle {
                asset: &patched.asset,
                exports: &patched.exports,
            },
            None,
        )
        .expect("reads back");
        verify_patch(&before, &after, edits, &patched.applied).expect("verified");
        (patched.asset, patched.exports, after)
    }

    fn text_of(parsed: &ParsedPackage, export: u32) -> String {
        print_script(parsed, export).expect("a script").text()
    }

    /// A new function is an export of the class, listed among its functions, returning at once
    /// until a text writes it; the event graph can then call it by name.
    #[test]
    fn a_new_function_is_listed_in_its_class_and_takes_its_text() {
        let built = crate::script_fixture::event_graph().build();
        let add = NewFunctionEdit {
            class: None,
            name: "Glow".into(),
            signature: "(Strength: Float) -> Glowed: Bool".into(),
            text: String::new(),
        };
        let (asset, exports, added) = save(
            &built.asset,
            &built.exports,
            &PackageEdits {
                new_functions: vec![add.clone()],
                ..Default::default()
            },
        );
        let index = new_function_export(&added, &add).expect("the function");
        assert_eq!(
            text_of(&added, index).trim(),
            "Return Nothing
EndOfScript"
        );
        let flags = added.exports[index as usize]
            .signature
            .as_ref()
            .expect("its fields")
            .flags;
        assert_eq!(flags, FUNCTION_FLAGS | HAS_OUT_PARMS);
        let graph = text_of(&added, GRAPH).replacen(
            "Jump LocalVariable(EntryPoint)
",
            "Jump LocalVariable(EntryPoint)
LocalVirtualFunction Glow(1.5f)
",
            1,
        );
        let (_, _, written) = save(
            &asset,
            &exports,
            &PackageEdits {
                script_texts: vec![
                    ScriptTextEdit {
                        export: index,
                        text: "LetBool LocalOutVariable(Glowed) = True
Return Nothing
EndOfScript"
                            .into(),
                        was: None,
                    },
                    ScriptTextEdit {
                        export: GRAPH,
                        text: graph,
                        was: None,
                    },
                ],
                ..Default::default()
            },
        );
        assert!(
            text_of(&written, index).contains("LetBool LocalOutVariable(Glowed) = True"),
            "{}",
            text_of(&written, index)
        );
        assert!(text_of(&written, GRAPH).contains("LocalVirtualFunction Glow(1.5f)"));
    }

    #[test]
    fn a_function_named_like_one_the_class_has_is_refused() {
        let built = crate::script_fixture::event_graph().build();
        let before = built.parsed();
        let header = built.header();
        let taken = NewFunctionEdit {
            name: "receivebeginplay".into(),
            signature: "()".into(),
            ..Default::default()
        };
        let refused = add_functions(&before, &header, &[taken])
            .err()
            .expect("refused");
        assert!(refused.contains("already has something named"), "{refused}");
    }

    #[test]
    fn a_signature_reads_its_inputs_references_and_outputs() {
        let params =
            parse_signature("(Strength: Float, ref Seen: Map<Name, Int>) -> Glowed: Bool").unwrap();
        let read: Vec<(&str, NewField)> = params
            .iter()
            .map(|param| (param.name.as_str(), param.role))
            .collect();
        assert_eq!(
            read,
            [
                ("Strength", NewField::Input),
                ("Seen", NewField::ByReference),
                ("Glowed", NewField::Output),
            ]
        );
        assert_eq!(
            params[1].ty,
            FieldType::Map(Box::new(FieldType::Name), Box::new(FieldType::Int))
        );
        let several = parse_signature("() -> (Hit: Bool, Count: Int)").unwrap();
        assert_eq!(several.len(), 2);
        assert!(parse_signature("()").unwrap().is_empty());
    }

    #[test]
    fn a_signature_without_parentheses_or_with_a_name_twice_is_refused() {
        assert!(parse_signature("A: Int").is_err());
        assert!(parse_signature("(A: Int, A: Float)").is_err());
        assert!(parse_signature("(A Int)").is_err());
        assert!(parse_signature("(A: Int) ->").is_err());
    }
}
