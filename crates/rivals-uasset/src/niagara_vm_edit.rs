//! Changes a constant a Niagara script's VM program reads, by pointing one operand at a literal holding the new value.
//!
//! Literals are deduplicated when the script is compiled, so one can feed several instructions,
//! and the compiler may have folded several inputs into it. Rewriting a literal changes every
//! reader at once; an edit does that only where the operand it names is the literal's one reader.
//! Otherwise the operand is pointed at a literal already holding the value, or at one appended to
//! the end of `ScriptLiterals`. The literals are the last of the constant tables the run time lays
//! end to end, so appending one moves nothing else, and the VM reads them from the cooked array on
//! every run.

use serde::{Deserialize, Serialize};

use crate::Mappings;
use crate::niagara_vm::{
    VmLoc, VmRole, VmScript, VmTable, VmType, decode_vm, read_vm_script, show_bits,
};
use crate::package::ParsedPackage;
use crate::path_edit::{PathEdit, PathOp, export_of};
use crate::value::PropertyValue;

/// One constant operand of a Niagara script's VM program given a new value.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmConstEdit {
    /// The `NiagaraScript` export: its path below the package, its full path, its bare name when
    /// no other object shares it, or its index.
    pub export: String,
    /// The instruction, by the offset `asset vm` prints at the start of its line.
    pub at: u32,
    /// Which of its operands, counted from 0 in the order they are stored, as `asset vm --trace`
    /// lists them after `#`.
    pub operand: u32,
    /// The value: a number for a float operand (`0.2`), an integer for an int one, or explicitly
    /// typed where the operand takes either (`float:1`, `int:-1`, `0x3F800000`, `true`).
    pub value: String,
    /// What the operand read when the edit was written, as `asset vm` keys it (`lit+48=0x3F400000`,
    /// `ext+12`). One that reads otherwise has changed since, and the edit is refused unless drift
    /// is allowed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub was: Option<String>,
    /// Point the operand at a literal of its own even where it is the only reader of the one it
    /// reads, leaving that literal as it is.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub retarget: bool,
}

/// What a script's program and literals are to read as once a save's edits on it have landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmEditPlan {
    pub export: u32,
    pub bytecode: Vec<u8>,
    pub literals: Vec<u8>,
}

/// The path edits that make a save's VM constant edits, what each script is to read as after, and
/// what was done for each.
#[derive(Debug, Clone, Default)]
pub struct LoweredVm {
    pub paths: Vec<PathEdit>,
    pub plans: Vec<VmEditPlan>,
    pub notes: Vec<String>,
}

/// The four bytes `text` stands for in an operand that reads `ty`.
pub fn parse_vm_value(text: &str, ty: VmType) -> Result<u32, String> {
    let text = text.trim();
    let float = |t: &str| {
        t.parse::<f32>()
            .map(f32::to_bits)
            .map_err(|_| format!("`{t}` is not a number"))
    };
    let int = |t: &str| {
        t.parse::<i32>()
            .map(|v| v as u32)
            .map_err(|_| format!("`{t}` is not a whole number"))
    };
    if let Some(rest) = text.strip_prefix("float:") {
        return float(rest.trim());
    }
    if let Some(rest) = text.strip_prefix("int:") {
        return int(rest.trim());
    }
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        return u32::from_str_radix(hex, 16).map_err(|_| format!("`{text}` is not hexadecimal"));
    }
    match text.to_ascii_lowercase().as_str() {
        "true" => return Ok(u32::MAX),
        "false" => return Ok(0),
        _ => {}
    }
    match ty {
        VmType::Float => float(text),
        VmType::Int => int(text),
        VmType::Any => Err(format!(
            "this operand takes a float or an int; write `float:{text}` or `int:{text}`"
        )),
    }
}

fn bits_at(bytes: &[u8], at: usize) -> Option<u32> {
    bytes
        .get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Plans `edits` against `parsed` and lowers them to path edits on each script's
/// `CachedScriptVM.ScriptLiterals` and `CachedScriptVM.ByteCode.Data`.
pub fn lower_vm_consts(
    parsed: &ParsedPackage,
    edits: &[VmConstEdit],
    mappings: Option<&Mappings>,
    allow_drift: bool,
) -> Result<LoweredVm, String> {
    let mut out = LoweredVm::default();
    // Each script's edits are planned together, in order, on one copy.
    let mut order: Vec<u32> = Vec::new();
    let mut grouped: std::collections::BTreeMap<u32, Vec<&VmConstEdit>> = Default::default();
    for edit in edits {
        let export = export_of(parsed, &edit.export)?;
        if !grouped.contains_key(&export.index) {
            order.push(export.index);
        }
        grouped.entry(export.index).or_default().push(edit);
    }
    for index in order {
        let export = parsed
            .exports
            .iter()
            .find(|e| e.index == index)
            .ok_or_else(|| format!("no export {index}"))?;
        let script = read_vm_script(export, mappings);
        let label = format!("{} (export {index})", export.object_name);
        refuse_unsafe(&script, &label, export)?;
        let calls: Vec<(u16, u16)> = script
            .functions
            .iter()
            .map(|f| (f.inputs, f.outputs))
            .collect();
        let mut bytes = script.bytecode.clone();
        let mut literals = script.literals.clone();
        let mut drift = Vec::new();
        for edit in &grouped[&index] {
            let program = decode_vm(&bytes, &calls);
            let instruction = program
                .instructions
                .iter()
                .find(|i| i.at == edit.at)
                .ok_or_else(|| format!("{label}: no instruction starts at 0x{:04X}", edit.at))?;
            let operand = instruction
                .operands
                .get(edit.operand as usize)
                .ok_or_else(|| {
                    format!(
                        "{label}: {} at 0x{:04X} has {} operand(s), not #{}",
                        instruction.name,
                        edit.at,
                        instruction.operands.len(),
                        edit.operand
                    )
                })?;
            let VmLoc::Constant(offset) = operand.loc else {
                return Err(format!(
                    "{label}: operand #{} of {} at 0x{:04X} reads a register, not a constant",
                    edit.operand, instruction.name, edit.at
                ));
            };
            if !matches!(
                operand.role,
                VmRole::Source(_) | VmRole::Valid | VmRole::CallIn(_)
            ) {
                return Err(format!(
                    "{label}: operand #{} of {} at 0x{:04X} is not a value it reads",
                    edit.operand, instruction.name, edit.at
                ));
            }
            let current = VmScript {
                literals: literals.clone(),
                ..script.clone()
            }
            .resolve(offset)
            .ok_or_else(|| format!("{label}: the constant at {offset} is past every table"))?;
            let wanted = parse_vm_value(&edit.value, operand.ty)
                .map_err(|e| format!("{label} at 0x{:04X} #{}: {e}", edit.at, edit.operand))?;
            let at_text = format!("0x{:04X} #{}", edit.at, edit.operand);
            if current.table == VmTable::Literals && current.bits == Some(wanted) {
                out.notes.push(format!(
                    "{label} {at_text} reads {} already",
                    show_bits(wanted, operand.ty)
                ));
                continue;
            }
            if let Some(was) = &edit.was
                && *was != current.key
            {
                drift.push(format!("{at_text} reads {}, not {was}", current.key));
                continue;
            }
            let base = script
                .layout
                .as_ref()
                .map_or(0, |layout| layout.literal_base());
            let readers = program
                .instructions
                .iter()
                .flat_map(|i| &i.operands)
                .filter(|o| o.loc == VmLoc::Constant(offset))
                .count();
            let shown = show_bits(wanted, operand.ty);
            let target = if current.table == VmTable::Literals && readers == 1 && !edit.retarget {
                let at = current.within as usize;
                literals[at..at + 4].copy_from_slice(&wanted.to_le_bytes());
                out.notes.push(format!(
                    "{label} {at_text}: lit+{} rewritten to {shown}, its only reader",
                    current.within
                ));
                None
            } else if let Some(slot) =
                (0..literals.len() / 4).find(|slot| bits_at(&literals, slot * 4) == Some(wanted))
            {
                out.notes.push(format!(
                    "{label} {at_text}: pointed at lit+{}, which holds {shown} already",
                    slot * 4
                ));
                Some(base as usize + slot * 4)
            } else {
                let within = literals.len();
                literals.extend_from_slice(&wanted.to_le_bytes());
                out.notes.push(format!(
                    "{label} {at_text}: pointed at lit+{within}, appended to hold {shown}{}",
                    if current.table == VmTable::Literals {
                        format!(
                            ", leaving lit+{} to its other {} reader(s)",
                            current.within,
                            readers.saturating_sub(1)
                        )
                    } else {
                        format!(
                            " in place of {}",
                            current.name.as_deref().unwrap_or(&current.key)
                        )
                    }
                ));
                Some(base as usize + within)
            };
            if let Some(target) = target {
                let limit = if matches!(operand.role, VmRole::CallIn(_)) {
                    0x7FFF
                } else {
                    0xFFFF
                };
                let target = u16::try_from(target)
                    .ok()
                    .filter(|t| *t <= limit)
                    .ok_or_else(|| {
                        format!(
                            "{label} {at_text}: the literal would sit at {target}, past the {limit} an operand reaches"
                        )
                    })?;
                let pos = operand.at as usize;
                bytes[pos..pos + 2].copy_from_slice(&target.to_le_bytes());
            }
        }
        if !drift.is_empty() {
            if !allow_drift {
                return Err(format!(
                    "{}: {label} {}. Re-read the asset and make the edits again, or apply them anyway",
                    crate::edit::DRIFT,
                    drift.join("; ")
                ));
            }
            out.notes
                .push(format!("{label}: applied despite {}", drift.join("; ")));
        }
        let script_path = index.to_string();
        if literals != script.literals {
            let mut raw = (literals.len() as i32).to_le_bytes().to_vec();
            raw.extend_from_slice(&literals);
            out.paths.push(PathEdit {
                export: script_path.clone(),
                row: None,
                defaults: false,
                path: "CachedScriptVM.ScriptLiterals".to_string(),
                op: PathOp::SetRaw {
                    hex: raw
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<Vec<_>>()
                        .join(" "),
                },
                was: None,
                becomes: None,
            });
        }
        for (pos, (old, new)) in script.bytecode.iter().zip(&bytes).enumerate() {
            if old != new {
                out.paths.push(PathEdit {
                    export: script_path.clone(),
                    row: None,
                    defaults: false,
                    path: format!("CachedScriptVM.ByteCode.Data[{pos}]"),
                    op: PathOp::Set {
                        text: new.to_string(),
                    },
                    was: Some(old.to_string()),
                    becomes: None,
                });
            }
        }
        if literals != script.literals || bytes != script.bytecode {
            out.plans.push(VmEditPlan {
                export: index,
                bytecode: bytes,
                literals,
            });
        }
    }
    Ok(out)
}

/// Why no constant of `script` can be changed safely, when none can: the CPU VM never runs it,
/// it holds no bytecode or holds it compressed, it carries the experimental VM's frozen data, or
/// its bytecode does not decode.
pub fn vm_edit_refusal(script: &VmScript) -> Option<String> {
    if script.layout.is_none() {
        return Some(format!(
            "a {} script, which the CPU VM never runs",
            script.usage
        ));
    }
    if script.bytecode.is_empty() {
        return Some("it holds no bytecode".to_string());
    }
    if script.compressed {
        return Some("its bytecode is stored compressed".to_string());
    }
    if script.experimental {
        return Some(
            "it carries data for the experimental VM, which fixes where its constants are when it is compiled"
                .to_string(),
        );
    }
    if let Some(stop) = &script.program.stop {
        return Some(format!(
            "its bytecode does not decode past 0x{:04X}: {}",
            stop.at, stop.reason
        ));
    }
    if !script.literals.len().is_multiple_of(4) {
        return Some("its literals are not whole 4-byte values".to_string());
    }
    None
}

/// Refuses a script whose program an edit cannot change safely.
fn refuse_unsafe(
    script: &VmScript,
    label: &str,
    export: &crate::package::ParsedExport,
) -> Result<(), String> {
    if export.class_name != "NiagaraScript" {
        return Err(format!(
            "{label} is a {}, not a Niagara script",
            export.class_name
        ));
    }
    if let Some(reason) = vm_edit_refusal(script) {
        return Err(format!("{label}: {reason}"));
    }
    let stored = export
        .properties
        .iter()
        .find(|p| p.name == "CachedScriptVM")
        .and_then(|vm| match &vm.value {
            PropertyValue::Struct { fields, .. } => {
                fields.iter().find(|f| f.name == "ScriptLiterals")
            }
            _ => None,
        })
        .is_some_and(|literals| matches!(literals.value, PropertyValue::Array { .. }));
    if !stored {
        return Err(format!(
            "{label} stores no literals of its own to add to; store CachedScriptVM.ScriptLiterals first"
        ));
    }
    Ok(())
}

/// Checks each planned script reads, after the save, exactly as planned.
pub fn verify_vm_edits(
    after: &ParsedPackage,
    plans: &[VmEditPlan],
    mappings: Option<&Mappings>,
) -> Result<(), String> {
    for plan in plans {
        let export = after
            .exports
            .iter()
            .find(|e| e.index == plan.export)
            .ok_or_else(|| format!("export {} is gone after the save", plan.export))?;
        let script = read_vm_script(export, mappings);
        if script.bytecode != plan.bytecode {
            return Err(format!(
                "{}'s bytecode reads otherwise than planned after the save",
                export.object_name
            ));
        }
        if script.literals != plan.literals {
            return Err(format!(
                "{}'s literals read otherwise than planned after the save",
                export.object_name
            ));
        }
        if let Some(stop) = &script.program.stop {
            return Err(format!(
                "{}'s bytecode stops decoding at 0x{:04X} after the save: {}",
                export.object_name, stop.at, stop.reason
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_value_is_typed_by_the_operand_or_by_its_prefix() {
        assert_eq!(parse_vm_value("0.5", VmType::Float), Ok(0x3F00_0000));
        assert_eq!(parse_vm_value("1", VmType::Float), Ok(0x3F80_0000));
        assert_eq!(parse_vm_value("-1", VmType::Int), Ok(u32::MAX));
        assert!(parse_vm_value("1.5", VmType::Int).is_err());
        assert!(parse_vm_value("1", VmType::Any).is_err());
        assert_eq!(parse_vm_value("float:1", VmType::Any), Ok(0x3F80_0000));
        assert_eq!(parse_vm_value("int:7", VmType::Any), Ok(7));
        assert_eq!(parse_vm_value("0x3E4CCCCD", VmType::Any), Ok(0x3E4C_CCCD));
        assert_eq!(parse_vm_value("true", VmType::Int), Ok(u32::MAX));
        assert_eq!(parse_vm_value("False", VmType::Any), Ok(0));
    }
}
