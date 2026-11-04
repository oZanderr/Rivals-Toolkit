//! Reads the copy of its notify tracks, sections and montage path an ability timeline serializes
//! itself after its properties.
//!
//! The copy holds the property copy's values field for field, plus each notify's class, and is
//! the one the game is expected to read, so each field is surfaced under the name its property
//! twin has and an edit to either is made to both (see `mirror.rs`).

use crate::props::{Ctx, Diagnostics};
use crate::reader::Cursor;
use crate::tails::object_entry;
use crate::value::{PropertyEntry, PropertyValue};

/// More tracks, notifies or sections than any timeline holds. A count past it is a misread.
const MAX_ITEMS: i32 = 4096;

/// `UAnimTimeline`'s own serializer: a version byte, the notify tracks, the sections and the
/// montage path.
pub(crate) fn read_timeline(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    fields: &mut Vec<PropertyEntry>,
) -> Result<(), String> {
    fields.push(byte_entry(cursor, "CookedVersion")?);
    fields.push(list(cursor, "CookedNotifyTracks", |cursor| {
        track(cursor, ctx, diagnostics)
    })?);
    fields.push(list(cursor, "CookedSections", |cursor| {
        section(cursor, ctx)
    })?);
    fields.push(spanned(cursor, "CookedMontagePath", |cursor| {
        Ok(PropertyValue::Str {
            value: cursor.read_string()?,
        })
    })?);
    Ok(())
}

fn track(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
) -> Result<PropertyValue, String> {
    let fields = vec![
        byte_entry(cursor, "Version")?,
        name_entry(cursor, ctx, "TrackName")?,
        native(cursor, ctx, diagnostics, "LinearColor", "TrackColor")?,
        list(cursor, "Notifies", |cursor| {
            notify(cursor, ctx, diagnostics)
        })?,
    ];
    Ok(PropertyValue::Struct {
        name: "AnimTimelineNotifyTrack".to_string(),
        fields,
    })
}

fn notify(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
) -> Result<PropertyValue, String> {
    let fields = vec![
        byte_entry(cursor, "Version")?,
        name_entry(cursor, ctx, "NotifyName")?,
        float_entry(cursor, "StartTime")?,
        spanned(cursor, "TrackIndex", |cursor| {
            Ok(PropertyValue::Int {
                value: i64::from(cursor.read_i32()?),
            })
        })?,
        native(cursor, ctx, diagnostics, "Color", "NotifyColor")?,
        // Not among the event's properties: the class of a notify the timeline fires by its tag,
        // and none for the rest.
        object_entry(cursor, ctx, diagnostics, "NotifyClass")?,
        object_entry(cursor, ctx, diagnostics, "NotifyObject")?,
        object_entry(cursor, ctx, diagnostics, "NotifyStateClass")?,
        float_entry(cursor, "Duration")?,
        float_entry(cursor, "TriggerTimeOffset")?,
        float_entry(cursor, "EndTriggerTimeOffset")?,
        spanned(cursor, "Identity", |cursor| {
            Ok(PropertyValue::Str {
                value: cursor.read_string()?,
            })
        })?,
    ];
    Ok(PropertyValue::Struct {
        name: "AnimTimelineEvent".to_string(),
        fields,
    })
}

fn section(cursor: &mut Cursor<'_>, ctx: &Ctx<'_>) -> Result<PropertyValue, String> {
    let fields = vec![
        byte_entry(cursor, "Version")?,
        name_entry(cursor, ctx, "SectionName")?,
        name_entry(cursor, ctx, "NextSectionName")?,
        float_entry(cursor, "Time")?,
    ];
    Ok(PropertyValue::Struct {
        name: "AnimTimelineSection".to_string(),
        fields,
    })
}

/// A counted list whose elements are read whole. Its length is the timeline's to keep in step
/// with the property copy, so it is not recorded as a container to add to or drop from.
fn list(
    cursor: &mut Cursor<'_>,
    name: &str,
    mut element: impl FnMut(&mut Cursor<'_>) -> Result<PropertyValue, String>,
) -> Result<PropertyEntry, String> {
    let start = cursor.file_offset();
    let count = cursor.read_i32()?;
    if !(0..=MAX_ITEMS).contains(&count) {
        return Err(cursor.err(format!("implausible {name} count {count}")));
    }
    let items = (0..count)
        .map(|_| element(cursor))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PropertyEntry {
        name: name.to_string(),
        element: None,
        value: PropertyValue::Array { items },
        span: Some((start, cursor.file_offset())),
        slot: None,
    })
}

/// A struct written whole, read as its property twin reads, so both copies show alike.
fn native(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    structure: &str,
    name: &str,
) -> Result<PropertyEntry, String> {
    let start = cursor.file_offset();
    let value = crate::structs::read_native(structure, cursor, ctx, diagnostics, 0)
        .ok_or_else(|| format!("{structure} has no native reader"))??;
    Ok(PropertyEntry {
        name: name.to_string(),
        element: None,
        value,
        span: Some((start, cursor.file_offset())),
        slot: None,
    })
}

fn spanned(
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

fn byte_entry(cursor: &mut Cursor<'_>, name: &str) -> Result<PropertyEntry, String> {
    spanned(cursor, name, |cursor| {
        Ok(PropertyValue::Byte {
            value: cursor.read_u8()?,
        })
    })
}

fn float_entry(cursor: &mut Cursor<'_>, name: &str) -> Result<PropertyEntry, String> {
    spanned(cursor, name, |cursor| {
        Ok(PropertyValue::Float {
            value: f64::from(cursor.read_f32()?),
        })
    })
}

fn name_entry(cursor: &mut Cursor<'_>, ctx: &Ctx<'_>, name: &str) -> Result<PropertyEntry, String> {
    spanned(cursor, name, |cursor| {
        Ok(PropertyValue::Name {
            value: cursor.read_name(ctx.names())?,
        })
    })
}
