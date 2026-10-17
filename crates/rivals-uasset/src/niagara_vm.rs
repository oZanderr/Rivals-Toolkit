//! Reads a cooked Niagara script's VectorVM bytecode: each instruction with its operands, what each constant reads and what each output writes.
//!
//! The bytecode is the legacy format UE 5.3 cooks into `CachedScriptVM.ByteCode.Data` and runs on
//! the CPU. An instruction is its opcode byte, then for most a byte of flags saying which sources
//! are constants, then little-endian u16 operands: a temp register, or a byte offset into the
//! constant tables the run time lays end to end (the engine's global, system, owner and emitter
//! parameters, the script's own parameter store, their previous-frame copies for an interpolated
//! spawn script, and last the script's literals). Outputs name a component of a data set, counted
//! per type over the script's sorted attributes.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::Mappings;
use crate::package::{ParsedExport, ParsedPackage};
use crate::value::{PropertyEntry, PropertyValue};

/// The opcode names of the legacy VM, in the engine's order (`VVM_OP_XM_LIST` in
/// `VectorVMCommon.h`). Cooked bytecode holds only these, and never `half_to_float` (95).
const OP_NAMES: [&str; 98] = [
    "done",
    "add",
    "sub",
    "mul",
    "div",
    "mad",
    "lerp",
    "rcp",
    "rsq",
    "sqrt",
    "neg",
    "abs",
    "exp",
    "exp2",
    "log",
    "log2",
    "sin",
    "cos",
    "tan",
    "asin",
    "acos",
    "atan",
    "atan2",
    "ceil",
    "floor",
    "fmod",
    "frac",
    "trunc",
    "clamp",
    "min",
    "max",
    "pow",
    "round",
    "sign",
    "step",
    "random",
    "noise",
    "cmplt",
    "cmple",
    "cmpgt",
    "cmpge",
    "cmpeq",
    "cmpneq",
    "select",
    "addi",
    "subi",
    "muli",
    "divi",
    "clampi",
    "mini",
    "maxi",
    "absi",
    "negi",
    "signi",
    "randomi",
    "cmplti",
    "cmplei",
    "cmpgti",
    "cmpgei",
    "cmpeqi",
    "cmpneqi",
    "bit_and",
    "bit_or",
    "bit_xor",
    "bit_not",
    "bit_lshift",
    "bit_rshift",
    "logic_and",
    "logic_or",
    "logic_xor",
    "logic_not",
    "f2i",
    "i2f",
    "f2b",
    "b2f",
    "i2b",
    "b2i",
    "inputdata_float",
    "inputdata_int32",
    "inputdata_half",
    "inputdata_noadvance_float",
    "inputdata_noadvance_int32",
    "inputdata_noadvance_half",
    "outputdata_float",
    "outputdata_int32",
    "outputdata_half",
    "acquireindex",
    "external_func_call",
    "exec_index",
    "noise2D",
    "noise3D",
    "enter_stat_scope",
    "exit_stat_scope",
    "update_id",
    "acquire_id",
    "half_to_float",
    "fasi",
    "iasf",
];

/// How an instruction's bytes are laid out, which is what sizes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VmShape {
    /// `[op][mask][src][dst]`.
    Unary,
    /// `[op][mask][src0][src1][dst]`.
    Binary,
    /// `[op][mask][src0][src1][src2][dst]`.
    Ternary,
    /// `[op][dst]`.
    ExecIndex,
    /// `[op][data set][component][dst]`.
    Input,
    /// `[op][mask][data set][index register][src][component]`.
    Output,
    /// `[op][mask][data set][valid][dst]`.
    AcquireIndex,
    /// `[op][function][input...][output...]`, as many of each as the function binding says.
    ExternalCall,
    /// `[op][data set][register][register]`: `acquire_id` writes both, `update_id` reads both.
    IdRw,
    /// `[op][constant]`: the constant holds the scope's index.
    EnterStat,
    ExitStat,
    Done,
}

/// The shape of opcode `op`, or `None` for one legacy bytecode never holds.
pub fn vm_shape(op: u8) -> Option<VmShape> {
    use VmShape::*;
    Some(match op {
        0 => Done,
        1..=4 | 22 | 25 | 29..=31 | 34 | 37..=42 | 44..=47 | 49 | 50 | 55..=63 | 65..=69 | 89 => {
            Binary
        }
        5 | 6 | 28 | 43 | 48 | 90 => Ternary,
        7..=21 | 23 | 24 | 26 | 27 | 32 | 33 | 35 | 36 | 51..=54 | 64 | 70..=76 | 96 | 97 => Unary,
        77..=82 => Input,
        83..=85 => Output,
        86 => AcquireIndex,
        87 => ExternalCall,
        88 => ExecIndex,
        91 => EnterStat,
        92 => ExitStat,
        93 | 94 => IdRw,
        _ => return None,
    })
}

/// The name of opcode `op`.
pub fn vm_op_name(op: u8) -> &'static str {
    OP_NAMES.get(op as usize).copied().unwrap_or("?")
}

/// What a value read by an operand holds, which is how a literal there is shown and typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VmType {
    Float,
    Int,
    /// Either: `select`'s choices and an external function's inputs.
    Any,
}

/// What source `k` of opcode `op` reads.
fn source_type(op: u8, k: usize) -> VmType {
    match op {
        43 if k == 0 => VmType::Int,
        43 | 87 => VmType::Any,
        1..=42 | 71 | 73 | 83 | 85 | 89 | 90 | 96 => VmType::Float,
        _ => VmType::Int,
    }
}

/// What an operand is to its instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "role", content = "index")]
pub enum VmRole {
    /// The `k`th value an instruction reads.
    Source(u8),
    /// The register an instruction writes.
    Dest,
    DataSet,
    Component,
    /// An output's register holding each instance's row.
    IndexReg,
    /// `acquireindex`'s condition.
    Valid,
    /// The `k`th input or output of an external function call.
    CallIn(u8),
    CallOut(u8),
    /// `enter_stat_scope`'s constant.
    Scope,
}

/// Where an operand's value is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "loc", content = "value")]
pub enum VmLoc {
    Register(u16),
    /// A byte offset into the constant tables laid end to end.
    Constant(u16),
    /// A data set or component number.
    Index(u16),
    /// An external call's input or output it leaves out (0xFFFF).
    Unused,
}

/// One u16 operand.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VmOperand {
    /// Where its two bytes are in the bytecode, which is what an edit patches.
    pub at: u32,
    pub raw: u16,
    pub role: VmRole,
    pub loc: VmLoc,
    pub ty: VmType,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VmInstruction {
    pub at: u32,
    pub len: u32,
    pub op: u8,
    pub name: &'static str,
    pub shape: VmShape,
    /// Which sources are constants, for the shapes that carry the byte.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mask: Option<u8>,
    /// The external call's index into `CalledVMExternalFunctions`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<u8>,
    /// The u16 fields in byte order; an edit names one by its place here.
    pub operands: Vec<VmOperand>,
}

/// Where decoding stopped short of a clean end, and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VmStop {
    pub at: u32,
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct VmProgram {
    pub instructions: Vec<VmInstruction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<VmStop>,
}

impl VmProgram {
    /// Whether every byte decoded and the program ends at its `done`.
    pub fn complete(&self) -> bool {
        self.stop.is_none()
    }
}

/// Decodes `bytes`. `calls` gives each external function's input and output counts, in the
/// order `CalledVMExternalFunctions` lists them; each call site has its own entry.
pub fn decode_vm(bytes: &[u8], calls: &[(u16, u16)]) -> VmProgram {
    let mut program = VmProgram::default();
    let mut at = 0usize;
    let stop = |at: usize, reason: String| {
        Some(VmStop {
            at: at as u32,
            reason,
        })
    };
    while at < bytes.len() {
        let op = bytes[at];
        let Some(shape) = vm_shape(op) else {
            program.stop = stop(
                at,
                format!(
                    "opcode {op} ({}) is not one legacy bytecode holds",
                    vm_op_name(op)
                ),
            );
            return program;
        };
        match read_instruction(bytes, at, op, shape, calls) {
            Ok(instruction) => {
                at += instruction.len as usize;
                let done = shape == VmShape::Done;
                program.instructions.push(instruction);
                if done {
                    if at < bytes.len() {
                        program.stop =
                            stop(at, format!("{} byte(s) follow `done`", bytes.len() - at));
                    }
                    return program;
                }
            }
            Err(reason) => {
                program.stop = stop(at, reason);
                return program;
            }
        }
    }
    program.stop = stop(at, "the bytecode ends without `done`".to_string());
    program
}

fn read_instruction(
    bytes: &[u8],
    at: usize,
    op: u8,
    shape: VmShape,
    calls: &[(u16, u16)],
) -> Result<VmInstruction, String> {
    let name = vm_op_name(op);
    let byte = |pos: usize| {
        bytes
            .get(pos)
            .copied()
            .ok_or_else(|| format!("{name} runs past the end"))
    };
    let word = |pos: usize| -> Result<u16, String> {
        Ok(u16::from_le_bytes([byte(pos)?, byte(pos + 1)?]))
    };
    let mut instruction = VmInstruction {
        at: at as u32,
        len: 1,
        op,
        name,
        shape,
        mask: None,
        function: None,
        operands: Vec::new(),
    };
    let mut pos = at + 1;
    let mut push = |pos: &mut usize, role: VmRole, loc: fn(u16) -> VmLoc, ty: VmType| {
        let raw = word(*pos)?;
        instruction.operands.push(VmOperand {
            at: *pos as u32,
            raw,
            role,
            loc: loc(raw),
            ty,
        });
        *pos += 2;
        Ok::<(), String>(())
    };
    let constant_if = |mask: u8, bit: u8| -> fn(u16) -> VmLoc {
        if mask & (1 << bit) != 0 {
            VmLoc::Constant
        } else {
            VmLoc::Register
        }
    };
    match shape {
        VmShape::Unary | VmShape::Binary | VmShape::Ternary => {
            let sources = match shape {
                VmShape::Unary => 1,
                VmShape::Binary => 2,
                _ => 3,
            };
            let mask = byte(pos)?;
            if u32::from(mask) >= 1 << sources {
                return Err(format!(
                    "{name}'s constant flags 0x{mask:02X} name a source it does not have"
                ));
            }
            pos += 1;
            for k in 0..sources {
                push(
                    &mut pos,
                    VmRole::Source(k),
                    constant_if(mask, k),
                    source_type(op, k as usize),
                )?;
            }
            push(&mut pos, VmRole::Dest, VmLoc::Register, VmType::Any)?;
            instruction.mask = Some(mask);
        }
        VmShape::ExecIndex => push(&mut pos, VmRole::Dest, VmLoc::Register, VmType::Int)?,
        VmShape::Input => {
            push(&mut pos, VmRole::DataSet, VmLoc::Index, VmType::Int)?;
            push(&mut pos, VmRole::Component, VmLoc::Index, VmType::Int)?;
            push(&mut pos, VmRole::Dest, VmLoc::Register, VmType::Any)?;
        }
        VmShape::Output | VmShape::AcquireIndex => {
            let mask = byte(pos)?;
            if mask > 1 {
                return Err(format!(
                    "{name}'s constant flags 0x{mask:02X} name a source it does not have"
                ));
            }
            pos += 1;
            push(&mut pos, VmRole::DataSet, VmLoc::Index, VmType::Int)?;
            if shape == VmShape::Output {
                push(&mut pos, VmRole::IndexReg, VmLoc::Register, VmType::Int)?;
                push(
                    &mut pos,
                    VmRole::Source(0),
                    constant_if(mask, 0),
                    source_type(op, 0),
                )?;
                push(&mut pos, VmRole::Component, VmLoc::Index, VmType::Int)?;
            } else {
                push(&mut pos, VmRole::Valid, constant_if(mask, 0), VmType::Int)?;
                push(&mut pos, VmRole::Dest, VmLoc::Register, VmType::Int)?;
            }
            instruction.mask = Some(mask);
        }
        VmShape::ExternalCall => {
            let function = byte(pos)?;
            pos += 1;
            let Some(&(inputs, outputs)) = calls.get(function as usize) else {
                return Err(format!(
                    "external call {function} is past the {} function(s) the script binds",
                    calls.len()
                ));
            };
            for k in 0..inputs {
                push(&mut pos, VmRole::CallIn(k as u8), call_input, VmType::Any)?;
            }
            for k in 0..outputs {
                push(&mut pos, VmRole::CallOut(k as u8), call_output, VmType::Any)?;
            }
            instruction.function = Some(function);
        }
        VmShape::IdRw => {
            push(&mut pos, VmRole::DataSet, VmLoc::Index, VmType::Int)?;
            // acquire_id writes the id's index and tag; update_id reads an id and an index.
            let role = |k: u8| {
                if op == 94 {
                    VmRole::Dest
                } else {
                    VmRole::Source(k)
                }
            };
            push(&mut pos, role(0), VmLoc::Register, VmType::Int)?;
            push(&mut pos, role(1), VmLoc::Register, VmType::Int)?;
        }
        VmShape::EnterStat => push(&mut pos, VmRole::Scope, VmLoc::Constant, VmType::Int)?,
        VmShape::ExitStat | VmShape::Done => {}
    }
    instruction.len = (pos - at) as u32;
    Ok(instruction)
}

/// An external call's input: bit 15 marks a register, else it is a constant's offset.
fn call_input(raw: u16) -> VmLoc {
    match raw {
        0xFFFF => VmLoc::Unused,
        raw if raw & 0x8000 != 0 => VmLoc::Register(raw & 0x7FFF),
        raw => VmLoc::Constant(raw),
    }
}

fn call_output(raw: u16) -> VmLoc {
    match raw {
        0xFFFF => VmLoc::Unused,
        raw => VmLoc::Register(raw),
    }
}

/// The bytes `program` decodes from, rebuilt: what a decode that lost nothing gives back.
pub fn encode_vm(program: &VmProgram) -> Vec<u8> {
    let mut out = Vec::new();
    for instruction in &program.instructions {
        out.push(instruction.op);
        if let Some(mask) = instruction.mask {
            out.push(mask);
        }
        if let Some(function) = instruction.function {
            out.push(function);
        }
        for operand in &instruction.operands {
            out.extend_from_slice(&operand.raw.to_le_bytes());
        }
    }
    out
}

/// One of the constant tables a script's constant operands index into.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VmTable {
    Global,
    System,
    Owner,
    Emitter,
    /// The script's own parameter store.
    External,
    PrevGlobal,
    PrevSystem,
    PrevOwner,
    PrevEmitter,
    PrevExternal,
    /// The script's literals, always last.
    Literals,
}

impl VmTable {
    /// The short form a constant's key starts with.
    pub fn key(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::System => "system",
            Self::Owner => "owner",
            Self::Emitter => "emitter",
            Self::External => "ext",
            Self::PrevGlobal => "prev-global",
            Self::PrevSystem => "prev-system",
            Self::PrevOwner => "prev-owner",
            Self::PrevEmitter => "prev-emitter",
            Self::PrevExternal => "prev-ext",
            Self::Literals => "lit",
        }
    }
}

/// The engine's own parameter tables: `FNiagaraGlobalParameters`, `FNiagaraSystemParameters`,
/// `FNiagaraOwnerParameters` and `FNiagaraEmitterParameters`, 16-byte aligned (`NiagaraTypes.h`).
const GLOBAL_BYTES: u32 = 32;
const SYSTEM_BYTES: u32 = 64;
const OWNER_BYTES: u32 = 512;
const EMITTER_BYTES: u32 = 32;

/// The tables a script's constant operands reach, in the order the run time lays them out, each
/// as `(table, start, length)` in bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VmConstLayout {
    pub tables: Vec<(VmTable, u32, u32)>,
}

impl VmConstLayout {
    /// Where the literals start.
    pub fn literal_base(&self) -> u32 {
        self.tables
            .iter()
            .find(|(table, ..)| *table == VmTable::Literals)
            .map_or(0, |(_, start, _)| *start)
    }

    /// The table `offset` falls in, and how far into it.
    pub fn locate(&self, offset: u32) -> Option<(VmTable, u32)> {
        self.tables
            .iter()
            .find(|(_, start, len)| offset >= *start && offset < start + len)
            .map(|(table, start, _)| (*table, offset - start))
    }

    /// One past the last byte of the last table.
    pub fn end(&self) -> u32 {
        self.tables.last().map_or(0, |(_, start, len)| start + len)
    }
}

/// The constant tables the CPU VM gives a script of `usage` (an `ENiagaraScriptUsage`
/// enumerator), or `None` for a script it never runs: a GPU, simulation stage, emitter, function
/// or module script. A particle script reads the engine's four tables and its own parameter
/// store, both twice over when it is an interpolated spawn script, then its literals; a system
/// script reads the global table, its own store, then its literals (`NiagaraEmitterInstance.cpp`
/// and `NiagaraSystemSimulation.cpp`, `BuildConstantBufferTable`).
pub fn const_layout(
    usage: &str,
    param_size: u32,
    interpolated: bool,
    literals: u32,
) -> Option<VmConstLayout> {
    use VmTable::*;
    let tables: Vec<(VmTable, u32)> = match usage {
        "ParticleSpawnScript"
        | "ParticleSpawnScriptInterpolated"
        | "ParticleUpdateScript"
        | "ParticleEventScript" => {
            let mut tables = vec![
                (Global, GLOBAL_BYTES),
                (System, SYSTEM_BYTES),
                (Owner, OWNER_BYTES),
                (Emitter, EMITTER_BYTES),
                (External, param_size),
            ];
            if interpolated {
                tables.extend([
                    (PrevGlobal, GLOBAL_BYTES),
                    (PrevSystem, SYSTEM_BYTES),
                    (PrevOwner, OWNER_BYTES),
                    (PrevEmitter, EMITTER_BYTES),
                    (PrevExternal, param_size),
                ]);
            }
            tables.push((Literals, literals));
            tables
        }
        "SystemSpawnScript" | "SystemUpdateScript" => vec![
            (Global, GLOBAL_BYTES),
            (External, param_size),
            (Literals, literals),
        ],
        _ => return None,
    };
    let mut start = 0;
    Some(VmConstLayout {
        tables: tables
            .into_iter()
            .map(|(table, len)| {
                let entry = (table, start, len);
                start += len;
                entry
            })
            .collect(),
    })
}

/// The name of the 4-byte slot `index` of one of the engine's tables (`NiagaraModule.cpp`,
/// `GetVariables`).
fn engine_slot_name(table: VmTable, index: u32) -> Option<String> {
    const GLOBAL: [&str; 8] = [
        "Engine.WorldDeltaTime",
        "Engine.DeltaTime",
        "Engine.InverseDeltaTime",
        "Engine.Time",
        "Engine.RealTime",
        "Engine.QualityLevel",
        "Engine.PaddingInt32_0",
        "Engine.PaddingInt32_1",
    ];
    const SYSTEM: [&str; 16] = [
        "Engine.Owner.TimeSinceRendered",
        "Engine.Owner.LODDistance",
        "Engine.Owner.LODDistanceFraction",
        "Engine.System.Age",
        "Engine.Owner.ExecutionState",
        "Engine.System.TickCount",
        "Engine.System.NumEmitters",
        "Engine.System.NumEmittersAlive",
        "Engine.System.SignificanceIndex",
        "Engine.System.RandomSeed",
        "Engine.System.CurrentTimeStep",
        "Engine.System.NumTimeSteps",
        "Engine.System.TimeStepFraction",
        "Engine.System.NumParticles",
        "Engine.System.PaddingInt32_0",
        "Engine.System.PaddingInt32_1",
    ];
    const EMITTER: [&str; 8] = [
        "Engine.Emitter.NumParticles",
        "Engine.Emitter.TotalSpawnedParticles",
        "Engine.Emitter.SpawnCountScale",
        "Emitter.Age",
        "Emitter.RandomSeed",
        "Engine.Emitter.InstanceSeed",
        "Engine.Emitter.PaddingInt32_0",
        "Engine.Emitter.PaddingInt32_1",
    ];
    /// The owner table's members after its six matrices, each with its components.
    const OWNER_TAIL: [(&str, &[&str]); 8] = [
        ("Engine.Owner.Rotation", &["X", "Y", "Z", "W"]),
        ("Engine.Owner.Position", &["X", "Y", "Z", "#Padding_0"]),
        ("Engine.Owner.Velocity", &["X", "Y", "Z", "#Padding_1"]),
        ("Engine.Owner.SystemXAxis", &["X", "Y", "Z", "#Padding_2"]),
        ("Engine.Owner.SystemYAxis", &["X", "Y", "Z", "#Padding_3"]),
        ("Engine.Owner.SystemZAxis", &["X", "Y", "Z", "#Padding_4"]),
        ("Engine.Owner.Scale", &["X", "Y", "Z", "#Padding_5"]),
        ("Engine.Owner.LWCTile", &["X", "Y", "Z", "W"]),
    ];
    const MATRICES: [&str; 6] = [
        "Engine.Owner.SystemLocalToWorld",
        "Engine.Owner.SystemWorldToLocal",
        "Engine.Owner.SystemLocalToWorldTransposed",
        "Engine.Owner.SystemWorldToLocalTransposed",
        "Engine.Owner.SystemLocalToWorldNoScale",
        "Engine.Owner.SystemWorldToLocalNoScale",
    ];
    let index = index as usize;
    match table {
        VmTable::Global | VmTable::PrevGlobal => GLOBAL.get(index).map(|n| (*n).to_string()),
        VmTable::System | VmTable::PrevSystem => SYSTEM.get(index).map(|n| (*n).to_string()),
        VmTable::Emitter | VmTable::PrevEmitter => EMITTER.get(index).map(|n| (*n).to_string()),
        VmTable::Owner | VmTable::PrevOwner => {
            if index < 96 {
                return Some(format!("{}[{}]", MATRICES[index / 16], index % 16));
            }
            let (name, parts) = OWNER_TAIL.get((index - 96) / 4)?;
            let part = parts[(index - 96) % 4];
            Some(match part.strip_prefix('#') {
                Some(pad) => format!("Engine.Owner.{pad}"),
                None => format!("{name}.{part}"),
            })
        }
        _ => None,
    }
}

/// What a constant operand reads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VmConst {
    pub offset: u16,
    pub table: VmTable,
    /// The byte offset within its table.
    pub within: u32,
    /// The parameter it reads, where the table names one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The four bytes it reads, where the script holds them: a literal, or a cooked parameter
    /// default, which the run time may replace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bits: Option<u32>,
    /// Names the constant in an edit's drift check: `lit+48=0x3F400000`, `ext+12`.
    pub key: String,
}

/// What a value's type is made of, component by component in property order, each a float, an
/// int or a half.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VmComponentKind {
    Float,
    Int,
    Half,
}

/// One component of a type: its kind and the suffix it adds to the value's name (empty for a
/// value of one component).
type Component = (VmComponentKind, String);

/// The components of the Niagara type `leaf` (the last part of its `ClassStructOrEnum` path),
/// laid out as `FNiagaraTypeLayoutInfo` does: floats as floats, ints and bools as ints, `uint16`
/// as halves, nested structs in property order (`NiagaraTypes.cpp`). An enum is one int. A type
/// this does not know is counted from its schema in `mappings`, when one is given.
fn components_of(
    leaf: &str,
    underlying: u64,
    mappings: Option<&Mappings>,
) -> Option<Vec<Component>> {
    use VmComponentKind::{Float as F, Half as H, Int as I};
    let named = |kind: VmComponentKind, names: &[&str]| -> Vec<Component> {
        names
            .iter()
            .map(|name| (kind, (*name).to_string()))
            .collect()
    };
    if underlying == 3 {
        return Some(vec![(I, String::new())]);
    }
    Some(match leaf {
        "NiagaraFloat" => vec![(F, String::new())],
        "NiagaraInt32" | "NiagaraBool" => vec![(I, String::new())],
        "NiagaraHalf" => vec![(H, String::new())],
        "NiagaraID" => named(I, &["Index", "AcquireTag"]),
        "NiagaraRandInfo" => named(I, &["Seed1", "Seed2", "Seed3"]),
        "NiagaraSpawnInfo" => vec![
            (I, "Count".to_string()),
            (F, "InterpStartDt".to_string()),
            (F, "IntervalDt".to_string()),
            (I, "SpawnGroup".to_string()),
        ],
        "Vector2f" | "Vector2D" => named(F, &["X", "Y"]),
        "Vector3f" | "Vector" | "NiagaraPosition" => named(F, &["X", "Y", "Z"]),
        "Vector4f" | "Vector4" | "Quat4f" | "Quat" => named(F, &["X", "Y", "Z", "W"]),
        "LinearColor" => named(F, &["R", "G", "B", "A"]),
        "NiagaraHalfVector2" => named(H, &["X", "Y"]),
        "NiagaraHalfVector3" => named(H, &["X", "Y", "Z"]),
        "NiagaraHalfVector4" => named(H, &["X", "Y", "Z", "W"]),
        "NiagaraMatrix" | "Matrix44f" | "Matrix" => (0..16)
            .map(|i| (F, format!("Row{}.{}", i / 4, ["X", "Y", "Z", "W"][i % 4])))
            .collect(),
        _ => return mappings.and_then(|mappings| schema_components(leaf, mappings, 0)),
    })
}

/// A struct's components from its schema, for a type the known list leaves out.
fn schema_components(name: &str, mappings: &Mappings, depth: u32) -> Option<Vec<Component>> {
    use usmap::PropertyInner as P;
    if depth > 8 {
        return None;
    }
    let schema = mappings.schema(name)?;
    let mut out = Vec::new();
    for slot in schema.iter() {
        let property = slot.property;
        let kind = match &property.inner {
            P::Float | P::Double => Some(VmComponentKind::Float),
            P::Int | P::Bool | P::Enum { .. } | P::UInt32 => Some(VmComponentKind::Int),
            P::UInt16 => Some(VmComponentKind::Half),
            P::Struct { name } => {
                let inner = components_of(name, 2, None)
                    .or_else(|| schema_components(name, mappings, depth + 1))?;
                out.extend(inner.into_iter().map(|(kind, suffix)| {
                    let joined = if suffix.is_empty() {
                        property.name.clone()
                    } else {
                        format!("{}.{suffix}", property.name)
                    };
                    (kind, joined)
                }));
                None
            }
            _ => return None,
        };
        if let Some(kind) = kind {
            out.push((kind, property.name.clone()));
        }
    }
    Some(out)
}

/// A variable a script's data set holds: one of its `Attributes`, or one of an event's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VmAttribute {
    pub name: String,
    /// The type's struct or enum, as its path ends.
    pub type_name: String,
    /// Its components, in property order; `None` when the type is unknown, which leaves every
    /// component after it unnamed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub components: Option<Vec<(VmComponentKind, String)>>,
}

/// A data set's components by kind, named as `Particles.Color.G`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct VmDataSet {
    pub name: String,
    pub floats: Vec<String>,
    pub ints: Vec<String>,
    pub halves: Vec<String>,
    /// Whether an unknown type cut the naming short, so later components go by number.
    pub partial: bool,
}

impl VmDataSet {
    fn from_attributes(name: &str, prefix: &str, attributes: &[VmAttribute]) -> Self {
        let mut set = VmDataSet {
            name: name.to_string(),
            ..Default::default()
        };
        for attribute in attributes {
            let Some(components) = &attribute.components else {
                set.partial = true;
                break;
            };
            for (kind, suffix) in components {
                let full = if suffix.is_empty() {
                    format!("{prefix}{}", attribute.name)
                } else {
                    format!("{prefix}{}.{suffix}", attribute.name)
                };
                match kind {
                    VmComponentKind::Float => set.floats.push(full),
                    VmComponentKind::Int => set.ints.push(full),
                    VmComponentKind::Half => set.halves.push(full),
                }
            }
        }
        set
    }

    /// The name of component `comp` of `kind`, or its number where the names run out.
    pub fn component(&self, kind: VmComponentKind, comp: u16) -> String {
        let (list, word) = match kind {
            VmComponentKind::Float => (&self.floats, "float"),
            VmComponentKind::Int => (&self.ints, "int"),
            VmComponentKind::Half => (&self.halves, "half"),
        };
        list.get(comp as usize)
            .cloned()
            .unwrap_or_else(|| format!("{} {word}[{comp}]", self.name))
    }

    /// Whether `comp` of `kind` is past every component the set's attributes add up to, which
    /// can only be told when every type was known.
    pub fn out_of_range(&self, kind: VmComponentKind, comp: u16) -> bool {
        let len = match kind {
            VmComponentKind::Float => self.floats.len(),
            VmComponentKind::Int => self.ints.len(),
            VmComponentKind::Half => self.halves.len(),
        };
        !self.partial && comp as usize >= len
    }
}

/// An external function a script calls, one per call site.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VmFunction {
    pub name: String,
    /// The data interface it belongs to, such as `Emitter.Scale Alpha.FloatCurve`.
    pub owner: String,
    pub inputs: u16,
    pub outputs: u16,
}

/// A data parameter in the script's own parameter store, with where it sits in the external
/// table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VmParam {
    pub name: String,
    pub type_name: String,
    pub offset: u32,
    pub size: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub components: Option<Vec<(VmComponentKind, String)>>,
}

/// A cooked script's VM program and everything that names what it reads and writes.
#[derive(Clone, Debug, Serialize)]
pub struct VmScript {
    pub export: u32,
    pub object_name: String,
    /// Its `ENiagaraScriptUsage`, by name.
    pub usage: String,
    /// Whether it runs as an interpolated spawn script, reading previous-frame tables too.
    pub interpolated: bool,
    /// The size of its parameter store's current-frame data.
    pub param_size: u32,
    /// How many bytes its parameter store holds: the current frame's, and the previous frame's
    /// too for an interpolated spawn script.
    pub param_data_len: u32,
    pub bytecode_len: u32,
    /// The bytecode is stored compressed, which the reader does not undo.
    pub compressed: bool,
    /// It carries frozen data for the experimental VM.
    pub experimental: bool,
    pub temp_registers: i64,
    #[serde(skip)]
    pub bytecode: Vec<u8>,
    pub literals: Vec<u8>,
    pub functions: Vec<VmFunction>,
    pub attributes: Vec<VmAttribute>,
    /// Data set 0 (the particles, or the system instance) and then each event data set it writes.
    pub data_sets: Vec<VmDataSet>,
    pub params: Vec<VmParam>,
    /// `None` when the CPU VM never runs a script of this usage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout: Option<VmConstLayout>,
    pub program: VmProgram,
}

/// `ENiagaraScriptUsage`'s enumerators in order, for a usage stored as a bare number.
const USAGES: [&str; 13] = [
    "Function",
    "Module",
    "DynamicInput",
    "ParticleSpawnScript",
    "ParticleSpawnScriptInterpolated",
    "ParticleUpdateScript",
    "ParticleEventScript",
    "ParticleSimulationStageScript",
    "ParticleGPUComputeScript",
    "EmitterSpawnScript",
    "EmitterUpdateScript",
    "SystemSpawnScript",
    "SystemUpdateScript",
];

fn fields_of(value: &PropertyValue) -> &[PropertyEntry] {
    match value {
        PropertyValue::Struct { fields, .. }
        | PropertyValue::Default { fields, .. }
        | PropertyValue::Unset { fields, .. } => fields,
        _ => &[],
    }
}

fn field<'a>(fields: &'a [PropertyEntry], name: &str) -> Option<&'a PropertyValue> {
    fields
        .iter()
        .find(|entry| entry.name == name)
        .map(|entry| &entry.value)
}

fn at_path<'a>(fields: &'a [PropertyEntry], path: &[&str]) -> Option<&'a PropertyValue> {
    let (last, parents) = path.split_last()?;
    let mut fields = fields;
    for name in parents {
        fields = fields_of(field(fields, name)?);
    }
    field(fields, last)
}

fn items_of(value: Option<&PropertyValue>) -> &[PropertyValue] {
    match value {
        Some(PropertyValue::Array { items } | PropertyValue::Set { items }) => items,
        _ => &[],
    }
}

fn bytes_of(value: Option<&PropertyValue>) -> Vec<u8> {
    items_of(value)
        .iter()
        .map(|item| match item {
            PropertyValue::Byte { value } => *value,
            _ => 0,
        })
        .collect()
}

fn int_of(value: Option<&PropertyValue>) -> Option<i64> {
    match value? {
        PropertyValue::Int { value } | PropertyValue::Enum { value, .. } => Some(*value),
        PropertyValue::UInt { value } => i64::try_from(*value).ok(),
        PropertyValue::Byte { value } => Some(i64::from(*value)),
        PropertyValue::Bool { value } => Some(i64::from(*value)),
        PropertyValue::Default { .. } => Some(0),
        _ => None,
    }
}

fn bool_of(value: Option<&PropertyValue>) -> bool {
    matches!(value, Some(PropertyValue::Bool { value: true }))
}

fn name_of(value: Option<&PropertyValue>) -> String {
    match value {
        Some(PropertyValue::Name { value } | PropertyValue::Str { value }) => value.clone(),
        _ => String::new(),
    }
}

/// A type definition's struct or enum, by its path's last part, and its underlying type.
fn type_of(value: Option<&PropertyValue>) -> (String, u64) {
    let fields = value.map(fields_of).unwrap_or(&[]);
    let leaf = match field(fields, "ClassStructOrEnum") {
        Some(PropertyValue::Object {
            path: Some(path), ..
        }) => path.rsplit(['.', '/']).next().unwrap_or(path).to_string(),
        _ => String::new(),
    };
    let underlying = int_of(field(fields, "UnderlyingType")).unwrap_or(0) as u64;
    (leaf, underlying)
}

fn attribute_of(value: &PropertyValue, mappings: Option<&Mappings>) -> VmAttribute {
    let fields = fields_of(value);
    let (type_name, underlying) = type_of(field(fields, "TypeDefHandle"));
    VmAttribute {
        name: name_of(field(fields, "Name")),
        components: components_of(&type_name, underlying, mappings),
        type_name,
    }
}

/// Reads the VM program `export` (a `NiagaraScript`) carries, with what names its constants and
/// outputs. Values the export does not store read as empty, zero or false. `mappings` lets a
/// struct type the known list leaves out be counted from its schema.
pub fn read_vm_script(export: &ParsedExport, mappings: Option<&Mappings>) -> VmScript {
    let props = &export.properties;
    let vm = field(props, "CachedScriptVM").map(fields_of).unwrap_or(&[]);
    let usage = match field(props, "Usage") {
        Some(PropertyValue::Enum {
            name: Some(name), ..
        }) => name.clone(),
        other => int_of(other)
            .and_then(|value| USAGES.get(usize::try_from(value).ok()?).copied())
            .unwrap_or("Function")
            .to_string(),
    };
    let interpolated = bool_of(at_path(props, &["CachedScriptVMId", "bInterpolatedSpawn"]));
    let store = field(props, "ScriptExecutionParamStore")
        .map(fields_of)
        .unwrap_or(&[]);
    let param_size = int_of(field(store, "ParameterSize")).unwrap_or(0).max(0) as u32;
    let bytecode = bytes_of(at_path(vm, &["ByteCode", "Data"]));
    let compressed = matches!(
        at_path(vm, &["ByteCode", "UncompressedSize"]),
        Some(PropertyValue::Int { value }) if *value != -1
    );
    let literals = bytes_of(field(vm, "ScriptLiterals"));
    let functions: Vec<VmFunction> = items_of(field(vm, "CalledVMExternalFunctions"))
        .iter()
        .map(|item| {
            let fields = fields_of(item);
            VmFunction {
                name: name_of(field(fields, "Name")),
                owner: name_of(field(fields, "OwnerName")),
                inputs: items_of(field(fields, "InputParamLocations")).len() as u16,
                outputs: int_of(field(fields, "NumOutputs")).unwrap_or(0).max(0) as u16,
            }
        })
        .collect();
    let attributes: Vec<VmAttribute> = items_of(field(vm, "Attributes"))
        .iter()
        .map(|item| attribute_of(item, mappings))
        .collect();
    let particle = usage.starts_with("Particle");
    let mut data_sets = vec![VmDataSet::from_attributes(
        if particle { "Particles" } else { "Instance" },
        if particle { "Particles." } else { "" },
        &attributes,
    )];
    for item in items_of(field(vm, "WriteDataSets")) {
        let fields = fields_of(item);
        let name = name_of(at_path(fields, &["ID", "Name"]));
        let variables: Vec<VmAttribute> = items_of(field(fields, "Variables"))
            .iter()
            .map(|item| attribute_of(item, mappings))
            .collect();
        data_sets.push(VmDataSet::from_attributes(
            &name,
            &format!("{name}."),
            &variables,
        ));
    }
    let mut params: Vec<VmParam> = items_of(field(store, "SortedParameterOffsets"))
        .iter()
        .filter_map(|item| {
            let fields = fields_of(item);
            let (type_name, underlying) = type_of(field(fields, "TypeDefHandle"));
            // A data interface or object parameter indexes its own list, not these bytes.
            if underlying == 1 {
                return None;
            }
            Some(VmParam {
                name: name_of(field(fields, "Name")),
                offset: int_of(field(fields, "Offset"))?.max(0) as u32,
                size: 0,
                components: components_of(&type_name, underlying, mappings),
                type_name,
            })
        })
        .collect();
    params.sort_by_key(|param| param.offset);
    // The store is packed, so each value runs up to the next.
    let data_len = bytes_of(field(store, "ParameterData")).len() as u32;
    let ends: Vec<u32> = params
        .iter()
        .skip(1)
        .map(|param| param.offset)
        .chain([data_len.max(param_size)])
        .collect();
    for (param, end) in params.iter_mut().zip(ends) {
        param.size = end.saturating_sub(param.offset);
    }
    let calls: Vec<(u16, u16)> = functions.iter().map(|f| (f.inputs, f.outputs)).collect();
    let program = if compressed {
        VmProgram {
            instructions: Vec::new(),
            stop: Some(VmStop {
                at: 0,
                reason: "the bytecode is stored compressed".to_string(),
            }),
        }
    } else {
        decode_vm(&bytecode, &calls)
    };
    VmScript {
        export: export.index,
        object_name: export.object_name.clone(),
        layout: const_layout(&usage, param_size, interpolated, literals.len() as u32),
        usage,
        interpolated,
        param_size,
        param_data_len: data_len,
        bytecode_len: bytecode.len() as u32,
        compressed,
        experimental: !items_of(field(vm, "ExperimentalContextData")).is_empty(),
        temp_registers: int_of(field(vm, "NumTempRegisters")).unwrap_or(0),
        bytecode,
        literals,
        functions,
        attributes,
        data_sets,
        params,
        program,
    }
}

impl VmScript {
    /// What the constant at byte `offset` reads, or `None` past every table.
    pub fn resolve(&self, offset: u16) -> Option<VmConst> {
        let (table, within) = self.layout.as_ref()?.locate(u32::from(offset))?;
        let slot = |bytes: &[u8], at: u32| {
            let at = at as usize;
            bytes
                .get(at..at + 4)
                .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        };
        let (name, bits) = match table {
            VmTable::Literals => (None, slot(&self.literals, within)),
            VmTable::External | VmTable::PrevExternal => {
                let into = if table == VmTable::PrevExternal {
                    within + self.param_size
                } else {
                    within
                };
                (self.param_name(into), None)
            }
            table => {
                let name = engine_slot_name(table, within / 4);
                let prev = matches!(
                    table,
                    VmTable::PrevGlobal
                        | VmTable::PrevSystem
                        | VmTable::PrevOwner
                        | VmTable::PrevEmitter
                );
                (
                    name.map(|name| if prev { format!("PREV {name}") } else { name }),
                    None,
                )
            }
        };
        let key = match bits.filter(|_| table == VmTable::Literals) {
            Some(bits) => format!("lit+{within}=0x{bits:08X}"),
            None => format!("{}+{within}", table.key()),
        };
        Some(VmConst {
            offset,
            table,
            within,
            name,
            bits,
            key,
        })
    }

    /// The parameter at byte `offset` of the parameter store, with its component.
    fn param_name(&self, offset: u32) -> Option<String> {
        let param =
            self.params.iter().rev().find(|param| {
                param.offset <= offset && offset < param.offset + param.size.max(4)
            })?;
        let within = offset - param.offset;
        if param.size <= 4 {
            return Some(param.name.clone());
        }
        let mut at = 0;
        for (kind, suffix) in param.components.iter().flatten() {
            let size = if *kind == VmComponentKind::Half { 2 } else { 4 };
            if within < at + size {
                return Some(format!("{}.{suffix}", param.name));
            }
            at += size;
        }
        Some(format!("{}[+{within}]", param.name))
    }

    /// Where each literal is read: by its offset within the literal table, the instructions and
    /// operands that read it.
    pub fn literal_uses(&self) -> BTreeMap<u32, Vec<(u32, usize)>> {
        let mut uses: BTreeMap<u32, Vec<(u32, usize)>> = BTreeMap::new();
        let Some(layout) = &self.layout else {
            return uses;
        };
        for instruction in &self.program.instructions {
            for (index, operand) in instruction.operands.iter().enumerate() {
                if let VmLoc::Constant(offset) = operand.loc
                    && let Some((VmTable::Literals, within)) = layout.locate(u32::from(offset))
                {
                    uses.entry(within)
                        .or_default()
                        .push((instruction.at, index));
                }
            }
        }
        uses
    }

    /// The data set component an output or input reaches, by name.
    pub fn component_name(&self, op: u8, data_set: u16, comp: u16) -> String {
        let kind = match op {
            78 | 81 | 84 => VmComponentKind::Int,
            79 | 82 | 85 => VmComponentKind::Half,
            _ => VmComponentKind::Float,
        };
        match self.data_sets.get(data_set as usize) {
            Some(set) => set.component(kind, comp),
            None => format!("ds{data_set} {comp}"),
        }
    }
}

/// How a 4-byte constant reads as `ty`.
pub fn show_bits(bits: u32, ty: VmType) -> String {
    match ty {
        VmType::Float => format!("{:?}", f32::from_bits(bits)),
        VmType::Int => (bits as i32).to_string(),
        VmType::Any => format!("0x{bits:08X}"),
    }
}

/// One instruction as `asset vm` prints it, with what its constants read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VmLine {
    pub at: u32,
    pub text: String,
    /// Each constant operand, by its place among the instruction's operands.
    pub constants: Vec<(usize, VmConst)>,
    /// The data set component an output writes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub writes: Option<String>,
}

/// The script's instructions as `asset vm` lists them.
pub fn vm_lines(script: &VmScript) -> Vec<VmLine> {
    let uses = script.literal_uses();
    script
        .program
        .instructions
        .iter()
        .map(|instruction| line_of(script, instruction, &uses))
        .collect()
}

fn line_of(
    script: &VmScript,
    instruction: &VmInstruction,
    uses: &BTreeMap<u32, Vec<(u32, usize)>>,
) -> VmLine {
    let mut constants = Vec::new();
    let mut show = |index: usize, operand: &VmOperand| -> String {
        match operand.loc {
            VmLoc::Register(reg) => format!("r{reg}"),
            VmLoc::Index(value) => value.to_string(),
            VmLoc::Unused => "_".to_string(),
            VmLoc::Constant(offset) => match script.resolve(offset) {
                Some(constant) => {
                    let text = match (&constant.name, constant.bits) {
                        (Some(name), _) => name.clone(),
                        (None, Some(bits)) if constant.table == VmTable::Literals => {
                            let count = uses.get(&constant.within).map_or(0, Vec::len);
                            let shared = if count > 1 {
                                format!(", {count} uses")
                            } else {
                                String::new()
                            };
                            format!(
                                "{} (lit+{}{shared})",
                                show_bits(bits, operand.ty),
                                constant.within
                            )
                        }
                        _ => constant.key.clone(),
                    };
                    constants.push((index, constant));
                    text
                }
                None => format!("#{offset}?"),
            },
        }
    };
    let ops: Vec<String> = instruction
        .operands
        .iter()
        .enumerate()
        .map(|(index, operand)| show(index, operand))
        .collect();
    let mut writes = None;
    let body = match instruction.shape {
        VmShape::Unary | VmShape::Binary | VmShape::Ternary => {
            let (sources, dest) = ops.split_at(ops.len() - 1);
            format!("{} -> {}", sources.join(", "), dest[0])
        }
        VmShape::ExecIndex => format!("-> {}", ops[0]),
        VmShape::Input => {
            let data_set = instruction.operands[0].raw;
            let comp = instruction.operands[1].raw;
            format!(
                "{} -> {}",
                script.component_name(instruction.op, data_set, comp),
                ops[2]
            )
        }
        VmShape::Output => {
            let data_set = instruction.operands[0].raw;
            let comp = instruction.operands[3].raw;
            let name = script.component_name(instruction.op, data_set, comp);
            writes = Some(name.clone());
            format!("{} -> {name} [row {}]", ops[2], ops[1])
        }
        VmShape::AcquireIndex => format!("ds{}, {} -> {}", ops[0], ops[1], ops[2]),
        VmShape::ExternalCall => {
            let function = instruction.function.unwrap_or(0);
            let named = script.functions.get(function as usize);
            let (inputs, outputs): (Vec<_>, Vec<_>) = instruction
                .operands
                .iter()
                .zip(&ops)
                .partition(|(operand, _)| matches!(operand.role, VmRole::CallIn(_)));
            let join = |list: Vec<(&VmOperand, &String)>| {
                list.into_iter()
                    .map(|(_, text)| text.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            format!(
                "{}.{}({}) -> {}",
                named.map_or("?", |f| f.owner.as_str()),
                named.map_or("?", |f| f.name.as_str()),
                join(inputs),
                join(outputs)
            )
        }
        VmShape::IdRw if instruction.op == 94 => format!("ds{} -> {}, {}", ops[0], ops[1], ops[2]),
        VmShape::IdRw => format!("ds{}, {}, {}", ops[0], ops[1], ops[2]),
        VmShape::EnterStat => ops[0].clone(),
        VmShape::ExitStat | VmShape::Done => String::new(),
    };
    let text = if body.is_empty() {
        instruction.name.to_string()
    } else {
        format!("{:<16} {body}", instruction.name)
    };
    VmLine {
        at: instruction.at,
        text,
        constants,
        writes,
    }
}

/// Something an output's value comes from that is not computed in the script itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VmLeaf {
    /// A constant operand: a literal, an engine value or a parameter.
    Constant {
        /// The instruction and the operand that read it, as `asset edit-vm` names them.
        at: u32,
        operand: usize,
        constant: VmConst,
        ty: VmType,
        /// How many operands in the script read the same literal.
        uses: usize,
        /// Every output the instruction reading it reaches, so a change to it changes those.
        feeds: Vec<String>,
    },
    /// A value read from a data set, such as the particle's own age.
    Input { at: u32, name: String },
    /// An output of an external function, such as a curve's sample.
    Call {
        at: u32,
        function: String,
        output: u8,
    },
    /// The instance's index, id or row, which the VM supplies.
    Index { at: u32, what: String },
    /// A register nothing wrote before it was read.
    Unwritten { register: u16 },
}

/// What one output of a script is computed from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VmTrace {
    /// The output instruction.
    pub at: u32,
    pub writes: String,
    /// The value as one expression, cut short where it nests deep.
    pub expression: String,
    pub leaves: Vec<VmLeaf>,
    /// Every leaf is a constant the script holds: the value is baked, not computed at run time.
    pub baked: bool,
}

/// Where an instruction's operand gets its value.
#[derive(Clone, Copy, Debug)]
enum Source {
    Constant(usize),
    /// The instruction that last wrote the register, and which of its outputs it was.
    Instruction(usize, u8),
    Unwritten(u16),
}

/// Each instruction's sources, by operand, in the program order the VM runs them.
fn sources_of(script: &VmScript) -> Vec<Vec<(usize, Source)>> {
    let mut writer: BTreeMap<u16, (usize, u8)> = BTreeMap::new();
    let mut all = Vec::with_capacity(script.program.instructions.len());
    for (index, instruction) in script.program.instructions.iter().enumerate() {
        let mut sources = Vec::new();
        for (k, operand) in instruction.operands.iter().enumerate() {
            let reads = matches!(
                operand.role,
                VmRole::Source(_) | VmRole::Valid | VmRole::CallIn(_)
            );
            if !reads {
                continue;
            }
            sources.push((
                k,
                match operand.loc {
                    VmLoc::Constant(_) => Source::Constant(k),
                    VmLoc::Register(reg) => match writer.get(&reg) {
                        Some(&(by, output)) => Source::Instruction(by, output),
                        None => Source::Unwritten(reg),
                    },
                    VmLoc::Index(_) | VmLoc::Unused => continue,
                },
            ));
        }
        for operand in &instruction.operands {
            match (operand.loc, operand.role) {
                (VmLoc::Register(reg), VmRole::Dest) => {
                    writer.insert(reg, (index, 0));
                }
                (VmLoc::Register(reg), VmRole::CallOut(k)) => {
                    writer.insert(reg, (index, k));
                }
                _ => {}
            }
        }
        all.push(sources);
    }
    all
}

/// What each output of `script` is computed from: its expression, and the constants, inputs and
/// calls at the bottom of it.
pub fn trace_outputs(script: &VmScript) -> Vec<VmTrace> {
    let sources = sources_of(script);
    let instructions = &script.program.instructions;
    let uses = script.literal_uses();
    // Which instructions each output depends on, to say what a constant feeds.
    let mut reaches: Vec<(usize, String, Vec<bool>)> = Vec::new();
    for (index, instruction) in instructions.iter().enumerate() {
        if instruction.shape != VmShape::Output {
            continue;
        }
        let mut seen = vec![false; instructions.len()];
        let mut stack = vec![index];
        while let Some(at) = stack.pop() {
            if std::mem::replace(&mut seen[at], true) {
                continue;
            }
            for (k, source) in &sources[at] {
                // An output's row index says where it writes, not what.
                if at == index && instruction.operands[*k].role == VmRole::IndexReg {
                    continue;
                }
                if let Source::Instruction(by, _) = source {
                    stack.push(*by);
                }
            }
        }
        let writes = script.component_name(
            instruction.op,
            instruction.operands[0].raw,
            instruction.operands[3].raw,
        );
        reaches.push((index, writes, seen));
    }
    reaches
        .iter()
        .map(|(index, writes, seen)| {
            let instruction = &instructions[*index];
            let mut leaves = Vec::new();
            let mut listed = std::collections::BTreeSet::new();
            for (at, flag) in seen.iter().enumerate() {
                if !flag {
                    continue;
                }
                let node = &instructions[at];
                let leaf = match node.shape {
                    VmShape::Input => Some(VmLeaf::Input {
                        at: node.at,
                        name: script.component_name(
                            node.op,
                            node.operands[0].raw,
                            node.operands[1].raw,
                        ),
                    }),
                    VmShape::ExternalCall => {
                        let function = script.functions.get(node.function.unwrap_or(0) as usize);
                        Some(VmLeaf::Call {
                            at: node.at,
                            function: function.map_or_else(
                                || "?".to_string(),
                                |f| format!("{}.{}", f.owner, f.name),
                            ),
                            output: 0,
                        })
                    }
                    VmShape::ExecIndex | VmShape::AcquireIndex | VmShape::IdRw => {
                        Some(VmLeaf::Index {
                            at: node.at,
                            what: node.name.to_string(),
                        })
                    }
                    _ => None,
                };
                if let Some(leaf) = leaf {
                    leaves.push(leaf);
                }
                for (k, source) in &sources[at] {
                    match source {
                        Source::Constant(k)
                            if at != *index || node.operands[*k].role != VmRole::IndexReg =>
                        {
                            let operand = &node.operands[*k];
                            let VmLoc::Constant(offset) = operand.loc else {
                                continue;
                            };
                            let Some(constant) = script.resolve(offset) else {
                                continue;
                            };
                            if !listed.insert((at, *k)) {
                                continue;
                            }
                            let feeds = reaches
                                .iter()
                                .filter(|(_, _, reach)| reach[at])
                                .map(|(_, name, _)| name.clone())
                                .collect();
                            leaves.push(VmLeaf::Constant {
                                at: node.at,
                                operand: *k,
                                uses: uses
                                    .get(&constant.within)
                                    .filter(|_| constant.table == VmTable::Literals)
                                    .map_or(1, Vec::len),
                                constant,
                                ty: operand.ty,
                                feeds,
                            });
                        }
                        Source::Unwritten(register) => {
                            leaves.push(VmLeaf::Unwritten {
                                register: *register,
                            });
                        }
                        _ => {
                            let _ = k;
                        }
                    }
                }
            }
            let baked = leaves.iter().all(|leaf| match leaf {
                VmLeaf::Constant { constant, .. } => constant.table == VmTable::Literals,
                _ => false,
            });
            VmTrace {
                at: instruction.at,
                writes: writes.clone(),
                expression: expression(script, &sources, *index, 0),
                leaves,
                baked,
            }
        })
        .collect()
}

/// The value instruction `index` computes, written out, down to `MAX_DEPTH` calls deep.
fn expression(
    script: &VmScript,
    sources: &[Vec<(usize, Source)>],
    index: usize,
    depth: u32,
) -> String {
    const MAX_DEPTH: u32 = 6;
    let instruction = &script.program.instructions[index];
    if depth > MAX_DEPTH {
        return "…".to_string();
    }
    let operand_text = |k: usize, source: &Source| -> String {
        match source {
            Source::Constant(_) => {
                let operand = &instruction.operands[k];
                let VmLoc::Constant(offset) = operand.loc else {
                    return "?".to_string();
                };
                match script.resolve(offset) {
                    Some(VmConst {
                        name: Some(name), ..
                    }) => name,
                    Some(VmConst {
                        bits: Some(bits), ..
                    }) => show_bits(bits, operand.ty),
                    Some(constant) => constant.key,
                    None => format!("#{offset}"),
                }
            }
            Source::Instruction(by, output) => {
                let node = &script.program.instructions[*by];
                if node.shape == VmShape::ExternalCall {
                    let inner = expression(script, sources, *by, depth + 1);
                    if *output == 0 {
                        inner
                    } else {
                        format!("{inner}.out{output}")
                    }
                } else {
                    expression(script, sources, *by, depth + 1)
                }
            }
            Source::Unwritten(reg) => format!("r{reg}?"),
        }
    };
    let args: Vec<String> = sources[index]
        .iter()
        .filter(|(k, _)| instruction.operands[*k].role != VmRole::IndexReg)
        .map(|(k, source)| operand_text(*k, source))
        .collect();
    match instruction.shape {
        VmShape::Output => args.join(", "),
        VmShape::Input => script.component_name(
            instruction.op,
            instruction.operands[0].raw,
            instruction.operands[1].raw,
        ),
        VmShape::ExternalCall => {
            let function = script
                .functions
                .get(instruction.function.unwrap_or(0) as usize);
            format!(
                "{}({})",
                function.map_or_else(|| "?".to_string(), |f| format!("{}.{}", f.owner, f.name)),
                args.join(", ")
            )
        }
        VmShape::ExecIndex | VmShape::AcquireIndex | VmShape::IdRw => instruction.name.to_string(),
        _ => format!("{}({})", instruction.name, args.join(", ")),
    }
}

/// What a walk over Niagara scripts found: how many there are by usage, which opcodes they use,
/// and every place the reader's model of them does not hold. A line starting `bad:` should never
/// count anything; one that does names a script in `examples`.
#[derive(Clone, Debug, Default, Serialize)]
pub struct VmCensus {
    pub scripts: usize,
    pub counts: BTreeMap<String, usize>,
    pub ops: BTreeMap<String, usize>,
    /// For each line, where it was found, as `(line, detail)`.
    pub examples: Vec<(String, String)>,
}

impl VmCensus {
    fn note(&mut self, line: &str, detail: Option<String>) {
        *self.counts.entry(line.to_string()).or_default() += 1;
        if let Some(detail) = detail {
            self.examples.push((line.to_string(), detail));
        }
    }
}

/// Reads every Niagara script in `parsed` and checks it against what the run time does with it:
/// that the bytecode decodes to its `done` and encodes back byte for byte, that every constant
/// falls inside the tables its usage gives it and names a parameter where it reads the parameter
/// store, that every output names a component its data set has, and that no register is read
/// before it is written.
pub fn vm_census(parsed: &ParsedPackage, mappings: Option<&Mappings>) -> VmCensus {
    let mut census = VmCensus::default();
    for export in parsed
        .exports
        .iter()
        .filter(|export| export.class_name == "NiagaraScript")
    {
        let script = read_vm_script(export, mappings);
        census.scripts += 1;
        census_of(&script, &export.path, &mut census);
    }
    census
}

fn census_of(script: &VmScript, path: &str, census: &mut VmCensus) {
    let at = |offset: u32, detail: &str| format!("{path} at 0x{offset:04X}: {detail}");
    if script.bytecode_len == 0 {
        census.note(&format!("scripts with no bytecode, {}", script.usage), None);
        return;
    }
    census.note(&format!("scripts with bytecode, {}", script.usage), None);
    if script.compressed {
        census.note("scripts with compressed bytecode", Some(path.to_string()));
        return;
    }
    if script.experimental {
        census.note("scripts with experimental VM data", Some(path.to_string()));
    }
    for set in &script.data_sets {
        if set.partial {
            let unknown: Vec<&str> = script
                .attributes
                .iter()
                .filter(|a| a.components.is_none())
                .map(|a| a.type_name.as_str())
                .collect();
            census.note(
                "data sets with an attribute of unknown type",
                Some(format!("{path}: {}", unknown.join(", "))),
            );
        }
    }
    if let Some(stop) = &script.program.stop {
        census.note("bad: decode stopped", Some(at(stop.at, &stop.reason)));
        return;
    }
    if encode_vm(&script.program) != script.bytecode {
        census.note("bad: re-encoded bytecode differs", Some(path.to_string()));
    }
    for instruction in &script.program.instructions {
        *census.ops.entry(instruction.name.to_string()).or_default() += 1;
    }
    let Some(layout) = &script.layout else {
        // Compiled for a stage the CPU VM never runs, such as a simulation stage.
        census.note(
            &format!("bytecode the CPU VM never runs, {}", script.usage),
            Some(path.to_string()),
        );
        return;
    };
    if script.interpolated != (script.usage == "ParticleSpawnScriptInterpolated") {
        census.note(
            "bad: interpolated spawn flag disagrees with the usage",
            Some(format!("{path}: {}", script.usage)),
        );
    }
    let frames = if script.interpolated { 2 } else { 1 };
    if script.param_data_len != script.param_size * frames {
        census.note(
            "bad: parameter data is not the parameter size per frame",
            Some(format!(
                "{path}: {} bytes for {} x {frames}",
                script.param_data_len, script.param_size
            )),
        );
    }
    if !script.literals.len().is_multiple_of(4) {
        census.note(
            "bad: literal table not a multiple of 4 bytes",
            Some(path.to_string()),
        );
    }
    let uses = script.literal_uses();
    let unread = (0..script.literals.len() as u32 / 4)
        .filter(|slot| !uses.contains_key(&(slot * 4)))
        .count();
    for _ in 0..unread {
        census.note("literals no operand reads", None);
    }
    let mut written = vec![false; 1 << 16];
    for instruction in &script.program.instructions {
        for operand in &instruction.operands {
            match operand.loc {
                VmLoc::Constant(offset) => {
                    let Some((table, within)) = layout.locate(u32::from(offset)) else {
                        census.note(
                            "bad: constant past the tables",
                            Some(at(
                                instruction.at,
                                &format!("{offset} past {}", layout.end()),
                            )),
                        );
                        continue;
                    };
                    match table {
                        VmTable::Literals if !within.is_multiple_of(4) => census.note(
                            "bad: literal read off a 4-byte boundary",
                            Some(at(instruction.at, &format!("lit+{within}"))),
                        ),
                        VmTable::External | VmTable::PrevExternal
                            if script.resolve(offset).and_then(|c| c.name).is_none() =>
                        {
                            census.note(
                                "bad: parameter store read names no parameter",
                                Some(at(instruction.at, &format!("{}+{within}", table.key()))),
                            )
                        }
                        _ => {}
                    }
                }
                VmLoc::Register(reg)
                    if matches!(
                        operand.role,
                        VmRole::Source(_) | VmRole::IndexReg | VmRole::Valid | VmRole::CallIn(_)
                    ) && !written[reg as usize] =>
                {
                    census.note(
                        "bad: register read before it is written",
                        Some(at(instruction.at, &format!("r{reg}"))),
                    );
                }
                _ => {}
            }
        }
        for operand in &instruction.operands {
            if let (VmLoc::Register(reg), VmRole::Dest | VmRole::CallOut(_)) =
                (operand.loc, operand.role)
            {
                written[reg as usize] = true;
            }
        }
        if instruction.shape == VmShape::Output || instruction.shape == VmShape::Input {
            let (set, comp) = if instruction.shape == VmShape::Output {
                (instruction.operands[0].raw, instruction.operands[3].raw)
            } else {
                (instruction.operands[0].raw, instruction.operands[1].raw)
            };
            let kind = match instruction.op {
                78 | 81 | 84 => VmComponentKind::Int,
                79 | 82 | 85 => VmComponentKind::Half,
                _ => VmComponentKind::Float,
            };
            let what = if instruction.shape == VmShape::Output {
                "output"
            } else {
                "input"
            };
            match script.data_sets.get(set as usize) {
                // An input from another data set reads an event the script does not describe.
                None if instruction.shape == VmShape::Input => census.note(
                    "inputs from a data set the script does not list",
                    Some(at(instruction.at, &format!("ds{set}"))),
                ),
                None => census.note(
                    "bad: output to a data set the script does not list",
                    Some(at(instruction.at, &format!("ds{set}"))),
                ),
                Some(data) if data.out_of_range(kind, comp) => census.note(
                    &format!("bad: {what} past its data set's attributes"),
                    Some(at(instruction.at, &format!("ds{set} {kind:?} {comp}"))),
                ),
                Some(_) => {}
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn le(words: &[u16]) -> Vec<u8> {
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    fn program(parts: &[&[u8]]) -> Vec<u8> {
        parts.concat()
    }

    /// Each shape reads the bytes its kernel does, and the whole program reads back exactly.
    #[test]
    fn every_shape_decodes_to_its_size_and_encodes_back() {
        let bytes = program(&[
            &[91],
            &le(&[1344]), // enter_stat_scope #1344
            &[94],
            &le(&[0, 0, 1]), // acquire_id ds0 -> r0, r1
            &[88],
            &le(&[2]), // exec_index -> r2
            &[44, 1],
            &le(&[700, 2, 3]), // addi #700, r2 -> r3
            &[64, 0],
            &le(&[3, 4]), // bit_not r3 -> r4 (unary: 6 bytes)
            &[36, 0],
            &le(&[5, 6]), // noise r5 -> r6
            &[89, 3],
            &le(&[700, 704, 7]), // noise2D #, # -> r7
            &[90, 5],
            &le(&[700, 6, 704, 8]), // noise3D #, r6, # -> r8
            &[43, 6],
            &le(&[4, 700, 704, 9]), // select r4, #, # -> r9
            &[77],
            &le(&[0, 3, 10]), // inputdata_float ds0 comp3 -> r10
            &[86, 1],
            &le(&[0, 700, 11]), // acquireindex ds0, #700 -> r11
            &[87, 0],
            &le(&[0x8000 | 10, 704, 12, 0xFFFF]), // call f0(r10, #704) -> r12, _
            &[83, 1],
            &le(&[0, 11, 704, 5]), // outputdata_float #704 -> comp 5, row r11
            &[93],
            &le(&[0, 0, 1]), // update_id ds0, r0, r1
            &[92],
            &[0],
        ]);
        let decoded = decode_vm(&bytes, &[(2, 2)]);
        assert!(decoded.complete(), "{:?}", decoded.stop);
        let lens: Vec<u32> = decoded.instructions.iter().map(|i| i.len).collect();
        assert_eq!(lens, [3, 7, 3, 8, 6, 6, 8, 10, 10, 7, 8, 10, 10, 7, 1, 1]);
        assert_eq!(encode_vm(&decoded), bytes);
        let call = &decoded.instructions[11];
        assert_eq!(
            call.operands.iter().map(|o| o.loc).collect::<Vec<_>>(),
            [
                VmLoc::Register(10),
                VmLoc::Constant(704),
                VmLoc::Register(12),
                VmLoc::Unused
            ]
        );
        let output = &decoded.instructions[12];
        assert_eq!(output.operands[2].loc, VmLoc::Constant(704));
        assert_eq!(output.operands[2].at, output.at + 6);
        assert_eq!(decoded.instructions[4].name, "bit_not");
    }

    /// Bytecode that is not what the VM runs stops the decode where it goes wrong.
    #[test]
    fn a_decode_stops_at_what_the_vm_would_refuse() {
        let stop = |bytes: &[u8]| decode_vm(bytes, &[(1, 1)]).stop.expect("stops");
        assert!(stop(&[95, 0, 0, 0, 0, 0]).reason.contains("half_to_float"));
        assert!(
            stop(&[120])
                .reason
                .contains("not one legacy bytecode holds")
        );
        assert!(
            stop(&[7, 2, 0, 0, 1, 0, 0])
                .reason
                .contains("constant flags")
        );
        assert!(
            stop(&[1, 4, 0, 0, 0, 0, 0, 0, 0])
                .reason
                .contains("constant flags")
        );
        assert!(stop(&[1, 0, 0, 0]).reason.contains("past the end"));
        assert!(
            stop(&[87, 3, 0, 0, 0, 0])
                .reason
                .contains("past the 1 function")
        );
        assert!(stop(&[92]).reason.contains("without `done`"));
        let trailing = stop(&[0, 0]);
        assert_eq!(
            (trailing.at, trailing.reason.as_str()),
            (1, "1 byte(s) follow `done`")
        );
        assert!(decode_vm(&[0], &[]).complete());
    }

    /// The literals start past every table the run time puts before them.
    #[test]
    fn literals_follow_the_tables_a_script_s_usage_gives_it() {
        let plain = const_layout("ParticleUpdateScript", 32, false, 16).unwrap();
        assert_eq!(plain.literal_base(), 640 + 32);
        assert_eq!(plain.locate(639), Some((VmTable::Emitter, 31)));
        assert_eq!(plain.locate(640), Some((VmTable::External, 0)));
        assert_eq!(plain.locate(671), Some((VmTable::External, 31)));
        assert_eq!(plain.locate(672), Some((VmTable::Literals, 0)));
        assert_eq!(plain.locate(688), None);
        let interpolated = const_layout("ParticleSpawnScriptInterpolated", 32, true, 144).unwrap();
        assert_eq!(interpolated.literal_base(), 1344);
        assert_eq!(interpolated.locate(672), Some((VmTable::PrevGlobal, 0)));
        assert_eq!(interpolated.locate(1343), Some((VmTable::PrevExternal, 31)));
        let system = const_layout("SystemUpdateScript", 8, false, 4).unwrap();
        assert_eq!(system.literal_base(), 40);
        assert_eq!(system.locate(32), Some((VmTable::External, 0)));
        assert!(const_layout("ParticleGPUComputeScript", 8, false, 4).is_none());
        assert!(const_layout("ParticleSimulationStageScript", 8, false, 4).is_none());
    }

    /// The engine's tables name their slots as the engine does.
    #[test]
    fn engine_tables_name_their_slots() {
        assert_eq!(
            engine_slot_name(VmTable::Global, 1).as_deref(),
            Some("Engine.DeltaTime")
        );
        assert_eq!(
            engine_slot_name(VmTable::Owner, 5).as_deref(),
            Some("Engine.Owner.SystemLocalToWorld[5]")
        );
        assert_eq!(
            engine_slot_name(VmTable::Owner, 97).as_deref(),
            Some("Engine.Owner.Rotation.Y")
        );
        assert_eq!(
            engine_slot_name(VmTable::Owner, 103).as_deref(),
            Some("Engine.Owner.Padding_0")
        );
        assert_eq!(
            engine_slot_name(VmTable::Owner, 127).as_deref(),
            Some("Engine.Owner.LWCTile.W")
        );
        assert_eq!(
            engine_slot_name(VmTable::Emitter, 3).as_deref(),
            Some("Emitter.Age")
        );
        assert_eq!(engine_slot_name(VmTable::Owner, 128), None);
    }

    fn attribute(name: &str, leaf: &str) -> VmAttribute {
        VmAttribute {
            name: name.to_string(),
            type_name: leaf.to_string(),
            components: components_of(leaf, 2, None),
        }
    }

    /// An output's component counts its own kind only, over the sorted attributes.
    fn script_of(bytes: Vec<u8>, literals: &[f32]) -> VmScript {
        let literals: Vec<u8> = literals.iter().flat_map(|v| v.to_le_bytes()).collect();
        let program = decode_vm(&bytes, &[(1, 1)]);
        assert!(program.complete(), "{:?}", program.stop);
        VmScript {
            export: 1,
            object_name: "SpawnScript".to_string(),
            usage: "ParticleSpawnScript".to_string(),
            interpolated: false,
            param_size: 4,
            param_data_len: 4,
            bytecode_len: bytes.len() as u32,
            compressed: false,
            experimental: false,
            temp_registers: 8,
            bytecode: bytes,
            layout: const_layout("ParticleSpawnScript", 4, false, literals.len() as u32),
            literals,
            functions: vec![VmFunction {
                name: "SampleCurve".to_string(),
                owner: "Emitter.Fade".to_string(),
                inputs: 1,
                outputs: 1,
            }],
            attributes: Vec::new(),
            data_sets: vec![VmDataSet::from_attributes(
                "Particles",
                "Particles.",
                &[
                    attribute("Age", "NiagaraFloat"),
                    attribute("Color", "LinearColor"),
                ],
            )],
            params: vec![VmParam {
                name: "User.IsEnemy".to_string(),
                type_name: "NiagaraFloat".to_string(),
                offset: 0,
                size: 4,
                components: Some(vec![(VmComponentKind::Float, String::new())]),
            }],
            program,
        }
    }

    /// Each output traces to the literals, parameters, inputs and calls it is computed from, and
    /// each constant says which outputs its instruction reaches.
    #[test]
    fn an_output_traces_to_the_constants_and_inputs_it_comes_from() {
        // Literals start at 644: 2.0 at +0, 0.5 at +4, 1.0 at +8.
        let bytes = program(&[
            &[86, 1],
            &le(&[0, 652, 0]), // acquireindex ds0, #1.0 -> r0
            &[77],
            &le(&[0, 0, 1]), // inputdata_float Age -> r1
            &[6, 7],
            &le(&[644, 648, 640, 2]), // lerp 2.0, 0.5, User.IsEnemy -> r2
            &[87, 0],
            &le(&[0x8000 | 1, 3]), // call Fade(r1) -> r3
            &[3, 1],
            &le(&[644, 3, 4]), // mul 2.0, r3 -> r4
            &[83, 0],
            &le(&[0, 0, 2, 1]), // Color.R = r2
            &[83, 1],
            &le(&[0, 0, 648, 2]), // Color.G = 0.5
            &[83, 0],
            &le(&[0, 0, 4, 4]), // Color.A = r4
            &[0],
        ]);
        let script = script_of(bytes, &[2.0, 0.5, 1.0]);
        let traces = trace_outputs(&script);
        let names: Vec<&str> = traces.iter().map(|t| t.writes.as_str()).collect();
        assert_eq!(
            names,
            [
                "Particles.Color.R",
                "Particles.Color.G",
                "Particles.Color.A"
            ]
        );
        let red = &traces[0];
        assert_eq!(red.expression, "lerp(2.0, 0.5, User.IsEnemy)");
        assert!(!red.baked, "a parameter is not baked");
        let constants: Vec<(u32, usize, String, usize, Vec<String>)> = red
            .leaves
            .iter()
            .filter_map(|leaf| match leaf {
                VmLeaf::Constant {
                    at,
                    operand,
                    constant,
                    uses,
                    feeds,
                    ..
                } => Some((*at, *operand, constant.key.clone(), *uses, feeds.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            constants,
            [
                (
                    15,
                    0,
                    "lit+0=0x40000000".to_string(),
                    2,
                    vec!["Particles.Color.R".to_string()]
                ),
                (
                    15,
                    1,
                    "lit+4=0x3F000000".to_string(),
                    2,
                    vec!["Particles.Color.R".to_string()]
                ),
                (
                    15,
                    2,
                    "ext+0".to_string(),
                    1,
                    vec!["Particles.Color.R".to_string()]
                ),
            ]
        );
        let green = &traces[1];
        assert!(green.baked);
        assert_eq!(green.expression, "0.5");
        let alpha = &traces[2];
        assert_eq!(
            alpha.expression,
            "mul(2.0, Emitter.Fade.SampleCurve(Particles.Age))"
        );
        assert!(
            alpha
                .leaves
                .iter()
                .any(|leaf| matches!(leaf, VmLeaf::Call { .. }))
        );
        assert!(
            alpha
                .leaves
                .iter()
                .any(|leaf| matches!(leaf, VmLeaf::Input { name, .. } if name == "Particles.Age"))
        );
    }

    #[test]
    fn outputs_are_named_by_their_kind_s_component() {
        let set = VmDataSet::from_attributes(
            "Particles",
            "Particles.",
            &[
                attribute("Age", "NiagaraFloat"),
                attribute("Color", "LinearColor"),
                attribute("ID", "NiagaraID"),
                attribute("Position", "NiagaraPosition"),
                attribute("Scale", "NiagaraHalfVector2"),
                attribute("UniqueID", "NiagaraInt32"),
            ],
        );
        assert_eq!(
            set.component(VmComponentKind::Float, 2),
            "Particles.Color.G"
        );
        assert_eq!(
            set.component(VmComponentKind::Float, 5),
            "Particles.Position.X"
        );
        assert_eq!(
            set.component(VmComponentKind::Int, 1),
            "Particles.ID.AcquireTag"
        );
        assert_eq!(set.component(VmComponentKind::Int, 2), "Particles.UniqueID");
        assert_eq!(set.component(VmComponentKind::Half, 1), "Particles.Scale.Y");
        assert!(set.out_of_range(VmComponentKind::Float, 8));
        let cut = VmDataSet::from_attributes(
            "Particles",
            "Particles.",
            &[
                attribute("Age", "NiagaraFloat"),
                attribute("Odd", "SomethingNew"),
            ],
        );
        assert!(cut.partial);
        assert_eq!(
            cut.component(VmComponentKind::Float, 3),
            "Particles float[3]"
        );
        assert!(!cut.out_of_range(VmComponentKind::Float, 3));
    }
}
