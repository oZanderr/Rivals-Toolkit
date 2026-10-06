//! The JSON form of a package edit list, shared by the desktop app, the CLI and edit files on disk.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use rivals_uasset::{
    BulkEdit, DependencyEdit, DuplicateExport, ExportEdit, ImportEdit, KeyEdit, PackageEdits,
    PayloadEdit, RowEdit, ScriptConstEdit, ScriptTextEdit, StringEdit, ValueEdit,
};

use crate::paths::{mods_dir, paks_dir};

/// Every change one save makes, in the form the app sends and an edit file holds. Bulk and payload
/// bytes are named by file rather than inlined: they are large by nature, and a path keeps an edit
/// file readable. A function's text may be either.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EditList {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<ValueEdit>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub imports: Vec<ImportEdit>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rows: Vec<RowEdit>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub strings: Vec<StringEdit>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<KeyEdit>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bulk: Vec<BulkFile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub payloads: Vec<PayloadFile>,
    /// Literal constants changed in place inside a function's bytecode.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scripts: Vec<ScriptConstEdit>,
    /// Whole functions written anew from assembler text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub script_texts: Vec<ScriptTextFile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remove_exports: Vec<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reset_exports: Vec<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub duplicate_exports: Vec<DuplicateExport>,
    /// Changes to the export table's own rows: names, outers, archetypes and flags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub export_edits: Vec<ExportEdit>,
    /// Replacements for whole preload dependency runs, which decide the order the loader builds
    /// the package in.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<DependencyEdit>,
    /// Values set inside structs that store nothing yet. See [`rivals_uasset::FieldSet`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub field_sets: Vec<rivals_uasset::FieldSet>,
    /// Changes named by object and property path, placed in the package as the save reads it.
    /// Each holds itself to what it was written against, so these need no `expect`. See
    /// [`rivals_uasset::PathEdit`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<rivals_uasset::PathEdit>,
    /// Drop the names nothing in the package uses. A save of its own.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub compact_names: bool,
    /// Empty objects of a class to add. A save of its own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub add_exports: Vec<rivals_uasset::AddExport>,
    /// Variables to add to a Blueprint class whose instances all live in its package. A save of
    /// its own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub add_variables: Vec<rivals_uasset::AddVariable>,
    /// Components to add to a Blueprint by duplicating one. A save of its own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub add_components: Vec<rivals_uasset::AddComponent>,
    /// Components to take out of a Blueprint. A save of its own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remove_components: Vec<rivals_uasset::RemoveComponent>,
    /// Save the package under another name once the other edits are made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub save_as: Option<rivals_uasset::SaveAs>,
    /// What the edits were written against. An edit file without it is applied unchecked.
    #[serde(default, skip_serializing_if = "rivals_uasset::Expected::is_empty")]
    pub expect: rivals_uasset::Expected,
    /// Apply even where the package no longer matches `expect`. Set by the caller, never stored.
    #[serde(skip)]
    pub allow_drift: bool,
    /// Save imports that point at nothing the game or an enabled mod has. Set by the caller.
    #[serde(skip)]
    pub allow_missing: bool,
    /// Save script edits pointing at something whose kind could not be confirmed. Set by the caller.
    #[serde(skip)]
    pub allow_unchecked: bool,
}

/// New bytes for one bulk data resource, as a file to read them from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BulkFile {
    pub resource: u32,
    pub file: String,
}

/// New bytes for an export's payload, as a file to read them from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PayloadFile {
    #[serde(default)]
    pub export: u32,
    /// The object by path rather than by index. See [`rivals_uasset::place_named`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object: Option<String>,
    pub file: String,
}

/// A function's whole script as assembler text: given inline, or as a file to read it from. With
/// `new_function`, the text is the script of a function added to the class under that name.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScriptTextFile {
    #[serde(default)]
    pub export: u32,
    /// The function by path rather than by index. See [`rivals_uasset::place_named`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object: Option<String>,
    /// A function to add to the class, by the name it takes, written from this text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_function: Option<String>,
    /// The new function's parameters, as a signature prints: `(A: Int, ref B: Array<Int>) -> Hit: Bool`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// The class the new function goes to, when the package holds more than one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// The text the script printed as when the edit was made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub was: Option<String>,
}

/// Text read from a file, whichever encoding a shell wrote it in: UTF-8, with or without a byte
/// order mark, or UTF-16 behind one, which is what Windows PowerShell's `>` writes.
pub fn decode_text(bytes: &[u8]) -> Result<String, String> {
    let utf16 = |bytes: &[u8], big: bool| {
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| {
                if big {
                    u16::from_be_bytes([pair[0], pair[1]])
                } else {
                    u16::from_le_bytes([pair[0], pair[1]])
                }
            })
            .collect();
        String::from_utf16(&units).map_err(|e| format!("not UTF-16 text: {e}"))
    };
    match bytes {
        [0xEF, 0xBB, 0xBF, rest @ ..] => {
            String::from_utf8(rest.to_vec()).map_err(|e| format!("not UTF-8 text: {e}"))
        }
        [0xFF, 0xFE, rest @ ..] => utf16(rest, false),
        [0xFE, 0xFF, rest @ ..] => utf16(rest, true),
        _ => String::from_utf8(bytes.to_vec()).map_err(|e| format!("not UTF-8 text: {e}")),
    }
}

impl EditList {
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
            && self.imports.is_empty()
            && self.rows.is_empty()
            && self.strings.is_empty()
            && self.keys.is_empty()
            && self.bulk.is_empty()
            && self.payloads.is_empty()
            && self.scripts.is_empty()
            && self.script_texts.is_empty()
            && self.remove_exports.is_empty()
            && self.reset_exports.is_empty()
            && self.duplicate_exports.is_empty()
            && self.export_edits.is_empty()
            && self.dependencies.is_empty()
            && self.field_sets.is_empty()
            && self.paths.is_empty()
            && !self.compact_names
            && self.add_exports.is_empty()
            && self.add_variables.is_empty()
            && self.add_components.is_empty()
            && self.remove_components.is_empty()
            && self.save_as.is_none()
    }

    /// Records what these edits find in `parsed`, the package they were written against, so applying
    /// them to one that has changed since is refused.
    pub fn expect_from(&mut self, parsed: &rivals_uasset::ParsedPackage) {
        // Only which exports and resources the files name matters here, not their bytes.
        let skeleton = PackageEdits {
            bulk: self
                .bulk
                .iter()
                .map(|entry| BulkEdit {
                    resource: entry.resource,
                    bytes: Vec::new(),
                })
                .collect(),
            payloads: self
                .payloads
                .iter()
                .map(|entry| PayloadEdit {
                    export: entry.export,
                    bytes: Vec::new(),
                    object: entry.object.clone(),
                })
                .collect(),
            values: self.values.clone(),
            imports: self.imports.clone(),
            rows: self.rows.clone(),
            strings: self.strings.clone(),
            keys: self.keys.clone(),
            scripts: self.scripts.clone(),
            script_texts: self
                .script_texts
                .iter()
                .filter(|entry| entry.new_function.is_none())
                .map(|entry| ScriptTextEdit {
                    export: entry.export,
                    object: entry.object.clone(),
                    ..Default::default()
                })
                .collect(),
            remove_exports: self.remove_exports.clone(),
            reset_exports: self.reset_exports.clone(),
            duplicate_exports: self.duplicate_exports.clone(),
            exports: self.export_edits.clone(),
            dependencies: self.dependencies.clone(),
            field_sets: self.field_sets.clone(),
            ..Default::default()
        };
        self.expect = rivals_uasset::expectations(parsed, &skeleton);
    }

    /// Reads the bulk and payload files, so what comes back is what the patcher takes. A relative
    /// path resolves against `base`, which is the edit file's own directory.
    pub fn resolve(self, base: &Path) -> Result<PackageEdits, String> {
        let read = |file: &str| -> Result<Vec<u8>, String> {
            let named = Path::new(file);
            let path = if named.is_absolute() {
                named.to_path_buf()
            } else {
                base.join(named)
            };
            std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))
        };
        let mut bulk = Vec::with_capacity(self.bulk.len());
        for entry in &self.bulk {
            bulk.push(BulkEdit {
                resource: entry.resource,
                bytes: read(&entry.file)?,
            });
        }
        let mut payloads = Vec::with_capacity(self.payloads.len());
        for entry in &self.payloads {
            payloads.push(PayloadEdit {
                export: entry.export,
                bytes: read(&entry.file)?,
                object: entry.object.clone(),
            });
        }
        let mut script_texts = Vec::with_capacity(self.script_texts.len());
        let mut new_functions = Vec::new();
        for entry in self.script_texts {
            let named = entry
                .new_function
                .clone()
                .unwrap_or_else(|| format!("export {}", entry.export));
            let text = match (entry.text, &entry.file) {
                (Some(text), None) => text,
                (None, Some(file)) => {
                    decode_text(&read(file)?).map_err(|e| format!("{file}: {e}"))?
                }
                _ => {
                    return Err(format!(
                        "the script text for {named} takes `text` or `file`, one of them"
                    ));
                }
            };
            match entry.new_function {
                Some(name) => new_functions.push(rivals_uasset::NewFunctionEdit {
                    class: entry.class,
                    name,
                    signature: entry.signature.ok_or_else(|| {
                        format!("the new function {named} needs a `signature`, as \"()\" for one taking nothing")
                    })?,
                    text,
                }),
                None => script_texts.push(ScriptTextEdit {
                    export: entry.export,
                    text,
                    was: entry.was,
                    object: entry.object,
                }),
            }
        }
        Ok(PackageEdits {
            values: self.values,
            imports: self.imports,
            rows: self.rows,
            strings: self.strings,
            keys: self.keys,
            bulk,
            payloads,
            scripts: self.scripts,
            script_texts,
            remove_exports: self.remove_exports,
            reset_exports: self.reset_exports,
            duplicate_exports: self.duplicate_exports,
            exports: self.export_edits,
            dependencies: self.dependencies,
            field_sets: self.field_sets,
            paths: self.paths,
            compact_names: self.compact_names,
            add_exports: self.add_exports,
            new_functions,
            add_variables: self.add_variables,
            add_components: self.add_components,
            remove_components: self.remove_components,
            save_as: self.save_as,
            expect: self.expect,
            allow_drift: self.allow_drift,
            allow_missing: self.allow_missing,
            allow_unchecked: self.allow_unchecked,
        })
    }
}

/// One package's worth of edits, as a file on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditFile {
    /// A path, or a container file name to look up under `Paks` and then `Paks/~mods`.
    pub container: String,
    pub entry: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mod_name: Option<String>,
    /// What to write. Overridden by a command line flag, and falling back to the app's setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<super::SaveTarget>,
    #[serde(default)]
    pub replace: bool,
    /// Build on the mod's own copy when it already holds one. The edits must then have been made
    /// against that copy.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub layer: bool,
    /// What the tool that wrote this file could not express, kept beside the edits rather than
    /// lost. Nothing reads them back; they are for the person holding the file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    pub edits: EditList,
}

/// Several edit files applied in one run, each independently.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mod_name: Option<String>,
    pub items: Vec<ManifestItem>,
}

/// An item is either the path of an edit file or one written inline.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ManifestItem {
    File(String),
    Inline(Box<EditFile>),
}

pub fn read_edit_file(path: &Path) -> Result<EditFile, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// The items a manifest names, each with the directory its own relative paths resolve against.
pub fn read_manifest(path: &Path) -> Result<Vec<(PathBuf, EditFile)>, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let manifest: Manifest =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let base = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let mut items = Vec::with_capacity(manifest.items.len());
    for item in manifest.items {
        match item {
            ManifestItem::File(relative) => {
                let file = base.join(&relative);
                let mut held = read_edit_file(&file)?;
                if held.mod_name.is_none() {
                    held.mod_name = manifest.mod_name.clone();
                }
                let own = file.parent().unwrap_or(Path::new(".")).to_path_buf();
                items.push((own, held));
            }
            ManifestItem::Inline(held) => {
                let mut held = *held;
                if held.mod_name.is_none() {
                    held.mod_name = manifest.mod_name.clone();
                }
                items.push((base.clone(), held));
            }
        }
    }
    Ok(items)
}

/// Where a container named in an edit file lives: as written, beside the file, or by bare name in
/// the game's own `Paks` folder and then in `~mods`.
pub fn resolve_container(spec: &str, base: &Path, game_root: &str) -> Result<String, String> {
    if spec.is_empty() {
        return Ok(String::new());
    }
    let tried = [
        PathBuf::from(spec),
        base.join(spec),
        paks_dir(game_root).join(spec),
        mods_dir(game_root).join(spec),
    ];
    for candidate in &tried {
        if candidate.is_file() {
            return Ok(candidate.to_string_lossy().into_owned());
        }
    }
    Err(format!(
        "no container named {spec}; looked at {}",
        tried
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rivals-json-{tag}-{stamp}"));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// The file form is the wire form: an edit file holds exactly what the app sends.
    #[test]
    fn an_edit_file_reads_every_family_it_names() {
        let json = r#"{
            "container": "pakchunk0-Windows.utoc",
            "entry": "Marvel/Content/X.uasset",
            "mod_name": "MyMod",
            "edits": {
                "values": [{"offset": 16, "name": "Damage", "kind": "float", "op": "set", "text": "42.5"}],
                "rows": [{"export": 0, "op": "remove", "name": "Row_D"}],
                "imports": [{"op": "add", "path": "/Game/X.X"}],
                "scripts": [{"export": 5, "statement": 1636, "value": "1000"}],
                "reset_exports": [3]
            }
        }"#;
        let file: EditFile = serde_json::from_str(json).expect("parse");
        assert_eq!(file.mod_name.as_deref(), Some("MyMod"));
        assert!(!file.replace);
        assert_eq!(file.edits.values.len(), 1);
        assert_eq!(file.edits.rows.len(), 1);
        assert_eq!(file.edits.imports.len(), 1);
        assert_eq!(file.edits.scripts.len(), 1);
        assert_eq!(file.edits.scripts[0].statement, 0x0664);
        assert_eq!(file.edits.scripts[0].constant, 0);
        assert_eq!(file.edits.reset_exports, vec![3]);
        assert!(file.edits.keys.is_empty());
    }

    /// The exact object the inspector sends. The app, the CLI and an edit file share one shape,
    /// so a field renamed on either side has to break here first.
    #[test]
    fn the_shape_the_inspector_sends_is_the_shape_the_engine_reads() {
        let sent = r#"{
            "values": [
                {"offset": 9496, "name": "HeroID", "kind": "str", "op": "set", "text": "2022"},
                {"offset": 16, "name": "Tags", "element": 1, "kind": "array", "op": "set_element", "index": 0, "text": "A"},
                {"offset": 32, "name": "Owners", "kind": "set", "op": "insert", "index": 0, "key": "Hero.A"},
                {"offset": 48, "name": "Radius", "kind": "float", "op": "clear"},
                {"offset": 64, "name": "Extra", "kind": "unset", "op": "store"},
                {"offset": 80, "name": "Scores", "kind": "map", "op": "set_key", "index": 1, "text": "Z"},
                {"offset": 96, "name": "Tags", "kind": "array", "op": "reorder", "order": [2, 0, 1]},
                {"offset": 112, "name": "Blob", "kind": "struct", "op": "set_raw", "hex": "0a000000"}
            ],
            "imports": [
                {"op": "retarget", "import": 2, "path": "/Game/A.A", "class": ["/Script/Engine", "Material"]},
                {"op": "add", "path": "/Game/B.B"}
            ],
            "rows": [
                {"export": 0, "op": "add", "name": "NewRow", "at": 2},
                {"export": 0, "op": "duplicate", "source": "Row_A", "name": "Row_A2"},
                {"export": 0, "op": "rename", "name": "Row_B", "to": "Row_C"}
            ],
            "strings": [
                {"export": 1, "op": "set_source", "index": 3, "key": "K", "to": "Hello"},
                {"export": 1, "op": "set_meta_data", "index": 3, "key": "K", "id": "Comment", "to": "note"},
                {"export": 1, "op": "remove_meta_data", "index": 3, "key": "K", "id": "Comment"},
                {"export": 1, "op": "add", "key": "New", "source": "Text"}
            ],
            "keys": [
                {"offset": 100, "name": "Channel", "op": "add", "time": 24, "value": 0.5},
                {"offset": 100, "name": "Channel", "op": "move", "index": 0, "time": 12}
            ],
            "payloads": [],
            "bulk": [],
            "remove_exports": [],
            "reset_exports": [4],
            "duplicate_exports": [{"export": 2, "name": "Copy"}]
        }"#;
        let list: EditList = serde_json::from_str(sent).expect("the inspector's own shape");
        assert_eq!(list.values.len(), 8);
        assert_eq!(list.imports.len(), 2);
        assert_eq!(list.rows.len(), 3);
        assert_eq!(list.strings.len(), 4);
        assert_eq!(list.keys.len(), 2);
        assert_eq!(list.reset_exports, vec![4]);
        assert_eq!(list.duplicate_exports.len(), 1);
        assert!(!list.is_empty());
        // Nothing to read from disk, so this is the whole conversion the save path makes.
        let edits = list.resolve(Path::new("")).expect("resolve");
        assert_eq!(edits.values.len(), 8);
        assert_eq!(edits.duplicate_exports[0].name, "Copy");
    }

    /// Edits named by path carry every op, each with what it was written against where it has one,
    /// and reach the save as they were written.
    #[test]
    fn path_edits_read_every_op() {
        let json = r#"{"paths": [
            {"export": "Table", "row": "Row_A", "path": "Damage", "op": "set", "text": "5", "was": "4"},
            {"export": "Table", "path": "Tags{Hero.A}", "op": "remove"},
            {"export": "Table", "path": "Scores{A}", "op": "set_key", "text": "B"},
            {"export": "BP_C:Mesh_GEN_VARIABLE", "path": "Items", "op": "insert"},
            {"export": "BP_C:Mesh_GEN_VARIABLE", "path": "Owners", "op": "insert", "index": 0, "key": "K"},
            {"export": "Struct", "defaults": true, "path": "Inner.X", "op": "clear"},
            {"export": "0", "path": "Extra", "op": "store"},
            {"export": "0", "path": "Extra", "op": "unset"},
            {"export": "0", "path": "Tags", "op": "reorder", "order": [1, 0]},
            {"export": "0", "path": "Blob", "op": "set_raw", "hex": "0a000000"}
        ]}"#;
        let list: EditList = serde_json::from_str(json).expect("parse");
        assert!(!list.is_empty());
        let edits = list.clone().resolve(Path::new("")).expect("resolve");
        assert_eq!(edits.paths, list.paths);
        assert_eq!(edits.paths.len(), 10);
        assert_eq!(edits.paths[0].was.as_deref(), Some("4"));
        assert!(edits.paths[5].defaults);
        assert!(matches!(
            edits.paths[3].op,
            rivals_uasset::PathOp::Insert {
                index: None,
                key: None
            }
        ));
        let written = serde_json::to_value(&list).expect("json");
        assert_eq!(
            written["paths"][3],
            serde_json::json!({"export": "BP_C:Mesh_GEN_VARIABLE", "path": "Items", "op": "insert"})
        );
        let error = serde_json::from_str::<EditList>(
            r#"{"paths": [{"export": "0", "path": "A", "op": "set_element", "index": 1}]}"#,
        )
        .expect_err("refused");
        assert!(error.to_string().contains("set_element"), "{error}");
    }

    /// A misspelt op is refused rather than quietly dropped, which is what makes a hand written
    /// edit file safe to run.
    #[test]
    fn an_unknown_op_is_refused() {
        let error = serde_json::from_str::<EditList>(
            r#"{"values": [{"offset": 0, "name": "A", "kind": "int", "op": "nope"}]}"#,
        )
        .expect_err("refused");
        assert!(error.to_string().contains("nope"), "{error}");
    }

    /// Payload and bulk bytes come from files beside the edit file, so an edit list stays readable.
    #[test]
    fn payload_and_bulk_files_resolve_against_the_edit_file() {
        let dir = scratch("resolve");
        std::fs::write(dir.join("mip0.bin"), [1, 2, 3]).expect("write");
        let list = EditList {
            bulk: vec![BulkFile {
                resource: 0,
                file: "mip0.bin".into(),
            }],
            ..Default::default()
        };
        let edits = list.resolve(&dir).expect("resolve");
        assert_eq!(edits.bulk.len(), 1);
        assert_eq!(edits.bulk[0].bytes, vec![1, 2, 3]);

        let missing = EditList {
            payloads: vec![PayloadFile {
                export: 0,
                file: "gone.bin".into(),
                object: None,
            }],
            ..Default::default()
        };
        let error = missing.resolve(&dir).expect_err("no such file");
        assert!(error.contains("gone.bin"), "{error}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A manifest takes both forms of item and hands its own mod name to those without one.
    #[test]
    fn a_manifest_reads_paths_and_inline_items() {
        let dir = scratch("manifest");
        std::fs::write(
            dir.join("one.json"),
            r#"{"container":"c.utoc","entry":"a.uasset","edits":{}}"#,
        )
        .expect("write");
        std::fs::write(
            dir.join("manifest.json"),
            r#"{"mod_name":"Batch","items":["one.json",{"container":"c.utoc","entry":"b.uasset","mod_name":"Own","edits":{}}]}"#,
        )
        .expect("write");
        let items = read_manifest(&dir.join("manifest.json")).expect("manifest");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].1.entry, "a.uasset");
        assert_eq!(items[0].1.mod_name.as_deref(), Some("Batch"));
        assert_eq!(items[1].1.mod_name.as_deref(), Some("Own"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A container may be named by path or by the bare file name of one the game ships.
    #[test]
    fn a_container_resolves_beside_the_edit_file_before_the_game() {
        let dir = scratch("container");
        std::fs::write(dir.join("here.utoc"), b"x").expect("write");
        let found = resolve_container("here.utoc", &dir, "C:/nowhere").expect("found");
        assert!(found.ends_with("here.utoc"), "{found}");
        let error = resolve_container("absent.utoc", &dir, "C:/nowhere").expect_err("refused");
        assert!(error.contains("absent.utoc"), "{error}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A function's text travels inline, as the app sends it, or as a file beside the edit file,
    /// as a person writes one; never both.
    #[test]
    fn a_script_text_is_inline_or_a_file_beside_the_edit_file() {
        let dir = scratch("text");
        std::fs::write(dir.join("fn.txt"), "Return Nothing\nEndOfScript\n").expect("write");
        let list: EditList = serde_json::from_str(
            r#"{"script_texts": [
                {"export": 4, "text": "EndOfScript\n", "was": "Return Nothing\nEndOfScript\n"},
                {"export": 5, "file": "fn.txt"}
            ]}"#,
        )
        .expect("parse");
        assert!(!list.is_empty());
        let edits = list.resolve(&dir).expect("resolved");
        assert_eq!(edits.script_texts.len(), 2);
        assert_eq!(edits.script_texts[0].text, "EndOfScript\n");
        assert_eq!(
            edits.script_texts[0].was.as_deref(),
            Some("Return Nothing\nEndOfScript\n")
        );
        assert_eq!(edits.script_texts[1].export, 5);
        assert_eq!(edits.script_texts[1].text, "Return Nothing\nEndOfScript\n");

        let both: EditList = serde_json::from_str(
            r#"{"script_texts": [{"export": 4, "text": "x", "file": "fn.txt"}]}"#,
        )
        .expect("parse");
        let error = both.resolve(&dir).expect_err("refused");
        assert!(error.contains("`text` or `file`"), "{error}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Windows PowerShell's `>` writes UTF-16 behind a byte order mark, which reads the same as the
    /// UTF-8 a shell elsewhere writes.
    #[test]
    fn text_reads_whichever_encoding_a_shell_wrote() {
        let text = "Jump @0045 unless LocalVariable(bOk)\n";
        let mut utf16 = vec![0xFF, 0xFE];
        for unit in text.encode_utf16() {
            utf16.extend_from_slice(&unit.to_le_bytes());
        }
        assert_eq!(decode_text(&utf16).as_deref(), Ok(text));
        let mut bom = vec![0xEF, 0xBB, 0xBF];
        bom.extend_from_slice(text.as_bytes());
        assert_eq!(decode_text(&bom).as_deref(), Ok(text));
        assert_eq!(decode_text(text.as_bytes()).as_deref(), Ok(text));
        assert!(decode_text(&[0xC3, 0x28]).is_err());
    }
}
