//! Adding a component to a Blueprint by duplicating one its construction script already builds.
//!
//! A component is a node in the class's `SimpleConstructionScript` and the template object the
//! node names, which the class copies into every actor it spawns. Both are copied as one set, so
//! the new node names the new template, and the copy is then given a variable name and guid of its
//! own and hung beside the original in the script's lists. The class gains no variable for it: UE
//! builds the component anyway and only notes that the variable is missing.

use serde::{Deserialize, Serialize};

use crate::duplicate::DuplicatePlan;
use crate::edit::{DuplicateExport, EditOp, FieldSet, PackageEdits, ValueEdit, kind_of};
use crate::mappings::Mappings;
use crate::package::{ParsedExport, ParsedPackage};
use crate::value::{PropertyEntry, PropertyValue};

/// A component added by duplicating the one construction script node `node` builds, under the
/// variable name `name`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddComponent {
    pub node: u32,
    pub name: String,
}

/// Where a node hangs in the construction script's tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeParent {
    /// One of the script's root nodes.
    Root,
    /// A child of this node.
    Node(u32),
}

/// What adding the component copies and where the copy is wired in.
#[derive(Debug, Clone)]
pub struct ComponentPlan {
    pub node: u32,
    pub template: u32,
    pub script: u32,
    pub node_name: String,
    pub template_name: String,
    pub variable: String,
    pub parent: NodeParent,
    pub(crate) plans: Vec<DuplicatePlan>,
}

fn export_of(index: i32) -> Option<u32> {
    (index > 0).then(|| (index - 1) as u32)
}

fn entry<'a>(entries: &'a [PropertyEntry], name: &str) -> Option<&'a PropertyEntry> {
    entries.iter().find(|entry| entry.name == name)
}

fn object_in(entries: &[PropertyEntry], name: &str) -> Option<i32> {
    match &entry(entries, name)?.value {
        PropertyValue::Object { index, .. } => Some(*index),
        _ => None,
    }
}

fn objects_in(entries: &[PropertyEntry], name: &str) -> Vec<i32> {
    match entry(entries, name).map(|entry| &entry.value) {
        Some(PropertyValue::Array { items }) => items
            .iter()
            .filter_map(|item| match item {
                PropertyValue::Object { index, .. } => Some(*index),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn name_in<'a>(entries: &'a [PropertyEntry], name: &str) -> Option<&'a str> {
    match &entry(entries, name)?.value {
        PropertyValue::Name { value } => Some(value),
        _ => None,
    }
}

/// The construction script nodes under `script`.
fn nodes_of(parsed: &ParsedPackage, script: u32) -> Vec<&ParsedExport> {
    parsed
        .exports
        .iter()
        .filter(|export| export.class_name == "SCS_Node" && export.outer_index == script as i32 + 1)
        .collect()
}

/// Works out what duplicating the component `add.node` builds copies, refusing a node that is not
/// one, a name a component, the template naming or a variable of the class already takes, and a
/// node the script's tree does not hold.
pub fn plan_component(parsed: &ParsedPackage, add: &AddComponent) -> Result<ComponentPlan, String> {
    let node = parsed.exports.get(add.node as usize).ok_or_else(|| {
        format!(
            "this package has {} exports, so there is no export {}",
            parsed.exports.len(),
            add.node
        )
    })?;
    if node.class_name != "SCS_Node" {
        return Err(format!(
            "{} is a {}, not a construction script node: pick the SCS_Node that builds the component",
            node.object_name, node.class_name
        ));
    }
    let template = object_in(&node.properties, "ComponentTemplate")
        .and_then(export_of)
        .ok_or_else(|| {
            format!(
                "{} builds no component template of this package",
                node.object_name
            )
        })?;
    let script = export_of(node.outer_index)
        .filter(|&script| parsed.exports[script as usize].class_name == "SimpleConstructionScript")
        .ok_or_else(|| format!("{} sits in no construction script", node.object_name))?;
    let name = add.name.trim();
    let identifier = name
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !identifier {
        return Err(format!(
            "{name:?} is not a variable name: letters, digits and underscores, not starting with a digit"
        ));
    }
    let nodes = nodes_of(parsed, script);
    if nodes.iter().any(|other| {
        name_in(&other.properties, "InternalVariableName")
            .is_some_and(|taken| taken.eq_ignore_ascii_case(name))
    }) {
        return Err(format!(
            "the Blueprint already has a component named {name}"
        ));
    }
    let template_name = format!("{name}_GEN_VARIABLE");
    if parsed
        .exports
        .iter()
        .any(|export| export.object_name.eq_ignore_ascii_case(&template_name))
    {
        return Err(format!(
            "the package already holds an object named {template_name}"
        ));
    }
    let class = export_of(parsed.exports[script as usize].outer_index)
        .and_then(|class| parsed.exports.get(class as usize));
    if class
        .and_then(|class| class.struct_definition.as_ref())
        .is_some_and(|definition| {
            definition
                .properties
                .iter()
                .any(|property| property.name.eq_ignore_ascii_case(name))
        })
    {
        return Err(format!("the Blueprint already has a variable named {name}"));
    }
    let next = nodes
        .iter()
        .filter_map(|other| {
            other
                .object_name
                .strip_prefix("SCS_Node_")?
                .parse::<u32>()
                .ok()
        })
        .max()
        .map_or(0, |highest| highest + 1);
    let node_name = format!("SCS_Node_{next}");
    let wanted = add.node as i32 + 1;
    let parent = nodes
        .iter()
        .find(|other| objects_in(&other.properties, "ChildNodes").contains(&wanted))
        .map(|other| NodeParent::Node(other.index))
        .or_else(|| {
            objects_in(&parsed.exports[script as usize].properties, "RootNodes")
                .contains(&wanted)
                .then_some(NodeParent::Root)
        })
        .ok_or_else(|| {
            format!(
                "{} hangs from nothing in the construction script, so a copy would have nowhere to go",
                node.object_name
            )
        })?;
    let plans = crate::plan_duplication(
        parsed,
        &[
            DuplicateExport {
                export: add.node,
                name: node_name.clone(),
                into_level: None,
            },
            DuplicateExport {
                export: template,
                name: template_name.clone(),
                into_level: None,
            },
        ],
    )?;
    Ok(ComponentPlan {
        node: add.node,
        template,
        script,
        node_name,
        template_name,
        variable: name.to_string(),
        parent,
        plans,
    })
}

/// The copies of the node and the template in a package the copy has landed in.
fn copies<'a>(
    parsed: &'a ParsedPackage,
    plan: &ComponentPlan,
) -> Result<(&'a ParsedExport, &'a ParsedExport), String> {
    let find = |name: &str| {
        parsed
            .exports
            .iter()
            .find(|export| export.object_name == name)
            .ok_or_else(|| format!("the copy {name} is not in the package"))
    };
    Ok((find(&plan.node_name)?, find(&plan.template_name)?))
}

/// A guid for the new variable, made from the package, the name and the original's guid, so it is
/// the same each time the same component is added and differs from every other.
fn variable_guid(parsed: &ParsedPackage, plan: &ComponentPlan, original: &str) -> String {
    let fnv = |seed: u64| {
        let text = format!(
            "{}\u{1}{}\u{1}{original}",
            parsed.info.package_name, plan.variable
        );
        text.bytes().fold(seed, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01B3)
        })
    };
    format!(
        "{:016X}{:016X}",
        fnv(0xCBF2_9CE4_8422_2325),
        fnv(0x6C62_272E_07BB_0142)
    )
}

fn value_edit(entry: &PropertyEntry, op: EditOp) -> Result<ValueEdit, String> {
    let (offset, _) = entry
        .span
        .ok_or_else(|| format!("{} has no recorded position", entry.label()))?;
    Ok(ValueEdit {
        offset,
        expect_name: entry.name.clone(),
        expect_element: entry.element,
        expect_kind: kind_of(&entry.value),
        op,
    })
}

/// An element added at the end of the object list `entry` holds, pointing at `path`.
fn append_object(
    entry: &PropertyEntry,
    path: &str,
    values: &mut Vec<ValueEdit>,
    sets: &mut Vec<FieldSet>,
) -> Result<(), String> {
    let count = match &entry.value {
        PropertyValue::Array { items } => items.len(),
        _ => 0,
    };
    let edit = value_edit(
        entry,
        EditOp::Insert {
            index: count as u32,
            key: None,
        },
    )?;
    sets.push(FieldSet {
        offset: edit.offset,
        expect_name: edit.expect_name.clone(),
        expect_element: edit.expect_element,
        path: vec![format!("[{count}]")],
        text: path.to_string(),
    });
    values.push(edit);
    Ok(())
}

/// The edits that make the copied node a component of its own and wire it into the construction
/// script: its variable name and guid, no children, and a place in `AllNodes` and beside the
/// original, in the package the copy landed in.
pub fn component_wiring(
    parsed: &ParsedPackage,
    plan: &ComponentPlan,
) -> Result<PackageEdits, String> {
    let (node, _) = copies(parsed, plan)?;
    let mut values = Vec::new();
    let mut sets = Vec::new();
    let field = |export: &ParsedExport, name: &str| {
        entry(&export.properties, name)
            .cloned()
            .ok_or_else(|| format!("{} has no {name}", export.object_name))
    };
    values.push(value_edit(
        &field(node, "InternalVariableName")?,
        EditOp::Set {
            text: plan.variable.clone(),
        },
    )?);
    let guid = field(node, "VariableGuid")?;
    let original = match &guid.value {
        PropertyValue::Str { value } => value.clone(),
        _ => String::new(),
    };
    values.push(value_edit(
        &guid,
        EditOp::Set {
            text: variable_guid(parsed, plan, &original),
        },
    )?);
    if let Some(children) = entry(&node.properties, "ChildNodes")
        && !objects_in(&node.properties, "ChildNodes").is_empty()
    {
        values.push(value_edit(children, EditOp::Unset)?);
    }
    let script = &parsed.exports[plan.script as usize];
    append_object(
        &field(script, "AllNodes")?,
        &node.path,
        &mut values,
        &mut sets,
    )?;
    let (holder, list) = match plan.parent {
        NodeParent::Root => (script, "RootNodes"),
        NodeParent::Node(parent) => (&parsed.exports[parent as usize], "ChildNodes"),
    };
    append_object(&field(holder, list)?, &node.path, &mut values, &mut sets)?;
    Ok(PackageEdits {
        values,
        field_sets: sets,
        ..Default::default()
    })
}

/// The added component reads as one: the new node names the new template under its variable name,
/// holds no children, and is listed in `AllNodes` and beside the original.
pub fn verify_component(parsed: &ParsedPackage, plan: &ComponentPlan) -> Result<(), String> {
    let (node, template) = copies(parsed, plan)?;
    let node_index = node.index as i32 + 1;
    let problem = if name_in(&node.properties, "InternalVariableName")
        != Some(plan.variable.as_str())
    {
        Some(format!(
            "{} is not named {}",
            node.object_name, plan.variable
        ))
    } else if object_in(&node.properties, "ComponentTemplate") != Some(template.index as i32 + 1) {
        Some(format!(
            "{} does not build {}",
            node.object_name, template.object_name
        ))
    } else if !objects_in(&node.properties, "ChildNodes").is_empty() {
        Some(format!("{} kept the original's children", node.object_name))
    } else if !objects_in(&parsed.exports[plan.script as usize].properties, "AllNodes")
        .contains(&node_index)
    {
        Some(format!("{} is not in AllNodes", node.object_name))
    } else {
        let listed = match plan.parent {
            NodeParent::Root => objects_in(
                &parsed.exports[plan.script as usize].properties,
                "RootNodes",
            ),
            NodeParent::Node(parent) => {
                objects_in(&parsed.exports[parent as usize].properties, "ChildNodes")
            }
        };
        (!listed.contains(&node_index))
            .then(|| format!("{} does not hang beside the original", node.object_name))
    };
    problem.map_or(Ok(()), Err)
}

/// One entry of a component's changed property list: a property, which element of a static array
/// it is, and the struct it is looked up in, `None` meaning the component's class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedProperty {
    pub name: String,
    pub index: u32,
    pub scope: Option<String>,
}

/// How deep a struct's fields are listed inside one another.
const MAX_LISTED_DEPTH: usize = 4;

/// The entries the cooker writes to have a top-level property of a component class copied onto the
/// components a Blueprint spawns. UE reads the list scope by scope: the component's class for
/// the top level, whichever class declares the property, and a struct's own type for its fields,
/// which follow it. `None` for a property the class lacks, a container, whose entries name the
/// elements that changed, and a struct whose fields are not known.
pub fn changed_property_entries(
    class_name: &str,
    class_path: Option<&str>,
    property: &str,
    element: u32,
    mappings: Option<&Mappings>,
    synth: Option<&Mappings>,
) -> Option<Vec<ChangedProperty>> {
    let schema = class_path
        .and_then(|path| synth.and_then(|m| m.class_schema(path, None)))
        .or_else(|| synth.and_then(|m| m.class_schema(class_name, None)))
        .or_else(|| mappings.and_then(|m| m.class_schema(class_name, None)))?;
    let slot = schema
        .iter()
        .find(|slot| slot.property.name == property && slot.element == element)?;
    let mut entries = vec![ChangedProperty {
        name: property.to_string(),
        index: element,
        scope: None,
    }];
    match &slot.property.inner {
        usmap::PropertyInner::Array { .. }
        | usmap::PropertyInner::Set { .. }
        | usmap::PropertyInner::Map { .. } => return None,
        usmap::PropertyInner::Struct { name } => {
            struct_fields(name, mappings, synth, 0, &mut entries)?;
        }
        _ => {}
    }
    Some(entries)
}

/// A struct's fields as list entries scoped to it, each nested struct's fields after its own
/// entry. A container field is left out, which leaves it at the class default.
fn struct_fields(
    struct_name: &str,
    mappings: Option<&Mappings>,
    synth: Option<&Mappings>,
    depth: usize,
    entries: &mut Vec<ChangedProperty>,
) -> Option<()> {
    if depth > MAX_LISTED_DEPTH {
        return None;
    }
    let schema = synth
        .and_then(|m| m.schema(struct_name))
        .or_else(|| mappings.and_then(|m| m.schema(struct_name)))?;
    if schema.is_empty() {
        return None;
    }
    for slot in schema.iter() {
        match &slot.property.inner {
            usmap::PropertyInner::Array { .. }
            | usmap::PropertyInner::Set { .. }
            | usmap::PropertyInner::Map { .. } => continue,
            inner => {
                entries.push(ChangedProperty {
                    name: slot.property.name.clone(),
                    index: slot.element,
                    scope: Some(struct_name.to_string()),
                });
                if let usmap::PropertyInner::Struct { name } = inner {
                    struct_fields(name, mappings, synth, depth + 1, entries)?;
                }
            }
        }
    }
    Some(())
}
