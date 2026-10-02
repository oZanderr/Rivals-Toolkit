//! Packages built byte by byte whose functions hold chosen bytecode, for the text assembler's
//! tests: a class, its functions with their parameters and locals, and the imports they call.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use retoc::legacy_asset::{
    EPackageFlags, FLegacyPackageFileSummary, FLegacyPackageHeader, FMinimalName, FObjectExport,
    FObjectImport, FPackageNameMap,
};
use retoc::zen::FPackageIndex;

use crate::package::{AssetBundle, FALLBACK_ENGINE_VERSION, ParsedPackage, parse_package};

const HEADER_SIZE: usize = 2048;

/// `CPF_Parm`, which makes a field a parameter rather than a local.
pub const PARM: u64 = 0x80;

/// A field record: a parameter or local of a function, or a member of the class.
pub struct Field {
    pub name: &'static str,
    /// `IntProperty`, `BoolProperty` or `ObjectProperty`.
    pub kind: &'static str,
    pub flags: u64,
}

pub fn local(name: &'static str, kind: &'static str) -> Field {
    Field {
        name,
        kind,
        flags: 0,
    }
}

pub fn param(name: &'static str, kind: &'static str) -> Field {
    Field {
        name,
        kind,
        flags: PARM,
    }
}

pub struct Function {
    pub name: &'static str,
    pub fields: Vec<Field>,
    pub script: Vec<u8>,
}

/// One import: its class's package and name, its outer and its own name.
pub struct Import {
    pub class_package: &'static str,
    pub class: &'static str,
    pub outer: i32,
    pub name: &'static str,
}

/// The package a builder lays out: a name table, imports, and a class whose functions follow it.
pub struct Package {
    pub names: Vec<String>,
    pub imports: Vec<Import>,
    pub class: &'static str,
    pub members: Vec<Field>,
    pub functions: Vec<Function>,
}

/// The `.uasset` and `.uexp` halves, which parse as the package they were built from.
pub struct Built {
    pub asset: Vec<u8>,
    pub exports: Vec<u8>,
}

impl Built {
    pub fn bundle(&self) -> AssetBundle<'_> {
        AssetBundle {
            asset: &self.asset,
            exports: &self.exports,
        }
    }

    pub fn parsed(&self) -> ParsedPackage {
        parse_package(&self.bundle(), None).expect("the built package parses")
    }

    pub fn header(&self) -> FLegacyPackageHeader {
        crate::package::read_header(&self.bundle()).expect("the built header reads")
    }
}

/// The names every package here takes, before the ones a test adds.
const BASE_NAMES: &[&str] = &[
    "None",
    "/Script/CoreUObject",
    "Package",
    "Class",
    "Function",
    "IntProperty",
    "BoolProperty",
    "ObjectProperty",
];

impl Package {
    /// A package holding the class `class`, with `extra` added to its name table.
    pub fn new(class: &'static str, extra: &[&str]) -> Self {
        let mut names: Vec<String> = BASE_NAMES.iter().map(|n| (*n).to_string()).collect();
        for name in std::iter::once(&class).chain(extra) {
            if !names.iter().any(|n| n == name) {
                names.push((*name).to_string());
            }
        }
        Package {
            names,
            imports: vec![
                Import {
                    class_package: "/Script/CoreUObject",
                    class: "Package",
                    outer: 0,
                    name: "/Script/CoreUObject",
                },
                Import {
                    class_package: "/Script/CoreUObject",
                    class: "Class",
                    outer: -1,
                    name: "Class",
                },
                Import {
                    class_package: "/Script/CoreUObject",
                    class: "Class",
                    outer: -1,
                    name: "Function",
                },
            ],
            class,
            members: Vec::new(),
            functions: Vec::new(),
        }
    }

    pub fn name(&self, value: &str) -> FMinimalName {
        FMinimalName {
            index: self.index_of(value),
            number: 0,
        }
    }

    pub fn index_of(&self, value: &str) -> i32 {
        self.names
            .iter()
            .position(|n| n == value)
            .unwrap_or_else(|| panic!("{value} is in the test name map")) as i32
    }

    /// Adds an import and returns the index the bytecode names it by.
    pub fn import(&mut self, import: Import) -> i32 {
        for name in [import.class_package, import.class, import.name] {
            if !self.names.iter().any(|n| n == name) {
                self.names.push(name.to_string());
            }
        }
        self.imports.push(import);
        -(self.imports.len() as i32)
    }

    /// The index the bytecode names the class by.
    pub fn class_index(&self) -> i32 {
        1
    }

    /// The index the bytecode names the `n`th function by.
    pub fn function_index(&self, n: usize) -> i32 {
        n as i32 + 2
    }

    fn name_bytes(&self, out: &mut Vec<u8>, value: &str) {
        out.extend_from_slice(&self.index_of(value).to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
    }

    fn fields(&self, out: &mut Vec<u8>, fields: &[Field]) {
        out.extend_from_slice(&(fields.len() as i32).to_le_bytes());
        for field in fields {
            self.name_bytes(out, field.kind);
            self.name_bytes(out, field.name);
            out.extend_from_slice(&0u32.to_le_bytes()); // FField flags
            out.extend_from_slice(&1i32.to_le_bytes()); // ArrayDim
            out.extend_from_slice(&4i32.to_le_bytes()); // ElementSize
            out.extend_from_slice(&field.flags.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // RepIndex
            self.name_bytes(out, "None"); // RepNotifyFunc
            out.push(0); // BlueprintReplicationCondition
            match field.kind {
                "BoolProperty" => out.extend_from_slice(&[4, 0, 1, 0xFF, 1, 1]),
                "ObjectProperty" => out.extend_from_slice(&0i32.to_le_bytes()),
                _ => {}
            }
        }
    }

    /// The bytes a function's export holds: no tagged properties, then its struct layout.
    fn function_body(&self, function: &Function, loaded: u32) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&0i64.to_le_bytes()); // the tagged list's `None`
        out.extend_from_slice(&0i32.to_le_bytes()); // no object guid
        out.extend_from_slice(&0i32.to_le_bytes()); // no super
        out.extend_from_slice(&0i32.to_le_bytes()); // no children
        self.fields(&mut out, &function.fields);
        out.extend_from_slice(&loaded.to_le_bytes());
        out.extend_from_slice(&(function.script.len() as u32).to_le_bytes());
        out.extend_from_slice(&function.script);
        out.extend_from_slice(&0u32.to_le_bytes()); // FunctionFlags
        out.extend_from_slice(&0i32.to_le_bytes()); // EventGraphFunction
        out.extend_from_slice(&0i32.to_le_bytes()); // EventGraphCallOffset
        out
    }

    fn class_body(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&0i64.to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes()); // no super
        out.extend_from_slice(&(self.functions.len() as i32).to_le_bytes());
        for n in 0..self.functions.len() {
            out.extend_from_slice(&self.function_index(n).to_le_bytes());
        }
        self.fields(&mut out, &self.members);
        out.extend_from_slice(&0i32.to_le_bytes()); // no bytecode, loaded
        out.extend_from_slice(&0i32.to_le_bytes()); // no bytecode, stored
        out.extend_from_slice(&0i32.to_le_bytes()); // an empty function map
        out.extend_from_slice(&0u32.to_le_bytes()); // ClassFlags
        out.extend_from_slice(&0i32.to_le_bytes()); // ClassWithin
        self.name_bytes(&mut out, "None"); // ClassConfigName
        out.extend_from_slice(&0i32.to_le_bytes()); // ClassGeneratedBy
        out.extend_from_slice(&0i32.to_le_bytes()); // no interfaces
        out.extend_from_slice(&0u32.to_le_bytes()); // bDeprecatedForceScriptOrder
        self.name_bytes(&mut out, "None");
        out.extend_from_slice(&1u32.to_le_bytes()); // bCooked
        out.extend_from_slice(&0i32.to_le_bytes()); // no default object
        out
    }

    /// The loaded size a script declares, read off the bytes themselves.
    fn loaded_size(&self, script: &[u8]) -> u32 {
        let exports = std::iter::once(self.class)
            .chain(self.functions.iter().map(|f| f.name))
            .map(|name| FObjectExport {
                object_name: self.name(name),
                ..Default::default()
            })
            .collect();
        let header = self.header(exports);
        let ctx = crate::props::Ctx {
            mappings: None,
            header: &header,
            fixups: None,
            synth: None,
            local: None,
        };
        let mut scratch = crate::props::Diagnostics::default();
        let read =
            crate::kismet::read_script(script, 0, 0, None, script.len() as u32, &ctx, &mut scratch);
        assert!(read.complete(), "the test script reads: {:?}", read.stopped);
        read.decoded_size
    }

    fn header(&self, exports: Vec<FObjectExport>) -> FLegacyPackageHeader {
        let mut summary = FLegacyPackageFileSummary {
            package_name: "/Game/Test".to_string(),
            ..Default::default()
        };
        summary.versioning_info.package_file_version =
            FALLBACK_ENGINE_VERSION.package_file_version();
        summary.versioning_info.total_header_size = HEADER_SIZE as i32;
        summary.package_flags =
            EPackageFlags::Cooked as u32 | EPackageFlags::FilterEditorOnly as u32;
        FLegacyPackageHeader {
            summary,
            name_map: FPackageNameMap::create_from_names(self.names.clone()),
            imports: self
                .imports
                .iter()
                .map(|import| FObjectImport {
                    class_package: self.name(import.class_package),
                    class_name: self.name(import.class),
                    outer_index: FPackageIndex {
                        index: import.outer,
                    },
                    object_name: self.name(import.name),
                    is_optional: false,
                })
                .collect(),
            exports,
            ..Default::default()
        }
    }

    pub fn build(&self) -> Built {
        let mut bodies = vec![(self.class, -2, 0, self.class_body())];
        for function in &self.functions {
            let loaded = self.loaded_size(&function.script);
            bodies.push((
                function.name,
                -3,
                self.class_index(),
                self.function_body(function, loaded),
            ));
        }
        let mut exports_bytes = Vec::new();
        let mut exports = Vec::new();
        for (name, class, outer, body) in bodies {
            exports.push(FObjectExport {
                object_name: self.name(name),
                class_index: FPackageIndex { index: class },
                outer_index: FPackageIndex { index: outer },
                serial_offset: exports_bytes.len() as i64,
                serial_size: body.len() as i64,
                ..Default::default()
            });
            exports_bytes.extend_from_slice(&body);
        }
        let header = self.header(exports);
        let mut asset = std::io::Cursor::new(Vec::new());
        header
            .serialize(
                &mut asset,
                Some(HEADER_SIZE),
                &retoc::logging::Log::no_log(),
            )
            .expect("serialize the test header");
        Built {
            asset: asset.into_inner(),
            exports: exports_bytes,
        }
    }
}

/// Bytecode, written the way the tests spell it.
#[derive(Default)]
pub struct Code<'p> {
    pub bytes: Vec<u8>,
    package: Option<&'p Package>,
}

impl<'p> Code<'p> {
    pub fn new(package: &'p Package) -> Self {
        Code {
            bytes: Vec::new(),
            package: Some(package),
        }
    }

    fn package(&self) -> &'p Package {
        self.package.expect("code built against a package")
    }

    pub fn op(mut self, token: u8) -> Self {
        self.bytes.push(token);
        self
    }

    pub fn ops(mut self, tokens: &[u8]) -> Self {
        self.bytes.extend_from_slice(tokens);
        self
    }

    pub fn int(mut self, value: i32) -> Self {
        self.bytes.extend_from_slice(&value.to_le_bytes());
        self
    }

    pub fn word(mut self, value: u32) -> Self {
        self.bytes.extend_from_slice(&value.to_le_bytes());
        self
    }

    pub fn name(mut self, value: &str) -> Self {
        self.package().name_bytes(&mut self.bytes, value);
        self
    }

    pub fn numbered(mut self, value: &str, number: i32) -> Self {
        let index = self.package().index_of(value);
        self.bytes.extend_from_slice(&index.to_le_bytes());
        self.bytes.extend_from_slice(&number.to_le_bytes());
        self
    }

    /// A field path of one segment.
    pub fn field(self, value: &str, owner: i32) -> Self {
        self.int(1).name(value).int(owner)
    }
}

/// A Blueprint class with an event graph, the stub entering it, and a function of its own.
///
/// The graph dispatches on its entry point, branches, assigns, calls through a context,
/// switches, and waits on a latent action resuming at its return.
pub fn event_graph() -> Package {
    let mut package = Package::new(
        "BP_Test_C",
        &[
            "ExecuteUbergraph_BP_Test",
            "ReceiveBeginPlay",
            "EntryPoint",
            "Flag",
            "Count",
            "LatentActionInfo",
            "/Script/Engine",
            "KismetSystemLibrary",
            "Delay",
            "ScriptStruct",
        ],
    );
    let delay_library = package.import(Import {
        class_package: "/Script/CoreUObject",
        class: "Package",
        outer: 0,
        name: "/Script/Engine",
    });
    let library = package.import(Import {
        class_package: "/Script/CoreUObject",
        class: "Class",
        outer: delay_library,
        name: "KismetSystemLibrary",
    });
    let delay = package.import(Import {
        class_package: "/Script/CoreUObject",
        class: "Function",
        outer: library,
        name: "Delay",
    });
    let latent = package.import(Import {
        class_package: "/Script/CoreUObject",
        class: "ScriptStruct",
        outer: delay_library,
        name: "LatentActionInfo",
    });
    let (graph, stub) = (package.function_index(0), package.function_index(1));
    // 0x00 Jump LocalVariable(EntryPoint): 1 + 1 + 8
    // 0x0A Jump @0092 unless LocalVariable(Flag): 1 + 4 + 1 + 8
    // 0x18 Let LocalVariable(Count) = 5: 1 + 8 + 9 + 5
    // 0x2F Self->LocalFinalFunction ReceiveBeginPlay(): 1 + 1 + 4 + 8 + 10
    // 0x47 SwitchValue(Count, 1 => IntOne, default => IntZero): 1 + 2 + 4 + 9 + 5 + 4 + 1 + 1
    // 0x62 FinalFunction Delay(LatentActionInfo{…}): 1 + 8 + 1 + 8 + 4 + 5 + 5 + 13 + 1 + 1 + 1
    // 0x92 Return Nothing, then EndOfScript at 0x94
    let graph_code = Code::new(&package)
        .ops(&[0x4E, 0x00])
        .field("EntryPoint", graph)
        .op(0x07)
        .word(0x92)
        .op(0x00)
        .field("Flag", graph)
        .op(0x0F)
        .field("Count", graph)
        .op(0x00)
        .field("Count", graph)
        .op(0x1D)
        .int(5)
        .ops(&[0x19, 0x17])
        .word(10)
        .int(0)
        .int(0)
        .op(0x46)
        .int(stub)
        .op(0x16)
        .op(0x69)
        .ops(&[1, 0])
        .word(0x62)
        .op(0x00)
        .field("Count", graph)
        .op(0x1D)
        .int(1)
        .word(0x61)
        .op(0x26)
        .op(0x25)
        .op(0x1C)
        .int(delay)
        .op(0x2F)
        .int(latent)
        .int(24)
        .op(0x5B)
        .word(0x92)
        .op(0x1D)
        .int(7)
        .op(0x21)
        .name("ExecuteUbergraph_BP_Test")
        .op(0x17)
        .ops(&[0x30, 0x16])
        .ops(&[0x04, 0x0B, 0x53])
        .bytes;
    let stub_code = Code::new(&package)
        .op(0x46)
        .int(graph)
        .op(0x1D)
        .int(10)
        .ops(&[0x16, 0x04, 0x0B, 0x53])
        .bytes;
    package.functions.push(Function {
        name: "ExecuteUbergraph_BP_Test",
        fields: vec![
            param("EntryPoint", "IntProperty"),
            local("Flag", "BoolProperty"),
            local("Count", "IntProperty"),
        ],
        script: graph_code,
    });
    package.functions.push(Function {
        name: "ReceiveBeginPlay",
        fields: Vec::new(),
        script: stub_code,
    });
    package
}
