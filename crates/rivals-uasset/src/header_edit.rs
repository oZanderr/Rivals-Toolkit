//! Edits to a package's import table, so references can reach objects the package never named.

use retoc::legacy_asset::{FLegacyPackageHeader, FMinimalName, FObjectImport, FPackageNameMap};
use retoc::zen::FPackageIndex;

use crate::edit::AppliedEdit;
use crate::package::path_from;

/// One change to the import table.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ImportEdit {
    /// Point an existing import at another object. `class` replaces the import's class when given;
    /// otherwise the import keeps the class it had.
    Retarget {
        import: u32,
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        class: Option<(String, String)>,
    },
    /// Add an import for `path`, an object of the given class.
    Add {
        path: String,
        #[serde(default = "placeholder_package")]
        class_package: String,
        #[serde(default = "placeholder_name")]
        class_name: String,
    },
    /// Drop an import the package no longer names. Every index above it moves down, which is why
    /// this is a save of its own.
    Remove { import: u32 },
}

/// The class an import is given when nothing says better. The linker accepts any subclass, and
/// retoc uses the same placeholder for imports it cannot resolve.
const PLACEHOLDER_CLASS: (&str, &str) = ("/Script/CoreUObject", "Object");

fn placeholder_package() -> String {
    PLACEHOLDER_CLASS.0.to_string()
}

fn placeholder_name() -> String {
    PLACEHOLDER_CLASS.1.to_string()
}

/// The name map and import table as they stand while a patch is assembled. Both grow as edits add
/// to them, and every later edit resolves against the grown tables.
pub(crate) struct Tables {
    pub names: FPackageNameMap,
    pub imports: Vec<FObjectImport>,
}

impl Tables {
    fn name(&self, name: FMinimalName) -> Option<String> {
        self.names.get(name).ok().map(|n| n.into_owned())
    }

    /// The class of an import, as the package and name it lives under.
    pub(crate) fn import_class(&self, index: i32) -> Option<(String, String)> {
        let index = FPackageIndex { index };
        if !index.is_import() {
            return None;
        }
        let import = self.imports.get(index.to_import_index() as usize)?;
        Some((
            self.name(import.class_package)?,
            self.name(import.class_name)?,
        ))
    }

    fn find(&self, outer: FPackageIndex, name: &str) -> Option<FPackageIndex> {
        self.imports
            .iter()
            .position(|import| {
                import.outer_index == outer
                    && self.name(import.object_name).as_deref() == Some(name)
            })
            .map(|at| FPackageIndex::create_import(at as u32))
    }

    fn push(&mut self, outer: FPackageIndex, name: &str, class: (&str, &str)) -> FPackageIndex {
        let import = FObjectImport {
            class_package: self.names.store(class.0),
            class_name: self.names.store(class.1),
            outer_index: outer,
            object_name: self.names.store(name),
            is_optional: false,
        };
        self.imports.push(import);
        FPackageIndex::create_import((self.imports.len() - 1) as u32)
    }

    /// The import for a package, added when the package was never named before.
    fn ensure_package(&mut self, package: &str) -> FPackageIndex {
        self.find(FPackageIndex::create_null(), package)
            .unwrap_or_else(|| {
                self.push(
                    FPackageIndex::create_null(),
                    package,
                    ("/Script/CoreUObject", "Package"),
                )
            })
    }

    fn ensure_object(
        &mut self,
        outer: FPackageIndex,
        name: &str,
        class: (&str, &str),
    ) -> FPackageIndex {
        self.find(outer, name)
            .unwrap_or_else(|| self.push(outer, name, class))
    }

    /// Imports for every object on a path but the last, so the last can be placed under them.
    fn ensure_outers(&mut self, path: &ObjectPath) -> FPackageIndex {
        let mut outer = self.ensure_package(&path.package);
        for name in &path.chain[..path.chain.len() - 1] {
            outer = self.ensure_object(outer, name, PLACEHOLDER_CLASS);
        }
        outer
    }
}

/// A UE object path: the package, then the object and the subobjects inside it.
pub struct ObjectPath {
    pub package: String,
    /// Outermost first, never empty.
    pub chain: Vec<String>,
}

/// Parses the dotted form `/Game/Path/Asset.Object` or `/Game/Path/Asset.Object:Sub`. The
/// slash-separated form does not say where the package ends, so it is not accepted here.
pub fn parse_object_path(text: &str) -> Result<ObjectPath, String> {
    let text = text.trim();
    let malformed = || {
        format!(
            "{text} is not an object path; expected /Package/Path.Object or /Package/Path.Object:Sub"
        )
    };
    let (package, rest) = text.split_once('.').ok_or_else(malformed)?;
    if !package.starts_with('/') || package.len() < 2 || rest.is_empty() {
        return Err(malformed());
    }
    let chain: Vec<String> = rest.split(':').map(str::to_string).collect();
    if chain.iter().any(String::is_empty) {
        return Err(malformed());
    }
    Ok(ObjectPath {
        package: package.to_string(),
        chain,
    })
}

/// Adds imports for `path`, reusing any already there, and returns the object's own index. The
/// object takes `class` when given and the placeholder class otherwise.
pub(crate) fn add_import(
    tables: &mut Tables,
    path: &str,
    class: Option<(String, String)>,
) -> Result<i32, String> {
    let parsed = parse_object_path(path)?;
    let outer = tables.ensure_outers(&parsed);
    let last = parsed
        .chain
        .last()
        .ok_or("an object path needs an object")?;
    let class = class.as_ref().map_or(PLACEHOLDER_CLASS, |(package, name)| {
        (package.as_str(), name.as_str())
    });
    Ok(tables.ensure_object(outer, last, class).index)
}

/// Applies one import edit to the tables, describing what changed.
pub(crate) fn apply_import_edit(
    tables: &mut Tables,
    package: &FLegacyPackageHeader,
    edit: &ImportEdit,
) -> Result<AppliedEdit, String> {
    let describe = |tables: &Tables, index: FPackageIndex| {
        path_from(
            &tables.names,
            &tables.imports,
            &package.exports,
            &package.summary.package_name,
            index,
        )
        .unwrap_or_default()
    };
    match edit {
        // Removal moves every index above it, so it is a save of its own rather than one of these.
        ImportEdit::Remove { .. } => {
            Err("removing an import is a save of its own; nothing else may change with it".into())
        }
        ImportEdit::Add {
            path,
            class_package,
            class_name,
        } => {
            let index = add_import(
                tables,
                path,
                Some((class_package.clone(), class_name.clone())),
            )?;
            Ok(AppliedEdit {
                name: format!("import {index}"),
                offset: 0,
                offset_after: 0,
                element: None,
                elements_after: None,
                before: "(none)".into(),
                after: describe(tables, FPackageIndex { index }),
            })
        }
        ImportEdit::Retarget {
            import,
            path,
            class,
        } => {
            let at = *import as usize;
            let index = FPackageIndex::create_import(*import);
            if at >= tables.imports.len() {
                return Err(format!(
                    "this package has {} imports, so there is no import {import}",
                    tables.imports.len()
                ));
            }
            // Decoding an export follows its class, and its stored values were diffed against its
            // archetype, so those references cannot move without changing what the bytes mean.
            for (position, export) in package.exports.iter().enumerate() {
                let role = if export.class_index == index {
                    "class"
                } else if export.super_index == index {
                    "parent class"
                } else if export.template_index == index {
                    "archetype"
                } else {
                    continue;
                };
                return Err(format!(
                    "import {} is the {role} of export {position}, and retargeting it would change how that export is read",
                    index.index
                ));
            }
            let before = describe(tables, index);
            if tables.imports[at].outer_index.is_null() {
                // A package import names a package and nothing more.
                let text = path.trim();
                if !text.starts_with('/') || text.contains('.') || text.contains(':') {
                    return Err(format!(
                        "import {} is a package, so it takes a package path such as /Game/Path/Asset",
                        index.index
                    ));
                }
                let name = tables.names.store(text);
                tables.imports[at].object_name = name;
            } else {
                let parsed = parse_object_path(path)?;
                let outer = tables.ensure_outers(&parsed);
                let mut probe = outer;
                while probe.is_import() {
                    if probe == index {
                        return Err(format!(
                            "{path} would put import {} inside itself",
                            index.index
                        ));
                    }
                    probe = tables.imports[probe.to_import_index() as usize].outer_index;
                }
                let last = parsed
                    .chain
                    .last()
                    .ok_or("an object path needs an object")?;
                let name = tables.names.store(last);
                let entry = &mut tables.imports[at];
                entry.outer_index = outer;
                entry.object_name = name;
            }
            if let Some((class_package, class_name)) = class {
                let class_package = tables.names.store(class_package);
                let class_name = tables.names.store(class_name);
                let entry = &mut tables.imports[at];
                entry.class_package = class_package;
                entry.class_name = class_name;
            }
            Ok(AppliedEdit {
                name: format!("import {}", index.index),
                offset: 0,
                offset_after: 0,
                element: None,
                elements_after: None,
                before,
                after: describe(tables, index),
            })
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn tables() -> Tables {
        Tables {
            names: FPackageNameMap::create_from_names(vec!["None".into()]),
            imports: Vec::new(),
        }
    }

    #[test]
    fn a_dotted_path_splits_into_package_object_and_subobjects() {
        let path = parse_object_path("/Game/Heroes/BP_Hero.BP_Hero_C:Mesh").expect("path");
        assert_eq!(path.package, "/Game/Heroes/BP_Hero");
        assert_eq!(path.chain, ["BP_Hero_C", "Mesh"]);
        for bad in [
            "BP_Hero",
            "/Game/Heroes/BP_Hero",
            "/Game/A.",
            "/Game/A.B::C",
            "Game/A.B",
        ] {
            assert!(parse_object_path(bad).is_err(), "{bad}");
        }
    }

    /// The package import comes first and is shared: two objects from one package add three
    /// imports, not four.
    #[test]
    fn adding_two_objects_of_one_package_shares_the_package_import() {
        let mut tables = tables();
        let a = add_import(&mut tables, "/Game/Meshes/SM_A.SM_A", None).expect("a");
        let b = add_import(&mut tables, "/Game/Meshes/SM_A.SM_B", None).expect("b");
        assert_eq!(tables.imports.len(), 3);
        assert_eq!(a, -2);
        assert_eq!(b, -3);
        assert!(tables.imports[0].outer_index.is_null());
        assert_eq!(
            tables.imports[1].outer_index,
            FPackageIndex::create_import(0)
        );
        let again = add_import(&mut tables, "/Game/Meshes/SM_A.SM_A", None).expect("again");
        assert_eq!(again, a, "an object already imported is reused");
    }

    #[test]
    fn a_subobject_path_creates_its_outers_with_the_placeholder_class() {
        let mut tables = tables();
        let index = add_import(
            &mut tables,
            "/Game/BP.BP_C:Mesh",
            Some(("/Script/Engine".into(), "StaticMeshComponent".into())),
        )
        .expect("add");
        assert_eq!(tables.imports.len(), 3);
        assert_eq!(
            tables.import_class(index),
            Some(("/Script/Engine".into(), "StaticMeshComponent".into()))
        );
        assert_eq!(
            tables.import_class(-2),
            Some(("/Script/CoreUObject".into(), "Object".into()))
        );
    }

    #[test]
    fn retargeting_moves_an_import_under_a_new_package_and_keeps_its_class() {
        let mut tables = tables();
        let mesh = add_import(
            &mut tables,
            "/Game/Meshes/SM_A.SM_A",
            Some(("/Script/Engine".into(), "StaticMesh".into())),
        )
        .expect("add");
        let package = FLegacyPackageHeader::default();
        let done = apply_import_edit(
            &mut tables,
            &package,
            &ImportEdit::Retarget {
                import: FPackageIndex { index: mesh }.to_import_index(),
                path: "/Game/Meshes/SM_B.SM_B".into(),
                class: None,
            },
        )
        .expect("retarget");
        assert_eq!(done.before, "/Game/Meshes/SM_A.SM_A");
        assert_eq!(done.after, "/Game/Meshes/SM_B.SM_B");
        assert_eq!(
            tables.import_class(mesh),
            Some(("/Script/Engine".into(), "StaticMesh".into()))
        );
        assert_eq!(tables.imports.len(), 3, "the old package import stays");
    }

    #[test]
    fn an_import_that_is_an_exports_class_cannot_be_retargeted() {
        let mut tables = tables();
        let class = add_import(&mut tables, "/Script/Engine.StaticMesh", None).expect("add");
        let package = FLegacyPackageHeader {
            exports: vec![retoc::legacy_asset::FObjectExport {
                class_index: FPackageIndex { index: class },
                ..Default::default()
            }],
            ..Default::default()
        };
        let error = apply_import_edit(
            &mut tables,
            &package,
            &ImportEdit::Retarget {
                import: FPackageIndex { index: class }.to_import_index(),
                path: "/Script/Engine.SkeletalMesh".into(),
                class: None,
            },
        )
        .expect_err("refused");
        assert!(error.contains("class"), "{error}");
    }
}
