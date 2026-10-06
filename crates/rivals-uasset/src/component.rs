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
/// variable name `name`, and with `with_children` the components under it too.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddComponent {
    pub node: u32,
    pub name: String,
    #[serde(default)]
    pub with_children: bool,
    /// Copy the component of this variable name that the parent Blueprint adds, rather than one of
    /// this Blueprint's own; `node` is then not read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_parent: Option<String>,
}

/// What copying a parent Blueprint's component into this one takes: the parent's node and template,
/// and where the new node goes and what it is called.
#[derive(Debug, Clone)]
pub struct InheritedComponent {
    pub source_node: u32,
    pub source_template: u32,
    pub script: u32,
    pub class: u32,
    pub node_name: String,
    pub template_name: String,
    pub variable: String,
    /// The component class, by object path.
    pub component_class: String,
    /// The component of the parent's the copy attaches to, by variable name, the class that owns it
    /// and whether that class is native.
    pub attach_to: String,
    pub attach_owner: String,
    pub attach_native: bool,
    pub socket: Option<String>,
}

/// Plans copying the component the parent Blueprint `parent` adds under the variable name `from`
/// into `child`, as a component of the child's own named `name` and attached where the original
/// is. Refused for the parent's scene root, and for a name the child or its parent already uses.
pub fn plan_inherited_component(
    child: &ParsedPackage,
    parent: &ParsedPackage,
    from: &str,
    name: &str,
) -> Result<InheritedComponent, String> {
    let name = name.trim();
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
    let script_of = |package: &ParsedPackage| {
        package
            .exports
            .iter()
            .find(|export| export.class_name == "SimpleConstructionScript")
            .map(|export| export.index)
    };
    let script = script_of(child)
        .ok_or("this Blueprint has no construction script to add a component to")?;
    let class = export_of(child.exports[script as usize].outer_index)
        .ok_or("the construction script sits in no class")?;
    let parent_script = script_of(parent).ok_or("the parent Blueprint adds no components")?;
    let parent_nodes = nodes_of(parent, parent_script);
    let source = parent_nodes
        .iter()
        .find(|node| {
            name_in(&node.properties, "InternalVariableName")
                .is_some_and(|variable| variable.eq_ignore_ascii_case(from))
        })
        .ok_or_else(|| format!("the parent Blueprint adds no component named {from}"))?;
    let source_template = object_in(&source.properties, "ComponentTemplate")
        .and_then(export_of)
        .ok_or_else(|| format!("{from} builds no template in the parent's package"))?;
    let template = &parent.exports[source_template as usize];
    let component_class = parent
        .imports
        .iter()
        .find(|import| import.index == template.class_index)
        .map(|import| import.path.clone())
        .or_else(|| {
            export_of(template.class_index)
                .and_then(|class| parent.exports.get(class as usize))
                .map(|class| class.path.clone())
        })
        .ok_or_else(|| format!("{from}'s template has no class"))?;

    // Where the original hangs in the parent: under another of its components, or at the root
    // under the scene root, or, where it attaches to a component the parent inherits, there.
    let wanted = source.index as i32 + 1;
    let parent_class = export_of(parent.exports[parent_script as usize].outer_index)
        .and_then(|class| parent.exports.get(class as usize))
        .map(|class| class.object_name.clone())
        .unwrap_or_default();
    let variable_of = |at: i32| {
        export_of(at)
            .and_then(|node| parent.exports.get(node as usize))
            .and_then(|node| name_in(&node.properties, "InternalVariableName"))
            .map(str::to_string)
    };
    let script_export = &parent.exports[parent_script as usize];
    let scene_root = object_in(&script_export.properties, "DefaultSceneRootNode").or_else(|| {
        objects_in(&script_export.properties, "RootNodes")
            .first()
            .copied()
    });
    let (attach_to, attach_owner, attach_native) = if let Some(holder) = parent_nodes
        .iter()
        .find(|node| objects_in(&node.properties, "ChildNodes").contains(&wanted))
    {
        (
            variable_of(holder.index as i32 + 1).unwrap_or_default(),
            parent_class.clone(),
            false,
        )
    } else if let Some(inherited) = name_in(&source.properties, "ParentComponentOrVariableName")
        .filter(|inherited| !inherited.is_empty() && *inherited != "None")
    {
        (
            inherited.to_string(),
            name_in(&source.properties, "ParentComponentOwnerClassName")
                .unwrap_or_default()
                .to_string(),
            matches!(
                entry(&source.properties, "bIsParentComponentNative").map(|entry| &entry.value),
                Some(PropertyValue::Bool { value: true })
            ),
        )
    } else if scene_root == Some(wanted) {
        return Err(format!(
            "{from} is the parent's scene root, which every other component hangs from"
        ));
    } else {
        (
            scene_root.and_then(variable_of).unwrap_or_default(),
            parent_class.clone(),
            false,
        )
    };
    if attach_to.is_empty() {
        return Err(format!("where {from} hangs in the parent is not known"));
    }

    let taken = |package: &ParsedPackage, script: u32| {
        nodes_of(package, script).iter().any(|node| {
            name_in(&node.properties, "InternalVariableName")
                .is_some_and(|variable| variable.eq_ignore_ascii_case(name))
        }) || package.exports.iter().any(|export| {
            export.struct_definition.as_ref().is_some_and(|definition| {
                definition
                    .properties
                    .iter()
                    .any(|property| property.name.eq_ignore_ascii_case(name))
            })
        })
    };
    if taken(child, script) || taken(parent, parent_script) {
        return Err(format!(
            "the Blueprint or its parent already has a component or variable named {name}"
        ));
    }
    let template_name = format!("{name}_GEN_VARIABLE");
    if child
        .exports
        .iter()
        .any(|export| export.object_name.eq_ignore_ascii_case(&template_name))
    {
        return Err(format!(
            "the package already holds an object named {template_name}"
        ));
    }
    let next = nodes_of(child, script)
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
    Ok(InheritedComponent {
        source_node: source.index,
        source_template,
        script,
        class,
        node_name: format!("SCS_Node_{next}"),
        template_name,
        variable: name.to_string(),
        component_class,
        attach_to,
        attach_owner,
        attach_native,
        socket: name_in(&source.properties, "AttachToName")
            .filter(|socket| !socket.is_empty() && *socket != "None")
            .map(str::to_string),
    })
}

/// The edits that make the new node, added empty under the child's construction script, build the
/// copied template as a component of the child's own, attached where the original is, and hang it
/// among the script's roots. The node's changed property list is filled on its own after these.
pub fn inherited_component_wiring(
    parsed: &ParsedPackage,
    plan: &InheritedComponent,
) -> Result<PackageEdits, String> {
    let node = copy_named(parsed, &plan.node_name)?;
    let template = copy_named(parsed, &plan.template_name)?;
    let field = |export: &ParsedExport, name: &str| {
        entry(&export.properties, name)
            .cloned()
            .ok_or_else(|| format!("{} has no {name}", export.object_name))
    };
    let mut values = Vec::new();
    let mut sets = Vec::new();
    let mut set = |name: &str, text: String| -> Result<(), String> {
        values.push(value_edit(&field(node, name)?, EditOp::Set { text })?);
        Ok(())
    };
    set("ComponentClass", plan.component_class.clone())?;
    set("ComponentTemplate", template.path.clone())?;
    set("InternalVariableName", plan.variable.clone())?;
    set(
        "VariableGuid",
        variable_guid(parsed, &plan.variable, &plan.template_name),
    )?;
    set("ParentComponentOrVariableName", plan.attach_to.clone())?;
    set("ParentComponentOwnerClassName", plan.attach_owner.clone())?;
    if plan.attach_native {
        set("bIsParentComponentNative", "true".into())?;
    }
    if let Some(socket) = &plan.socket {
        set("AttachToName", socket.clone())?;
    }
    let data = field(node, "CookedComponentInstancingData")?;
    let (offset, _) = data
        .span
        .ok_or("the node's instancing data has no recorded position")?;
    sets.push(FieldSet {
        offset,
        expect_name: data.name.clone(),
        expect_element: data.element,
        path: vec!["bHasValidCookedData".into()],
        text: "true".into(),
    });
    let script = &parsed.exports[plan.script as usize];
    append_object(
        &field(script, "AllNodes")?,
        &node.path,
        &mut values,
        &mut sets,
    )?;
    append_object(
        &field(script, "RootNodes")?,
        &node.path,
        &mut values,
        &mut sets,
    )?;
    Ok(PackageEdits {
        values,
        field_sets: sets,
        ..Default::default()
    })
}

/// The copied component reads as one of the child's own: its node builds the copied template under
/// its variable name, attached where the original is, among the script's roots.
pub fn verify_inherited_component(
    parsed: &ParsedPackage,
    plan: &InheritedComponent,
) -> Result<(), String> {
    let node = copy_named(parsed, &plan.node_name)?;
    let template = copy_named(parsed, &plan.template_name)?;
    let index = node.index as i32 + 1;
    let script = &parsed.exports[plan.script as usize];
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
    } else if name_in(&node.properties, "ParentComponentOrVariableName")
        != Some(plan.attach_to.as_str())
    {
        Some(format!(
            "{} does not attach to {}",
            node.object_name, plan.attach_to
        ))
    } else if !objects_in(&script.properties, "RootNodes").contains(&index) {
        Some(format!(
            "{} is not among the script's roots",
            node.object_name
        ))
    } else if !objects_in(&script.properties, "AllNodes").contains(&index) {
        Some(format!("{} is not in AllNodes", node.object_name))
    } else {
        None
    };
    problem.map_or(Ok(()), Err)
}

/// A component under the duplicated one, copied with it.
#[derive(Debug, Clone)]
pub struct CopiedNode {
    pub node: u32,
    pub template: u32,
    pub node_name: String,
    pub template_name: String,
    pub variable: String,
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
    /// The components under the duplicated one, copied with it, depth first.
    pub descendants: Vec<CopiedNode>,
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
    let class_variables: Vec<String> = class
        .and_then(|class| class.struct_definition.as_ref())
        .map(|definition| {
            definition
                .properties
                .iter()
                .map(|property| property.name.to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_default();
    if class_variables.contains(&name.to_ascii_lowercase()) {
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
    // The components under it, each given the next free name after its own, as UE's editor names a
    // duplicate: `StaticMesh1`, `StaticMesh2`.
    let mut taken: Vec<String> = nodes
        .iter()
        .filter_map(|other| name_in(&other.properties, "InternalVariableName"))
        .map(str::to_ascii_lowercase)
        .chain(class_variables)
        .collect();
    taken.push(name.to_ascii_lowercase());
    let objects: Vec<String> = parsed
        .exports
        .iter()
        .map(|export| export.object_name.to_ascii_lowercase())
        .collect();
    let mut descendants = Vec::new();
    if add.with_children {
        for (offset, child) in subtree(parsed, add.node).into_iter().enumerate() {
            let export = &parsed.exports[child as usize];
            let child_template = object_in(&export.properties, "ComponentTemplate")
                .and_then(export_of)
                .ok_or_else(|| {
                    format!(
                        "{} builds no component template of this package",
                        export.object_name
                    )
                })?;
            let own = name_in(&export.properties, "InternalVariableName").unwrap_or("Component");
            let base = own.trim_end_matches(|c: char| c.is_ascii_digit());
            let base = if base.is_empty() { own } else { base };
            let variable = (1u32..)
                .map(|n| format!("{base}{n}"))
                .find(|candidate| {
                    let lower = candidate.to_ascii_lowercase();
                    !taken.contains(&lower) && !objects.contains(&format!("{lower}_gen_variable"))
                })
                .unwrap_or_else(|| format!("{base}Copy"));
            taken.push(variable.to_ascii_lowercase());
            descendants.push(CopiedNode {
                node: child,
                template: child_template,
                node_name: format!("SCS_Node_{}", next + 1 + offset as u32),
                template_name: format!("{variable}_GEN_VARIABLE"),
                variable,
            });
        }
    }
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
    let mut roots = vec![
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
    ];
    for copied in &descendants {
        roots.push(DuplicateExport {
            export: copied.node,
            name: copied.node_name.clone(),
            into_level: None,
        });
        roots.push(DuplicateExport {
            export: copied.template,
            name: copied.template_name.clone(),
            into_level: None,
        });
    }
    let plans = crate::plan_duplication(parsed, &roots)?;
    Ok(ComponentPlan {
        node: add.node,
        template,
        script,
        node_name,
        template_name,
        variable: name.to_string(),
        parent,
        descendants,
        plans,
    })
}

/// The copy of the object named `name` in a package the copy has landed in.
fn copy_named<'a>(parsed: &'a ParsedPackage, name: &str) -> Result<&'a ParsedExport, String> {
    parsed
        .exports
        .iter()
        .find(|export| export.object_name == name)
        .ok_or_else(|| format!("the copy {name} is not in the package"))
}

/// The copies of the node and the template in a package the copy has landed in.
fn copies<'a>(
    parsed: &'a ParsedPackage,
    plan: &ComponentPlan,
) -> Result<(&'a ParsedExport, &'a ParsedExport), String> {
    Ok((
        copy_named(parsed, &plan.node_name)?,
        copy_named(parsed, &plan.template_name)?,
    ))
}

/// A guid for a new variable, made from the package, the name and the original's guid, so it is
/// the same each time the same component is added and differs from every other.
fn variable_guid(parsed: &ParsedPackage, variable: &str, original: &str) -> String {
    derived_guid(&[&parsed.info.package_name, variable, original])
}

/// A guid made from `parts`, as 32 uppercase hex digits: the same each time the same parts make
/// it, so a save that writes one writes the same bytes again, and different for any others.
pub fn derived_guid(parts: &[&str]) -> String {
    let text = parts.join("\u{1}");
    let fnv = |seed: u64| {
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
    let mut name_it = |copy: &ParsedExport, variable: &str| -> Result<(), String> {
        values.push(value_edit(
            &field(copy, "InternalVariableName")?,
            EditOp::Set {
                text: variable.to_string(),
            },
        )?);
        let guid = field(copy, "VariableGuid")?;
        let original = match &guid.value {
            PropertyValue::Str { value } => value.clone(),
            _ => String::new(),
        };
        values.push(value_edit(
            &guid,
            EditOp::Set {
                text: variable_guid(parsed, variable, &original),
            },
        )?);
        Ok(())
    };
    name_it(node, &plan.variable)?;
    let mut copied = vec![node];
    for descendant in &plan.descendants {
        let copy = copy_named(parsed, &descendant.node_name)?;
        name_it(copy, &descendant.variable)?;
        copied.push(copy);
    }
    if plan.descendants.is_empty()
        && let Some(children) = entry(&node.properties, "ChildNodes")
        && !objects_in(&node.properties, "ChildNodes").is_empty()
    {
        values.push(value_edit(children, EditOp::Unset)?);
    }
    let script = &parsed.exports[plan.script as usize];
    let all = field(script, "AllNodes")?;
    let count = objects_in(&script.properties, "AllNodes").len();
    for (offset, copy) in copied.iter().enumerate() {
        let edit = value_edit(
            &all,
            EditOp::Insert {
                index: (count + offset) as u32,
                key: None,
            },
        )?;
        sets.push(FieldSet {
            offset: edit.offset,
            expect_name: edit.expect_name.clone(),
            expect_element: edit.expect_element,
            path: vec![format!("[{}]", count + offset)],
            text: copy.path.clone(),
        });
        values.push(edit);
    }
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

/// What is wrong with the copies of the components under the duplicated one, if anything: each is
/// named, builds its own template, is in `AllNodes`, and hangs under the copy of its parent.
fn descendant_problem(parsed: &ParsedPackage, plan: &ComponentPlan) -> Option<String> {
    let all = objects_in(&parsed.exports[plan.script as usize].properties, "AllNodes");
    for descendant in &plan.descendants {
        let (Ok(node), Ok(template)) = (
            copy_named(parsed, &descendant.node_name),
            copy_named(parsed, &descendant.template_name),
        ) else {
            return Some(format!(
                "the copy of {} is not in the package",
                descendant.variable
            ));
        };
        let index = node.index as i32 + 1;
        if name_in(&node.properties, "InternalVariableName") != Some(descendant.variable.as_str()) {
            return Some(format!(
                "{} is not named {}",
                node.object_name, descendant.variable
            ));
        }
        if object_in(&node.properties, "ComponentTemplate") != Some(template.index as i32 + 1) {
            return Some(format!(
                "{} does not build {}",
                node.object_name, template.object_name
            ));
        }
        if !all.contains(&index) {
            return Some(format!("{} is not in AllNodes", node.object_name));
        }
        let hung = parsed
            .exports
            .iter()
            .any(|other| objects_in(&other.properties, "ChildNodes").contains(&index));
        if !hung {
            return Some(format!("{} hangs from nothing", node.object_name));
        }
    }
    None
}

/// A component taken out of a Blueprint: the construction script node that builds it, and whether
/// the components hanging under it go with it or take its place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveComponent {
    pub node: u32,
    #[serde(default)]
    pub with_children: bool,
}

/// What removing a component takes out and where the components under it go.
#[derive(Debug, Clone, Serialize)]
pub struct ComponentRemoval {
    /// The nodes that go, the one asked for first, then its subtree when it goes too.
    pub nodes: Vec<u32>,
    /// The templates those nodes build.
    pub templates: Vec<u32>,
    #[serde(skip)]
    pub script: u32,
    #[serde(skip)]
    pub parent: NodeParent,
    /// The nodes that hang where the removed one did, by package index.
    #[serde(skip)]
    pub children: Vec<i32>,
    /// The variable names of the components that go.
    pub variables: Vec<String>,
    /// What the removal leaves that still names them.
    pub warnings: Vec<String>,
}

/// The nodes under `node`, depth first, not counting `node` itself.
fn subtree(parsed: &ParsedPackage, node: u32) -> Vec<u32> {
    let mut out = Vec::new();
    let mut stack = vec![node];
    while let Some(at) = stack.pop() {
        for child in objects_in(&parsed.exports[at as usize].properties, "ChildNodes")
            .into_iter()
            .filter_map(export_of)
        {
            if child != node && !out.contains(&child) && (child as usize) < parsed.exports.len() {
                out.push(child);
                stack.push(child);
            }
        }
    }
    out
}

/// Works out what removing the component `remove.node` builds takes out, refusing a node that is
/// not one, and the script's scene root while components still hang under it: the actor's root
/// would change under them.
pub fn plan_component_removal(
    parsed: &ParsedPackage,
    remove: &RemoveComponent,
) -> Result<ComponentRemoval, String> {
    let node = parsed.exports.get(remove.node as usize).ok_or_else(|| {
        format!(
            "this package has {} exports, so there is no export {}",
            parsed.exports.len(),
            remove.node
        )
    })?;
    if node.class_name != "SCS_Node" {
        return Err(format!(
            "{} is a {}, not a construction script node: pick the SCS_Node that builds the component",
            node.object_name, node.class_name
        ));
    }
    let script = export_of(node.outer_index)
        .filter(|&script| parsed.exports[script as usize].class_name == "SimpleConstructionScript")
        .ok_or_else(|| format!("{} sits in no construction script", node.object_name))?;
    let script_export = &parsed.exports[script as usize];
    let wanted = remove.node as i32 + 1;
    let nodes = nodes_of(parsed, script);
    let parent = nodes
        .iter()
        .find(|other| objects_in(&other.properties, "ChildNodes").contains(&wanted))
        .map(|other| NodeParent::Node(other.index))
        .or_else(|| {
            objects_in(&script_export.properties, "RootNodes")
                .contains(&wanted)
                .then_some(NodeParent::Root)
        })
        .ok_or_else(|| {
            format!(
                "{} hangs from nothing in the construction script",
                node.object_name
            )
        })?;
    let children = objects_in(&node.properties, "ChildNodes");
    let scene_root = object_in(&script_export.properties, "DefaultSceneRootNode") == Some(wanted)
        || objects_in(&script_export.properties, "RootNodes").first() == Some(&wanted);
    if scene_root && !children.is_empty() && !remove.with_children {
        return Err(format!(
            "{} is the scene root the other components hang from, so it goes only with them",
            node.object_name
        ));
    }
    let mut removed = vec![remove.node];
    if remove.with_children {
        removed.extend(subtree(parsed, remove.node));
    }
    let mut templates = Vec::new();
    let mut variables = Vec::new();
    for &at in &removed {
        let export = &parsed.exports[at as usize];
        if let Some(template) =
            object_in(&export.properties, "ComponentTemplate").and_then(export_of)
        {
            templates.push(template);
        }
        if let Some(variable) = name_in(&export.properties, "InternalVariableName") {
            variables.push(variable.to_string());
        }
    }
    let mut warnings = Vec::new();
    for export in &parsed.exports {
        let Some(script) = &export.script else {
            continue;
        };
        let text = crate::kismet::render_script(script);
        for variable in &variables {
            let named = text.match_indices(variable.as_str()).any(|(at, _)| {
                let before = text[..at].chars().next_back();
                let after = text[at + variable.len()..].chars().next();
                let part = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
                !part(before) && !part(after)
            });
            if named {
                warnings.push(format!(
                    "{} reads {variable}, which is None once the component is gone",
                    export.object_name
                ));
            }
        }
    }
    warnings
        .push("actors already placed in a map keep the component they were saved with".to_string());
    Ok(ComponentRemoval {
        nodes: removed,
        templates,
        script,
        parent,
        children: if remove.with_children {
            Vec::new()
        } else {
            children
        },
        variables,
        warnings,
    })
}

/// The edits that unhook the removed nodes from the construction script before they go: out of
/// `AllNodes` and the list they hang in, with the nodes under the removed one taking its place
/// there unless they go too.
pub fn component_removal_wiring(
    parsed: &ParsedPackage,
    plan: &ComponentRemoval,
) -> Result<PackageEdits, String> {
    let script = &parsed.exports[plan.script as usize];
    let field = |export: &ParsedExport, name: &str| {
        entry(&export.properties, name)
            .cloned()
            .ok_or_else(|| format!("{} has no {name}", export.object_name))
    };
    let mut values = Vec::new();
    let mut sets = Vec::new();
    let gone: Vec<i32> = plan.nodes.iter().map(|node| *node as i32 + 1).collect();
    let all = field(script, "AllNodes")?;
    for (index, item) in objects_in(&script.properties, "AllNodes")
        .iter()
        .enumerate()
    {
        if gone.contains(item) {
            values.push(value_edit(
                &all,
                EditOp::Remove {
                    index: index as u32,
                },
            )?);
        }
    }
    let (holder, list) = match plan.parent {
        NodeParent::Root => (script, "RootNodes"),
        NodeParent::Node(parent) => (&parsed.exports[parent as usize], "ChildNodes"),
    };
    let hung = field(holder, list)?;
    let listed = objects_in(&holder.properties, list);
    let at = listed
        .iter()
        .position(|item| *item == gone[0])
        .ok_or("the component is not in the list it hangs in")?;
    values.push(value_edit(&hung, EditOp::Remove { index: at as u32 })?);
    for (offset, child) in plan.children.iter().enumerate() {
        let path = export_of(*child)
            .and_then(|child| parsed.exports.get(child as usize))
            .map(|child| child.path.clone())
            .ok_or("a child node is not in the package")?;
        let edit = value_edit(
            &hung,
            EditOp::Insert {
                index: (at + offset) as u32,
                key: None,
            },
        )?;
        sets.push(FieldSet {
            offset: edit.offset,
            expect_name: edit.expect_name.clone(),
            expect_element: edit.expect_element,
            path: vec![format!("[{}]", at + offset)],
            text: path,
        });
        values.push(edit);
    }
    if object_in(&script.properties, "DefaultSceneRootNode") == Some(gone[0]) {
        values.push(value_edit(
            &field(script, "DefaultSceneRootNode")?,
            EditOp::Set {
                text: "None".into(),
            },
        )?);
    }
    Ok(PackageEdits {
        values,
        field_sets: sets,
        ..Default::default()
    })
}

/// The removed component is gone: its nodes and templates are no longer in the package, and the
/// nodes that hung under it hang where it did.
pub fn verify_component_removal(
    before: &ParsedPackage,
    after: &ParsedPackage,
    plan: &ComponentRemoval,
) -> Result<(), String> {
    let path_of = |at: u32| before.exports[at as usize].path.clone();
    for gone in plan.nodes.iter().chain(&plan.templates) {
        let path = path_of(*gone);
        if after.exports.iter().any(|export| export.path == path) {
            return Err(format!("{path} is still in the package"));
        }
    }
    let script_path = before.exports[plan.script as usize].path.clone();
    let script = after
        .exports
        .iter()
        .find(|export| export.path == script_path)
        .ok_or("the construction script is gone")?;
    let holder = match plan.parent {
        NodeParent::Root => script,
        NodeParent::Node(parent) => {
            let path = path_of(parent);
            after
                .exports
                .iter()
                .find(|export| export.path == path)
                .ok_or("the node the component hung from is gone")?
        }
    };
    let list = match plan.parent {
        NodeParent::Root => "RootNodes",
        NodeParent::Node(_) => "ChildNodes",
    };
    let hung: Vec<String> = objects_in(&holder.properties, list)
        .into_iter()
        .filter_map(export_of)
        .filter_map(|at| after.exports.get(at as usize))
        .map(|export| export.path.clone())
        .collect();
    for child in plan.children.iter().filter_map(|child| export_of(*child)) {
        let path = path_of(child);
        if !hung.contains(&path) {
            return Err(format!(
                "{path} does not hang where the removed component did"
            ));
        }
    }
    Ok(())
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
    } else if plan.descendants.is_empty() && !objects_in(&node.properties, "ChildNodes").is_empty()
    {
        Some(format!("{} kept the original's children", node.object_name))
    } else if !objects_in(&parsed.exports[plan.script as usize].properties, "AllNodes")
        .contains(&node_index)
    {
        Some(format!("{} is not in AllNodes", node.object_name))
    } else if let Some(problem) = descendant_problem(parsed, plan) {
        Some(problem)
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

/// How deep a struct's fields are listed inside one another.
const MAX_LISTED_DEPTH: usize = 4;

/// A field path segment as the field and its element: `Name`, or `Name[2]` for a static array.
fn segment(text: &str) -> (&str, u32) {
    match text.strip_suffix(']').and_then(|head| head.split_once('[')) {
        Some((name, element)) if !name.is_empty() => (name, element.parse().unwrap_or(0)),
        _ => (text, 0),
    }
}

/// Where a changed property list entry is looked up: the component's class, a struct by name, or,
/// for an entry read from a package, the object path it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListScope {
    Class,
    Struct(String),
    Path(String),
}

/// One entry of a component's changed property list: a property, which element of a static array
/// or of an array it is, and the class or struct it is looked up in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListEntry {
    pub name: String,
    pub index: u32,
    pub scope: ListScope,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ListNode {
    entry: ListEntry,
    /// A struct's fields, or an array's elements, as the list names them after the entry.
    children: Vec<ListNode>,
}

/// The class or struct entries are read against.
#[derive(Debug, Clone)]
enum Level {
    Class,
    Struct(String),
}

impl Level {
    fn scope(&self) -> ListScope {
        match self {
            Level::Class => ListScope::Class,
            Level::Struct(name) => ListScope::Struct(name.clone()),
        }
    }
}

/// What reading and extending a component's list needs: its class, by name and by path, and the
/// layouts of the class and of the structs it holds.
pub struct ListContext<'a> {
    pub class_name: &'a str,
    pub class_path: &'a str,
    pub mappings: Option<&'a Mappings>,
    pub synth: Option<&'a Mappings>,
}

impl ListContext<'_> {
    fn schema(&self, level: &Level) -> Option<crate::mappings::Schema<'_>> {
        match level {
            Level::Class => self
                .synth
                .and_then(|m| m.class_schema(self.class_path, None))
                .or_else(|| {
                    self.synth
                        .and_then(|m| m.class_schema(self.class_name, None))
                })
                .or_else(|| {
                    self.mappings
                        .and_then(|m| m.class_schema(self.class_name, None))
                }),
            Level::Struct(name) => self
                .synth
                .and_then(|m| m.schema(name))
                .or_else(|| self.mappings.and_then(|m| m.schema(name))),
        }
    }

    fn matches(&self, scope: &ListScope, level: &Level) -> bool {
        match (scope, level) {
            (ListScope::Class, Level::Class) => true,
            (ListScope::Struct(a), Level::Struct(b)) => a.eq_ignore_ascii_case(b),
            (ListScope::Path(path), Level::Class) => path.eq_ignore_ascii_case(self.class_path),
            (ListScope::Path(path), Level::Struct(name)) => path
                .rsplit(['.', ':'])
                .next()
                .unwrap_or(path)
                .eq_ignore_ascii_case(name),
            _ => false,
        }
    }
}

/// A component's changed property list as UE reads it: top-level entries in the component class's
/// scope, a struct's fields after it in the struct's scope, and an array's elements after it,
/// named after the array in its owner's scope, up to a `None` that closes them. Entries it cannot
/// place are kept as they stand, so an untouched list flattens back to what it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedList {
    nodes: Vec<ListNode>,
    rest: Vec<ListEntry>,
}

const LIST_NONE: &str = "None";

impl ChangedList {
    pub fn read(entries: &[ListEntry], ctx: &ListContext<'_>) -> Self {
        let mut at = 0;
        let nodes = read_scope(entries, &mut at, &Level::Class, ctx, 0);
        Self {
            nodes,
            rest: entries[at..].to_vec(),
        }
    }

    /// Entries left over past where UE stops reading the list, which nothing places.
    pub fn unplaced(&self) -> usize {
        self.rest.len()
    }

    pub fn entries(&self) -> Vec<ListEntry> {
        let mut out = Vec::new();
        flatten(&self.nodes, &mut out);
        out.extend(self.rest.iter().cloned());
        out
    }

    /// Lists what a save changed of the top-level property `property` (element `element` of a
    /// static array), as the template now holds it: the property, the fields of a struct `fields`
    /// names below it (`None` for all of them), and every element of an array. `None` for a
    /// property the class lacks or a struct whose fields are not known.
    pub fn touch(
        &mut self,
        property: &str,
        element: u32,
        fields: Option<&[Vec<String>]>,
        value: Option<&PropertyValue>,
        ctx: &ListContext<'_>,
    ) -> Option<()> {
        touch_in(
            &mut self.nodes,
            &Level::Class,
            property,
            element,
            fields,
            value,
            ctx,
            0,
        )
    }
}

fn flatten(nodes: &[ListNode], out: &mut Vec<ListEntry>) {
    for node in nodes {
        out.push(node.entry.clone());
        flatten(&node.children, out);
    }
}

fn read_scope(
    entries: &[ListEntry],
    at: &mut usize,
    level: &Level,
    ctx: &ListContext<'_>,
    depth: usize,
) -> Vec<ListNode> {
    let schema = ctx.schema(level);
    let mut nodes = Vec::new();
    while let Some(entry) = entries
        .get(*at)
        .filter(|entry| ctx.matches(&entry.scope, level))
    {
        let entry = entry.clone();
        *at += 1;
        let inner = schema.as_ref().and_then(|schema| {
            schema
                .iter()
                .find(|slot| {
                    slot.property.name.eq_ignore_ascii_case(&entry.name)
                        && slot.element == entry.index
                })
                .map(|slot| slot.property.inner.clone())
        });
        let children = match inner {
            Some(usmap::PropertyInner::Struct { name }) if depth < MAX_LISTED_DEPTH => {
                read_scope(entries, at, &Level::Struct(name), ctx, depth + 1)
            }
            Some(usmap::PropertyInner::Array { inner }) => {
                read_elements(entries, at, level, &entry.name, &inner, ctx, depth)
            }
            _ => Vec::new(),
        };
        nodes.push(ListNode { entry, children });
    }
    nodes
}

fn read_elements(
    entries: &[ListEntry],
    at: &mut usize,
    owner: &Level,
    array: &str,
    inner: &usmap::PropertyInner,
    ctx: &ListContext<'_>,
    depth: usize,
) -> Vec<ListNode> {
    let mut nodes = Vec::new();
    while let Some(entry) = entries.get(*at).filter(|entry| {
        ctx.matches(&entry.scope, owner)
            && (entry.name.eq_ignore_ascii_case(array) || entry.name == LIST_NONE)
    }) {
        let entry = entry.clone();
        *at += 1;
        if entry.name == LIST_NONE {
            nodes.push(ListNode {
                entry,
                children: Vec::new(),
            });
            break;
        }
        let children = match inner {
            usmap::PropertyInner::Struct { name } if depth < MAX_LISTED_DEPTH => {
                read_scope(entries, at, &Level::Struct(name.clone()), ctx, depth + 1)
            }
            _ => Vec::new(),
        };
        nodes.push(ListNode { entry, children });
    }
    nodes
}

#[allow(clippy::too_many_arguments)]
fn touch_in(
    nodes: &mut Vec<ListNode>,
    level: &Level,
    property: &str,
    element: u32,
    fields: Option<&[Vec<String>]>,
    value: Option<&PropertyValue>,
    ctx: &ListContext<'_>,
    depth: usize,
) -> Option<()> {
    if depth > MAX_LISTED_DEPTH {
        return None;
    }
    // A value the template inherits is the one its spawned components start with.
    if inherited(value) {
        return Some(());
    }
    let inner = ctx
        .schema(level)?
        .iter()
        .find(|slot| slot.property.name == property && slot.element == element)?
        .property
        .inner
        .clone();
    let at = match nodes.iter().position(|node| {
        node.entry.name.eq_ignore_ascii_case(property) && node.entry.index == element
    }) {
        Some(at) => at,
        None => {
            nodes.push(ListNode {
                entry: ListEntry {
                    name: property.to_string(),
                    index: element,
                    scope: level.scope(),
                },
                children: Vec::new(),
            });
            nodes.len() - 1
        }
    };
    match &inner {
        usmap::PropertyInner::Struct { name } => {
            let struct_level = Level::Struct(name.clone());
            match fields {
                None => {
                    nodes[at].children = full_listing(name, value, ctx, depth + 1)?;
                }
                Some(paths) => {
                    for path in paths {
                        let Some((first, below)) = path.split_first() else {
                            continue;
                        };
                        let (field, field_element) = segment(first);
                        let below = (!below.is_empty()).then(|| vec![below.to_vec()]);
                        touch_in(
                            &mut nodes[at].children,
                            &struct_level,
                            field,
                            field_element,
                            below.as_deref(),
                            struct_field(value, field, field_element),
                            ctx,
                            depth + 1,
                        )?;
                    }
                }
            }
        }
        usmap::PropertyInner::Array { inner } => {
            nodes[at].children = element_listing(level, property, inner, value, ctx, depth)?;
        }
        usmap::PropertyInner::Set { .. } | usmap::PropertyInner::Map { .. } => {
            nodes[at].children.clear();
        }
        _ => {}
    }
    Some(())
}

/// A struct the reader decodes into a value of its own kind, as the fields the list names: a tag
/// container is read as its tag names, and a tag as its name.
fn natively_read(name: &str, value: Option<&PropertyValue>) -> Option<PropertyValue> {
    let tag = |value: &PropertyValue| PropertyValue::Struct {
        name: "GameplayTag".into(),
        fields: vec![PropertyEntry {
            name: "TagName".into(),
            element: None,
            value: value.clone(),
            span: None,
            slot: None,
        }],
    };
    match (name, value?) {
        ("GameplayTagContainer", PropertyValue::Array { items }) => Some(PropertyValue::Struct {
            name: name.into(),
            fields: vec![PropertyEntry {
                name: "GameplayTags".into(),
                element: None,
                value: PropertyValue::Array {
                    items: items.iter().map(tag).collect(),
                },
                span: None,
                slot: None,
            }],
        }),
        ("GameplayTag", value @ PropertyValue::Name { .. }) => Some(tag(value)),
        _ => None,
    }
}

/// Whether a value is one the object inherits rather than holds.
fn inherited(value: Option<&PropertyValue>) -> bool {
    matches!(value, None | Some(PropertyValue::Unset { .. }))
}

/// A field of a struct value, where it holds one.
fn struct_field<'v>(
    value: Option<&'v PropertyValue>,
    field: &str,
    element: u32,
) -> Option<&'v PropertyValue> {
    let fields = match value? {
        PropertyValue::Struct { fields, .. } | PropertyValue::Default { fields, .. } => fields,
        _ => return None,
    };
    fields
        .iter()
        .find(|entry| entry.name == field && entry.element.unwrap_or(0) == element)
        .map(|entry| &entry.value)
}

/// Every field of the struct `name` that `value` holds, each nested struct's fields after its entry
/// and each array's elements after its own. A field it inherits is left to the class default.
fn full_listing(
    name: &str,
    value: Option<&PropertyValue>,
    ctx: &ListContext<'_>,
    depth: usize,
) -> Option<Vec<ListNode>> {
    if depth > MAX_LISTED_DEPTH {
        return None;
    }
    let level = Level::Struct(name.to_string());
    let schema = ctx.schema(&level)?;
    if schema.is_empty() {
        return None;
    }
    let native = natively_read(name, value);
    let value = native.as_ref().or(value);
    let mut nodes = Vec::new();
    for slot in schema.iter() {
        let field_value = struct_field(value, &slot.property.name, slot.element);
        if inherited(field_value) && !matches!(value, Some(PropertyValue::Default { .. })) {
            continue;
        }
        let children = match &slot.property.inner {
            usmap::PropertyInner::Struct { name } => {
                full_listing(name, field_value, ctx, depth + 1)?
            }
            usmap::PropertyInner::Array { inner } => {
                element_listing(&level, &slot.property.name, inner, field_value, ctx, depth)?
            }
            _ => Vec::new(),
        };
        nodes.push(ListNode {
            entry: ListEntry {
                name: slot.property.name.clone(),
                index: slot.element,
                scope: level.scope(),
            },
            children,
        });
    }
    Some(nodes)
}

/// Every element an array holds, named after the array in its owner's scope, each struct element's
/// fields after it. Listing each one copies all of them whatever the class default holds.
fn element_listing(
    owner: &Level,
    array: &str,
    inner: &usmap::PropertyInner,
    value: Option<&PropertyValue>,
    ctx: &ListContext<'_>,
    depth: usize,
) -> Option<Vec<ListNode>> {
    let items: &[PropertyValue] = match value {
        Some(PropertyValue::Array { items }) => items,
        _ => &[],
    };
    let mut nodes = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let children = match inner {
            usmap::PropertyInner::Struct { name } => {
                full_listing(name, Some(item), ctx, depth + 1)?
            }
            _ => Vec::new(),
        };
        nodes.push(ListNode {
            entry: ListEntry {
                name: array.to_string(),
                index: index as u32,
                scope: owner.scope(),
            },
            children,
        });
    }
    Some(nodes)
}
