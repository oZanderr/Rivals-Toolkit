//! Accounts for the bytes a class writes after its properties, so a leftover tail stops being
//! ambiguous evidence of a desync.
//!
//! Two mechanisms. Small tails are read outright, walking the class chain in `Super::Serialize`
//! order so an export can reach `Complete`. Large payloads (texture mips, shader maps, mesh
//! render data) are not decoded at all; they are named and measured, which is enough to tell an
//! expected remainder from a real parse failure.

use crate::package::is_unresolved_import_name;
use crate::props::{Ctx, Diagnostics, read_index};
use crate::reader::Cursor;
use crate::value::{PropertyEntry, PropertyValue};

/// A class whose remaining bytes are a known bulk payload, matched against the class chain.
struct Payload {
    class: &'static str,
    kind: &'static str,
}

/// Ordered most specific first so a subclass can claim a better description than its base.
const PAYLOADS: &[Payload] = &[
    Payload {
        class: "Texture",
        kind: "texture mip data",
    },
    Payload {
        class: "SoundWave",
        kind: "audio data",
    },
    Payload {
        class: "StaticMesh",
        kind: "mesh render data",
    },
    Payload {
        class: "SkeletalMesh",
        kind: "mesh render data",
    },
    Payload {
        class: "MaterialInterface",
        kind: "shader maps",
    },
    Payload {
        class: "AnimSequenceBase",
        kind: "compressed animation data",
    },
    // The class walks to its end and keeps its definition; the VM serialized after it is the rest.
    Payload {
        class: "RigVMBlueprintGeneratedClass",
        kind: "rig VM class data",
    },
    Payload {
        class: "Class",
        kind: "class layout and bytecode",
    },
    // A function whose layout walk failed; one that walked names its bytecode alone, measured.
    Payload {
        class: "Function",
        kind: "function layout and bytecode",
    },
    Payload {
        class: "Level",
        kind: "level data",
    },
    Payload {
        class: "World",
        kind: "level data",
    },
    Payload {
        class: "Model",
        kind: "geometry data",
    },
    Payload {
        class: "ModelComponent",
        kind: "model geometry data",
    },
    Payload {
        class: "BodySetup",
        kind: "collision data",
    },
    Payload {
        class: "NavCollisionBase",
        kind: "navigation collision data",
    },
    Payload {
        class: "NiagaraScript",
        kind: "particle system data",
    },
    Payload {
        class: "NiagaraSystem",
        kind: "particle system data",
    },
    Payload {
        class: "NiagaraEmitter",
        kind: "particle system data",
    },
    // This game's ability timelines carry their tracks in a serializer of their own.
    Payload {
        class: "AnimTimeline",
        kind: "animation timeline data",
    },
    Payload {
        class: "InstancedStaticMeshComponent",
        kind: "instance transform data",
    },
    Payload {
        class: "LandscapeComponent",
        kind: "landscape data",
    },
    Payload {
        class: "LandscapeHeightfieldCollisionComponent",
        kind: "landscape collision data",
    },
    Payload {
        class: "MorphTarget",
        kind: "morph target data",
    },
    Payload {
        class: "FontFace",
        kind: "font data",
    },
    Payload {
        class: "BlendSpace",
        kind: "blend space sample data",
    },
    Payload {
        class: "Skeleton",
        kind: "skeleton data",
    },
    Payload {
        class: "NavigationData",
        kind: "navigation data",
    },
    Payload {
        class: "PhysicsAsset",
        kind: "collision table",
    },
    Payload {
        class: "SVONVolume",
        kind: "navigation octree data",
    },
    Payload {
        class: "MapBuildDataRegistry",
        kind: "map build data",
    },
    // Wwise events keep everything but an all-skipped property header in their cooked data.
    Payload {
        class: "AkAudioEvent",
        kind: "Wwise event data",
    },
    Payload {
        class: "VectorFieldStatic",
        kind: "vector field data",
    },
    Payload {
        class: "OptimusComputeGraph",
        kind: "compute graph data",
    },
    Payload {
        class: "GeometryCacheTrack",
        kind: "geometry cache data",
    },
    Payload {
        class: "NavigationDataChunk",
        kind: "navigation mesh data",
    },
    Payload {
        class: "BaseMediaSource",
        kind: "media source data",
    },
    Payload {
        class: "RigVMMemoryStorage",
        kind: "rig VM memory data",
    },
    Payload {
        class: "GeometryCollection",
        kind: "geometry collection data",
    },
    // Every other Wwise object keeps cooked data of its own past the properties, as the event does.
    Payload {
        class: "AkAudioType",
        kind: "Wwise audio data",
    },
    Payload {
        class: "DatasmithScene",
        kind: "Datasmith scene data",
    },
    Payload {
        class: "GoatTurnCtrlRigBakedData",
        kind: "baked control rig data",
    },
    // Strip flags, then the baked transforms of every animation in the collection.
    Payload {
        class: "SkelotAnimCollection",
        kind: "Skelot animation data",
    },
];

/// `FSimpleMemberReference`: the owning class, the property name and a guid.
const UCS_RECORD_BYTES: usize = 4 + 8 + 16;

/// `FVector2f`.
const CUTOUT_VERTEX_BYTES: usize = 8;

/// `TPair<FName, int64>`: an enumerator's name and value.
const ENUMERATOR_BYTES: usize = 8 + 8;

/// Names the bulk payload a class is expected to store after its properties, if any. `chain` is
/// the class ancestry, root first.
/// Classes whose `Serialize` never enters the property system, so the whole export is theirs.
const OPAQUE: &[Payload] = &[
    Payload {
        class: "RigHierarchy",
        kind: "rig hierarchy data",
    },
    Payload {
        class: "RigVM",
        kind: "rig VM data",
    },
    Payload {
        class: "RigVMMemoryStorage",
        kind: "rig VM memory data",
    },
    Payload {
        class: "MarvelVehicleAnimBakedData",
        kind: "vehicle animation baked data",
    },
];

/// What an export is when its property block cannot be read at all: an instance of a class whose
/// import retoc could not resolve (a native class this build no longer ships, or an export hash no
/// package answers to), an instance of a class descending from one, or one of the classes in
/// [`OPAQUE`].
pub(crate) fn opaque_kind(
    class_name: &str,
    chain: &[String],
    missing_ancestor: Option<&str>,
) -> Option<&'static str> {
    if is_unresolved_import_name(class_name)
        || missing_ancestor.is_some_and(is_unresolved_import_name)
    {
        return Some("instance of a class this build does not have");
    }
    OPAQUE
        .iter()
        .find(|payload| chain.iter().any(|step| step == payload.class))
        .map(|payload| payload.kind)
}

/// An object reference a class writes after its properties, on record for renumbering and shown
/// like one inside them.
fn object_entry(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    name: &str,
) -> Result<PropertyEntry, String> {
    let start = cursor.file_offset();
    let index = read_index(cursor, diagnostics)?;
    Ok(PropertyEntry {
        name: name.to_string(),
        element: None,
        value: PropertyValue::Object {
            index,
            path: ctx.object_path(index).map_err(|e| cursor.err(e))?,
        },
        span: Some((start, cursor.file_offset())),
        slot: None,
    })
}

/// The chain steps that write anything after an object's properties, in the order they write it.
///
/// Retyping an object keeps the bytes past its property block, so it is only safe between two
/// classes whose chains write the same things: the old tail is still there and the new class has
/// to read exactly it.
pub(crate) fn tail_steps(chain: &[&str]) -> Vec<&'static str> {
    const WRITERS: &[&str] = &[
        "Actor",
        "LevelInstance",
        "ActorComponent",
        "StaticMeshComponent",
        "NiagaraRendererProperties",
        "NiagaraSpriteRendererProperties",
        "Enum",
        "Font",
        "InstancedFoliageActor",
        "NiagaraDataInterfaceTexture",
        "SkyAtmosphereComponent",
        "AnimCurveCompressionCodec",
        "AnimationAsset",
        "SoundNode",
        "SoundCue",
        "SoundNodeWavePlayer",
        "Rig",
        "Level",
    ];
    chain
        .iter()
        .filter_map(|step| WRITERS.iter().find(|held| *held == step).copied())
        .collect()
}

pub(crate) fn payload_kind(chain: &[String]) -> Option<&'static str> {
    PAYLOADS
        .iter()
        .find(|payload| chain.iter().any(|step| step == payload.class))
        .map(|payload| payload.kind)
}

pub(crate) enum TailOutcome {
    /// Every trailing byte the class writes was read.
    Consumed,
    /// The tail runs into bulk data that is named and measured rather than decoded.
    Payload(&'static str),
}

/// Reads the tails the class chain is known to write, base class first, exactly as
/// `Super::Serialize` would. Returns an error the moment a step cannot be accounted for, which
/// leaves the export reported as `Partial` rather than falsely `Complete`. `chain` is the class
/// ancestry, root first. A tail holding a value worth showing or editing lands in `fields`.
pub(crate) fn read_class_tail(
    chain: &[String],
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    fields: &mut Vec<PropertyEntry>,
) -> Result<TailOutcome, String> {
    let sprite = chain
        .iter()
        .any(|step| step == "NiagaraSpriteRendererProperties");
    for step in chain {
        let outcome = match step.as_str() {
            "Actor" => {
                read_actor_label_tail(cursor)?;
                TailOutcome::Consumed
            }
            // Level instances follow the label with the guid of the level they stream in.
            "LevelInstance" => {
                cursor.skip(16)?;
                TailOutcome::Consumed
            }
            "ActorComponent" => {
                read_actor_component_tail(cursor, ctx, diagnostics)?;
                TailOutcome::Consumed
            }
            "StaticMeshComponent" => read_static_mesh_component_tail(cursor)?,
            // Every renderer closes with one word that has read zero in every export inspected,
            // except that a sprite renderer's word is the count of its cutout geometry.
            "NiagaraRendererProperties" if !sprite => {
                read_zero_word(cursor, "Niagara renderer trailer")?;
                TailOutcome::Consumed
            }
            "NiagaraSpriteRendererProperties" => {
                read_cutout_geometry(cursor)?;
                TailOutcome::Consumed
            }
            "Enum" => {
                read_enum_tail(cursor)?;
                TailOutcome::Consumed
            }
            // Every font closes with one word that has read zero in every font inspected.
            "Font" => {
                read_zero_word(cursor, "font trailer")?;
                TailOutcome::Consumed
            }
            // `FoliageInfos`, a map of foliage type to instances, which is bulk data once it holds
            // anything.
            "InstancedFoliageActor" => read_counted_payload(cursor, "foliage instance data")?,
            // `StreamData`, the texture bytes the interface streams to the GPU.
            "NiagaraDataInterfaceTexture" => read_counted_payload(cursor, "texture stream data")?,
            // The guid of the static lighting build the component was cooked against.
            "SkyAtmosphereComponent" => {
                cursor.skip(16)?;
                TailOutcome::Consumed
            }
            // `InstanceGuid`, which identifies the codec the curves were compressed with.
            "AnimCurveCompressionCodec" => {
                cursor.skip(16)?;
                TailOutcome::Consumed
            }
            // `UAnimationAsset::Serialize` closes with the guid of the skeleton the asset was
            // built for, ahead of whatever compressed data a subclass adds.
            "AnimationAsset" => {
                fields.push(crate::structs::guid_entry(
                    cursor,
                    diagnostics,
                    "SkeletonGuid",
                )?);
                TailOutcome::Consumed
            }
            // `FStripDataFlags`, the two bytes that open a sound node's or a cue's cooked data.
            "SoundNode" | "SoundCue" => {
                cursor.skip(2)?;
                TailOutcome::Consumed
            }
            // A cooked player names its wave as a hard reference, so the wave loads with the cue.
            "SoundNodeWavePlayer" => {
                fields.push(object_entry(cursor, ctx, diagnostics, "SoundWave")?);
                TailOutcome::Consumed
            }
            // `ULevel::Serialize`: the actor list, the URL the level was cooked from, its model
            // and components, and the script actor. What follows is precomputed data.
            "Level" => {
                read_level_tail(cursor, ctx, diagnostics, fields)?;
                TailOutcome::Payload("level data")
            }
            // Three words that have read zero in every rig inspected.
            "Rig" => {
                for _ in 0..3 {
                    read_zero_word(cursor, "rig trailer")?;
                }
                TailOutcome::Consumed
            }
            _ => TailOutcome::Consumed,
        };
        if let TailOutcome::Payload(kind) = outcome {
            return Ok(TailOutcome::Payload(kind));
        }
    }
    Ok(TailOutcome::Consumed)
}

/// `ULevel::Serialize` after the properties: `Actors`, the `FURL` the level was cooked from,
/// `Model`, `ModelComponents`, `LevelScriptActor`, and the two navigation bounds. Everything past
/// that is precomputed visibility and distance field data, which is measured rather than decoded.
///
/// Every index here is a real reference: an export removed or renumbered without rewriting the
/// actor list leaves the level naming whatever now sits at that number.
fn read_level_tail(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    fields: &mut Vec<PropertyEntry>,
) -> Result<(), String> {
    fields.push(object_list(cursor, ctx, diagnostics, "Actors")?);
    fields.push(read_url(cursor)?);
    fields.push(object_entry(cursor, ctx, diagnostics, "Model")?);
    fields.push(object_list(cursor, ctx, diagnostics, "ModelComponents")?);
    fields.push(object_entry(cursor, ctx, diagnostics, "LevelScriptActor")?);
    fields.push(object_entry(cursor, ctx, diagnostics, "NavListStart")?);
    fields.push(object_entry(cursor, ctx, diagnostics, "NavListEnd")?);
    Ok(())
}

/// A counted list of object references, recorded as a container so an element can be added or
/// dropped, and as references so a renumber follows them.
fn object_list(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    name: &str,
) -> Result<PropertyEntry, String> {
    let at = cursor.file_offset();
    let count = cursor.read_i32()?;
    let fits = usize::try_from(count)
        .ok()
        .and_then(|n| n.checked_mul(4))
        .is_some_and(|bytes| bytes <= cursor.remaining());
    if !fits {
        return Err(cursor.err(format!("implausible {name} count {count}")));
    }
    let mut items = Vec::with_capacity(count as usize);
    let mut elements = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let start = cursor.file_offset();
        let index = read_index(cursor, diagnostics)?;
        items.push(PropertyValue::Object {
            index,
            path: ctx.object_path(index).map_err(|e| cursor.err(e))?,
        });
        elements.push((start, cursor.file_offset()));
    }
    crate::props::record_container(
        diagnostics,
        ctx,
        at,
        at,
        elements,
        &usmap::PropertyInner::Object,
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

/// `FURL::Serialize`: the address the level was cooked from. Kept as a struct rather than skipped
/// so a level that names another map is visible.
fn read_url(cursor: &mut Cursor<'_>) -> Result<PropertyEntry, String> {
    let at = cursor.file_offset();
    let mut fields = Vec::with_capacity(7);
    let mut string_field = |cursor: &mut Cursor<'_>, name: &str| -> Result<(), String> {
        let start = cursor.file_offset();
        let value = cursor.read_string()?;
        fields.push(PropertyEntry {
            name: name.to_string(),
            element: None,
            value: PropertyValue::Str { value },
            span: Some((start, cursor.file_offset())),
            slot: None,
        });
        Ok(())
    };
    for name in ["Protocol", "Host", "Map", "Portal"] {
        string_field(cursor, name)?;
    }
    let ops_at = cursor.file_offset();
    let ops = cursor.read_i32()?;
    let fits = usize::try_from(ops)
        .ok()
        .and_then(|n| n.checked_mul(4))
        .is_some_and(|bytes| bytes <= cursor.remaining());
    if !fits {
        return Err(cursor.err(format!("implausible URL option count {ops}")));
    }
    let mut items = Vec::with_capacity(ops as usize);
    for _ in 0..ops {
        items.push(PropertyValue::Str {
            value: cursor.read_string()?,
        });
    }
    fields.push(PropertyEntry {
        name: "Op".to_string(),
        element: None,
        value: PropertyValue::Array { items },
        span: Some((ops_at, cursor.file_offset())),
        slot: None,
    });
    for name in ["Port", "Valid"] {
        let start = cursor.file_offset();
        let value = i64::from(cursor.read_i32()?);
        fields.push(PropertyEntry {
            name: name.to_string(),
            element: None,
            value: PropertyValue::Int { value },
            span: Some((start, cursor.file_offset())),
            slot: None,
        });
    }
    Ok(PropertyEntry {
        name: "URL".to_string(),
        element: None,
        value: PropertyValue::Struct {
            name: "URL".to_string(),
            fields,
        },
        span: Some((at, cursor.file_offset())),
        slot: None,
    })
}

/// This game's engine follows every actor with a word of 1 and the actor's editor label, which
/// stock UE never cooks. The word is checked rather than assumed so a different layout stays visible
/// as unexplained bytes.
fn read_actor_label_tail(cursor: &mut Cursor<'_>) -> Result<(), String> {
    match cursor.peek_u32() {
        Some(1) => {
            cursor.skip(4)?;
            cursor.read_string()?;
            Ok(())
        }
        Some(other) => Err(cursor.err(format!(
            "actor trailer starts with {other:#X} rather than the 1 always seen so far"
        ))),
        None => Err(cursor.err("actor trailer is missing")),
    }
}

/// `UActorComponent::Serialize` writes `UCSModifiedProperties`: the properties a construction
/// script changed, each as the class that declares it, the property name and a guid. The class is
/// an object reference, so it is recorded for edits that renumber exports.
fn read_actor_component_tail(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
) -> Result<(), String> {
    let count = cursor.read_i32()?;
    if count < 0 || (count as usize).saturating_mul(UCS_RECORD_BYTES) > cursor.remaining() {
        return Err(cursor.err(format!("implausible UCSModifiedProperties count {count}")));
    }
    for _ in 0..count {
        read_index(cursor, diagnostics)?;
        // Read rather than skipped: a copy into another package rewrites every name it holds, and
        // a name stepped over here would keep pointing into the source package's map.
        cursor.read_name(&ctx.header.name_map)?;
        cursor.skip(UCS_RECORD_BYTES - 4 - 8)?;
    }
    Ok(())
}

/// `UNiagaraSpriteRendererProperties` writes the bounding vertices of its sub-image cutout,
/// `TArray<FVector2f>` with eight per frame, where the other renderers write their zero word. A
/// renderer without a cutout writes a count of zero. The count has to account for every remaining
/// byte, which is what keeps a wrong reading visible.
fn read_cutout_geometry(cursor: &mut Cursor<'_>) -> Result<(), String> {
    let count = cursor.read_i32()?;
    let bytes = usize::try_from(count)
        .ok()
        .and_then(|count| count.checked_mul(CUTOUT_VERTEX_BYTES));
    match bytes {
        Some(bytes) if bytes == cursor.remaining() => cursor.skip(bytes),
        _ => Err(cursor.err(format!(
            "sprite cutout geometry declares {count} vertices for {} bytes",
            cursor.remaining()
        ))),
    }
}

/// A count that opens bulk data: zero is consumed outright, anything else names the payload rather
/// than decoding it.
fn read_counted_payload(
    cursor: &mut Cursor<'_>,
    kind: &'static str,
) -> Result<TailOutcome, String> {
    let count = cursor.read_i32()?;
    if count < 0 {
        return Err(cursor.err(format!("negative {kind} count {count}")));
    }
    Ok(if count == 0 {
        TailOutcome::Consumed
    } else {
        TailOutcome::Payload(kind)
    })
}

/// `UEnum::Serialize`: the enumerators as name and value pairs, then the `ECppForm` byte. This
/// build writes no enum flags after it, which the export's declared size confirms.
fn read_enum_tail(cursor: &mut Cursor<'_>) -> Result<(), String> {
    let count = cursor.read_i32()?;
    if count < 0 || (count as usize).saturating_mul(ENUMERATOR_BYTES) > cursor.remaining() {
        return Err(cursor.err(format!("implausible enumerator count {count}")));
    }
    cursor.skip(count as usize * ENUMERATOR_BYTES)?;
    cursor.skip(1)
}

/// A trailer this reader has only ever seen as zero, consumed only while that holds so a different
/// layout stays visible as unexplained bytes.
fn read_zero_word(cursor: &mut Cursor<'_>, what: &str) -> Result<(), String> {
    match cursor.peek_u32() {
        Some(0) => cursor.skip(4),
        Some(other) => Err(cursor.err(format!(
            "{what} is {other:#X} rather than the zero word always seen so far"
        ))),
        None => Err(cursor.err(format!("{what} is missing"))),
    }
}

/// `UStaticMeshComponent::Serialize` writes `TArray<FStaticMeshComponentLODInfo> LODData`. Each
/// entry carries painted vertex colours and build data, which is bulk data rather than properties.
fn read_static_mesh_component_tail(cursor: &mut Cursor<'_>) -> Result<TailOutcome, String> {
    let count = cursor.read_i32()?;
    if count < 0 {
        return Err(cursor.err(format!("negative static mesh LOD count {count}")));
    }
    Ok(if count == 0 {
        TailOutcome::Consumed
    } else {
        TailOutcome::Payload("static mesh LOD override data")
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use retoc::legacy_asset::FLegacyPackageHeader;

    use super::*;
    use crate::mappings::Mappings;

    fn label(bytes: &[u8]) -> Vec<u8> {
        let mut data = 1i32.to_le_bytes().to_vec();
        data.extend_from_slice(bytes);
        data
    }

    #[test]
    fn an_actor_label_is_consumed_whatever_its_encoding() {
        let mut ascii = 5i32.to_le_bytes().to_vec();
        ascii.extend_from_slice(b"Rock\0");
        let mut wide = (-3i32).to_le_bytes().to_vec();
        for unit in [0x00E9u16, 0x0061, 0x0000] {
            wide.extend_from_slice(&unit.to_le_bytes());
        }
        for tail in [ascii, wide, 0i32.to_le_bytes().to_vec()] {
            let data = label(&tail);
            let mut cursor = Cursor::new(&data, 0);
            read_actor_label_tail(&mut cursor).expect("consumed");
            assert_eq!(cursor.remaining(), 0);
        }
    }

    #[test]
    fn an_actor_trailer_that_does_not_start_with_one_is_refused_unread() {
        let data = 2u32.to_le_bytes();
        let mut cursor = Cursor::new(&data, 0);
        let err = read_actor_label_tail(&mut cursor).expect_err("refused");
        assert!(err.contains("0x2"), "{err}");
        assert_eq!(cursor.remaining(), 4);
    }

    #[test]
    fn a_component_with_no_modified_properties_is_consumed() {
        let data = [0u8; 4];
        let mut cursor = Cursor::new(&data, 0);
        let mut diagnostics = Diagnostics::default();
        let header = FLegacyPackageHeader::default();
        read_actor_component_tail(&mut cursor, &bare_ctx(&header), &mut diagnostics)
            .expect("consumed");
        assert_eq!(cursor.remaining(), 0);
        assert!(diagnostics.references.is_empty());
    }

    /// Each record names the class it changed and the property it changed, and both are on record:
    /// the index for a renumber, the name for a copy into another package.
    #[test]
    fn modified_properties_record_the_class_and_the_name_each_one_points_at() {
        let mut data = 2i32.to_le_bytes().to_vec();
        for class in [-40i32, 3] {
            data.extend_from_slice(&class.to_le_bytes());
            data.extend_from_slice(&[0u8; 24]);
        }
        let mut cursor = Cursor::new(&data, 0x100);
        let mut diagnostics = Diagnostics::default();
        let header = FLegacyPackageHeader {
            name_map: retoc::legacy_asset::FPackageNameMap::create_from_names(vec![
                "Rate".to_string(),
            ]),
            ..Default::default()
        };
        read_actor_component_tail(&mut cursor, &bare_ctx(&header), &mut diagnostics)
            .expect("consumed");
        assert_eq!(cursor.remaining(), 0);
        let seen: Vec<(u64, i32)> = diagnostics
            .references
            .iter()
            .map(|r| (r.at, r.index))
            .collect();
        assert_eq!(seen, vec![(0x104, -40), (0x104 + 28, 3)]);
        assert_eq!(
            cursor.take_names(),
            vec![0x108, 0x108 + 28],
            "the record's name is read rather than stepped over"
        );
    }

    #[test]
    fn an_implausible_modified_property_count_is_refused() {
        let data = 7i32.to_le_bytes();
        let mut cursor = Cursor::new(&data, 0);
        let mut diagnostics = Diagnostics::default();
        let header = FLegacyPackageHeader::default();
        let err = read_actor_component_tail(&mut cursor, &bare_ctx(&header), &mut diagnostics)
            .expect_err("refused");
        assert!(err.contains("7"), "{err}");
    }

    #[test]
    fn cutout_geometry_is_read_only_when_its_count_accounts_for_every_byte_left() {
        let mut cursor = Cursor::new(&[], 0);
        assert!(
            read_cutout_geometry(&mut cursor).is_err(),
            "the count is always written"
        );
        let none = 0i32.to_le_bytes();
        let mut cursor = Cursor::new(&none, 0);
        read_cutout_geometry(&mut cursor).expect("no cutout");
        assert_eq!(cursor.remaining(), 0);

        let mut data = 2i32.to_le_bytes().to_vec();
        data.extend_from_slice(&[0u8; 16]);
        let mut cursor = Cursor::new(&data, 0);
        read_cutout_geometry(&mut cursor).expect("two vertices");
        assert_eq!(cursor.remaining(), 0);

        let mut short = 3i32.to_le_bytes().to_vec();
        short.extend_from_slice(&[0u8; 16]);
        let mut cursor = Cursor::new(&short, 0);
        let err = read_cutout_geometry(&mut cursor).expect_err("refused");
        assert!(err.contains("3 vertices for 16 bytes"), "{err}");

        let negative = (-1i32).to_le_bytes();
        let mut cursor = Cursor::new(&negative, 0);
        assert!(read_cutout_geometry(&mut cursor).is_err());
    }

    /// A sprite renderer's trailer word is its cutout vertex count, so the base renderer's zero
    /// word must not be read first; a mesh renderer still closes with the zero word alone.
    #[test]
    fn a_sprite_renderer_reads_its_cutout_geometry_where_other_renderers_read_a_zero_word() {
        use usmap::Struct;
        let class = |name: &str, base: Option<&str>| Struct {
            name: name.into(),
            super_struct: base.map(str::to_string),
            properties: Vec::new(),
        };
        let mappings = Mappings::from_structs(vec![
            class("Object", None),
            class("NiagaraRendererProperties", Some("Object")),
            class(
                "NiagaraSpriteRendererProperties",
                Some("NiagaraRendererProperties"),
            ),
            class(
                "NiagaraMeshRendererProperties",
                Some("NiagaraRendererProperties"),
            ),
        ]);
        let chain = |name: &str| -> Vec<String> {
            mappings
                .ancestry(name)
                .into_iter()
                .map(str::to_string)
                .collect()
        };
        let mut data = 8i32.to_le_bytes().to_vec();
        data.extend_from_slice(&[0u8; 64]);
        let mut cursor = Cursor::new(&data, 0x100);
        let mut diagnostics = Diagnostics::default();
        let header = FLegacyPackageHeader::default();
        let ctx = bare_ctx(&header);
        let outcome = read_class_tail(
            &chain("NiagaraSpriteRendererProperties"),
            &mut cursor,
            &ctx,
            &mut diagnostics,
            &mut Vec::new(),
        )
        .expect("consumed");
        assert!(matches!(outcome, TailOutcome::Consumed));
        assert_eq!(cursor.remaining(), 0);

        let word = 0i32.to_le_bytes();
        let mut cursor = Cursor::new(&word, 0x100);
        read_class_tail(
            &chain("NiagaraMeshRendererProperties"),
            &mut cursor,
            &ctx,
            &mut diagnostics,
            &mut Vec::new(),
        )
        .expect("consumed");
        assert_eq!(cursor.remaining(), 0);
    }

    /// A header holding `count` unnamed exports, so an index inside the level tail resolves.
    fn header_with_exports(count: usize) -> FLegacyPackageHeader {
        FLegacyPackageHeader {
            exports: vec![retoc::legacy_asset::FObjectExport::default(); count],
            ..Default::default()
        }
    }

    /// An ASCII FString: the length including the terminator, then the bytes.
    fn fstring(text: &str) -> Vec<u8> {
        let mut out = ((text.len() + 1) as i32).to_le_bytes().to_vec();
        out.extend_from_slice(text.as_bytes());
        out.push(0);
        out
    }

    /// A level tail with two actors, one of them null, a URL, a model, one component, a script
    /// actor and no navigation bounds.
    fn level_bytes() -> Vec<u8> {
        let mut out = 2i32.to_le_bytes().to_vec();
        out.extend_from_slice(&1i32.to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
        out.extend(fstring("unreal"));
        out.extend(fstring(""));
        out.extend(fstring("/Game/Maps/Entry"));
        out.extend(fstring(""));
        out.extend_from_slice(&0i32.to_le_bytes());
        out.extend_from_slice(&7777i32.to_le_bytes());
        out.extend_from_slice(&1i32.to_le_bytes());
        out.extend_from_slice(&2i32.to_le_bytes());
        out.extend_from_slice(&1i32.to_le_bytes());
        out.extend_from_slice(&3i32.to_le_bytes());
        out.extend_from_slice(&4i32.to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
        out
    }

    /// The level tail reads in `ULevel::Serialize` order, and what follows it is the precomputed
    /// data the export is measured for rather than decoded.
    #[test]
    fn a_level_tail_reads_its_actors_url_and_components() {
        let mut data = level_bytes();
        let tail = data.len();
        data.extend_from_slice(&[0xAA; 20]);
        let mut cursor = Cursor::new(&data, 0);
        let mut diagnostics = Diagnostics::default();
        let header = header_with_exports(5);
        let mut fields = Vec::new();
        let outcome = read_class_tail(
            &chain_of(&["Object", "Level"]),
            &mut cursor,
            &bare_ctx(&header),
            &mut diagnostics,
            &mut fields,
        )
        .expect("read");

        assert!(matches!(outcome, TailOutcome::Payload("level data")));
        assert_eq!(cursor.position(), tail, "the precomputed data is left");
        let names: Vec<&str> = fields.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Actors",
                "URL",
                "Model",
                "ModelComponents",
                "LevelScriptActor",
                "NavListStart",
                "NavListEnd"
            ]
        );
        let PropertyValue::Array { items } = &fields[0].value else {
            panic!("the actor list is not an array");
        };
        assert_eq!(items.len(), 2);
        assert!(matches!(items[1], PropertyValue::Object { index: 0, .. }));

        // Two lists are recorded as containers, so an element can be added to either.
        assert_eq!(diagnostics.containers.len(), 2);
        assert_eq!(diagnostics.containers[0].at, 0);
        assert_eq!(diagnostics.containers[0].elements.len(), 2);
        // Every non-null index is on record, so a renumber follows the list.
        let recorded: Vec<i32> = diagnostics.references.iter().map(|r| r.index).collect();
        assert!(recorded.contains(&1), "{recorded:?}");
        assert!(recorded.contains(&2), "the model is a reference too");

        let PropertyValue::Struct { fields: url, .. } = &fields[1].value else {
            panic!("the URL is not a struct");
        };
        let read = |name: &str| {
            url.iter()
                .find(|entry| entry.name == name)
                .map(|entry| entry.value.summary())
                .unwrap_or_default()
        };
        assert_eq!(read("Protocol"), "unreal");
        assert_eq!(read("Map"), "/Game/Maps/Entry");
        assert_eq!(read("Port"), "7777");
        assert_eq!(read("Valid"), "1");
    }

    /// A count that could not fit in what is left is a different layout, not a huge list, and is
    /// refused rather than read as one.
    #[test]
    fn an_implausible_actor_count_is_refused() {
        let data = 1_000_000i32.to_le_bytes();
        let mut cursor = Cursor::new(&data, 0);
        let mut diagnostics = Diagnostics::default();
        let header = FLegacyPackageHeader::default();
        let err = read_class_tail(
            &chain_of(&["Level"]),
            &mut cursor,
            &bare_ctx(&header),
            &mut diagnostics,
            &mut Vec::new(),
        )
        .err()
        .expect("refused");
        assert!(err.contains("implausible Actors count"), "{err}");
    }

    /// A truncated URL string stops the tail rather than reading past it into the model index.
    #[test]
    fn a_truncated_url_stops_the_tail() {
        let mut data = 0i32.to_le_bytes().to_vec();
        data.extend(fstring("unreal"));
        data.extend_from_slice(&40i32.to_le_bytes());
        let mut cursor = Cursor::new(&data, 0);
        let mut diagnostics = Diagnostics::default();
        let header = FLegacyPackageHeader::default();
        assert!(
            read_class_tail(
                &chain_of(&["Level"]),
                &mut cursor,
                &bare_ctx(&header),
                &mut diagnostics,
                &mut Vec::new(),
            )
            .is_err()
        );
    }

    fn bare_ctx(header: &FLegacyPackageHeader) -> Ctx<'_> {
        Ctx {
            mappings: None,
            header,
            fixups: None,
            synth: None,
            local: None,
        }
    }

    fn chain_of(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_string()).collect()
    }

    /// The skeleton guid an animation asset closes with is a field of its own, ahead of the
    /// compressed data a sequence adds.
    #[test]
    fn an_animation_asset_tail_is_its_skeleton_guid() {
        let data: Vec<u8> = (1..=16).collect();
        let mut cursor = Cursor::new(&data, 0x100);
        let mut diagnostics = Diagnostics::default();
        let header = FLegacyPackageHeader::default();
        let mut fields = Vec::new();
        let outcome = read_class_tail(
            &chain_of(&["Object", "AnimationAsset", "PoseAsset"]),
            &mut cursor,
            &bare_ctx(&header),
            &mut diagnostics,
            &mut fields,
        )
        .expect("consumed");
        assert!(matches!(outcome, TailOutcome::Consumed));
        assert_eq!(cursor.remaining(), 0);
        assert_eq!(fields[0].name, "SkeletonGuid");
        assert_eq!(fields[0].span, Some((0x100, 0x110)));
        assert!(matches!(&fields[0].value, PropertyValue::Str { value } if value.len() == 32));
    }

    /// A sound node strips two bytes; a wave player then names its wave, which goes on record
    /// for renumbering like any reference. A cue strips the same two bytes.
    #[test]
    fn sound_tails_strip_two_bytes_and_a_wave_player_names_its_wave() {
        let mut data = vec![1u8, 0];
        data.extend_from_slice(&(-10i32).to_le_bytes());
        let mut cursor = Cursor::new(&data, 0x100);
        let mut diagnostics = Diagnostics::default();
        let header = FLegacyPackageHeader {
            imports: vec![retoc::legacy_asset::FObjectImport::default(); 10],
            ..Default::default()
        };
        let mut fields = Vec::new();
        read_class_tail(
            &chain_of(&[
                "Object",
                "SoundNode",
                "SoundNodeAssetReferencer",
                "SoundNodeWavePlayer",
            ]),
            &mut cursor,
            &bare_ctx(&header),
            &mut diagnostics,
            &mut fields,
        )
        .expect("consumed");
        assert_eq!(cursor.remaining(), 0);
        assert_eq!(fields[0].name, "SoundWave");
        assert!(matches!(
            fields[0].value,
            PropertyValue::Object { index: -10, .. }
        ));
        assert_eq!(diagnostics.references.len(), 1);
        assert_eq!(diagnostics.references[0].at, 0x102);

        let mut cursor = Cursor::new(&data[..2], 0);
        read_class_tail(
            &chain_of(&["Object", "SoundBase", "SoundCue"]),
            &mut cursor,
            &bare_ctx(&header),
            &mut diagnostics,
            &mut Vec::new(),
        )
        .expect("consumed");
        assert_eq!(cursor.remaining(), 0);
    }

    #[test]
    fn a_rig_tail_is_three_zero_words_and_anything_else_is_refused() {
        let mut cursor = Cursor::new(&[0u8; 12], 0);
        let mut diagnostics = Diagnostics::default();
        let header = FLegacyPackageHeader::default();
        read_class_tail(
            &chain_of(&["Object", "Rig"]),
            &mut cursor,
            &bare_ctx(&header),
            &mut diagnostics,
            &mut Vec::new(),
        )
        .expect("consumed");
        assert_eq!(cursor.remaining(), 0);

        let mut data = [0u8; 12];
        data[8] = 5;
        let mut cursor = Cursor::new(&data, 0);
        assert!(
            read_class_tail(
                &chain_of(&["Object", "Rig"]),
                &mut cursor,
                &bare_ctx(&header),
                &mut diagnostics,
                &mut Vec::new(),
            )
            .is_err()
        );
    }

    /// An event keeps its own name; every other Wwise object is named by the shared ancestor.
    #[test]
    fn wwise_events_keep_their_name_and_other_wwise_objects_share_one() {
        assert_eq!(
            payload_kind(&chain_of(&["Object", "AkAudioType", "AkAudioEvent"])),
            Some("Wwise event data")
        );
        assert_eq!(
            payload_kind(&chain_of(&["Object", "AkAudioType", "AkAuxBus"])),
            Some("Wwise audio data")
        );
        assert_eq!(
            opaque_kind(
                "MarvelVehicleAnimBakedData",
                &chain_of(&["Object", "MarvelVehicleAnimBakedData"]),
                None
            ),
            Some("vehicle animation baked data")
        );
    }

    #[test]
    fn a_counted_payload_is_consumed_when_empty_and_named_otherwise() {
        let mut cursor = Cursor::new(&[0u8; 4], 0);
        assert!(matches!(
            read_counted_payload(&mut cursor, "foliage instance data").expect("empty"),
            TailOutcome::Consumed
        ));
        let data = 3i32.to_le_bytes();
        let mut cursor = Cursor::new(&data, 0);
        assert!(matches!(
            read_counted_payload(&mut cursor, "foliage instance data").expect("named"),
            TailOutcome::Payload("foliage instance data")
        ));
        let data = (-1i32).to_le_bytes();
        let mut cursor = Cursor::new(&data, 0);
        assert!(read_counted_payload(&mut cursor, "foliage instance data").is_err());
    }

    #[test]
    fn an_enum_tail_is_its_enumerators_and_the_form_byte() {
        let mut data = 2i32.to_le_bytes().to_vec();
        data.extend_from_slice(&[0u8; 32]);
        data.push(2);
        let mut cursor = Cursor::new(&data, 0);
        read_enum_tail(&mut cursor).expect("consumed");
        assert_eq!(cursor.remaining(), 0);

        let data = 9i32.to_le_bytes();
        let mut cursor = Cursor::new(&data, 0);
        let err = read_enum_tail(&mut cursor).expect_err("refused");
        assert!(err.contains("9"), "{err}");
    }

    #[test]
    fn a_zero_trailer_is_consumed_and_anything_else_refused_unread() {
        let mut cursor = Cursor::new(&[0u8; 4], 0);
        read_zero_word(&mut cursor, "trailer").expect("consumed");
        assert_eq!(cursor.remaining(), 0);
        let data = 9u32.to_le_bytes();
        let mut cursor = Cursor::new(&data, 0);
        let err = read_zero_word(&mut cursor, "trailer").expect_err("refused");
        assert!(err.contains("0x9"), "{err}");
        assert_eq!(cursor.remaining(), 4);
    }

    #[test]
    fn populated_static_mesh_lod_data_is_named_as_a_payload_not_decoded() {
        let data = 2i32.to_le_bytes();
        let mut cursor = Cursor::new(&data, 0);
        let outcome = read_static_mesh_component_tail(&mut cursor).expect("should classify");
        assert!(matches!(outcome, TailOutcome::Payload(kind) if kind.contains("LOD")));
    }

    #[test]
    fn empty_static_mesh_lod_data_is_fully_consumed() {
        let data = 0i32.to_le_bytes();
        let mut cursor = Cursor::new(&data, 0);
        let outcome = read_static_mesh_component_tail(&mut cursor).expect("should consume");
        assert!(matches!(outcome, TailOutcome::Consumed));
    }
}
