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
    /// `FunctionFlags`: [`crate::ustruct::FUNC_UBERGRAPH`] makes the function its class's event
    /// graph.
    pub flags: u32,
}

/// One import: its class's package and name, its outer and its own name.
pub struct Import {
    pub class_package: &'static str,
    pub class: &'static str,
    pub outer: i32,
    pub name: &'static str,
}

/// The package a builder lays out: a name table, imports, and a class whose functions follow it,
/// then the class's default object when it has one.
pub struct Package {
    pub names: Vec<String>,
    pub imports: Vec<Import>,
    pub class: &'static str,
    pub members: Vec<Field>,
    pub functions: Vec<Function>,
    /// The class the Blueprint derives from, by the index the package names it by; 0 for none.
    pub parent: i32,
    /// The tagged properties the class stores itself, ahead of their `None`.
    pub class_values: Vec<u8>,
    /// The tagged properties of the class's default object, which the package then holds after
    /// the functions, named `Default__` and the class.
    pub default_values: Option<Vec<u8>>,
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
            parent: 0,
            class_values: Vec::new(),
            default_values: None,
        }
    }

    /// A tag's head against this package's names: the property's name, its type, its value's size
    /// and an array index of 0, then no property guid.
    fn tag(&self, out: &mut Vec<u8>, property: &str, kind: &str, size: usize) {
        self.name_bytes(out, property);
        self.name_bytes(out, kind);
        out.extend_from_slice(&(size as i32).to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
        out.push(0);
    }

    /// An int property's tag and value.
    pub fn tag_int(&self, out: &mut Vec<u8>, property: &str, value: i32) {
        self.tag(out, property, "IntProperty", 4);
        out.extend_from_slice(&value.to_le_bytes());
    }

    /// An object property's tag and the package index it holds.
    pub fn tag_object(&self, out: &mut Vec<u8>, property: &str, index: i32) {
        self.tag(out, property, "ObjectProperty", 4);
        out.extend_from_slice(&index.to_le_bytes());
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
        out.extend_from_slice(&function.flags.to_le_bytes()); // FunctionFlags
        out.extend_from_slice(&0i32.to_le_bytes()); // EventGraphFunction
        out.extend_from_slice(&0i32.to_le_bytes()); // EventGraphCallOffset
        out
    }

    fn class_body(&self) -> Vec<u8> {
        let mut out = self.class_values.clone();
        self.name_bytes(&mut out, "None");
        out.extend_from_slice(&0i32.to_le_bytes());
        out.extend_from_slice(&self.parent.to_le_bytes());
        out.extend_from_slice(&(self.functions.len() as i32).to_le_bytes());
        for n in 0..self.functions.len() {
            out.extend_from_slice(&self.function_index(n).to_le_bytes());
        }
        self.fields(&mut out, &self.members);
        out.extend_from_slice(&0i32.to_le_bytes()); // no bytecode, loaded
        out.extend_from_slice(&0i32.to_le_bytes()); // no bytecode, stored
        // The function map: each function's name and index.
        out.extend_from_slice(&(self.functions.len() as i32).to_le_bytes());
        for (n, function) in self.functions.iter().enumerate() {
            self.name_bytes(&mut out, function.name);
            out.extend_from_slice(&self.function_index(n).to_le_bytes());
        }
        out.extend_from_slice(&0u32.to_le_bytes()); // ClassFlags
        out.extend_from_slice(&0i32.to_le_bytes()); // ClassWithin
        self.name_bytes(&mut out, "None"); // ClassConfigName
        out.extend_from_slice(&0i32.to_le_bytes()); // ClassGeneratedBy
        out.extend_from_slice(&0i32.to_le_bytes()); // no interfaces
        out.extend_from_slice(&0u32.to_le_bytes()); // bDeprecatedForceScriptOrder
        self.name_bytes(&mut out, "None");
        out.extend_from_slice(&1u32.to_le_bytes()); // bCooked
        let default_object = match self.default_values {
            Some(_) => self.functions.len() as i32 + 2,
            None => 0,
        };
        out.extend_from_slice(&default_object.to_le_bytes());
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
            tagged: false,
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
        let mut bodies = vec![(self.class.to_string(), -2, 0, self.class_body())];
        for function in &self.functions {
            let loaded = self.loaded_size(&function.script);
            bodies.push((
                function.name.to_string(),
                -3,
                self.class_index(),
                self.function_body(function, loaded),
            ));
        }
        if let Some(values) = &self.default_values {
            let mut body = values.clone();
            self.name_bytes(&mut body, "None");
            body.extend_from_slice(&0i32.to_le_bytes()); // no object guid
            bodies.push((
                format!("Default__{}", self.class),
                self.class_index(),
                0,
                body,
            ));
        }
        let mut exports_bytes = Vec::new();
        let mut exports = Vec::new();
        for (name, class, outer, body) in bodies {
            exports.push(FObjectExport {
                object_name: self.name(&name),
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
    event_graph_named("ExecuteUbergraph_BP_Test")
}

/// [`event_graph`] with its event graph named `graph`, which only its flags mark as one.
pub fn event_graph_named(graph_name: &'static str) -> Package {
    graph_package(graph_name, false)
}

/// [`event_graph`] with a second event, `ReceiveTick`, entering the graph at its switch.
pub fn event_graph_two_events() -> Package {
    graph_package("ExecuteUbergraph_BP_Test", true)
}

/// The two-event graph laid out from text: shared code both events reach, a Delay only Tick runs
/// and resumes from, a local only that resume reads, and a statement after the return nothing
/// reaches.
pub const TWO_EVENTS: &str = "Jump LocalVariable(EntryPoint)\n\
    @000A:\n\
    Let LocalVariable(Count) = 5\n\
    Self->LocalFinalFunction /Game/Test.BP_Test_C:ReceiveBeginPlay()\n\
    Jump @shared\n\
    @0047:\n\
    FinalFunction /Script/Engine.KismetSystemLibrary:Delay(StructConst</Script/Engine.LatentActionInfo, 24>(SkipOffsetConst(@wake), 7, 'ExecuteUbergraph_BP_Test', Self))\n\
    Jump @0092\n\
    @wake:\n\
    Jump @0092 unless LocalVariable(Flag)\n\
    @shared:\n\
    SwitchValue(LocalVariable(Count), 1 => IntOne, default => IntZero)\n\
    @0092:\n\
    Return Nothing\n\
    Let LocalVariable(Count) = 9\n\
    EndOfScript\n";

/// [`event_graph_two_events`] with its graph rewritten as [`TWO_EVENTS`] says.
pub fn two_events_laid() -> Built {
    let built = event_graph_two_events().build();
    let before = built.parsed();
    let edits = crate::edit::PackageEdits {
        script_texts: vec![crate::edit::ScriptTextEdit {
            export: 1,
            text: TWO_EVENTS.to_string(),
            was: None,
            object: None,
        }],
        ..Default::default()
    };
    let patched = crate::edit::patch_package_with(
        &built.bundle(),
        crate::edit::Sidecars::default(),
        &before,
        &edits,
        None,
    )
    .expect("the two-event graph lays out");
    Built {
        asset: patched.asset,
        exports: patched.exports,
    }
}

fn graph_package(graph_name: &'static str, tick: bool) -> Package {
    let mut names = vec![
        graph_name,
        "ReceiveBeginPlay",
        "EntryPoint",
        "Flag",
        "Count",
        "LatentActionInfo",
        "/Script/Engine",
        "KismetSystemLibrary",
        "Delay",
        "ScriptStruct",
    ];
    if tick {
        names.push("ReceiveTick");
    }
    let mut package = Package::new("BP_Test_C", &names);
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
        .name(graph_name)
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
        name: graph_name,
        fields: vec![
            param("EntryPoint", "IntProperty"),
            local("Flag", "BoolProperty"),
            local("Count", "IntProperty"),
        ],
        script: graph_code,
        flags: crate::ustruct::FUNC_UBERGRAPH,
    });
    package.functions.push(Function {
        name: "ReceiveBeginPlay",
        fields: Vec::new(),
        script: stub_code,
        flags: 0,
    });
    if tick {
        let tick_code = Code::new(&package)
            .op(0x46)
            .int(graph)
            .op(0x1D)
            .int(0x47)
            .ops(&[0x16, 0x04, 0x0B, 0x53])
            .bytes;
        package.functions.push(Function {
            name: "ReceiveTick",
            fields: Vec::new(),
            script: tick_code,
            flags: 0,
        });
    }
    package
}

/// A Blueprint class the mappings know nothing of, as one a mod adds, deriving from the native
/// `Actor` and declaring `Health`, with a default object storing `Health` 3 and nothing else.
pub fn class_on_native_parent() -> Built {
    let mut package = Package::new(
        "BP_Test_C",
        &["Health", "Default__BP_Test_C", "/Script/Engine", "Actor"],
    );
    let engine = package.import(Import {
        class_package: "/Script/CoreUObject",
        class: "Package",
        outer: 0,
        name: "/Script/Engine",
    });
    package.parent = package.import(Import {
        class_package: "/Script/CoreUObject",
        class: "Class",
        outer: engine,
        name: "Actor",
    });
    package.members.push(local("Health", "IntProperty"));
    let mut values = Vec::new();
    package.tag_int(&mut values, "Health", 3);
    package.default_values = Some(values);
    package.build()
}

/// The mappings [`class_on_native_parent`] reads against: `Actor`, declaring `bHidden`, under
/// `Object`, and no entry for the Blueprint class.
pub fn native_parent_mappings() -> crate::mappings::Mappings {
    use usmap::{Property, PropertyInner, Struct};
    crate::mappings::Mappings::from_structs(vec![
        Struct {
            name: "Object".into(),
            super_struct: None,
            properties: Vec::new(),
        },
        Struct {
            name: "Actor".into(),
            super_struct: Some("Object".into()),
            properties: vec![Property {
                name: "bHidden".into(),
                array_dim: 1,
                index: 0,
                inner: PropertyInner::Bool,
            }],
        },
    ])
}

/// A Blueprint class whose event graph no event enters, as a class is left once its events go:
/// the graph only returns, and the class names it as its event graph.
pub fn lone_event_graph() -> Built {
    let mut package = Package::new(
        "BP_Test_C",
        &[
            "ExecuteUbergraph_BP_Test",
            "EntryPoint",
            "UberGraphFunction",
        ],
    );
    package.functions.push(Function {
        name: "ExecuteUbergraph_BP_Test",
        fields: vec![param("EntryPoint", "IntProperty")],
        script: vec![0x04, 0x0B, 0x53],
        flags: crate::ustruct::FUNC_UBERGRAPH,
    });
    let mut values = Vec::new();
    package.tag_object(&mut values, "UberGraphFunction", package.function_index(0));
    package.class_values = values;
    package.build()
}
