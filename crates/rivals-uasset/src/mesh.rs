//! Reads the native data a mesh writes outside its properties: a skeletal mesh's bounds, material
//! slots and reference skeleton ahead of its render data, and a static mesh's collision, sockets,
//! bounds, LOD screen sizes and material slots around its render data.
//!
//! Each layout is read as this game cooks it first and as a stock 5.3 editor cooks it second. A
//! stock read records the gaps a conversion fills, which is how a mod cooked outside this game is
//! told apart from one that already matches it.

use crate::props::{Ctx, Diagnostics};
use crate::reader::Cursor;
use crate::stock::{StockGap, StockGapKind, StockRender};
use crate::tails::{object_entry, object_list};
use crate::value::{PropertyEntry, PropertyValue};

/// More slots than any mesh the game ships. A count past it is a misread, not a mesh.
const MAX_MATERIALS: usize = 256;
const MAX_BONES: usize = 65_535;
/// `FSkeletalMaterial` as a stock cook writes it, the least a slot can take.
const SKELETAL_MATERIAL_BYTES: usize = 4 + 8 + 4 + 24;
/// `FStaticMaterial` as cooked: the material, the slot name and the UV channel data.
const STATIC_MATERIAL_BYTES: usize = 4 + 8 + 24;
/// `FMeshBoneInfo`: the bone's name and its parent's index.
const BONE_BYTES: usize = 8 + 4;
/// `FTransform` in doubles: rotation, translation and scale.
const POSE_BYTES: usize = 32 + 24 + 24;
/// `FBoxSphereBounds` in doubles: origin, extent and radius.
const BOUNDS_BYTES: usize = 24 + 24 + 8;
/// `MAX_STATIC_MESH_LODS`: a static mesh writes a screen size for every LOD it could have.
const STATIC_LODS: usize = 8;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Layout {
    Game,
    Stock,
}

impl Layout {
    /// How wide a static mesh's screen size is: this game follows each with a second
    /// per-platform float.
    fn screen_size_bytes(self) -> usize {
        match self {
            Self::Game => 16,
            Self::Stock => 8,
        }
    }
}

/// `USkeletalMesh::Serialize` after the properties, up to its render data: strip flags,
/// `ImportedBounds`, `Materials` and the reference skeleton. The skeleton checks itself (every
/// bone's parent comes before it, and the pose and name map count every bone), which is what
/// decides between this game's slots and a stock editor's.
pub(crate) fn read_skeletal_head(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    fields: &mut Vec<PropertyEntry>,
) -> Result<(), String> {
    cursor.skip(2)?;
    fields.push(bounds(cursor, ctx, diagnostics, "ImportedBounds")?);
    let (at, names, marks, held) = (
        cursor.position(),
        cursor.names_len(),
        diagnostics.marks(),
        fields.len(),
    );
    let game = skeletal_slots(cursor, ctx, diagnostics, fields, Layout::Game);
    let Err(reason) = game else {
        return Ok(());
    };
    cursor.seek_to(at)?;
    cursor.truncate_names(names);
    diagnostics.rewind(&marks);
    fields.truncate(held);
    skeletal_slots(cursor, ctx, diagnostics, fields, Layout::Stock).map_err(|_| reason)
}

fn skeletal_slots(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    fields: &mut Vec<PropertyEntry>,
    layout: Layout,
) -> Result<(), String> {
    fields.push(skeletal_materials(cursor, ctx, diagnostics, layout)?);
    fields.push(reference_skeleton(cursor, ctx)?);
    Ok(())
}

fn skeletal_materials(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    layout: Layout,
) -> Result<PropertyEntry, String> {
    let at = cursor.file_offset();
    let count = material_count(cursor, SKELETAL_MATERIAL_BYTES)?;
    let mut items = Vec::with_capacity(count);
    for _ in 0..count {
        let mut parts = vec![object_entry(cursor, ctx, diagnostics, "MaterialInterface")?];
        let slot = name_entry(cursor, ctx, "MaterialSlotName")?;
        let slot_name = slot.value.summary();
        parts.push(slot);
        // Whether the editor-only imported slot name follows, which a cook for a target without
        // editor data leaves out.
        match cursor.read_u32()? {
            0 => {}
            1 => parts.push(name_entry(cursor, ctx, "ImportedMaterialSlotName")?),
            other => {
                return Err(cursor.err(format!(
                    "material slot {slot_name}'s imported name flag is {other:#X}"
                )));
            }
        }
        parts.push(uv_channel_info(cursor)?);
        match layout {
            Layout::Game => {
                let start = cursor.file_offset();
                let value = crate::structs::read_native(
                    "GameplayTagContainer",
                    cursor,
                    ctx,
                    diagnostics,
                    0,
                )
                .ok_or("a gameplay tag container has no native reader")??;
                parts.push(PropertyEntry {
                    name: "MaterialTags".to_string(),
                    element: None,
                    value,
                    span: Some((start, cursor.file_offset())),
                    slot: None,
                });
            }
            Layout::Stock => diagnostics.stock_gaps.push(StockGap {
                export: 0,
                at: cursor.file_offset(),
                kind: StockGapKind::MaterialTags { slot: slot_name },
            }),
        }
        items.push(PropertyValue::Struct {
            name: "SkeletalMaterial".to_string(),
            fields: parts,
        });
    }
    Ok(PropertyEntry {
        name: "Materials".to_string(),
        element: None,
        value: PropertyValue::Array { items },
        span: Some((at, cursor.file_offset())),
        slot: None,
    })
}

/// `FReferenceSkeleton`: the bones, their reference pose and the name map, shown as the bones
/// alone and not editable, since the render data and every animation index them.
fn reference_skeleton(cursor: &mut Cursor<'_>, ctx: &Ctx<'_>) -> Result<PropertyEntry, String> {
    let count = cursor.read_i32()?;
    let Some(count) = usize::try_from(count)
        .ok()
        .filter(|n| (1..=MAX_BONES).contains(n) && n * BONE_BYTES <= cursor.remaining())
    else {
        return Err(cursor.err(format!("implausible bone count {count}")));
    };
    let mut bones = Vec::with_capacity(count);
    for bone in 0..count {
        let name = cursor.read_name(ctx.names())?;
        let parent = cursor.read_i32()?;
        let ordered = if bone == 0 {
            parent == -1
        } else {
            usize::try_from(parent).is_ok_and(|parent| parent < bone)
        };
        if !ordered {
            return Err(cursor.err(format!("bone {bone} ({name}) names parent {parent}")));
        }
        bones.push(PropertyValue::Struct {
            name: "MeshBoneInfo".to_string(),
            fields: vec![
                plain("Name", PropertyValue::Name { value: name }),
                plain(
                    "ParentIndex",
                    PropertyValue::Int {
                        value: i64::from(parent),
                    },
                ),
            ],
        });
    }
    let poses = cursor.read_i32()?;
    if usize::try_from(poses).ok() != Some(count) {
        return Err(cursor.err(format!("{poses} reference poses for {count} bones")));
    }
    cursor.skip(count * POSE_BYTES)?;
    let mapped = cursor.read_i32()?;
    if usize::try_from(mapped).ok() != Some(count) {
        return Err(cursor.err(format!("{mapped} bone names mapped for {count} bones")));
    }
    for _ in 0..count {
        cursor.read_name(ctx.names())?;
        let index = cursor.read_i32()?;
        if !usize::try_from(index).is_ok_and(|index| index < count) {
            return Err(cursor.err(format!("a bone name maps to index {index}")));
        }
    }
    Ok(plain(
        "RefSkeleton",
        PropertyValue::Struct {
            name: "ReferenceSkeleton".to_string(),
            fields: vec![plain("RefBoneInfo", PropertyValue::Array { items: bones })],
        },
    ))
}

/// `UStaticMesh::Serialize` after the properties: strip flags, the cooked flag, the collision and
/// navigation objects, the lighting guid and the sockets, then the render data. The render data
/// stays a payload; what closes it and the mesh (bounds, screen sizes, the speed tree flag and the
/// material slots again) is read back from the export's end, where its widths are known.
pub(crate) fn read_static_mesh(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    fields: &mut Vec<PropertyEntry>,
) -> Result<(), String> {
    cursor.skip(2)?;
    match cursor.read_u32()? {
        1 => {}
        other => {
            return Err(cursor.err(format!(
                "static mesh cooked flag is {other:#X}, so no render data follows"
            )));
        }
    }
    fields.push(object_entry(cursor, ctx, diagnostics, "BodySetup")?);
    fields.push(object_entry(cursor, ctx, diagnostics, "NavCollision")?);
    fields.push(crate::structs::guid_entry(
        cursor,
        diagnostics,
        "LightingGuid",
    )?);
    fields.push(object_list(cursor, ctx, diagnostics, "Sockets")?);
    let render_at = cursor.position();
    let render_file = cursor.file_offset();
    let end = render_at + cursor.remaining();
    let hint = fields
        .iter()
        .find(|entry| entry.name == "StaticMaterials")
        .and_then(|entry| match &entry.value {
            PropertyValue::Array { items } => Some(items.len()),
            _ => None,
        });
    for layout in [Layout::Game, Layout::Stock] {
        for count in hint.into_iter().chain(0..=MAX_MATERIALS) {
            let Some(start) = static_tail_start(cursor, render_at, end, count, layout) else {
                continue;
            };
            if layout == Layout::Stock
                && let Some(found) = stock_render(cursor.slice(render_at, start), render_file)
            {
                diagnostics.stock_gaps.push(StockGap {
                    export: 0,
                    at: render_file,
                    kind: StockGapKind::RenderData { found },
                });
            }
            cursor.seek_to(start)?;
            // The render data opens with its LOD count, which decides each LOD's distance.
            let lods = cursor
                .slice(render_at, render_at + 4)
                .try_into()
                .map_or(0, u32::from_le_bytes);
            static_tail(cursor, ctx, diagnostics, fields, count, layout, lods)?;
            if cursor.position() != end {
                return Err(cursor.err("the static mesh's closing data stops short of its end"));
            }
            return cursor.seek_to(render_at);
        }
    }
    Err(cursor.err("no closing layout fits the static mesh's last bytes"))
}

/// Where the closing data starts if a mesh with `count` material slots in `layout` ends at `end`:
/// the slot count sits where it should, every flag in between is a flag, and the bounds are real
/// numbers.
fn static_tail_start(
    cursor: &Cursor<'_>,
    render_at: usize,
    end: usize,
    count: usize,
    layout: Layout,
) -> Option<usize> {
    let data = cursor.slice(0, end);
    let word = |at: usize| -> Option<u32> {
        let bytes = data.get(at..at + 4)?;
        Some(u32::from_le_bytes(bytes.try_into().ok()?))
    };
    let double = |at: usize| -> Option<f64> {
        let bytes = data.get(at..at + 8)?;
        Some(f64::from_le_bytes(bytes.try_into().ok()?))
    };
    let flag = |at: usize| word(at).is_some_and(|value| value <= 1);
    let stride = layout.screen_size_bytes();
    let materials = end.checked_sub(4 + count * STATIC_MATERIAL_BYTES)?;
    let speed_tree = materials.checked_sub(4)?;
    let screens = speed_tree.checked_sub(STATIC_LODS * stride)?;
    let shared = screens.checked_sub(4)?;
    let bounds = shared.checked_sub(BOUNDS_BYTES)?;
    let fits = bounds >= render_at
        && usize::try_from(word(materials)?).ok() == Some(count)
        && flag(speed_tree)
        && flag(shared)
        && (0..STATIC_LODS).all(|lod| {
            let entry = screens + lod * stride;
            word(entry) == Some(1) && (layout == Layout::Stock || word(entry + 8) == Some(1))
        })
        && (3..7).all(|part| {
            double(bounds + part * 8).is_some_and(|value| value.is_finite() && value >= 0.0)
        });
    fits.then_some(bounds)
}

/// `FStaticMeshLODResources` class strip flags: the reversed index buffers and the ray tracing
/// geometry left out of a cook.
const STRIPPED_REVERSED_INDICES: u8 = 4;
const STRIPPED_RAY_TRACING: u8 = 8;
/// `FStripDataFlags` for the mesh cards and the distance fields: the global flag for a cook that
/// strips server data, and each one's class flag for a cook that left it out.
const STRIPPED_FOR_SERVER: u8 = 2;
const STRIPPED_CARDS: u8 = 2;
const STRIPPED_DISTANCE_FIELDS: u8 = 1;
/// `FBox3d` and a `bMostlyTwoSided` word, ahead of a LOD's mesh cards.
const CARDS_HEAD_BYTES: usize = 49 + 4;
/// `FLumenCardBuildData`: the card's oriented box in floats and its direction.
const CARD_BYTES: usize = 60 + 1;
/// What follows the flags and the streamed pages of empty Nanite resources: six empty arrays and
/// the counts, all zero.
const EMPTY_NANITE_BYTES: usize = 6 * 4 + 28;

/// What a stock cook's render data holds that this game cannot take, read from the LOD count to the
/// bounds as UE 5.3 lays it out: each LOD's buffers, the Nanite resources, then every LOD's mesh
/// cards and after them every LOD's distance field. The layout was pinned against the same mesh
/// cooked plain, with distance fields and with Nanite, and its order over several LODs against the
/// engine's `FStaticMeshRenderData::Serialize`; anything else it does not follow is said so.
fn stock_render(data: &[u8], base: u64) -> Option<StockRender> {
    match walk_stock_render(&mut Cursor::new(data, base)) {
        Ok(found) => found,
        Err(why) => Some(StockRender::Unread { why }),
    }
}

fn walk_stock_render(walk: &mut Cursor<'_>) -> Result<Option<StockRender>, String> {
    fn short<E>(_: E) -> String {
        "it ends before its last part".into()
    }
    let flag = |walk: &mut Cursor<'_>| -> Result<bool, String> {
        match walk.read_u32().map_err(short)? {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(format!(
                "a flag reads {other} at {:#X}",
                walk.file_offset() - 4
            )),
        }
    };
    let count = |walk: &mut Cursor<'_>, each: usize| -> Result<usize, String> {
        let n = walk.read_u32().map_err(short)? as usize;
        n.checked_mul(each).ok_or_else(|| short(()))
    };
    // TArray::BulkSerialize: the element size, the count, the elements.
    let array = |walk: &mut Cursor<'_>| -> Result<usize, String> {
        let size = walk.read_u32().map_err(short)? as usize;
        let bytes = count(walk, size)?;
        walk.skip(bytes).map_err(short)?;
        Ok(bytes)
    };
    let lods = walk.read_u32().map_err(short)?;
    if !(1..=STATIC_LODS as u32).contains(&lods) {
        return Err(format!("it reads {lods} LODs"));
    }
    for lod in 0..lods {
        walk.skip(2).map_err(short)?;
        let sections = walk.read_u32().map_err(short)? as usize;
        for _ in 0..sections {
            walk.skip(20).map_err(short)?;
            for _ in 0..5 {
                flag(walk)?;
            }
        }
        walk.skip(4).map_err(short)?;
        if flag(walk)? {
            return Err(format!("LOD {lod} is cooked out"));
        }
        if !flag(walk)? {
            return Err(format!("LOD {lod} streams its buffers"));
        }
        let strip = walk.take(2).map_err(short)?;
        if strip[0] & 1 == 0 {
            return Err("its editor data is not stripped".into());
        }
        let stripped = strip[1];
        walk.skip(8).map_err(short)?;
        let mut buffers = array(walk)?;
        walk.skip(2 + 16).map_err(short)?;
        buffers += array(walk)? + array(walk)?;
        walk.skip(2 + 4).map_err(short)?;
        if walk.read_u32().map_err(short)? > 0 {
            buffers += array(walk)?;
        }
        let reversed = stripped & STRIPPED_REVERSED_INDICES == 0;
        for present in [true, reversed, true, reversed] {
            if present {
                flag(walk)?;
                buffers += array(walk)?;
                flag(walk)?;
            }
        }
        if stripped & STRIPPED_RAY_TRACING == 0 {
            return Err("it keeps ray tracing data".into());
        }
        for _ in 0..=sections {
            for _ in 0..2 {
                let bytes = count(walk, 4)?;
                walk.skip(bytes).map_err(short)?;
            }
            walk.skip(4).map_err(short)?;
        }
        let recorded = walk.read_u32().map_err(short)? as usize;
        walk.skip(8).map_err(short)?;
        if recorded != buffers {
            return Err(format!(
                "LOD {lod}'s buffers come to {buffers} bytes where its size record says {recorded}"
            ));
        }
    }
    let inlined = walk.take(1).map_err(short)?[0];
    if u32::from(inlined) != lods {
        return Err(format!("{inlined} of its {lods} LODs are inlined"));
    }
    walk.skip(2).map_err(short)?;
    let flags = walk.read_u32().map_err(short)?;
    walk.skip(4).map_err(short)?;
    let rest = walk.take(EMPTY_NANITE_BYTES).map_err(short)?;
    if flags != 0 || rest.iter().any(|&byte| byte != 0) {
        return Ok(Some(StockRender::Nanite));
    }
    // A class the cook stripped writes its strip flags and none of its LODs.
    let kept = |walk: &mut Cursor<'_>, class: u8| -> Result<bool, String> {
        let strip = walk.take(2).map_err(short)?;
        Ok(strip[0] & STRIPPED_FOR_SERVER == 0 && strip[1] & class == 0)
    };
    if kept(walk, STRIPPED_CARDS)? {
        for _ in 0..lods {
            if flag(walk)? {
                walk.skip(CARDS_HEAD_BYTES).map_err(short)?;
                let bytes = count(walk, CARD_BYTES)?;
                walk.skip(bytes).map_err(short)?;
            }
        }
    }
    if kept(walk, STRIPPED_DISTANCE_FIELDS)? {
        for lod in 0..lods {
            if flag(walk)? {
                return Ok(Some(StockRender::DistanceField { lod }));
            }
        }
    }
    match walk.remaining() {
        0 => Ok(None),
        left => Err(format!("{left} bytes are left before its bounds")),
    }
}

fn static_tail(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    fields: &mut Vec<PropertyEntry>,
    count: usize,
    layout: Layout,
    lods: u32,
) -> Result<(), String> {
    fields.push(bounds(cursor, ctx, diagnostics, "RenderBounds")?);
    fields.push(flag_entry(cursor, "bLODsShareStaticLighting")?);
    let at = cursor.file_offset();
    let mut sizes = Vec::with_capacity(STATIC_LODS);
    for lod in 0..STATIC_LODS {
        let mut parts = per_platform_float(cursor)?;
        match layout {
            Layout::Game => parts.push(plain_span(cursor, "Extra", |cursor| {
                Ok(PropertyValue::Struct {
                    name: "PerPlatformFloat".to_string(),
                    fields: per_platform_float(cursor)?,
                })
            })?),
            Layout::Stock => diagnostics.stock_gaps.push(StockGap {
                export: 0,
                at: cursor.file_offset(),
                kind: StockGapKind::ScreenSize {
                    lod: lod as u32,
                    lods,
                },
            }),
        }
        sizes.push(PropertyValue::Struct {
            name: "PerPlatformFloat".to_string(),
            fields: parts,
        });
    }
    fields.push(PropertyEntry {
        name: "ScreenSize".to_string(),
        element: None,
        value: PropertyValue::Array { items: sizes },
        span: Some((at, cursor.file_offset())),
        slot: None,
    });
    fields.push(flag_entry(cursor, "bHasSpeedTreeWind")?);
    let at = cursor.file_offset();
    if material_count(cursor, STATIC_MATERIAL_BYTES)? != count {
        return Err(cursor.err("the cooked material slots moved while being read"));
    }
    let mut items = Vec::with_capacity(count);
    for _ in 0..count {
        let fields = vec![
            object_entry(cursor, ctx, diagnostics, "MaterialInterface")?,
            name_entry(cursor, ctx, "MaterialSlotName")?,
            uv_channel_info(cursor)?,
        ];
        items.push(PropertyValue::Struct {
            name: "StaticMaterial".to_string(),
            fields,
        });
    }
    fields.push(PropertyEntry {
        name: "CookedStaticMaterials".to_string(),
        element: None,
        value: PropertyValue::Array { items },
        span: Some((at, cursor.file_offset())),
        slot: None,
    });
    Ok(())
}

/// `FPerPlatformFloat` as cooked: the cooked flag as a word, then the value.
fn per_platform_float(cursor: &mut Cursor<'_>) -> Result<Vec<PropertyEntry>, String> {
    Ok(vec![
        flag_entry(cursor, "bCooked")?,
        plain_span(cursor, "Default", |cursor| {
            Ok(PropertyValue::Float {
                value: f64::from(cursor.read_f32()?),
            })
        })?,
    ])
}

/// A slot count no wider than what is left could hold.
fn material_count(cursor: &mut Cursor<'_>, smallest: usize) -> Result<usize, String> {
    let count = cursor.read_i32()?;
    usize::try_from(count)
        .ok()
        .filter(|n| *n <= MAX_MATERIALS && n * smallest <= cursor.remaining())
        .ok_or_else(|| cursor.err(format!("implausible material slot count {count}")))
}

/// `FMeshUVChannelInfo`: two flags as words and the four channel densities.
fn uv_channel_info(cursor: &mut Cursor<'_>) -> Result<PropertyEntry, String> {
    plain_span(cursor, "UVChannelData", |cursor| {
        let mut fields = vec![
            flag_entry(cursor, "bInitialized")?,
            flag_entry(cursor, "bOverrideDensities")?,
        ];
        for channel in 0..4 {
            let mut density = plain_span(cursor, "LocalUVDensities", |cursor| {
                Ok(PropertyValue::Float {
                    value: f64::from(cursor.read_f32()?),
                })
            })?;
            density.element = Some(channel);
            fields.push(density);
        }
        Ok(PropertyValue::Struct {
            name: "MeshUVChannelInfo".to_string(),
            fields,
        })
    })
}

/// `FBoxSphereBounds` in doubles, each number editable.
fn bounds(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    name: &str,
) -> Result<PropertyEntry, String> {
    plain_span(cursor, name, |cursor| {
        let mut fields = Vec::with_capacity(3);
        for part in ["Origin", "BoxExtent"] {
            fields.push(plain_span(cursor, part, |cursor| {
                crate::structs::read_native("Vector", cursor, ctx, diagnostics, 0)
                    .ok_or("a vector has no native reader")?
            })?);
        }
        fields.push(plain_span(cursor, "SphereRadius", |cursor| {
            Ok(PropertyValue::Float {
                value: cursor.read_f64()?,
            })
        })?);
        Ok(PropertyValue::Struct {
            name: "BoxSphereBounds".to_string(),
            fields,
        })
    })
}

/// A flag written as a whole word, which only reads as one while it is 0 or 1.
fn flag_entry(cursor: &mut Cursor<'_>, name: &str) -> Result<PropertyEntry, String> {
    plain_span(cursor, name, |cursor| match cursor.read_u32()? {
        value @ (0 | 1) => Ok(PropertyValue::Bool { value: value == 1 }),
        other => Err(cursor.err(format!("{name} is {other:#X} where a flag was expected"))),
    })
}

fn name_entry(cursor: &mut Cursor<'_>, ctx: &Ctx<'_>, name: &str) -> Result<PropertyEntry, String> {
    plain_span(cursor, name, |cursor| {
        Ok(PropertyValue::Name {
            value: cursor.read_name(ctx.names())?,
        })
    })
}

/// A field whose bytes are its own, editable in place.
fn plain_span(
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

/// A field shown but not edited: nothing records where it sits.
fn plain(name: &str, value: PropertyValue) -> PropertyEntry {
    PropertyEntry {
        name: name.to_string(),
        element: None,
        value,
        span: None,
        slot: None,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use retoc::legacy_asset::{
        FLegacyPackageHeader, FObjectExport, FObjectImport, FPackageNameMap,
    };

    use super::*;

    fn header() -> FLegacyPackageHeader {
        FLegacyPackageHeader {
            name_map: FPackageNameMap::create_from_names(
                ["root", "spine", "Body", "MaterialTag.Hidden"]
                    .map(str::to_string)
                    .to_vec(),
            ),
            imports: vec![FObjectImport::default(); 2],
            exports: vec![FObjectExport::default(); 3],
            ..Default::default()
        }
    }

    fn ctx(header: &FLegacyPackageHeader) -> Ctx<'_> {
        Ctx {
            mappings: None,
            header,
            fixups: None,
            synth: None,
            local: None,
            tagged: false,
        }
    }

    fn words(out: &mut Vec<u8>, values: &[i32]) {
        for value in values {
            out.extend_from_slice(&value.to_le_bytes());
        }
    }

    /// A UV channel record: initialised, one density measured.
    fn uv(out: &mut Vec<u8>) {
        words(out, &[1, 0]);
        for density in [12.5f32, 0.0, 0.0, 0.0] {
            out.extend_from_slice(&density.to_le_bytes());
        }
    }

    /// Strip flags, bounds, one material slot (with a tag in this game's layout), and a two-bone
    /// skeleton with its poses and name map. Gives the bytes and where the tags go in.
    fn skeletal_head(tagged: bool) -> (Vec<u8>, usize) {
        let mut out = vec![1, 0];
        for value in [0.0f64, 0.0, 50.0, 10.0, 10.0, 50.0, 52.0] {
            out.extend_from_slice(&value.to_le_bytes());
        }
        words(&mut out, &[1, -1, 2, 0, 0]);
        uv(&mut out);
        let tags_at = out.len();
        if tagged {
            words(&mut out, &[1, 3, 0]);
        }
        words(&mut out, &[2, 0, 0, -1, 1, 0, 0]);
        words(&mut out, &[2]);
        out.extend_from_slice(&[0u8; 2 * POSE_BYTES]);
        words(&mut out, &[2, 0, 0, 0, 1, 0, 1]);
        (out, tags_at)
    }

    #[test]
    fn a_skeletal_mesh_in_this_game_s_layout_reads_its_slots_tags_and_bones() {
        let (mut data, _) = skeletal_head(true);
        let head = data.len();
        data.extend_from_slice(&[0xAB; 16]);
        let header = header();
        let mut cursor = Cursor::new(&data, 0x100);
        let mut diagnostics = Diagnostics::default();
        let mut fields = Vec::new();
        read_skeletal_head(&mut cursor, &ctx(&header), &mut diagnostics, &mut fields)
            .expect("read");
        assert_eq!(cursor.position(), head, "the render data is left");
        let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["ImportedBounds", "Materials", "RefSkeleton"]);
        assert!(diagnostics.stock_gaps.is_empty());
        let PropertyValue::Array { items } = &fields[1].value else {
            panic!("{:?}", fields[1].value);
        };
        let PropertyValue::Struct { fields: slot, .. } = &items[0] else {
            panic!("{:?}", items[0]);
        };
        let tags = slot
            .iter()
            .find(|f| f.name == "MaterialTags")
            .expect("tags");
        assert!(
            matches!(&tags.value, PropertyValue::Array { items } if items.len() == 1),
            "{:?}",
            tags.value
        );
        assert_eq!(diagnostics.containers.len(), 1, "the tags take an insert");
        assert_eq!(
            diagnostics.references.len(),
            1,
            "the slot's material is on record"
        );
    }

    /// A stock cook writes no tag container, which the skeleton after the slot is what proves:
    /// read as this game's layout, the bone count would be taken for a tag count.
    #[test]
    fn a_stock_skeletal_mesh_reads_and_records_where_each_slot_lacks_its_tags() {
        let (data, tags_at) = skeletal_head(false);
        let header = header();
        let mut cursor = Cursor::new(&data, 0x100);
        let mut diagnostics = Diagnostics::default();
        let mut fields = Vec::new();
        read_skeletal_head(&mut cursor, &ctx(&header), &mut diagnostics, &mut fields)
            .expect("read");
        assert_eq!(cursor.remaining(), 0);
        assert_eq!(
            diagnostics.stock_gaps,
            [StockGap {
                export: 0,
                at: 0x100 + tags_at as u64,
                kind: StockGapKind::MaterialTags {
                    slot: "Body".into()
                },
            }]
        );
        assert!(
            diagnostics.containers.is_empty(),
            "the failed read is rewound"
        );
    }

    #[test]
    fn a_skeleton_whose_bones_are_out_of_order_is_refused() {
        let (mut data, tags_at) = skeletal_head(true);
        // The second bone's parent word, after the tags, the bone count and the first bone.
        let parent = tags_at + 12 + 4 + 12 + 8;
        data[parent..parent + 4].copy_from_slice(&1i32.to_le_bytes());
        let header = header();
        let mut cursor = Cursor::new(&data, 0);
        let err = read_skeletal_head(
            &mut cursor,
            &ctx(&header),
            &mut Diagnostics::default(),
            &mut Vec::new(),
        )
        .expect_err("refused");
        assert!(err.contains("bone 1"), "{err}");
    }

    /// A stock 5.3 cook's render data for one LOD of one triangle, laid out as the bush box cooked:
    /// the LOD's buffers and their size record, empty or filled Nanite resources, one mesh card,
    /// and the distance field flag.
    fn stock_render_block(nanite: bool, distance_field: bool) -> Vec<u8> {
        stock_render_lods(1, nanite, distance_field.then_some(0))
    }

    /// The same for `lods` LODs of one triangle each: every LOD's buffers, the Nanite resources,
    /// every LOD's mesh card, then every LOD's distance field flag, the one `distance_field` names
    /// set.
    fn stock_render_lods(lods: usize, nanite: bool, distance_field: Option<usize>) -> Vec<u8> {
        let mut out = Vec::new();
        let bulk = |out: &mut Vec<u8>, size: i32, count: i32| {
            words(out, &[size, count]);
            out.extend(std::iter::repeat_n(0x7F, (size * count) as usize));
        };
        words(&mut out, &[lods as i32]);
        for _ in 0..lods {
            out.extend_from_slice(&[0, 0]);
            words(&mut out, &[1, 0, 0, 1, 0, 2, 1, 1, 0, 1, 1, 0, 0, 1]);
            out.extend_from_slice(&[1, STRIPPED_RAY_TRACING]);
            words(&mut out, &[12, 3]);
            bulk(&mut out, 12, 3);
            out.extend_from_slice(&[1, 0]);
            words(&mut out, &[1, 3, 0, 0]);
            bulk(&mut out, 8, 3);
            bulk(&mut out, 4, 3);
            out.extend_from_slice(&[1, 0]);
            words(&mut out, &[0, 0]);
            for _ in 0..4 {
                words(&mut out, &[0]);
                bulk(&mut out, 1, 6);
                words(&mut out, &[0]);
            }
            words(&mut out, &[0, 0, 0, 0, 0, 0, 96, 6, 12]);
        }
        out.extend_from_slice(&[lods as u8, 1, 0]);
        words(&mut out, &[i32::from(nanite), 2]);
        out.extend_from_slice(&[0; EMPTY_NANITE_BYTES]);
        out.extend_from_slice(&[1, 0]);
        for _ in 0..lods {
            words(&mut out, &[1]);
            out.extend_from_slice(&[0; CARDS_HEAD_BYTES]);
            words(&mut out, &[1]);
            out.extend_from_slice(&[0; CARD_BYTES]);
        }
        out.extend_from_slice(&[1, 0]);
        for lod in 0..lods {
            let field = distance_field == Some(lod);
            words(&mut out, &[i32::from(field)]);
            if field {
                out.extend_from_slice(&[0x5A; 32]);
            }
        }
        out
    }

    /// Strip flags, the cooked flag, collision, navigation, lighting guid, no sockets, render data,
    /// then the closing data with `stride`-wide screen sizes and one material slot.
    fn static_mesh(stride: usize) -> (Vec<u8>, usize, usize) {
        static_mesh_with(stride, &stock_render_block(false, false))
    }

    fn static_mesh_with(stride: usize, render: &[u8]) -> (Vec<u8>, usize, usize) {
        let mut out = vec![1, 0];
        words(&mut out, &[1, 1, 2]);
        out.extend_from_slice(&[0x11; 16]);
        words(&mut out, &[0]);
        let render_at = out.len();
        out.extend_from_slice(render);
        for value in [0.0f64, 0.0, 5.0, 4.0, 4.0, 5.0, 7.0] {
            out.extend_from_slice(&value.to_le_bytes());
        }
        words(&mut out, &[1]);
        let screens_at = out.len();
        for lod in 0..STATIC_LODS {
            words(&mut out, &[1]);
            let size: f32 = if lod == 0 { 1.0 } else { 0.0 };
            out.extend_from_slice(&size.to_le_bytes());
            if stride == 16 {
                words(&mut out, &[1, 0]);
            }
        }
        words(&mut out, &[0, 1, -2, 2, 0]);
        uv(&mut out);
        (out, render_at, screens_at)
    }

    #[test]
    fn a_static_mesh_reads_its_head_and_its_closing_data_and_leaves_the_render_data() {
        let (data, render_at, _) = static_mesh(16);
        let header = header();
        let mut cursor = Cursor::new(&data, 0);
        let mut diagnostics = Diagnostics::default();
        let mut fields = Vec::new();
        read_static_mesh(&mut cursor, &ctx(&header), &mut diagnostics, &mut fields).expect("read");
        assert_eq!(
            cursor.position(),
            render_at,
            "the render data is the payload"
        );
        let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "BodySetup",
                "NavCollision",
                "LightingGuid",
                "Sockets",
                "RenderBounds",
                "bLODsShareStaticLighting",
                "ScreenSize",
                "bHasSpeedTreeWind",
                "CookedStaticMaterials"
            ]
        );
        assert!(diagnostics.stock_gaps.is_empty());
        let indices: Vec<i32> = diagnostics.references.iter().map(|r| r.index).collect();
        assert_eq!(
            indices,
            [1, 2, -2],
            "collision, navigation and the material"
        );
    }

    #[test]
    fn a_stock_static_mesh_records_where_each_screen_size_is_short() {
        let (data, render_at, screens_at) = static_mesh(8);
        let header = header();
        let mut cursor = Cursor::new(&data, 0);
        let mut diagnostics = Diagnostics::default();
        read_static_mesh(
            &mut cursor,
            &ctx(&header),
            &mut diagnostics,
            &mut Vec::new(),
        )
        .expect("read");
        assert_eq!(cursor.position(), render_at);
        let gaps: Vec<u64> = diagnostics.stock_gaps.iter().map(|g| g.at).collect();
        let expected: Vec<u64> = (1..=STATIC_LODS)
            .map(|lod| (screens_at + lod * 8) as u64)
            .collect();
        assert_eq!(gaps, expected);
    }

    /// What the read finds in a stock cook's render data: nothing when it is plain and lands on
    /// the bounds, Nanite resources or a distance field when the editor cooked them in, and the
    /// reason when the bytes do not end where the walk does.
    #[test]
    fn a_stock_static_mesh_s_render_data_is_read_for_nanite_and_distance_fields() {
        assert_eq!(stock_render(&stock_render_block(false, false), 0), None);
        assert_eq!(
            stock_render(&stock_render_block(false, true), 0),
            Some(StockRender::DistanceField { lod: 0 })
        );
        assert_eq!(
            stock_render(&stock_render_block(true, false), 0),
            Some(StockRender::Nanite)
        );
        let mut longer = stock_render_block(false, false);
        longer.extend_from_slice(&[0; 4]);
        assert!(matches!(
            stock_render(&longer, 0),
            Some(StockRender::Unread { why }) if why.contains("4 bytes are left")
        ));
    }

    /// Several LODs keep their mesh cards together and then their distance fields together, as
    /// the engine writes them. A cook that strips either writes no LOD of it, and a LOD left out of
    /// the inlined count is said so.
    #[test]
    fn a_stock_mesh_of_several_lods_reads_its_cards_then_its_distance_fields() {
        for lods in [2, 3] {
            assert_eq!(stock_render(&stock_render_lods(lods, false, None), 0), None);
            assert_eq!(
                stock_render(&stock_render_lods(lods, false, Some(lods - 1)), 0),
                Some(StockRender::DistanceField {
                    lod: lods as u32 - 1
                })
            );
            assert_eq!(
                stock_render(&stock_render_lods(lods, true, None), 0),
                Some(StockRender::Nanite)
            );

            let tail = 2 + lods * (4 + CARDS_HEAD_BYTES + 4 + CARD_BYTES) + 2 + lods * 4;
            let mut stripped = stock_render_lods(lods, false, None);
            stripped.truncate(stripped.len() - tail);
            stripped.extend_from_slice(&[1, STRIPPED_CARDS, 1, STRIPPED_DISTANCE_FIELDS]);
            assert_eq!(stock_render(&stripped, 0), None, "{lods} LODs stripped");

            let mut short = stock_render_lods(lods, false, None);
            let inlined = short.len() - tail - EMPTY_NANITE_BYTES - 8 - 3;
            short[inlined] = lods as u8 - 1;
            assert!(matches!(
                stock_render(&short, 0),
                Some(StockRender::Unread { why }) if why.contains("LODs are inlined")
            ));
        }
    }

    /// The finding is a gap of a stock cook only; this game's own meshes keep distance fields in
    /// their own format, which the read leaves alone.
    #[test]
    fn a_distance_field_is_recorded_on_a_stock_mesh_and_not_on_the_game_s() {
        let render = stock_render_block(false, true);
        let header = header();
        let found = |stride: usize| -> Vec<(u64, StockGapKind)> {
            let (data, _, _) = static_mesh_with(stride, &render);
            let mut diagnostics = Diagnostics::default();
            read_static_mesh(
                &mut Cursor::new(&data, 0),
                &ctx(&header),
                &mut diagnostics,
                &mut Vec::new(),
            )
            .expect("read");
            diagnostics
                .stock_gaps
                .into_iter()
                .filter(|gap| matches!(gap.kind, StockGapKind::RenderData { .. }))
                .map(|gap| (gap.at, gap.kind))
                .collect()
        };
        let (_, render_at, _) = static_mesh_with(8, &render);
        let distance_field = StockGapKind::RenderData {
            found: StockRender::DistanceField { lod: 0 },
        };
        assert_eq!(found(8), [(render_at as u64, distance_field)]);
        assert!(found(16).is_empty());
    }

    /// Bytes no layout closes leave the mesh a payload, refused rather than half read.
    #[test]
    fn a_static_mesh_whose_end_fits_no_layout_is_refused() {
        let (mut data, _, _) = static_mesh(16);
        data.pop();
        let header = header();
        let mut cursor = Cursor::new(&data, 0);
        assert!(
            read_static_mesh(
                &mut cursor,
                &ctx(&header),
                &mut Diagnostics::default(),
                &mut Vec::new()
            )
            .is_err()
        );
    }
}
