//! Disassembles the Kismet bytecode a function or class export stores, recording every package
//! index the script names.
//!
//! The script is stored behind two words: the size it occupies once loaded, and the size it takes
//! on disk. They differ because a name is twelve bytes in memory and eight on disk, and an object
//! pointer eight and four, so the loaded size is the space every jump offset indexes. That makes
//! it an exact oracle: a walk that ends anywhere else has read some operand at the wrong width.

use serde::Serialize;

use crate::props::{Ctx, Diagnostics, read_field_path, read_index};
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
}

impl Script {
    /// Whether every byte was accounted for, which is what makes the references it names complete.
    pub fn complete(&self) -> bool {
        self.stopped.is_none()
    }

    pub fn script_len(&self) -> usize {
        self.statements.len()
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

/// A property the script reads or writes, as the dotted name chain and the object that owns it.
#[derive(Debug, Clone, Serialize)]
pub struct PropertyRef {
    pub path: String,
    pub owner: ObjectRef,
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
        debug: bool,
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
    },
    FinalCall {
        name: &'static str,
        function: ObjectRef,
        params: Vec<Expr>,
    },
    IntConst {
        value: i32,
    },
    Int64Const {
        value: i64,
    },
    UInt64Const {
        value: u64,
    },
    FloatConst {
        value: f32,
    },
    DoubleConst {
        value: f64,
    },
    ByteConst {
        name: &'static str,
        value: u8,
    },
    StringConst {
        value: String,
    },
    UnicodeStringConst {
        value: String,
    },
    ObjectConst {
        object: ObjectRef,
    },
    NameConst {
        name: &'static str,
        value: String,
    },
    /// Rotations, vectors and transforms: a fixed run of floating point numbers.
    Numbers {
        name: &'static str,
        values: Vec<f64>,
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

/// The conversions `EX_Cast` performs, for the disassembly.
fn conversion_name(kind: u8) -> &'static str {
    match kind {
        0x46 => "ObjectToInterface",
        0x47 => "ObjectToBool",
        0x49 => "InterfaceToBool",
        0x4A => "DoubleToFloat",
        0x4B => "FloatToDouble",
        _ => "Cast",
    }
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
}

impl<'a, 'b> Reader<'a, 'b> {
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
    fn name(&mut self) -> Result<String, String> {
        let value = self.cursor.read_name(self.ctx.names())?;
        self.offset += LOADED_NAME;
        Ok(value)
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
        let start = self.cursor.file_offset();
        let (path, owner) = read_field_path(&mut self.cursor, self.ctx, self.diagnostics)?;
        let _ = start;
        self.offset += LOADED_POINTER;
        let resolved = self
            .ctx
            .object_path(owner)
            .map_err(|e| self.cursor.err(e))?;
        Ok(PropertyRef {
            path,
            owner: ObjectRef {
                index: owner,
                path: resolved,
            },
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
        self.depth += 1;
        let value = self.body(token);
        self.depth -= 1;
        value
    }

    fn body(&mut self, token: u8) -> Result<Expr, String> {
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
                target: self.u32v()?,
            },
            0x07 => Expr::JumpIfNot {
                target: self.u32v()?,
                condition: self.boxed()?,
            },
            0x09 => Expr::Assert {
                line: self.u16v()?,
                debug: self.u8v()? != 0,
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
            0x12 | 0x19 | 0x1A => Expr::Context {
                name: named(token),
                object: self.boxed()?,
                skip: self.u32v()?,
                property: self.property()?,
                member: self.boxed()?,
            },
            0x13 | 0x2E | 0x52 | 0x54 | 0x55 => Expr::Cast {
                name: named(token),
                class: self.object()?,
                value: self.boxed()?,
            },
            0x17 => Expr::SelfRef,
            0x18 => Expr::Skip {
                skip: self.u32v()?,
                value: self.boxed()?,
            },
            0x1B | 0x45 => Expr::VirtualCall {
                name: named(token),
                function: self.name()?,
                params: self.until(0x16)?,
            },
            0x1C | 0x46 | 0x68 => Expr::FinalCall {
                name: named(token),
                function: self.object()?,
                params: self.until(0x16)?,
            },
            0x1D => Expr::IntConst {
                value: self.i32v()?,
            },
            0x1E => Expr::FloatConst {
                value: self.f32v()?,
            },
            0x1F => Expr::StringConst {
                value: self.ansi()?,
            },
            0x20 => Expr::ObjectConst {
                object: self.object()?,
            },
            0x21 | 0x4B => Expr::NameConst {
                name: named(token),
                value: self.name()?,
            },
            0x22 | 0x23 => Expr::Numbers {
                name: named(token),
                values: self.doubles(3)?,
            },
            0x2B => Expr::Numbers {
                name: named(token),
                values: self.doubles(10)?,
            },
            0x41 => Expr::Numbers {
                name: named(token),
                values: self.floats(3)?,
            },
            0x24 | 0x2C => Expr::ByteConst {
                name: named(token),
                value: self.u8v()?,
            },
            0x29 => Expr::TextConst { text: self.text()? },
            0x2F => Expr::StructConst {
                struct_type: self.object()?,
                size: self.i32v()?,
                fields: self.until(0x30)?,
            },
            0x31 => Expr::SetArray {
                array: self.boxed()?,
                items: self.until(0x32)?,
            },
            0x33 => Expr::PropertyConst {
                property: self.property()?,
            },
            0x34 => Expr::UnicodeStringConst {
                value: self.utf16()?,
            },
            0x35 => Expr::Int64Const {
                value: self.i64v()?,
            },
            0x36 => Expr::UInt64Const {
                value: self.u64v()?,
            },
            0x37 => Expr::DoubleConst {
                value: self.f64v()?,
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
                target: self.u32v()?,
            },
            0x4E => Expr::ComputedJump {
                target: self.boxed()?,
            },
            0x4F | 0x51 | 0x5D | 0x67 | 0x6D => Expr::Unary {
                name: named(token),
                value: self.boxed()?,
            },
            0x5B => Expr::SkipOffsetConst {
                value: self.u32v()?,
            },
            0x5C | 0x62 => Expr::DelegateOp {
                name: named(token),
                delegate: self.boxed()?,
                value: self.boxed()?,
            },
            0x61 => Expr::BindDelegate {
                function: self.name()?,
                delegate: self.boxed()?,
                object: self.boxed()?,
            },
            0x63 => Expr::CallMulticastDelegate {
                signature: self.object()?,
                delegate: self.boxed()?,
                params: self.until(0x16)?,
            },
            0x69 => self.switch()?,
            0x6A => {
                let event = self.u8v()?;
                // Only an inline event carries a name of its own.
                let name = (event == 4).then(|| self.name()).transpose()?;
                Expr::InstrumentationEvent { event, name }
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

    fn switch(&mut self) -> Result<Expr, String> {
        let cases = self.u16v()?;
        let end = self.u32v()?;
        let index = self.boxed()?;
        let mut out = Vec::with_capacity(usize::from(cases));
        for _ in 0..cases {
            out.push(SwitchCase {
                value: self.expr()?,
                next: self.u32v()?,
                result: self.expr()?,
            });
        }
        Ok(Expr::SwitchValue {
            end,
            index,
            cases: out,
            default: self.boxed()?,
        })
    }
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
    if stopped.is_some() {
        // A script that did not decode whole may have named its objects at the wrong offsets.
        diagnostics.references.truncate(mark);
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
    }
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

fn object_text(object: &ObjectRef) -> String {
    match &object.path {
        Some(path) => path.clone(),
        None if object.index == 0 => "None".to_string(),
        None => object.index.to_string(),
    }
}

fn list(items: &[Expr]) -> String {
    items.iter().map(render).collect::<Vec<_>>().join(", ")
}

fn render(expr: &Expr) -> String {
    match expr {
        Expr::Simple { name } => (*name).to_string(),
        Expr::Variable { name, property } => format!("{name}({})", property.path),
        Expr::Return { value } => format!("Return {}", render(value)),
        Expr::Jump { target } => format!("Jump 0x{target:04X}"),
        Expr::JumpIfNot { target, condition } => {
            format!("Jump 0x{target:04X} unless {}", render(condition))
        }
        Expr::Assert {
            line,
            debug,
            condition,
        } => format!("Assert line {line} debug {debug} {}", render(condition)),
        Expr::NothingInt32 { value } => format!("Nothing({value})"),
        Expr::Let {
            name,
            property,
            variable,
            value,
        } => match property {
            Some(property) => format!(
                "{name} {}<{}> = {}",
                render(variable),
                property.path,
                render(value)
            ),
            None => format!("{name} {} = {}", render(variable), render(value)),
        },
        Expr::BitFieldConst { property, value } => format!("BitField({}) {value}", property.path),
        Expr::Context {
            name,
            object,
            property,
            member,
            ..
        } => {
            let arrow = if *name == "ClassContext" { "::" } else { "->" };
            format!(
                "{}{arrow}{} [{}]",
                render(object),
                render(member),
                property.path
            )
        }
        Expr::Cast { name, class, value } => {
            format!("{name}<{}>({})", object_text(class), render(value))
        }
        Expr::SelfRef => "Self".to_string(),
        Expr::Skip { skip, value } => format!("Skip 0x{skip:04X} {}", render(value)),
        Expr::VirtualCall {
            name,
            function,
            params,
        } => format!("{name} {function}({})", list(params)),
        Expr::FinalCall {
            name,
            function,
            params,
        } => format!("{name} {}({})", object_text(function), list(params)),
        Expr::IntConst { value } => value.to_string(),
        Expr::Int64Const { value } => value.to_string(),
        Expr::UInt64Const { value } => value.to_string(),
        Expr::FloatConst { value } => format!("{value}f"),
        Expr::DoubleConst { value } => value.to_string(),
        Expr::ByteConst { value, .. } => value.to_string(),
        Expr::StringConst { value } | Expr::UnicodeStringConst { value } => format!("{value:?}"),
        Expr::ObjectConst { object } => format!("Object({})", object_text(object)),
        Expr::NameConst { value, .. } => format!("'{value}'"),
        Expr::Numbers { name, values } => format!(
            "{name}({})",
            values
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Expr::TextConst { text } => match text {
            TextLiteral::Empty => "Text(\"\")".to_string(),
            TextLiteral::Localized { source, .. } => format!("Text({})", render(source)),
            TextLiteral::Invariant { source } | TextLiteral::Literal { source } => {
                format!("Text({})", render(source))
            }
            TextLiteral::StringTable { table, key, .. } => {
                format!("Text({} in {})", render(key), object_text(table))
            }
        },
        Expr::StructConst {
            struct_type,
            fields,
            ..
        } => format!("{}{{{}}}", object_text(struct_type), list(fields)),
        Expr::SetArray { array, items } => format!("{} = [{}]", render(array), list(items)),
        Expr::PropertyConst { property } => format!("Property({})", property.path),
        Expr::Conversion { conversion, value } => {
            format!("{}({})", conversion_name(*conversion), render(value))
        }
        Expr::SetContainer {
            name,
            target,
            items,
            ..
        } => format!("{name} {} = {{{}}}", render(target), list(items)),
        Expr::ContainerConst {
            name,
            property,
            items,
            ..
        } => format!("{name}<{}>[{}]", property.path, list(items)),
        Expr::MapConst {
            key, value, items, ..
        } => format!("Map<{}, {}>{{{}}}", key.path, value.path, list(items)),
        Expr::Member {
            name,
            property,
            value,
        } => format!("{name} {}.{}", render(value), property.path),
        Expr::PushExecutionFlow { target } => format!("PushFlow 0x{target:04X}"),
        Expr::ComputedJump { target } => format!("Jump {}", render(target)),
        Expr::Unary { name, value } => format!("{name}({})", render(value)),
        Expr::SkipOffsetConst { value } => format!("Offset 0x{value:04X}"),
        Expr::DelegateOp {
            name,
            delegate,
            value,
        } => format!("{name} {} {}", render(delegate), render(value)),
        Expr::BindDelegate {
            function,
            delegate,
            object,
        } => format!(
            "BindDelegate '{function}' {} {}",
            render(delegate),
            render(object)
        ),
        Expr::CallMulticastDelegate {
            signature,
            delegate,
            params,
        } => format!(
            "{}.Broadcast<{}>({})",
            render(delegate),
            object_text(signature),
            list(params)
        ),
        Expr::SwitchValue {
            index,
            cases,
            default,
            ..
        } => {
            let arms = cases
                .iter()
                .map(|case| format!("{} -> {}", render(&case.value), render(&case.result)))
                .collect::<Vec<_>>()
                .join("; ");
            format!(
                "Switch({}) {{{arms}; default -> {}}}",
                render(index),
                render(default)
            )
        }
        Expr::InstrumentationEvent { event, name } => match name {
            Some(name) => format!("Instrumentation {event} '{name}'"),
            None => format!("Instrumentation {event}"),
        },
        Expr::ArrayGetByRef { array, index } => format!("{}[{}]", render(array), render(index)),
        Expr::Unknown { token } => format!("?? {token:#04X}"),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use retoc::legacy_asset::{FLegacyPackageHeader, FPackageNameMap};

    const NAMES: &[&str] = &["None", "EntryPoint", "Damage", "OnFired", "Target"];

    /// Index -1: the one import every test names, so an object operand resolves to a path.
    const TARGET: i32 = -1;

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
            imports: vec![retoc::legacy_asset::FObjectImport {
                class_package: none,
                class_name: none,
                outer_index: retoc::zen::FPackageIndex::create_null(),
                object_name: retoc::legacy_asset::FMinimalName { index, number: 0 },
                is_optional: false,
            }],
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
        assert!(matches!(params[0], Expr::IntConst { value: 411 }));
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
        assert!(matches!(**value, Expr::IntConst { value: 5 }));
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
}
