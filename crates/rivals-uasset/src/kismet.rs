//! Disassembles the Kismet bytecode a function or class export stores, recording every package
//! index the script names.
//!
//! The script is stored behind two words: the size it occupies once loaded, and the size it takes
//! on disk. They differ because a name is twelve bytes in memory and eight on disk, and an object
//! pointer eight and four, so the loaded size is the space every jump offset indexes. That makes
//! it an exact oracle: a walk that ends anywhere else has read some operand at the wrong width.

use std::collections::BTreeMap;

use serde::Serialize;

use retoc::legacy_asset::FPackageNameMap;

use crate::props::{Ctx, Diagnostics, read_count, read_index};
use crate::reader::Cursor;

/// A statement is one top-level expression, so a script nested deeper than this is not one this
/// reader will ever meet honestly.
const MAX_EXPR_DEPTH: u32 = 64;

/// Widths an operand takes once loaded, which is the space the jump offsets count in.
const LOADED_NAME: u32 = 12;
const LOADED_POINTER: u32 = 8;

#[derive(Debug, Clone, Serialize)]
pub struct Script {
    /// `BytecodeBufferSize`: the size the script occupies once loaded.
    pub buffer_size: u32,
    /// `SerializedScriptSize`: what it takes on disk, which is what a replacement must match.
    pub storage_size: u32,
    /// The loaded size the decoded statements account for. Equal to `buffer_size` on a whole read.
    pub decoded_size: u32,
    /// Where the two size words sit, so a replacement of another length can rewrite them.
    #[serde(skip)]
    pub sizes_at: u64,
    #[serde(skip)]
    pub start: u64,
    #[serde(skip)]
    pub end: u64,
    pub statements: Vec<Statement>,
    /// Why the walk stopped, when it did. A script that stopped is not trusted for its references.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stopped: Option<ScriptStop>,
    /// Where each name the statements hold was read from, when the walk was whole.
    #[serde(skip)]
    pub names: Vec<u64>,
    /// Where every expression sits, in the order they start, which is the order [`children`]
    /// visits them in.
    #[serde(skip)]
    pub spans: Vec<Span>,
    /// Every field that holds a code offset or a length of code, which a change of size has to
    /// rewrite.
    #[serde(skip)]
    pub fixups: Vec<Fixup>,
    /// Why this script has to keep its size, when it does, first reason first. An edit that keeps
    /// every byte where it was is unaffected.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub resize_locks: Vec<ResizeLock>,
}

/// Where one expression sits, both in the file and in the loaded bytes jumps count in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub token: u8,
    pub at: u64,
    pub end_at: u64,
    pub offset: u32,
    pub end_offset: u32,
}

/// Which instruction a code offset belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FixupSource {
    Jump,
    JumpIfNot,
    PushFlow,
    SwitchEnd,
    SwitchNext,
    /// An `Offset` constant: a latent action's resume point.
    Linkage,
    ContextSkip,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixupKind {
    /// A loaded offset into a script: this one, or the function `resumes` names when a latent
    /// action resumes somewhere else.
    Absolute {
        target: u32,
        resumes: Option<String>,
    },
    /// The loaded length of the code from `from` to `to`.
    Relative { from: u32, to: u32 },
}

/// A four-byte field holding a code offset or length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fixup {
    /// Where the field sits in the file.
    pub at: u64,
    pub source: FixupSource,
    pub kind: FixupKind,
}

/// Why a script cannot change size: something holds an offset into it that a resize could not
/// follow, or one of its own offsets does not say what the decoder expects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResizeLock {
    /// A short category the audit counts by.
    pub kind: &'static str,
    pub reason: String,
}

impl Script {
    /// Whether every byte was accounted for, which is what makes the references it names complete.
    pub fn complete(&self) -> bool {
        self.stopped.is_none()
    }

    pub fn script_len(&self) -> usize {
        self.statements.len()
    }

    /// Why the script cannot change size, when it cannot: it did not decode whole, or something
    /// holds an offset into it that a resize could not follow.
    pub fn resize_lock(&self) -> Option<String> {
        if let Some(stop) = &self.stopped {
            return Some(format!(
                "the script stops decoding at 0x{:04X}: {}",
                stop.offset, stop.reason
            ));
        }
        self.resize_locks.first().map(|lock| lock.reason.clone())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Statement {
    /// The offset a jump would name, counted in loaded bytes from the script's start.
    pub offset: u32,
    /// Where the statement begins in the file.
    pub at: u64,
    pub expr: Expr,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScriptStop {
    pub offset: u32,
    pub at: u64,
    pub token: u8,
    pub reason: String,
}

/// An object the script names, resolved to a path where the package can.
#[derive(Debug, Clone, Serialize)]
pub struct ObjectRef {
    pub index: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// A name exactly as the bytes hold it: an entry of the package's name table and the number UE
/// appends to it, stored one higher than it prints.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct NameId {
    pub index: i32,
    pub number: i32,
}

impl From<retoc::legacy_asset::FMinimalName> for NameId {
    fn from(name: retoc::legacy_asset::FMinimalName) -> Self {
        NameId {
            index: name.index,
            number: name.number,
        }
    }
}

/// A property the script reads or writes, as the dotted name chain and the object that owns it.
#[derive(Debug, Clone, Serialize)]
pub struct PropertyRef {
    pub path: String,
    pub owner: ObjectRef,
    /// Each segment of the chain as the bytes name it, which the path's text cannot always say:
    /// a segment may hold a dot, and the table may hold a name twice.
    #[serde(skip)]
    pub names: Vec<NameId>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SwitchCase {
    pub value: Expr,
    /// Where execution goes when this case does not match.
    pub next: u32,
    pub result: Expr,
}

/// The forms `EX_TextConst` takes, chosen by the byte in front of it.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "text", rename_all = "snake_case")]
pub enum TextLiteral {
    Empty,
    Localized {
        source: Box<Expr>,
        key: Box<Expr>,
        namespace: Box<Expr>,
    },
    Invariant {
        source: Box<Expr>,
    },
    Literal {
        source: Box<Expr>,
    },
    StringTable {
        table: ObjectRef,
        table_id: Box<Expr>,
        key: Box<Expr>,
    },
}

/// One instruction. Tokens that share an operand shape share a variant and carry their own name,
/// so the JSON and the disassembly still say exactly which instruction it was.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Expr {
    /// A token with no operands at all.
    Simple {
        name: &'static str,
    },
    Variable {
        name: &'static str,
        property: PropertyRef,
    },
    Return {
        value: Box<Expr>,
    },
    Jump {
        target: u32,
    },
    JumpIfNot {
        target: u32,
        condition: Box<Expr>,
    },
    Assert {
        line: u16,
        /// Whether the assert only fires in a debug build. A byte, kept whole so it writes back.
        debug: u8,
        condition: Box<Expr>,
    },
    NothingInt32 {
        value: i32,
    },
    Let {
        name: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        property: Option<PropertyRef>,
        variable: Box<Expr>,
        value: Box<Expr>,
    },
    BitFieldConst {
        property: PropertyRef,
        value: u8,
    },
    Context {
        name: &'static str,
        object: Box<Expr>,
        skip: u32,
        property: PropertyRef,
        member: Box<Expr>,
    },
    Cast {
        name: &'static str,
        class: ObjectRef,
        value: Box<Expr>,
    },
    SelfRef,
    Skip {
        skip: u32,
        value: Box<Expr>,
    },
    VirtualCall {
        name: &'static str,
        function: String,
        params: Vec<Expr>,
        #[serde(skip)]
        id: NameId,
    },
    FinalCall {
        name: &'static str,
        function: ObjectRef,
        params: Vec<Expr>,
    },
    /// Literal constants carry `at`, the file offset of their token byte in the same space
    /// `Statement::at` uses, so one can be located and replaced at its own width.
    IntConst {
        value: i32,
        at: u64,
    },
    Int64Const {
        value: i64,
        at: u64,
    },
    UInt64Const {
        value: u64,
        at: u64,
    },
    FloatConst {
        value: f32,
        at: u64,
    },
    DoubleConst {
        value: f64,
        at: u64,
    },
    ByteConst {
        name: &'static str,
        value: u8,
        at: u64,
    },
    StringConst {
        value: String,
        at: u64,
    },
    UnicodeStringConst {
        value: String,
        at: u64,
    },
    ObjectConst {
        object: ObjectRef,
    },
    NameConst {
        name: &'static str,
        value: String,
        at: u64,
        #[serde(skip)]
        id: NameId,
    },
    /// Rotations, vectors and transforms: a fixed run of floating point numbers.
    Numbers {
        name: &'static str,
        values: Vec<f64>,
        at: u64,
    },
    TextConst {
        text: TextLiteral,
    },
    StructConst {
        struct_type: ObjectRef,
        size: i32,
        fields: Vec<Expr>,
    },
    SetArray {
        array: Box<Expr>,
        items: Vec<Expr>,
    },
    PropertyConst {
        property: PropertyRef,
    },
    Conversion {
        conversion: u8,
        value: Box<Expr>,
    },
    /// `SetSet` and `SetMap`: a target, a count and the elements. A map's pairs run key, value.
    SetContainer {
        name: &'static str,
        target: Box<Expr>,
        count: i32,
        items: Vec<Expr>,
    },
    /// `SetConst` and `ArrayConst`: a property, a count and the elements.
    ContainerConst {
        name: &'static str,
        property: PropertyRef,
        count: i32,
        items: Vec<Expr>,
    },
    MapConst {
        key: PropertyRef,
        value: PropertyRef,
        count: i32,
        items: Vec<Expr>,
    },
    Member {
        name: &'static str,
        property: PropertyRef,
        value: Box<Expr>,
    },
    PushExecutionFlow {
        target: u32,
    },
    ComputedJump {
        target: Box<Expr>,
    },
    /// A token taking one expression and nothing else.
    Unary {
        name: &'static str,
        value: Box<Expr>,
    },
    SkipOffsetConst {
        value: u32,
    },
    DelegateOp {
        name: &'static str,
        delegate: Box<Expr>,
        value: Box<Expr>,
    },
    BindDelegate {
        function: String,
        delegate: Box<Expr>,
        object: Box<Expr>,
        #[serde(skip)]
        id: NameId,
    },
    CallMulticastDelegate {
        signature: ObjectRef,
        delegate: Box<Expr>,
        params: Vec<Expr>,
    },
    SwitchValue {
        end: u32,
        index: Box<Expr>,
        cases: Vec<SwitchCase>,
        default: Box<Expr>,
    },
    InstrumentationEvent {
        event: u8,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(skip)]
        id: NameId,
    },
    ArrayGetByRef {
        array: Box<Expr>,
        index: Box<Expr>,
    },
    /// A token this reader does not model. The walk stops here rather than guessing a width.
    Unknown {
        token: u8,
    },
}

/// The name UE gives a token, for the disassembly and for the audit's census.
pub fn token_name(token: u8) -> Option<&'static str> {
    Some(match token {
        0x00 => "LocalVariable",
        0x01 => "InstanceVariable",
        0x02 => "DefaultVariable",
        0x04 => "Return",
        0x06 => "Jump",
        0x07 => "JumpIfNot",
        0x09 => "Assert",
        0x0B => "Nothing",
        0x0C => "NothingInt32",
        0x0F => "Let",
        0x11 => "BitFieldConst",
        0x12 => "ClassContext",
        0x13 => "MetaCast",
        0x14 => "LetBool",
        0x15 => "EndParmValue",
        0x16 => "EndFunctionParms",
        0x17 => "Self",
        0x18 => "Skip",
        0x19 => "Context",
        0x1A => "Context_FailSilent",
        0x1B => "VirtualFunction",
        0x1C => "FinalFunction",
        0x1D => "IntConst",
        0x1E => "FloatConst",
        0x1F => "StringConst",
        0x20 => "ObjectConst",
        0x21 => "NameConst",
        0x22 => "RotationConst",
        0x23 => "VectorConst",
        0x24 => "ByteConst",
        0x25 => "IntZero",
        0x26 => "IntOne",
        0x27 => "True",
        0x28 => "False",
        0x29 => "TextConst",
        0x2A => "NoObject",
        0x2B => "TransformConst",
        0x2C => "IntConstByte",
        0x2D => "NoInterface",
        0x2E => "DynamicCast",
        0x2F => "StructConst",
        0x30 => "EndStructConst",
        0x31 => "SetArray",
        0x32 => "EndArray",
        0x33 => "PropertyConst",
        0x34 => "UnicodeStringConst",
        0x35 => "Int64Const",
        0x36 => "UInt64Const",
        0x37 => "DoubleConst",
        0x38 => "Cast",
        0x39 => "SetSet",
        0x3A => "EndSet",
        0x3B => "SetMap",
        0x3C => "EndMap",
        0x3D => "SetConst",
        0x3E => "EndSetConst",
        0x3F => "MapConst",
        0x40 => "EndMapConst",
        0x41 => "Vector3fConst",
        0x42 => "StructMemberContext",
        0x43 => "LetMulticastDelegate",
        0x44 => "LetDelegate",
        0x45 => "LocalVirtualFunction",
        0x46 => "LocalFinalFunction",
        0x48 => "LocalOutVariable",
        0x4A => "DeprecatedOp4A",
        0x4B => "InstanceDelegate",
        0x4C => "PushExecutionFlow",
        0x4D => "PopExecutionFlow",
        0x4E => "ComputedJump",
        0x4F => "PopExecutionFlowIfNot",
        0x50 => "Breakpoint",
        0x51 => "InterfaceContext",
        0x52 => "ObjToInterfaceCast",
        0x53 => "EndOfScript",
        0x54 => "CrossInterfaceCast",
        0x55 => "InterfaceToObjCast",
        0x5A => "WireTracepoint",
        0x5B => "SkipOffsetConst",
        0x5C => "AddMulticastDelegate",
        0x5D => "ClearMulticastDelegate",
        0x5E => "Tracepoint",
        0x5F => "LetObj",
        0x60 => "LetWeakObjPtr",
        0x61 => "BindDelegate",
        0x62 => "RemoveMulticastDelegate",
        0x63 => "CallMulticastDelegate",
        0x64 => "LetValueOnPersistentFrame",
        0x65 => "ArrayConst",
        0x66 => "EndArrayConst",
        0x67 => "SoftObjectConst",
        0x68 => "CallMath",
        0x69 => "SwitchValue",
        0x6A => "InstrumentationEvent",
        0x6B => "ArrayGetByRef",
        0x6C => "ClassSparseDataVariable",
        0x6D => "FieldPathConst",
        _ => return None,
    })
}

/// The token a name from [`token_name`] stands for.
pub fn token_named(name: &str) -> Option<u8> {
    static TOKENS: std::sync::OnceLock<std::collections::HashMap<&'static str, u8>> =
        std::sync::OnceLock::new();
    TOKENS
        .get_or_init(|| {
            (0..=u8::MAX)
                .filter_map(|token| Some((token_name(token)?, token)))
                .collect()
        })
        .get(name)
        .copied()
}

/// The conversions `EX_Cast` performs, by UE5's numbering, which is the one the game's scripts use.
pub(crate) fn conversion_name(kind: u8) -> Option<&'static str> {
    Some(match kind {
        0x00 => "ObjectToInterface",
        0x01 => "ObjectToBool",
        0x02 => "InterfaceToBool",
        0x03 => "DoubleToFloat",
        0x04 => "FloatToDouble",
        _ => return None,
    })
}

/// The conversion a name from [`conversion_name`] stands for.
pub(crate) fn conversion_kind(name: &str) -> Option<u8> {
    (0..=u8::MAX).find(|kind| conversion_name(*kind) == Some(name))
}

struct Reader<'a, 'b> {
    cursor: Cursor<'a>,
    ctx: &'b Ctx<'b>,
    diagnostics: &'b mut Diagnostics,
    /// How far the walk has come in loaded bytes, which is what a jump offset names.
    offset: u32,
    depth: u32,
    last_token: u8,
    last_token_offset: u32,
    last_token_at: u64,
    spans: Vec<Span>,
    fixups: Vec<Fixup>,
    locks: Vec<ResizeLock>,
}

impl<'a, 'b> Reader<'a, 'b> {
    fn lock(&mut self, kind: &'static str, reason: String) {
        self.locks.push(ResizeLock { kind, reason });
    }

    /// A four-byte code offset, recorded so a resize can rewrite it.
    fn code_offset(&mut self, source: FixupSource) -> Result<u32, String> {
        let at = self.cursor.file_offset();
        let target = self.u32v()?;
        self.fixups.push(Fixup {
            at,
            source,
            kind: FixupKind::Absolute {
                target,
                resumes: None,
            },
        });
        Ok(target)
    }

    fn stop(&self, reason: String) -> ScriptStop {
        ScriptStop {
            offset: self.last_token_offset,
            at: self.last_token_at,
            token: self.last_token,
            reason,
        }
    }

    fn u8v(&mut self) -> Result<u8, String> {
        let value = self.cursor.read_u8()?;
        self.offset += 1;
        Ok(value)
    }

    fn u16v(&mut self) -> Result<u16, String> {
        let value = self.cursor.read_u16()?;
        self.offset += 2;
        Ok(value)
    }

    fn i32v(&mut self) -> Result<i32, String> {
        let value = self.cursor.read_i32()?;
        self.offset += 4;
        Ok(value)
    }

    fn u32v(&mut self) -> Result<u32, String> {
        let value = self.cursor.read_u32()?;
        self.offset += 4;
        Ok(value)
    }

    fn i64v(&mut self) -> Result<i64, String> {
        let value = self.cursor.read_i64()?;
        self.offset += 8;
        Ok(value)
    }

    fn u64v(&mut self) -> Result<u64, String> {
        let value = self.cursor.read_u64()?;
        self.offset += 8;
        Ok(value)
    }

    fn f32v(&mut self) -> Result<f32, String> {
        let value = self.cursor.read_f32()?;
        self.offset += 4;
        Ok(value)
    }

    fn f64v(&mut self) -> Result<f64, String> {
        let value = self.cursor.read_f64()?;
        self.offset += 8;
        Ok(value)
    }

    /// A name is eight bytes on disk and twelve once loaded, which is where the two sizes part.
    fn name(&mut self) -> Result<(String, NameId), String> {
        let (value, id) = self.cursor.read_name_id(self.ctx.names())?;
        self.offset += LOADED_NAME;
        Ok((value, id.into()))
    }

    /// An object is a four byte package index on disk and a pointer once loaded.
    fn object(&mut self) -> Result<ObjectRef, String> {
        let index = read_index(&mut self.cursor, self.diagnostics)?;
        self.offset += LOADED_POINTER;
        let path = self
            .ctx
            .object_path(index)
            .map_err(|e| self.cursor.err(e))?;
        Ok(ObjectRef { index, path })
    }

    /// A property is an `FFieldPath` on disk and a pointer once loaded.
    fn property(&mut self) -> Result<PropertyRef, String> {
        let count = read_count(&mut self.cursor, "field path")?;
        let mut segments = Vec::with_capacity(count);
        let mut names = Vec::with_capacity(count);
        for _ in 0..count {
            let (segment, id) = self.cursor.read_name_id(self.ctx.names())?;
            segments.push(segment);
            names.push(id.into());
        }
        let owner = read_index(&mut self.cursor, self.diagnostics)?;
        self.offset += LOADED_POINTER;
        let resolved = self
            .ctx
            .object_path(owner)
            .map_err(|e| self.cursor.err(e))?;
        Ok(PropertyRef {
            path: segments.join("."),
            owner: ObjectRef {
                index: owner,
                path: resolved,
            },
            names,
        })
    }

    /// A null terminated ANSI string, the same width in both spaces.
    fn ansi(&mut self) -> Result<String, String> {
        let mut out = String::new();
        loop {
            let byte = self.u8v()?;
            if byte == 0 {
                return Ok(out);
            }
            out.push(byte as char);
        }
    }

    /// A null terminated UTF-16 string.
    fn utf16(&mut self) -> Result<String, String> {
        let mut units = Vec::new();
        loop {
            let unit = self.u16v()?;
            if unit == 0 {
                return Ok(String::from_utf16_lossy(&units));
            }
            units.push(unit);
        }
    }

    fn boxed(&mut self) -> Result<Box<Expr>, String> {
        Ok(Box::new(self.expr()?))
    }

    /// Expressions until `terminator`, which is consumed.
    fn until(&mut self, terminator: u8) -> Result<Vec<Expr>, String> {
        let mut out = Vec::new();
        loop {
            match self.cursor.peek_u8() {
                Some(token) if token == terminator => {
                    self.u8v()?;
                    self.count_token(terminator);
                    return Ok(out);
                }
                Some(_) => out.push(self.expr()?),
                None => {
                    return Err(format!(
                        "the script ends before its {} terminator",
                        token_name(terminator).unwrap_or("closing")
                    ));
                }
            }
        }
    }

    fn count_token(&mut self, token: u8) {
        *self.diagnostics.script_tokens.entry(token).or_default() += 1;
    }

    fn expr(&mut self) -> Result<Expr, String> {
        if self.depth > MAX_EXPR_DEPTH {
            return Err("the script nests deeper than this reader follows".to_string());
        }
        let at = self.cursor.file_offset();
        let offset = self.offset;
        let token = self.u8v()?;
        self.last_token = token;
        self.last_token_offset = offset;
        self.last_token_at = at;
        self.count_token(token);
        let slot = self.spans.len();
        self.spans.push(Span {
            token,
            at,
            end_at: at,
            offset,
            end_offset: offset,
        });
        self.depth += 1;
        let value = self.body(token, at, offset);
        self.depth -= 1;
        let (end_at, end_offset) = (self.cursor.file_offset(), self.offset);
        if let Some(span) = self.spans.get_mut(slot) {
            span.end_at = end_at;
            span.end_offset = end_offset;
        }
        value
    }

    fn body(&mut self, token: u8, at: u64, offset: u32) -> Result<Expr, String> {
        let named = |token: u8| token_name(token).unwrap_or("Unknown");
        Ok(match token {
            0x00 | 0x01 | 0x02 | 0x48 | 0x6C => Expr::Variable {
                name: named(token),
                property: self.property()?,
            },
            0x04 => Expr::Return {
                value: self.boxed()?,
            },
            0x06 => Expr::Jump {
                target: self.code_offset(FixupSource::Jump)?,
            },
            0x07 => Expr::JumpIfNot {
                target: self.code_offset(FixupSource::JumpIfNot)?,
                condition: self.boxed()?,
            },
            0x09 => Expr::Assert {
                line: self.u16v()?,
                debug: self.u8v()?,
                condition: self.boxed()?,
            },
            0x0B | 0x15 | 0x16 | 0x25 | 0x26 | 0x27 | 0x28 | 0x2A | 0x2D | 0x30 | 0x32 | 0x3A
            | 0x3C | 0x3E | 0x40 | 0x4A | 0x4D | 0x50 | 0x53 | 0x5A | 0x5E | 0x66 => {
                Expr::Simple { name: named(token) }
            }
            0x0C => Expr::NothingInt32 {
                value: self.i32v()?,
            },
            0x0F => Expr::Let {
                name: named(token),
                property: Some(self.property()?),
                variable: self.boxed()?,
                value: self.boxed()?,
            },
            0x14 | 0x43 | 0x44 | 0x5F | 0x60 => Expr::Let {
                name: named(token),
                property: None,
                variable: self.boxed()?,
                value: self.boxed()?,
            },
            0x11 => Expr::BitFieldConst {
                property: self.property()?,
                value: self.u8v()?,
            },
            0x12 | 0x19 | 0x1A => {
                let object = self.boxed()?;
                let skip_at = self.cursor.file_offset();
                let skip = self.u32v()?;
                let property = self.property()?;
                // The skip is how far a null context jumps over the member, counted from after
                // the property, so it is the member's own loaded length.
                let from = self.offset;
                let member = self.boxed()?;
                let to = self.offset;
                self.fixups.push(Fixup {
                    at: skip_at,
                    source: FixupSource::ContextSkip,
                    kind: FixupKind::Relative { from, to },
                });
                if skip != to - from {
                    self.lock(
                        "context skip",
                        format!(
                            "the context at 0x{offset:04X} skips {skip} bytes where its member takes {}",
                            to - from
                        ),
                    );
                }
                Expr::Context {
                    name: named(token),
                    object,
                    skip,
                    property,
                    member,
                }
            }
            0x13 | 0x2E | 0x52 | 0x54 | 0x55 => Expr::Cast {
                name: named(token),
                class: self.object()?,
                value: self.boxed()?,
            },
            0x17 => Expr::SelfRef,
            0x18 => {
                self.lock(
                    "skip",
                    format!(
                        "an EX_Skip at 0x{offset:04X} holds a length the compiler never writes, so nothing says how it would move"
                    ),
                );
                Expr::Skip {
                    skip: self.u32v()?,
                    value: self.boxed()?,
                }
            }
            0x1B | 0x45 => {
                let (function, id) = self.name()?;
                Expr::VirtualCall {
                    name: named(token),
                    function,
                    params: self.until(0x16)?,
                    id,
                }
            }
            0x1C | 0x46 | 0x68 => Expr::FinalCall {
                name: named(token),
                function: self.object()?,
                params: self.until(0x16)?,
            },
            0x1D => Expr::IntConst {
                value: self.i32v()?,
                at,
            },
            0x1E => Expr::FloatConst {
                value: self.f32v()?,
                at,
            },
            0x1F => Expr::StringConst {
                value: self.ansi()?,
                at,
            },
            0x20 => Expr::ObjectConst {
                object: self.object()?,
            },
            0x21 | 0x4B => {
                let (value, id) = self.name()?;
                Expr::NameConst {
                    name: named(token),
                    value,
                    at,
                    id,
                }
            }
            0x22 | 0x23 => Expr::Numbers {
                name: named(token),
                values: self.doubles(3)?,
                at,
            },
            0x2B => Expr::Numbers {
                name: named(token),
                values: self.doubles(10)?,
                at,
            },
            0x41 => Expr::Numbers {
                name: named(token),
                values: self.floats(3)?,
                at,
            },
            0x24 | 0x2C => Expr::ByteConst {
                name: named(token),
                value: self.u8v()?,
                at,
            },
            0x29 => Expr::TextConst { text: self.text()? },
            0x2F => {
                let struct_type = self.object()?;
                let size = self.i32v()?;
                let first = self.fixups.len();
                let fields = self.until(0x30)?;
                if is_latent_info(&struct_type) {
                    self.claim_linkage(first, &fields, offset);
                }
                Expr::StructConst {
                    struct_type,
                    size,
                    fields,
                }
            }
            0x31 => Expr::SetArray {
                array: self.boxed()?,
                items: self.until(0x32)?,
            },
            0x33 => Expr::PropertyConst {
                property: self.property()?,
            },
            0x34 => Expr::UnicodeStringConst {
                value: self.utf16()?,
                at,
            },
            0x35 => Expr::Int64Const {
                value: self.i64v()?,
                at,
            },
            0x36 => Expr::UInt64Const {
                value: self.u64v()?,
                at,
            },
            0x37 => Expr::DoubleConst {
                value: self.f64v()?,
                at,
            },
            0x38 => Expr::Conversion {
                conversion: self.u8v()?,
                value: self.boxed()?,
            },
            0x39 => Expr::SetContainer {
                name: named(token),
                target: self.boxed()?,
                count: self.i32v()?,
                items: self.until(0x3A)?,
            },
            0x3B => Expr::SetContainer {
                name: named(token),
                target: self.boxed()?,
                count: self.i32v()?,
                items: self.until(0x3C)?,
            },
            0x3D => Expr::ContainerConst {
                name: named(token),
                property: self.property()?,
                count: self.i32v()?,
                items: self.until(0x3E)?,
            },
            0x65 => Expr::ContainerConst {
                name: named(token),
                property: self.property()?,
                count: self.i32v()?,
                items: self.until(0x66)?,
            },
            0x3F => Expr::MapConst {
                key: self.property()?,
                value: self.property()?,
                count: self.i32v()?,
                items: self.until(0x40)?,
            },
            0x42 | 0x64 => Expr::Member {
                name: named(token),
                property: self.property()?,
                value: self.boxed()?,
            },
            0x4C => Expr::PushExecutionFlow {
                target: self.code_offset(FixupSource::PushFlow)?,
            },
            0x4E => {
                let target = self.boxed()?;
                // An event graph dispatches on the offset its stubs pass in, which the package
                // pass accounts for before it lifts this; any other computed jump goes wherever a
                // variable says, and nothing can follow that.
                if is_entry_point(&target) {
                    self.lock(
                        ENTRY_DISPATCH,
                        format!(
                            "the jump at 0x{offset:04X} enters at the offset its event stubs pass in"
                        ),
                    );
                } else {
                    self.lock(
                        "computed jump",
                        format!(
                            "the computed jump at 0x{offset:04X} goes wherever a variable says"
                        ),
                    );
                }
                Expr::ComputedJump { target }
            }
            0x4F | 0x51 | 0x5D | 0x67 | 0x6D => Expr::Unary {
                name: named(token),
                value: self.boxed()?,
            },
            0x5B => Expr::SkipOffsetConst {
                value: self.code_offset(FixupSource::Linkage)?,
            },
            0x5C | 0x62 => Expr::DelegateOp {
                name: named(token),
                delegate: self.boxed()?,
                value: self.boxed()?,
            },
            0x61 => {
                let (function, id) = self.name()?;
                Expr::BindDelegate {
                    function,
                    delegate: self.boxed()?,
                    object: self.boxed()?,
                    id,
                }
            }
            0x63 => Expr::CallMulticastDelegate {
                signature: self.object()?,
                delegate: self.boxed()?,
                params: self.until(0x16)?,
            },
            0x69 => self.switch(offset)?,
            0x6A => {
                let event = self.u8v()?;
                // Only an inline event carries a name of its own.
                let named = (event == 4).then(|| self.name()).transpose()?;
                let id = named.as_ref().map(|(_, id)| *id).unwrap_or_default();
                Expr::InstrumentationEvent {
                    event,
                    name: named.map(|(name, _)| name),
                    id,
                }
            }
            0x6B => Expr::ArrayGetByRef {
                array: self.boxed()?,
                index: self.boxed()?,
            },
            other => {
                return Err(format!("token {other:#04X} is not one this reader models"));
            }
        })
    }

    fn doubles(&mut self, count: usize) -> Result<Vec<f64>, String> {
        (0..count).map(|_| self.f64v()).collect()
    }

    fn floats(&mut self, count: usize) -> Result<Vec<f64>, String> {
        (0..count).map(|_| self.f32v().map(f64::from)).collect()
    }

    fn text(&mut self) -> Result<TextLiteral, String> {
        let kind = self.u8v()?;
        Ok(match kind {
            0 => TextLiteral::Empty,
            1 => TextLiteral::Localized {
                source: self.boxed()?,
                key: self.boxed()?,
                namespace: self.boxed()?,
            },
            2 => TextLiteral::Invariant {
                source: self.boxed()?,
            },
            3 => TextLiteral::Literal {
                source: self.boxed()?,
            },
            4 => TextLiteral::StringTable {
                table: self.object()?,
                table_id: self.boxed()?,
                key: self.boxed()?,
            },
            other => return Err(format!("text literal type {other} is not modelled")),
        })
    }

    /// Each case names where the next one starts and the switch names where it ends, both as
    /// offsets that have to be exactly the end of what precedes them.
    fn switch(&mut self, offset: u32) -> Result<Expr, String> {
        let cases = self.u16v()?;
        let end = self.code_offset(FixupSource::SwitchEnd)?;
        let index = self.boxed()?;
        let mut out = Vec::with_capacity(usize::from(cases));
        for _ in 0..cases {
            let value = self.expr()?;
            let next = self.code_offset(FixupSource::SwitchNext)?;
            let result = self.expr()?;
            if next != self.offset {
                self.lock(
                    "switch offset",
                    format!(
                        "a case of the switch at 0x{offset:04X} says the next starts at 0x{next:04X}, not 0x{:04X}",
                        self.offset
                    ),
                );
            }
            out.push(SwitchCase {
                value,
                next,
                result,
            });
        }
        let default = self.boxed()?;
        if end != self.offset {
            self.lock(
                "switch offset",
                format!(
                    "the switch at 0x{offset:04X} says it ends at 0x{end:04X}, not 0x{:04X}",
                    self.offset
                ),
            );
        }
        Ok(Expr::SwitchValue {
            end,
            index,
            cases: out,
            default,
        })
    }

    /// A latent action carries its resume point as an `Offset` constant, and the function it
    /// resumes by name beside it: the offset counts in that function's code, not this one's.
    fn claim_linkage(&mut self, first: usize, fields: &[Expr], offset: u32) {
        let function = fields.iter().find_map(|field| match field {
            Expr::NameConst {
                name: "NameConst",
                value,
                ..
            } => Some(value.clone()),
            _ => None,
        });
        match (fields.first(), function) {
            // A latent action that calls back through a delegate has no resume point at all.
            (Some(Expr::IntConst { value: -1, .. }), _) => {}
            (Some(Expr::SkipOffsetConst { .. }), Some(function)) => {
                if let Some(fixup) = self.fixups[first..]
                    .iter_mut()
                    .find(|fixup| fixup.source == FixupSource::Linkage)
                    && let FixupKind::Absolute { resumes, .. } = &mut fixup.kind
                {
                    *resumes = Some(function);
                }
            }
            _ => self.lock(
                "latent action",
                format!(
                    "the latent action at 0x{offset:04X} does not carry its resume point as an Offset constant beside the function it resumes"
                ),
            ),
        }
    }
}

/// The lock an event graph's dispatch takes until the package pass has accounted for every stub.
const ENTRY_DISPATCH: &str = "event graph entry";

/// The parameter an event graph dispatches on.
fn is_entry_point(target: &Expr) -> bool {
    matches!(target, Expr::Variable { property, .. } if property.path == "EntryPoint")
}

/// `FLatentActionInfo`, whose `Linkage` is a code offset.
fn is_latent_info(struct_type: &ObjectRef) -> bool {
    struct_type
        .path
        .as_deref()
        .and_then(|path| path.rsplit(['.', ':', '/']).next())
        == Some("LatentActionInfo")
}

/// Walks one script. `buffer_size` is the loaded size the export declares; passing `None` skips
/// that check, which is what a replacement being sized needs before its words are written.
pub(crate) fn read_script(
    bytes: &[u8],
    start: u64,
    sizes_at: u64,
    buffer_size: Option<u32>,
    storage_size: u32,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
) -> Script {
    let mark = diagnostics.references.len();
    let mut reader = Reader {
        cursor: Cursor::new(bytes, start),
        ctx,
        diagnostics,
        offset: 0,
        depth: 0,
        last_token: 0,
        last_token_offset: 0,
        last_token_at: start,
        spans: Vec::new(),
        fixups: Vec::new(),
        locks: Vec::new(),
    };
    let mut statements = Vec::new();
    let mut stopped = None;
    while reader.cursor.remaining() > 0 {
        let at = reader.cursor.file_offset();
        let offset = reader.offset;
        match reader.expr() {
            Ok(expr) => statements.push(Statement { offset, at, expr }),
            Err(reason) => {
                stopped = Some(reader.stop(reason));
                break;
            }
        }
    }
    let decoded_size = reader.offset;
    // The declared loaded size is the oracle: landing anywhere else means an operand was read at
    // the wrong width, however plausible the instructions look.
    if stopped.is_none()
        && let Some(declared) = buffer_size
        && decoded_size != declared
    {
        stopped = Some(reader.stop(format!(
            "the walk accounts for {decoded_size} loaded bytes where the export declares {declared}"
        )));
    }
    let mut names = reader.cursor.take_names();
    let (spans, fixups, mut locks) = (reader.spans, reader.fixups, reader.locks);
    if stopped.is_some() {
        // A script that did not decode whole may have named its objects at the wrong offsets.
        diagnostics.references.truncate(mark);
        names.clear();
    }
    if stopped.is_none() {
        locks.extend(stray_targets(&spans, &fixups, decoded_size));
    }
    Script {
        buffer_size: buffer_size.unwrap_or(decoded_size),
        storage_size,
        decoded_size,
        sizes_at,
        start,
        end: start + bytes.len() as u64,
        statements,
        stopped,
        names,
        spans,
        fixups,
        resize_locks: locks,
    }
}

/// A jump into the middle of an expression would land somewhere else once anything before it
/// moves, so every place this script jumps to in itself has to be where an expression starts, or
/// its very end. A switch's offsets are the ends of its arms instead, which the walk checks
/// exactly as it reads them.
fn stray_targets(spans: &[Span], fixups: &[Fixup], end: u32) -> Vec<ResizeLock> {
    let starts = expression_starts(spans);
    fixups
        .iter()
        .filter(|fixup| {
            !matches!(
                fixup.source,
                FixupSource::SwitchEnd | FixupSource::SwitchNext
            )
        })
        .filter_map(|fixup| match &fixup.kind {
            FixupKind::Absolute {
                target,
                resumes: None,
            } if *target != end && !starts.contains(target) => Some(ResizeLock {
                kind: "stray target",
                reason: format!(
                    "an offset at file {:#X} names 0x{target:04X}, where no expression starts",
                    fixup.at
                ),
            }),
            _ => None,
        })
        .collect()
}

fn expression_starts(spans: &[Span]) -> std::collections::HashSet<u32> {
    spans.iter().map(|span| span.offset).collect()
}

/// Visits every expression of a script with its span and the expression it sits in, in the order
/// the decoder recorded the spans.
pub fn visit_spans<'a, F>(script: &'a Script, visit: &mut F)
where
    F: FnMut(&'a Expr, &'a Span, Option<&'a Expr>),
{
    let mut next = 0;
    for statement in &script.statements {
        visit_tree(&statement.expr, None, &script.spans, &mut next, visit);
    }
}

fn visit_tree<'a, F>(
    expr: &'a Expr,
    parent: Option<&'a Expr>,
    spans: &'a [Span],
    next: &mut usize,
    visit: &mut F,
) where
    F: FnMut(&'a Expr, &'a Span, Option<&'a Expr>),
{
    let Some(span) = spans.get(*next) else {
        return;
    };
    *next += 1;
    visit(expr, span, parent);
    for child in children(expr) {
        visit_tree(child, Some(expr), spans, next, visit);
    }
}

/// A function's script, with what the package pass needs to know about it.
#[derive(Debug, Clone, Copy)]
pub struct Function<'a> {
    /// The export's index in the package.
    pub export: u32,
    pub name: &'a str,
    pub script: &'a Script,
    /// `EventGraphFunction` and `EventGraphCallOffset` from the function's tail.
    pub event_graph: (i32, i32),
}

impl Function<'_> {
    /// How the package's bytecode names this export.
    fn package_index(&self) -> i32 {
        self.export as i32 + 1
    }

    /// An event graph's code is entered at offsets its stubs pass in, which is what makes those
    /// stubs' literals part of its layout.
    pub fn is_event_graph(&self) -> bool {
        self.name.starts_with("ExecuteUbergraph")
    }
}

/// An event stub's entry: the integer it passes its event graph, which is an offset into it.
#[derive(Debug, Clone)]
pub struct Entry {
    /// The stub's export.
    pub export: u32,
    /// Where the literal sits.
    pub span: Span,
    pub offset: u32,
}

/// Everything outside a function's own code that holds an offset into it.
#[derive(Debug, Clone, Default)]
pub struct Inbound {
    pub entries: Vec<Entry>,
    /// Latent resume points another function's code holds: that function's export, and the field.
    pub linkages: Vec<(u32, Fixup)>,
    /// References to an event graph that might carry an offset nothing here can follow.
    pub unclassified: Vec<String>,
}

/// The integer an event stub passes its event graph, in whichever form the compiler chose for it.
pub fn entry_value(expr: &Expr) -> Option<u32> {
    match expr {
        Expr::IntConst { value, .. } => u32::try_from(*value).ok(),
        Expr::ByteConst {
            name: "IntConstByte",
            value,
            ..
        } => Some(u32::from(*value)),
        Expr::Simple { name: "IntZero" } => Some(0),
        Expr::Simple { name: "IntOne" } => Some(1),
        _ => None,
    }
}

/// What in `functions` holds an offset into `target`.
pub fn inbound(functions: &[Function<'_>], target: &Function<'_>) -> Inbound {
    let mut out = Inbound::default();
    let own = target.package_index();
    let event_graph = target.is_event_graph();
    let calls_target = |expr: &Expr| match expr {
        Expr::FinalCall { function, .. } => function.index == own,
        Expr::VirtualCall { function, .. } => function == target.name,
        _ => false,
    };
    for function in functions {
        for fixup in &function.script.fixups {
            if function.export != target.export
                && let FixupKind::Absolute {
                    resumes: Some(name),
                    ..
                } = &fixup.kind
                && name == target.name
            {
                out.linkages.push((function.export, fixup.clone()));
            }
        }
        if !event_graph {
            continue;
        }
        if function.event_graph.0 == own && function.event_graph.1 != 0 {
            out.unclassified.push(format!(
                "{} enters {} directly at {}",
                function.name, target.name, function.event_graph.1
            ));
        }
        let mut visit = |expr: &Expr, span: &Span, parent: Option<&Expr>| {
            if let Some(call) = parent.filter(|call| calls_target(call))
                && let Expr::FinalCall { params, .. } | Expr::VirtualCall { params, .. } = call
                && params
                    .first()
                    .is_some_and(|first| std::ptr::eq(first, expr))
                && let Some(offset) = entry_value(expr)
            {
                out.entries.push(Entry {
                    export: function.export,
                    span: *span,
                    offset,
                });
            }
            let strange = match expr {
                Expr::FinalCall { params, .. } | Expr::VirtualCall { params, .. }
                    if calls_target(expr) =>
                {
                    !matches!(params.as_slice(), [only] if entry_value(only).is_some())
                }
                Expr::ObjectConst { object } => object.index == own,
                Expr::NameConst { value, .. } => {
                    value == target.name && !parent.is_some_and(is_latent_struct)
                }
                Expr::BindDelegate { function: name, .. } => name == target.name,
                Expr::StringConst { value, .. } | Expr::UnicodeStringConst { value, .. } => {
                    value.contains(target.name)
                }
                _ => false,
            };
            if strange {
                out.unclassified.push(format!(
                    "{} names {} in `{}`",
                    function.name,
                    target.name,
                    render(expr)
                ));
            }
        };
        visit_spans(function.script, &mut visit);
    }
    out
}

fn is_latent_struct(expr: &Expr) -> bool {
    matches!(expr, Expr::StructConst { struct_type, .. } if is_latent_info(struct_type))
}

/// Settles which scripts can change size once the whole package has been read. An event graph's
/// dispatch is fine once every stub entering it is accounted for; anything else pointing into it,
/// or an offset landing mid-expression, locks it.
pub(crate) fn settle_resize_locks(exports: &mut [crate::package::ParsedExport]) {
    let mut changes: Vec<(usize, Vec<ResizeLock>)> = Vec::new();
    {
        let functions = functions_of(exports);
        for (position, target) in exports.iter().enumerate() {
            let Some(target) = functions.iter().find(|f| f.export == target.index) else {
                continue;
            };
            let script = target.script;
            let found = inbound(&functions, target);
            let mut locks: Vec<ResizeLock> = script
                .resize_locks
                .iter()
                .filter(|lock| !(target.is_event_graph() && lock.kind == ENTRY_DISPATCH))
                .cloned()
                .collect();
            let starts = expression_starts(&script.spans);
            let lands = |offset: u32| offset == script.decoded_size || starts.contains(&offset);
            let own_linkages = script.fixups.iter().filter(|fixup| {
                matches!(&fixup.kind, FixupKind::Absolute { resumes: Some(name), .. } if name == target.name)
            });
            for fixup in own_linkages.chain(found.linkages.iter().map(|(_, fixup)| fixup)) {
                if let FixupKind::Absolute { target: offset, .. } = fixup.kind
                    && !lands(offset)
                {
                    locks.push(ResizeLock {
                        kind: "stray target",
                        reason: format!(
                            "a latent action resumes {} at 0x{offset:04X}, where no expression starts",
                            target.name
                        ),
                    });
                }
            }
            for entry in &found.entries {
                if !lands(entry.offset) {
                    locks.push(ResizeLock {
                        kind: "stray target",
                        reason: format!(
                            "an event enters {} at 0x{:04X}, where no expression starts",
                            target.name, entry.offset
                        ),
                    });
                }
            }
            locks.extend(found.unclassified.into_iter().map(|reason| ResizeLock {
                kind: "event graph reference",
                reason,
            }));
            if locks != script.resize_locks {
                changes.push((position, locks));
            }
        }
    }
    for (position, locks) in changes {
        if let Some(script) = exports[position].script.as_mut() {
            script.resize_locks = locks;
        }
    }
}

/// What the audit counts about a package's bytecode: the shapes relocation relies on, and every
/// place they do not hold.
#[derive(Debug, Clone, Default)]
pub struct ScriptCensus {
    pub counts: BTreeMap<String, usize>,
    /// `(line, "#export: detail")`, for the lines worth an example.
    pub examples: Vec<(String, String)>,
}

impl ScriptCensus {
    fn count(&mut self, line: &str) {
        *self.counts.entry(line.to_string()).or_default() += 1;
    }

    fn note(&mut self, line: &str, export: u32, detail: String) {
        self.count(line);
        self.examples
            .push((line.to_string(), format!("#{export}: {detail}")));
    }
}

/// Counts what relocation relies on across one package's scripts.
pub fn census(parsed: &crate::package::ParsedPackage) -> ScriptCensus {
    let mut out = ScriptCensus::default();
    let functions = functions_of(&parsed.exports);
    let local_names: std::collections::HashSet<&str> =
        functions.iter().map(|function| function.name).collect();
    for function in &functions {
        let script = function.script;
        if script.resize_locks.is_empty() {
            out.count("scripts that can change size");
        }
        let mut kinds: Vec<&str> = script.resize_locks.iter().map(|lock| lock.kind).collect();
        kinds.sort_unstable();
        kinds.dedup();
        for kind in kinds {
            if let Some(lock) = script.resize_locks.iter().find(|lock| lock.kind == kind) {
                out.note(
                    &format!("locked: {kind}"),
                    function.export,
                    lock.reason.clone(),
                );
            }
        }
        if function.is_event_graph() {
            out.count("event graphs");
            let found = inbound(&functions, function);
            for entry in &found.entries {
                out.count(&format!(
                    "event entries carried by {}",
                    token_name(entry.span.token).unwrap_or("unknown")
                ));
            }
            for _ in &found.linkages {
                out.count("latent resume points held by another function");
            }
        }
        if function.event_graph.1 != 0 {
            out.note(
                "functions entering an event graph directly",
                function.export,
                format!("at {}", function.event_graph.1),
            );
        }
        for fixup in &script.fixups {
            match (&fixup.source, &fixup.kind) {
                (
                    FixupSource::Linkage,
                    FixupKind::Absolute {
                        resumes: Some(_), ..
                    },
                ) => {
                    out.count("latent resume points");
                }
                (FixupSource::Linkage, _) => {
                    out.note(
                        "Offset constants outside a latent action",
                        function.export,
                        format!("file {:#X}", fixup.at),
                    );
                }
                _ => {}
            }
        }
        let mut visit = |expr: &Expr, span: &Span, _: Option<&Expr>| match expr {
            Expr::TextConst { .. } => out.note(
                "text constants",
                function.export,
                format!("at 0x{:04X} {}", span.offset, render(expr)),
            ),
            Expr::StringConst { value, .. } if value.chars().any(|c| u32::from(c) > 0x7F) => {
                out.note(
                    "StringConsts holding a byte above 0x7F",
                    function.export,
                    format!("{value:?}"),
                );
            }
            Expr::FinalCall {
                function: callee, ..
            } if callee.index < 0
                && callee
                    .path
                    .as_deref()
                    .is_some_and(|path| callee_name(path).starts_with("ExecuteUbergraph")) =>
            {
                out.note(
                    "calls into another package's event graph",
                    function.export,
                    render(expr),
                );
            }
            Expr::VirtualCall {
                function: callee, ..
            } if callee.starts_with("ExecuteUbergraph")
                && !local_names.contains(callee.as_str()) =>
            {
                out.note(
                    "calls into another package's event graph",
                    function.export,
                    render(expr),
                );
            }
            _ => {}
        };
        visit_spans(script, &mut visit);
        script_import_edges(parsed, function, &mut out);
    }
    out
}

/// Whether the imports a script names sit in its export's dependency runs, which says whether a
/// script edit naming a new object has to add an edge the way a property edit does.
fn script_import_edges(
    parsed: &crate::package::ParsedPackage,
    function: &Function<'_>,
    out: &mut ScriptCensus,
) {
    let Some(runs) = parsed
        .dependencies
        .as_ref()
        .and_then(|runs| runs.get(function.export as usize))
    else {
        return;
    };
    let script = function.script;
    let mut seen = std::collections::HashSet::new();
    for reference in &parsed.references {
        if reference.at < script.start || reference.at >= script.end || reference.index >= 0 {
            continue;
        }
        if !seen.insert(reference.index) {
            continue;
        }
        let Some(import) = parsed.imports.get((-reference.index - 1) as usize) else {
            continue;
        };
        // Native objects and whole packages never take an edge in an IoStore package.
        if import.path.starts_with("/Script/") || import.outer_index == 0 {
            continue;
        }
        let line = if runs.create_before_serialize.contains(&reference.index) {
            "script imports with a create-before-serialize edge"
        } else if runs.serialize_before_serialize.contains(&reference.index) {
            "script imports with a serialize-before-serialize edge"
        } else if runs.create_before_create.contains(&reference.index)
            || runs.serialize_before_create.contains(&reference.index)
        {
            "script imports with another edge"
        } else {
            "script imports with no edge"
        };
        if line.ends_with("no edge") {
            out.note(line, function.export, import.path.clone());
        } else {
            out.count(line);
        }
    }
}

/// Every export holding a script that decoded whole, as the package pass sees it.
pub fn functions_of(exports: &[crate::package::ParsedExport]) -> Vec<Function<'_>> {
    exports
        .iter()
        .filter_map(|export| {
            let script = export.script.as_ref().filter(|script| script.complete())?;
            let event_graph = export
                .signature
                .as_ref()
                .map_or((0, 0), |s| (s.event_graph, s.event_graph_offset));
            Some(Function {
                export: export.index,
                name: export.object_name.as_str(),
                script,
                event_graph,
            })
        })
        .collect()
}

/// One statement as a viewer shows it, with every offset it can send execution to.
#[derive(Debug, Clone, Serialize)]
pub struct ScriptLine {
    pub offset: u32,
    pub text: String,
    /// The labels the text defines just before the statement, each with what enters there.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<crate::script_text::TextLabel>,
    /// What the text notes beside the statement: what an unresolved call probably was, or what a
    /// raw object index names.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Where a jump, a pushed flow, or a latent action's resume point leads, in statement offsets.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<u32>,
    /// The functions this statement calls, by name, so a viewer can open the ones it holds.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<String>,
    /// The constants in this statement that can take a new value in place.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub literals: Vec<LiteralSlot>,
    /// Everything in this statement an edit can address by where it starts.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub expressions: Vec<ExpressionSlot>,
}

/// What an addressed edit does at an expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotKind {
    /// A constant, or a one-byte form like `True` or `IntZero`.
    Literal,
    /// An object constant, or `NoObject`.
    Object,
    Text,
    Call,
    /// A `JumpIfNot` or `PopExecutionFlowIfNot` statement, whose condition an edit can fix.
    Condition,
}

impl SlotKind {
    pub fn label(self) -> &'static str {
        match self {
            SlotKind::Literal => "literal",
            SlotKind::Object => "object",
            SlotKind::Text => "text",
            SlotKind::Call => "call",
            SlotKind::Condition => "condition",
        }
    }
}

/// An expression an edit can address by the loaded offset it starts at.
#[derive(Debug, Clone, Serialize)]
pub struct ExpressionSlot {
    pub at: u32,
    pub kind: SlotKind,
    /// The instruction, such as `IntConst` or `JumpIfNot`.
    pub token: &'static str,
    /// The expression as the disassembly shows it, which is what an edit's `was` holds.
    pub text: String,
    /// What it holds as an edit would type it: a literal's value, an object's path, a text in
    /// UE's literal syntax, the function a call names. Nothing for a condition.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Where it sits in its line's text, in UTF-16 units, start and end.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<(u32, u32)>,
}

/// The string a text part holds.
pub(crate) fn string_of(expr: &Expr) -> &str {
    match expr {
        Expr::StringConst { value, .. } | Expr::UnicodeStringConst { value, .. } => value,
        _ => "",
    }
}

/// A text constant in UE's literal syntax, as an edit would type it.
pub(crate) fn text_form(expr: &Expr) -> String {
    use crate::text_literal::TextLiteral as Typed;
    let Expr::TextConst { text } = expr else {
        return render(expr);
    };
    let typed = match text {
        TextLiteral::Empty => Typed::Invariant(String::new()),
        TextLiteral::Invariant { source } | TextLiteral::Literal { source } => {
            Typed::Invariant(string_of(source).to_string())
        }
        TextLiteral::Localized {
            source,
            key,
            namespace,
        } => Typed::Localized {
            namespace: string_of(namespace).to_string(),
            key: string_of(key).to_string(),
            source: string_of(source).to_string(),
        },
        TextLiteral::StringTable { table_id, key, .. } => Typed::Table {
            table_id: string_of(table_id).to_string(),
            key: string_of(key).to_string(),
        },
    };
    crate::text_literal::format(&typed)
}

/// What an expression an edit can address holds, as the edit would type it.
fn slot_value(expr: &Expr, kind: SlotKind) -> Option<String> {
    match kind {
        SlotKind::Literal => match expr {
            Expr::Simple { name: "True" } => Some("true".into()),
            Expr::Simple { name: "False" } => Some("false".into()),
            Expr::Simple { name: "IntZero" } => Some("0".into()),
            Expr::Simple { name: "IntOne" } => Some("1".into()),
            literal => literal_value(literal),
        },
        SlotKind::Object => Some(match expr {
            Expr::ObjectConst { object } => object.path.clone().unwrap_or_else(|| "None".into()),
            _ => "None".into(),
        }),
        SlotKind::Text => Some(text_form(expr)),
        SlotKind::Call => match expr {
            Expr::FinalCall { function, .. } => function.path.clone(),
            Expr::VirtualCall { function, .. } => Some(function.clone()),
            _ => None,
        },
        SlotKind::Condition => None,
    }
}

/// What an edit can do at `expr`, which starts a statement when `top` is set.
fn slot_kind(expr: &Expr, top: bool) -> Option<SlotKind> {
    match expr {
        Expr::ObjectConst { .. } | Expr::Simple { name: "NoObject" } => Some(SlotKind::Object),
        Expr::TextConst { .. } => Some(SlotKind::Text),
        Expr::FinalCall { .. } | Expr::VirtualCall { .. } => Some(SlotKind::Call),
        Expr::JumpIfNot { .. }
        | Expr::Unary {
            name: "PopExecutionFlowIfNot",
            ..
        } if top => Some(SlotKind::Condition),
        literal if is_literal(literal) => Some(SlotKind::Literal),
        _ => None,
    }
}

/// Every expression of each statement with its span, statement by statement.
pub(crate) fn nodes_by_statement(script: &Script) -> Vec<Vec<(&Expr, Span)>> {
    let mut next = 0;
    script
        .statements
        .iter()
        .map(|statement| {
            let mut out = Vec::new();
            visit_tree(
                &statement.expr,
                None,
                &script.spans,
                &mut next,
                &mut |expr, span, _| out.push((expr, *span)),
            );
            out
        })
        .collect()
}

/// The expressions an edit can address in each statement, with where each sits in the statement's
/// line when the line is given.
fn slots_of(
    nodes: &[(&Expr, Span)],
    line: Option<&crate::script_text::TextLine>,
) -> Vec<ExpressionSlot> {
    let units =
        |text: &str, at: usize| text.get(..at).map_or(0, |head| head.encode_utf16().count()) as u32;
    nodes
        .iter()
        .enumerate()
        .filter_map(|(position, (expr, span))| {
            let kind = slot_kind(expr, position == 0)?;
            let range = line.and_then(|line| {
                let (_, (from, to)) = line.ranges.iter().find(|(at, _)| *at == span.offset)?;
                Some((units(&line.text, *from), units(&line.text, *to)))
            });
            Some(ExpressionSlot {
                at: span.offset,
                kind,
                token: token_name(span.token).unwrap_or("unknown"),
                text: render(expr),
                value: slot_value(expr, kind),
                range,
            })
        })
        .collect()
}

/// Where the `nth` expression of `kind` in the statement at `statement` starts, counted from 0 in
/// bytecode order. A miss lists what the statement does hold.
pub fn expression_at(
    script: &Script,
    statement: u32,
    kind: SlotKind,
    nth: u32,
) -> Result<u32, String> {
    let position = script
        .statements
        .iter()
        .position(|s| s.offset == statement)
        .ok_or_else(|| format!("no statement starts at 0x{statement:04X}"))?;
    let nodes = nodes_by_statement(script);
    let slots = slots_of(&nodes[position], None);
    let of_kind: Vec<&ExpressionSlot> = slots.iter().filter(|slot| slot.kind == kind).collect();
    of_kind.get(nth as usize).map(|slot| slot.at).ok_or_else(|| {
        let listing: Vec<String> = slots
            .iter()
            .map(|slot| format!("  0x{:04X} {} {}", slot.at, slot.kind.label(), slot.text))
            .collect();
        format!(
            "the statement at 0x{statement:04X} holds {} {}(s), so there is no {} {nth}; it holds:\n{}",
            of_kind.len(),
            kind.label(),
            kind.label(),
            listing.join("\n")
        )
    })
}

/// A call as a retarget sees it: the token it is made with, the function it names, how many
/// arguments it passes, and what becomes of what it returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallInfo {
    pub opcode: &'static str,
    /// A final call's function path, or the name a virtual call looks up.
    pub callee: String,
    pub arity: usize,
    pub use_: CallUse,
}

/// What a statement does with the value a call returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CallUse {
    /// The call is the statement, alone or behind the object it is made on.
    Discarded,
    /// A `Let` stores it, possibly through the object the call is made on.
    Kept,
    /// Something else takes it: an argument, a condition, a context's object.
    Consumed,
}

/// How the statement `root` uses the value of `call`, one of its expressions, and the variable it
/// is kept in when a `Let` keeps it. A call made on another object sits under that object's
/// `Context`, so `Let X = Target.Fn()` keeps the value as surely as `Let X = Fn()` does.
pub fn call_use<'a>(root: &'a Expr, call: &Expr) -> (CallUse, Option<&'a Expr>) {
    let through = |mut expr: &'a Expr| {
        while let Expr::Context { member, .. } = expr {
            if std::ptr::eq(expr, call) {
                break;
            }
            expr = member;
        }
        expr
    };
    if std::ptr::eq(through(root), call) {
        return (CallUse::Discarded, None);
    }
    if let Expr::Let {
        variable, value, ..
    } = root
        && std::ptr::eq(through(value), call)
    {
        return (CallUse::Kept, Some(variable));
    }
    (CallUse::Consumed, None)
}

/// The call starting at loaded offset `at` in the statement at `statement`, with what becomes of
/// its value and the variable a `Let` keeps it in.
pub fn call_expr_at(
    script: &Script,
    statement: u32,
    at: u32,
) -> Option<(&Expr, CallUse, Option<&Expr>)> {
    let position = script
        .statements
        .iter()
        .position(|s| s.offset == statement)?;
    let root = &script.statements[position].expr;
    let mut next = script
        .spans
        .iter()
        .position(|span| span.at == script.statements[position].at)?;
    let mut found = None;
    visit_tree(
        root,
        None,
        &script.spans,
        &mut next,
        &mut |expr, span, _| {
            if found.is_none()
                && span.offset == at
                && matches!(expr, Expr::FinalCall { .. } | Expr::VirtualCall { .. })
            {
                found = Some(expr);
            }
        },
    );
    let call = found?;
    let (use_, kept_in) = call_use(root, call);
    Some((call, use_, kept_in))
}

/// The call starting at loaded offset `at` in the statement at `statement`.
pub fn call_at(script: &Script, statement: u32, at: u32) -> Option<CallInfo> {
    let (call, use_, _) = call_expr_at(script, statement, at)?;
    match call {
        Expr::FinalCall {
            name,
            function,
            params,
        } => Some(CallInfo {
            opcode: name,
            callee: function.path.clone().unwrap_or_default(),
            arity: params.len(),
            use_,
        }),
        Expr::VirtualCall {
            name,
            function,
            params,
            ..
        } => Some(CallInfo {
            opcode: name,
            callee: function.clone(),
            arity: params.len(),
            use_,
        }),
        _ => None,
    }
}

/// The expression starting at loaded offset `at` in the statement at `statement`.
pub fn expression_starting(script: &Script, statement: u32, at: u32) -> Option<&Expr> {
    let position = script
        .statements
        .iter()
        .position(|s| s.offset == statement)?;
    nodes_by_statement(script)
        .into_iter()
        .nth(position)?
        .into_iter()
        .find(|(_, span)| span.offset == at)
        .map(|(expr, _)| expr)
}

/// A constant a viewer can offer for editing, addressed the way a script edit names it.
#[derive(Debug, Clone, Serialize)]
pub struct LiteralSlot {
    /// Which literal in the statement, counted from 0 in bytecode order across every literal.
    pub index: u32,
    pub kind: &'static str,
    /// The value as an edit would type it.
    pub value: String,
}

/// A literal's value as an edit would type it, for the kinds with value bytes.
fn literal_value(literal: &Expr) -> Option<String> {
    Some(match literal {
        Expr::IntConst { value, .. } => value.to_string(),
        Expr::Int64Const { value, .. } => value.to_string(),
        Expr::UInt64Const { value, .. } => value.to_string(),
        Expr::FloatConst { value, .. } => value.to_string(),
        Expr::DoubleConst { value, .. } => value.to_string(),
        Expr::ByteConst { value, .. } => value.to_string(),
        Expr::StringConst { value, .. } | Expr::UnicodeStringConst { value, .. } => value.clone(),
        Expr::NameConst { value, .. } => value.clone(),
        Expr::Numbers { values, .. } => values
            .iter()
            .map(f64::to_string)
            .collect::<Vec<_>>()
            .join(", "),
        _ => return None,
    })
}

/// The literals of `expr` a script edit can rewrite, with their index among all its literals.
pub fn literal_slots(expr: &Expr) -> Vec<LiteralSlot> {
    literals(expr)
        .into_iter()
        .enumerate()
        .filter_map(|(index, literal)| {
            Some(LiteralSlot {
                index: index as u32,
                kind: literal_kind(literal),
                value: literal_value(literal)?,
            })
        })
        .collect()
}

/// Every statement of the script `export` holds, as the assembler text writes it, alongside the
/// offsets it links to and what an edit can address in it.
pub fn script_lines(parsed: &crate::package::ParsedPackage, export: u32) -> Vec<ScriptLine> {
    let Some(script) = parsed
        .exports
        .get(export as usize)
        .and_then(|export| export.script.as_ref())
    else {
        return Vec::new();
    };
    let mut lines = crate::script_text::print_script(parsed, export)
        .map(|text| text.lines)
        .unwrap_or_default()
        .into_iter();
    let nodes = nodes_by_statement(script);
    script
        .statements
        .iter()
        .enumerate()
        .map(|(position, statement)| {
            let line = lines.next();
            let mut targets = Vec::new();
            flow_targets(&statement.expr, &mut targets);
            targets.dedup();
            let mut calls: Vec<String> = statement_terms(&statement.expr)
                .into_iter()
                .filter(|t| t.kind == TermKind::Call)
                .map(|t| callee_name(&t.text).to_string())
                .collect();
            calls.dedup();
            let expressions = nodes
                .get(position)
                .map(|n| slots_of(n, line.as_ref()))
                .unwrap_or_default();
            let (text, labels, note) = match line {
                Some(line) => (line.text, line.labels, line.note),
                None => (render(&statement.expr), Vec::new(), None),
            };
            ScriptLine {
                offset: statement.offset,
                text,
                labels,
                note,
                targets,
                calls,
                literals: literal_slots(&statement.expr),
                expressions,
            }
        })
        .collect()
}

/// What a statement names that is worth searching for or following.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TermKind {
    /// A string constant, which is where file names, URLs and messages live.
    String,
    /// A function called: a full object path for a final call, a bare name for a virtual one.
    Call,
    /// A property read, by its dotted path, a struct's member included.
    Read,
    /// A property assigned or changed in place: a `Let`'s target, a struct member set, an array
    /// or map built or changed, a delegate bound or changed, an event's parameter stored on the
    /// event graph's frame.
    Write,
    /// An object named outright, such as a class a cast or spawn takes.
    Object,
    /// A name constant, such as a row, a socket or a material parameter.
    Name,
    /// A function bound to a delegate, or the signature a multicast delegate broadcasts.
    Delegate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Term {
    pub kind: TermKind,
    pub text: String,
}

/// Everything `expr` names that can be searched for, in bytecode order.
pub fn statement_terms(expr: &Expr) -> Vec<Term> {
    let mut out = Vec::new();
    collect_terms(expr, false, &mut out);
    out
}

/// `writing` says `expr` is where a value is stored, so the variables it names are written.
fn collect_terms(expr: &Expr, writing: bool, out: &mut Vec<Term>) {
    let term = |kind, text: &str| Term {
        kind,
        text: text.to_string(),
    };
    let variable = if writing {
        TermKind::Write
    } else {
        TermKind::Read
    };
    match expr {
        Expr::StringConst { value, .. } | Expr::UnicodeStringConst { value, .. } => {
            out.push(term(TermKind::String, value));
        }
        Expr::FinalCall { function, .. } => {
            if let Some(path) = &function.path {
                out.push(term(TermKind::Call, path));
            }
        }
        Expr::VirtualCall { function, .. } => out.push(term(TermKind::Call, function)),
        Expr::Variable { property, .. } => out.push(term(variable, &property.path)),
        Expr::ObjectConst { object } | Expr::Cast { class: object, .. } => {
            if let Some(path) = &object.path {
                out.push(term(TermKind::Object, path));
            }
        }
        // An event's parameter copied onto the event graph's frame: the property is where the
        // value goes, and what it holds is the value.
        Expr::Member {
            name: "LetValueOnPersistentFrame",
            property,
            ..
        } => out.push(term(TermKind::Write, &property.path)),
        Expr::Member { property, .. } => out.push(term(variable, &property.path)),
        // An instance delegate reads as the function it names.
        Expr::NameConst {
            name: "InstanceDelegate",
            value,
            ..
        } => out.push(term(TermKind::Delegate, value)),
        Expr::NameConst { value, .. } => out.push(term(TermKind::Name, value)),
        Expr::BindDelegate { function, .. } => out.push(term(TermKind::Delegate, function)),
        Expr::CallMulticastDelegate { signature, .. } => {
            if let Some(path) = &signature.path {
                out.push(term(TermKind::Delegate, path));
            }
        }
        _ => {}
    }
    let written = written_child(expr, writing);
    for child in children(expr) {
        let writes = written.is_some_and(|w| std::ptr::eq(w, child));
        collect_terms(child, writes, out);
    }
}

/// The child `expr` stores a value in or changes, if any. `writing` says `expr` is itself where a
/// value is stored: the store then lands in the member a context reaches, the struct a member
/// belongs to and the array an element sits in, while the object reached through and the index
/// are only read.
fn written_child(expr: &Expr, writing: bool) -> Option<&Expr> {
    match expr {
        Expr::Let { variable, .. } => Some(&**variable),
        Expr::SetArray { array: target, .. } | Expr::SetContainer { target, .. } => Some(&**target),
        Expr::DelegateOp { delegate, .. } | Expr::BindDelegate { delegate, .. } => {
            Some(&**delegate)
        }
        Expr::Unary {
            name: "ClearMulticastDelegate",
            value,
        } => Some(&**value),
        Expr::FinalCall {
            function, params, ..
        } => changed_argument(function).and_then(|at| params.get(at)),
        Expr::Context { member, .. } if writing => Some(&**member),
        Expr::Member {
            name: "StructMemberContext",
            value,
            ..
        } if writing => Some(&**value),
        Expr::ArrayGetByRef { array, .. } if writing => Some(&**array),
        _ => None,
    }
}

/// Which argument `function` changes, for the engine functions known to change one they are
/// handed: an array, map or set, a timer handle, a random stream or a gameplay tag container.
/// Nothing in bytecode says a call changes an argument; these are the common ones, and engine
/// functions do not change with the game. A timer function's handle follows the world context
/// the compiler passes first.
fn changed_argument(function: &ObjectRef) -> Option<usize> {
    let (class, name) = function.path.as_deref()?.split_once(':')?;
    match (class, name) {
        (
            "/Script/Engine.KismetSystemLibrary",
            "K2_ClearAndInvalidateTimerHandle" | "K2_ClearAndInvalidateCommonTimerHandle",
        ) => Some(1),
        (
            "/Script/Engine.KismetArrayLibrary",
            "Array_Add" | "Array_AddUnique" | "Array_Append" | "Array_Clear" | "Array_Insert"
            | "Array_Remove" | "Array_RemoveItem" | "Array_Resize" | "Array_Reverse" | "Array_Set"
            | "Array_Shuffle" | "Array_Swap",
        )
        | ("/Script/Engine.BlueprintMapLibrary", "Map_Add" | "Map_Remove" | "Map_Clear")
        | (
            "/Script/Engine.BlueprintSetLibrary",
            "Set_Add" | "Set_AddItems" | "Set_Remove" | "Set_RemoveItems" | "Set_Clear",
        )
        | ("/Script/Engine.KismetSystemLibrary", "K2_InvalidateTimerHandle")
        | (
            "/Script/Engine.KismetMathLibrary",
            "SetRandomStreamSeed" | "SeedRandomStream" | "ResetRandomStream",
        )
        | (
            "/Script/GameplayTags.BlueprintGameplayTagLibrary",
            "AddGameplayTag" | "RemoveGameplayTag" | "AppendGameplayTagContainers",
        ) => Some(0),
        _ => None,
    }
}

/// Where each function is called from, keyed by the callee's bare name: the calling function and
/// the offset of the statement that makes the call. `scripts` pairs each function with its script.
pub fn call_sites<'a>(
    scripts: impl IntoIterator<Item = (&'a str, &'a Script)>,
) -> BTreeMap<String, Vec<(String, u32)>> {
    let mut sites: BTreeMap<String, Vec<(String, u32)>> = BTreeMap::new();
    for (caller, script) in scripts {
        for statement in &script.statements {
            let mut seen = Vec::new();
            for term in statement_terms(&statement.expr) {
                if term.kind != TermKind::Call {
                    continue;
                }
                let callee = callee_name(&term.text).to_string();
                if seen.contains(&callee) {
                    continue;
                }
                sites
                    .entry(callee.clone())
                    .or_default()
                    .push((caller.to_string(), statement.offset));
                seen.push(callee);
            }
        }
    }
    sites
}

/// The function a call names, without the class that owns it: `Class_C:MountMods` is `MountMods`.
pub fn callee_name(call: &str) -> &str {
    call.rsplit(':').next().unwrap_or(call)
}

/// A latent action carries its resume point as an `Offset` constant inside `LatentActionInfo`,
/// which is why constants count here alongside the jumps.
fn flow_targets(expr: &Expr, out: &mut Vec<u32>) {
    match expr {
        Expr::Jump { target }
        | Expr::JumpIfNot { target, .. }
        | Expr::PushExecutionFlow { target } => out.push(*target),
        Expr::SkipOffsetConst { value } => out.push(*value),
        _ => {}
    }
    for child in children(expr) {
        flow_targets(child, out);
    }
}

/// The events that run inside a Blueprint's Ubergraph, keyed by the Ubergraph function's name.
///
/// Each event is a stub function whose whole body calls `ExecuteUbergraph_<Class>(N)`; `N` is the
/// offset in the Ubergraph where that event's code starts. `scripts` pairs each function's name
/// with its script.
pub fn ubergraph_entries<'a>(
    scripts: impl IntoIterator<Item = (&'a str, &'a Script)>,
) -> BTreeMap<String, Vec<(u32, String)>> {
    let mut entries: BTreeMap<String, Vec<(u32, String)>> = BTreeMap::new();
    for (name, script) in scripts {
        for statement in &script.statements {
            let Expr::FinalCall {
                function, params, ..
            } = &statement.expr
            else {
                continue;
            };
            let Some(callee) = function
                .path
                .as_deref()
                .and_then(|p| p.rsplit(':').next())
                .filter(|f| f.starts_with("ExecuteUbergraph"))
            else {
                continue;
            };
            if let [Expr::IntConst { value, .. }] = params.as_slice() {
                entries
                    .entry(callee.to_string())
                    .or_default()
                    .push((*value as u32, name.to_string()));
            }
        }
    }
    for events in entries.values_mut() {
        events.sort();
    }
    entries
}

/// One statement per line, offsets as a jump would name them.
pub fn render_script(script: &Script) -> String {
    let mut out = String::new();
    for statement in &script.statements {
        out.push_str(&format!(
            "0x{:04X}  {}\n",
            statement.offset,
            render(&statement.expr)
        ));
    }
    if let Some(stop) = &script.stopped {
        out.push_str(&format!(
            "!! stopped at 0x{:04X} (file {:#X}) on token {:#04X} {}: {}\n",
            stop.offset,
            stop.at,
            stop.token,
            token_name(stop.token).unwrap_or("unknown"),
            stop.reason
        ));
    }
    out
}

/// Every literal in `expr` in bytecode order: the constants a value can be written into, plus
/// the forms that cannot take one, so a count taken from the disassembly lands on the same node.
pub fn literals(expr: &Expr) -> Vec<&Expr> {
    let mut out = Vec::new();
    collect_literals(expr, &mut out);
    out
}

fn is_literal(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::IntConst { .. }
            | Expr::Int64Const { .. }
            | Expr::UInt64Const { .. }
            | Expr::FloatConst { .. }
            | Expr::DoubleConst { .. }
            | Expr::ByteConst { .. }
            | Expr::StringConst { .. }
            | Expr::UnicodeStringConst { .. }
            | Expr::NameConst { .. }
            | Expr::Numbers { .. }
            | Expr::ObjectConst { .. }
            | Expr::Simple {
                name: "IntZero" | "IntOne" | "True" | "False"
            }
    )
}

/// Mirrors the field order `Reader::body` parses in, which is the order the bytes sit in.
fn collect_literals<'a>(expr: &'a Expr, out: &mut Vec<&'a Expr>) {
    if is_literal(expr) {
        out.push(expr);
        return;
    }
    for child in children(expr) {
        collect_literals(child, out);
    }
}

/// The expressions `expr` holds, in the order their bytes sit in.
pub fn children(expr: &Expr) -> Vec<&Expr> {
    let mut out: Vec<&Expr> = Vec::new();
    match expr {
        Expr::Return { value }
        | Expr::Skip { value, .. }
        | Expr::Conversion { value, .. }
        | Expr::Unary { value, .. }
        | Expr::Member { value, .. }
        | Expr::Cast { value, .. } => out.push(value),
        Expr::JumpIfNot { condition, .. } | Expr::Assert { condition, .. } => out.push(condition),
        Expr::Let {
            variable, value, ..
        } => {
            out.push(variable);
            out.push(value);
        }
        Expr::Context { object, member, .. } => {
            out.push(object);
            out.push(member);
        }
        Expr::VirtualCall { params, .. } | Expr::FinalCall { params, .. } => out.extend(params),
        Expr::StructConst { fields, .. } => out.extend(fields),
        Expr::SetArray { array, items } => {
            out.push(array);
            out.extend(items);
        }
        Expr::SetContainer { target, items, .. } => {
            out.push(target);
            out.extend(items);
        }
        Expr::ContainerConst { items, .. } | Expr::MapConst { items, .. } => out.extend(items),
        Expr::ComputedJump { target } => out.push(target),
        Expr::DelegateOp {
            delegate, value, ..
        } => {
            out.push(delegate);
            out.push(value);
        }
        Expr::BindDelegate {
            delegate, object, ..
        } => {
            out.push(delegate);
            out.push(object);
        }
        Expr::CallMulticastDelegate {
            delegate, params, ..
        } => {
            out.push(delegate);
            out.extend(params);
        }
        Expr::SwitchValue {
            index,
            cases,
            default,
            ..
        } => {
            out.push(index);
            for case in cases {
                out.push(&case.value);
                out.push(&case.result);
            }
            out.push(default);
        }
        Expr::ArrayGetByRef { array, index } => {
            out.push(array);
            out.push(index);
        }
        Expr::TextConst { text } => match text {
            TextLiteral::Empty => {}
            TextLiteral::Localized {
                source,
                key,
                namespace,
            } => {
                out.push(source);
                out.push(key);
                out.push(namespace);
            }
            TextLiteral::Invariant { source } | TextLiteral::Literal { source } => out.push(source),
            TextLiteral::StringTable { table_id, key, .. } => {
                out.push(table_id);
                out.push(key);
            }
        },
        _ => {}
    }
    out
}

/// [`children`], for rewriting them in place.
pub(crate) fn children_mut(expr: &mut Expr) -> Vec<&mut Expr> {
    let mut out: Vec<&mut Expr> = Vec::new();
    match expr {
        Expr::Return { value }
        | Expr::Skip { value, .. }
        | Expr::Conversion { value, .. }
        | Expr::Unary { value, .. }
        | Expr::Member { value, .. }
        | Expr::Cast { value, .. } => out.push(value),
        Expr::JumpIfNot { condition, .. } | Expr::Assert { condition, .. } => out.push(condition),
        Expr::Let {
            variable, value, ..
        } => {
            out.push(variable);
            out.push(value);
        }
        Expr::Context { object, member, .. } => {
            out.push(object);
            out.push(member);
        }
        Expr::VirtualCall { params, .. } | Expr::FinalCall { params, .. } => out.extend(params),
        Expr::StructConst { fields, .. } => out.extend(fields),
        Expr::SetArray { array, items } => {
            out.push(array);
            out.extend(items);
        }
        Expr::SetContainer { target, items, .. } => {
            out.push(target);
            out.extend(items);
        }
        Expr::ContainerConst { items, .. } | Expr::MapConst { items, .. } => out.extend(items),
        Expr::ComputedJump { target } => out.push(target),
        Expr::DelegateOp {
            delegate, value, ..
        } => {
            out.push(delegate);
            out.push(value);
        }
        Expr::BindDelegate {
            delegate, object, ..
        } => {
            out.push(delegate);
            out.push(object);
        }
        Expr::CallMulticastDelegate {
            delegate, params, ..
        } => {
            out.push(delegate);
            out.extend(params);
        }
        Expr::SwitchValue {
            index,
            cases,
            default,
            ..
        } => {
            out.push(index);
            for case in cases {
                out.push(&mut case.value);
                out.push(&mut case.result);
            }
            out.push(default);
        }
        Expr::ArrayGetByRef { array, index } => {
            out.push(array);
            out.push(index);
        }
        Expr::TextConst { text } => match text {
            TextLiteral::Empty => {}
            TextLiteral::Localized {
                source,
                key,
                namespace,
            } => {
                out.push(source);
                out.push(key);
                out.push(namespace);
            }
            TextLiteral::Invariant { source } | TextLiteral::Literal { source } => out.push(source),
            TextLiteral::StringTable { table_id, key, .. } => {
                out.push(table_id);
                out.push(key);
            }
        },
        _ => {}
    }
    out
}

/// Visits every expression in pre-order, the order the decoder records spans in, for rewriting.
pub(crate) fn visit_mut(expr: &mut Expr, visit: &mut impl FnMut(&mut Expr)) {
    visit(expr);
    for child in children_mut(expr) {
        visit_mut(child, visit);
    }
}

/// A statement as a comparison should see it: where its bytes sit in the file, and the path an
/// object resolves to, are left out, so a script that moved compares equal to itself. So are the
/// lengths a context and a switch hold, which the decoder measures against the code they cover
/// and locks the script over when they disagree, and the table entries names were stored as, which
/// an edit naming something new does not know until it is written. The debug form keeps every
/// other operand, a float as its exact value and a NaN as one.
pub fn shape(expr: &Expr) -> String {
    let mut copy = expr.clone();
    visit_mut(&mut copy, &mut |node| {
        match node {
            Expr::Context { skip, .. } => *skip = 0,
            Expr::SwitchValue { end, cases, .. } => {
                *end = 0;
                for case in cases {
                    case.next = 0;
                }
            }
            _ => {}
        }
        match node {
            Expr::IntConst { at, .. }
            | Expr::Int64Const { at, .. }
            | Expr::UInt64Const { at, .. }
            | Expr::FloatConst { at, .. }
            | Expr::DoubleConst { at, .. }
            | Expr::ByteConst { at, .. }
            | Expr::StringConst { at, .. }
            | Expr::UnicodeStringConst { at, .. }
            | Expr::NameConst { at, .. }
            | Expr::Numbers { at, .. } => *at = 0,
            _ => {}
        }
        match node {
            Expr::VirtualCall { id, .. }
            | Expr::NameConst { id, .. }
            | Expr::BindDelegate { id, .. }
            | Expr::InstrumentationEvent { id, .. } => *id = NameId::default(),
            _ => {}
        }
        for object in objects_mut(node) {
            object.path = None;
        }
        for property in properties_mut(node) {
            property.names.clear();
        }
    });
    format!("{copy:?}")
}

/// The field paths an expression holds itself, not those of its children.
pub(crate) fn properties_mut(expr: &mut Expr) -> Vec<&mut PropertyRef> {
    match expr {
        Expr::Variable { property, .. }
        | Expr::BitFieldConst { property, .. }
        | Expr::PropertyConst { property }
        | Expr::Member { property, .. }
        | Expr::ContainerConst { property, .. }
        | Expr::Context { property, .. }
        | Expr::Let {
            property: Some(property),
            ..
        } => vec![property],
        Expr::MapConst { key, value, .. } => vec![key, value],
        _ => Vec::new(),
    }
}

/// The object operands an expression holds itself, not those of its children.
fn objects_mut(expr: &mut Expr) -> Vec<&mut ObjectRef> {
    match expr {
        Expr::Variable { property, .. }
        | Expr::BitFieldConst { property, .. }
        | Expr::PropertyConst { property }
        | Expr::Member { property, .. }
        | Expr::ContainerConst { property, .. } => vec![&mut property.owner],
        Expr::Let {
            property: Some(property),
            ..
        } => vec![&mut property.owner],
        Expr::Context { property, .. } => vec![&mut property.owner],
        Expr::MapConst { key, value, .. } => vec![&mut key.owner, &mut value.owner],
        Expr::Cast { class, .. } => vec![class],
        Expr::FinalCall { function, .. } => vec![function],
        Expr::ObjectConst { object } => vec![object],
        Expr::StructConst { struct_type, .. } => vec![struct_type],
        Expr::CallMulticastDelegate { signature, .. } => vec![signature],
        Expr::TextConst {
            text: TextLiteral::StringTable { table, .. },
        } => vec![table],
        _ => Vec::new(),
    }
}

/// The token name of a literal, as the disassembly and the refusals call it.
pub(crate) fn literal_kind(literal: &Expr) -> &'static str {
    match literal {
        Expr::IntConst { .. } => "IntConst",
        Expr::Int64Const { .. } => "Int64Const",
        Expr::UInt64Const { .. } => "UInt64Const",
        Expr::FloatConst { .. } => "FloatConst",
        Expr::DoubleConst { .. } => "DoubleConst",
        Expr::StringConst { .. } => "StringConst",
        Expr::UnicodeStringConst { .. } => "UnicodeStringConst",
        Expr::ObjectConst { .. } => "ObjectConst",
        Expr::ByteConst { name, .. }
        | Expr::NameConst { name, .. }
        | Expr::Numbers { name, .. }
        | Expr::Simple { name } => name,
        _ => "expression",
    }
}

/// The `nth` literal of the statement whose loaded offset is `statement`, the way the disassembly
/// addresses them. A miss lists what the statement does hold.
pub(crate) fn literal_at(script: &Script, statement: u32, nth: u32) -> Result<&Expr, String> {
    let found = script
        .statements
        .iter()
        .find(|s| s.offset == statement)
        .ok_or_else(|| {
            format!(
                "no statement starts at 0x{statement:04X}; the disassembly names each one by the offset at the start of its line"
            )
        })?;
    let all = literals(&found.expr);
    all.get(nth as usize).copied().ok_or_else(|| {
        if all.is_empty() {
            return format!("the statement at 0x{statement:04X} holds no literal constant");
        }
        let listing: Vec<String> = all
            .iter()
            .enumerate()
            .map(|(i, literal)| format!("  [{i}] {} {}", literal_kind(literal), render(literal)))
            .collect();
        format!(
            "the statement at 0x{statement:04X} holds {} literal constant(s), so there is no constant {nth}:\n{}",
            all.len(),
            listing.join("\n")
        )
    })
}

/// How many bytes the literal's value takes on disk, after its token byte.
pub(crate) fn stored_width(literal: &Expr) -> u64 {
    match literal {
        Expr::IntConst { .. } | Expr::FloatConst { .. } | Expr::ObjectConst { .. } => 4,
        Expr::Int64Const { .. }
        | Expr::UInt64Const { .. }
        | Expr::DoubleConst { .. }
        | Expr::NameConst { .. } => 8,
        Expr::ByteConst { .. } => 1,
        Expr::StringConst { value, .. } => value.chars().count() as u64 + 1,
        Expr::UnicodeStringConst { value, .. } => (value.encode_utf16().count() as u64 + 1) * 2,
        Expr::Numbers { name, values, .. } => numbers_width(name) * values.len() as u64,
        _ => 0,
    }
}

fn numbers_width(name: &str) -> u64 {
    if name == "Vector3fConst" { 4 } else { 8 }
}

/// The same literal with `text` as its value, refused when the value would not fit the bytes
/// the old one took: a script constant can only be replaced at its own width.
pub(crate) fn with_value(literal: &Expr, text: &str) -> Result<Expr, String> {
    // Numbers forgive surrounding whitespace; a string or a name is taken exactly as given.
    let number = text.trim();
    let parse_error =
        |kind: &str, what: &str| format!("{text:?} is not {what}, which is what a {kind} holds");
    Ok(match literal {
        Expr::IntConst { at, .. } => Expr::IntConst {
            value: number
                .parse()
                .map_err(|_| parse_error("IntConst", "a 32-bit integer"))?,
            at: *at,
        },
        Expr::Int64Const { at, .. } => Expr::Int64Const {
            value: number
                .parse()
                .map_err(|_| parse_error("Int64Const", "a 64-bit integer"))?,
            at: *at,
        },
        Expr::UInt64Const { at, .. } => Expr::UInt64Const {
            value: number
                .parse()
                .map_err(|_| parse_error("UInt64Const", "an unsigned 64-bit integer"))?,
            at: *at,
        },
        Expr::FloatConst { at, .. } => Expr::FloatConst {
            value: number
                .trim_end_matches(['f', 'F'])
                .parse()
                .map_err(|_| parse_error("FloatConst", "a number"))?,
            at: *at,
        },
        Expr::DoubleConst { at, .. } => Expr::DoubleConst {
            value: number
                .parse()
                .map_err(|_| parse_error("DoubleConst", "a number"))?,
            at: *at,
        },
        Expr::ByteConst { name, at, .. } => Expr::ByteConst {
            name,
            value: number
                .parse()
                .map_err(|_| parse_error(name, "a byte from 0 to 255"))?,
            at: *at,
        },
        Expr::StringConst { value, at } => {
            if let Some(wide) = text.chars().find(|c| u32::from(*c) > 0xFF) {
                return Err(format!(
                    "{wide:?} does not fit a StringConst, which holds one byte per character; only a UnicodeStringConst can carry it"
                ));
            }
            let (was, now) = (value.chars().count(), text.chars().count());
            if was != now {
                return Err(format!(
                    "{value:?} is {was} character(s) stored as {} bytes; the replacement {text:?} is {now}. A script constant can only be replaced at its own width",
                    was + 1
                ));
            }
            Expr::StringConst {
                value: text.to_string(),
                at: *at,
            }
        }
        Expr::UnicodeStringConst { value, at } => {
            let (was, now) = (value.encode_utf16().count(), text.encode_utf16().count());
            if was != now {
                return Err(format!(
                    "{value:?} is {was} UTF-16 unit(s) stored as {} bytes; the replacement {text:?} is {now}. A script constant can only be replaced at its own width",
                    (was + 1) * 2
                ));
            }
            Expr::UnicodeStringConst {
                value: text.to_string(),
                at: *at,
            }
        }
        Expr::NameConst { name, at, .. } => {
            if text.is_empty() {
                return Err(format!("a {name} needs a name; an empty one is not a name"));
            }
            Expr::NameConst {
                name,
                value: text.to_string(),
                at: *at,
                id: NameId::default(),
            }
        }
        Expr::Numbers { name, values, at } => {
            let parsed: Result<Vec<f64>, String> = number
                .split(',')
                .map(|part| {
                    part.trim()
                        .parse::<f64>()
                        .map_err(|_| parse_error(name, "a comma-separated run of numbers"))
                })
                .collect();
            let mut parsed = parsed?;
            if parsed.len() != values.len() {
                return Err(format!(
                    "{name} takes {} numbers, not {}",
                    values.len(),
                    parsed.len()
                ));
            }
            if numbers_width(name) == 4 {
                for value in &mut parsed {
                    *value = f64::from(*value as f32);
                }
            }
            Expr::Numbers {
                name,
                values: parsed,
                at: *at,
            }
        }
        Expr::Simple { name } => {
            return Err(format!(
                "{name} is a one-byte form with no value bytes. It cannot take a value in place: a full constant would be longer and move every jump after it"
            ));
        }
        Expr::ObjectConst { .. } => {
            return Err(
                "an ObjectConst is an object reference, not a literal; only literal values can be set this way"
                    .to_string(),
            );
        }
        other => return Err(format!("{} is not a literal constant", literal_kind(other))),
    })
}

/// The value bytes a literal writes after its token, in the package's byte order. A name not in
/// the table is appended to it, which is how the header learns it grew.
pub(crate) fn literal_bytes(
    literal: &Expr,
    names: &mut FPackageNameMap,
) -> Result<Vec<u8>, String> {
    Ok(match literal {
        Expr::IntConst { value, .. } => value.to_le_bytes().to_vec(),
        Expr::Int64Const { value, .. } => value.to_le_bytes().to_vec(),
        Expr::UInt64Const { value, .. } => value.to_le_bytes().to_vec(),
        Expr::FloatConst { value, .. } => value.to_le_bytes().to_vec(),
        Expr::DoubleConst { value, .. } => value.to_le_bytes().to_vec(),
        Expr::ByteConst { value, .. } => vec![*value],
        Expr::StringConst { value, .. } => {
            let mut out: Vec<u8> = value.chars().map(|c| u32::from(c) as u8).collect();
            out.push(0);
            out
        }
        Expr::UnicodeStringConst { value, .. } => {
            let mut out = Vec::new();
            for unit in value.encode_utf16() {
                out.extend_from_slice(&unit.to_le_bytes());
            }
            out.extend_from_slice(&[0, 0]);
            out
        }
        Expr::NameConst { value, .. } => {
            let stored = names.store(value);
            let mut out = Vec::with_capacity(8);
            out.extend_from_slice(&stored.index.to_le_bytes());
            out.extend_from_slice(&stored.number.to_le_bytes());
            out
        }
        Expr::Numbers { name, values, .. } => {
            let mut out = Vec::new();
            for value in values {
                if numbers_width(name) == 4 {
                    out.extend_from_slice(&(*value as f32).to_le_bytes());
                } else {
                    out.extend_from_slice(&value.to_le_bytes());
                }
            }
            out
        }
        other => {
            return Err(format!(
                "{} has no value bytes to write",
                literal_kind(other)
            ));
        }
    })
}

/// `CallFunc_<Function>_<Result>[_N]` is how a Blueprint names the local a call fills.
fn called_function(local: &str) -> Option<&str> {
    let leaf = local.rsplit(['.', ':']).next().unwrap_or(local);
    let rest = leaf.strip_prefix("CallFunc_")?;
    let rest = match rest.rsplit_once('_') {
        Some((head, number)) if number.bytes().all(|b| b.is_ascii_digit()) => head,
        _ => rest,
    };
    rest.rsplit_once('_').map(|(function, _)| function)
}

/// The function an unresolved call target probably was, read off the locals around it: the
/// variable a `Let` fills, else the one distinct name the out-params carry.
fn probable_function(variable: Option<&Expr>, params: &[Expr]) -> Option<String> {
    if let Some(Expr::Variable { property, .. }) = variable
        && let Some(function) = called_function(&property.path)
    {
        return Some(function.to_string());
    }
    let mut candidates: Vec<&str> = params
        .iter()
        .filter_map(|param| match param {
            Expr::Variable { property, .. } => called_function(&property.path),
            _ => None,
        })
        .collect();
    candidates.sort_unstable();
    candidates.dedup();
    match candidates.as_slice() {
        [only] => Some((*only).to_string()),
        _ => None,
    }
}

fn is_unresolved_target(function: &ObjectRef) -> bool {
    function
        .path
        .as_deref()
        .and_then(|path| path.rsplit(['.', ':']).next())
        .is_some_and(crate::package::is_unresolved_import_name)
}

/// The function an unresolved call probably was, read off the locals around it. `None` for a call
/// the package resolves.
pub(crate) fn probable_call(
    function: &ObjectRef,
    params: &[Expr],
    variable: Option<&Expr>,
) -> Option<String> {
    is_unresolved_target(function)
        .then(|| probable_function(variable, params))
        .flatten()
}

/// An expression in the assembler text's syntax, without the package around it: what a message
/// or an edit's `was` quotes.
pub(crate) fn render(expr: &Expr) -> String {
    crate::script_text::print_expr(expr)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use retoc::legacy_asset::{FLegacyPackageHeader, FPackageNameMap};

    const NAMES: &[&str] = &[
        "None",
        "EntryPoint",
        "Damage",
        "OnFired",
        "Target",
        "LatentActionInfo",
        "ExecuteUbergraph_X",
    ];

    /// Index -1: the one import every test names, so an object operand resolves to a path.
    const TARGET: i32 = -1;
    /// Index -2: `LatentActionInfo`, the struct a latent action's resume point travels in.
    const LATENT_INFO: i32 = -2;

    fn header() -> FLegacyPackageHeader {
        let index = NAMES
            .iter()
            .position(|n| *n == "Target")
            .expect("the target name") as i32;
        let none = retoc::legacy_asset::FMinimalName {
            index: 0,
            number: 0,
        };
        FLegacyPackageHeader {
            name_map: FPackageNameMap::create_from_names(
                NAMES.iter().map(|n| (*n).to_string()).collect(),
            ),
            imports: vec![
                retoc::legacy_asset::FObjectImport {
                    class_package: none,
                    class_name: none,
                    outer_index: retoc::zen::FPackageIndex::create_null(),
                    object_name: retoc::legacy_asset::FMinimalName { index, number: 0 },
                    is_optional: false,
                },
                retoc::legacy_asset::FObjectImport {
                    class_package: none,
                    class_name: none,
                    outer_index: retoc::zen::FPackageIndex::create_null(),
                    object_name: retoc::legacy_asset::FMinimalName {
                        index: NAMES
                            .iter()
                            .position(|n| *n == "LatentActionInfo")
                            .expect("the struct name") as i32,
                        number: 0,
                    },
                    is_optional: false,
                },
            ],
            ..Default::default()
        }
    }

    fn name_bytes(out: &mut Vec<u8>, value: &str) {
        let index = NAMES
            .iter()
            .position(|n| *n == value)
            .expect("a name in the test map") as i32;
        out.extend_from_slice(&index.to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
    }

    /// An `FFieldPath`: one name and the owner index.
    fn field_path(out: &mut Vec<u8>, value: &str, owner: i32) {
        out.extend_from_slice(&1i32.to_le_bytes());
        name_bytes(out, value);
        out.extend_from_slice(&owner.to_le_bytes());
    }

    /// The lines of a script that belongs to no package of its own.
    fn lines_of(script: &Script) -> Vec<ScriptLine> {
        let parsed =
            crate::package::ParsedPackage::of_exports(vec![crate::package::ParsedExport {
                script: Some(script.clone()),
                ..crate::package::ParsedExport::blank(0)
            }]);
        script_lines(&parsed, 0)
    }

    fn decode(bytes: &[u8], buffer_size: Option<u32>) -> (Script, Diagnostics) {
        let header = header();
        let ctx = Ctx {
            mappings: None,
            header: &header,
            fixups: None,
            synth: None,
            local: None,
        };
        let mut diagnostics = Diagnostics::default();
        let script = read_script(
            bytes,
            0,
            0,
            buffer_size,
            bytes.len() as u32,
            &ctx,
            &mut diagnostics,
        );
        (script, diagnostics)
    }

    /// The bytes `ReceiveBeginPlay` stores in `SimpleSelectHeroLevel.umap`, with the indices moved
    /// into range: a local call, a return and the end marker. Fourteen bytes on disk, eighteen
    /// loaded, which is exactly the gap an object pointer opens.
    #[test]
    fn a_level_blueprint_event_decodes_to_its_loaded_size() {
        let mut data = vec![0x46];
        data.extend_from_slice(&TARGET.to_le_bytes());
        data.push(0x1D);
        data.extend_from_slice(&411i32.to_le_bytes());
        data.extend_from_slice(&[0x16, 0x04, 0x0B, 0x53]);
        assert_eq!(data.len(), 14);

        let (script, _) = decode(&data, Some(18));
        assert!(script.complete(), "{:?}", script.stopped);
        assert_eq!(script.decoded_size, 18);
        assert_eq!(
            script
                .statements
                .iter()
                .map(|s| s.offset)
                .collect::<Vec<_>>(),
            [0, 15, 17],
            "the call takes fifteen loaded bytes: a token, a pointer, a constant and the end marker"
        );
        let Expr::FinalCall {
            function, params, ..
        } = &script.statements[0].expr
        else {
            panic!("expected a local final call");
        };
        assert_eq!(function.index, TARGET);
        assert!(matches!(params[0], Expr::IntConst { value: 411, .. }));
    }

    /// Every object a script names is recorded, so a removal can find it. A stopped walk keeps
    /// none of them: an index read at the wrong offset would be renumbered into a lie.
    #[test]
    fn references_are_kept_only_when_the_whole_script_decodes() {
        let mut good = vec![0x20];
        good.extend_from_slice(&TARGET.to_le_bytes());
        good.push(0x53);
        let (script, diagnostics) = decode(&good, None);
        assert!(script.complete());
        assert_eq!(diagnostics.references.len(), 1);
        assert_eq!(diagnostics.references[0].index, TARGET);

        let mut bad = vec![0x20];
        bad.extend_from_slice(&TARGET.to_le_bytes());
        bad.push(0xFE);
        let (script, diagnostics) = decode(&bad, None);
        let stop = script.stopped.expect("an unknown token stops the walk");
        assert_eq!(stop.token, 0xFE);
        assert_eq!(
            stop.offset, 9,
            "the loaded offset of the token that stopped"
        );
        assert!(diagnostics.references.is_empty());
    }

    /// The declared loaded size is the oracle. A reader that took a name for eight bytes rather
    /// than twelve would still produce plausible instructions, and this is what catches it.
    #[test]
    fn a_loaded_size_that_does_not_match_is_a_stop() {
        let mut data = vec![0x21];
        name_bytes(&mut data, "Damage");
        data.push(0x53);
        let (script, _) = decode(&data, Some(14));
        assert!(script.complete(), "12 for the name and 1 each side");
        assert_eq!(script.decoded_size, 14);

        let (script, _) = decode(&data, Some(10));
        let stop = script.stopped.expect("a mismatch stops the walk");
        assert!(stop.reason.contains("declares 10"), "{}", stop.reason);
    }

    /// Variables and assignments carry field paths, which are wide on disk and a pointer loaded.
    #[test]
    fn a_let_reads_its_property_then_both_sides() {
        let mut data = vec![0x0F];
        field_path(&mut data, "Damage", TARGET);
        data.push(0x00);
        field_path(&mut data, "Damage", TARGET);
        data.push(0x1D);
        data.extend_from_slice(&5i32.to_le_bytes());
        data.push(0x53);

        let (script, _) = decode(&data, None);
        assert!(script.complete(), "{:?}", script.stopped);
        // Let, the local variable, the constant and EndOfScript: 1 + 8 + 1 + 8 + 1 + 4 + 1.
        assert_eq!(script.decoded_size, 24);
        let Expr::Let {
            property, value, ..
        } = &script.statements[0].expr
        else {
            panic!("expected a let");
        };
        assert_eq!(property.as_ref().expect("a property").path, "Damage");
        assert!(matches!(**value, Expr::IntConst { value: 5, .. }));
    }

    /// Jumps and flow pushes are offsets into the loaded script, not the stored one.
    #[test]
    fn jumps_carry_loaded_offsets() {
        let mut data = vec![0x4C];
        data.extend_from_slice(&0x1A0u32.to_le_bytes());
        data.push(0x06);
        data.extend_from_slice(&0x20u32.to_le_bytes());
        data.push(0x53);
        let (script, _) = decode(&data, None);
        assert!(script.complete());
        assert!(matches!(
            script.statements[0].expr,
            Expr::PushExecutionFlow { target: 0x1A0 }
        ));
        assert!(matches!(
            script.statements[1].expr,
            Expr::Jump { target: 0x20 }
        ));
        assert_eq!(
            script
                .statements
                .iter()
                .map(|s| s.offset)
                .collect::<Vec<_>>(),
            [0, 5, 10]
        );
    }

    /// Containers close on their own terminators, and each terminator is a token of its own.
    #[test]
    fn containers_read_until_their_terminators() {
        let mut data = vec![0x31, 0x00];
        field_path(&mut data, "Damage", TARGET);
        data.push(0x1D);
        data.extend_from_slice(&TARGET.to_le_bytes());
        data.push(0x1D);
        data.extend_from_slice(&2i32.to_le_bytes());
        data.push(0x32);
        data.push(0x53);
        let (script, _) = decode(&data, None);
        assert!(script.complete(), "{:?}", script.stopped);
        let Expr::SetArray { items, .. } = &script.statements[0].expr else {
            panic!("expected a set array");
        };
        assert_eq!(items.len(), 2);
    }

    /// A call gathers parameters until the end marker, whatever they are.
    #[test]
    fn a_call_gathers_its_parameters() {
        let mut data = vec![0x1B];
        name_bytes(&mut data, "OnFired");
        data.push(0x17);
        data.push(0x27);
        data.push(0x16);
        data.push(0x53);
        let (script, _) = decode(&data, None);
        assert!(script.complete(), "{:?}", script.stopped);
        let Expr::VirtualCall {
            function, params, ..
        } = &script.statements[0].expr
        else {
            panic!("expected a virtual call");
        };
        assert_eq!(function, "OnFired");
        assert_eq!(params.len(), 2);
    }

    /// A switch reads its case count, then a value, a next offset and a result for each.
    #[test]
    fn a_switch_reads_every_case_and_its_default() {
        let mut data = vec![0x69];
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&0x30u32.to_le_bytes());
        data.push(0x1D);
        data.extend_from_slice(&7i32.to_le_bytes());
        data.push(0x1D);
        data.extend_from_slice(&7i32.to_le_bytes());
        data.extend_from_slice(&0x20u32.to_le_bytes());
        data.push(0x26);
        data.push(0x25);
        data.push(0x53);
        let (script, _) = decode(&data, None);
        assert!(script.complete(), "{:?}", script.stopped);
        let Expr::SwitchValue { cases, .. } = &script.statements[0].expr else {
            panic!("expected a switch");
        };
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].next, 0x20);
    }

    /// Constants take fixed widths, and the loaded size is what proves each one right.
    #[test]
    fn constants_take_the_widths_they_declare() {
        let cases: Vec<(Vec<u8>, u32)> = vec![
            (vec![0x22; 1].into_iter().chain([0u8; 24]).collect(), 25),
            (vec![0x41].into_iter().chain([0u8; 12]).collect(), 13),
            (vec![0x2B].into_iter().chain([0u8; 80]).collect(), 81),
            (vec![0x35].into_iter().chain([0u8; 8]).collect(), 9),
            (vec![0x37].into_iter().chain([0u8; 8]).collect(), 9),
            (vec![0x24, 3], 2),
            (vec![0x29, 0], 2),
        ];
        for (bytes, loaded) in cases {
            let (script, _) = decode(&bytes, Some(loaded));
            assert!(
                script.complete(),
                "{:#04X} did not read whole: {:?}",
                bytes[0],
                script.stopped
            );
        }
    }

    /// A call whose target retoc could not name is labelled with the function the Blueprint's own
    /// locals were named after, and only when that reading is unambiguous.
    #[test]
    fn an_unresolved_call_is_labelled_with_the_function_its_locals_name() {
        let unresolved = ObjectRef {
            index: -57,
            path: Some("/Engine/UnknownPackage.UnknownExport".to_string()),
        };
        let local = |path: &str| Expr::Variable {
            name: "LocalVariable",
            property: PropertyRef {
                names: Vec::new(),
                path: path.to_string(),
                owner: ObjectRef {
                    index: 6,
                    path: None,
                },
            },
        };
        let call = |params: Vec<Expr>| Expr::FinalCall {
            name: "CallMath",
            function: unresolved.clone(),
            params,
        };

        let filled = Expr::Let {
            name: "LetBool",
            property: None,
            variable: Box::new(local("CallFunc_MountPak_ReturnValue")),
            value: Box::new(call(vec![
                local("CallFunc_Array_Get_Item_1"),
                local("CallFunc_Add_IntInt_ReturnValue_1"),
            ])),
        };
        // The note sits beside the statement, out of the text the assembler reads.
        let note = |expr: &Expr| {
            lines_of(&script_of(vec![(0, expr.clone())]))[0]
                .note
                .clone()
        };
        assert_eq!(note(&filled).as_deref(), Some("probably MountPak"));

        let out_param = call(vec![local("CallFunc_GetMountedPakNames_PakFilenames_1")]);
        assert_eq!(
            note(&out_param).as_deref(),
            Some("probably GetMountedPakNames")
        );

        let ambiguous = call(vec![
            local("CallFunc_Array_Get_Item_1"),
            local("CallFunc_Add_IntInt_ReturnValue_1"),
        ]);
        assert_eq!(note(&ambiguous), None);

        let resolved = Expr::FinalCall {
            name: "CallMath",
            function: ObjectRef {
                index: -33,
                path: Some("/Script/Engine.KismetMathLibrary:Add_IntInt".to_string()),
            },
            params: vec![local("CallFunc_Subtract_IntInt_ReturnValue_1")],
        };
        assert_eq!(note(&resolved), None);
    }

    /// A literal knows where its token sits, and the walk hands literals back in bytecode order
    /// under the address the disassembly prints.
    #[test]
    fn literals_carry_their_file_offsets_in_bytecode_order() {
        let mut data = vec![0x46];
        data.extend_from_slice(&TARGET.to_le_bytes());
        data.push(0x1D);
        data.extend_from_slice(&411i32.to_le_bytes());
        data.extend_from_slice(&[0x16, 0x04, 0x0B, 0x53]);
        let (script, _) = decode(&data, Some(18));
        assert!(script.complete(), "{:?}", script.stopped);

        let found = literals(&script.statements[0].expr);
        assert_eq!(found.len(), 1);
        assert!(matches!(found[0], Expr::IntConst { value: 411, at: 5 }));
        assert_eq!(stored_width(found[0]), 4);
        assert_eq!(literal_kind(found[0]), "IntConst");

        let same = literal_at(&script, 0, 0).expect("the call's one literal");
        assert!(matches!(same, Expr::IntConst { value: 411, .. }));
        let err = literal_at(&script, 15, 0).expect_err("Return holds nothing");
        assert!(err.contains("holds no literal constant"), "{err}");
        let err = literal_at(&script, 0, 1).expect_err("only one literal");
        assert!(err.contains("[0] IntConst 411"), "{err}");
        let err = literal_at(&script, 3, 0).expect_err("no statement there");
        assert!(err.contains("no statement starts at 0x0003"), "{err}");
    }

    /// A value goes in only where its bytes fit exactly; the forms with no value bytes, and a
    /// string of another length, are refused with the reason.
    #[test]
    fn a_literal_takes_a_value_only_at_its_own_width() {
        let mut names = FPackageNameMap::create();
        let int = Expr::IntConst { value: 411, at: 5 };
        let set = with_value(&int, "1000").expect("an int");
        assert!(matches!(set, Expr::IntConst { value: 1000, at: 5 }));
        assert_eq!(
            literal_bytes(&set, &mut names).expect("bytes"),
            vec![0xE8, 0x03, 0, 0]
        );
        let err = with_value(&int, "big").expect_err("not an int");
        assert!(err.contains("32-bit integer"), "{err}");

        let float = Expr::FloatConst { value: 1.5, at: 9 };
        assert!(matches!(
            with_value(&float, "2.5f").expect("a float"),
            Expr::FloatConst { value, .. } if value == 2.5
        ));

        let text = Expr::StringConst {
            value: "ab".to_string(),
            at: 0,
        };
        assert_eq!(stored_width(&text), 3);
        let err = with_value(&text, "abc").expect_err("longer");
        assert!(err.contains("own width"), "{err}");
        let kept = with_value(&text, " b").expect("same length keeps its space");
        assert_eq!(
            literal_bytes(&kept, &mut names).expect("bytes"),
            vec![b' ', b'b', 0]
        );
        let err = with_value(&text, "\u{e9}\u{4e2d}").expect_err("not one byte per char");
        assert!(err.contains("UnicodeStringConst"), "{err}");

        let vector = Expr::Numbers {
            name: "Vector3fConst",
            values: vec![0.0, 0.0, 0.0],
            at: 0,
        };
        assert_eq!(stored_width(&vector), 12);
        let err = with_value(&vector, "1,2").expect_err("two numbers");
        assert!(err.contains("takes 3 numbers"), "{err}");
        let set = with_value(&vector, "0.1, 2, 3").expect("three numbers");
        assert_eq!(literal_bytes(&set, &mut names).expect("bytes").len(), 12);

        let mut data = vec![0x46];
        data.extend_from_slice(&TARGET.to_le_bytes());
        data.extend_from_slice(&[0x25, 0x16, 0x53]);
        let (script, _) = decode(&data, Some(14));
        let zero = literal_at(&script, 0, 0).expect("IntZero is addressable");
        assert!(matches!(zero, Expr::Simple { name: "IntZero" }));
        let err = with_value(zero, "5").expect_err("no value bytes");
        assert!(err.contains("one-byte form"), "{err}");

        let name = Expr::NameConst {
            id: NameId::default(),
            name: "NameConst",
            value: "Old".to_string(),
            at: 0,
        };
        let before = names.num_names();
        let renamed = with_value(&name, "Brand_New").expect("a name");
        let bytes = literal_bytes(&renamed, &mut names).expect("bytes");
        assert_eq!(bytes.len(), 8);
        assert!(names.num_names() > before, "a new name is appended");
        let err = with_value(&name, "").expect_err("empty");
        assert!(err.contains("needs a name"), "{err}");
    }

    /// The disassembly is one statement per line, addressed the way a jump would name it.
    #[test]
    fn the_render_puts_one_statement_on_each_line() {
        let mut data = vec![0x46];
        data.extend_from_slice(&TARGET.to_le_bytes());
        data.push(0x1D);
        data.extend_from_slice(&411i32.to_le_bytes());
        data.extend_from_slice(&[0x16, 0x04, 0x0B, 0x53]);
        let (script, _) = decode(&data, Some(18));
        let text = render_script(&script);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(
            lines[0].starts_with("0x0000  LocalFinalFunction"),
            "{}",
            lines[0]
        );
        assert!(lines[0].contains("411"), "{}", lines[0]);
        assert_eq!(lines[1], "0x000F  Return Nothing");
        assert_eq!(lines[2], "0x0011  EndOfScript");
    }

    fn script_of(exprs: Vec<(u32, Expr)>) -> Script {
        Script {
            buffer_size: 0,
            storage_size: 0,
            decoded_size: 0,
            sizes_at: 0,
            start: 0,
            end: 0,
            statements: exprs
                .into_iter()
                .map(|(offset, expr)| Statement {
                    offset,
                    at: 0,
                    expr,
                })
                .collect(),
            stopped: None,
            names: Vec::new(),
            spans: Vec::new(),
            fixups: Vec::new(),
            resize_locks: Vec::new(),
        }
    }

    fn object(path: &str) -> ObjectRef {
        ObjectRef {
            index: TARGET,
            path: Some(path.into()),
        }
    }

    /// A latent call's resume point is buried in a struct argument, and it is as much a place
    /// execution goes as any jump.
    #[test]
    fn a_line_links_to_its_jumps_pushed_flows_and_latent_resume_points() {
        let delay = Expr::FinalCall {
            name: "CallMath",
            function: object("/Script/Engine.KismetSystemLibrary:Delay"),
            params: vec![Expr::StructConst {
                struct_type: object("/Script/Engine.LatentActionInfo"),
                size: 0,
                fields: vec![Expr::SkipOffsetConst { value: 0x40 }],
            }],
        };
        let script = script_of(vec![
            (0x00, Expr::PushExecutionFlow { target: 0x30 }),
            (
                0x05,
                Expr::JumpIfNot {
                    target: 0x20,
                    condition: Box::new(Expr::Simple { name: "True" }),
                },
            ),
            (0x10, delay),
            (
                0x20,
                Expr::Simple {
                    name: "EndOfScript",
                },
            ),
        ]);
        let lines = lines_of(&script);
        let targets: Vec<&[u32]> = lines.iter().map(|l| l.targets.as_slice()).collect();
        assert_eq!(targets, [&[0x30][..], &[0x20], &[0x40], &[]]);
        assert_eq!(lines[1].offset, 0x05);
    }

    /// Each event stub names the Ubergraph offset it starts at, which is the only place that
    /// offset is written down.
    #[test]
    fn event_stubs_name_the_ubergraph_offsets_they_enter_at() {
        let stub = |at: i32| {
            script_of(vec![(
                0,
                Expr::FinalCall {
                    name: "LocalFinalFunction",
                    function: object("/Game/X.X_C:ExecuteUbergraph_X"),
                    params: vec![Expr::IntConst { value: at, at: 0 }],
                },
            )])
        };
        let tick = stub(10);
        let begin = stub(2953);
        let other = script_of(vec![(
            0,
            Expr::Simple {
                name: "EndOfScript",
            },
        )]);
        let entries = ubergraph_entries([
            ("ReceiveBeginPlay", &begin),
            ("Tick", &tick),
            ("Helper", &other),
        ]);
        assert_eq!(
            entries.get("ExecuteUbergraph_X").map(Vec::as_slice),
            Some(
                &[
                    (10, "Tick".to_string()),
                    (2953, "ReceiveBeginPlay".to_string())
                ][..]
            )
        );
        assert_eq!(entries.len(), 1);
    }

    fn call(path: &str, params: Vec<Expr>) -> Expr {
        Expr::FinalCall {
            name: "LocalFinalFunction",
            function: object(path),
            params,
        }
    }

    /// A string buried in a call's arguments is as findable as the call itself.
    #[test]
    fn a_statement_names_its_calls_strings_and_variables_in_byte_order() {
        let expr = Expr::Let {
            name: "Let",
            property: None,
            variable: Box::new(Expr::Variable {
                name: "LocalVariable",
                property: PropertyRef {
                    names: Vec::new(),
                    path: "Loaded".into(),
                    owner: object("/Game/X.X_C:F"),
                },
            }),
            value: Box::new(call(
                "/Script/Marvel.MarvelFileUtil:LoadFromFile",
                vec![Expr::StringConst {
                    value: "Keys.txt".into(),
                    at: 0,
                }],
            )),
        };
        let found = statement_terms(&expr);
        let terms: Vec<(TermKind, &str)> =
            found.iter().map(|t| (t.kind, t.text.as_str())).collect();
        assert_eq!(
            terms,
            [
                (TermKind::Write, "Loaded"),
                (TermKind::Call, "/Script/Marvel.MarvelFileUtil:LoadFromFile"),
                (TermKind::String, "Keys.txt"),
            ]
        );
    }

    /// Name constants, the functions delegates bind and broadcast, cast classes and struct
    /// members are named too, each before what it holds.
    #[test]
    fn a_statement_names_its_names_delegates_casts_and_members_in_byte_order() {
        let name = |value: &str| Expr::NameConst {
            name: "NameConst",
            value: value.into(),
            at: 0,
            id: NameId::default(),
        };
        let delegate = Expr::BindDelegate {
            function: "OnPicked".into(),
            delegate: Box::new(Expr::SelfRef),
            object: Box::new(Expr::SelfRef),
            id: NameId::default(),
        };
        let broadcast = Expr::CallMulticastDelegate {
            signature: object("/Game/X.X_C:OnDone__DelegateSignature"),
            delegate: Box::new(Expr::NameConst {
                name: "InstanceDelegate",
                value: "OnTimer".into(),
                at: 0,
                id: NameId::default(),
            }),
            params: vec![Expr::Cast {
                name: "DynamicCast",
                class: object("/Script/Engine.Actor"),
                value: Box::new(Expr::Member {
                    name: "StructMemberContext",
                    property: PropertyRef {
                        names: Vec::new(),
                        path: "Location".into(),
                        owner: object("/Script/CoreUObject.Transform"),
                    },
                    value: Box::new(name("Selected")),
                }),
            }],
        };
        let expr = call("/Game/X.X_C:Helper", vec![delegate, broadcast]);
        let terms: Vec<(TermKind, String)> = statement_terms(&expr)
            .into_iter()
            .map(|t| (t.kind, t.text))
            .collect();
        let expect = |kind, text: &str| (kind, text.to_string());
        assert_eq!(
            terms,
            [
                expect(TermKind::Call, "/Game/X.X_C:Helper"),
                expect(TermKind::Delegate, "OnPicked"),
                expect(TermKind::Delegate, "/Game/X.X_C:OnDone__DelegateSignature"),
                expect(TermKind::Delegate, "OnTimer"),
                expect(TermKind::Object, "/Script/Engine.Actor"),
                expect(TermKind::Read, "Location"),
                expect(TermKind::Name, "Selected"),
            ]
        );
    }

    /// A function a delegate binds or broadcasts is not called there, so no call site lists it.
    #[test]
    fn call_sites_leave_out_bound_and_broadcast_delegates() {
        let expr = call(
            "/Game/X.X_C:Helper",
            vec![Expr::BindDelegate {
                function: "OnPicked".into(),
                delegate: Box::new(Expr::SelfRef),
                object: Box::new(Expr::SelfRef),
                id: NameId::default(),
            }],
        );
        let script = script_of(vec![(0x10, expr)]);
        let sites = call_sites([("First", &script)]);
        assert_eq!(sites.keys().collect::<Vec<_>>(), ["Helper"]);
    }

    fn prop(path: &str) -> PropertyRef {
        PropertyRef {
            names: Vec::new(),
            path: path.into(),
            owner: object("/Game/X.X_C:F"),
        }
    }

    /// A variable of `name`'s kind: `LocalVariable`, `InstanceVariable` and the like.
    fn var(name: &'static str, path: &str) -> Expr {
        Expr::Variable {
            name,
            property: prop(path),
        }
    }

    fn member(name: &'static str, path: &str, value: Expr) -> Expr {
        Expr::Member {
            name,
            property: prop(path),
            value: Box::new(value),
        }
    }

    fn assign(variable: Expr, value: Expr) -> Expr {
        Expr::Let {
            name: "Let",
            property: None,
            variable: Box::new(variable),
            value: Box::new(value),
        }
    }

    fn context(object: Expr, member: Expr) -> Expr {
        Expr::Context {
            name: "Context",
            object: Box::new(object),
            skip: 0,
            property: prop("ReturnValue"),
            member: Box::new(member),
        }
    }

    /// Each term as its kind and text, for comparing at a glance.
    fn terms_of(expr: &Expr) -> Vec<String> {
        statement_terms(expr)
            .into_iter()
            .map(|t| format!("{:?} {}", t.kind, t.text))
            .collect()
    }

    /// What a statement stores into is written, along the members and elements that lead to it;
    /// everything else it names is read, the object a member is reached through included.
    #[test]
    fn an_assignment_writes_its_target_and_reads_the_rest() {
        let subtract = "/Script/Engine.KismetMathLibrary:Subtract_DoubleDouble";
        let health = assign(
            var("InstanceVariable", "Health"),
            call(
                subtract,
                vec![
                    var("InstanceVariable", "Health"),
                    var("LocalVariable", "Damage"),
                ],
            ),
        );
        assert_eq!(
            terms_of(&health),
            [
                "Write Health".to_string(),
                format!("Call {subtract}"),
                "Read Health".into(),
                "Read Damage".into(),
            ]
        );
        let zero = || Expr::IntConst { value: 0, at: 0 };
        let through = assign(
            context(
                var("InstanceVariable", "Target"),
                var("InstanceVariable", "Health"),
            ),
            zero(),
        );
        assert_eq!(terms_of(&through), ["Read Target", "Write Health"]);
        let location = || {
            member(
                "StructMemberContext",
                "Location",
                var("LocalVariable", "Transform"),
            )
        };
        let set_member = assign(member("StructMemberContext", "X", location()), zero());
        assert_eq!(
            terms_of(&set_member),
            ["Write X", "Write Location", "Write Transform"]
        );
        let get_member = assign(var("LocalVariable", "Copy"), location());
        assert_eq!(
            terms_of(&get_member),
            ["Write Copy", "Read Location", "Read Transform"]
        );
        let element = assign(
            Expr::ArrayGetByRef {
                array: Box::new(var("InstanceVariable", "Items")),
                index: Box::new(var("LocalVariable", "Index")),
            },
            var("LocalVariable", "Item"),
        );
        assert_eq!(
            terms_of(&element),
            ["Write Items", "Read Index", "Read Item"]
        );
        let stored = member(
            "LetValueOnPersistentFrame",
            "K2Node_Event_Damage",
            var("LocalVariable", "Damage"),
        );
        assert_eq!(
            terms_of(&stored),
            ["Write K2Node_Event_Damage", "Read Damage"]
        );
        let built = Expr::SetArray {
            array: Box::new(var("InstanceVariable", "Names")),
            items: vec![var("LocalVariable", "First")],
        };
        assert_eq!(terms_of(&built), ["Write Names", "Read First"]);
    }

    /// Binding a delegate, adding to one, removing from one and clearing one all change it;
    /// broadcasting only reads it.
    #[test]
    fn binding_or_clearing_a_delegate_writes_it() {
        let added = Expr::DelegateOp {
            name: "AddMulticastDelegate",
            delegate: Box::new(context(
                var("InstanceVariable", "Button"),
                var("InstanceVariable", "OnClicked"),
            )),
            value: Box::new(Expr::NameConst {
                name: "InstanceDelegate",
                value: "OnPressed".into(),
                at: 0,
                id: NameId::default(),
            }),
        };
        assert_eq!(
            terms_of(&added),
            ["Read Button", "Write OnClicked", "Delegate OnPressed"]
        );
        let cleared = Expr::Unary {
            name: "ClearMulticastDelegate",
            value: Box::new(var("InstanceVariable", "OnDone")),
        };
        assert_eq!(terms_of(&cleared), ["Write OnDone"]);
        let bound = Expr::BindDelegate {
            function: "OnPicked".into(),
            delegate: Box::new(var("LocalVariable", "K2Node_CreateDelegate_OutputDelegate")),
            object: Box::new(var("InstanceVariable", "Picker")),
            id: NameId::default(),
        };
        assert_eq!(
            terms_of(&bound),
            [
                "Delegate OnPicked",
                "Write K2Node_CreateDelegate_OutputDelegate",
                "Read Picker",
            ]
        );
        let broadcast = Expr::CallMulticastDelegate {
            signature: object("/Game/X.X_C:OnDone__DelegateSignature"),
            delegate: Box::new(var("InstanceVariable", "OnDone")),
            params: Vec::new(),
        };
        assert_eq!(
            terms_of(&broadcast),
            [
                "Delegate /Game/X.X_C:OnDone__DelegateSignature",
                "Read OnDone",
            ]
        );
    }

    /// The engine functions that change what they are handed write it, a timer's handle after
    /// the world context included; any other call only reads what it is handed.
    #[test]
    fn an_engine_call_writes_the_argument_it_changes() {
        let library = |function: &str, params| {
            context(
                Expr::ObjectConst {
                    object: object("/Script/Engine.Default__KismetArrayLibrary"),
                },
                call(
                    &format!("/Script/Engine.KismetArrayLibrary:{function}"),
                    params,
                ),
            )
        };
        let paths = || var("InstanceVariable", "MountedPaths");
        let cleared = library("Array_Clear", vec![paths()]);
        assert_eq!(
            terms_of(&cleared),
            [
                "Object /Script/Engine.Default__KismetArrayLibrary",
                "Call /Script/Engine.KismetArrayLibrary:Array_Clear",
                "Write MountedPaths",
            ]
        );
        let added = assign(
            var("LocalVariable", "CallFunc_Array_Add_ReturnValue"),
            library("Array_Add", vec![paths(), var("LocalVariable", "Path")]),
        );
        assert_eq!(
            terms_of(&added)
                .into_iter()
                .filter(|t| !t.starts_with("Object") && !t.starts_with("Call"))
                .collect::<Vec<_>>(),
            [
                "Write CallFunc_Array_Add_ReturnValue",
                "Write MountedPaths",
                "Read Path",
            ]
        );
        let counted = library("Array_Length", vec![paths()]);
        assert!(terms_of(&counted).contains(&"Read MountedPaths".to_string()));
        let handed = call("/Game/X.X_C:Fill", vec![paths()]);
        assert_eq!(
            terms_of(&handed),
            ["Call /Game/X.X_C:Fill", "Read MountedPaths"]
        );
        let timer = |function: &str| {
            call(
                &format!("/Script/Engine.KismetSystemLibrary:{function}"),
                vec![Expr::SelfRef, var("InstanceVariable", "PollTimer")],
            )
        };
        assert_eq!(
            terms_of(&timer("K2_ClearAndInvalidateTimerHandle"))[1..],
            ["Write PollTimer"]
        );
        assert_eq!(
            terms_of(&timer("K2_PauseTimerHandle"))[1..],
            ["Read PollTimer"]
        );
    }

    /// Only constants with value bytes of their own are offered, but each keeps its index among
    /// all of them, which is how an edit addresses it.
    #[test]
    fn a_line_offers_its_editable_constants_by_their_literal_index() {
        let expr = call(
            "/Script/Marvel.MarvelFileUtil:SaveToFile",
            vec![
                Expr::Simple { name: "True" },
                Expr::StringConst {
                    value: "Keys.txt".into(),
                    at: 0,
                },
                Expr::IntConst { value: 7, at: 0 },
            ],
        );
        let slots = literal_slots(&expr);
        let seen: Vec<(u32, &str)> = slots.iter().map(|s| (s.index, s.value.as_str())).collect();
        assert_eq!(seen, [(1, "Keys.txt"), (2, "7")]);
    }

    #[test]
    fn call_sites_list_each_caller_once_per_statement_by_the_callees_bare_name() {
        let both = call(
            "/Game/X.X_C:Helper",
            vec![call("/Game/X.X_C:Helper", Vec::new())],
        );
        let first = script_of(vec![(0x10, both)]);
        let second = script_of(vec![(0x20, call("/Game/X.X_C:Helper", Vec::new()))]);
        let sites = call_sites([("First", &first), ("Second", &second)]);
        assert_eq!(
            sites.get("Helper").map(Vec::as_slice),
            Some(&[("First".to_string(), 0x10), ("Second".to_string(), 0x20)][..])
        );
        let lines = lines_of(&first);
        assert_eq!(lines[0].calls, ["Helper"]);
    }

    /// A stop is rendered too, so a partial disassembly says where it gave up.
    #[test]
    fn a_stopped_script_renders_its_reason() {
        let (script, _) = decode(&[0xFE], None);
        let text = render_script(&script);
        assert!(text.contains("stopped at 0x0000"), "{text}");
        assert!(text.contains("0xFE"), "{text}");
    }

    /// Every token the reader decodes has a name, so the census never reports a bare number for
    /// something it understood.
    #[test]
    fn every_modelled_token_has_a_name() {
        for token in 0u8..=0xFF {
            if token_name(token).is_some() {
                continue;
            }
            // An unnamed token must also be one the body refuses, which the reader proves by
            // stopping on it.
            let (script, _) = decode(&[token], None);
            assert!(
                script.stopped.is_some(),
                "{token:#04X} decodes but has no name"
            );
        }
    }

    fn kinds(script: &Script) -> Vec<&'static str> {
        script.resize_locks.iter().map(|lock| lock.kind).collect()
    }

    fn absolute(fixup: &Fixup) -> Option<(u32, Option<&str>)> {
        match &fixup.kind {
            FixupKind::Absolute { target, resumes } => Some((*target, resumes.as_deref())),
            FixupKind::Relative { .. } => None,
        }
    }

    /// Each offset field is recorded where its four bytes sit, so a resize can rewrite it, and a
    /// jump that lands where no expression starts keeps the script at its size.
    #[test]
    fn offset_fields_are_recorded_where_they_sit() {
        let mut data = vec![0x4C];
        data.extend_from_slice(&0x0Au32.to_le_bytes());
        data.push(0x06);
        data.extend_from_slice(&0x05u32.to_le_bytes());
        data.push(0x53);
        let (script, _) = decode(&data, None);
        assert_eq!(
            script
                .fixups
                .iter()
                .map(|f| (f.at, f.source, absolute(f)))
                .collect::<Vec<_>>(),
            [
                (1, FixupSource::PushFlow, Some((0x0A, None))),
                (6, FixupSource::Jump, Some((0x05, None))),
            ]
        );
        assert!(script.resize_locks.is_empty(), "{:?}", script.resize_locks);

        data[1..5].copy_from_slice(&0x07u32.to_le_bytes());
        let (stray, _) = decode(&data, None);
        assert_eq!(kinds(&stray), ["stray target"]);
    }

    /// A switch's offsets name the ends of its arms, which the walk checks as it reads them.
    #[test]
    fn a_switch_names_the_ends_of_its_arms() {
        let switch = |next: u32, end: u32| {
            let mut data = vec![0x69];
            data.extend_from_slice(&1u16.to_le_bytes());
            data.extend_from_slice(&end.to_le_bytes());
            data.push(0x1D);
            data.extend_from_slice(&7i32.to_le_bytes());
            data.push(0x1D);
            data.extend_from_slice(&7i32.to_le_bytes());
            data.extend_from_slice(&next.to_le_bytes());
            data.push(0x26);
            data.push(0x25);
            data.push(0x53);
            decode(&data, None).0
        };
        let good = switch(22, 23);
        assert!(good.resize_locks.is_empty(), "{:?}", good.resize_locks);
        assert_eq!(
            good.fixups.iter().map(|f| f.source).collect::<Vec<_>>(),
            [FixupSource::SwitchEnd, FixupSource::SwitchNext]
        );
        assert_eq!(kinds(&switch(0x20, 23)), ["switch offset"]);
        assert_eq!(kinds(&switch(22, 0x30)), ["switch offset"]);
    }

    /// A context's skip is the loaded length of its member, recorded as the span it measures.
    #[test]
    fn a_context_skip_is_its_members_loaded_length() {
        let context = |skip: u32| {
            let mut data = vec![0x19, 0x17];
            data.extend_from_slice(&skip.to_le_bytes());
            field_path(&mut data, "Damage", TARGET);
            data.push(0x00);
            field_path(&mut data, "Damage", TARGET);
            data.push(0x53);
            decode(&data, None).0
        };
        let good = context(9);
        assert!(good.resize_locks.is_empty(), "{:?}", good.resize_locks);
        assert_eq!(
            good.fixups[0].kind,
            FixupKind::Relative { from: 14, to: 23 }
        );
        assert_eq!(kinds(&context(8)), ["context skip"]);
    }

    fn latent(linkage: &[u8]) -> Vec<u8> {
        let mut data = vec![0x2F];
        data.extend_from_slice(&LATENT_INFO.to_le_bytes());
        data.extend_from_slice(&32i32.to_le_bytes());
        data.extend_from_slice(linkage);
        data.push(0x1D);
        data.extend_from_slice(&5i32.to_le_bytes());
        data.push(0x21);
        name_bytes(&mut data, "ExecuteUbergraph_X");
        data.extend_from_slice(&[0x17, 0x30, 0x53]);
        data
    }

    /// A latent action's resume point counts in the function its struct names, and one that calls
    /// back through a delegate instead has no resume point at all.
    #[test]
    fn a_latent_action_resumes_the_function_it_names() {
        let mut offset = vec![0x5B];
        offset.extend_from_slice(&0x40u32.to_le_bytes());
        let (script, _) = decode(&latent(&offset), None);
        assert_eq!(
            script.fixups.iter().map(absolute).collect::<Vec<_>>(),
            [Some((0x40, Some("ExecuteUbergraph_X")))]
        );
        assert!(script.resize_locks.is_empty(), "{:?}", script.resize_locks);

        let mut none = vec![0x1D];
        none.extend_from_slice(&(-1i32).to_le_bytes());
        let (script, _) = decode(&latent(&none), None);
        assert!(script.resize_locks.is_empty(), "{:?}", script.resize_locks);

        let mut plain = vec![0x1D];
        plain.extend_from_slice(&0x40i32.to_le_bytes());
        let (script, _) = decode(&latent(&plain), None);
        assert_eq!(kinds(&script), ["latent action"]);
    }

    /// An event graph's dispatch waits for the package pass; any other computed jump is final.
    #[test]
    fn only_an_event_graph_dispatch_waits_for_its_stubs() {
        let jump = |variable: &str| {
            let mut data = vec![0x4E, 0x00];
            field_path(&mut data, variable, 0);
            data.push(0x53);
            decode(&data, None).0
        };
        assert_eq!(kinds(&jump("EntryPoint")), [ENTRY_DISPATCH]);
        assert_eq!(kinds(&jump("Damage")), ["computed jump"]);
    }

    #[test]
    fn every_expression_has_a_span_in_visiting_order() {
        let mut data = vec![0x46];
        data.extend_from_slice(&TARGET.to_le_bytes());
        data.push(0x1D);
        data.extend_from_slice(&411i32.to_le_bytes());
        data.extend_from_slice(&[0x16, 0x04, 0x0B, 0x53]);
        let (script, _) = decode(&data, Some(18));
        let mut seen = Vec::new();
        visit_spans(&script, &mut |expr, span, _| {
            seen.push((render(expr), span.offset, span.end_offset))
        });
        assert_eq!(seen.len(), script.spans.len());
        assert_eq!(
            seen,
            [
                ("LocalFinalFunction 'Target'(411)".to_string(), 0, 15),
                ("411".to_string(), 9, 14),
                ("Return Nothing".to_string(), 15, 17),
                ("Nothing".to_string(), 16, 17),
                ("EndOfScript".to_string(), 17, 18),
            ]
        );
    }

    fn function(index: u32, name: &str, data: &[u8]) -> crate::package::ParsedExport {
        let (script, _) = decode(data, None);
        assert!(script.complete(), "{:?}", script.stopped);
        crate::package::ParsedExport {
            object_name: name.into(),
            script: Some(script),
            ..crate::package::ParsedExport::blank(index)
        }
    }

    /// An event graph `ExecuteUbergraph_X` that dispatches on its entry point and returns at 0x0A,
    /// and a stub entering it at `entry`.
    fn event_graph_and_stub(entry: i32, extra: &[u8]) -> Vec<crate::package::ParsedExport> {
        let mut graph = vec![0x4E, 0x00];
        field_path(&mut graph, "EntryPoint", 0);
        graph.extend_from_slice(&[0x04, 0x0B, 0x53]);
        let mut stub = vec![0x45];
        name_bytes(&mut stub, "ExecuteUbergraph_X");
        stub.push(0x1D);
        stub.extend_from_slice(&entry.to_le_bytes());
        stub.push(0x16);
        stub.extend_from_slice(extra);
        stub.extend_from_slice(&[0x04, 0x0B, 0x53]);
        vec![
            function(0, "ExecuteUbergraph_X", &graph),
            function(1, "ReceiveBeginPlay", &stub),
        ]
    }

    /// The package pass lifts an event graph's dispatch lock once every stub entering it lands on
    /// an expression, and locks it for a stub that does not or for any other mention of it.
    #[test]
    fn an_event_graph_resizes_once_every_entry_into_it_is_accounted_for() {
        let mut exports = event_graph_and_stub(10, &[]);
        settle_resize_locks(&mut exports);
        let graph = exports[0].script.as_ref().expect("a script");
        assert!(graph.resize_locks.is_empty(), "{:?}", graph.resize_locks);
        let functions = functions_of(&exports);
        let found = inbound(&functions, &functions[0]);
        assert_eq!(
            found
                .entries
                .iter()
                .map(|e| (e.export, e.offset, e.span.token))
                .collect::<Vec<_>>(),
            [(1, 10, 0x1D)]
        );

        let mut exports = event_graph_and_stub(3, &[]);
        settle_resize_locks(&mut exports);
        assert_eq!(
            kinds(exports[0].script.as_ref().expect("a script")),
            ["stray target"]
        );

        let mut named = vec![0x21];
        name_bytes(&mut named, "ExecuteUbergraph_X");
        let mut exports = event_graph_and_stub(10, &named);
        settle_resize_locks(&mut exports);
        assert_eq!(
            kinds(exports[0].script.as_ref().expect("a script")),
            ["event graph reference"]
        );
    }

    /// A `Let` keeps a call's value even through the object the call is made on; a call passed on
    /// as an argument is consumed, and one that is the whole statement is discarded.
    #[test]
    fn a_call_kept_through_its_object_is_kept() {
        let reference = |path: &str| PropertyRef {
            names: Vec::new(),
            path: path.into(),
            owner: ObjectRef {
                index: 0,
                path: None,
            },
        };
        let call = || Expr::FinalCall {
            name: "FinalFunction",
            function: ObjectRef {
                index: -1,
                path: Some("/Script/Engine.Actor:GetOwner".into()),
            },
            params: Vec::new(),
        };
        let variable = Expr::Variable {
            name: "LocalVariable",
            property: reference("Owner"),
        };
        let through = Expr::Let {
            name: "LetObj",
            property: None,
            variable: Box::new(variable),
            value: Box::new(Expr::Context {
                name: "Context",
                object: Box::new(Expr::SelfRef),
                skip: 0,
                property: reference("ReturnValue"),
                member: Box::new(call()),
            }),
        };
        let Expr::Let { value, .. } = &through else {
            unreachable!()
        };
        let Expr::Context { member, .. } = value.as_ref() else {
            unreachable!()
        };
        let (use_, kept_in) = call_use(&through, member);
        assert_eq!(use_, CallUse::Kept);
        assert!(matches!(kept_in, Some(Expr::Variable { .. })));

        let alone = call();
        assert_eq!(call_use(&alone, &alone).0, CallUse::Discarded);

        let outer = Expr::FinalCall {
            name: "FinalFunction",
            function: ObjectRef {
                index: -2,
                path: Some("/Script/Engine.Actor:SetOwner".into()),
            },
            params: vec![call()],
        };
        let Expr::FinalCall { params, .. } = &outer else {
            unreachable!()
        };
        assert_eq!(call_use(&outer, &params[0]).0, CallUse::Consumed);
    }

    /// A name keeps the table entry and number the bytes hold, a field path one per segment, so a
    /// text printed from the script can say exactly which entry each name was.
    #[test]
    fn every_name_keeps_the_entry_and_number_it_was_stored_as() {
        let damage = NAMES.iter().position(|n| *n == "Damage").expect("named") as i32;
        let fired = NAMES.iter().position(|n| *n == "OnFired").expect("named") as i32;
        let mut data = vec![0x21];
        data.extend_from_slice(&damage.to_le_bytes());
        data.extend_from_slice(&3i32.to_le_bytes());
        data.push(0x00);
        data.extend_from_slice(&2i32.to_le_bytes());
        data.extend_from_slice(&damage.to_le_bytes());
        data.extend_from_slice(&0i32.to_le_bytes());
        data.extend_from_slice(&fired.to_le_bytes());
        data.extend_from_slice(&2i32.to_le_bytes());
        data.extend_from_slice(&0i32.to_le_bytes());
        data.push(0x53);

        let (script, _) = decode(&data, None);
        assert!(script.complete(), "{:?}", script.stopped);
        let Expr::NameConst { value, id, .. } = &script.statements[0].expr else {
            panic!("{:?}", script.statements[0].expr);
        };
        assert_eq!(value, "Damage_2");
        assert_eq!(
            *id,
            NameId {
                index: damage,
                number: 3
            }
        );
        let Expr::Variable { property, .. } = &script.statements[1].expr else {
            panic!("{:?}", script.statements[1].expr);
        };
        assert_eq!(property.path, "Damage.OnFired_1");
        assert_eq!(
            property.names,
            [
                NameId {
                    index: damage,
                    number: 0
                },
                NameId {
                    index: fired,
                    number: 2
                }
            ]
        );
        assert_eq!(
            shape(&script.statements[0].expr),
            shape(&Expr::NameConst {
                name: "NameConst",
                value: "Damage_2".into(),
                at: 0,
                id: NameId::default(),
            }),
            "a comparison leaves the stored entry out"
        );
    }

    /// The flag byte an assert carries is kept whole, so a value other than 0 or 1 writes back.
    #[test]
    fn an_assert_keeps_its_debug_byte() {
        let data = [0x09, 0x0C, 0x00, 0x07, 0x27, 0x53];
        let (script, _) = decode(&data, None);
        assert!(script.complete(), "{:?}", script.stopped);
        assert!(matches!(
            script.statements[0].expr,
            Expr::Assert {
                line: 12,
                debug: 7,
                ..
            }
        ));
    }

    #[test]
    fn every_token_name_maps_back_to_its_token() {
        for token in 0..=u8::MAX {
            if let Some(name) = token_name(token) {
                assert_eq!(token_named(name), Some(token), "{name}");
            }
        }
        assert_eq!(token_named("NotAToken"), None);
    }

    /// UE5 numbers the conversions from zero, which is what the game's scripts hold.
    #[test]
    fn conversions_are_named_by_the_engines_numbering() {
        assert_eq!(conversion_name(0x00), Some("ObjectToInterface"));
        assert_eq!(conversion_name(0x03), Some("DoubleToFloat"));
        assert_eq!(conversion_name(0x46), None);
        for kind in 0..=0x04 {
            let name = conversion_name(kind).expect("named");
            assert_eq!(conversion_kind(name), Some(kind));
        }
    }
}
