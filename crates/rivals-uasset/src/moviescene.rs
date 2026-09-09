//! Native layouts for the MovieScene structs that serialize themselves: the channels, whose keys
//! are two bulk arrays, the compiled evaluation trees, and the inline template values that name
//! their struct before writing it.

use crate::props::{Ctx, Diagnostics, read_struct, spanned};
use crate::reader::Cursor;
use crate::structs::NativeDefault;
use crate::value::{PropertyEntry, PropertyValue};

/// Whether `name` is one of the structs this module reads in place of the schema.
pub(crate) fn reads(name: &str) -> bool {
    matches!(
        name,
        "MovieSceneFloatChannel"
            | "MovieSceneDoubleChannel"
            | "MovieSceneTrackIdentifier"
            | "MovieSceneSequenceID"
            | "MovieSceneEvaluationFieldEntityTree"
            | "MovieSceneSubSequenceTree"
            | "MovieSceneEvaluationKey"
            | "MovieSceneEvalTemplatePtr"
            | "MovieSceneTrackImplementationPtr"
            | "MovieSceneSequenceInstanceDataPtr"
    )
}

/// What each layout holds when default-constructed, for storing one from nothing.
pub(crate) fn default_of(name: &str) -> NativeDefault {
    NativeDefault::Fixed(match name {
        "MovieSceneFloatChannel" => empty_channel(Precision::Single),
        "MovieSceneDoubleChannel" => empty_channel(Precision::Double),
        "MovieSceneEvaluationFieldEntityTree" | "MovieSceneSubSequenceTree" => empty_tree(),
        // An empty type path, so no struct block follows.
        "MovieSceneEvalTemplatePtr"
        | "MovieSceneTrackImplementationPtr"
        | "MovieSceneSequenceInstanceDataPtr" => vec![0u8; 4],
        "MovieSceneTrackIdentifier" | "MovieSceneSequenceID" => vec![0u8; 4],
        "MovieSceneEvaluationKey" => vec![0u8; 12],
        _ => return NativeDefault::NotNative,
    })
}

/// `RCCE_Constant`, the extrapolation a channel is constructed with.
const CONSTANT_EXTRAPOLATION: u8 = 4;
/// `ERangeBoundTypes::Open`, the bound of a tree root covering every frame.
const OPEN_BOUND: u8 = 2;
/// The tick resolution a channel is constructed with: 24000 ticks per second.
const DEFAULT_TICK_RESOLUTION: [i32; 2] = [24000, 1];

/// A constructed channel: constant extrapolation both ways, no keys, no default, the default tick
/// resolution and the curve hidden.
fn empty_channel(precision: Precision) -> Vec<u8> {
    let mut out = vec![CONSTANT_EXTRAPOLATION, CONSTANT_EXTRAPOLATION];
    for size in [FRAME_NUMBER_SIZE, precision.value_size()] {
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
    }
    out.extend(std::iter::repeat_n(
        0u8,
        match precision {
            Precision::Single => 4,
            Precision::Double => 8,
        },
    ));
    out.extend_from_slice(&0u32.to_le_bytes());
    for part in DEFAULT_TICK_RESOLUTION {
        out.extend_from_slice(&part.to_le_bytes());
    }
    out.extend_from_slice(&0u32.to_le_bytes());
    out
}

/// A constructed tree: an open root with no parent, children or data, and four empty tables.
fn empty_tree() -> Vec<u8> {
    let mut out = Vec::with_capacity(NODE_BYTES + 16);
    for _ in 0..2 {
        out.push(OPEN_BOUND);
        out.extend_from_slice(&0i32.to_le_bytes());
    }
    for _ in 0..4 {
        out.extend_from_slice(&(-1i32).to_le_bytes());
    }
    out.extend_from_slice(&[0u8; 16]);
    out
}

pub(crate) fn read(
    name: &str,
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    depth: u32,
) -> Result<PropertyValue, String> {
    match name {
        "MovieSceneFloatChannel" => channel(cursor, ctx, diagnostics, Precision::Single),
        "MovieSceneDoubleChannel" => channel(cursor, ctx, diagnostics, Precision::Double),
        // `FMovieSceneEvaluationKey::operator<<`: three bare words.
        "MovieSceneEvaluationKey" => Ok(PropertyValue::Struct {
            name: name.to_string(),
            fields: vec![
                word(cursor, "SequenceID")?,
                word(cursor, "TrackIdentifier")?,
                word(cursor, "SectionIndex")?,
            ],
        }),
        "MovieSceneEvaluationFieldEntityTree" => evaluation_tree(
            cursor,
            ctx,
            diagnostics,
            depth,
            name,
            ENTITY_INDEX_BYTES,
            &|c, _, _, _| {
                Ok(PropertyValue::Struct {
                    name: "EntityAndMetaDataIndex".to_string(),
                    fields: vec![int(c, "EntityIndex")?, int(c, "MetaDataIndex")?],
                })
            },
        ),
        // `FMovieSceneSubSequenceTreeEntry`: the sequence id, the evaluation flags byte, then the
        // warp counter as a reflected block of its own.
        "MovieSceneSubSequenceTree" => evaluation_tree(
            cursor,
            ctx,
            diagnostics,
            depth,
            name,
            SUB_SEQUENCE_ENTRY_MIN_BYTES,
            &|c, ctx, diagnostics, depth| {
                Ok(PropertyValue::Struct {
                    name: "MovieSceneSubSequenceTreeEntry".to_string(),
                    fields: vec![
                        word(c, "SequenceID")?,
                        mode(c, ctx, "Flags", "ESectionEvaluationFlags")?,
                        spanned(c, "RootToSequenceWarpCounter", |c| {
                            read_struct("MovieSceneWarpCounter", c, ctx, diagnostics, depth + 1)
                        })?,
                    ],
                })
            },
        ),
        "MovieSceneEvalTemplatePtr"
        | "MovieSceneTrackImplementationPtr"
        | "MovieSceneSequenceInstanceDataPtr" => {
            inline_value(name, cursor, ctx, diagnostics, depth)
        }
        // Both write their one `uint32` bare, without the struct header.
        "MovieSceneTrackIdentifier" | "MovieSceneSequenceID" => Ok(PropertyValue::Struct {
            name: name.to_string(),
            fields: vec![spanned(cursor, "Value", |c| {
                Ok(PropertyValue::UInt {
                    value: u64::from(c.read_u32()?),
                })
            })?],
        }),
        other => Err(cursor.err(format!("{other} has no MovieScene layout"))),
    }
}

/// `FMovieSceneFloatChannel` keys hold a `float`, `FMovieSceneDoubleChannel` keys a `double`; the
/// rest of the two layouts is shared.
#[derive(Clone, Copy)]
enum Precision {
    Single,
    Double,
}

impl Precision {
    fn channel_name(self) -> &'static str {
        match self {
            Self::Single => "MovieSceneFloatChannel",
            Self::Double => "MovieSceneDoubleChannel",
        }
    }

    fn key_name(self) -> &'static str {
        match self {
            Self::Single => "MovieSceneFloatKey",
            Self::Double => "MovieSceneDoubleKey",
        }
    }

    /// `sizeof` of the bulk-serialized value struct, which the stream states in front of the keys.
    fn value_size(self) -> i32 {
        match self {
            Self::Single => 28,
            Self::Double => 32,
        }
    }

    fn read_value(self, cursor: &mut Cursor<'_>) -> Result<f64, String> {
        match self {
            Self::Single => Ok(f64::from(cursor.read_f32()?)),
            Self::Double => cursor.read_f64(),
        }
    }
}

const FRAME_NUMBER_SIZE: i32 = 4;

/// `FMovieSceneFloatChannel::Serialize`: the extrapolation modes, the key times and values as two
/// bulk arrays, the default, the tick resolution, and the curve visibility flag this build cooks.
/// Times and values are zipped into one `Keys[i]` entry per key, which has no span of its own
/// because its bytes lie apart. The two arrays only ever change length together, which the key
/// editor does through the layout recorded here.
fn channel(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    precision: Precision,
) -> Result<PropertyValue, String> {
    let at = cursor.file_offset();
    let mut fields = vec![
        mode(cursor, ctx, "PreInfinityExtrap", "ERichCurveExtrapolation")?,
        mode(cursor, ctx, "PostInfinityExtrap", "ERichCurveExtrapolation")?,
    ];
    let times_count_at = cursor.file_offset() + 4;
    let count = bulk_count(cursor, "Times", FRAME_NUMBER_SIZE)?;
    let mut times = Vec::with_capacity(count);
    let mut time_offsets = Vec::with_capacity(count);
    let mut frames = Vec::with_capacity(count);
    for _ in 0..count {
        time_offsets.push(cursor.file_offset());
        let time = int(cursor, "Time")?;
        if let PropertyValue::Int { value } = time.value {
            frames.push(value as i32);
        }
        times.push(time);
    }
    let values_count_at = cursor.file_offset() + 4;
    let values = bulk_count(cursor, "Values", precision.value_size())?;
    if values != count {
        return Err(cursor.err(format!("{count} key times but {values} key values")));
    }
    let mut value_spans = Vec::with_capacity(count);
    for (index, time) in times.into_iter().enumerate() {
        let start = cursor.file_offset();
        let mut key = Vec::with_capacity(9);
        key.push(time);
        key.extend(key_value(cursor, ctx, precision)?);
        value_spans.push((start, cursor.file_offset()));
        fields.push(PropertyEntry {
            name: "Keys".to_string(),
            element: Some(index as u32),
            value: PropertyValue::Struct {
                name: precision.key_name().to_string(),
                fields: key,
            },
            span: None,
            slot: None,
        });
    }
    diagnostics.channels.push(crate::props::ChannelLayout {
        at,
        times_count_at,
        times: time_offsets,
        frames,
        values_count_at,
        values: value_spans,
        value_bytes: precision.value_size() as u64,
    });
    fields.push(spanned(cursor, "DefaultValue", |c| {
        Ok(PropertyValue::Float {
            value: precision.read_value(c)?,
        })
    })?);
    fields.push(flag(cursor, "bHasDefaultValue")?);
    fields.push(spanned(cursor, "TickResolution", |c| {
        Ok(PropertyValue::Struct {
            name: "FrameRate".to_string(),
            fields: vec![int(c, "Numerator")?, int(c, "Denominator")?],
        })
    })?);
    fields.push(flag(cursor, "bShowCurve")?);
    Ok(PropertyValue::Struct {
        name: precision.channel_name().to_string(),
        fields,
    })
}

/// A `TArray::BulkSerialize` prefix: the element size, then the count. A size other than the one
/// this layout is built for means the engine changed the struct, which is better refused here than
/// misread as keys.
fn bulk_count(cursor: &mut Cursor<'_>, what: &str, element_size: i32) -> Result<usize, String> {
    let size = cursor.read_i32()?;
    if size != element_size {
        return Err(cursor.err(format!(
            "{what} are written {size} bytes each, not the {element_size} this layout expects"
        )));
    }
    let count = cursor.read_i32()?;
    let fits = usize::try_from(count)
        .ok()
        .and_then(|n| n.checked_mul(element_size as usize))
        .is_some_and(|bytes| bytes <= cursor.remaining());
    if !fits {
        return Err(cursor.err(format!("implausible {what} count {count}")));
    }
    Ok(count as usize)
}

/// One bulk-serialized `FMovieSceneFloatValue` or `FMovieSceneDoubleValue`: the value, the tangent
/// block and the interpolation modes, with the padding of the in-memory layout between them.
fn key_value(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    precision: Precision,
) -> Result<Vec<PropertyEntry>, String> {
    let mut fields = vec![spanned(cursor, "Value", |c| {
        Ok(PropertyValue::Float {
            value: precision.read_value(c)?,
        })
    })?];
    for name in [
        "ArriveTangent",
        "LeaveTangent",
        "ArriveTangentWeight",
        "LeaveTangentWeight",
    ] {
        fields.push(spanned(cursor, name, |c| {
            Ok(PropertyValue::Float {
                value: f64::from(c.read_f32()?),
            })
        })?);
    }
    fields.push(mode(
        cursor,
        ctx,
        "TangentWeightMode",
        "ERichCurveTangentWeightMode",
    )?);
    cursor.skip(3)?;
    fields.push(mode(cursor, ctx, "InterpMode", "ERichCurveInterpMode")?);
    fields.push(mode(cursor, ctx, "TangentMode", "ERichCurveTangentMode")?);
    cursor.skip(2)?;
    Ok(fields)
}

/// A byte-sized curve enum, named where the mappings know the enumerator.
fn mode(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    name: &str,
    enum_name: &str,
) -> Result<PropertyEntry, String> {
    spanned(cursor, name, |c| {
        let value = i64::from(c.read_u8()?);
        Ok(PropertyValue::Enum {
            value,
            name: ctx.enum_name(enum_name, value),
            enum_type: Some(enum_name.to_string()),
        })
    })
}

/// A tree node: a frame range, the handle of the parent, and the entries holding the children
/// and the data.
const NODE_BYTES: usize = 26;
const ENTRY_BYTES: usize = 12;
const ENTITY_INDEX_BYTES: usize = 8;
/// A sequence id, a flags byte and an empty warp counter block.
const SUB_SEQUENCE_ENTRY_MIN_BYTES: usize = 4 + 1 + 2;

type ItemReader<'r> =
    &'r dyn Fn(&mut Cursor<'_>, &Ctx<'_>, &mut Diagnostics, u32) -> Result<PropertyValue, String>;

/// `TMovieSceneEvaluationTree<T>` as `operator<<` writes it: the root node, the child nodes behind
/// their entry table, then the data items behind theirs. Every list is a count and records with no
/// header anywhere, so each comes out as `Name[i]` entries. `item_bytes` is the least an item can
/// take, which keeps a wild count from being believed.
fn evaluation_tree(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    depth: u32,
    name: &str,
    item_bytes: usize,
    item: ItemReader<'_>,
) -> Result<PropertyValue, String> {
    let mut fields = vec![tree_node(cursor, ctx, "RootNode")?];
    fields.extend(entry_table(cursor, "ChildEntries")?);
    for index in 0..counted(cursor, "child node", NODE_BYTES)? {
        fields.push(indexed(tree_node(cursor, ctx, "ChildNodes")?, index));
    }
    fields.extend(entry_table(cursor, "DataEntries")?);
    for index in 0..counted(cursor, "data item", item_bytes)? {
        let entry = spanned(cursor, "Data", |c| item(c, ctx, diagnostics, depth))?;
        fields.push(indexed(entry, index));
    }
    Ok(PropertyValue::Struct {
        name: name.to_string(),
        fields,
    })
}

/// `TEvaluationTreeEntryContainer::Entries`: where each node's children or data start in the item
/// list, how many there are, and the capacity the tree reserved.
fn entry_table(cursor: &mut Cursor<'_>, name: &str) -> Result<Vec<PropertyEntry>, String> {
    let mut entries = Vec::new();
    for index in 0..counted(cursor, "tree entry", ENTRY_BYTES)? {
        let entry = spanned(cursor, name, |c| {
            Ok(PropertyValue::Struct {
                name: "EvaluationTreeEntry".to_string(),
                fields: vec![int(c, "StartIndex")?, int(c, "Size")?, int(c, "Capacity")?],
            })
        })?;
        entries.push(indexed(entry, index));
    }
    Ok(entries)
}

fn tree_node(cursor: &mut Cursor<'_>, ctx: &Ctx<'_>, name: &str) -> Result<PropertyEntry, String> {
    spanned(cursor, name, |c| {
        Ok(PropertyValue::Struct {
            name: "MovieSceneEvaluationTreeNode".to_string(),
            fields: vec![
                range_bound(c, ctx, "LowerBound")?,
                range_bound(c, ctx, "UpperBound")?,
                int(c, "ParentChildrenEntry")?,
                int(c, "ParentIndex")?,
                int(c, "ChildrenEntry")?,
                int(c, "DataEntry")?,
            ],
        })
    })
}

/// `TRangeBound<FFrameNumber>`: the bound type byte, then the frame.
fn range_bound(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    name: &str,
) -> Result<PropertyEntry, String> {
    spanned(cursor, name, |c| {
        Ok(PropertyValue::Struct {
            name: "FrameNumberRangeBound".to_string(),
            fields: vec![mode(c, ctx, "Type", "ERangeBoundTypes")?, int(c, "Value")?],
        })
    })
}

/// `TInlineValue` as `FMovieSceneEvalTemplatePtr::Serialize` writes it: the struct's object path,
/// then, unless the path is empty, the struct as an unversioned block of its own.
fn inline_value(
    name: &str,
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    depth: u32,
) -> Result<PropertyValue, String> {
    let type_name = spanned(cursor, "TypeName", |c| {
        Ok(PropertyValue::Str {
            value: c.read_string()?,
        })
    })?;
    let path = match &type_name.value {
        PropertyValue::Str { value } => value.clone(),
        _ => String::new(),
    };
    let mut fields = vec![type_name];
    if !path.is_empty() {
        let short = path.rsplit(['.', '/']).next().unwrap_or(&path).to_string();
        fields.push(spanned(cursor, "Value", |c| {
            read_struct(&short, c, ctx, diagnostics, depth + 1)
        })?);
    }
    Ok(PropertyValue::Struct {
        name: name.to_string(),
        fields,
    })
}

fn counted(cursor: &mut Cursor<'_>, what: &str, bytes_each: usize) -> Result<usize, String> {
    let count = cursor.read_i32()?;
    let fits = usize::try_from(count)
        .ok()
        .and_then(|n| n.checked_mul(bytes_each))
        .is_some_and(|bytes| bytes <= cursor.remaining());
    if !fits {
        return Err(cursor.err(format!("implausible {what} count {count}")));
    }
    Ok(count as usize)
}

fn indexed(mut entry: PropertyEntry, index: usize) -> PropertyEntry {
    entry.element = Some(index as u32);
    entry
}

fn word(cursor: &mut Cursor<'_>, name: &str) -> Result<PropertyEntry, String> {
    spanned(cursor, name, |c| {
        Ok(PropertyValue::UInt {
            value: u64::from(c.read_u32()?),
        })
    })
}

fn flag(cursor: &mut Cursor<'_>, name: &str) -> Result<PropertyEntry, String> {
    spanned(cursor, name, |c| {
        Ok(PropertyValue::Bool {
            value: c.read_bool32()?,
        })
    })
}

fn int(cursor: &mut Cursor<'_>, name: &str) -> Result<PropertyEntry, String> {
    spanned(cursor, name, |c| {
        Ok(PropertyValue::Int {
            value: i64::from(c.read_i32()?),
        })
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use retoc::legacy_asset::FLegacyPackageHeader;
    use usmap::{Property, PropertyInner, Struct};

    use super::*;
    use crate::mappings::Mappings;

    fn read_all(name: &str, data: &[u8]) -> Result<(PropertyValue, usize), String> {
        read_with(name, data, None)
    }

    fn read_with(
        name: &str,
        data: &[u8],
        mappings: Option<&Mappings>,
    ) -> Result<(PropertyValue, usize), String> {
        let header = FLegacyPackageHeader::default();
        let ctx = Ctx {
            mappings,
            header: &header,
            fixups: None,
            synth: None,
            local: None,
        };
        let mut diagnostics = Diagnostics::default();
        let mut cursor = Cursor::new(data, 0x200);
        let value = read(name, &mut cursor, &ctx, &mut diagnostics, 0)?;
        Ok((value, cursor.position()))
    }

    fn words(out: &mut Vec<u8>, values: &[i32]) {
        for value in values {
            out.extend_from_slice(&value.to_le_bytes());
        }
    }

    /// The tree measured in a widget's compiled data: an open root with one child covering frames
    /// 0 to 20000, and two entities on that child.
    #[test]
    fn an_entity_tree_is_a_root_then_two_entry_tables_with_their_items() {
        let mut data = vec![2u8];
        words(&mut data, &[0]);
        data.push(2);
        words(&mut data, &[0, -1, -1, 0, -1]);
        words(&mut data, &[1, 0, 1, 1]);
        words(&mut data, &[1]);
        data.push(1);
        words(&mut data, &[0]);
        data.push(1);
        words(&mut data, &[20000, -1, 0, -1, 0]);
        words(&mut data, &[1, 0, 2, 2]);
        words(&mut data, &[2, 0, -1, 1, -1]);
        assert_eq!(data.len(), 108);
        let (value, consumed) =
            read_all("MovieSceneEvaluationFieldEntityTree", &data).expect("tree");
        assert_eq!(consumed, 108);
        let PropertyValue::Struct { fields, .. } = value else {
            panic!("expected a struct");
        };
        let names: Vec<_> = fields
            .iter()
            .map(|f| match f.element {
                Some(i) => format!("{}[{i}]", f.name),
                None => f.name.clone(),
            })
            .collect();
        assert_eq!(
            names,
            [
                "RootNode",
                "ChildEntries[0]",
                "ChildNodes[0]",
                "DataEntries[0]",
                "Data[0]",
                "Data[1]"
            ]
        );
        let PropertyValue::Struct { fields: child, .. } = &fields[2].value else {
            panic!("expected a node");
        };
        let PropertyValue::Struct { fields: upper, .. } = &child[1].value else {
            panic!("expected a bound");
        };
        assert!(matches!(
            upper[1].value,
            PropertyValue::Int { value: 20000 }
        ));
        assert!(
            matches!(child[5].value, PropertyValue::Int { value: 0 }),
            "data entry 0"
        );
        let PropertyValue::Struct { fields: second, .. } = &fields[5].value else {
            panic!("expected an item");
        };
        assert!(matches!(second[0].value, PropertyValue::Int { value: 1 }));
        assert_eq!(fields[5].span, Some((0x200 + 100, 0x200 + 108)));
    }

    /// The item measured in a boss sequence: the sub-sequence's id, a zero flags byte, then a warp
    /// counter block whose one array is empty.
    #[test]
    fn a_sub_sequence_tree_item_ends_with_a_reflected_warp_counter() {
        let mappings = Mappings::from_structs(vec![Struct {
            name: "MovieSceneWarpCounter".into(),
            super_struct: None,
            properties: vec![Property {
                name: "WarpCounts".into(),
                array_dim: 1,
                index: 0,
                inner: PropertyInner::Array {
                    inner: Box::new(PropertyInner::UInt32),
                },
            }],
        }]);
        let mut data = vec![2u8];
        words(&mut data, &[0]);
        data.push(2);
        words(&mut data, &[0, -1, -1, 0, -1]);
        words(&mut data, &[0, 0, 1, 0, 1, 1, 1]);
        data.extend_from_slice(&0x402A_2DE3u32.to_le_bytes());
        data.push(0);
        data.extend_from_slice(&[0x00, 0x03]);
        words(&mut data, &[0]);
        let (value, consumed) =
            read_with("MovieSceneSubSequenceTree", &data, Some(&mappings)).expect("tree");
        assert_eq!(consumed, data.len());
        let PropertyValue::Struct { fields, .. } = value else {
            panic!("expected a struct");
        };
        assert_eq!(fields.last().map(|f| f.name.as_str()), Some("Data"));
        let PropertyValue::Struct { fields: item, .. } = &fields[2].value else {
            panic!("expected an item");
        };
        assert!(matches!(
            item[0].value,
            PropertyValue::UInt { value: 0x402A_2DE3 }
        ));
        assert_eq!(item[2].name, "RootToSequenceWarpCounter");
    }

    #[test]
    fn an_evaluation_key_is_three_bare_words() {
        let mut data = Vec::new();
        words(&mut data, &[5, 7, 2]);
        let (value, consumed) = read_all("MovieSceneEvaluationKey", &data).expect("key");
        assert_eq!(consumed, 12);
        let PropertyValue::Struct { fields, .. } = value else {
            panic!("expected a struct");
        };
        assert!(matches!(fields[1].value, PropertyValue::UInt { value: 7 }));
    }

    #[test]
    fn an_empty_inline_value_is_only_its_empty_type_name() {
        let data = 0i32.to_le_bytes();
        let (value, consumed) = read_all("MovieSceneEvalTemplatePtr", &data).expect("pointer");
        assert_eq!(consumed, 4);
        assert!(matches!(&value, PropertyValue::Struct { fields, .. } if fields.len() == 1));
    }

    /// The struct named by the path follows as its own unversioned block, looked up by the name
    /// after the last dot.
    #[test]
    fn a_named_inline_value_reads_the_struct_it_names() {
        let mappings = Mappings::from_structs(vec![Struct {
            name: "MovieSceneSlomoSectionTemplate".into(),
            super_struct: None,
            properties: vec![Property {
                name: "Rate".into(),
                array_dim: 1,
                index: 0,
                inner: PropertyInner::Int,
            }],
        }]);
        let path = "/Script/MovieSceneTracks.MovieSceneSlomoSectionTemplate";
        let mut data = ((path.len() + 1) as i32).to_le_bytes().to_vec();
        data.extend_from_slice(path.as_bytes());
        data.push(0);
        data.extend_from_slice(&[0x00, 0x03]);
        data.extend_from_slice(&7i32.to_le_bytes());
        let (value, consumed) =
            read_with("MovieSceneTrackImplementationPtr", &data, Some(&mappings)).expect("pointer");
        assert_eq!(consumed, data.len());
        let PropertyValue::Struct { fields, .. } = value else {
            panic!("expected a struct");
        };
        assert_eq!(fields[1].name, "Value");
        let PropertyValue::Struct {
            name,
            fields: inner,
        } = &fields[1].value
        else {
            panic!("expected the named struct");
        };
        assert_eq!(name, "MovieSceneSlomoSectionTemplate");
        assert!(matches!(inner[0].value, PropertyValue::Int { value: 7 }));
    }

    /// One bulk-serialized key value: the value bytes, four tangents, the weight mode and its
    /// padding, then the two modes and the padding byte plus alignment.
    fn key(value: &[u8], interp: u8, tangent_mode: u8, weight_mode: u8) -> Vec<u8> {
        let mut out = value.to_vec();
        for tangent in [0.0f32, 0.0, 1.0, 1.0] {
            out.extend_from_slice(&tangent.to_le_bytes());
        }
        out.push(weight_mode);
        out.extend_from_slice(&[0, 0, 0]);
        out.push(interp);
        out.push(tangent_mode);
        out.extend_from_slice(&[0, 0]);
        out
    }

    fn channel_bytes(value_size: i32, times: &[i32], keys: &[Vec<u8>], default: &[u8]) -> Vec<u8> {
        let mut out = vec![4u8, 4];
        out.extend_from_slice(&FRAME_NUMBER_SIZE.to_le_bytes());
        out.extend_from_slice(&(times.len() as i32).to_le_bytes());
        for time in times {
            out.extend_from_slice(&time.to_le_bytes());
        }
        out.extend_from_slice(&value_size.to_le_bytes());
        out.extend_from_slice(&(keys.len() as i32).to_le_bytes());
        for key in keys {
            out.extend_from_slice(key);
        }
        out.extend_from_slice(default);
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&24000i32.to_le_bytes());
        out.extend_from_slice(&1i32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out
    }

    fn struct_fields(value: &PropertyValue) -> &[PropertyEntry] {
        let PropertyValue::Struct { fields, .. } = value else {
            panic!("expected a struct, got {value:?}");
        };
        fields
    }

    #[test]
    fn a_float_channel_zips_its_two_bulk_arrays_into_keys() {
        let data = channel_bytes(
            28,
            &[0, 24000],
            &[
                key(&1.0f32.to_le_bytes(), 2, 0, 0),
                key(&0.5f32.to_le_bytes(), 1, 1, 3),
            ],
            &0.25f32.to_le_bytes(),
        );
        let (value, consumed) = read_all("MovieSceneFloatChannel", &data).expect("channel");
        assert_eq!(consumed, data.len());
        let fields = struct_fields(&value);
        let names: Vec<_> = fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "PreInfinityExtrap",
                "PostInfinityExtrap",
                "Keys",
                "Keys",
                "DefaultValue",
                "bHasDefaultValue",
                "TickResolution",
                "bShowCurve"
            ]
        );
        assert!(matches!(
            fields[0].value,
            PropertyValue::Enum { value: 4, .. }
        ));
        assert_eq!(fields[3].element, Some(1));
        assert_eq!(
            fields[3].span, None,
            "a key's bytes lie apart, so it has no span of its own"
        );

        let second = struct_fields(&fields[3].value);
        let key_names: Vec<_> = second.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            key_names,
            [
                "Time",
                "Value",
                "ArriveTangent",
                "LeaveTangent",
                "ArriveTangentWeight",
                "LeaveTangentWeight",
                "TangentWeightMode",
                "InterpMode",
                "TangentMode"
            ]
        );
        assert!(matches!(
            second[0].value,
            PropertyValue::Int { value: 24000 }
        ));
        assert_eq!(second[0].span, Some((0x20E, 0x212)), "the second time");
        assert!(matches!(second[1].value, PropertyValue::Float { value } if value == 0.5));
        assert_eq!(second[1].span, Some((0x236, 0x23A)), "the second value");
        assert!(matches!(second[5].value, PropertyValue::Float { value } if value == 1.0));
        assert!(matches!(
            second[6].value,
            PropertyValue::Enum { value: 3, .. }
        ));
        assert!(matches!(
            second[7].value,
            PropertyValue::Enum { value: 1, .. }
        ));
        assert_eq!(
            second[7].span,
            Some((0x24E, 0x24F)),
            "past the weight padding"
        );
        assert!(matches!(
            second[8].value,
            PropertyValue::Enum { value: 1, .. }
        ));

        assert!(matches!(fields[4].value, PropertyValue::Float { value } if value == 0.25));
        assert!(matches!(
            fields[5].value,
            PropertyValue::Bool { value: true }
        ));
        assert_eq!(fields[5].span, Some((0x256, 0x25A)), "a full-word bool");
        let resolution = struct_fields(&fields[6].value);
        assert!(matches!(
            resolution[0].value,
            PropertyValue::Int { value: 24000 }
        ));
        assert!(matches!(
            resolution[1].value,
            PropertyValue::Int { value: 1 }
        ));
        assert!(matches!(
            fields[7].value,
            PropertyValue::Bool { value: false }
        ));
    }

    #[test]
    fn a_double_channel_holds_eight_byte_values_and_default() {
        let data = channel_bytes(
            32,
            &[12000],
            &[key(&2.5f64.to_le_bytes(), 2, 0, 0)],
            &(-1.0f64).to_le_bytes(),
        );
        let (value, consumed) = read_all("MovieSceneDoubleChannel", &data).expect("channel");
        assert_eq!(consumed, 78);
        assert_eq!(consumed, data.len());
        let fields = struct_fields(&value);
        let first = struct_fields(&fields[2].value);
        assert!(matches!(first[1].value, PropertyValue::Float { value } if value == 2.5));
        assert_eq!(first[1].span.map(|(s, e)| e - s), Some(8));
        assert!(matches!(fields[3].value, PropertyValue::Float { value } if value == -1.0));
        assert_eq!(fields[3].span.map(|(s, e)| e - s), Some(8));
    }

    #[test]
    fn a_channel_without_keys_still_carries_its_trailer() {
        let data = channel_bytes(28, &[], &[], &0.0f32.to_le_bytes());
        let (value, consumed) = read_all("MovieSceneFloatChannel", &data).expect("channel");
        assert_eq!(consumed, 38);
        assert_eq!(struct_fields(&value).len(), 6);
    }

    /// An element size the layout does not predict means the engine changed the struct; reading
    /// on would take the wrong bytes for keys and fail somewhere unrelated.
    #[test]
    fn a_foreign_key_size_is_refused_at_the_prefix() {
        let data = channel_bytes(24, &[], &[], &0.0f32.to_le_bytes());
        let error = read_all("MovieSceneFloatChannel", &data).expect_err("refused");
        assert!(error.contains("24 bytes each"), "{error}");
    }

    #[test]
    fn mismatched_time_and_value_counts_are_refused() {
        let mut data = vec![4u8, 4];
        data.extend_from_slice(&4i32.to_le_bytes());
        data.extend_from_slice(&1i32.to_le_bytes());
        data.extend_from_slice(&0i32.to_le_bytes());
        data.extend_from_slice(&28i32.to_le_bytes());
        data.extend_from_slice(&0i32.to_le_bytes());
        data.extend_from_slice(&[0u8; 20]);
        let error = read_all("MovieSceneFloatChannel", &data).expect_err("refused");
        assert!(error.contains("1 key times but 0 key values"), "{error}");
    }

    #[test]
    fn an_identifier_is_one_bare_word() {
        let (value, consumed) =
            read_all("MovieSceneTrackIdentifier", &7u32.to_le_bytes()).expect("identifier");
        assert_eq!(consumed, 4);
        let fields = struct_fields(&value);
        assert!(matches!(fields[0].value, PropertyValue::UInt { value: 7 }));
        assert_eq!(fields[0].span, Some((0x200, 0x204)));
    }
}
