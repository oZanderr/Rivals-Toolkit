//! Changes a Niagara script's VM program: a constant it reads, pointed at a literal holding the new value, or a colour it writes, transformed just before it is written.
//!
//! Literals are deduplicated when the script is compiled, so one can feed several instructions,
//! and the compiler may have folded several inputs into it. Rewriting a literal changes every
//! reader at once; an edit does that only where the operand it names is the literal's one reader.
//! Otherwise the operand is pointed at a literal already holding the value, or at one appended to
//! the end of `ScriptLiterals`. The literals are the last of the constant tables the run time lays
//! end to end, so appending one moves nothing else, and the VM reads them from the cooked array on
//! every run.
//!
//! A colour transform inserts instructions, which the program can take: it has no jumps, so
//! nothing holds an instruction's offset, and the run time rebuilds its optimised program from the
//! cooked `ByteCode` when it loads. The block goes before the `acquireindex` that gives the
//! outputs their rows, never between it and them: the run time packs that run of outputs, and an
//! instruction inside it sends every output after it to the wrong rows (`VectorVM.cpp`,
//! `PackedOutputOptimization`). It writes temp registers past the ones the script uses, and the
//! colour's outputs read those.

use serde::{Deserialize, Serialize};

use crate::Mappings;
use crate::niagara_vm::{
    VmInstruction, VmLoc, VmOperand, VmProgram, VmRole, VmScript, VmShape, VmTable, VmType,
    const_layout, decode_vm, encode_vm, read_vm_script, script_problems, show_bits, vm_op_name,
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

/// A colour a Niagara script writes, transformed where it is written.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VmColorTransform {
    /// The `NiagaraScript` export, as [`VmConstEdit::export`] names it.
    pub export: String,
    /// The colour attribute, as its outputs name it: `Particles.Color`.
    pub attribute: String,
    #[serde(flatten)]
    pub transform: ColorTransform,
}

/// What a colour transform makes of the red, green and blue a script writes. Alpha is kept.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "transform", rename_all = "snake_case")]
pub enum ColorTransform {
    /// Each channel becomes its row of `matrix` times the colour, kept at 0 or above.
    Linear { matrix: [[f32; 3]; 3] },
    /// Each channel becomes `k` times the colour's brightest channel.
    Paint { k: [f32; 3] },
}

/// What a script's program and literals are to read as once a save's edits on it have landed,
/// with its temp register count where a transform raised it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmEditPlan {
    pub export: u32,
    pub bytecode: Vec<u8>,
    pub literals: Vec<u8>,
    pub temp_registers: Option<i64>,
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

/// Plans `edits` and `colors` against `parsed` and lowers them to path edits on each script's
/// `CachedScriptVM.ScriptLiterals` and `CachedScriptVM.ByteCode.Data`, and its
/// `NumTempRegisters` where a colour transform needs more. A script's constant edits land before
/// its transforms, at the offsets they were read at.
pub fn lower_vm_edits(
    parsed: &ParsedPackage,
    edits: &[VmConstEdit],
    colors: &[VmColorTransform],
    mappings: Option<&Mappings>,
    allow_drift: bool,
) -> Result<LoweredVm, String> {
    let mut out = LoweredVm::default();
    // Each script's edits are planned together, in order, on one copy.
    let mut order: Vec<u32> = Vec::new();
    let mut grouped: std::collections::BTreeMap<u32, Vec<&VmConstEdit>> = Default::default();
    let mut transforms: std::collections::BTreeMap<u32, Vec<&VmColorTransform>> =
        Default::default();
    for edit in edits {
        let export = export_of(parsed, &edit.export)?;
        if !order.contains(&export.index) {
            order.push(export.index);
        }
        grouped.entry(export.index).or_default().push(edit);
    }
    for color in colors {
        let export = export_of(parsed, &color.export)?;
        if !order.contains(&export.index) {
            order.push(export.index);
        }
        transforms.entry(export.index).or_default().push(color);
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
        for edit in grouped.get(&index).into_iter().flatten() {
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
        let mut temps = script.temp_registers;
        for color in transforms.get(&index).into_iter().flatten() {
            let (transformed, used) = insert_color_transform(
                &script,
                &bytes,
                &mut literals,
                temps,
                color,
                &calls,
                &label,
            )?;
            out.notes.push(format!(
                "{label}: {} transformed before it is written, on {used} new temp register(s)",
                color.attribute
            ));
            bytes = transformed;
            temps += used;
        }
        let script_path = index.to_string();
        let grown = bytes.len() != script.bytecode.len();
        if grown {
            let mut raw = (bytes.len() as i32).to_le_bytes().to_vec();
            raw.extend_from_slice(&bytes);
            out.paths.push(PathEdit {
                export: script_path.clone(),
                row: None,
                defaults: false,
                path: "CachedScriptVM.ByteCode.Data".to_string(),
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
        if temps != script.temp_registers {
            out.paths.push(PathEdit {
                export: script_path.clone(),
                row: None,
                defaults: false,
                path: "CachedScriptVM.NumTempRegisters".to_string(),
                op: PathOp::Set {
                    text: temps.to_string(),
                },
                was: Some(script.temp_registers.to_string()),
                becomes: None,
            });
        }
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
            if !grown && old != new {
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
                temp_registers: (temps != script.temp_registers).then_some(temps),
            });
        }
    }
    Ok(out)
}

const OP_MUL: u8 = 3;
const OP_MAD: u8 = 5;
const OP_MAX: u8 = 30;
const OP_OUTPUT_FLOAT: u8 = 83;
const OP_OUTPUT_HALF: u8 = 85;

/// The highest register an external call can name: bit 15 marks a register among its inputs.
const MAX_REGISTER: i64 = 0x7FFF;

/// An instruction's source in a block a transform builds.
#[derive(Clone, Copy)]
enum Src {
    Register(u16),
    Constant(u16),
}

/// A binary or ternary instruction reading `sources` and writing register `dest`.
fn instruction(op: u8, sources: &[Src], dest: u16) -> VmInstruction {
    let mut mask = 0u8;
    let mut operands: Vec<VmOperand> = sources
        .iter()
        .enumerate()
        .map(|(k, source)| {
            let (raw, loc) = match *source {
                Src::Register(reg) => (reg, VmLoc::Register(reg)),
                Src::Constant(offset) => {
                    mask |= 1 << k;
                    (offset, VmLoc::Constant(offset))
                }
            };
            VmOperand {
                at: 0,
                raw,
                role: VmRole::Source(k as u8),
                loc,
                ty: VmType::Float,
            }
        })
        .collect();
    operands.push(VmOperand {
        at: 0,
        raw: dest,
        role: VmRole::Dest,
        loc: VmLoc::Register(dest),
        ty: VmType::Any,
    });
    VmInstruction {
        at: 0,
        len: 2 + 2 * operands.len() as u32,
        op,
        name: vm_op_name(op),
        shape: if sources.len() == 3 {
            VmShape::Ternary
        } else {
            VmShape::Binary
        },
        mask: Some(mask),
        function: None,
        operands,
    }
}

fn writes(instruction: &VmInstruction, reg: u16) -> bool {
    instruction.operands.iter().any(|operand| {
        matches!(operand.role, VmRole::Dest | VmRole::CallOut(_))
            && operand.loc == VmLoc::Register(reg)
    })
}

/// `bytes` with `color` inserted before the run of outputs that writes its attribute, and how many
/// temp registers it added past `temps`. Its coefficients go into `literals`.
#[allow(clippy::too_many_arguments)]
fn insert_color_transform(
    script: &VmScript,
    bytes: &[u8],
    literals: &mut Vec<u8>,
    temps: i64,
    color: &VmColorTransform,
    calls: &[(u16, u16)],
    label: &str,
) -> Result<(Vec<u8>, i64), String> {
    let attribute = &color.attribute;
    let program = decode_vm(bytes, calls);
    if let Some(stop) = &program.stop {
        return Err(format!(
            "{label}: its bytecode does not decode past 0x{:04X}: {}",
            stop.at, stop.reason
        ));
    }
    let mut instructions = program.instructions;
    let mut outputs: [Option<usize>; 3] = [None; 3];
    for (index, instruction) in instructions.iter().enumerate() {
        if !matches!(instruction.op, OP_OUTPUT_FLOAT | OP_OUTPUT_HALF) {
            continue;
        }
        let name = script.component_name(
            instruction.op,
            instruction.operands[0].raw,
            instruction.operands[3].raw,
        );
        // A colour stored at half precision is a half vector, its channels X, Y and Z.
        for (channel, letters) in [["R", "X"], ["G", "Y"], ["B", "Z"]].iter().enumerate() {
            if letters.iter().any(|l| name == format!("{attribute}.{l}"))
                && outputs[channel].replace(index).is_some()
            {
                return Err(format!("{label}: it writes {name} more than once"));
            }
        }
    }
    let mut at = [0usize; 3];
    for (channel, letter) in ["R", "G", "B"].iter().enumerate() {
        at[channel] = outputs[channel]
            .ok_or_else(|| format!("{label}: it writes no {attribute}.{letter}"))?;
    }
    let index_reg = instructions[at[0]].operands[1].raw;
    if at
        .iter()
        .any(|&index| instructions[index].operands[1].raw != index_reg)
    {
        return Err(format!(
            "{label}: {attribute}'s channels are written to rows of different registers"
        ));
    }
    let first = at.iter().copied().min().unwrap_or(0);
    let last = at.iter().copied().max().unwrap_or(0);
    let acquire = (0..first)
        .rev()
        .find(|&index| {
            let instruction = &instructions[index];
            vm_op_name(instruction.op) == "acquireindex"
                && instruction.operands.last().map(|operand| operand.loc)
                    == Some(VmLoc::Register(index_reg))
        })
        .ok_or_else(|| {
            format!("{label}: no acquireindex gives the rows {attribute} is written to")
        })?;
    // The run time packs the run of outputs after an acquireindex, which only holds while nothing
    // else is in it.
    if let Some(other) = instructions[acquire + 1..=last].iter().find(|instruction| {
        instruction.shape != VmShape::Output && vm_op_name(instruction.op) != "update_id"
    }) {
        return Err(format!(
            "{label}: {} at 0x{:04X} sits between the acquireindex and {attribute}'s outputs",
            other.name, other.at
        ));
    }
    let mut sources = [Src::Register(0); 3];
    for channel in 0..3 {
        let output = &instructions[at[channel]];
        let operand = &output.operands[2];
        sources[channel] = match operand.loc {
            VmLoc::Constant(offset) => Src::Constant(offset),
            VmLoc::Register(reg) => {
                if let Some(writer) = instructions[acquire..at[channel]]
                    .iter()
                    .find(|instruction| writes(instruction, reg))
                {
                    return Err(format!(
                        "{label}: {} at 0x{:04X} writes the register {attribute} is written from \
                         after its acquireindex",
                        writer.name, writer.at
                    ));
                }
                Src::Register(reg)
            }
            _ => return Err(format!("{label}: {attribute}'s output reads no value")),
        };
    }
    let base = script
        .layout
        .as_ref()
        .map_or(0, |layout| layout.literal_base()) as usize;
    let mut literal = |value: f32| -> Result<Src, String> {
        let bits = value.to_bits();
        let slot =
            match (0..literals.len() / 4).find(|slot| bits_at(literals, slot * 4) == Some(bits)) {
                Some(slot) => slot * 4,
                None => {
                    let within = literals.len();
                    literals.extend_from_slice(&bits.to_le_bytes());
                    within
                }
            };
        u16::try_from(base + slot)
            .map(Src::Constant)
            .map_err(|_| format!("{label}: a literal would sit past the 65535 an operand reaches"))
    };
    let mut next = temps;
    let mut temp = || -> Result<u16, String> {
        if next > MAX_REGISTER {
            return Err(format!(
                "{label}: it would need a register past {MAX_REGISTER}"
            ));
        }
        next += 1;
        Ok((next - 1) as u16)
    };
    let mut block = Vec::new();
    let mut results = [0u16; 3];
    match color.transform {
        ColorTransform::Linear { matrix } => {
            for channel in 0..3 {
                let t = temp()?;
                let terms: Vec<(usize, f32)> = (0..3)
                    .map(|from| (from, matrix[channel][from]))
                    .filter(|(_, weight)| *weight != 0.0)
                    .collect();
                match terms.split_first() {
                    None => block.push(instruction(OP_MUL, &[sources[0], literal(0.0)?], t)),
                    Some((&(from, weight), rest)) => {
                        block.push(instruction(OP_MUL, &[sources[from], literal(weight)?], t));
                        for &(from, weight) in rest {
                            block.push(instruction(
                                OP_MAD,
                                &[sources[from], literal(weight)?, Src::Register(t)],
                                t,
                            ));
                        }
                    }
                }
                block.push(instruction(OP_MAX, &[Src::Register(t), literal(0.0)?], t));
                results[channel] = t;
            }
        }
        ColorTransform::Paint { k } => {
            let brightest = temp()?;
            block.push(instruction(OP_MAX, &[sources[0], sources[1]], brightest));
            block.push(instruction(
                OP_MAX,
                &[Src::Register(brightest), sources[2]],
                brightest,
            ));
            for channel in 0..3 {
                let t = temp()?;
                block.push(instruction(
                    OP_MUL,
                    &[Src::Register(brightest), literal(k[channel])?],
                    t,
                ));
                results[channel] = t;
            }
        }
    }
    for channel in 0..3 {
        let output = &mut instructions[at[channel]];
        output.mask = Some(output.mask.unwrap_or(0) & !1);
        output.operands[2].raw = results[channel];
        output.operands[2].loc = VmLoc::Register(results[channel]);
    }
    instructions.splice(acquire..acquire, block);
    let rebuilt = encode_vm(&VmProgram {
        instructions,
        stop: None,
    });
    let check = VmScript {
        bytecode: rebuilt.clone(),
        layout: const_layout(
            &script.usage,
            script.param_size,
            script.interpolated,
            literals.len() as u32,
        ),
        literals: literals.clone(),
        temp_registers: next,
        program: decode_vm(&rebuilt, calls),
        ..script.clone()
    };
    let problems = script_problems(&check);
    if let Some(problem) = problems.first() {
        return Err(format!(
            "{label}: the transformed program breaks: {problem}"
        ));
    }
    if let Some(high) = check
        .program
        .instructions
        .iter()
        .flat_map(|instruction| &instruction.operands)
        .filter_map(|operand| match (operand.role, operand.loc) {
            (VmRole::DataSet | VmRole::Component, _) => None,
            (_, VmLoc::Register(reg)) => Some(i64::from(reg)),
            _ => None,
        })
        .find(|reg| *reg >= next)
    {
        return Err(format!(
            "{label}: register {high} is past the {next} the transformed script has"
        ));
    }
    Ok((rebuilt, next - temps))
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
        if let Some(temps) = plan.temp_registers
            && script.temp_registers != temps
        {
            return Err(format!(
                "{} has {} temp registers after the save, not the {temps} planned",
                export.object_name, script.temp_registers
            ));
        }
        if plan.temp_registers.is_some()
            && let Some(problem) = script_problems(&script).first()
        {
            return Err(format!(
                "{}'s transformed program breaks after the save: {problem}",
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
    use crate::niagara_vm::tests::{le, program, script_of, script_with_color};

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

    const ACQUIRE: u8 = 86;

    /// Literals start at 644: 2.0 at +0, 0.5 at +4, 1.0 at +8. Red is computed, green a literal,
    /// blue and alpha read the age; `output` is the op each channel is written with.
    fn colour_program(output: u8, between: &[u8]) -> Vec<u8> {
        // Age is the first float, so a float colour's channels start at 1 and a half one's at 0.
        let red = if output == OP_OUTPUT_HALF { 0 } else { 1 };
        program(&[
            &[77],
            &le(&[0, 0, 1]), // inputdata_float Age -> r1
            &[OP_MUL, 1],
            &le(&[644, 1, 2]), // mul 2.0, r1 -> r2
            &[ACQUIRE, 1],
            &le(&[0, 652, 0]), // acquireindex ds0, #1.0 -> r0
            between,
            &[output, 0],
            &le(&[0, 0, 2, red]), // Color.R = r2
            &[output, 1],
            &le(&[0, 0, 648, red + 1]), // Color.G = 0.5
            &[output, 0],
            &le(&[0, 0, 1, red + 2]), // Color.B = r1
            &[output, 0],
            &le(&[0, 0, 1, red + 3]), // Color.A = r1
            &[0],
        ])
    }

    fn transform(script: &VmScript, transform: ColorTransform) -> Result<(VmScript, i64), String> {
        let color = VmColorTransform {
            export: "SpawnScript".to_string(),
            attribute: "Particles.Color".to_string(),
            transform,
        };
        let mut literals = script.literals.clone();
        let (bytes, used) = insert_color_transform(
            script,
            &script.bytecode,
            &mut literals,
            script.temp_registers,
            &color,
            &[(1, 1)],
            "SpawnScript",
        )?;
        let after = VmScript {
            program: decode_vm(&bytes, &[(1, 1)]),
            layout: const_layout(&script.usage, 4, false, literals.len() as u32),
            bytecode_len: bytes.len() as u32,
            bytecode: bytes,
            literals,
            temp_registers: script.temp_registers + used,
            ..script.clone()
        };
        Ok((after, used))
    }

    fn source(instruction: &VmInstruction) -> VmLoc {
        instruction.operands[2].loc
    }

    /// Paint reads each channel as the output did, register or literal, takes the brightest and
    /// scales it, all before the acquireindex; the outputs read the new temps.
    #[test]
    fn a_paint_goes_before_the_acquireindex_and_feeds_the_colour_outputs() {
        let script = script_of(colour_program(OP_OUTPUT_FLOAT, &[]), &[2.0, 0.5, 1.0]);
        let (after, used) =
            transform(&script, ColorTransform::Paint { k: [0.0, 0.5, 1.0] }).unwrap();
        assert_eq!(used, 4);
        let names: Vec<&str> = after.program.instructions.iter().map(|i| i.name).collect();
        assert_eq!(
            names,
            [
                "inputdata_float",
                "mul",
                "max",
                "max",
                "mul",
                "mul",
                "mul",
                "acquireindex",
                "outputdata_float",
                "outputdata_float",
                "outputdata_float",
                "outputdata_float",
                "done"
            ]
        );
        let ins = &after.program.instructions;
        assert_eq!(ins[2].operands[0].loc, VmLoc::Register(2));
        assert_eq!(ins[2].operands[1].loc, VmLoc::Constant(648));
        assert_eq!(ins[2].operands[2].loc, VmLoc::Register(8));
        assert_eq!(ins[3].operands[1].loc, VmLoc::Register(1));
        // 0.0 is appended; 0.5 and 1.0 are read where they already are.
        assert_eq!(after.literals.len(), 16);
        assert_eq!(ins[4].operands[1].loc, VmLoc::Constant(656));
        assert_eq!(ins[5].operands[1].loc, VmLoc::Constant(648));
        assert_eq!(ins[6].operands[1].loc, VmLoc::Constant(652));
        assert_eq!(source(&ins[8]), VmLoc::Register(9));
        assert_eq!(source(&ins[9]), VmLoc::Register(10));
        assert_eq!(ins[9].mask, Some(0), "green reads a register now");
        assert_eq!(source(&ins[10]), VmLoc::Register(11));
        assert_eq!(source(&ins[11]), VmLoc::Register(1), "alpha is kept");
        assert!(
            script_problems(&after).is_empty(),
            "{:?}",
            script_problems(&after)
        );
        // Everything but the block and the three repointed sources reads as it did.
        let before = &script.program.instructions;
        for (old, new) in before.iter().zip(ins[..2].iter().chain(&ins[7..])) {
            assert_eq!(old.name, new.name);
            for (a, b) in old.operands.iter().zip(&new.operands) {
                if a.role != VmRole::Source(0) || old.shape != VmShape::Output {
                    assert_eq!(a.loc, b.loc);
                }
            }
        }
    }

    /// A full matrix reads every channel into each one and keeps the result at 0 or above; a
    /// colour stored at half precision is found by its X, Y and Z.
    #[test]
    fn a_matrix_on_half_outputs_mixes_every_channel_into_each() {
        let script = script_with_color(
            colour_program(OP_OUTPUT_HALF, &[]),
            &[2.0, 0.5, 1.0],
            "NiagaraHalfVector4",
        );
        let matrix = [[0.5, 0.25, 0.25], [0.0, 1.0, 0.0], [0.1, 0.2, 0.7]];
        let (after, used) = transform(&script, ColorTransform::Linear { matrix }).unwrap();
        assert_eq!(used, 3);
        let names: Vec<&str> = after.program.instructions[2..14]
            .iter()
            .map(|i| i.name)
            .collect();
        assert_eq!(
            names,
            [
                "mul",
                "mad",
                "mad",
                "max",
                "mul",
                "max",
                "mul",
                "mad",
                "mad",
                "max",
                "acquireindex",
                "outputdata_half"
            ]
        );
        let ins = &after.program.instructions;
        assert_eq!(source(&ins[13]), VmLoc::Register(8));
        assert_eq!(source(&ins[14]), VmLoc::Register(9));
        assert_eq!(source(&ins[15]), VmLoc::Register(10));
        assert!(
            script_problems(&after).is_empty(),
            "{:?}",
            script_problems(&after)
        );
    }

    /// Anything but outputs after the acquireindex would be packed into the wrong rows, so a
    /// script with one is left alone, as is one that would need a register past 0x7FFF.
    #[test]
    fn a_transform_is_refused_where_it_cannot_go_safely() {
        let mut between = vec![OP_MUL, 1];
        between.extend(le(&[644, 1, 3])); // mul 2.0, r1 -> r3
        let script = script_of(colour_program(OP_OUTPUT_FLOAT, &between), &[2.0, 0.5, 1.0]);
        let paint = ColorTransform::Paint { k: [1.0, 0.0, 0.0] };
        let err = transform(&script, paint).unwrap_err();
        assert!(err.contains("sits between the acquireindex"), "{err}");
        let full = VmScript {
            temp_registers: 0x7FFE,
            ..script_of(colour_program(OP_OUTPUT_FLOAT, &[]), &[2.0, 0.5, 1.0])
        };
        let err = transform(&full, paint).unwrap_err();
        assert!(err.contains("past 32767"), "{err}");
    }
}
