//! Reads `.usmap` mappings files: the reflected classes, structs and enums that unversioned properties are decoded against.

use std::collections::BTreeMap;

use retoc::compression::{CompressionMethod, decompress};

use crate::reader::Cursor;

const MAGIC: u16 = 0x30C4;
const PACKAGE_VERSIONING: u8 = 1;
const LONG_FNAME: u8 = 2;
const LARGE_ENUMS: u8 = 3;
const EXPLICIT_ENUM_VALUES: u8 = 4;
/// A whole game's mappings unpack to about 14 MB. A header claiming more than this is corrupt, and
/// allocating what it claims would abort the process.
const MAX_BODY: u32 = 1 << 30;
/// Containers nest a few deep at most, so this only stops a corrupt file recursing off the stack.
const MAX_NESTING: u32 = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Usmap {
    pub enums: Vec<Enum>,
    pub structs: Vec<Struct>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Struct {
    pub name: String,
    pub super_struct: Option<String>,
    pub properties: Vec<Property>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enum {
    pub name: String,
    pub entries: BTreeMap<i64, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Property {
    pub name: String,
    pub array_dim: u8,
    /// Position within its own struct, not within the flattened chain.
    pub index: u16,
    pub inner: PropertyInner,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropertyInner {
    Byte,
    Bool,
    Int,
    Float,
    Object,
    Name,
    Delegate,
    Double,
    Array {
        inner: Box<PropertyInner>,
    },
    Struct {
        name: String,
    },
    Str,
    Text,
    Interface,
    MulticastDelegate,
    WeakObject,
    LazyObject,
    AssetObject,
    SoftObject,
    UInt64,
    UInt32,
    UInt16,
    Int64,
    Int16,
    Int8,
    Map {
        key: Box<PropertyInner>,
        value: Box<PropertyInner>,
    },
    Set {
        key: Box<PropertyInner>,
    },
    Enum {
        inner: Box<PropertyInner>,
        name: String,
    },
    FieldPath,
    Optional {
        inner: Box<PropertyInner>,
    },
    Utf8Str,
    AnsiStr,
    Unknown,
}

impl Usmap {
    /// The extension blocks after the structs (PPTH, EATR, ENVP, GPKG and whatever dumpers add
    /// next) carry nothing this reads, so they are left alone.
    pub fn read(bytes: &[u8]) -> Result<Self, String> {
        let (version, body) = unpack(bytes)?;
        let mut body = Body {
            cursor: Cursor::new(&body, 0),
            names: Vec::new(),
            version,
        };
        body.read_names()?;
        let enums = body.read_enums()?;
        let structs = body.read_structs()?;
        Ok(Self { enums, structs })
    }
}

/// The header's version and the body it wraps, decompressed.
fn unpack(bytes: &[u8]) -> Result<(u8, Vec<u8>), String> {
    let mut cursor = Cursor::new(bytes, 0);
    let magic = cursor
        .read_u16()
        .map_err(|_| "file is too short to be a .usmap".to_string())?;
    if magic != MAGIC {
        return Err(format!("not a .usmap file (magic 0x{magic:04X})"));
    }
    let version = cursor.read_u8()?;
    if version > EXPLICIT_ENUM_VALUES {
        return Err(format!(
            ".usmap version {version} is newer than the {EXPLICIT_ENUM_VALUES} this reads"
        ));
    }
    if version >= PACKAGE_VERSIONING && cursor.read_i32()? > 0 {
        // The UE4 and UE5 file versions, the custom versions (a GUID and a number each), the changelist.
        cursor.skip(8)?;
        let custom_versions = cursor.read_u32()? as usize;
        cursor.skip(custom_versions.saturating_mul(20))?;
        cursor.skip(4)?;
    }
    let method = cursor.read_u8()?;
    let packed_len = cursor.read_u32()?;
    let unpacked_len = cursor.read_u32()?;
    let packed = cursor.take(packed_len as usize)?;
    let method = match method {
        0 => return Ok((version, packed.to_vec())),
        1 => CompressionMethod::Oodle,
        2 => {
            return Err(
                "this .usmap is Brotli compressed, which is not supported. Re-save it uncompressed, zstd or Oodle compressed."
                    .to_string(),
            );
        }
        3 => CompressionMethod::Zstd,
        other => return Err(format!("unknown .usmap compression method {other}")),
    };
    if unpacked_len > MAX_BODY {
        return Err(format!(
            "the .usmap header claims {unpacked_len} bytes unpacked, past the {MAX_BODY} a mappings file can hold"
        ));
    }
    let mut body = vec![0; unpacked_len as usize];
    decompress(method, packed, &mut body)
        .map_err(|e| format!("could not decompress the {method:?} .usmap: {e:#}"))?;
    Ok((version, body))
}

struct Body<'a> {
    cursor: Cursor<'a>,
    names: Vec<String>,
    version: u8,
}

impl Body<'_> {
    fn read_names(&mut self) -> Result<(), String> {
        for _ in 0..self.cursor.read_u32()? {
            let len = if self.version >= LONG_FNAME {
                usize::from(self.cursor.read_u16()?)
            } else {
                usize::from(self.cursor.read_u8()?)
            };
            let raw = self.cursor.take(len)?;
            let raw = raw
                .iter()
                .position(|b| *b == 0)
                .map_or(raw, |end| &raw[..end]);
            let name = String::from_utf8(raw.to_vec())
                .map_err(|_| self.cursor.err("a name is not UTF-8"))?;
            self.names.push(name);
        }
        Ok(())
    }

    fn name(&mut self) -> Result<String, String> {
        let index = self.cursor.read_u32()?;
        self.names.get(index as usize).cloned().ok_or_else(|| {
            self.cursor.err(format!(
                "name index {index} past the {} names",
                self.names.len()
            ))
        })
    }

    fn optional_name(&mut self) -> Result<Option<String>, String> {
        if self.cursor.peek_u32() == Some(u32::MAX) {
            self.cursor.skip(4)?;
            return Ok(None);
        }
        self.name().map(Some)
    }

    fn read_enums(&mut self) -> Result<Vec<Enum>, String> {
        let mut enums = Vec::new();
        for _ in 0..self.cursor.read_u32()? {
            let name = self.name()?;
            let count = if self.version >= LARGE_ENUMS {
                usize::from(self.cursor.read_u16()?)
            } else {
                usize::from(self.cursor.read_u8()?)
            };
            let mut entries = BTreeMap::new();
            for at in 0..count {
                let value = if self.version >= EXPLICIT_ENUM_VALUES {
                    self.cursor.read_i64()?
                } else {
                    at as i64
                };
                entries.insert(value, self.name()?);
            }
            enums.push(Enum { name, entries });
        }
        Ok(enums)
    }

    fn read_structs(&mut self) -> Result<Vec<Struct>, String> {
        let mut structs = Vec::new();
        for _ in 0..self.cursor.read_u32()? {
            let name = self.name()?;
            let super_struct = self.optional_name()?;
            let slots = self.cursor.read_u16()?;
            let mut properties = Vec::new();
            for _ in 0..self.cursor.read_u16()? {
                let index = self.cursor.read_u16()?;
                let array_dim = self.cursor.read_u8()?;
                let name = self.name()?;
                let inner = self.property_inner(0)?;
                properties.push(Property {
                    name,
                    array_dim,
                    index,
                    inner,
                });
            }
            let held: u32 = properties.iter().map(|p| u32::from(p.array_dim)).sum();
            if held != u32::from(slots) {
                return Err(self.cursor.err(format!(
                    "{name} declares {slots} property slots and holds {held}"
                )));
            }
            structs.push(Struct {
                name,
                super_struct,
                properties,
            });
        }
        Ok(structs)
    }

    fn property_inner(&mut self, depth: u32) -> Result<PropertyInner, String> {
        if depth > MAX_NESTING {
            return Err(self.cursor.err("property types nest too deep"));
        }
        let nested = |body: &mut Self| body.property_inner(depth + 1).map(Box::new);
        Ok(match self.cursor.read_u8()? {
            0 => PropertyInner::Byte,
            1 => PropertyInner::Bool,
            2 => PropertyInner::Int,
            3 => PropertyInner::Float,
            4 => PropertyInner::Object,
            5 => PropertyInner::Name,
            6 => PropertyInner::Delegate,
            7 => PropertyInner::Double,
            8 => PropertyInner::Array {
                inner: nested(self)?,
            },
            9 => PropertyInner::Struct { name: self.name()? },
            10 => PropertyInner::Str,
            11 => PropertyInner::Text,
            12 => PropertyInner::Interface,
            13 => PropertyInner::MulticastDelegate,
            14 => PropertyInner::WeakObject,
            15 => PropertyInner::LazyObject,
            16 => PropertyInner::AssetObject,
            17 => PropertyInner::SoftObject,
            18 => PropertyInner::UInt64,
            19 => PropertyInner::UInt32,
            20 => PropertyInner::UInt16,
            21 => PropertyInner::Int64,
            22 => PropertyInner::Int16,
            23 => PropertyInner::Int8,
            24 => PropertyInner::Map {
                key: nested(self)?,
                value: nested(self)?,
            },
            25 => PropertyInner::Set { key: nested(self)? },
            26 => PropertyInner::Enum {
                inner: nested(self)?,
                name: self.name()?,
            },
            27 => PropertyInner::FieldPath,
            28 => PropertyInner::Optional {
                inner: nested(self)?,
            },
            29 => PropertyInner::Utf8Str,
            30 => PropertyInner::AnsiStr,
            0xFF => PropertyInner::Unknown,
            other => return Err(self.cursor.err(format!("unknown property type {other}"))),
        })
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Usmap {
    /// This mappings file, uncompressed at the newest version, for tests that need one on disk.
    pub fn write(&self) -> Vec<u8> {
        let mut out = Writer::default();
        out.u32(self.enums.len() as u32);
        for entry in &self.enums {
            out.name(&entry.name);
            out.body
                .extend_from_slice(&(entry.entries.len() as u16).to_le_bytes());
            for (value, name) in &entry.entries {
                out.body.extend_from_slice(&value.to_le_bytes());
                out.name(name);
            }
        }
        out.u32(self.structs.len() as u32);
        for entry in &self.structs {
            out.name(&entry.name);
            match &entry.super_struct {
                Some(parent) => out.name(parent),
                None => out.u32(u32::MAX),
            }
            let slots: u32 = entry
                .properties
                .iter()
                .map(|p| u32::from(p.array_dim))
                .sum();
            out.body.extend_from_slice(&(slots as u16).to_le_bytes());
            out.body
                .extend_from_slice(&(entry.properties.len() as u16).to_le_bytes());
            for property in &entry.properties {
                out.body.extend_from_slice(&property.index.to_le_bytes());
                out.body.push(property.array_dim);
                out.name(&property.name);
                out.property_inner(&property.inner);
            }
        }

        let mut body = (out.names.len() as u32).to_le_bytes().to_vec();
        for name in &out.names {
            body.extend_from_slice(&(name.len() as u16).to_le_bytes());
            body.extend_from_slice(name.as_bytes());
        }
        body.extend_from_slice(&out.body);

        let mut file = MAGIC.to_le_bytes().to_vec();
        file.push(EXPLICIT_ENUM_VALUES);
        file.extend_from_slice(&0i32.to_le_bytes());
        file.push(0);
        file.extend_from_slice(&(body.len() as u32).to_le_bytes());
        file.extend_from_slice(&(body.len() as u32).to_le_bytes());
        file.extend_from_slice(&body);
        file
    }
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Default)]
struct Writer {
    names: Vec<String>,
    index: std::collections::HashMap<String, u32>,
    body: Vec<u8>,
}

#[cfg(any(test, feature = "test-support"))]
impl Writer {
    fn u32(&mut self, value: u32) {
        self.body.extend_from_slice(&value.to_le_bytes());
    }

    fn name(&mut self, name: &str) {
        let next = self.names.len() as u32;
        let index = *self.index.entry(name.to_string()).or_insert(next);
        if index == next {
            self.names.push(name.to_string());
        }
        self.u32(index);
    }

    fn property_inner(&mut self, inner: &PropertyInner) {
        use PropertyInner as P;
        let kind: u8 = match inner {
            P::Byte => 0,
            P::Bool => 1,
            P::Int => 2,
            P::Float => 3,
            P::Object => 4,
            P::Name => 5,
            P::Delegate => 6,
            P::Double => 7,
            P::Array { .. } => 8,
            P::Struct { .. } => 9,
            P::Str => 10,
            P::Text => 11,
            P::Interface => 12,
            P::MulticastDelegate => 13,
            P::WeakObject => 14,
            P::LazyObject => 15,
            P::AssetObject => 16,
            P::SoftObject => 17,
            P::UInt64 => 18,
            P::UInt32 => 19,
            P::UInt16 => 20,
            P::Int64 => 21,
            P::Int16 => 22,
            P::Int8 => 23,
            P::Map { .. } => 24,
            P::Set { .. } => 25,
            P::Enum { .. } => 26,
            P::FieldPath => 27,
            P::Optional { .. } => 28,
            P::Utf8Str => 29,
            P::AnsiStr => 30,
            P::Unknown => 0xFF,
        };
        self.body.push(kind);
        match inner {
            P::Array { inner } | P::Optional { inner } | P::Set { key: inner } => {
                self.property_inner(inner)
            }
            P::Struct { name } => self.name(name),
            P::Map { key, value } => {
                self.property_inner(key);
                self.property_inner(value);
            }
            P::Enum { inner, name } => {
                self.property_inner(inner);
                self.name(name);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Magic, version, no package versioning, compression method, packed and unpacked length.
    const HEADER_LEN: usize = 16;

    fn sample() -> Usmap {
        Usmap {
            enums: vec![Enum {
                name: "EColor".into(),
                entries: BTreeMap::from([(0, "Red".into()), (5, "Blue".into())]),
            }],
            structs: vec![
                Struct {
                    name: "Object".into(),
                    super_struct: None,
                    properties: Vec::new(),
                },
                Struct {
                    name: "Thing".into(),
                    super_struct: Some("Object".into()),
                    properties: vec![
                        Property {
                            name: "Counts".into(),
                            array_dim: 3,
                            index: 0,
                            inner: PropertyInner::Int,
                        },
                        Property {
                            name: "ByColor".into(),
                            array_dim: 1,
                            index: 3,
                            inner: PropertyInner::Map {
                                key: Box::new(PropertyInner::Enum {
                                    inner: Box::new(PropertyInner::Byte),
                                    name: "EColor".into(),
                                }),
                                value: Box::new(PropertyInner::Array {
                                    inner: Box::new(PropertyInner::Struct {
                                        name: "Thing".into(),
                                    }),
                                }),
                            },
                        },
                        Property {
                            name: "Maybe".into(),
                            array_dim: 1,
                            index: 4,
                            inner: PropertyInner::Optional {
                                inner: Box::new(PropertyInner::Set {
                                    key: Box::new(PropertyInner::Name),
                                }),
                            },
                        },
                    ],
                },
            ],
        }
    }

    fn with_method(file: &[u8], method: u8, packed: &[u8], unpacked_len: usize) -> Vec<u8> {
        let mut out = file[..7].to_vec();
        out.push(method);
        out.extend_from_slice(&(packed.len() as u32).to_le_bytes());
        out.extend_from_slice(&(unpacked_len as u32).to_le_bytes());
        out.extend_from_slice(packed);
        out
    }

    #[test]
    fn a_written_file_reads_back_whole() {
        let usmap = sample();
        assert_eq!(Usmap::read(&usmap.write()).expect("read"), usmap);
    }

    /// GPKG is what made S11's published mappings unreadable to the jmap crate, which refused
    /// any block it did not know.
    #[test]
    fn extension_blocks_after_the_structs_are_left_alone() {
        let usmap = sample();
        let file = usmap.write();
        let mut body = file[HEADER_LEN..].to_vec();
        body.extend_from_slice(b"CEXT\x00\x01\x00\x00\x00GPKG\x05\x00\x00\x00\xFF\xFF\xFF\xFF\xFF");
        let file = with_method(&file, 0, &body, body.len());
        assert_eq!(Usmap::read(&file).expect("read"), usmap);
    }

    #[test]
    fn a_zstd_body_reads() {
        let usmap = sample();
        let file = usmap.write();
        let body = &file[HEADER_LEN..];
        let mut packed = Vec::new();
        retoc::compression::compress(CompressionMethod::Zstd, None, body, &mut packed)
            .expect("compress");
        let file = with_method(&file, 3, &packed, body.len());
        assert_eq!(Usmap::read(&file).expect("read"), usmap);
    }

    #[test]
    fn an_oodle_body_reads() {
        if std::env::var_os("OODLE_LIB_PATH").is_none() {
            return;
        }
        let usmap = sample();
        let file = usmap.write();
        let body = &file[HEADER_LEN..];
        let mut packed = Vec::new();
        retoc::compression::compress(CompressionMethod::Oodle, None, body, &mut packed)
            .expect("compress");
        let file = with_method(&file, 1, &packed, body.len());
        assert_eq!(Usmap::read(&file).expect("read"), usmap);
    }

    #[test]
    fn a_brotli_body_is_refused_with_a_readable_message() {
        let file = sample().write();
        let body = &file[HEADER_LEN..];
        let err = Usmap::read(&with_method(&file, 2, body, body.len())).expect_err("refused");
        assert!(err.contains("Brotli"), "{err}");
    }

    #[test]
    fn a_header_claiming_an_impossible_size_is_refused_before_allocating_it() {
        let file = sample().write();
        let err =
            Usmap::read(&with_method(&file, 3, &[0; 8], u32::MAX as usize)).expect_err("refused");
        assert!(err.contains("past the"), "{err}");
    }

    #[test]
    fn a_name_index_past_the_table_is_an_error() {
        let file = Usmap {
            enums: Vec::new(),
            structs: Vec::new(),
        }
        .write();
        // No names, no enums, then one struct whose name is index 7.
        let mut body = file[HEADER_LEN..HEADER_LEN + 8].to_vec();
        body.extend_from_slice(&1u32.to_le_bytes());
        body.extend_from_slice(&7u32.to_le_bytes());
        let err = Usmap::read(&with_method(&file, 0, &body, body.len())).expect_err("refused");
        assert!(err.contains("name index 7 past the 0 names"), "{err}");
    }

    #[test]
    fn a_newer_version_is_refused() {
        let mut file = sample().write();
        file[2] = EXPLICIT_ENUM_VALUES + 1;
        let err = Usmap::read(&file).expect_err("refused");
        assert!(err.contains("newer"), "{err}");
    }

    #[test]
    fn a_header_with_package_versioning_finds_the_body() {
        let usmap = sample();
        let file = usmap.write();
        let mut versioned = file[..3].to_vec();
        versioned.extend_from_slice(&1i32.to_le_bytes());
        versioned.extend_from_slice(&[0; 8]);
        versioned.extend_from_slice(&2u32.to_le_bytes());
        versioned.extend_from_slice(&[0xAB; 40]);
        versioned.extend_from_slice(&[0; 4]);
        versioned.extend_from_slice(&file[7..]);
        assert_eq!(Usmap::read(&versioned).expect("read"), usmap);
    }
}
