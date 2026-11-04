//! Lists where a Niagara package's colours come from (curve LUTs, colour arrays, parameters, the values its scripts bake and its renderers' materials) and plans recolouring them.

use std::sync::Arc;

use rivals_uasset::{
    ColorTransform, Mappings, ParsedExport, ParsedPackage, PathEdit, PathOp, PropertyEntry,
    PropertyValue, VmColorTransform, VmLeaf, VmScript, VmTrace, VmType, lower_vm_edits,
    read_vm_script, show_bits, store_params, trace_outputs, was_of,
};
use serde::{Deserialize, Serialize};

use crate::inherit::ArchetypeSource;

/// Every colour source in a Niagara package, in export order.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ColorReport {
    pub sources: Vec<ColorSource>,
    /// The scripts compiled for the GPU, whose own colours are shader code: only the curves and
    /// arrays they sample can be changed.
    pub gpu_scripts: Vec<String>,
    /// Each renderer's materials with the colours they draw with, where [`effect_colors`] read
    /// them.
    pub renderers: Vec<RendererColors>,
}

/// A renderer's materials and the colour parameters they draw with.
#[derive(Clone, Debug, Serialize)]
pub struct RendererColors {
    pub export: u32,
    pub object_name: String,
    pub class_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emitter: Option<String>,
    /// Its materials, by object path.
    pub materials: Vec<String>,
    pub colors: Vec<MaterialColor>,
    /// The materials whose colours could not be read, and why.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unread: Vec<String>,
}

/// A colour parameter a renderer draws one of its materials with.
#[derive(Clone, Debug, Serialize)]
pub struct MaterialColor {
    pub name: String,
    pub color: [f32; 4],
    /// The material it is drawn with, by object path, or `None` for the renderer's own value,
    /// which the engine sets over every material's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub material: Option<String>,
    /// That material or the parent of it that sets the value, by object path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    pub enemy: bool,
    /// The variable the renderer binds it to, which sets it every frame in place of this value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bound: Option<String>,
    /// A material layer's or blend's parameter, which the renderer's own values do not reach.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub layered: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ColorSource {
    pub export: u32,
    pub object_name: String,
    pub class_name: String,
    /// The emitter it belongs to, by its unique name; `None` for the system's own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emitter: Option<String>,
    /// The object it sits under, where that is not the emitter or the system: the script that
    /// keeps this copy of a curve, as each of an emitter's scripts keeps one of its own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// The brightest red, green or blue it holds, past 1 for HDR; `None` where a script computes
    /// every one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peak: Option<f32>,
    /// Every colour it holds is grey, so only a tint gives it a hue.
    pub grey: bool,
    /// A name on it mentions enemies, as the colours the game shows for the other team do.
    pub enemy: bool,
    #[serde(flatten)]
    pub body: ColorBody,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ColorBody {
    /// A colour curve's baked samples, RGBA, which are what the game reads: it never bakes them
    /// again from the curves.
    Lut {
        samples: Vec<[f32; 4]>,
        min_time: f32,
        max_time: f32,
        use_lut: bool,
        /// The keys the curves themselves store, which the run time reads only without the LUT.
        keys: usize,
        /// The samples kept again as a texture, which a material reads only where a renderer binds
        /// it, and which an edit to the samples leaves as it was.
        #[serde(skip_serializing_if = "Option::is_none")]
        exposed_texture: Option<String>,
    },
    /// A colour array's elements.
    Array {
        field: String,
        colors: Vec<[f32; 4]>,
    },
    /// A colour parameter's default, as the stores holding it keep it: a user parameter's, a
    /// localization parameter's, or a script's constant. A script's store holds every other
    /// parameter only as a placeholder the run time fills, so those are left out.
    Parameter {
        name: String,
        color: [f32; 4],
        copies: Vec<ParamCopy>,
    },
    /// A colour attribute a CPU script writes, channel by channel.
    Script {
        usage: String,
        attribute: String,
        /// Whether a renderer draws it, by its colour binding or a material parameter bound to
        /// it. One none draws is a value others are worked out from.
        drawn: bool,
        channels: Vec<ScriptChannel>,
    },
}

/// Where a parameter store keeps one copy of a parameter.
#[derive(Clone, Debug, Serialize)]
pub struct ParamCopy {
    /// The store's path among the export's properties.
    pub store: String,
    /// Its byte offset in the store's `ParameterData`, then the previous frame's copy's where an
    /// interpolated spawn script keeps one.
    pub offsets: Vec<u32>,
}

/// One channel a script writes, with what it is computed from.
#[derive(Clone, Debug, Serialize)]
pub struct ScriptChannel {
    /// `R`, `G`, `B` or `A`.
    pub channel: String,
    /// Its value, where it is one literal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<f32>,
    #[serde(flatten)]
    pub trace: VmTrace,
}

const CHANNELS: [&str; 4] = ["R", "G", "B", "A"];
/// A vector's components, read as [`CHANNELS`] where a renderer draws it as a colour.
const COMPONENTS: [&str; 4] = ["X", "Y", "Z", "W"];

/// Where every colour in `parsed` comes from. `mappings` names the struct types a script's data
/// sets hold that the VM reader does not know.
pub fn package_colors(parsed: &ParsedPackage, mappings: Option<&Mappings>) -> ColorReport {
    let mut report = ColorReport::default();
    let drawn = drawn_colors(parsed);
    for export in &parsed.exports {
        let emitter = emitter_of(parsed, export);
        let mut found: Vec<(ColorBody, Vec<String>)> = Vec::new();
        let props = &export.properties;
        match export.class_name.as_str() {
            // A curve nothing baked holds no samples, and nothing reads a colour from it.
            "NiagaraDataInterfaceColorCurve" => match lut_of(props) {
                ColorBody::Lut { samples, .. } if samples.is_empty() => {}
                lut => found.push((lut, Vec::new())),
            },
            "NiagaraDataInterfaceArrayColor" => found.push((
                ColorBody::Array {
                    field: "ColorData".to_string(),
                    colors: items_of(field(props, "ColorData"))
                        .iter()
                        .map(|item| rgba_of(fields_of(item)))
                        .collect(),
                },
                Vec::new(),
            )),
            "NiagaraScript" => {
                let script = read_vm_script(export, mappings);
                if script.usage == "ParticleGPUComputeScript" {
                    report.gpu_scripts.push(export.object_name.clone());
                }
                for (attribute, channels) in script_colors(&script, &drawn, &emitter) {
                    let names = channels
                        .iter()
                        .flat_map(|channel| channel.trace.leaves.iter())
                        .filter_map(leaf_name)
                        .map(str::to_string)
                        .collect();
                    found.push((
                        ColorBody::Script {
                            usage: script.usage.clone(),
                            drawn: drawn.contains(&(emitter.clone(), attribute.clone())),
                            attribute,
                            channels,
                        },
                        names,
                    ));
                }
            }
            _ => {}
        }
        let mut params: Vec<(String, [f32; 4], Vec<ParamCopy>)> = Vec::new();
        for (store, fields) in parameter_stores(props) {
            let (held, data) = store_params(fields, mappings);
            let frame = int_of(field(fields, "ParameterSize")).unwrap_or(0);
            // An interpolated spawn script keeps the previous frame's values after the current's.
            let previous = (frame > 0 && data.len() == frame * 2).then_some(frame as u32);
            let script = matches!(
                store.as_str(),
                "ScriptExecutionParamStore" | "RapidIterationParameters"
            );
            for param in held.iter().filter(|p| p.type_name == "LinearColor") {
                if script && !param.name.starts_with("Constants.") {
                    continue;
                }
                let Some(color) = floats_at(&data, param.offset) else {
                    continue;
                };
                let copy = ParamCopy {
                    store: store.clone(),
                    offsets: std::iter::once(param.offset)
                        .chain(previous.map(|frame| param.offset + frame))
                        .collect(),
                };
                match params.iter_mut().find(|(name, ..)| *name == param.name) {
                    Some((_, _, copies)) => copies.push(copy),
                    None => params.push((param.name.clone(), color, vec![copy])),
                }
            }
        }
        for (name, color, copies) in params {
            found.push((
                ColorBody::Parameter {
                    name: name.clone(),
                    color,
                    copies,
                },
                vec![name],
            ));
        }
        for (body, names) in found {
            let (peak, grey) = tone(&body);
            let enemy = mentions_enemy(&export.object_name)
                || emitter.as_deref().is_some_and(mentions_enemy)
                || names.iter().any(|name| mentions_enemy(name));
            report.sources.push(ColorSource {
                export: export.index,
                object_name: export.object_name.clone(),
                class_name: export.class_name.clone(),
                emitter: emitter.clone(),
                owner: outer_of(parsed, export)
                    .filter(|outer| {
                        !matches!(
                            outer.class_name.as_str(),
                            "NiagaraEmitter" | "NiagaraSystem"
                        )
                    })
                    .map(|outer| outer.object_name.clone()),
                peak,
                grey,
                enemy,
                body,
            });
        }
    }
    report
}

/// [`package_colors`] with the colours each renderer's materials draw with, the materials and
/// their parents read from `source`.
pub fn effect_colors(parsed: &ParsedPackage, source: &ArchetypeSource<'_>) -> ColorReport {
    let mut report = package_colors(parsed, source.mappings);
    let mut loaded = 0;
    report.renderers = renderer_colors(parsed, &mut |name: &str| source.package(name, &mut loaded));
    report
}

/// How many parents a material's colours are followed through.
const MAX_PARENTS: usize = 8;

type Load<'a> = dyn FnMut(&str) -> Result<Arc<ParsedPackage>, String> + 'a;

/// Each renderer in `parsed` with its materials, read through `load`, and the colour parameters
/// they draw with: the renderer's own values first, then each material's, nearest parent first.
fn renderer_colors(parsed: &ParsedPackage, load: &mut Load<'_>) -> Vec<RendererColors> {
    let mut out = Vec::new();
    for export in &parsed.exports {
        if !export.class_name.ends_with("RendererProperties") {
            continue;
        }
        let props = &export.properties;
        let mut renderer = RendererColors {
            export: export.index,
            object_name: export.object_name.clone(),
            class_name: export.class_name.clone(),
            emitter: emitter_of(parsed, export),
            materials: Vec::new(),
            colors: Vec::new(),
            unread: Vec::new(),
        };
        let parameters = field(props, "MaterialParameters").map_or(&[][..], fields_of);
        let bound: Vec<(&str, &str)> = items_of(field(parameters, "AttributeBindings"))
            .iter()
            .filter_map(|binding| {
                let binding = fields_of(binding);
                let variable = field(binding, "NiagaraVariable").map(fields_of)?;
                Some((
                    name_of(field(binding, "MaterialParameterName"))?,
                    name_of(field(variable, "Name"))?,
                ))
            })
            .collect();
        let bound_to = |name: &str| {
            bound
                .iter()
                .find(|(parameter, _)| *parameter == name)
                .map(|(_, variable)| variable.to_string())
        };
        for item in items_of(field(parameters, "VectorParameters")) {
            let item = fields_of(item);
            let Some(name) = name_of(field(item, "MaterialParameterName")) else {
                continue;
            };
            if names_a_colour(name) {
                renderer.colors.push(MaterialColor {
                    name: name.to_string(),
                    color: field(item, "Value").map_or([0.0; 4], |v| rgba_of(fields_of(v))),
                    material: None,
                    from: None,
                    enemy: mentions_enemy(name),
                    bound: bound_to(name),
                    layered: false,
                });
            }
        }
        renderer.materials = renderer_materials(props, load, &mut renderer.unread);
        for material in &renderer.materials {
            let (found, stopped) = material_colors(material, load);
            if let Some(why) = stopped {
                renderer.unread.push(format!("{material}: {why}"));
            }
            for mut color in found {
                let overridden = renderer
                    .colors
                    .iter()
                    .any(|c| c.material.is_none() && c.name == color.name);
                if !overridden {
                    color.bound = bound_to(&color.name);
                    renderer.colors.push(color);
                }
            }
        }
        if !renderer.materials.is_empty() || !renderer.colors.is_empty() {
            out.push(renderer);
        }
    }
    out
}

/// The materials a renderer draws with, by object path: its own, and a mesh renderer's
/// overrides where it uses them, or else its meshes' own.
fn renderer_materials(
    props: &[PropertyEntry],
    load: &mut Load<'_>,
    unread: &mut Vec<String>,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |path: Option<&String>| {
        if let Some(path) = path.filter(|path| !out.contains(path)) {
            out.push(path.clone());
        }
    };
    add(object_path(field(props, "Material")));
    if matches!(
        field(props, "bOverrideMaterials"),
        Some(PropertyValue::Bool { value: true })
    ) {
        for item in items_of(field(props, "OverrideMaterials")) {
            add(object_path(field(fields_of(item), "ExplicitMat")));
        }
        return out;
    }
    for item in items_of(field(props, "Meshes")) {
        let Some(mesh) = object_path(field(fields_of(item), "Mesh")) else {
            continue;
        };
        match load_object(mesh, load) {
            Ok((package, at)) => {
                for slot in items_of(field(&package.exports[at].properties, "StaticMaterials")) {
                    add(object_path(field(fields_of(slot), "MaterialInterface")));
                }
            }
            Err(why) => unread.push(format!("{mesh}: {why}")),
        }
    }
    out
}

/// The colour parameters material `path` sets, nearest first: its own, then those it leaves to
/// each parent in turn, down to the base material's defaults. Says why where the walk ends before
/// the base.
fn material_colors(path: &str, load: &mut Load<'_>) -> (Vec<MaterialColor>, Option<String>) {
    let mut out: Vec<MaterialColor> = Vec::new();
    let mut at = path.to_string();
    for _ in 0..MAX_PARENTS {
        let (package, index) = match load_object(&at, load) {
            Ok(found) => found,
            Err(why) => return (out, Some(format!("{at}: {why}"))),
        };
        let export = &package.exports[index];
        let instance = export.class_name.starts_with("MaterialInstance");
        let vectors = if instance {
            items_of(field(&export.properties, "VectorParameterValues"))
                .iter()
                .map(|item| {
                    let item = fields_of(item);
                    (
                        field(item, "ParameterInfo").map_or(&[][..], fields_of),
                        field(item, "ParameterValue").map_or([0.0; 4], |v| rgba_of(fields_of(v))),
                    )
                })
                .collect()
        } else {
            base_vectors(&export.properties)
        };
        for (info, color) in vectors {
            let Some(name) = name_of(field(info, "Name")) else {
                continue;
            };
            if !names_a_colour(name) || out.iter().any(|held| held.name == name) {
                continue;
            }
            // An unstored association is the first, a layer's.
            let global = matches!(field(info, "Association"),
                Some(PropertyValue::Enum { name: Some(association), .. })
                    if association.ends_with("GlobalParameter"));
            out.push(MaterialColor {
                name: name.to_string(),
                color,
                material: Some(path.to_string()),
                from: Some(at.clone()),
                enemy: mentions_enemy(name),
                bound: None,
                layered: !global,
            });
        }
        if !instance {
            return (out, None);
        }
        match object_path(field(&export.properties, "Parent")) {
            Some(parent) => at = parent.clone(),
            None => return (out, None),
        }
    }
    (out, Some(format!("it has more than {MAX_PARENTS} parents")))
}

/// A base material's vector parameters, each with its default, from the data its cook saves
/// beside the shaders: the names are the second runtime entry's set (by `EMaterialParameterType`)
/// and the values follow in the same order.
fn base_vectors(props: &[PropertyEntry]) -> Vec<(&[PropertyEntry], [f32; 4])> {
    let cached = field(props, "CachedExpressionData").map_or(&[][..], fields_of);
    let entry = cached
        .iter()
        .find(|entry| entry.name == "RuntimeEntries" && entry.element == Some(1))
        .map_or(&[][..], |entry| fields_of(&entry.value));
    items_of(field(entry, "ParameterInfoSet"))
        .iter()
        .zip(items_of(field(cached, "VectorValues")))
        .map(|(info, value)| (fields_of(info), rgba_of(fields_of(value))))
        .collect()
}

/// The top-level object `path` names, as the package `load` reads holds it, by its position.
fn load_object(path: &str, load: &mut Load<'_>) -> Result<(Arc<ParsedPackage>, usize), String> {
    let (package, object) = path
        .split_once('.')
        .ok_or_else(|| format!("{path} names no object"))?;
    if package.starts_with("/Script/") {
        return Err("it is native".to_string());
    }
    let parsed = load(package)?;
    let at = parsed
        .exports
        .iter()
        .position(|export| export.outer_index == 0 && export.object_name == object)
        .ok_or_else(|| format!("{package} holds no {object}"))?;
    Ok((parsed, at))
}

fn object_path(value: Option<&PropertyValue>) -> Option<&String> {
    match value? {
        PropertyValue::Object {
            path: Some(path), ..
        } => Some(path),
        _ => None,
    }
}

/// Whether a material parameter's name says it is a colour, and not a colour's offset or
/// softness, which are other numbers.
fn names_a_colour(name: &str) -> bool {
    let name = name.to_lowercase();
    ["color", "colour", "tint", "颜色"]
        .iter()
        .any(|word| name.contains(word))
        && !["offset", "softness"]
            .iter()
            .any(|word| name.contains(word))
}

/// A change [`recolor`] makes to a colour, alpha aside.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
#[serde(tag = "tool", rename_all = "snake_case")]
pub enum Recolor {
    /// Multiplies red, green and blue by these, past 1 to brighten.
    Tint { rgb: [f32; 3] },
    /// Gives the colour this one's hue and saturation at its own brightness, so a fade or an HDR
    /// glow keeps its shape.
    Paint { rgb: [f32; 3] },
    /// Turns the colour's hue by this many degrees about the grey axis, so a grey stays grey.
    Hue { degrees: f32 },
}

pub fn recolor(color: [f32; 4], tool: Recolor) -> [f32; 4] {
    let [r, g, b, a] = color;
    let rgb = match tool {
        Recolor::Tint { rgb } => [r * rgb[0], g * rgb[1], b * rgb[2]],
        Recolor::Paint { rgb } => {
            let level = r.max(g).max(b);
            paint_scale(rgb).map(|k| k * level)
        }
        Recolor::Hue { degrees } => {
            hue_matrix(degrees).map(|row| row[0] * r + row[1] * g + row[2] * b)
        }
    };
    [rgb[0].max(0.0), rgb[1].max(0.0), rgb[2].max(0.0), a]
}

/// The matrix turning a colour's hue by `degrees` about the grey axis.
fn hue_matrix(degrees: f32) -> [[f32; 3]; 3] {
    let (sin, cos) = degrees.to_radians().sin_cos();
    let k = (1.0 - cos) / 3.0;
    let q = sin / 3f32.sqrt();
    [
        [cos + k, k - q, k + q],
        [k + q, cos + k, k - q],
        [k - q, k + q, cos + k],
    ]
}

/// What a paint multiplies a colour's brightest channel by to give each of its channels.
fn paint_scale(rgb: [f32; 3]) -> [f32; 3] {
    let target = rgb[0].max(rgb[1]).max(rgb[2]);
    if target > 0.0 {
        rgb.map(|channel| channel / target)
    } else {
        [0.0; 3]
    }
}

/// `tool` as a script computes it just before it writes a colour.
fn transform_of(tool: Recolor) -> ColorTransform {
    match tool {
        Recolor::Tint { rgb } => ColorTransform::Linear {
            matrix: [[rgb[0], 0.0, 0.0], [0.0, rgb[1], 0.0], [0.0, 0.0, rgb[2]]],
        },
        Recolor::Paint { rgb } => ColorTransform::Paint {
            k: paint_scale(rgb),
        },
        Recolor::Hue { degrees } => ColorTransform::Linear {
            matrix: hue_matrix(degrees),
        },
    }
}

/// Which colours [`recolor_edits`] changes: every curve, array and parameter in the package, or
/// those of some exports, of one parameter, or whose names mention enemies.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct RecolorPick {
    /// Empty for every export.
    #[serde(default)]
    pub exports: Vec<u32>,
    /// One colour by its name: a parameter, a script colour's attribute or a material colour.
    #[serde(default)]
    pub param: Option<String>,
    #[serde(default)]
    pub enemy: bool,
    /// Also change the colours CPU scripts write for a renderer to draw, where each is written.
    #[serde(default)]
    pub scripts: bool,
    /// Also change the colours the renderers' materials draw with, each set on the renderer.
    #[serde(default)]
    pub materials: bool,
}

/// The value edits a recolour makes, each held to the value it read.
#[derive(Clone, Debug, Default, Serialize)]
pub struct RecolorPlan {
    pub paths: Vec<PathEdit>,
    /// The script colours it transforms where they are written.
    pub vm_colors: Vec<VmColorTransform>,
    /// How many colour sources it changes.
    pub sources: usize,
    /// The script colours among the pick it leaves as they are.
    pub scripts: usize,
    pub notes: Vec<String>,
}

/// The edits giving every colour `pick` names in `parsed` the change `tool` makes. `report` is
/// [`package_colors`] of the same reading, and `mappings` what it was read with.
pub fn recolor_edits(
    parsed: &ParsedPackage,
    report: &ColorReport,
    pick: &RecolorPick,
    tool: Recolor,
    mappings: Option<&Mappings>,
) -> Result<RecolorPlan, String> {
    let picked: Vec<&ColorSource> = report
        .sources
        .iter()
        .filter(|source| pick.exports.is_empty() || pick.exports.contains(&source.export))
        .filter(|source| !pick.enemy || source.enemy)
        .filter(|source| match (&pick.param, &source.body) {
            (Some(param), ColorBody::Parameter { name, .. }) => name == param,
            (Some(param), ColorBody::Script { attribute, .. }) => attribute == param,
            (Some(_), _) => false,
            (None, _) => true,
        })
        .collect();
    let mut plan = RecolorPlan::default();
    let binds_texture = binds_a_texture(parsed);
    if pick.enemy && pick.scripts {
        plan.notes.push(
            "a script works a team's colours out together with the other's, so an enemy pick \
             changes none a script writes"
                .to_string(),
        );
    }
    let mut undrawn = 0;
    for source in picked {
        let export = parsed
            .exports
            .iter()
            .find(|export| export.index == source.export)
            .ok_or_else(|| format!("no export {}", source.export))?;
        let props = &export.properties;
        let mut set = |path: String, value: Option<&PropertyValue>, text: String| {
            plan.paths.push(PathEdit {
                export: source.export.to_string(),
                row: None,
                defaults: false,
                path,
                op: PathOp::Set { text },
                was: value.map(was_of),
                becomes: None,
            });
        };
        match &source.body {
            ColorBody::Lut {
                samples,
                exposed_texture,
                ..
            } => {
                let items = items_of(field(props, "ShaderLUT"));
                for (index, sample) in samples.iter().enumerate() {
                    let new = recolor(*sample, tool);
                    for channel in (0..3).filter(|&c| new[c].to_bits() != sample[c].to_bits()) {
                        let at = index * 4 + channel;
                        set(
                            format!("ShaderLUT[{at}]"),
                            items.get(at),
                            new[channel].to_string(),
                        );
                    }
                }
                if let Some(texture) = exposed_texture.as_ref().filter(|_| binds_texture) {
                    plan.notes.push(format!(
                        "{} is also kept as its texture {texture}, which keeps the old colours, \
                         and a renderer here binds a texture into its material",
                        source.object_name
                    ));
                }
            }
            ColorBody::Array {
                field: name,
                colors,
            } => {
                let items = items_of(field(props, name));
                for (index, color) in colors.iter().enumerate() {
                    let new = recolor(*color, tool);
                    for channel in (0..3).filter(|&c| new[c].to_bits() != color[c].to_bits()) {
                        let value = items
                            .get(index)
                            .and_then(|item| field(fields_of(item), CHANNELS[channel]));
                        set(
                            format!("{name}[{index}].{}", CHANNELS[channel]),
                            value,
                            new[channel].to_string(),
                        );
                    }
                }
            }
            ColorBody::Parameter { color, copies, .. } => {
                let stores = parameter_stores(props);
                let new: Vec<u8> = recolor(*color, tool)[..3]
                    .iter()
                    .flat_map(|c| c.to_le_bytes())
                    .collect();
                for ParamCopy { store, offsets } in copies {
                    let fields = stores
                        .iter()
                        .find(|(path, _)| path == store)
                        .map(|(_, fields)| *fields)
                        .ok_or_else(|| {
                            format!("{}: no parameter store {store}", export.object_name)
                        })?;
                    let items = items_of(field(fields, "ParameterData"));
                    for offset in offsets {
                        for (k, byte) in new.iter().enumerate() {
                            let at = *offset as usize + k;
                            let held = items.get(at);
                            // Each copy is held to its own bytes, which need not match the first's.
                            if matches!(held, Some(PropertyValue::Byte { value }) if value == byte)
                            {
                                continue;
                            }
                            set(
                                format!("{store}.ParameterData[{at}]"),
                                held,
                                byte.to_string(),
                            );
                        }
                    }
                }
            }
            ColorBody::Script {
                attribute,
                drawn,
                channels,
                ..
            } if pick.scripts && !pick.enemy => {
                if !drawn {
                    plan.scripts += 1;
                    undrawn += 1;
                    continue;
                }
                match script_transform(parsed, export, attribute, channels, tool, mappings) {
                    Ok(color) => plan.vm_colors.push(color),
                    Err(why) => {
                        plan.scripts += 1;
                        plan.notes.push(format!(
                            "{attribute} in {} left as it is: {why}",
                            source.object_name
                        ));
                        continue;
                    }
                }
            }
            ColorBody::Script { .. } => {
                plan.scripts += 1;
                continue;
            }
        }
        plan.sources += 1;
    }
    if pick.materials {
        for renderer in report
            .renderers
            .iter()
            .filter(|renderer| pick.exports.is_empty() || pick.exports.contains(&renderer.export))
        {
            material_edits(parsed, renderer, pick, tool, &mut plan)?;
        }
    }
    if plan.sources == 0 {
        return Err(if plan.scripts > 0 && pick.enemy {
            format!(
                "the {} enemy colour(s) picked are worked out by scripts, together with the other                  team's, so an enemy pick cannot change them",
                plan.scripts
            )
        } else if plan.scripts > 0 && !pick.scripts {
            format!(
                "the {} colour(s) picked are computed by scripts: --scripts changes them",
                plan.scripts
            )
        } else if plan.scripts > 0 {
            format!(
                "none of the {} script colour(s) picked can be changed: {}",
                plan.scripts,
                if plan.notes.is_empty() {
                    "no renderer draws them".to_string()
                } else {
                    plan.notes.join("; ")
                }
            )
        } else if let Some(param) = &pick.param {
            format!("no colour parameter or script colour is named {param}")
        } else if pick.enemy {
            "no colour picked mentions enemies".to_string()
        } else if !pick.exports.is_empty() {
            "the exports picked hold no colour".to_string()
        } else {
            if pick.materials {
                "the package holds no colour curve, array, parameter or material colour"
            } else {
                "the package holds no colour curve, array or parameter"
            }
            .to_string()
        });
    }
    if undrawn > 0 {
        plan.notes.push(format!(
            "{undrawn} script colour(s) no renderer draws, which the drawn ones are worked out \
             from, are left as they are"
        ));
    }
    Ok(plan)
}

/// The edits giving each colour `pick` names among those `renderer`'s materials draw with the
/// change `tool` makes, set on the renderer, which the engine applies over the material's own.
/// Only this renderer changes, whatever else draws with the material.
fn material_edits(
    parsed: &ParsedPackage,
    renderer: &RendererColors,
    pick: &RecolorPick,
    tool: Recolor,
    plan: &mut RecolorPlan,
) -> Result<(), String> {
    const VECTORS: &str = "MaterialParameters.VectorParameters";
    let export = parsed
        .exports
        .iter()
        .find(|export| export.index == renderer.export)
        .ok_or_else(|| format!("no export {}", renderer.export))?;
    let stored = items_of(
        field(&export.properties, "MaterialParameters")
            .and_then(|parameters| field(fields_of(parameters), "VectorParameters")),
    );
    let mut names: Vec<&str> = Vec::new();
    for color in &renderer.colors {
        if !names.contains(&color.name.as_str()) {
            names.push(&color.name);
        }
    }
    let mut added = 0;
    for name in names {
        let same: Vec<&MaterialColor> = renderer.colors.iter().filter(|c| c.name == name).collect();
        let first = same[0];
        if (pick.enemy && !first.enemy) || pick.param.as_ref().is_some_and(|param| param != name) {
            continue;
        }
        let label = format!("{} {name}", renderer.object_name);
        if let Some(variable) = &first.bound {
            plan.notes.push(format!(
                "{label} is set from {variable} every frame, so it changes where a script writes that"
            ));
            continue;
        }
        if same.iter().any(|c| c.layered) {
            plan.notes.push(format!(
                "{label} belongs to a material layer, which the renderer's values do not reach"
            ));
            continue;
        }
        if same.iter().any(|c| c.color != first.color) {
            plan.notes.push(format!(
                "{label}: the renderer's materials give it different colours, which one value on \
                 the renderer cannot keep apart"
            ));
            continue;
        }
        let new = recolor(first.color, tool);
        plan.sources += 1;
        if new.map(f32::to_bits) == first.color.map(f32::to_bits) {
            continue;
        }
        let held = stored.iter().position(|item| {
            name_of(field(fields_of(item), "MaterialParameterName")) == Some(name)
        });
        let mut push = |path: String, op: PathOp, was: Option<&PropertyValue>| {
            plan.paths.push(PathEdit {
                export: renderer.export.to_string(),
                row: None,
                defaults: false,
                path,
                op,
                was: was.map(was_of),
                becomes: None,
            });
        };
        let index = match held {
            Some(index) => index,
            None => {
                let index = stored.len() + added;
                added += 1;
                // Each insert names its place, as two alike would be taken for one.
                push(
                    VECTORS.to_string(),
                    PathOp::Insert {
                        index: Some(index as u32),
                        key: None,
                    },
                    None,
                );
                push(
                    format!("{VECTORS}[{index}].MaterialParameterName"),
                    PathOp::Set {
                        text: name.to_string(),
                    },
                    None,
                );
                index
            }
        };
        let value = held
            .and_then(|at| field(fields_of(&stored[at]), "Value"))
            .map_or(&[][..], fields_of);
        for (channel, letter) in CHANNELS.iter().enumerate() {
            let now = field(value, letter);
            let target = new[channel];
            if held.is_some() && float_of(now).is_some_and(|v| v.to_bits() == target.to_bits()) {
                continue;
            }
            push(
                format!("{VECTORS}[{index}].Value.{letter}"),
                PathOp::Set {
                    text: target.to_string(),
                },
                now,
            );
        }
    }
    Ok(())
}

/// The transform giving the colour `attribute` that `export` writes the change `tool` makes, or
/// why it is left as it is.
fn script_transform(
    parsed: &ParsedPackage,
    export: &ParsedExport,
    attribute: &str,
    channels: &[ScriptChannel],
    tool: Recolor,
    mappings: Option<&Mappings>,
) -> Result<VmColorTransform, String> {
    if emitter_export_of(parsed, export).is_some_and(runs_on_gpu) {
        return Err("its emitter runs on the GPU, which never runs this script".to_string());
    }
    if !matches!(tool, Recolor::Paint { .. }) && reads_itself(attribute, channels) {
        return Err(
            "it is worked out from its own last value, so a tint or a hue turn would build up \
             every frame; a paint would not"
                .to_string(),
        );
    }
    let color = VmColorTransform {
        export: export.index.to_string(),
        attribute: attribute.to_string(),
        transform: transform_of(tool),
    };
    lower_vm_edits(parsed, &[], std::slice::from_ref(&color), mappings, false)?;
    Ok(color)
}

/// Whether the red, green or blue `attribute` is written from reads its own red, green or blue
/// (or X, Y or Z, for a vector drawn as a colour).
fn reads_itself(attribute: &str, channels: &[ScriptChannel]) -> bool {
    channels
        .iter()
        .filter(|channel| channel.channel != "A")
        .flat_map(|channel| &channel.trace.leaves)
        .any(|leaf| {
            matches!(leaf, VmLeaf::Input { name, .. } if name
            .strip_prefix(attribute)
            .and_then(|rest| rest.strip_prefix('.'))
            .is_some_and(|channel| {
                CHANNELS[..3].contains(&channel) || COMPONENTS[..3].contains(&channel)
            }))
        })
}

/// The colour attributes renderers draw, each with the emitter whose particles hold it, or
/// `None` for one the system's scripts write under its emitter's name: a renderer's colour
/// binding, `Particles.Color` unless it stores another, and the variables its material
/// parameters are bound to that are colours, or vectors bound to a parameter named as one.
fn drawn_colors(parsed: &ParsedPackage) -> Vec<(Option<String>, String)> {
    let mut out: Vec<(Option<String>, String)> = Vec::new();
    for export in &parsed.exports {
        if !export.class_name.ends_with("RendererProperties") {
            continue;
        }
        let emitter = emitter_of(parsed, export);
        let props = &export.properties;
        let mut add = |name: &str| {
            let scoped = match (name.split_once('.'), &emitter) {
                (Some(("Particles", _)), _) => (emitter.clone(), name.to_string()),
                (Some(("Emitter", rest)), Some(unique)) => (None, format!("{unique}.{rest}")),
                _ => (None, name.to_string()),
            };
            if !out.contains(&scoped) {
                out.push(scoped);
            }
        };
        let binding = ["ColorBinding", "DecalColorBinding"]
            .iter()
            .find_map(|key| field(props, key))
            .map(fields_of);
        if binding.is_some() || COLOR_BOUND.contains(&export.class_name.as_str()) {
            match binding.and_then(|b| field(b, "ParamMapVariable")) {
                Some(variable) => {
                    if let Some(name) = name_of(field(fields_of(variable), "Name")) {
                        add(name);
                    }
                }
                None => add("Particles.Color"),
            }
        }
        let parameters = field(props, "MaterialParameters").map(fields_of);
        for binding in items_of(parameters.and_then(|p| field(p, "AttributeBindings"))) {
            let binding = fields_of(binding);
            let variable = |key: &str| field(binding, key).map(fields_of);
            let colour = variable("NiagaraVariable")
                .and_then(|v| field(v, "TypeDefHandle"))
                .and_then(|handle| object_path(field(fields_of(handle), "ClassStructOrEnum")))
                .is_some_and(|ty| {
                    ty.ends_with(".LinearColor")
                        || ([".Vector3f", ".Vector4f"].iter().any(|v| ty.ends_with(v))
                            && name_of(field(binding, "MaterialParameterName"))
                                .is_some_and(names_a_colour))
                });
            let name = ["ResolvedNiagaraVariable", "NiagaraVariable"]
                .iter()
                .find_map(|key| name_of(variable(key).and_then(|v| field(v, "Name"))));
            if let (true, Some(name)) = (colour, name) {
                add(name);
            }
        }
    }
    out
}

/// Whether a renderer in `parsed` binds a texture variable into its material, the only way a
/// material reads a curve's texture.
fn binds_a_texture(parsed: &ParsedPackage) -> bool {
    parsed
        .exports
        .iter()
        .filter(|export| export.class_name.ends_with("RendererProperties"))
        .flat_map(|export| {
            let parameters = field(&export.properties, "MaterialParameters");
            items_of(parameters.and_then(|p| field(fields_of(p), "AttributeBindings")))
        })
        .any(|binding| {
            field(fields_of(binding), "NiagaraVariable")
                .and_then(|v| field(fields_of(v), "TypeDefHandle"))
                .and_then(|handle| object_path(field(fields_of(handle), "ClassStructOrEnum")))
                .is_some_and(|ty| ty.contains("Texture"))
        })
}

/// The renderers drawing `Particles.Color` when they store no colour binding of their own.
const COLOR_BOUND: [&str; 5] = [
    "NiagaraSpriteRendererProperties",
    "NiagaraRibbonRendererProperties",
    "NiagaraMeshRendererProperties",
    "NiagaraLightRendererProperties",
    "NiagaraDecalRendererProperties",
];

/// Whether `emitter` runs its particles on the GPU, where its scripts' colours are shader code.
fn runs_on_gpu(emitter: &ParsedExport) -> bool {
    items_of(field(&emitter.properties, "VersionData"))
        .first()
        .and_then(|data| field(fields_of(data), "SimTarget"))
        .is_some_and(|target| {
            matches!(target, PropertyValue::Enum { name: Some(name), .. }
                if name.ends_with("GPUComputeSim"))
        })
}

fn mentions_enemy(name: &str) -> bool {
    name.to_ascii_lowercase().contains("enemy")
}

fn outer_of<'a>(parsed: &'a ParsedPackage, export: &ParsedExport) -> Option<&'a ParsedExport> {
    parsed
        .exports
        .get(usize::try_from(export.outer_index).ok()?.checked_sub(1)?)
}

/// The first emitter among `export`'s outers.
fn emitter_export_of<'a>(
    parsed: &'a ParsedPackage,
    export: &'a ParsedExport,
) -> Option<&'a ParsedExport> {
    let mut at = export;
    for _ in 0..parsed.exports.len() {
        let found = outer_of(parsed, at)?;
        if found.class_name == "NiagaraEmitter" {
            return Some(found);
        }
        at = found;
    }
    None
}

/// The unique name of the first emitter among `export`'s outers.
fn emitter_of(parsed: &ParsedPackage, export: &ParsedExport) -> Option<String> {
    let found = emitter_export_of(parsed, export)?;
    Some(match field(&found.properties, "UniqueEmitterName") {
        Some(PropertyValue::Str { value } | PropertyValue::Name { value }) => value.clone(),
        _ => found.object_name.clone(),
    })
}

/// A name or string value, unless it is `None`.
fn name_of(value: Option<&PropertyValue>) -> Option<&str> {
    match value? {
        PropertyValue::Name { value } | PropertyValue::Str { value } => {
            Some(value.as_str()).filter(|name| *name != "None")
        }
        _ => None,
    }
}

/// The brightest red, green or blue among the colours `body` holds, and whether every one is
/// grey.
fn tone(body: &ColorBody) -> (Option<f32>, bool) {
    let colors: Vec<[f32; 4]> = match body {
        ColorBody::Lut { samples, .. } => samples.clone(),
        ColorBody::Array { colors, .. } => colors.clone(),
        ColorBody::Parameter { color, .. } => vec![*color],
        ColorBody::Script { channels, .. } => {
            let known: Vec<f32> = CHANNELS[..3]
                .iter()
                .filter_map(|name| channels.iter().find(|c| c.channel == *name)?.value)
                .collect();
            let grey = known.len() == 3 && is_grey(&[known[0], known[1], known[2], 1.0]);
            return (known.into_iter().reduce(f32::max), grey);
        }
    };
    let peak = colors
        .iter()
        .flat_map(|color| color[..3].iter().copied())
        .reduce(f32::max);
    (peak, !colors.is_empty() && colors.iter().all(is_grey))
}

fn is_grey(color: &[f32; 4]) -> bool {
    const SAME: f32 = 1e-3;
    (color[0] - color[1]).abs() < SAME && (color[1] - color[2]).abs() < SAME
}

/// A colour curve's samples and how they are read. Values it does not store take the class's
/// defaults (`NiagaraDataInterfaceCurveBase.cpp`): times 0 to 1, the LUT in use.
fn lut_of(props: &[PropertyEntry]) -> ColorBody {
    let floats: Vec<f32> = items_of(field(props, "ShaderLUT"))
        .iter()
        .map(|item| float_of(Some(item)).unwrap_or(0.0))
        .collect();
    let keys = ["Red", "Green", "Blue", "Alpha"]
        .iter()
        .map(|curve| {
            let curve = field(props, curve).map(fields_of).unwrap_or(&[]);
            items_of(field(curve, "Keys")).len()
        })
        .sum();
    let exposed = matches!(
        field(props, "bExposeCurve"),
        Some(PropertyValue::Bool { value: true })
    );
    ColorBody::Lut {
        samples: floats
            .chunks_exact(4)
            .map(|c| [c[0], c[1], c[2], c[3]])
            .collect(),
        min_time: float_of(field(props, "LUTMinTime")).unwrap_or(0.0),
        max_time: float_of(field(props, "LUTMaxTime")).unwrap_or(1.0),
        use_lut: !matches!(
            field(props, "bUseLUT"),
            Some(PropertyValue::Bool { value: false })
        ),
        keys,
        exposed_texture: match field(props, "ExposedTexture") {
            Some(PropertyValue::Object {
                path: Some(path), ..
            }) if exposed => Some(path.clone()),
            _ => None,
        },
    }
}

/// Each colour attribute `script` writes to its particles or instance, with its channels in the
/// order it writes them: its colours, and the vectors a renderer draws as one (`drawn`, read for
/// the script's `emitter`), their X, Y, Z and W listed as R, G, B and A.
fn script_colors(
    script: &VmScript,
    drawn: &[(Option<String>, String)],
    emitter: &Option<String>,
) -> Vec<(String, Vec<ScriptChannel>)> {
    let colors: Vec<(&str, bool)> = script
        .attributes
        .iter()
        .filter_map(|attribute| match attribute.type_name.as_str() {
            "LinearColor" => Some((attribute.name.as_str(), false)),
            "Vector3f" | "Vector4f" => Some((attribute.name.as_str(), true)),
            _ => None,
        })
        .collect();
    let mut out: Vec<(String, Vec<ScriptChannel>)> = Vec::new();
    for trace in trace_outputs(script) {
        let Some((attribute, component)) = trace.writes.rsplit_once('.') else {
            continue;
        };
        let Some(&(_, vector)) = colors
            .iter()
            .find(|(color, _)| attribute == *color || attribute.ends_with(&format!(".{color}")))
        else {
            continue;
        };
        let channel = if vector {
            COMPONENTS
                .iter()
                .position(|c| *c == component)
                .filter(|_| drawn.contains(&(emitter.clone(), attribute.to_string())))
                .map(|at| CHANNELS[at])
        } else {
            CHANNELS.iter().copied().find(|c| *c == component)
        };
        let Some(channel) = channel else {
            continue;
        };
        let value = match trace.leaves.as_slice() {
            [
                VmLeaf::Constant {
                    constant,
                    ty: VmType::Float,
                    ..
                },
            ] => constant
                .bits
                .filter(|bits| trace.expression == show_bits(*bits, VmType::Float))
                .map(f32::from_bits),
            _ => None,
        };
        let attribute = attribute.to_string();
        let channel = ScriptChannel {
            channel: channel.to_string(),
            value,
            trace,
        };
        match out.iter_mut().find(|(name, _)| *name == attribute) {
            Some((_, channels)) => channels.push(channel),
            None => out.push((attribute, vec![channel])),
        }
    }
    out
}

fn leaf_name(leaf: &VmLeaf) -> Option<&str> {
    match leaf {
        VmLeaf::Constant { constant, .. } => constant.name.as_deref(),
        VmLeaf::Input { name, .. } => Some(name),
        VmLeaf::Call { function, .. } => Some(function),
        _ => None,
    }
}

/// Every parameter store among `props`, by its path, with its fields.
fn parameter_stores(props: &[PropertyEntry]) -> Vec<(String, &[PropertyEntry])> {
    fn walk<'a>(
        value: &'a PropertyValue,
        path: String,
        out: &mut Vec<(String, &'a [PropertyEntry])>,
    ) {
        match value {
            PropertyValue::Struct { fields, .. } => {
                if field(fields, "SortedParameterOffsets").is_some() {
                    out.push((path, fields));
                    return;
                }
                for entry in fields {
                    walk(&entry.value, format!("{path}.{}", entry.name), out);
                }
            }
            PropertyValue::Array { items } => {
                for (index, item) in items.iter().enumerate() {
                    walk(item, format!("{path}[{index}]"), out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    for entry in props {
        walk(&entry.value, entry.name.clone(), &mut out);
    }
    out
}

fn floats_at(data: &[u8], offset: u32) -> Option<[f32; 4]> {
    let at = offset as usize;
    let bytes = data.get(at..at.checked_add(16)?)?;
    let float = |i: usize| f32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
    Some([float(0), float(4), float(8), float(12)])
}

fn rgba_of(fields: &[PropertyEntry]) -> [f32; 4] {
    CHANNELS.map(|channel| float_of(field(fields, channel)).unwrap_or(0.0))
}

fn field<'a>(fields: &'a [PropertyEntry], name: &str) -> Option<&'a PropertyValue> {
    fields
        .iter()
        .find(|entry| entry.name == name)
        .map(|entry| &entry.value)
}

fn fields_of(value: &PropertyValue) -> &[PropertyEntry] {
    match value {
        PropertyValue::Struct { fields, .. } | PropertyValue::Default { fields, .. } => fields,
        _ => &[],
    }
}

fn items_of(value: Option<&PropertyValue>) -> &[PropertyValue] {
    match value {
        Some(PropertyValue::Array { items } | PropertyValue::Set { items }) => items,
        _ => &[],
    }
}

fn float_of(value: Option<&PropertyValue>) -> Option<f32> {
    match value? {
        PropertyValue::Float { value } => Some(*value as f32),
        _ => None,
    }
}

fn int_of(value: Option<&PropertyValue>) -> Option<usize> {
    match value? {
        PropertyValue::Int { value } => usize::try_from(*value).ok(),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn entry(name: &str, value: PropertyValue) -> PropertyEntry {
        PropertyEntry {
            name: name.to_string(),
            element: None,
            value,
            span: None,
            slot: None,
        }
    }

    fn floats(values: &[f32]) -> PropertyValue {
        PropertyValue::Array {
            items: values
                .iter()
                .map(|value| PropertyValue::Float {
                    value: f64::from(*value),
                })
                .collect(),
        }
    }

    fn close(a: [f32; 4], b: [f32; 4]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5)
    }

    #[test]
    fn a_tint_multiplies_and_a_paint_keeps_each_colours_brightness() {
        let glow = [2.0, 0.5, 0.25, 0.75];
        let tint = Recolor::Tint {
            rgb: [0.5, 1.0, 2.0],
        };
        assert!(close(recolor(glow, tint), [1.0, 0.5, 0.5, 0.75]));
        let purple = Recolor::Paint {
            rgb: [0.5, 0.0, 1.0],
        };
        assert!(close(recolor(glow, purple), [1.0, 0.0, 2.0, 0.75]));
        assert!(close(recolor([0.0; 4], purple), [0.0; 4]));
    }

    #[test]
    fn a_hue_turn_moves_red_to_green_and_leaves_grey_and_alpha() {
        let turn = Recolor::Hue { degrees: 120.0 };
        assert!(close(
            recolor([1.0, 0.0, 0.0, 0.5], turn),
            [0.0, 1.0, 0.0, 0.5]
        ));
        assert!(close(
            recolor([3.0, 3.0, 3.0, 1.0], turn),
            [3.0, 3.0, 3.0, 1.0]
        ));
        let any = recolor([0.9, 0.1, 0.4, 1.0], Recolor::Hue { degrees: 77.0 });
        assert!(any[..3].iter().all(|channel| *channel >= 0.0));
    }

    #[test]
    fn a_curve_reads_its_lut_as_rgba_samples_with_the_class_defaults_where_unstored() {
        let props = vec![
            entry(
                "ShaderLUT",
                floats(&[2.0, 1.0, 0.5, 1.0, 1.0, 1.0, 1.0, 0.0]),
            ),
            entry("bExposeCurve", PropertyValue::Bool { value: false }),
            entry(
                "ExposedTexture",
                PropertyValue::Object {
                    index: 3,
                    path: Some("/Game/NS.NS:Curve:Texture2D_0".into()),
                },
            ),
        ];
        let ColorBody::Lut {
            samples,
            min_time,
            max_time,
            use_lut,
            keys,
            exposed_texture,
        } = lut_of(&props)
        else {
            panic!("a curve reads as a LUT");
        };
        assert_eq!(samples, vec![[2.0, 1.0, 0.5, 1.0], [1.0, 1.0, 1.0, 0.0]]);
        assert_eq!((min_time, max_time, use_lut, keys), (0.0, 1.0, true, 0));
        assert_eq!(exposed_texture, None, "a texture only counts once exposed");
        let body = lut_of(&props);
        assert_eq!(tone(&body), (Some(2.0), false));
    }

    fn channel(name: &str, leaves: Vec<VmLeaf>) -> ScriptChannel {
        ScriptChannel {
            channel: name.to_string(),
            value: None,
            trace: VmTrace {
                at: 0,
                writes: format!("Particles.Color.{name}"),
                expression: String::new(),
                leaves,
                baked: false,
            },
        }
    }

    /// A script applies a tool as a matrix or a paint scale, which gives what `recolor` does.
    #[test]
    fn a_tool_as_a_script_transform_gives_what_recolor_does() {
        let apply = |transform: ColorTransform, c: [f32; 4]| -> [f32; 4] {
            let rgb = match transform {
                ColorTransform::Linear { matrix } => {
                    matrix.map(|row| (row[0] * c[0] + row[1] * c[1] + row[2] * c[2]).max(0.0))
                }
                ColorTransform::Paint { k } => {
                    let level = c[0].max(c[1]).max(c[2]);
                    k.map(|k| level * k)
                }
            };
            [rgb[0], rgb[1], rgb[2], c[3]]
        };
        let tools = [
            Recolor::Tint {
                rgb: [0.5, 1.0, 2.0],
            },
            Recolor::Paint {
                rgb: [0.0, 0.3, 1.0],
            },
            Recolor::Hue { degrees: 120.0 },
            Recolor::Hue { degrees: -45.0 },
        ];
        for tool in tools {
            for color in [[2.0, 0.5, 0.25, 0.75], [0.0; 4], [1.0, 0.26, 0.0, 1.0]] {
                assert!(
                    close(apply(transform_of(tool), color), recolor(color, tool)),
                    "{tool:?} {color:?}"
                );
            }
        }
    }

    /// A colour worked out from its own red, green or blue would take a tint again every frame;
    /// one worked out from another attribute, or from its own alpha, would not.
    #[test]
    fn a_colour_read_back_from_itself_is_told_from_one_read_from_another() {
        let input = |name: &str| VmLeaf::Input {
            at: 0,
            name: name.to_string(),
        };
        let from_initial = vec![
            channel("R", vec![input("Particles.Initial.Color.R")]),
            channel("A", vec![input("Particles.Color.A")]),
        ];
        assert!(!reads_itself("Particles.Color", &from_initial));
        let fading = vec![channel("G", vec![input("Particles.Color.G")])];
        assert!(reads_itself("Particles.Color", &fading));
        let vector = vec![channel("R", vec![input("glowb002.Color02.X")])];
        assert!(reads_itself("glowb002.Color02", &vector));
    }

    #[test]
    fn a_material_colour_is_told_by_its_name_in_either_language() {
        for name in [
            "Enemy1Color01",
            "MuzzleColour",
            "EdgeTint",
            "头部自发光颜色",
        ] {
            assert!(names_a_colour(name), "{name}");
        }
        for name in [
            "FadeDistance",
            "UVOffset",
            "头部自发光强度倍数",
            "LinerColor_Offset&Softness",
        ] {
            assert!(!names_a_colour(name), "{name}");
        }
    }

    #[test]
    fn a_parameter_store_is_found_inside_structs_and_arrays() {
        let store = PropertyValue::Struct {
            name: "NiagaraParameterStore".into(),
            fields: vec![entry(
                "SortedParameterOffsets",
                PropertyValue::Array { items: vec![] },
            )],
        };
        let props = vec![entry(
            "SystemCompiledData",
            PropertyValue::Struct {
                name: "NiagaraSystemCompiledData".into(),
                fields: vec![entry("Stores", PropertyValue::Array { items: vec![store] })],
            },
        )];
        let found: Vec<String> = parameter_stores(&props)
            .into_iter()
            .map(|(path, _)| path)
            .collect();
        assert_eq!(found, vec!["SystemCompiledData.Stores[0]"]);
        let mut data = vec![0u8; 4];
        for value in [1.0f32, 0.5, 0.25, 1.0] {
            data.extend(value.to_le_bytes());
        }
        assert_eq!(floats_at(&data, 4), Some([1.0, 0.5, 0.25, 1.0]));
        assert_eq!(floats_at(&data, 8), None);
    }
}

/// Set `RIVALS_GAME_ROOT` and `RIVALS_USMAP` to run these against the game's own effects.
#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod game_data_tests {
    use super::*;
    use crate::asset::AssetSource;
    use crate::asset_edit::{AssetEditRequest, preview_edits};

    fn install() -> Option<(String, std::sync::Arc<Mappings>)> {
        let root = std::env::var("RIVALS_GAME_ROOT").ok()?;
        let usmap = std::env::var("RIVALS_USMAP").ok()?;
        let mappings = crate::mappings::load(std::path::Path::new(&usmap)).expect("mappings");
        Some((root, mappings))
    }

    fn read(root: &str, mappings: &Mappings, filter: &str) -> crate::game_search::ReadPackage {
        let mut read = crate::game_search::read_packages(root, mappings, false, &[filter])
            .expect("read the game");
        assert_eq!(read.len(), 1, "{filter} names one package");
        read.remove(0)
    }

    const GLOW: &str = "1014301/NS_1014301_MVP_Glow_07.";

    /// A GPU effect: its scripts carry no bytecode, so its colours are its curves'.
    #[test]
    fn a_gpu_effects_colours_are_its_curves_luts() {
        let Some((root, mappings)) = install() else {
            return;
        };
        let package = read(&root, &mappings, GLOW);
        let report = package_colors(package.parsed.as_ref().unwrap(), Some(&mappings));
        assert_eq!(report.gpu_scripts.len(), 2);
        assert_eq!(report.sources.len(), 6, "{:#?}", report.sources);
        // One emitter's two curves, a copy in each of its scripts.
        let copies: Vec<(Option<&str>, Option<&str>)> = report
            .sources
            .iter()
            .map(|source| (source.emitter.as_deref(), source.owner.as_deref()))
            .collect();
        let emitter = Some("NS_Particle_03001");
        assert_eq!(
            copies,
            [
                (emitter, Some("GPUComputeScript")),
                (emitter, Some("GPUComputeScript")),
                (emitter, Some("SpawnScript")),
                (emitter, Some("SpawnScript")),
                (emitter, Some("UpdateScript")),
                (emitter, Some("UpdateScript")),
            ]
        );
        let first = &report.sources[0];
        assert_eq!(first.peak, Some(2.0));
        let ColorBody::Lut {
            samples,
            use_lut,
            exposed_texture,
            ..
        } = &first.body
        else {
            panic!("a curve reads as a LUT");
        };
        assert_eq!(samples.len(), 58);
        assert!(use_lut);
        assert!(
            exposed_texture
                .as_deref()
                .is_some_and(|t| t.ends_with("Texture2D_0"))
        );
    }

    /// The hit effects keep the colour shown for an enemy's hits in a parameter store of their own.
    #[test]
    fn an_enemy_colour_reads_from_its_parameter_store() {
        let Some((root, mappings)) = install() else {
            return;
        };
        let package = read(
            &root,
            &mappings,
            "CombineFX/NS_101441_Characters_Hit_01_CB.",
        );
        let report = package_colors(package.parsed.as_ref().unwrap(), Some(&mappings));
        let light = report
            .sources
            .iter()
            .find(|source| {
                matches!(&source.body, ColorBody::Parameter { name, .. }
                    if name == "L10n.Muzzle_light_IsEnemyColor")
            })
            .expect("the muzzle light's enemy colour");
        assert!(light.enemy);
        let ColorBody::Parameter { color, copies, .. } = &light.body else {
            unreachable!()
        };
        // The system update script's own copy is a white placeholder, and is left out.
        let stores: Vec<&str> = copies.iter().map(|copy| copy.store.as_str()).collect();
        assert_eq!(stores, vec!["LocalizationParametersDefault"]);
        assert_eq!(copies[0].offsets.len(), 1);
        assert!(close(*color, [50.0, 5.848813, 5.848813, 1.0]));
    }

    /// A CPU effect's spawn script writes particle colours, some channels straight from a literal.
    #[test]
    fn a_cpu_scripts_colour_channels_are_traced_to_their_literals() {
        let Some((root, mappings)) = install() else {
            return;
        };
        let package = read(&root, &mappings, "101486/NS_101486_Buff_01.");
        let report = package_colors(package.parsed.as_ref().unwrap(), Some(&mappings));
        let literal = report
            .sources
            .iter()
            .filter_map(|source| match &source.body {
                ColorBody::Script {
                    attribute,
                    channels,
                    ..
                } => Some((attribute, channels)),
                _ => None,
            })
            .find_map(|(attribute, channels)| {
                let channel = channels.iter().find(|c| c.value.is_some())?;
                Some((attribute.clone(), channel.clone()))
            })
            .expect("a channel written from a literal");
        assert!(literal.0.ends_with("Color"), "{}", literal.0);
        assert!(literal.1.trace.baked);
        assert!(matches!(
            literal.1.trace.leaves.as_slice(),
            [VmLeaf::Constant { .. }]
        ));
    }

    fn close(a: [f32; 4], b: [f32; 4]) -> bool {
        a.iter()
            .zip(b)
            .all(|(x, y)| (x - y).abs() <= 1e-4 * x.abs().max(1.0))
    }

    /// The package as `changes` save it, read back.
    fn saved(
        root: &str,
        mappings: &Mappings,
        package: &crate::game_search::ReadPackage,
        changes: rivals_uasset::PackageEdits,
    ) -> ParsedPackage {
        let request = AssetEditRequest {
            game_root: root,
            container: &package.container,
            entry: &package.path,
            kind: AssetSource::Utoc,
            mod_name: "",
            changes,
        };
        let (patched, _) = preview_edits(&request, Some(mappings)).expect("preview");
        rivals_uasset::parse_package(
            &rivals_uasset::AssetBundle {
                asset: &patched.asset,
                exports: &patched.exports,
            },
            Some(mappings),
        )
        .expect("reparse")
    }

    /// The AR's tracer colours its trail in its update script, `Initial * (1.0, 0.26, 0.0)`, and
    /// works out the beam material's enemy colours in its system scripts. A paint with `scripts`
    /// transforms each colour a renderer draws where it is written, so the trail reads back
    /// through the paint, and the colours nothing draws are left as they were.
    #[test]
    fn a_script_paint_saved_reads_back_in_the_tracer() {
        let Some((root, mappings)) = install() else {
            return;
        };
        let package = read(&root, &mappings, "1014001/101431/NS_101431_Trajectory_01.");
        let parsed = package.parsed.as_ref().unwrap();
        let before = package_colors(parsed, Some(&mappings));
        let green = Recolor::Paint {
            rgb: [0.0, 1.0, 0.0],
        };
        let pick = RecolorPick {
            scripts: true,
            ..Default::default()
        };
        let plan = recolor_edits(parsed, &before, &pick, green, Some(&mappings)).expect("plan");
        let attributes: Vec<&str> = plan
            .vm_colors
            .iter()
            .map(|c| c.attribute.as_str())
            .collect();
        assert!(attributes.contains(&"Particles.Color"), "{attributes:?}");
        assert!(
            attributes.contains(&"Trail_01.EnemyColor0"),
            "{:?}",
            plan.notes
        );
        assert!(!attributes.contains(&"Particles.Initial.Color"));
        let after = saved(
            &root,
            &mappings,
            &package,
            rivals_uasset::PackageEdits {
                paths: plan.paths,
                vm_colors: plan.vm_colors,
                ..Default::default()
            },
        );
        let after = package_colors(&after, Some(&mappings));
        let trail = after
            .sources
            .iter()
            .find_map(|source| match &source.body {
                ColorBody::Script {
                    usage,
                    attribute,
                    channels,
                    ..
                } if usage == "ParticleUpdateScript"
                    && attribute == "Particles.Color"
                    && source.emitter.as_deref() == Some("Trail_02") =>
                {
                    Some(channels)
                }
                _ => None,
            })
            .expect("the trail's update colour");
        let expression = |name: &str| {
            trail
                .iter()
                .find(|c| c.channel == name)
                .map(|c| c.trace.expression.clone())
                .expect("the channel")
        };
        let red = expression("R");
        assert!(red.starts_with("mul(max(max("), "{red}");
        assert!(red.contains("Particles.Initial.Color.G"), "{red}");
        assert!(red.ends_with(", 0.0)"), "{red}");
        assert!(expression("G").ends_with(", 1.0)"));
        assert!(expression("A").starts_with("mul(Particles.Initial.Color.A"));
    }

    const BURST: &str = "1028301/NS_1028301_Lobby_BG_Burst_12th_01.";

    /// A renderer binds its material's `Step1Color` to `Emitter.Color02`, a three-component
    /// vector the system update script writes, so that vector is a colour the renderer draws:
    /// listed with its X, Y and Z as red, green and blue, painted where it is written, and picked
    /// alone by its name.
    #[test]
    fn a_vector_a_renderer_draws_as_a_colour_is_painted_where_it_is_written() {
        let Some((root, mappings)) = install() else {
            return;
        };
        let package = read(&root, &mappings, BURST);
        let parsed = package.parsed.as_ref().unwrap();
        let before = package_colors(parsed, Some(&mappings));
        let update = |report: &ColorReport| {
            report
                .sources
                .iter()
                .find_map(|source| match &source.body {
                    ColorBody::Script {
                        usage,
                        attribute,
                        drawn,
                        channels,
                    } if usage == "SystemUpdateScript" && attribute == "glowb002.Color02" => {
                        Some((*drawn, channels.clone()))
                    }
                    _ => None,
                })
                .expect("the system update script's Color02")
        };
        let (drawn, channels) = update(&before);
        assert!(drawn);
        let named: Vec<(&str, &str)> = channels
            .iter()
            .map(|c| (c.channel.as_str(), c.trace.writes.as_str()))
            .collect();
        assert_eq!(
            named,
            [
                ("R", "glowb002.Color02.X"),
                ("G", "glowb002.Color02.Y"),
                ("B", "glowb002.Color02.Z"),
            ]
        );
        let values: Vec<f32> = channels.iter().filter_map(|c| c.value).collect();
        assert!(
            close(
                [values[0], values[1], values[2], 1.0],
                [0.9, 0.117212, 0.060937, 1.0]
            ),
            "{values:?}"
        );
        let pick = RecolorPick {
            scripts: true,
            ..Default::default()
        };
        let green = Recolor::Paint {
            rgb: [0.0, 1.0, 0.0],
        };
        let plan = recolor_edits(parsed, &before, &pick, green, Some(&mappings)).expect("plan");
        assert!(
            plan.vm_colors
                .iter()
                .any(|c| c.attribute == "glowb002.Color02"),
            "{:?}",
            plan.notes
        );
        let one = RecolorPick {
            scripts: true,
            param: Some("glowb002.Color02".into()),
            ..Default::default()
        };
        let only = recolor_edits(parsed, &before, &one, green, Some(&mappings)).expect("one");
        let picked: Vec<(&str, &str)> = only
            .vm_colors
            .iter()
            .map(|c| (c.export.as_str(), c.attribute.as_str()))
            .collect();
        assert_eq!(
            picked.len(),
            2,
            "the spawn and update scripts' writes: {picked:?}"
        );
        assert!(
            picked
                .iter()
                .all(|(_, attribute)| *attribute == "glowb002.Color02")
        );
        assert!(only.paths.is_empty());
        let after = saved(
            &root,
            &mappings,
            &package,
            rivals_uasset::PackageEdits {
                paths: plan.paths,
                vm_colors: plan.vm_colors,
                ..Default::default()
            },
        );
        let (_, channels) = update(&package_colors(&after, Some(&mappings)));
        let expression = |name: &str| {
            channels
                .iter()
                .find(|c| c.channel == name)
                .map(|c| c.trace.expression.clone())
                .expect("the channel")
        };
        assert!(expression("R").ends_with(", 0.0)"), "{}", expression("R"));
        assert!(expression("G").ends_with(", 1.0)"), "{}", expression("G"));
    }

    /// A decal renderer draws `Particles.Color` through a binding of its own name,
    /// `DecalColorBinding`.
    #[test]
    fn a_decal_draws_its_particles_colour() {
        let Some((root, mappings)) = install() else {
            return;
        };
        let package = read(&root, &mappings, "1011500303/NS_1011500303_Liquid_02.");
        let report = package_colors(package.parsed.as_ref().unwrap(), Some(&mappings));
        assert!(report.sources.iter().any(|source| matches!(
            &source.body,
            ColorBody::Script { attribute, drawn: true, .. } if attribute == "Particles.Color"
        )));
    }

    /// A hue turn is a matrix, so it goes only on colours not worked out from themselves: the
    /// system update script's copy of the beam colours is named and left, the spawn script's
    /// taken.
    #[test]
    fn a_hue_turn_leaves_a_colour_worked_out_from_itself() {
        let Some((root, mappings)) = install() else {
            return;
        };
        let package = read(&root, &mappings, "1014001/101431/NS_101431_Trajectory_01.");
        let parsed = package.parsed.as_ref().unwrap();
        let before = package_colors(parsed, Some(&mappings));
        let pick = RecolorPick {
            scripts: true,
            ..Default::default()
        };
        let turn = Recolor::Hue { degrees: 120.0 };
        let plan = recolor_edits(parsed, &before, &pick, turn, Some(&mappings)).expect("plan");
        let exports: Vec<(&str, &str)> = plan
            .vm_colors
            .iter()
            .map(|c| (c.export.as_str(), c.attribute.as_str()))
            .collect();
        assert!(
            exports.contains(&("44", "Trail_01.EnemyColor0")),
            "{exports:?}"
        );
        assert!(
            !exports.contains(&("45", "Trail_01.EnemyColor0")),
            "{exports:?}"
        );
        assert!(
            plan.notes
                .iter()
                .any(|n| n.contains("SystemUpdateScript") && n.contains("its own last value")),
            "{:?}",
            plan.notes
        );
        saved(
            &root,
            &mappings,
            &package,
            rivals_uasset::PackageEdits {
                vm_colors: plan.vm_colors,
                ..Default::default()
            },
        );
    }

    /// Every colour a renderer draws in Punisher's CPU effects takes a transform where it is
    /// written, or is refused for a reason that says why; none leaves a program that breaks.
    #[test]
    fn a_transform_fits_the_colours_punishers_effects_draw() {
        let Some((root, mappings)) = install() else {
            return;
        };
        let packages = crate::game_search::read_packages(
            &root,
            &mappings,
            false,
            &["VFX/Particles/Characters/1014/"],
        )
        .expect("read the game");
        let mut planned = 0;
        let mut refused: std::collections::BTreeMap<String, usize> = Default::default();
        for package in &packages {
            let Ok(parsed) = package.parsed.as_ref() else {
                continue;
            };
            let report = package_colors(parsed, Some(&mappings));
            for source in &report.sources {
                let ColorBody::Script {
                    attribute,
                    drawn: true,
                    channels,
                    ..
                } = &source.body
                else {
                    continue;
                };
                let export = parsed
                    .exports
                    .iter()
                    .find(|e| e.index == source.export)
                    .unwrap();
                let paint = Recolor::Paint {
                    rgb: [0.0, 0.3, 1.0],
                };
                match script_transform(parsed, export, attribute, channels, paint, Some(&mappings))
                {
                    Ok(_) => planned += 1,
                    Err(why) => {
                        // The reason without the script it names or the offsets it gives.
                        let why = why.rsplit_once("): ").map_or(why.as_str(), |(_, w)| w);
                        let key: String = why
                            .split_whitespace()
                            .filter(|word| !word.starts_with("0x") && !word.contains('.'))
                            .collect::<Vec<_>>()
                            .join(" ");
                        *refused.entry(key).or_default() += 1;
                    }
                }
            }
        }
        eprintln!("{planned} planned, refused: {refused:#?}");
        assert!(planned > 0);
        assert!(
            refused
                .keys()
                .all(|why| !why.contains("breaks") && !why.contains("transformed script has")),
            "{refused:#?}"
        );
    }

    const TRACER: &str = "1014001/101431/NS_101431_Trajectory_01.";

    fn colors_of(
        root: &str,
        mappings: &Mappings,
        package: &crate::game_search::ReadPackage,
        parsed: &ParsedPackage,
    ) -> ColorReport {
        effect_colors(
            parsed,
            &ArchetypeSource {
                game_root: root,
                container: &package.container,
                mappings: Some(mappings),
            },
        )
    }

    /// The tracer's ribbons draw with one beam material each: their colours read from it, and the
    /// two the renderer binds to the emitter's script colours say so.
    #[test]
    fn a_renderer_lists_the_colours_its_material_draws_with() {
        let Some((root, mappings)) = install() else {
            return;
        };
        let package = read(&root, &mappings, TRACER);
        let parsed = package.parsed.as_ref().unwrap();
        let report = colors_of(&root, &mappings, &package, parsed);
        assert_eq!(report.renderers.len(), 2);
        let trail = |emitter: &str| {
            report
                .renderers
                .iter()
                .find(|r| r.emitter.as_deref() == Some(emitter))
                .expect("the trail's renderer")
        };
        let second = trail("Trail_02");
        assert!(second.materials[0].ends_with("MI_101431_Beam_3_005_21"));
        assert!(second.unread.is_empty(), "{:?}", second.unread);
        let head = second
            .colors
            .iter()
            .find(|c| c.name == "头部自发光颜色")
            .expect("the head glow");
        assert!(close(head.color, [0.235294, 0.0, 1.0, 1.0]));
        assert!(!head.enemy);
        assert!(
            second
                .colors
                .iter()
                .any(|c| c.name == "Enemy1Color01" && c.enemy)
        );
        let bound = trail("Trail_01")
            .colors
            .iter()
            .find(|c| c.name == "MuzzleColor_3_1")
            .expect("the bound muzzle colour");
        assert_eq!(bound.bound.as_deref(), Some("Emitter.EnemyColor0"));
        let base = second
            .colors
            .iter()
            .find(|c| c.name == "Enemy2Color01")
            .expect("a colour only the base material sets");
        assert!(
            base.from
                .as_deref()
                .is_some_and(|from| from.ends_with(".M_101431_Beam_3_005")),
            "{:?}",
            base.from
        );
        assert!(base.enemy && !base.layered);
    }

    /// A base material's vector defaults are its cached parameter names paired with the values
    /// beside them, in order.
    #[test]
    fn a_base_material_s_colour_defaults_pair_names_with_values() {
        let Some((root, mappings)) = install() else {
            return;
        };
        let package = read(&root, &mappings, "MasterMaterials/M_101431_Beam_3_005.");
        let parsed = package.parsed.as_ref().unwrap();
        let material = parsed
            .exports
            .iter()
            .find(|export| export.class_name == "Material")
            .expect("the material");
        let vectors = base_vectors(&material.properties);
        assert_eq!(vectors.len(), 9);
        let (_, enemy) = vectors
            .iter()
            .find(|(info, _)| name_of(field(info, "Name")) == Some("Enemy1Color01"))
            .expect("Enemy1Color01");
        let want = [0.0, 0.2394, 1.0, 0.0];
        assert!(
            enemy.iter().zip(want).all(|(a, b)| (a - b).abs() < 5e-4),
            "{enemy:?}"
        );
    }

    /// An enemy paint with the materials sets the beam's enemy colours on each ribbon, over the
    /// material, and leaves the material's other colours to it. Made again on the saved copy, it
    /// finds them set and changes nothing.
    #[test]
    fn an_enemy_material_paint_saved_reads_back_as_the_renderer_s_own() {
        let Some((root, mappings)) = install() else {
            return;
        };
        let package = read(&root, &mappings, TRACER);
        let parsed = package.parsed.as_ref().unwrap();
        let before = colors_of(&root, &mappings, &package, parsed);
        let blue = Recolor::Paint {
            rgb: [0.0, 0.3, 1.0],
        };
        let pick = RecolorPick {
            enemy: true,
            materials: true,
            ..Default::default()
        };
        let plan = recolor_edits(parsed, &before, &pick, blue, Some(&mappings)).expect("plan");
        assert_eq!(plan.sources, 12, "six enemy colours on each ribbon");
        let after = saved(
            &root,
            &mappings,
            &package,
            rivals_uasset::PackageEdits {
                paths: plan.paths,
                ..Default::default()
            },
        );
        let report = colors_of(&root, &mappings, &package, &after);
        for renderer in &report.renderers {
            let own: Vec<&MaterialColor> = renderer
                .colors
                .iter()
                .filter(|c| c.material.is_none())
                .collect();
            let names: Vec<&str> = own.iter().map(|c| c.name.as_str()).collect();
            assert_eq!(
                names,
                [
                    "Enemy1Color01",
                    "Enemy1Color02",
                    "Enemy1Color03",
                    "Enemy2Color01",
                    "Enemy2Color02",
                    "Enemy2Color03"
                ],
                "{}",
                renderer.object_name
            );
            let was = before
                .renderers
                .iter()
                .find(|r| r.export == renderer.export)
                .and_then(|r| r.colors.iter().find(|c| c.name == "Enemy1Color01"))
                .expect("the material's enemy colour");
            assert!(close(own[0].color, recolor(was.color, blue)));
            assert!(
                !renderer
                    .colors
                    .iter()
                    .any(|c| c.material.is_some() && c.enemy)
            );
        }
        let again = recolor_edits(&after, &report, &pick, blue, Some(&mappings)).expect("again");
        assert!(again.paths.is_empty(), "{:?}", again.paths);
    }

    /// A paint of the enemy colours lands in the parameter store's bytes as the colours planned.
    #[test]
    fn an_enemy_paint_saved_reads_back_in_the_store() {
        let Some((root, mappings)) = install() else {
            return;
        };
        let package = read(
            &root,
            &mappings,
            "CombineFX/NS_101441_Characters_Hit_01_CB.",
        );
        let parsed = package.parsed.as_ref().unwrap();
        let before = package_colors(parsed, Some(&mappings));
        let purple = Recolor::Paint {
            rgb: [0.6, 0.1, 1.0],
        };
        let pick = RecolorPick {
            enemy: true,
            ..Default::default()
        };
        let plan = recolor_edits(parsed, &before, &pick, purple, Some(&mappings)).expect("plan");
        assert!(plan.paths.iter().all(
            |edit| edit.path.contains(".ParameterData[") || edit.path.starts_with("ShaderLUT[")
        ));
        let request = AssetEditRequest {
            game_root: &root,
            container: &package.container,
            entry: &package.path,
            kind: AssetSource::Utoc,
            mod_name: "",
            changes: rivals_uasset::PackageEdits {
                paths: plan.paths,
                ..Default::default()
            },
        };
        let (patched, _) = preview_edits(&request, Some(&mappings)).expect("preview");
        let after = rivals_uasset::parse_package(
            &rivals_uasset::AssetBundle {
                asset: &patched.asset,
                exports: &patched.exports,
            },
            Some(&mappings),
        )
        .expect("reparse");
        let after = package_colors(&after, Some(&mappings));
        let colour = |report: &ColorReport, wanted: &str| {
            report.sources.iter().find_map(|source| match &source.body {
                ColorBody::Parameter { name, color, .. } if name == wanted => Some(*color),
                _ => None,
            })
        };
        let name = "L10n.Muzzle_light_IsEnemyColor";
        let was = colour(&before, name).expect("before");
        assert!(close(
            colour(&after, name).expect("after"),
            recolor(was, purple)
        ));
    }

    /// A tint written through the editor's save path reads back as the planned colours, and the
    /// curves it was not given are untouched.
    #[test]
    fn a_tint_saved_reads_back_multiplied() {
        let Some((root, mappings)) = install() else {
            return;
        };
        let package = read(&root, &mappings, GLOW);
        let parsed = package.parsed.as_ref().unwrap();
        let before = package_colors(parsed, Some(&mappings));
        let tint = Recolor::Tint {
            rgb: [0.25, 1.0, 2.0],
        };
        let first = before.sources[0].export;
        let pick = RecolorPick {
            exports: vec![first],
            ..Default::default()
        };
        let plan = recolor_edits(parsed, &before, &pick, tint, Some(&mappings)).expect("plan");
        assert_eq!(plan.sources, 1);
        assert!(!plan.paths.is_empty());
        assert!(
            plan.notes.is_empty(),
            "no renderer binds the curve's texture"
        );
        let request = AssetEditRequest {
            game_root: &root,
            container: &package.container,
            entry: &package.path,
            kind: AssetSource::Utoc,
            mod_name: "",
            changes: rivals_uasset::PackageEdits {
                paths: plan.paths,
                ..Default::default()
            },
        };
        let (patched, _) = preview_edits(&request, Some(&mappings)).expect("preview");
        let after = rivals_uasset::parse_package(
            &rivals_uasset::AssetBundle {
                asset: &patched.asset,
                exports: &patched.exports,
            },
            Some(&mappings),
        )
        .expect("reparse");
        let after = package_colors(&after, Some(&mappings));
        for (was, now) in before.sources.iter().zip(&after.sources) {
            let (
                ColorBody::Lut {
                    samples: was_samples,
                    ..
                },
                ColorBody::Lut { samples, .. },
            ) = (&was.body, &now.body)
            else {
                panic!("curves stay curves");
            };
            for (old, new) in was_samples.iter().zip(samples) {
                let expect = if was.export == first {
                    recolor(*old, tint)
                } else {
                    *old
                };
                assert!(close(*new, expect), "{} {old:?} -> {new:?}", was.export);
            }
        }
    }
}
