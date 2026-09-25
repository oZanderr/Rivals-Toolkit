//! The JSON form of a package edit list, shared by the desktop app, the CLI and edit files on disk.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use rivals_uasset::{
    BulkEdit, DependencyEdit, DuplicateExport, ExportEdit, ImportEdit, KeyEdit, PackageEdits,
    PayloadEdit, RowEdit, ScriptConstEdit, StringEdit, ValueEdit,
};

use crate::paths::{mods_dir, paks_dir};

/// Every change one save makes, in the form the app sends and an edit file holds. Bulk and payload
/// bytes are named by file rather than inlined: they are large by nature, and a path keeps an edit
/// file readable.
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
    /// What the edits were written against. An edit file without it is applied unchecked.
    #[serde(default, skip_serializing_if = "rivals_uasset::Expected::is_empty")]
    pub expect: rivals_uasset::Expected,
    /// Apply even where the package no longer matches `expect`. Set by the caller, never stored.
    #[serde(skip)]
    pub allow_drift: bool,
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
    pub export: u32,
    pub file: String,
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
            && self.remove_exports.is_empty()
            && self.reset_exports.is_empty()
            && self.duplicate_exports.is_empty()
            && self.export_edits.is_empty()
            && self.dependencies.is_empty()
            && self.field_sets.is_empty()
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
                })
                .collect(),
            values: self.values.clone(),
            imports: self.imports.clone(),
            rows: self.rows.clone(),
            strings: self.strings.clone(),
            keys: self.keys.clone(),
            scripts: self.scripts.clone(),
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
            });
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
            remove_exports: self.remove_exports,
            reset_exports: self.reset_exports,
            duplicate_exports: self.duplicate_exports,
            exports: self.export_edits,
            dependencies: self.dependencies,
            field_sets: self.field_sets,
            expect: self.expect,
            allow_drift: self.allow_drift,
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
                {"offset": 64, "name": "Extra", "kind": "unset", "op": "store"}
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
        assert_eq!(list.values.len(), 5);
        assert_eq!(list.imports.len(), 2);
        assert_eq!(list.rows.len(), 3);
        assert_eq!(list.strings.len(), 4);
        assert_eq!(list.keys.len(), 2);
        assert_eq!(list.reset_exports, vec![4]);
        assert_eq!(list.duplicate_exports.len(), 1);
        assert!(!list.is_empty());
        // Nothing to read from disk, so this is the whole conversion the save path makes.
        let edits = list.resolve(Path::new("")).expect("resolve");
        assert_eq!(edits.values.len(), 5);
        assert_eq!(edits.duplicate_exports[0].name, "Copy");
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
}
