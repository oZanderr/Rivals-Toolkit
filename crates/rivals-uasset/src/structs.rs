//! Fixed layouts for the engine structs that serialize themselves and therefore ignore schemas.
//!
//! Getting this table wrong desyncs the rest of an export, so anything not listed here falls
//! through to schema-driven parsing and shows up in the audit as an unknown struct. Only the
//! `immutable`/`noexport` core types and those declaring `WithSerializer` belong here: a plain
//! USTRUCT such as FGameplayTag serializes through the property schema like anything else.

use crate::props::{
    ContainerLayout, Ctx, DefaultPart, Diagnostics, NativeLeaf, native_list, read_index,
    read_property_block, record_container, record_container_width,
};
use crate::reader::Cursor;
use crate::value::{PropertyEntry, PropertyValue};

/// UE5 large world coordinates make the core maths types 64-bit, so `Vector` is three doubles.
/// The explicit `f` variants stay 32-bit.
pub(crate) fn read_native(
    name: &str,
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    depth: u32,
) -> Option<Result<PropertyValue, String>> {
    let value = match name {
        // Native around a schema-driven core, so these recurse into the property reader.
        name if crate::niagara::reads(name) => {
            crate::niagara::read(name, cursor, ctx, diagnostics, depth)
        }
        name if crate::moviescene::reads(name) => {
            crate::moviescene::read(name, cursor, ctx, diagnostics, depth)
        }
        "Vector" | "Vector3d" => doubles(cursor, name, &["X", "Y", "Z"]),
        "Vector3f" => floats(cursor, name, &["X", "Y", "Z"]),
        "Vector2D" | "Vector2d" => doubles(cursor, name, &["X", "Y"]),
        // Slate's deprecation wrapper is binary compatible with the float vector it extends.
        "Vector2f" | "DeprecateSlateVector2D" => floats(cursor, name, &["X", "Y"]),
        "Vector4" | "Vector4d" => doubles(cursor, name, &["X", "Y", "Z", "W"]),
        "Vector4f" => floats(cursor, name, &["X", "Y", "Z", "W"]),
        "Rotator" | "Rotator3d" => doubles(cursor, name, &["Pitch", "Yaw", "Roll"]),
        "Rotator3f" => floats(cursor, name, &["Pitch", "Yaw", "Roll"]),
        "Quat" | "Quat4d" => doubles(cursor, name, &["X", "Y", "Z", "W"]),
        "Quat4f" => floats(cursor, name, &["X", "Y", "Z", "W"]),
        "Plane" | "Plane4d" => doubles(cursor, name, &["X", "Y", "Z", "W"]),
        "Plane4f" => floats(cursor, name, &["X", "Y", "Z", "W"]),
        "Matrix" | "Matrix44d" => matrix(cursor, name, true),
        "Matrix44f" => matrix(cursor, name, false),
        "IntPoint" => integers(cursor, name, &["X", "Y"]),
        "IntVector" => integers(cursor, name, &["X", "Y", "Z"]),
        "IntVector4" => integers(cursor, name, &["X", "Y", "Z", "W"]),
        // Written by its own serializer as the bare frame count, without the struct's header.
        "FrameNumber" => integers(cursor, name, &["Value"]),
        "IntVector2" => integers(cursor, name, &["X", "Y"]),
        // The float transform declares a serializer of its own; the double one reads by schema.
        "Transform3f" => transform3f(cursor, name),
        "FontCharacter" => font_character(cursor, name),
        "FontData" => font_data(cursor, ctx, diagnostics),
        "ShaderValueTypeHandle" => shader_value_type(cursor, ctx),
        "SkeletalMeshSamplingLODBuiltData" => scalar(cursor, "AreaWeightedTriangleSampler", |c| {
            weighted_sampler(
                c,
                ctx,
                diagnostics,
                "SkeletalMeshAreaWeightedTriangleSampler",
            )
        })
        .map(|sampler| build(name, vec![sampler])),
        "SkeletalMeshSamplingRegionBuiltData" => region_built_data(cursor, ctx, diagnostics, name),
        // `FKeyHandleMap::Serialize` writes its map only into the transaction buffer.
        "KeyHandleMap" => Ok(build(name, Vec::new())),
        "ClothTetherData" => cloth_tether_data(cursor, ctx, diagnostics, depth),
        "ClothLODDataCommon" => cloth_lod_data(cursor, ctx, diagnostics, depth),
        "MaterialOverrideNanite" => material_override_nanite(cursor, ctx, diagnostics, depth),
        "Guid" => {
            diagnostics
                .native_leaves
                .push((cursor.file_offset(), NativeLeaf::Guid));
            guid(cursor)
        }
        "Color" => bytes(cursor, name, &["B", "G", "R", "A"]),
        "LinearColor" => floats(cursor, name, &["R", "G", "B", "A"]),
        "DateTime" => ticks(cursor, name, "Ticks"),
        "Timespan" => ticks(cursor, name, "Ticks"),
        "Box" => bounds(cursor, name, &["X", "Y", "Z"]),
        "Box2D" => bounds(cursor, name, &["X", "Y"]),
        "Box2f" => bounds_f32(cursor, name, &["X", "Y"]),
        "MovieSceneEventParameters" => event_parameters(cursor, ctx, diagnostics),
        "Sphere" => sphere(cursor, name),
        "TopLevelAssetPath" => {
            diagnostics
                .native_leaves
                .push((cursor.file_offset(), NativeLeaf::TopLevelAssetPath));
            top_level_asset_path(cursor, ctx)
        }
        "SoftObjectPath" | "SoftClassPath" => soft_object_path(cursor, ctx),
        // The game's own wrapper around FSoftObjectPath. Its serializer writes the path string
        // alone, so neither of the two fields the mappings declare appears in cooked data.
        "MarvelSoftObjectPath" => {
            diagnostics
                .native_leaves
                .push((cursor.file_offset(), NativeLeaf::MarvelSoftObjectPath));
            marvel_soft_object_path(cursor)
        }
        // Another of the game's own serializers, and again nothing the mappings declare for it
        // reaches the disk.
        "SerializablePropertySoftPath" => serializable_property_soft_path(cursor, ctx, diagnostics),
        "GameplayTagContainer" => gameplay_tag_container(cursor, ctx, diagnostics),
        "NavAgentSelector" => nav_agent_selector(cursor),
        "RichCurveKey" => rich_curve_key(cursor),
        "MovieSceneFrameRange" => frame_range(cursor, name),
        "PerPlatformFloat" => per_platform(cursor, name, PerPlatform::Float),
        "PerPlatformInt" => per_platform(cursor, name, PerPlatform::Int),
        "PerPlatformBool" => per_platform(cursor, name, PerPlatform::Bool),
        "PerPlatformFrameRate" => per_platform(cursor, name, PerPlatform::FrameRate),
        _ => return None,
    };
    Some(value)
}

/// What a native struct holds when every field is default, for storing one from nothing.
pub(crate) enum NativeDefault {
    /// Not a native layout; the schema decides.
    NotNative,
    Fixed(Vec<u8>),
    /// A layout that spells a name or embeds a reflected struct, assembled from parts.
    Recipe(Vec<DefaultPart>),
}

/// The bytes a native struct holds when every field is default, for storing one from nothing. A
/// layout with a name or a reflected block inside comes back as a recipe for the reader to resolve
/// and the editor to finish.
pub(crate) fn native_default(name: &str) -> NativeDefault {
    let width = match name {
        "Vector" | "Vector3d" | "Rotator" | "Rotator3d" => 24,
        "Vector3f" | "Rotator3f" | "IntVector" => 12,
        "Vector2D" | "Vector2d" | "Vector4f" | "Quat4f" | "Plane4f" | "LinearColor"
        | "IntVector4" | "Guid" => 16,
        "Vector2f"
        | "DeprecateSlateVector2D"
        | "IntPoint"
        | "IntVector2"
        | "DateTime"
        | "Timespan" => 8,
        "Vector4" | "Vector4d" | "Quat" | "Quat4d" | "Plane" | "Plane4d" | "Sphere" => 32,
        "Matrix" | "Matrix44d" => 128,
        "Matrix44f" => 64,
        "Color"
        | "NavAgentSelector"
        | "GameplayTagContainer"
        | "MarvelSoftObjectPath"
        | "FrameNumber" => 4,
        // No serialized bytes, then no path segments.
        "SerializablePropertySoftPath" => 5,
        "Box" => 49,
        "Box2D" => 33,
        "Box2f" => 17,
        // An empty type path (two None names and an empty string) and no payload bytes.
        "MovieSceneEventParameters" => {
            return NativeDefault::Recipe(vec![
                DefaultPart::NoneName,
                DefaultPart::NoneName,
                DefaultPart::Bytes(vec![0u8; 8]),
            ]);
        }
        "RichCurveKey" => 27,
        "MovieSceneFrameRange" => 10,
        "FontCharacter" => 21,
        // The cooked flag comes first, and a clear one would make the loader expect the
        // per-platform map, or the editor font fields, that cooked data does not carry.
        "PerPlatformFloat" | "PerPlatformInt" | "PerPlatformBool" => {
            return NativeDefault::Fixed(vec![1, 0, 0, 0, 0, 0, 0, 0]);
        }
        "PerPlatformFrameRate" => {
            let mut bytes = vec![1, 0, 0, 0];
            bytes.extend_from_slice(&60_000i32.to_le_bytes());
            bytes.extend_from_slice(&1i32.to_le_bytes());
            return NativeDefault::Fixed(bytes);
        }
        "FontData" => return NativeDefault::Fixed(vec![1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
        "Transform3f" => return NativeDefault::Fixed(identity_transform3f()),
        // A Bool scalar: the type byte, the dynamic-array word and the dimension byte.
        "ShaderValueTypeHandle" => 6,
        // Two empty arrays and a zero total weight; the region adds three more empty arrays.
        "SkeletalMeshSamplingLODBuiltData" => 12,
        "SkeletalMeshSamplingRegionBuiltData" => 24,
        "KeyHandleMap" => return NativeDefault::Fixed(Vec::new()),
        // The struct's own reflected block, then no transition skinning data either way.
        "ClothLODDataCommon" => {
            return NativeDefault::Recipe(vec![
                DefaultPart::Struct("ClothLODDataCommon"),
                DefaultPart::Bytes(vec![0u8; 8]),
            ]);
        }
        // The struct's own reflected block, then no tether batches.
        "ClothTetherData" => {
            return NativeDefault::Recipe(vec![
                DefaultPart::Struct("ClothTetherData"),
                DefaultPart::Bytes(vec![0u8; 4]),
            ]);
        }
        // Cooked, no override material, then the reflected block with every slot unset.
        "MaterialOverrideNanite" => {
            return NativeDefault::Recipe(vec![
                DefaultPart::Bytes(vec![1, 0, 0, 0, 0, 0, 0, 0]),
                DefaultPart::Struct("MaterialOverrideNanite"),
            ]);
        }
        name if crate::niagara::reads(name) => return crate::niagara::default_of(name),
        name if crate::moviescene::reads(name) => return crate::moviescene::default_of(name),
        _ => return NativeDefault::NotNative,
    };
    NativeDefault::Fixed(vec![0u8; width])
}

fn entry(name: &str, value: PropertyValue) -> PropertyEntry {
    PropertyEntry {
        name: name.to_string(),
        element: None,
        value,
        span: None,
        slot: None,
    }
}

/// A field whose bytes are its own, recorded so it can be edited in place later.
fn scalar(
    cursor: &mut Cursor<'_>,
    name: &str,
    read: impl FnOnce(&mut Cursor<'_>) -> Result<PropertyValue, String>,
) -> Result<PropertyEntry, String> {
    let start = cursor.file_offset();
    let value = read(cursor)?;
    Ok(PropertyEntry {
        name: name.to_string(),
        element: None,
        value,
        span: Some((start, cursor.file_offset())),
        slot: None,
    })
}

fn build(name: &str, fields: Vec<PropertyEntry>) -> PropertyValue {
    PropertyValue::Struct {
        name: name.to_string(),
        fields,
    }
}

fn doubles(cursor: &mut Cursor<'_>, name: &str, labels: &[&str]) -> Result<PropertyValue, String> {
    let mut fields = Vec::with_capacity(labels.len());
    for label in labels {
        fields.push(scalar(cursor, label, |c| {
            Ok(PropertyValue::Float {
                value: c.read_f64()?,
            })
        })?);
    }
    Ok(build(name, fields))
}

fn floats(cursor: &mut Cursor<'_>, name: &str, labels: &[&str]) -> Result<PropertyValue, String> {
    let mut fields = Vec::with_capacity(labels.len());
    for label in labels {
        fields.push(scalar(cursor, label, |c| {
            Ok(PropertyValue::Float {
                value: f64::from(c.read_f32()?),
            })
        })?);
    }
    Ok(build(name, fields))
}

fn integers(cursor: &mut Cursor<'_>, name: &str, labels: &[&str]) -> Result<PropertyValue, String> {
    let mut fields = Vec::with_capacity(labels.len());
    for label in labels {
        fields.push(scalar(cursor, label, |c| {
            Ok(PropertyValue::Int {
                value: i64::from(c.read_i32()?),
            })
        })?);
    }
    Ok(build(name, fields))
}

fn bytes(cursor: &mut Cursor<'_>, name: &str, labels: &[&str]) -> Result<PropertyValue, String> {
    let mut fields = Vec::with_capacity(labels.len());
    for label in labels {
        fields.push(scalar(cursor, label, |c| {
            Ok(PropertyValue::Byte {
                value: c.read_u8()?,
            })
        })?);
    }
    Ok(build(name, fields))
}

fn ticks(cursor: &mut Cursor<'_>, name: &str, label: &str) -> Result<PropertyValue, String> {
    let value = cursor.read_i64()?;
    Ok(build(
        name,
        vec![entry(label, PropertyValue::Int { value })],
    ))
}

fn matrix(cursor: &mut Cursor<'_>, name: &str, wide: bool) -> Result<PropertyValue, String> {
    let mut fields = Vec::with_capacity(16);
    for index in 0..16 {
        let value = if wide {
            cursor.read_f64()?
        } else {
            f64::from(cursor.read_f32()?)
        };
        fields.push(entry(
            &format!("M{}{}", index / 4, index % 4),
            PropertyValue::Float { value },
        ));
    }
    Ok(build(name, fields))
}

fn bounds(cursor: &mut Cursor<'_>, name: &str, axes: &[&str]) -> Result<PropertyValue, String> {
    let min = doubles(cursor, "Min", axes)?;
    let max = doubles(cursor, "Max", axes)?;
    let is_valid = cursor.read_u8()? != 0;
    Ok(build(
        name,
        vec![
            entry("Min", min),
            entry("Max", max),
            entry("IsValid", PropertyValue::Bool { value: is_valid }),
        ],
    ))
}

/// The single-precision box: `Vector2f` bounds and the validity byte.
fn bounds_f32(cursor: &mut Cursor<'_>, name: &str, axes: &[&str]) -> Result<PropertyValue, String> {
    let min = floats(cursor, "Min", axes)?;
    let max = floats(cursor, "Max", axes)?;
    let is_valid = cursor.read_u8()? != 0;
    Ok(build(
        name,
        vec![
            entry("Min", min),
            entry("Max", max),
            entry("IsValid", PropertyValue::Bool { value: is_valid }),
        ],
    ))
}

/// `FMovieSceneEventParameters::Serialize`: the payload struct's soft path, then its bytes as a
/// counted array, kept as a container so they stay inspectable.
fn event_parameters(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
) -> Result<PropertyValue, String> {
    let struct_type = scalar(cursor, "StructType", |c| soft_object_path(c, ctx))?;
    let bytes = scalar_array(
        cursor,
        ctx,
        diagnostics,
        "StructBytes",
        &usmap::PropertyInner::Byte,
    )?;
    Ok(build("MovieSceneEventParameters", vec![struct_type, bytes]))
}

fn sphere(cursor: &mut Cursor<'_>, name: &str) -> Result<PropertyValue, String> {
    let center = doubles(cursor, "Center", &["X", "Y", "Z"])?;
    let radius = cursor.read_f64()?;
    Ok(build(
        name,
        vec![
            entry("Center", center),
            entry("W", PropertyValue::Float { value: radius }),
        ],
    ))
}

/// A guid a class writes after its properties, kept editable like one inside them.
pub(crate) fn guid_entry(
    cursor: &mut Cursor<'_>,
    diagnostics: &mut Diagnostics,
    name: &str,
) -> Result<PropertyEntry, String> {
    diagnostics
        .native_leaves
        .push((cursor.file_offset(), NativeLeaf::Guid));
    scalar(cursor, name, guid)
}

fn guid(cursor: &mut Cursor<'_>) -> Result<PropertyValue, String> {
    let mut parts = [0u32; 4];
    for part in &mut parts {
        *part = cursor.read_u32()?;
    }
    Ok(PropertyValue::Str {
        value: format!(
            "{:08X}{:08X}{:08X}{:08X}",
            parts[0], parts[1], parts[2], parts[3]
        ),
    })
}

fn top_level_asset_path(cursor: &mut Cursor<'_>, ctx: &Ctx<'_>) -> Result<PropertyValue, String> {
    let package = cursor.read_name(ctx.names())?;
    let asset = cursor.read_name(ctx.names())?;
    Ok(PropertyValue::SoftObject {
        path: join_asset_path(&package, &asset, ""),
    })
}

/// The sixteen reflected bitfield bools share one word that is not itself a property, and
/// `FNavAgentSelector::Serialize` writes only that word.
fn nav_agent_selector(cursor: &mut Cursor<'_>) -> Result<PropertyValue, String> {
    let packed = cursor.read_u32()?;
    let fields = (0..16)
        .map(|bit| {
            entry(
                &format!("bSupportsAgent{bit}"),
                PropertyValue::Bool {
                    value: packed >> bit & 1 == 1,
                },
            )
        })
        .collect();
    Ok(build("NavAgentSelector", fields))
}

fn marvel_soft_object_path(cursor: &mut Cursor<'_>) -> Result<PropertyValue, String> {
    Ok(PropertyValue::SoftObject {
        path: cursor.read_string()?,
    })
}

/// This game's `FSerializablePropertySoftPath`: the bytes it serialized as a counted array, then
/// the property path as a list of segments behind a byte count. The two reflected slots the
/// mappings declare for it are not what the serializer writes.
fn serializable_property_soft_path(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
) -> Result<PropertyValue, String> {
    let data = scalar_array(
        cursor,
        ctx,
        diagnostics,
        "Data",
        &usmap::PropertyInner::Byte,
    )?;
    let path = byte_counted_strings(cursor, ctx, diagnostics, "PropertySoftPath")?;
    Ok(build("SerializablePropertySoftPath", vec![data, path]))
}

/// A list of strings behind a `u8` count, recorded as a container so its segments read, edit, grow
/// and shrink like a declared array's.
fn byte_counted_strings(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    name: &str,
) -> Result<PropertyEntry, String> {
    let at = cursor.file_offset();
    let count = usize::from(cursor.read_u8()?);
    // Even an empty string costs its four byte length, so this bounds the list against the export.
    if count * 4 > cursor.remaining() {
        return Err(cursor.err(format!("implausible {name} count {count}")));
    }
    let mut items = Vec::with_capacity(count);
    let mut elements = Vec::with_capacity(count);
    for _ in 0..count {
        let start = cursor.file_offset();
        items.push(PropertyValue::Str {
            value: cursor.read_string()?,
        });
        elements.push((start, cursor.file_offset()));
    }
    record_container_width(
        diagnostics,
        ctx,
        at,
        at,
        1,
        elements,
        &usmap::PropertyInner::Str,
        None,
    );
    Ok(PropertyEntry {
        name: name.to_string(),
        element: None,
        value: PropertyValue::Array { items },
        span: Some((at, cursor.file_offset())),
        slot: None,
    })
}

fn soft_object_path(cursor: &mut Cursor<'_>, ctx: &Ctx<'_>) -> Result<PropertyValue, String> {
    let package = cursor.read_name(ctx.names())?;
    let asset = cursor.read_name(ctx.names())?;
    let sub_path = cursor.read_string()?;
    Ok(PropertyValue::SoftObject {
        path: join_asset_path(&package, &asset, &sub_path),
    })
}

fn join_asset_path(package: &str, asset: &str, sub_path: &str) -> String {
    let mut path = String::new();
    if package != "None" && !package.is_empty() {
        path.push_str(package);
    }
    if asset != "None" && !asset.is_empty() {
        if !path.is_empty() {
            path.push('.');
        }
        path.push_str(asset);
    }
    if !sub_path.is_empty() {
        path.push(':');
        path.push_str(sub_path);
    }
    path
}

/// `FGameplayTagContainer::Serialize` writes only the tag array; parent tags are rebuilt on load.
///
/// This reads like an array and is presented as one, so it records the same layout an array does.
/// Without that its tags would show as editable and then be refused at save time.
fn gameplay_tag_container(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
) -> Result<PropertyValue, String> {
    let count_at = cursor.file_offset();
    let count = cursor.read_i32()?;
    if count < 0 || (count as usize).saturating_mul(8) > cursor.remaining() {
        return Err(cursor.err(format!("implausible gameplay tag count {count}")));
    }
    let mut items = Vec::with_capacity(count as usize);
    let mut elements = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let start = cursor.file_offset();
        items.push(PropertyValue::Name {
            value: cursor.read_name(ctx.names())?,
        });
        elements.push((start, cursor.file_offset()));
    }
    diagnostics.containers.push(ContainerLayout {
        at: count_at,
        count_at,
        count_width: 4,
        elements_at: None,
        elements,
        element_kind: "Name",
        // A tag is a name; the empty one is spelt None and encoded when the edit is made.
        default_element: None,
        element_is_enum: false,
        element_enum: None,
        default_name: Some("None".to_string()),
        default_recipe: None,
        keys: None,
    });
    Ok(PropertyValue::Array { items })
}

fn rich_curve_key(cursor: &mut Cursor<'_>) -> Result<PropertyValue, String> {
    let mut fields = Vec::with_capacity(9);
    for label in ["InterpMode", "TangentMode", "TangentWeightMode"] {
        fields.push(entry(
            label,
            PropertyValue::Byte {
                value: cursor.read_u8()?,
            },
        ));
    }
    for label in [
        "Time",
        "Value",
        "ArriveTangent",
        "ArriveTangentWeight",
        "LeaveTangent",
        "LeaveTangentWeight",
    ] {
        fields.push(entry(
            label,
            PropertyValue::Float {
                value: f64::from(cursor.read_f32()?),
            },
        ));
    }
    Ok(build("RichCurveKey", fields))
}

/// A `TRange<FFrameNumber>`: each bound is a type byte followed by the frame number.
fn frame_range(cursor: &mut Cursor<'_>, name: &str) -> Result<PropertyValue, String> {
    let mut fields = Vec::with_capacity(2);
    for label in ["LowerBound", "UpperBound"] {
        let kind = cursor.read_i8()?;
        let value = cursor.read_i32()?;
        fields.push(entry(
            label,
            build(
                "FrameNumberRangeBound",
                vec![
                    entry(
                        "Type",
                        PropertyValue::Int {
                            value: i64::from(kind),
                        },
                    ),
                    entry(
                        "Value",
                        PropertyValue::Int {
                            value: i64::from(value),
                        },
                    ),
                ],
            ),
        ));
    }
    Ok(build(name, fields))
}

enum PerPlatform {
    Float,
    Int,
    Bool,
    FrameRate,
}

/// `TPerPlatformProperty` writes its cooked flag as a full word, then the single surviving
/// platform value, a bool among them a full word too.
fn per_platform(
    cursor: &mut Cursor<'_>,
    name: &str,
    kind: PerPlatform,
) -> Result<PropertyValue, String> {
    let cooked = scalar(cursor, "bCooked", |c| {
        Ok(PropertyValue::Bool {
            value: c.read_bool32()?,
        })
    })?;
    let value = match kind {
        PerPlatform::Float => scalar(cursor, "Value", |c| {
            Ok(PropertyValue::Float {
                value: f64::from(c.read_f32()?),
            })
        })?,
        PerPlatform::Int => scalar(cursor, "Value", |c| {
            Ok(PropertyValue::Int {
                value: i64::from(c.read_i32()?),
            })
        })?,
        PerPlatform::Bool => scalar(cursor, "Value", |c| {
            Ok(PropertyValue::Bool {
                value: c.read_bool32()?,
            })
        })?,
        PerPlatform::FrameRate => scalar(cursor, "Value", |c| {
            integers(c, "FrameRate", &["Numerator", "Denominator"])
        })?,
    };
    Ok(build(name, vec![cooked, value]))
}

/// `TTransform<float>::Serialize`: rotation, translation and scale with no header between them.
fn transform3f(cursor: &mut Cursor<'_>, name: &str) -> Result<PropertyValue, String> {
    let rotation = scalar(cursor, "Rotation", |c| {
        floats(c, "Quat4f", &["X", "Y", "Z", "W"])
    })?;
    let translation = scalar(cursor, "Translation", |c| {
        floats(c, "Vector3f", &["X", "Y", "Z"])
    })?;
    let scale = scalar(cursor, "Scale3D", |c| {
        floats(c, "Vector3f", &["X", "Y", "Z"])
    })?;
    Ok(build(name, vec![rotation, translation, scale]))
}

fn identity_transform3f() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(40);
    for value in [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

/// `FFontCharacter`'s own `operator<<`: four ints, the texture index byte, the vertical offset.
fn font_character(cursor: &mut Cursor<'_>, name: &str) -> Result<PropertyValue, String> {
    let mut fields = Vec::with_capacity(6);
    for label in ["StartU", "StartV", "USize", "VSize"] {
        fields.push(scalar(cursor, label, |c| {
            Ok(PropertyValue::Int {
                value: i64::from(c.read_i32()?),
            })
        })?);
    }
    fields.push(scalar(cursor, "TextureIndex", |c| {
        Ok(PropertyValue::Byte {
            value: c.read_u8()?,
        })
    })?);
    fields.push(scalar(cursor, "VerticalOffset", |c| {
        Ok(PropertyValue::Int {
            value: i64::from(c.read_i32()?),
        })
    })?);
    Ok(build(name, fields))
}

/// `FFontData::Serialize` for cooked data: a full-word cooked flag, the font face asset and the
/// sub-face index. The file name, hinting and loading policy the mappings declare are editor data.
fn font_data(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
) -> Result<PropertyValue, String> {
    let cooked = scalar(cursor, "bIsCooked", |c| {
        Ok(PropertyValue::Bool {
            value: c.read_bool32()?,
        })
    })?;
    let asset = scalar(cursor, "FontFaceAsset", |c| {
        let index = read_index(c, diagnostics)?;
        Ok(PropertyValue::Object {
            index,
            path: ctx.object_path(index).map_err(|e| c.err(e))?,
        })
    })?;
    let sub_face = scalar(cursor, "SubFaceIndex", |c| {
        Ok(PropertyValue::Int {
            value: i64::from(c.read_i32()?),
        })
    })?;
    Ok(build("FontData", vec![cooked, asset, sub_face]))
}

/// `FShaderValueTypeHandle::Serialize` writes the value type itself: the fundamental type, a
/// full-word dynamic-array flag, the dimension, then the vector or matrix size that dimension
/// needs. A struct type would carry a name and elements past that; none appears in this game's
/// cooked data, so one is refused rather than guessed at.
fn shader_value_type(cursor: &mut Cursor<'_>, ctx: &Ctx<'_>) -> Result<PropertyValue, String> {
    const STRUCT: u8 = 4;
    const VECTOR: u8 = 1;
    const MATRIX: u8 = 2;
    let (kind, kind_value) = enum_byte(cursor, ctx, "Type", "EShaderFundamentalType")?;
    if kind_value == STRUCT {
        return Err(cursor.err("struct-typed shader values are not modelled"));
    }
    let dynamic = scalar(cursor, "bIsDynamicArray", |c| {
        Ok(PropertyValue::Bool {
            value: c.read_bool32()?,
        })
    })?;
    let (dimension, dimension_value) = enum_byte(
        cursor,
        ctx,
        "DimensionType",
        "EShaderFundamentalDimensionType",
    )?;
    let mut fields = vec![kind, dynamic, dimension];
    let byte = |c: &mut Cursor<'_>| {
        Ok(PropertyValue::Byte {
            value: c.read_u8()?,
        })
    };
    match dimension_value {
        VECTOR => fields.push(scalar(cursor, "VectorElemCount", byte)?),
        MATRIX => {
            fields.push(scalar(cursor, "MatrixRowCount", byte)?);
            fields.push(scalar(cursor, "MatrixColumnCount", byte)?);
        }
        _ => {}
    }
    Ok(build("ShaderValueTypeHandle", fields))
}

/// `FWeightedRandomSampler::Serialize`: the probabilities, the alias table and the total weight.
fn weighted_sampler(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    name: &str,
) -> Result<PropertyValue, String> {
    let fields = vec![
        scalar_array(
            cursor,
            ctx,
            diagnostics,
            "Prob",
            &usmap::PropertyInner::Float,
        )?,
        scalar_array(
            cursor,
            ctx,
            diagnostics,
            "Alias",
            &usmap::PropertyInner::Int,
        )?,
        scalar(cursor, "TotalWeight", |c| {
            Ok(PropertyValue::Float {
                value: f64::from(c.read_f32()?),
            })
        })?,
    ];
    Ok(build(name, fields))
}

/// `FSkeletalMeshSamplingRegionBuiltData::Serialize`: the region's triangles and bones, its own
/// sampler, then the vertices, which Niagara's vertex sampling added last.
fn region_built_data(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    name: &str,
) -> Result<PropertyValue, String> {
    let triangles = scalar_array(
        cursor,
        ctx,
        diagnostics,
        "TriangleIndices",
        &usmap::PropertyInner::Int,
    )?;
    let bones = scalar_array(
        cursor,
        ctx,
        diagnostics,
        "BoneIndices",
        &usmap::PropertyInner::Int,
    )?;
    let sampler = scalar(cursor, "AreaWeightedSampler", |c| {
        weighted_sampler(
            c,
            ctx,
            diagnostics,
            "SkeletalMeshAreaWeightedTriangleSampler",
        )
    })?;
    let vertices = scalar_array(
        cursor,
        ctx,
        diagnostics,
        "Vertices",
        &usmap::PropertyInner::Int,
    )?;
    Ok(build(name, vec![triangles, bones, sampler, vertices]))
}

/// The struct's own reflected properties, read against the schema, for the layouts that write
/// them around bytes of their own.
fn reflected(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    depth: u32,
    name: &str,
    fields: &mut Vec<PropertyEntry>,
) -> Result<(), String> {
    let Some(schema) = ctx.schema(name) else {
        diagnostics.unresolved_structs.insert(name.to_string());
        return Err(cursor.err(format!("struct {name} has no schema in the mappings file")));
    };
    read_property_block(cursor, &schema, ctx, diagnostics, depth + 1, fields)
}

/// `FClothTetherData::Serialize`: the struct's reflected block, empty in this build, then the
/// tethers in batches of anchor index, vertex index and reference length.
fn cloth_tether_data(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    depth: u32,
) -> Result<PropertyValue, String> {
    let mut fields = Vec::new();
    reflected(
        cursor,
        ctx,
        diagnostics,
        depth,
        "ClothTetherData",
        &mut fields,
    )?;
    fields.push(native_list(
        cursor,
        diagnostics,
        "Tethers",
        vec![DefaultPart::Bytes(vec![0u8; 4])],
        |c, diagnostics| {
            Ok(native_list(
                c,
                diagnostics,
                "Batch",
                vec![DefaultPart::Bytes(vec![0u8; 12])],
                |c, _| cloth_tether(c),
            )?
            .value)
        },
    )?);
    Ok(build("ClothTetherData", fields))
}

fn cloth_tether(cursor: &mut Cursor<'_>) -> Result<PropertyValue, String> {
    let int = |c: &mut Cursor<'_>| {
        Ok(PropertyValue::Int {
            value: i64::from(c.read_i32()?),
        })
    };
    let fields = vec![
        scalar(cursor, "AnchorIndex", int)?,
        scalar(cursor, "VertexIndex", int)?,
        scalar(cursor, "RefLength", |c| {
            Ok(PropertyValue::Float {
                value: f64::from(c.read_f32()?),
            })
        })?,
    ];
    Ok(build("ClothTether", fields))
}

/// `FClothLODDataCommon::Serialize`: the struct's reflected block, then the skinning data for
/// transitions from the LOD above and from the one below, neither a reflected property.
fn cloth_lod_data(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    depth: u32,
) -> Result<PropertyValue, String> {
    let mut fields = Vec::new();
    reflected(
        cursor,
        ctx,
        diagnostics,
        depth,
        "ClothLODDataCommon",
        &mut fields,
    )?;
    for name in ["TransitionUpSkinData", "TransitionDownSkinData"] {
        fields.push(native_list(
            cursor,
            diagnostics,
            name,
            vec![DefaultPart::Bytes(vec![0u8; MESH_TO_MESH_VERT_BYTES])],
            |c, _| mesh_to_mesh_vert(c),
        )?);
    }
    Ok(build("ClothLODDataCommon", fields))
}

/// Three `FVector4f`, four `uint16` indices, the weight and a padding word.
const MESH_TO_MESH_VERT_BYTES: usize = 16 * 3 + 2 * 4 + 4 + 4;

/// `FMeshToMeshVertData`, written raw: barycentric coordinates and distances for the position,
/// normal and tangent, the four source vertices, the weight and a padding word.
fn mesh_to_mesh_vert(cursor: &mut Cursor<'_>) -> Result<PropertyValue, String> {
    let mut fields = Vec::new();
    for name in [
        "PositionBaryCoordsAndDist",
        "NormalBaryCoordsAndDist",
        "TangentBaryCoordsAndDist",
    ] {
        fields.push(scalar(cursor, name, |c| {
            floats(c, "Vector4f", &["X", "Y", "Z", "W"])
        })?);
    }
    for index in 0..4u32 {
        let mut entry = scalar(cursor, "SourceMeshVertIndices", |c| {
            Ok(PropertyValue::UInt {
                value: u64::from(c.read_u16()?),
            })
        })?;
        entry.element = Some(index);
        fields.push(entry);
    }
    fields.push(scalar(cursor, "Weight", |c| {
        Ok(PropertyValue::Float {
            value: f64::from(c.read_f32()?),
        })
    })?);
    fields.push(scalar(cursor, "Padding", |c| {
        Ok(PropertyValue::UInt {
            value: u64::from(c.read_u32()?),
        })
    })?);
    Ok(build("MeshToMeshVertData", fields))
}

/// `FMaterialOverrideNanite::Serialize`: a cooked flag, the override material as a hard reference
/// in place of the editor's soft one, then the struct's reflected block with its slots unset.
fn material_override_nanite(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    depth: u32,
) -> Result<PropertyValue, String> {
    let cooked = scalar(cursor, "bCooked", |c| {
        let flag = c.read_u32()?;
        if flag != 1 {
            return Err(c.err(format!(
                "a Nanite override's cooked flag reads {flag}; only cooked data is understood"
            )));
        }
        Ok(PropertyValue::Bool { value: true })
    })?;
    let material = scalar(cursor, "CookedOverrideMaterial", |c| {
        let index = read_index(c, diagnostics)?;
        Ok(PropertyValue::Object {
            index,
            path: ctx.object_path(index).map_err(|e| c.err(e))?,
        })
    })?;
    let mut fields = vec![cooked, material];
    reflected(
        cursor,
        ctx,
        diagnostics,
        depth,
        "MaterialOverrideNanite",
        &mut fields,
    )?;
    Ok(build("MaterialOverrideNanite", fields))
}

/// A bare `TArray` of four-byte numbers inside a native struct, recorded as a container so it
/// grows and shrinks like a declared one.
fn scalar_array(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    name: &str,
    inner: &usmap::PropertyInner,
) -> Result<PropertyEntry, String> {
    let at = cursor.file_offset();
    let count = cursor.read_i32()?;
    let width = match inner {
        usmap::PropertyInner::Byte => 1,
        _ => 4,
    };
    let fits = usize::try_from(count)
        .ok()
        .and_then(|n| n.checked_mul(width))
        .is_some_and(|bytes| bytes <= cursor.remaining());
    if !fits {
        return Err(cursor.err(format!("implausible {name} count {count}")));
    }
    let mut items = Vec::with_capacity(count as usize);
    let mut elements = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let start = cursor.file_offset();
        items.push(match inner {
            usmap::PropertyInner::Float => PropertyValue::Float {
                value: f64::from(cursor.read_f32()?),
            },
            usmap::PropertyInner::Byte => PropertyValue::Byte {
                value: cursor.read_u8()?,
            },
            _ => PropertyValue::Int {
                value: i64::from(cursor.read_i32()?),
            },
        });
        elements.push((start, cursor.file_offset()));
    }
    record_container(diagnostics, ctx, at, at, elements, inner, None);
    Ok(PropertyEntry {
        name: name.to_string(),
        element: None,
        value: PropertyValue::Array { items },
        span: Some((at, cursor.file_offset())),
        slot: None,
    })
}

/// A byte-sized enum field, named where the mappings know the enumerator, with its raw value for
/// the caller's own branching.
fn enum_byte(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    name: &str,
    enum_name: &str,
) -> Result<(PropertyEntry, u8), String> {
    let mut raw = 0u8;
    let entry = scalar(cursor, name, |c| {
        raw = c.read_u8()?;
        Ok(PropertyValue::Enum {
            value: i64::from(raw),
            name: ctx.enum_name(enum_name, i64::from(raw)),
            enum_type: Some(enum_name.to_string()),
        })
    })?;
    Ok((entry, raw))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    use retoc::legacy_asset::FLegacyPackageHeader;

    fn ctx_over(header: &FLegacyPackageHeader) -> Ctx<'_> {
        Ctx {
            mappings: None,
            header,
            fixups: None,
            synth: None,
            local: None,
        }
    }

    fn fields_of(value: PropertyValue) -> Vec<PropertyEntry> {
        match value {
            PropertyValue::Struct { fields, .. } => fields,
            other => panic!("expected a struct, got {other:?}"),
        }
    }

    fn soft_path(data: &[u8]) -> Result<PropertyValue, String> {
        let header = FLegacyPackageHeader::default();
        let ctx = ctx_over(&header);
        let mut cursor = Cursor::new(data, 0);
        let mut diagnostics = Diagnostics::default();
        read_native(
            "SerializablePropertySoftPath",
            &mut cursor,
            &ctx,
            &mut diagnostics,
            0,
        )
        .expect("a native layout")
    }

    fn string_bytes(out: &mut Vec<u8>, text: &str) {
        out.extend_from_slice(&(text.len() as i32 + 1).to_le_bytes());
        out.extend_from_slice(text.as_bytes());
        out.push(0);
    }

    /// The bytes one `DiffProperties` element holds in `1023_2201_EffectTable`: no serialized
    /// data, then a two segment path. The mappings describe this struct as two reflected slots,
    /// which is why reading it by schema fails inside its payload.
    #[test]
    fn a_serializable_property_soft_path_reads_its_data_then_its_segments() {
        let mut data = 0i32.to_le_bytes().to_vec();
        data.push(2);
        string_bytes(&mut data, "ScopeQuote");
        string_bytes(&mut data, "Spawn_AgentId");
        assert_eq!(data.len(), 38, "the element the game writes");

        let header = FLegacyPackageHeader::default();
        let ctx = ctx_over(&header);
        let mut cursor = Cursor::new(&data, 0);
        let mut diagnostics = Diagnostics::default();
        let value = read_native(
            "SerializablePropertySoftPath",
            &mut cursor,
            &ctx,
            &mut diagnostics,
            0,
        )
        .expect("a native layout")
        .expect("reads");
        assert_eq!(cursor.remaining(), 0, "it ends exactly on the payload");

        let fields = fields_of(value);
        assert_eq!(
            fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
            ["Data", "PropertySoftPath"]
        );
        let PropertyValue::Array { items } = &fields[1].value else {
            panic!("expected the segments as an array");
        };
        assert_eq!(
            items.iter().map(PropertyValue::summary).collect::<Vec<_>>(),
            ["ScopeQuote", "Spawn_AgentId"]
        );
        assert_eq!(fields[1].span, Some((4, 38)));

        assert_eq!(diagnostics.containers.len(), 2);
        assert_eq!(diagnostics.containers[0].count_width, 4, "Data is a TArray");
        let segments = &diagnostics.containers[1];
        assert_eq!((segments.at, segments.count_at), (4, 4));
        assert_eq!(segments.count_width, 1);
        assert_eq!(segments.element_kind, "Str");
        assert_eq!(segments.elements, vec![(5, 20), (20, 38)]);
    }

    /// The serialized bytes are a counted array of their own, and a path may hold none at all.
    #[test]
    fn a_serializable_property_soft_path_carries_its_serialized_bytes() {
        let mut data = 3i32.to_le_bytes().to_vec();
        data.extend_from_slice(&[0xAA, 0xBB, 0xCC]);
        data.push(1);
        string_bytes(&mut data, "A");

        let fields = fields_of(soft_path(&data).expect("reads"));
        let PropertyValue::Array { items } = &fields[0].value else {
            panic!("expected the data as an array");
        };
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].summary(), "170");
        let PropertyValue::Array { items } = &fields[1].value else {
            panic!("expected the segments as an array");
        };
        assert_eq!(
            items.iter().map(PropertyValue::summary).collect::<Vec<_>>(),
            ["A"]
        );
    }

    /// A count no run of bytes could satisfy is refused where it is read, rather than walking off
    /// the end of the export.
    #[test]
    fn an_implausible_segment_count_is_refused() {
        let mut data = 0i32.to_le_bytes().to_vec();
        data.push(200);
        let error = soft_path(&data).expect_err("refused");
        assert!(
            error.contains("implausible PropertySoftPath count"),
            "{error}"
        );
    }

    /// Every native layout has a default, and each one reads back through its own reader to
    /// exactly its length, so a slot stored from nothing decodes as the struct it declares.
    #[test]
    fn every_native_default_reads_back_to_its_own_length() {
        use retoc::legacy_asset::FPackageNameMap;
        use usmap::{Property, PropertyInner, Struct};

        use crate::mappings::Mappings;

        const NATIVE: &[&str] = &[
            "Vector",
            "Vector3f",
            "Vector2D",
            "Vector2f",
            "Vector4",
            "Vector4f",
            "Rotator",
            "Rotator3f",
            "Quat",
            "Quat4f",
            "Plane",
            "Plane4f",
            "Matrix",
            "Matrix44f",
            "IntPoint",
            "IntVector",
            "IntVector4",
            "IntVector2",
            "FrameNumber",
            "Guid",
            "Color",
            "LinearColor",
            "DateTime",
            "Timespan",
            "Box",
            "Box2D",
            "Box2f",
            "MovieSceneEventParameters",
            "Sphere",
            "RichCurveKey",
            "MovieSceneFrameRange",
            "FontCharacter",
            "FontData",
            "Transform3f",
            "PerPlatformFloat",
            "PerPlatformInt",
            "PerPlatformBool",
            "PerPlatformFrameRate",
            "ShaderValueTypeHandle",
            "SkeletalMeshSamplingLODBuiltData",
            "SkeletalMeshSamplingRegionBuiltData",
            "SerializablePropertySoftPath",
            "KeyHandleMap",
            "ClothTetherData",
            "ClothLODDataCommon",
            "MaterialOverrideNanite",
            "NiagaraVariableBase",
            "NiagaraVariable",
            "NiagaraVariableWithOffset",
            "NiagaraTypeDefinitionHandle",
            "NiagaraDataInterfaceGPUParamInfo",
            "NiagaraDataInterfaceGeneratedFunction",
            "MovieSceneFloatChannel",
            "MovieSceneDoubleChannel",
            "MovieSceneTrackIdentifier",
            "MovieSceneSequenceID",
            "MovieSceneEvaluationFieldEntityTree",
            "MovieSceneSubSequenceTree",
            "MovieSceneEvaluationKey",
            "MovieSceneEvalTemplatePtr",
            "MovieSceneTrackImplementationPtr",
            "MovieSceneSequenceInstanceDataPtr",
        ];
        let mut structs = reflected_structs();
        structs.push(Struct {
            name: "NiagaraTypeDefinition".into(),
            super_struct: None,
            properties: vec![Property {
                name: "ClassStructOrEnum".into(),
                array_dim: 1,
                index: 0,
                inner: PropertyInner::Object,
            }],
        });
        let mappings = Mappings::from_structs(structs);
        let header = FLegacyPackageHeader {
            name_map: FPackageNameMap::create_from_names(vec!["None".into()]),
            ..Default::default()
        };
        let ctx = Ctx {
            mappings: Some(&mappings),
            header: &header,
            fixups: None,
            synth: None,
            local: None,
        };
        for name in NATIVE {
            let parts = match native_default(name) {
                NativeDefault::Fixed(bytes) => vec![DefaultPart::Bytes(bytes)],
                NativeDefault::Recipe(parts) => parts,
                NativeDefault::NotNative => panic!("{name} has no default"),
            };
            let mut bytes = Vec::new();
            for part in parts {
                match part {
                    DefaultPart::Bytes(more) => bytes.extend_from_slice(&more),
                    DefaultPart::NoneName => bytes.extend_from_slice(&[0u8; 8]),
                    DefaultPart::Struct(inner) => {
                        let schema = ctx.schema(inner).expect(inner);
                        bytes.extend_from_slice(&crate::unversioned::empty_header(schema.len()));
                    }
                }
            }
            let mut diagnostics = Diagnostics::default();
            let mut cursor = Cursor::new(&bytes, 0x100);
            let value = read_native(name, &mut cursor, &ctx, &mut diagnostics, 0)
                .unwrap_or_else(|| panic!("{name} is not native"))
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(
                cursor.position(),
                bytes.len(),
                "{name} consumes its default"
            );
            assert!(
                !matches!(value, PropertyValue::Unset { .. }),
                "{name} reads back as a value"
            );
        }
    }

    /// The reflected side of the layouts that wrap a property block: the tether data declares no
    /// properties in this build, the Nanite override three.
    fn reflected_structs() -> Vec<usmap::Struct> {
        use usmap::{Property, PropertyInner, Struct};
        let property = |name: &str, index: u16, inner| Property {
            name: name.into(),
            array_dim: 1,
            index,
            inner,
        };
        vec![
            Struct {
                name: "ClothTetherData".into(),
                super_struct: None,
                properties: Vec::new(),
            },
            Struct {
                name: "ClothLODDataCommon".into(),
                super_struct: None,
                properties: vec![property("SkinningKernelRadius", 0, PropertyInner::Float)],
            },
            Struct {
                name: "MaterialOverrideNanite".into(),
                super_struct: None,
                properties: vec![
                    property("bEnableOverride", 0, PropertyInner::Bool),
                    property("OverrideMaterial", 1, PropertyInner::Object),
                    property("OverrideMaterialRef", 2, PropertyInner::SoftObject),
                ],
            },
        ]
    }

    fn ctx_with_schemas<'a>(
        mappings: &'a crate::mappings::Mappings,
        header: &'a FLegacyPackageHeader,
    ) -> Ctx<'a> {
        Ctx {
            mappings: Some(mappings),
            header,
            fixups: None,
            synth: None,
            local: None,
        }
    }

    /// `FClothTetherData` writes its empty reflected block, then batches of tethers; both lists
    /// are recorded so a batch or a tether can be added.
    #[test]
    fn cloth_tether_data_reads_its_reflected_block_then_its_tether_batches() {
        let mut data = vec![0x00, 0x01];
        data.extend_from_slice(&2i32.to_le_bytes());
        data.extend_from_slice(&1i32.to_le_bytes());
        data.extend_from_slice(&7i32.to_le_bytes());
        data.extend_from_slice(&9i32.to_le_bytes());
        data.extend_from_slice(&1.5f32.to_le_bytes());
        data.extend_from_slice(&0i32.to_le_bytes());
        let mappings = crate::mappings::Mappings::from_structs(reflected_structs());
        let header = FLegacyPackageHeader::default();
        let ctx = ctx_with_schemas(&mappings, &header);
        let mut diagnostics = Diagnostics::default();
        let mut cursor = Cursor::new(&data, 0);
        let value = read_native("ClothTetherData", &mut cursor, &ctx, &mut diagnostics, 0)
            .expect("native")
            .expect("reads");
        assert_eq!(cursor.remaining(), 0);
        let fields = fields_of(value);
        let tethers = fields
            .iter()
            .find(|f| f.name == "Tethers")
            .expect("tethers");
        let PropertyValue::Array { items } = &tethers.value else {
            panic!("tethers are a list");
        };
        assert_eq!(items.len(), 2);
        let PropertyValue::Array { items: batch } = &items[0] else {
            panic!("a batch is a list");
        };
        let PropertyValue::Struct { fields: tether, .. } = &batch[0] else {
            panic!("a tether is a struct");
        };
        assert!(matches!(tether[0].value, PropertyValue::Int { value: 7 }));
        assert!(matches!(tether[1].value, PropertyValue::Int { value: 9 }));
        assert!(matches!(tether[2].value, PropertyValue::Float { value } if value == 1.5));
        assert_eq!(diagnostics.containers.len(), 3);
    }

    /// A cloth LOD writes its reflected block, then the two transition skinning lists raw: one
    /// 64-byte record per entry, the four source vertices as a static array.
    #[test]
    fn a_cloth_lod_reads_its_transition_skinning_data_after_its_reflected_block() {
        let mut data = vec![0x01, 0x01]; // the one declared slot, skipped
        data.extend_from_slice(&1i32.to_le_bytes());
        for value in 1..=12 {
            data.extend_from_slice(&(value as f32).to_le_bytes());
        }
        for index in [7u16, 8, 9, 10] {
            data.extend_from_slice(&index.to_le_bytes());
        }
        data.extend_from_slice(&0.25f32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&0i32.to_le_bytes());
        let mappings = crate::mappings::Mappings::from_structs(reflected_structs());
        let header = FLegacyPackageHeader::default();
        let ctx = ctx_with_schemas(&mappings, &header);
        let mut diagnostics = Diagnostics::default();
        let mut cursor = Cursor::new(&data, 0);
        let value = read_native("ClothLODDataCommon", &mut cursor, &ctx, &mut diagnostics, 0)
            .expect("native")
            .expect("reads");
        assert_eq!(cursor.remaining(), 0);
        let fields = fields_of(value);
        let up = fields
            .iter()
            .find(|f| f.name == "TransitionUpSkinData")
            .expect("up");
        let PropertyValue::Array { items } = &up.value else {
            panic!("a list");
        };
        let PropertyValue::Struct { fields: record, .. } = &items[0] else {
            panic!("a record");
        };
        assert_eq!(record.len(), 3 + 4 + 2);
        assert_eq!(record[3].element, Some(0));
        assert!(matches!(record[6].value, PropertyValue::UInt { value: 10 }));
        assert!(matches!(record[7].value, PropertyValue::Float { value } if value == 0.25));
        assert_eq!(diagnostics.containers.len(), 2);
    }

    /// A Nanite override is cooked as a flag, a hard reference and the reflected block with every
    /// slot skipped; a flag other than one is a layout this reader does not know.
    #[test]
    fn a_nanite_override_reads_its_cooked_reference_then_its_reflected_block() {
        let mut data = 1u32.to_le_bytes().to_vec();
        data.extend_from_slice(&0i32.to_le_bytes());
        data.extend_from_slice(&[0x03, 0x01]);
        let mappings = crate::mappings::Mappings::from_structs(reflected_structs());
        let header = FLegacyPackageHeader::default();
        let ctx = ctx_with_schemas(&mappings, &header);
        let mut diagnostics = Diagnostics::default();
        let mut cursor = Cursor::new(&data, 0);
        let value = read_native(
            "MaterialOverrideNanite",
            &mut cursor,
            &ctx,
            &mut diagnostics,
            0,
        )
        .expect("native")
        .expect("reads");
        assert_eq!(cursor.remaining(), 0);
        let fields = fields_of(value);
        assert_eq!(fields[0].name, "bCooked");
        assert!(matches!(
            fields[1].value,
            PropertyValue::Object { index: 0, .. }
        ));
        assert_eq!(fields[1].span, Some((4, 8)));

        data[0] = 0;
        let mut cursor = Cursor::new(&data, 0);
        let error = read_native(
            "MaterialOverrideNanite",
            &mut cursor,
            &ctx,
            &mut diagnostics,
            0,
        )
        .expect("native")
        .expect_err("refused");
        assert!(error.contains("cooked flag reads 0"), "{error}");
    }

    /// The region's bone list sits between the triangles and the sampler, and the vertices come
    /// last; the map of key handles occupies no bytes at all.
    #[test]
    fn a_sampling_region_orders_its_lists_around_the_sampler_and_a_key_handle_map_is_empty() {
        let mut data = Vec::new();
        for word in [1i32, 1, 2, 2, 3, 0, 0] {
            data.extend_from_slice(&word.to_le_bytes());
        }
        data.extend_from_slice(&0.5f32.to_le_bytes());
        for word in [1i32, 4] {
            data.extend_from_slice(&word.to_le_bytes());
        }
        let header = FLegacyPackageHeader::default();
        let ctx = ctx_over(&header);
        let mut diagnostics = Diagnostics::default();
        let mut cursor = Cursor::new(&data, 0);
        let value = read_native(
            "SkeletalMeshSamplingRegionBuiltData",
            &mut cursor,
            &ctx,
            &mut diagnostics,
            0,
        )
        .expect("native")
        .expect("reads");
        assert_eq!(cursor.remaining(), 0);
        let fields = fields_of(value);
        let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "TriangleIndices",
                "BoneIndices",
                "AreaWeightedSampler",
                "Vertices"
            ]
        );
        assert!(matches!(
            &fields[3].value,
            PropertyValue::Array { items } if matches!(items[0], PropertyValue::Int { value: 4 })
        ));

        let mut cursor = Cursor::new(&data, 0);
        read_native("KeyHandleMap", &mut cursor, &ctx, &mut diagnostics, 0)
            .expect("native")
            .expect("reads");
        assert_eq!(cursor.position(), 0);
    }

    /// `TPerPlatformProperty` writes its cooked flag as a full word. Read one byte wide it left
    /// three bytes over, which a following float swallowed as a denormal.
    #[test]
    fn a_per_platform_property_writes_its_cooked_flag_as_a_word() {
        let mut data = 1u32.to_le_bytes().to_vec();
        data.extend_from_slice(&1.0f32.to_le_bytes());
        let mut cursor = Cursor::new(&data, 0);
        let fields = fields_of(
            per_platform(&mut cursor, "PerPlatformFloat", PerPlatform::Float).expect("float"),
        );
        assert_eq!(cursor.remaining(), 0);
        assert!(matches!(
            fields[0].value,
            PropertyValue::Bool { value: true }
        ));
        assert_eq!(fields[0].span, Some((0, 4)));
        assert!(matches!(fields[1].value, PropertyValue::Float { value } if value == 1.0));

        let mut data = 1u32.to_le_bytes().to_vec();
        data.extend_from_slice(&60i32.to_le_bytes());
        data.extend_from_slice(&1i32.to_le_bytes());
        let mut cursor = Cursor::new(&data, 0);
        let fields = fields_of(
            per_platform(&mut cursor, "PerPlatformFrameRate", PerPlatform::FrameRate)
                .expect("rate"),
        );
        assert_eq!(cursor.remaining(), 0);
        let rate = fields_of(fields[1].value.clone());
        assert!(matches!(rate[0].value, PropertyValue::Int { value: 60 }));
        assert!(matches!(rate[1].value, PropertyValue::Int { value: 1 }));
    }

    #[test]
    fn a_float_transform_is_forty_bytes_without_a_header() {
        let data = identity_transform3f();
        let mut cursor = Cursor::new(&data, 0x10);
        let fields = fields_of(transform3f(&mut cursor, "Transform3f").expect("transform"));
        assert_eq!(cursor.remaining(), 0);
        assert_eq!(fields[0].span, Some((0x10, 0x20)));
        assert_eq!(fields[2].span, Some((0x2C, 0x38)));
        let scale = fields_of(fields[2].value.clone());
        assert!(matches!(scale[2].value, PropertyValue::Float { value } if value == 1.0));
    }

    #[test]
    fn a_font_character_is_twenty_one_bytes() {
        let mut data = Vec::new();
        for value in [235i32, 6, 6, 26] {
            data.extend_from_slice(&value.to_le_bytes());
        }
        data.push(2);
        data.extend_from_slice(&(-3i32).to_le_bytes());
        let mut cursor = Cursor::new(&data, 0);
        let fields = fields_of(font_character(&mut cursor, "FontCharacter").expect("character"));
        assert_eq!(cursor.remaining(), 0);
        assert!(matches!(fields[4].value, PropertyValue::Byte { value: 2 }));
        assert!(matches!(fields[5].value, PropertyValue::Int { value: -3 }));
    }

    #[test]
    fn cooked_font_data_is_a_flag_an_asset_and_a_sub_face() {
        let mut data = 1u32.to_le_bytes().to_vec();
        data.extend_from_slice(&0i32.to_le_bytes());
        data.extend_from_slice(&2i32.to_le_bytes());
        let header = FLegacyPackageHeader::default();
        let ctx = ctx_over(&header);
        let mut diagnostics = Diagnostics::default();
        let mut cursor = Cursor::new(&data, 0);
        let fields = fields_of(font_data(&mut cursor, &ctx, &mut diagnostics).expect("font data"));
        assert_eq!(cursor.remaining(), 0);
        assert_eq!(fields[1].name, "FontFaceAsset");
        assert!(matches!(fields[2].value, PropertyValue::Int { value: 2 }));
    }

    #[test]
    fn a_weighted_sampler_is_two_arrays_and_a_total() {
        let mut data = 2i32.to_le_bytes().to_vec();
        data.extend_from_slice(&0.5f32.to_le_bytes());
        data.extend_from_slice(&0.5f32.to_le_bytes());
        data.extend_from_slice(&2i32.to_le_bytes());
        data.extend_from_slice(&1i32.to_le_bytes());
        data.extend_from_slice(&0i32.to_le_bytes());
        data.extend_from_slice(&1.0f32.to_le_bytes());
        let header = FLegacyPackageHeader::default();
        let ctx = ctx_over(&header);
        let mut diagnostics = Diagnostics::default();
        let mut cursor = Cursor::new(&data, 0);
        let fields = fields_of(
            weighted_sampler(&mut cursor, &ctx, &mut diagnostics, "Sampler").expect("sampler"),
        );
        assert_eq!(cursor.remaining(), 0);
        assert!(matches!(&fields[0].value, PropertyValue::Array { items } if items.len() == 2));
        assert_eq!(fields[1].span, Some((12, 24)));
        assert_eq!(diagnostics.containers.len(), 2, "both arrays can grow");
        assert!(matches!(fields[2].value, PropertyValue::Float { value } if value == 1.0));
    }

    /// The sizes measured in a deformer graph: `int3`, `float4x4` and `uint`, each ending where the
    /// next parameter's header began.
    #[test]
    fn a_shader_value_type_carries_only_the_size_its_dimension_needs() {
        let header = FLegacyPackageHeader::default();
        let ctx = ctx_over(&header);
        let cases: [(&[u8], usize); 3] = [
            (&[1, 0, 0, 0, 0, 1, 3], 4),
            (&[3, 0, 0, 0, 0, 2, 4, 4], 5),
            (&[2, 0, 0, 0, 0, 0], 3),
        ];
        for (bytes, field_count) in cases {
            let mut cursor = Cursor::new(bytes, 0);
            let fields = fields_of(shader_value_type(&mut cursor, &ctx).expect("value type"));
            assert_eq!(cursor.remaining(), 0, "{bytes:?}");
            assert_eq!(fields.len(), field_count, "{bytes:?}");
        }
        let mut cursor = Cursor::new(&[4, 0, 0, 0, 0, 0], 0);
        assert!(shader_value_type(&mut cursor, &ctx).is_err());
    }

    /// The mappings declare sixteen bitfield bools, but they share one word that is not a
    /// property of its own, and that word is all `FNavAgentSelector::Serialize` writes.
    #[test]
    fn a_nav_agent_selector_is_one_packed_word_not_sixteen_bools() {
        let data = 0b1001u32.to_le_bytes();
        let mut cursor = Cursor::new(&data, 0);
        let value = nav_agent_selector(&mut cursor).expect("selector");
        assert!(cursor.remaining() == 0);
        let PropertyValue::Struct { fields, .. } = value else {
            panic!("expected a struct");
        };
        assert_eq!(fields.len(), 16);
        let set: Vec<_> = fields
            .iter()
            .filter(|f| matches!(f.value, PropertyValue::Bool { value: true }))
            .map(|f| f.name.clone())
            .collect();
        assert_eq!(set, ["bSupportsAgent0", "bSupportsAgent3"]);
    }

    #[test]
    fn a_vector_reads_as_three_doubles_under_large_world_coordinates() {
        let mut data = Vec::new();
        for value in [1.0f64, 2.0, 3.0] {
            data.extend_from_slice(&value.to_le_bytes());
        }
        let mut cursor = Cursor::new(&data, 0);
        let value = doubles(&mut cursor, "Vector", &["X", "Y", "Z"]).expect("vector");
        assert_eq!(value.summary(), "Vector(1.0, 2.0, 3.0)");
        assert!(cursor.remaining() == 0, "a UE5 vector is 24 bytes, not 12");
    }

    #[test]
    fn a_colour_reads_in_bgra_order() {
        let data = [1u8, 2, 3, 4];
        let mut cursor = Cursor::new(&data, 0);
        let value = bytes(&mut cursor, "Color", &["B", "G", "R", "A"]).expect("color");
        let PropertyValue::Struct { fields, .. } = value else {
            panic!("expected a struct");
        };
        assert_eq!(fields[0].name, "B");
        assert_eq!(fields[2].name, "R");
    }

    /// Every default in the table has to be exactly what the matching layout reads, or storing a
    /// struct from nothing would desync everything after it.
    #[test]
    fn every_native_default_is_read_back_whole_by_its_layout() {
        use crate::props::{Ctx, Diagnostics};
        use retoc::legacy_asset::{FLegacyPackageHeader, FPackageNameMap};

        let header = FLegacyPackageHeader {
            name_map: FPackageNameMap::create_from_names(vec!["None".into()]),
            ..Default::default()
        };
        let ctx = Ctx {
            mappings: None,
            header: &header,
            fixups: None,
            synth: None,
            local: None,
        };
        for name in [
            "Vector",
            "Vector3f",
            "Vector2D",
            "Vector2f",
            "Vector4",
            "Vector4f",
            "Rotator",
            "Rotator3f",
            "Quat",
            "Quat4f",
            "Plane",
            "Plane4f",
            "Matrix",
            "Matrix44f",
            "IntPoint",
            "IntVector",
            "IntVector4",
            "Guid",
            "Color",
            "LinearColor",
            "DateTime",
            "Timespan",
            "Box",
            "Box2D",
            "Sphere",
            "NavAgentSelector",
            "GameplayTagContainer",
            "MarvelSoftObjectPath",
            "SerializablePropertySoftPath",
            "RichCurveKey",
            "FrameNumber",
            "MovieSceneFrameRange",
            "PerPlatformFloat",
            "PerPlatformInt",
            "PerPlatformBool",
        ] {
            let NativeDefault::Fixed(bytes) = native_default(name) else {
                panic!("{name} has no fixed default");
            };
            let mut cursor = Cursor::new(&bytes, 0);
            let mut diagnostics = Diagnostics::default();
            read_native(name, &mut cursor, &ctx, &mut diagnostics, 0)
                .expect(name)
                .expect(name);
            assert_eq!(cursor.remaining(), 0, "{name} default is the wrong width");
        }
    }

    #[test]
    fn an_asset_path_omits_empty_and_none_segments() {
        assert_eq!(join_asset_path("None", "None", ""), "");
        assert_eq!(join_asset_path("/Game/A", "A", ""), "/Game/A.A");
        assert_eq!(join_asset_path("/Game/A", "A", "Sub"), "/Game/A.A:Sub");
    }
}
