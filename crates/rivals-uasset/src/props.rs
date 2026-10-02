//! Reads one unversioned property block by walking the schema the header indexes into.

use std::collections::{BTreeMap, BTreeSet};

use retoc::legacy_asset::{FLegacyPackageHeader, FPackageNameMap};
use retoc::zen::FPackageIndex;
use serde::Serialize;
use usmap::PropertyInner;

use crate::mappings::{Mappings, Schema, SchemaFixups, kind_name};
use crate::reader::Cursor;
use crate::structs;
use crate::unversioned;
use crate::value::{MapEntry, PropertyEntry, PropertyValue, SlotRef};

/// Structs can nest arbitrarily deep in principle; this stops a corrupt stream from recursing away.
const MAX_STRUCT_DEPTH: u32 = 64;

pub(crate) struct Ctx<'a> {
    /// Absent when reading a tagged package, which carries its own type information and so needs
    /// no mappings file at all.
    pub mappings: Option<&'a Mappings>,
    pub header: &'a FLegacyPackageHeader,
    /// Schema slots this build does not serialize, found by [`crate::package`]'s repair search.
    pub fixups: Option<&'a SchemaFixups>,
    /// Structs recovered from the game's own packages, consulted only when the mappings file has
    /// no entry, so synthesis can never override a real one.
    pub synth: Option<&'a Mappings>,
    /// The struct an export defines itself, consulted first while its own default instance is read:
    /// the definition on disk is the truth for those bytes whatever the mappings say.
    pub local: Option<&'a Mappings>,
}

/// A schema the reader needed and could not find, with where its definition lives.
#[derive(Debug, Clone, Serialize)]
pub struct MissingSchema {
    pub name: String,
    pub object_path: String,
}

impl<'a> Ctx<'a> {
    pub(crate) fn names(&self) -> &'a FPackageNameMap {
        &self.header.name_map
    }

    /// A struct recovered from the game's own packages outranks the mappings file, as a class
    /// does: a Blueprint-generated struct (an animation Blueprint's mutable data, say) shares its
    /// name with every other Blueprint's, and the file can hold only one of them.
    pub(crate) fn schema(&self, name: &str) -> Option<Schema<'a>> {
        self.local
            .and_then(|m| m.schema_fixed(name, self.fixups))
            .or_else(|| self.synth.and_then(|m| m.schema_fixed(name, self.fixups)))
            .or_else(|| {
                self.mappings
                    .and_then(|m| m.schema_fixed(name, self.fixups))
            })
    }

    /// A class recovered from the game's own package outranks the mappings file: it is what was
    /// cooked, where the file may describe an older revision of the Blueprint.
    pub(crate) fn class_schema(&self, name: &str) -> Option<Schema<'a>> {
        self.synth
            .and_then(|m| m.class_schema(name, self.fixups))
            .or_else(|| {
                self.mappings
                    .and_then(|m| m.class_schema(name, self.fixups))
            })
    }

    /// [`Self::class_schema`] for a class known by its object path as well: two Blueprint classes
    /// can share a name across packages, and only the path tells a recovered one from the other.
    pub(crate) fn class_schema_at(&self, name: &str, path: Option<&str>) -> Option<Schema<'a>> {
        path.and_then(|path| self.synth.and_then(|m| m.class_schema(path, self.fixups)))
            .or_else(|| self.class_schema(name))
    }

    /// [`Self::ancestry`] for a class known by its object path as well.
    pub(crate) fn ancestry_at(&self, name: &str, path: Option<&str>) -> Vec<String> {
        if let Some(path) = path
            && let Some(synth) = self.synth
        {
            let chain: Vec<String> = synth
                .ancestry(path)
                .into_iter()
                .map(str::to_string)
                .collect();
            if chain.first().is_some_and(|root| root == "Object") {
                return chain;
            }
        }
        self.ancestry(name)
    }

    /// The parent a recovered class names that nothing defines, when its chain fails to root at
    /// `Object`. An unresolved import there means the class cannot be laid out at all.
    pub(crate) fn missing_ancestor(&self, name: &str, path: Option<&str>) -> Option<String> {
        let synth = self.synth?;
        path.and_then(|path| synth.missing_ancestor(path))
            .or_else(|| synth.missing_ancestor(name))
    }

    /// The class chain, root first, from whichever mappings hold the class: a recovered class
    /// carries its parents with it, so its tails resolve like any other.
    pub(crate) fn ancestry(&self, name: &str) -> Vec<String> {
        let from = |mappings: Option<&Mappings>| {
            mappings.map_or_else(Vec::new, |m| {
                m.ancestry(name).into_iter().map(str::to_string).collect()
            })
        };
        let synth = from(self.synth);
        if synth.first().is_some_and(|root| root == "Object") {
            return synth;
        }
        from(self.mappings)
    }

    pub(crate) fn enum_value(&self, enum_name: &str, entry: &str) -> Option<i64> {
        self.mappings.and_then(|m| m.enum_value(enum_name, entry))
    }

    pub(crate) fn enum_name(&self, enum_name: &str, value: i64) -> Option<String> {
        self.mappings
            .and_then(|m| m.enum_name(enum_name, value))
            .map(str::to_string)
    }

    /// An index outside both maps means the stream has desynced. retoc indexes these unchecked,
    /// so the bounds test has to happen here, and it doubles as a corruption tripwire. Paths come
    /// out in UE's dotted form, the same one the Package view shows and an edit accepts.
    pub(crate) fn object_path(&self, index: i32) -> Result<Option<String>, String> {
        let package_index = FPackageIndex { index };
        let in_range = if package_index.is_import() {
            (package_index.to_import_index() as usize) < self.header.imports.len()
        } else if package_index.is_export() {
            (package_index.to_export_index() as usize) < self.header.exports.len()
        } else {
            return Ok(None);
        };
        if !in_range {
            return Err(format!(
                "object index {index} points outside the {} imports and {} exports of this package",
                self.header.imports.len(),
                self.header.exports.len()
            ));
        }
        Ok(crate::package::dotted_path(self.header, package_index).filter(|name| !name.is_empty()))
    }
}

/// One decoded property and the exact bytes it consumed. A read of the wrong width shows up here
/// as a range that does not line up, which is far quicker than inferring it from a hex dump.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TraceEntry {
    pub depth: u32,
    pub name: String,
    pub kind: &'static str,
    pub start: u64,
    pub end: u64,
    pub value: String,
}

/// Only ever populated when a caller explicitly asks to trace, and the CLI narrows the output to
/// one export, so the cap is here purely to bound memory on a pathological package.
const MAX_TRACE: usize = 200_000;

/// Where a container's elements sit, so one can be replaced, added or dropped without
/// re-serializing anything around it.
///
/// Container elements are bare values with no property entry of their own, so unlike a struct
/// field they have nowhere to record a span. This is that record, addressed by where the container
/// itself starts.
#[derive(Debug, Clone)]
pub struct ContainerLayout {
    /// Where the container's own bytes begin, which is what an edit addresses it by.
    pub at: u64,
    /// The count holding how many elements follow.
    pub count_at: u64,
    /// How many bytes that count occupies: four for a `TArray`, one for a list a native struct
    /// writes behind a byte count.
    pub count_width: u8,
    /// Where the first element goes when there is none to follow, if not straight after the
    /// count: a tagged array of structs writes its element tag in between.
    pub elements_at: Option<u64>,
    /// Byte range of each element. A map pair counts as one, spanning its key and its value.
    pub elements: Vec<(u64, u64)>,
    /// The type an element is stored as, for sizing a number that has no bytes yet.
    pub element_kind: &'static str,
    /// Bytes a freshly defaulted element would occupy, where that is unambiguous. `None` for the
    /// kinds whose default depends on something this does not know, such as an enumerator name.
    pub default_element: Option<Vec<u8>>,
    /// Elements are written as the inner property's own value, which spells an enum as its
    /// enumerator name rather than as the underlying integer.
    pub element_is_enum: bool,
    /// The enum's type when the elements are enums, so a number typed for one can be named.
    pub element_enum: Option<String>,
    /// A name the default element spells, encoded when the edit is made because only the package's
    /// name map can say how: `None` for a Name, the first enumerator for an Enum, an empty path
    /// for a soft object.
    pub default_name: Option<String>,
    /// A native struct element's default in parts, for the layouts that spell a name inside.
    pub default_recipe: Option<Vec<DefaultPart>>,
    /// For a map, where each pair's key sits and how a fresh key is written.
    pub keys: Option<MapKeys>,
    /// Set for a container the header does not store, by its header block and schema slot: it has
    /// no count yet, so the first element added writes one ahead of itself.
    pub absent: Option<(u64, u32)>,
}

/// The key side of a map's pairs. A value edit addresses the bytes after the key, and a fresh pair
/// needs a key no other pair has.
#[derive(Debug, Clone)]
pub struct MapKeys {
    pub spans: Vec<(u64, u64)>,
    pub kind: &'static str,
    pub is_enum: bool,
    pub enum_type: Option<String>,
    pub default: Option<Vec<u8>>,
    pub default_name: Option<String>,
    /// A native struct key's default in parts, for the layouts that spell a name inside.
    pub default_recipe: Option<Vec<DefaultPart>>,
}

/// Where a stored `FPackageIndex` sits in the property data, and what it holds. Recorded for
/// every reference read, so an export removal can find each one to clear or renumber.
#[derive(Debug, Clone, Copy)]
pub struct IndexRef {
    pub at: u64,
    pub index: i32,
}

/// The field an instanced struct shows its type as, ahead of the fields the type gives it. Set to
/// another struct's path, it gives the instanced struct that type and its defaults.
pub const TYPE_FIELD: &str = "(type)";

/// The field a payload that did not decode shows its bytes as.
pub const UNDECODED_FIELD: &str = "(undecoded)";

/// Where an `FInstancedStruct` keeps its payload and the byte length that guards it. An edit that
/// changes the width of anything inside has to move the length too.
#[derive(Debug, Clone, Copy)]
pub struct InstancedLayout {
    /// The `i32` byte length covering the payload: written just before it by an instanced struct,
    /// or in the tag ahead of it by a tagged property.
    pub size_at: u64,
    pub payload_start: u64,
    pub payload_end: u64,
}

/// An instanced struct payload that did not decode. The export still reads to its end, because
/// the payload's own length prefix puts the cursor back, so without this record the failure is
/// invisible: the export reports as exact and only the placeholder in the tree says otherwise.
#[derive(Debug, Clone, Serialize)]
pub struct UndecodedPayload {
    /// The struct the payload declares itself to be.
    pub struct_name: String,
    pub at: u64,
    pub end: u64,
    /// Why the inner parse stopped, with the offset it stopped at.
    pub reason: String,
}

/// A declared slot the header skips, and what storing it from nothing would write.
#[derive(Debug, Clone)]
pub struct UnsetSlot {
    /// Where a value for it would be inserted, which is also the entry's span.
    pub at: u64,
    pub header_at: u64,
    pub schema_index: u32,
    pub declared: &'static str,
    /// The struct type, for the few structs whose default needs a name from the package.
    pub struct_name: Option<String>,
    /// The type's minimal stored form, for kinds that need no name to spell it. `None` leaves it
    /// to the editor, which either has the name map or needs a typed value.
    pub default_bytes: Option<Vec<u8>>,
    /// The minimal form of a type that spells a name, finished against the name map at edit time.
    pub default_recipe: Option<Vec<DefaultPart>>,
    /// For a zero struct, its stored form with every slot flagged zero, which is what keeps it
    /// zero. The minimal form skips them, and a skipped slot takes the archetype's value.
    pub zero_bytes: Option<Vec<u8>>,
}

/// Where a MovieScene channel's two bulk arrays sit. A key is a frame in `Times` and a value block
/// at the same position in `Values`; the two only ever change length together.
#[derive(Debug, Clone)]
pub struct ChannelLayout {
    /// Where the channel's bytes begin, which is what an edit addresses it by.
    pub at: u64,
    /// The `i32` count in front of the frames.
    pub times_count_at: u64,
    /// Where each frame word sits.
    pub times: Vec<u64>,
    /// The frames as read, in stream order.
    pub frames: Vec<i32>,
    /// The `i32` count in front of the value blocks.
    pub values_count_at: u64,
    /// Each value block's span.
    pub values: Vec<(u64, u64)>,
    /// A value block's width: 28 for a float channel, 32 for a double one.
    pub value_bytes: u64,
}

/// One piece of a default written from nothing. Bytes the type alone settles; the name `None`,
/// which only the package's name map can spell; or an empty block of a reflected struct, which the
/// reader resolves through the schemas it has before the recipe leaves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefaultPart {
    Bytes(Vec<u8>),
    NoneName,
    Struct(&'static str),
}

/// A native struct whose single value is written differently from what its decoded kind suggests:
/// a guid reads as a string and the two asset paths as soft references. An edit has to know which
/// one it is holding to write it back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeLeaf {
    /// Sixteen raw bytes.
    Guid,
    /// A package name and an asset name, with no sub-path.
    TopLevelAssetPath,
    /// One string holding the whole path.
    MarvelSoftObjectPath,
    /// The name of the string table a text shows an entry of, which the package has to import.
    StringTableId,
    /// The word a `NavAgentSelector` packs its sixteen bools into. Each bool spans the whole word.
    AgentBits,
}

/// A tagged property's tag from its name to the end of its value, keyed by where its entry's span
/// starts: for a bool, the value byte inside the tag.
#[derive(Debug, Clone)]
pub struct TagBounds {
    pub key: u64,
    pub tag_at: u64,
    pub tag_end: u64,
    /// The terminating `None` of the block holding it, where it reads as absent once removed.
    pub none_at: u64,
}

/// A property a tagged block does not hold, listed at the block's terminating `None`, which is
/// where a tag for it goes in.
#[derive(Debug, Clone)]
pub struct TaggedAbsent {
    pub none_at: u64,
    pub name: String,
    /// The slot of a fixed-size array, as the tag's array index names it.
    pub array_index: u32,
    pub inner: PropertyInner,
    /// For a native struct, its value with every field default, which an empty one is written as.
    pub native_default: Option<Vec<DefaultPart>>,
}

#[derive(Default, Debug)]
pub struct Diagnostics {
    /// Structs that had neither a native layout nor a schema, which is the actionable signal for
    /// extending the native table.
    pub unresolved_structs: BTreeSet<String>,
    pub unsupported: BTreeSet<String>,
    /// How many values of each property type were actually read. A type the mappings declare but
    /// that never appears here is never serialized, which is the difference between code that is
    /// untested and code that is unreachable.
    pub property_kinds: BTreeMap<&'static str, usize>,
    /// Populated only when the caller asks for a trace.
    pub trace: Option<Vec<TraceEntry>>,
    /// Structs the reader was inside when it gave up, innermost first. Drives the repair search.
    pub failing_structs: Vec<String>,
    /// Schemas the mappings file did not cover, with the package that defines them.
    pub missing_schemas: Vec<MissingSchema>,
    /// Where each container's elements sit, for edits that add or drop one.
    pub containers: Vec<ContainerLayout>,
    /// Declared slots the headers skipped, recorded only when `declared_slots` is set.
    pub unset: Vec<UnsetSlot>,
    /// Every non-null object reference read, by position.
    pub references: Vec<IndexRef>,
    /// Where each string table's strings sit, for edits that change one.
    pub string_tables: Vec<crate::stringtable::StringTableLayout>,
    /// Every instanced struct payload and the length prefix that has to follow its width.
    pub instanced: Vec<InstancedLayout>,
    /// Payloads whose contents did not decode, so a silent failure is counted rather than hidden.
    pub undecoded: Vec<UndecodedPayload>,
    /// How many of each bytecode token were read, which says what the game's scripts actually use
    /// and which tokens this build adds.
    pub script_tokens: BTreeMap<u8, usize>,
    /// Where each DataTable's rows sit, for edits that add, drop or rename one.
    pub tables: Vec<crate::datatable::DataTableLayout>,
    /// Where each MovieScene channel's key arrays sit, for edits that add or drop a key.
    pub channels: Vec<ChannelLayout>,
    /// Where each [`NativeLeaf`] value starts.
    pub native_leaves: Vec<(u64, NativeLeaf)>,
    /// Where each tagged property's whole tag sits, for removing one.
    pub tag_bounds: Vec<TagBounds>,
    /// Properties a tagged block's owner declares that the block does not hold.
    pub tagged_absent: Vec<TaggedAbsent>,
    /// How many texts of each `ETextHistoryType` were read, which says which text layouts the
    /// data exercises at all.
    pub text_histories: BTreeMap<i8, usize>,
    /// How many texts carried each `ETextFlag` word under each history, which says what flags a
    /// text rebuilt as another history should take.
    pub text_flags: BTreeMap<(i8, u32), usize>,
    /// Re-encode every unversioned header and compare it with the bytes it came from. The
    /// writer may only be trusted where it reproduces UE's own fragmentation exactly.
    pub check_headers: bool,
    /// Emit an entry for every declared slot the header skips, so a value that inherits its
    /// parent's can be shown and stored. Off for corpus statistics, which count stored values.
    pub declared_slots: bool,
    pub headers_checked: usize,
    pub headers_differing: usize,
    /// How many headers took each shape the audit counts, recorded with `check_headers`.
    pub header_shapes: BTreeMap<&'static str, usize>,
    /// Headers in a shape no header the game ships takes.
    pub odd_headers: Vec<OddHeader>,
}

/// A property header in a shape the game never writes, and where it sits.
#[derive(Debug, Clone, Serialize)]
pub struct OddHeader {
    pub shape: &'static str,
    pub at: u64,
}

/// How much of each layout list had been recorded at a point in the parse, so a read that turns
/// out to have been against the wrong schema can drop what it recorded.
pub(crate) struct Marks {
    references: usize,
    containers: usize,
    instanced: usize,
    unset: usize,
    channels: usize,
    native_leaves: usize,
    tag_bounds: usize,
    tagged_absent: usize,
    odd_headers: usize,
}

impl Diagnostics {
    pub(crate) fn marks(&self) -> Marks {
        Marks {
            references: self.references.len(),
            containers: self.containers.len(),
            instanced: self.instanced.len(),
            unset: self.unset.len(),
            channels: self.channels.len(),
            native_leaves: self.native_leaves.len(),
            tag_bounds: self.tag_bounds.len(),
            tagged_absent: self.tagged_absent.len(),
            odd_headers: self.odd_headers.len(),
        }
    }

    /// Forgets everything recorded since `marks`. A reference decoded at the wrong offset would
    /// otherwise be renumbered by a later export removal, writing over bytes that mean something
    /// else entirely.
    pub(crate) fn rewind(&mut self, marks: &Marks) {
        self.references.truncate(marks.references);
        self.containers.truncate(marks.containers);
        self.instanced.truncate(marks.instanced);
        self.unset.truncate(marks.unset);
        self.channels.truncate(marks.channels);
        self.native_leaves.truncate(marks.native_leaves);
        self.tag_bounds.truncate(marks.tag_bounds);
        self.tagged_absent.truncate(marks.tagged_absent);
        self.odd_headers.truncate(marks.odd_headers);
    }
}

/// Appends into `entries` rather than returning them so a caller that fails partway still has
/// everything decoded up to the desync, which is what the export view shows.
pub(crate) fn read_property_block(
    cursor: &mut Cursor<'_>,
    schema: &Schema<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    depth: u32,
    entries: &mut Vec<PropertyEntry>,
) -> Result<(), String> {
    let header_at = cursor.file_offset();
    let header_start = cursor.position();
    let header = unversioned::read_header(cursor)?;
    for shape in header.shapes() {
        if diagnostics.check_headers {
            *diagnostics.header_shapes.entry(shape).or_default() += 1;
        }
        if unversioned::NEVER_WRITTEN.contains(&shape) {
            diagnostics.odd_headers.push(OddHeader {
                shape,
                at: header_at,
            });
        }
    }
    if diagnostics.check_headers {
        diagnostics.headers_checked += 1;
        let original = cursor.slice(header_start, cursor.position());
        if !header.write().is_ok_and(|bytes| bytes == original) {
            diagnostics.headers_differing += 1;
        }
    }
    // A header reaching past the schema is not this struct's header at all: the stream has
    // desynced, or the struct is written by a serializer of its own. Failing here names the
    // culprit; reading on would fail somewhere unrelated.
    let covered = header.covered_slots();
    if covered > schema.len() {
        return Err(cursor.err(format!(
            "{} has {} schema slots but the header covers {covered}. The mappings file does not match this build, or the struct has a native serializer.",
            schema.name(),
            schema.len()
        )));
    }
    let items = header.items;
    entries.reserve(items.len());
    let mut next_slot = 0usize;
    for item in items {
        let index = item.schema_index as usize;
        if diagnostics.declared_slots {
            emit_unset(
                cursor,
                schema,
                ctx,
                diagnostics,
                header_at,
                next_slot..index,
                entries,
            );
        }
        next_slot = index + 1;
        let Some(slot) = schema.slot(index) else {
            return Err(cursor.err(format!(
                "{} has {} schema slots but the header asked for slot {}. The mappings file does not match this build.",
                schema.name(),
                schema.len(),
                item.schema_index
            )));
        };
        let start = cursor.file_offset();
        let value = if item.is_zero {
            // A zero value occupies no bytes, so storing it or giving it one takes what an unset
            // slot's does.
            record_unset(
                diagnostics,
                ctx,
                start,
                header_at,
                item.schema_index as usize,
                &slot.property.inner,
                true,
            );
            match zero_value(&slot.property.inner, ctx) {
                PropertyValue::Default { declared, .. } if diagnostics.declared_slots => {
                    PropertyValue::Default {
                        declared,
                        fields: zero_fields(&slot.property.inner, ctx, 0),
                    }
                }
                other => other,
            }
        } else {
            read_value(&slot.property.inner, cursor, ctx, diagnostics, depth).map_err(|e| {
                if diagnostics.failing_structs.last().map(String::as_str) != Some(schema.name()) {
                    diagnostics.failing_structs.push(schema.name().to_string());
                }
                format!("{}.{}: {e}", schema.name(), slot.property.name)
            })?
        };
        if let Some(trace) = diagnostics.trace.as_mut()
            && trace.len() < MAX_TRACE
        {
            trace.push(TraceEntry {
                depth,
                name: slot.property.name.clone(),
                kind: kind_name(&slot.property.inner),
                start,
                end: cursor.file_offset(),
                value: value.summary(),
            });
        }
        let end = cursor.file_offset();
        entries.push(PropertyEntry {
            name: slot.property.name.clone(),
            element: (slot.property.array_dim > 1).then_some(slot.element),
            value,
            span: Some((start, end)),
            slot: Some(SlotRef {
                header_at,
                schema_index: item.schema_index,
                declared: storage_kind(&slot.property.inner),
            }),
        });
    }
    if diagnostics.declared_slots {
        emit_unset(
            cursor,
            schema,
            ctx,
            diagnostics,
            header_at,
            next_slot..schema.len(),
            entries,
        );
    }
    Ok(())
}

/// Entries for the slots a header skips, placed where a value for them would be inserted: after
/// the previous stored value and before the next, which is where the cursor stands between items.
fn emit_unset(
    cursor: &Cursor<'_>,
    schema: &Schema<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    header_at: u64,
    slots: std::ops::Range<usize>,
    entries: &mut Vec<PropertyEntry>,
) {
    let at = cursor.file_offset();
    for index in slots {
        let Some(slot) = schema.slot(index) else {
            continue;
        };
        let declared = storage_kind(&slot.property.inner);
        record_unset(
            diagnostics,
            ctx,
            at,
            header_at,
            index,
            &slot.property.inner,
            false,
        );
        entries.push(PropertyEntry {
            name: slot.property.name.clone(),
            element: (slot.property.array_dim > 1).then_some(slot.element),
            value: PropertyValue::Unset {
                declared: typed_as(&slot.property.inner),
                enum_type: match &slot.property.inner {
                    PropertyInner::Enum { name, .. } => Some(name.clone()),
                    _ => None,
                },
                fields: unset_fields(&slot.property.inner, ctx, 0),
            },
            span: Some((at, at)),
            slot: Some(SlotRef {
                header_at,
                schema_index: index as u32,
                declared,
            }),
        });
    }
}

/// How deep an unset struct's preview goes into structs of its own.
pub const PREVIEW_DEPTH: u32 = 5;

/// The fields a reflected struct declares, each unset, for showing what storing it would hold.
/// A native struct lays itself out and names no fields a schema would, so it has none here.
pub(crate) fn unset_fields(inner: &PropertyInner, ctx: &Ctx<'_>, depth: u32) -> Vec<PropertyEntry> {
    let PropertyInner::Struct { name } = inner else {
        return Vec::new();
    };
    if depth >= PREVIEW_DEPTH {
        return Vec::new();
    }
    if let Some(fields) = native_fields(name, ctx) {
        return fields.into_iter().map(unset_preview).collect();
    }
    // A struct that writes itself has fields a schema would misname, or none at all.
    if reads_natively(name, ctx) {
        return Vec::new();
    }
    let Some(schema) = ctx.schema(name) else {
        return Vec::new();
    };
    (0..schema.len())
        .filter_map(|index| schema.slot(index))
        .map(|slot| PropertyEntry {
            name: slot.property.name.clone(),
            element: (slot.property.array_dim > 1).then_some(slot.element),
            value: PropertyValue::Unset {
                declared: typed_as(&slot.property.inner),
                enum_type: enum_type_of(&slot.property.inner),
                fields: unset_fields(&slot.property.inner, ctx, depth + 1),
            },
            span: None,
            slot: None,
        })
        .collect()
}

/// The fields a zero struct holds, each zero, for showing and addressing them through it.
fn zero_fields(inner: &PropertyInner, ctx: &Ctx<'_>, depth: u32) -> Vec<PropertyEntry> {
    let PropertyInner::Struct { name } = inner else {
        return Vec::new();
    };
    if depth >= PREVIEW_DEPTH {
        return Vec::new();
    }
    if let Some(fields) = native_fields(name, ctx) {
        return fields;
    }
    if reads_natively(name, ctx) {
        return Vec::new();
    }
    let Some(schema) = ctx.schema(name) else {
        return Vec::new();
    };
    (0..schema.len())
        .filter_map(|index| schema.slot(index))
        .map(|slot| PropertyEntry {
            name: slot.property.name.clone(),
            element: (slot.property.array_dim > 1).then_some(slot.element),
            value: match zero_value(&slot.property.inner, ctx) {
                PropertyValue::Default { declared, .. } => PropertyValue::Default {
                    declared,
                    fields: zero_fields(&slot.property.inner, ctx, depth + 1),
                },
                other => other,
            },
            span: None,
            slot: None,
        })
        .collect()
}

/// Whether a struct serializes itself rather than through its schema. Asked of the native reader
/// itself over no bytes: it takes any struct it lays out, and fails to read it.
fn reads_natively(name: &str, ctx: &Ctx<'_>) -> bool {
    let mut scratch = Diagnostics::default();
    structs::read_native(name, &mut Cursor::new(&[], 0), ctx, &mut scratch, 0).is_some()
}

/// The native structs that hold a single value, which an edit types in whole.
const VALUE_STRUCTS: [&str; 5] = [
    "SoftObjectPath",
    "SoftClassPath",
    "TopLevelAssetPath",
    "MarvelSoftObjectPath",
    "Guid",
];

/// The type an edit writes a slot as: its stored kind, or for a native struct that holds a single
/// value, that struct's name, which is what says how to write it.
pub(crate) fn typed_as(inner: &PropertyInner) -> &'static str {
    match inner {
        PropertyInner::Struct { name } => VALUE_STRUCTS
            .iter()
            .find(|held| **held == name.as_str())
            .copied()
            .unwrap_or("Struct"),
        other => storage_kind(other),
    }
}

/// A native struct's fields, read from the bytes it holds when every field is default. It lays
/// itself out, so no schema names them. `None` for a struct that is not native, or whose layout
/// is one value rather than fields.
fn native_fields(name: &str, ctx: &Ctx<'_>) -> Option<Vec<PropertyEntry>> {
    let mut bytes = Vec::new();
    for part in native_parts(name, ctx)?? {
        match part {
            DefaultPart::Bytes(held) => bytes.extend(held),
            DefaultPart::NoneName => {
                let none = ctx
                    .names()
                    .copy_raw_names()
                    .iter()
                    .position(|held| held == "None")?;
                bytes.extend_from_slice(&(none as i32).to_le_bytes());
                bytes.extend_from_slice(&0i32.to_le_bytes());
            }
            DefaultPart::Struct(_) => return None,
        }
    }
    let mut cursor = Cursor::new(&bytes, 0);
    let mut scratch = Diagnostics::default();
    match structs::read_native(name, &mut cursor, ctx, &mut scratch, 0)? {
        Ok(PropertyValue::Struct { fields, .. }) => Some(without_places(fields)),
        _ => None,
    }
}

/// Entries read from bytes that are not the package's, so no offset in them means anything.
fn without_places(entries: Vec<PropertyEntry>) -> Vec<PropertyEntry> {
    entries
        .into_iter()
        .map(|mut entry| {
            entry.span = None;
            entry.slot = None;
            if let PropertyValue::Struct { fields, .. } = &mut entry.value {
                *fields = without_places(std::mem::take(fields));
            }
            entry
        })
        .collect()
}

/// A field as an unset struct would hold it: not stored, of the type its default has.
fn unset_preview(entry: PropertyEntry) -> PropertyEntry {
    let (declared, fields) = match entry.value {
        PropertyValue::Bool { .. } => ("Bool", Vec::new()),
        PropertyValue::Int { .. } => ("Int", Vec::new()),
        PropertyValue::UInt { .. } => ("UInt32", Vec::new()),
        PropertyValue::Float { .. } => ("Float", Vec::new()),
        PropertyValue::Byte { .. } => ("Byte", Vec::new()),
        PropertyValue::Str { .. } => ("Str", Vec::new()),
        PropertyValue::Name { .. } | PropertyValue::Enum { .. } => ("Name", Vec::new()),
        PropertyValue::Text { .. } => ("Text", Vec::new()),
        PropertyValue::Object { .. } => ("Object", Vec::new()),
        PropertyValue::SoftObject { .. } => ("SoftObject", Vec::new()),
        PropertyValue::Array { .. } => ("Array", Vec::new()),
        PropertyValue::Struct { fields, .. } => {
            ("Struct", fields.into_iter().map(unset_preview).collect())
        }
        _ => ("Struct", Vec::new()),
    };
    PropertyEntry {
        value: PropertyValue::Unset {
            declared,
            enum_type: None,
            fields,
        },
        ..entry
    }
}

/// What storing a slot that holds no bytes takes: its minimal form, and for a container, how an
/// element is written into it.
fn record_unset(
    diagnostics: &mut Diagnostics,
    ctx: &Ctx<'_>,
    at: u64,
    header_at: u64,
    index: usize,
    inner: &PropertyInner,
    zero: bool,
) {
    let (default_bytes, default_recipe) = split_default(unset_default(inner, ctx));
    let zero_bytes = match inner {
        // An instanced struct lays itself out, so a zero one is stored as one with no type.
        PropertyInner::Struct { name }
            if matches!(name.as_str(), "InstancedStruct" | "ConstStruct") =>
        {
            None
        }
        PropertyInner::Struct { name } if zero && native_parts(name, ctx).is_none() => ctx
            .schema(name)
            .and_then(|schema| unversioned::zero_header(schema.len()).ok()),
        _ => None,
    };
    diagnostics.unset.push(UnsetSlot {
        at,
        header_at,
        schema_index: index as u32,
        declared: storage_kind(inner),
        struct_name: match inner {
            PropertyInner::Struct { name } => Some(name.clone()),
            _ => None,
        },
        default_bytes,
        default_recipe,
        zero_bytes,
    });
    match inner {
        PropertyInner::Array { inner } => {
            record_container(diagnostics, ctx, at, at, Vec::new(), inner, None)
        }
        PropertyInner::MulticastDelegate => record_container(
            diagnostics,
            ctx,
            at,
            at,
            Vec::new(),
            &PropertyInner::Delegate,
            None,
        ),
        PropertyInner::Set { key } => {
            record_container(diagnostics, ctx, at, at, Vec::new(), key, None)
        }
        PropertyInner::Map { key, value } => record_container(
            diagnostics,
            ctx,
            at,
            at,
            Vec::new(),
            value,
            Some((Vec::new(), key)),
        ),
        _ => return,
    }
    if let Some(layout) = diagnostics.containers.last_mut() {
        layout.absent = Some((header_at, index as u32));
        layout.elements_at = Some(at);
    }
}

/// The bytes a slot would hold if stored with nothing in it, for the kinds that need no name from
/// the package to spell that. Scalars and strings need a typed value instead, and the kinds built
/// from names are left to the editor, which has the name map.
fn unset_default(inner: &PropertyInner, ctx: &Ctx<'_>) -> Option<Vec<DefaultPart>> {
    let bytes = match inner {
        PropertyInner::Struct { name } => match name.as_str() {
            // No type, and no payload.
            "InstancedStruct" | "ConstStruct" => vec![0u8; 8],
            _ => {
                return match native_parts(name, ctx) {
                    Some(parts) => parts,
                    None => ctx.schema(name).map(|schema| {
                        vec![DefaultPart::Bytes(unversioned::empty_header(schema.len()))]
                    }),
                };
            }
        },
        PropertyInner::Array { .. } | PropertyInner::MulticastDelegate => {
            0i32.to_le_bytes().to_vec()
        }
        PropertyInner::Set { .. } | PropertyInner::Map { .. } => vec![0u8; 8],
        PropertyInner::Object | PropertyInner::WeakObject | PropertyInner::Interface => {
            vec![0u8; 4]
        }
        PropertyInner::LazyObject => vec![0u8; 16],
        PropertyInner::FieldPath => vec![0u8; 8],
        PropertyInner::Delegate => {
            return Some(vec![
                DefaultPart::Bytes(vec![0u8; 4]),
                DefaultPart::NoneName,
            ]);
        }
        // Flags, the None history, and no culture-invariant string.
        PropertyInner::Text => {
            let mut out = vec![0u8; 4];
            out.push(0xFF);
            out.extend_from_slice(&0u32.to_le_bytes());
            out
        }
        _ => return None,
    };
    Some(vec![DefaultPart::Bytes(bytes)])
}

/// A native struct's default with the reflected blocks it embeds resolved through the schemas at
/// hand. `None` for a struct that is not native; `Some(None)` when a schema it needs is missing.
pub(crate) fn native_parts(name: &str, ctx: &Ctx<'_>) -> Option<Option<Vec<DefaultPart>>> {
    let parts = match structs::native_default(name) {
        structs::NativeDefault::NotNative => return None,
        structs::NativeDefault::Fixed(bytes) => vec![DefaultPart::Bytes(bytes)],
        structs::NativeDefault::Recipe(parts) => parts,
    };
    let mut resolved = Vec::with_capacity(parts.len());
    for part in parts {
        resolved.push(match part {
            DefaultPart::Struct(inner) => match ctx.schema(inner) {
                Some(schema) => DefaultPart::Bytes(unversioned::empty_header(schema.len())),
                None => return Some(None),
            },
            other => other,
        });
    }
    Some(Some(resolved))
}

/// Parts that are all bytes collapse into the bytes themselves; anything spelling a name stays a
/// recipe for the editor to finish.
fn split_default(parts: Option<Vec<DefaultPart>>) -> (Option<Vec<u8>>, Option<Vec<DefaultPart>>) {
    let Some(parts) = parts else {
        return (None, None);
    };
    if parts
        .iter()
        .all(|part| matches!(part, DefaultPart::Bytes(_)))
    {
        let bytes = parts
            .into_iter()
            .flat_map(|part| match part {
                DefaultPart::Bytes(bytes) => bytes,
                _ => Vec::new(),
            })
            .collect();
        return (Some(bytes), None);
    }
    (None, Some(parts))
}

/// One element of an array, set or map.
///
/// Unversioned serialization writes a struct's own enum properties as the raw underlying integer,
/// because the schema already names the type. A container element does not go through that path:
/// `FArrayProperty::SerializeItem` calls the inner property's own `SerializeItem`, and
/// `FEnumProperty` writes its value as the enumerator's `FName`. Reading these as bytes desyncs
/// the rest of the export by seven bytes per element.
fn read_element(
    inner: &PropertyInner,
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    depth: u32,
) -> Result<PropertyValue, String> {
    let PropertyInner::Enum { name, .. } = inner else {
        return read_value(inner, cursor, ctx, diagnostics, depth);
    };
    *diagnostics
        .property_kinds
        .entry(kind_name(inner))
        .or_default() += 1;
    let text = cursor.read_name(ctx.names())?;
    Ok(PropertyValue::Enum {
        value: ctx.enum_value(name, &text).unwrap_or_default(),
        name: Some(text),
        enum_type: Some(name.clone()),
    })
}

pub(crate) fn read_value(
    inner: &PropertyInner,
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    depth: u32,
) -> Result<PropertyValue, String> {
    if depth > MAX_STRUCT_DEPTH {
        return Err(cursor.err("property nesting is too deep"));
    }
    *diagnostics
        .property_kinds
        .entry(kind_name(inner))
        .or_default() += 1;
    let value = match inner {
        PropertyInner::Bool => PropertyValue::Bool {
            value: cursor.read_u8()? != 0,
        },
        PropertyInner::Int8 => PropertyValue::Int {
            value: i64::from(cursor.read_i8()?),
        },
        PropertyInner::Int16 => PropertyValue::Int {
            value: i64::from(cursor.read_i16()?),
        },
        PropertyInner::Int => PropertyValue::Int {
            value: i64::from(cursor.read_i32()?),
        },
        PropertyInner::Int64 => PropertyValue::Int {
            value: cursor.read_i64()?,
        },
        PropertyInner::Byte => PropertyValue::Byte {
            value: cursor.read_u8()?,
        },
        PropertyInner::UInt16 => PropertyValue::UInt {
            value: u64::from(cursor.read_u16()?),
        },
        PropertyInner::UInt32 => PropertyValue::UInt {
            value: u64::from(cursor.read_u32()?),
        },
        PropertyInner::UInt64 => PropertyValue::UInt {
            value: cursor.read_u64()?,
        },
        PropertyInner::Float => PropertyValue::Float {
            value: f64::from(cursor.read_f32()?),
        },
        PropertyInner::Double => PropertyValue::Float {
            value: cursor.read_f64()?,
        },
        PropertyInner::Str | PropertyInner::Utf8Str | PropertyInner::AnsiStr => {
            PropertyValue::Str {
                value: cursor.read_string()?,
            }
        }
        PropertyInner::Name => PropertyValue::Name {
            value: cursor.read_name(ctx.names())?,
        },
        PropertyInner::Text => read_text(cursor, ctx, diagnostics, depth)?,
        PropertyInner::Object | PropertyInner::WeakObject | PropertyInner::Interface => {
            let index = read_index(cursor, diagnostics)?;
            PropertyValue::Object {
                index,
                path: ctx.object_path(index).map_err(|e| cursor.err(e))?,
            }
        }
        // Declared by the mappings but never seen serialized, so the FUniqueObjectGuid layout
        // here is still unconfirmed against real bytes.
        PropertyInner::LazyObject => {
            diagnostics
                .native_leaves
                .push((cursor.file_offset(), NativeLeaf::Guid));
            PropertyValue::LazyObject {
                guid: read_guid(cursor)?,
            }
        }
        PropertyInner::SoftObject | PropertyInner::AssetObject => {
            let package = cursor.read_name(ctx.names())?;
            let asset = cursor.read_name(ctx.names())?;
            let sub_path = cursor.read_string()?;
            PropertyValue::SoftObject {
                path: soft_path(&package, &asset, &sub_path),
            }
        }
        PropertyInner::Delegate => read_delegate(cursor, ctx, diagnostics)?,
        // The bindings are a list like an array's, so they are added and dropped like elements.
        PropertyInner::MulticastDelegate => {
            let at = cursor.file_offset();
            let count = read_count(cursor, "multicast delegate")?;
            let mut items = Vec::with_capacity(count);
            let mut elements = Vec::with_capacity(count);
            for _ in 0..count {
                let start = cursor.file_offset();
                items.push(read_delegate(cursor, ctx, diagnostics)?);
                elements.push((start, cursor.file_offset()));
            }
            let binding = PropertyInner::Delegate;
            record_container(diagnostics, ctx, at, at, elements, &binding, None);
            PropertyValue::Array { items }
        }
        PropertyInner::FieldPath => {
            let (path, owner) = read_field_path(cursor, ctx, diagnostics)?;
            PropertyValue::FieldPath {
                path,
                owner: ctx.object_path(owner).map_err(|e| cursor.err(e))?,
            }
        }
        PropertyInner::Enum { inner, name } => {
            let raw = match read_value(inner, cursor, ctx, diagnostics, depth + 1)? {
                PropertyValue::Byte { value } => i64::from(value),
                PropertyValue::Int { value } => value,
                PropertyValue::UInt { value } => value as i64,
                other => {
                    return Err(cursor.err(format!(
                        "enum {name} has a non-numeric underlying value {other:?}"
                    )));
                }
            };
            PropertyValue::Enum {
                value: raw,
                name: ctx.enum_name(name, raw),
                enum_type: Some(name.clone()),
            }
        }
        PropertyInner::Array { inner } => {
            let at = cursor.file_offset();
            let count_at = at;
            let count = read_count(cursor, "array")?;
            let mut items = Vec::with_capacity(count.min(4096));
            let mut elements = Vec::with_capacity(count.min(4096));
            for _ in 0..count {
                let start = cursor.file_offset();
                items.push(read_element(inner, cursor, ctx, diagnostics, depth + 1)?);
                elements.push((start, cursor.file_offset()));
            }
            record_container(diagnostics, ctx, at, count_at, elements, inner, None);
            PropertyValue::Array { items }
        }
        PropertyInner::Set { key } => {
            let at = cursor.file_offset();
            read_removed(key, cursor, ctx, diagnostics, depth, "set removal")?;
            let count_at = cursor.file_offset();
            let count = read_count(cursor, "set")?;
            let mut items = Vec::with_capacity(count.min(4096));
            let mut elements = Vec::with_capacity(count.min(4096));
            for _ in 0..count {
                let start = cursor.file_offset();
                items.push(read_element(key, cursor, ctx, diagnostics, depth + 1)?);
                elements.push((start, cursor.file_offset()));
            }
            record_container(diagnostics, ctx, at, count_at, elements, key, None);
            PropertyValue::Set { items }
        }
        PropertyInner::Map { key, value } => {
            let at = cursor.file_offset();
            read_removed(key, cursor, ctx, diagnostics, depth, "map removal")?;
            let count_at = cursor.file_offset();
            let count = read_count(cursor, "map")?;
            let mut entries = Vec::with_capacity(count.min(4096));
            let mut elements = Vec::with_capacity(count.min(4096));
            let mut keys = Vec::with_capacity(count.min(4096));
            for _ in 0..count {
                let start = cursor.file_offset();
                let pair_key = read_element(key, cursor, ctx, diagnostics, depth + 1)?;
                keys.push((start, cursor.file_offset()));
                entries.push(MapEntry {
                    key: pair_key,
                    value: read_element(value, cursor, ctx, diagnostics, depth + 1)?,
                });
                elements.push((start, cursor.file_offset()));
            }
            record_container(
                diagnostics,
                ctx,
                at,
                count_at,
                elements,
                value,
                Some((keys, key)),
            );
            PropertyValue::Map { entries }
        }
        // Declared nowhere in the shipped mappings, so the layout was never confirmed against
        // real bytes. Failing by name beats emitting a plausible wrong value.
        PropertyInner::Optional { .. } => {
            diagnostics.unsupported.insert("OptionalProperty".into());
            return Err(cursor.err("TOptional property serialization is not implemented"));
        }
        PropertyInner::Struct { name } => match name.as_str() {
            "InstancedStruct" | "ConstStruct" => {
                read_instanced_struct(name, cursor, ctx, diagnostics, depth + 1)?
            }
            _ => read_struct(name, cursor, ctx, diagnostics, depth + 1)?,
        },
        PropertyInner::Unknown => {
            diagnostics
                .unsupported
                .insert("Unknown property type".into());
            return Err(cursor.err("mappings declare an unknown property type"));
        }
    };
    Ok(value)
}

/// `FInstancedStruct` writes the struct type as an object reference followed by the byte length
/// of the payload. That length is what makes this safe: whatever happens to the inner parse, the
/// cursor is placed exactly at the end of the payload afterwards. The type shows as a field of its
/// own, first, so it can be read and set like any reference; one with no type is a struct holding
/// that field alone, named as declared.
fn read_instanced_struct(
    declared: &str,
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    depth: u32,
) -> Result<PropertyValue, String> {
    let type_at = cursor.file_offset();
    let type_index = read_index(cursor, diagnostics)?;
    let size_at = cursor.file_offset();
    let serial_size = cursor.read_i32()?;
    if serial_size < 0 || serial_size as usize > cursor.remaining() {
        return Err(cursor.err(format!("implausible instanced struct size {serial_size}")));
    }
    let payload_end = cursor.position() + serial_size as usize;
    let payload_start = cursor.file_offset();
    let payload_finish = payload_start + serial_size as u64;
    diagnostics.instanced.push(InstancedLayout {
        size_at,
        payload_start,
        payload_end: payload_finish,
    });

    let type_path = ctx.object_path(type_index).map_err(|e| cursor.err(e))?;
    let type_name = type_path.as_deref().map(last_segment);
    let typed = PropertyEntry {
        name: TYPE_FIELD.into(),
        element: None,
        value: PropertyValue::Object {
            index: type_index,
            path: type_path,
        },
        span: Some((type_at, type_at + 4)),
        slot: None,
    };

    let marks = diagnostics.marks();
    let value = match type_name {
        Some(name) if serial_size > 0 => {
            let parsed = read_struct(&name, cursor, ctx, diagnostics, depth);
            match parsed {
                Ok(PropertyValue::Struct { name, mut fields }) => {
                    fields.insert(0, typed);
                    PropertyValue::Struct { name, fields }
                }
                // A native struct that holds one value shows it as a field beside its type.
                Ok(value) => PropertyValue::Struct {
                    name,
                    fields: vec![
                        typed,
                        PropertyEntry {
                            name: "Value".into(),
                            element: None,
                            value,
                            span: Some((payload_start, payload_finish)),
                            slot: None,
                        },
                    ],
                },
                Err(reason) => {
                    diagnostics.rewind(&marks);
                    diagnostics.undecoded.push(UndecodedPayload {
                        struct_name: name.clone(),
                        at: payload_start,
                        end: payload_finish,
                        reason: reason.clone(),
                    });
                    PropertyValue::Struct {
                        name,
                        fields: vec![
                            typed,
                            PropertyEntry {
                                name: UNDECODED_FIELD.into(),
                                element: None,
                                span: None,
                                slot: None,
                                value: PropertyValue::Undecoded {
                                    reason,
                                    bytes: serial_size as u64,
                                },
                            },
                        ],
                    }
                }
            }
        }
        Some(name) => PropertyValue::Struct {
            name,
            fields: vec![typed],
        },
        None => PropertyValue::Struct {
            name: declared.to_string(),
            fields: vec![typed],
        },
    };

    cursor.seek_to(payload_end)?;
    Ok(value)
}

fn last_segment(path: &str) -> String {
    path.rsplit(['/', '.', ':'])
        .next()
        .unwrap_or(path)
        .to_string()
}

pub(crate) fn read_struct(
    name: &str,
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    depth: u32,
) -> Result<PropertyValue, String> {
    if depth > MAX_STRUCT_DEPTH {
        return Err(cursor.err("property nesting is too deep"));
    }
    if let Some(result) = structs::read_native(name, cursor, ctx, diagnostics, depth) {
        return result;
    }
    let Some(schema) = ctx.schema(name) else {
        diagnostics.unresolved_structs.insert(name.to_string());
        return Err(cursor.err(format!(
            "struct {name} has no native layout and no schema in the mappings file"
        )));
    };
    let mut fields = Vec::new();
    read_property_block(cursor, &schema, ctx, diagnostics, depth, &mut fields)?;
    Ok(PropertyValue::Struct {
        name: name.to_string(),
        fields,
    })
}

/// `FScriptDelegate`: the bound object as a package index, then the function name.
fn read_delegate(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
) -> Result<PropertyValue, String> {
    let index = read_index(cursor, diagnostics)?;
    let function = cursor.read_name(ctx.names())?;
    Ok(PropertyValue::Delegate {
        object: ctx.object_path(index).map_err(|e| cursor.err(e))?,
        function,
    })
}

/// An `FPackageIndex`, remembered by position when it points at something.
/// An `FFieldPath`: the name chain that reaches the property, then the object that owns it. The
/// bytecode reader needs the same bytes, so it lives here rather than being written twice.
pub(crate) fn read_field_path(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
) -> Result<(String, i32), String> {
    let count = read_count(cursor, "field path")?;
    let mut names = Vec::with_capacity(count);
    for _ in 0..count {
        names.push(cursor.read_name(ctx.names())?);
    }
    let owner = read_index(cursor, diagnostics)?;
    Ok((names.join("."), owner))
}

pub(crate) fn read_index(
    cursor: &mut Cursor<'_>,
    diagnostics: &mut Diagnostics,
) -> Result<i32, String> {
    let at = cursor.file_offset();
    let index = cursor.read_i32()?;
    if index != 0 {
        diagnostics.references.push(IndexRef { at, index });
    }
    Ok(index)
}

fn read_guid(cursor: &mut Cursor<'_>) -> Result<String, String> {
    let mut parts = [0u32; 4];
    for part in &mut parts {
        *part = cursor.read_u32()?;
    }
    Ok(format!(
        "{:08X}{:08X}{:08X}{:08X}",
        parts[0], parts[1], parts[2], parts[3]
    ))
}

/// A field of a natively serialized struct, with the bytes it owns recorded so it edits in place.
pub(crate) fn spanned(
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

/// A cooked set or map opens with the keys it removes from the defaults it inherits, each one a
/// full key. They are not contents, so only their width and their object references matter.
fn read_removed(
    key: &PropertyInner,
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    depth: u32,
    what: &str,
) -> Result<(), String> {
    let count = read_count(cursor, what)?;
    for _ in 0..count {
        read_element(key, cursor, ctx, diagnostics, depth + 1)?;
    }
    Ok(())
}

/// Container counts come straight off disk, so a desynced stream shows up here as an absurd
/// length rather than a multi-gigabyte allocation.
pub(crate) fn read_count(cursor: &mut Cursor<'_>, what: &str) -> Result<usize, String> {
    let count = cursor.read_i32()?;
    if count < 0 {
        return Err(cursor.err(format!("negative {what} count {count}")));
    }
    let count = count as usize;
    if count > cursor.remaining() + 1 {
        return Err(cursor.err(format!(
            "{what} count {count} exceeds the {} bytes left in this export",
            cursor.remaining()
        )));
    }
    Ok(count)
}

/// `ETextHistoryType` decides the payload, laid out as UE 5.3 writes each. A text built from
/// pieces keeps them as parts, each spanned so it edits in place, named after the history's own
/// members; the text shows what they make. Layouts this reader is unsure of fail by name.
pub(crate) fn read_text(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    depth: u32,
) -> Result<PropertyValue, String> {
    if depth > MAX_STRUCT_DEPTH {
        return Err(cursor.err("text nesting is too deep"));
    }
    let flags = cursor.read_u32()?;
    let history = cursor.read_i8()?;
    *diagnostics.text_histories.entry(history).or_default() += 1;
    *diagnostics.text_flags.entry((history, flags)).or_default() += 1;
    let mut parts = Vec::new();
    let (mut namespace, mut key) = (None, None);
    let value = match history {
        -1 => {
            if cursor.read_bool32()? {
                Some(cursor.read_string()?)
            } else {
                None
            }
        }
        0 => {
            namespace = Some(cursor.read_string()?);
            key = Some(cursor.read_string()?);
            Some(cursor.read_string()?)
        }
        10 => {
            let source = text_part(cursor, ctx, diagnostics, depth, "SourceText", &mut parts)?;
            parts.push(spanned(cursor, "TransformType", byte_value)?);
            source
        }
        // NamedFormat, OrderedFormat and ArgumentFormat: the pattern, then its arguments.
        1..=3 => {
            let pattern = text_part(cursor, ctx, diagnostics, depth, "SourceFmt", &mut parts)?
                .unwrap_or_default();
            let at = cursor.file_offset();
            let count = read_count(cursor, "format argument")?;
            let mut fields = Vec::with_capacity(count);
            let mut shown = Vec::with_capacity(count);
            for position in 0..count {
                let name = match history {
                    2 => position.to_string(),
                    _ => cursor.read_string()?,
                };
                // `FFormatArgumentData` writes its type as a byte, which is the same bits.
                let kind = cursor.read_i8()?;
                if history == 3 && kind == 1 {
                    return Err(cursor.err(
                        "an ArgumentFormat text holds an unsigned argument, whose layout this \
                         reader is not sure of",
                    ));
                }
                let value =
                    argument_value(kind, cursor, ctx, diagnostics, depth, &name, &mut fields)?;
                shown.push((name, value.unwrap_or_default()));
            }
            parts.push(PropertyEntry {
                name: "Arguments".into(),
                element: None,
                value: PropertyValue::Struct {
                    name: "Arguments".into(),
                    fields,
                },
                span: Some((at, cursor.file_offset())),
                slot: None,
            });
            Some(format_pattern(&pattern, &shown))
        }
        // AsNumber, AsPercent and AsCurrency, `FTextHistory_FormatNumber`: a currency's code, the
        // source value, a full-word flag for the formatting options, the options, then the
        // culture.
        4..=6 => {
            if history == 6 {
                parts.push(spanned(cursor, "CurrencyCode", string_value)?);
            }
            let kind = cursor.read_i8()?;
            let source = argument_value(
                kind,
                cursor,
                ctx,
                diagnostics,
                depth,
                "SourceValue",
                &mut parts,
            )?;
            if cursor.read_bool32()? {
                parts.push(spanned(cursor, "FormatOptions", number_options)?);
            }
            parts.push(spanned(cursor, "TargetCulture", string_value)?);
            source
        }
        // AsDate, AsTime and AsDateTime: the moment, the styles the history has, the time zone
        // and the culture.
        7..=9 => {
            let moment = spanned(cursor, "SourceDateTime", |c| {
                Ok(PropertyValue::Int {
                    value: c.read_i64()?,
                })
            })?;
            let shown = match moment.value {
                PropertyValue::Int { value } => ticks_text(value),
                _ => String::new(),
            };
            parts.push(moment);
            let styles: &[&str] = match history {
                7 => &["DateStyle"],
                8 => &["TimeStyle"],
                _ => &["DateStyle", "TimeStyle"],
            };
            for style in styles {
                let part = spanned(cursor, style, |c| {
                    Ok(PropertyValue::Int {
                        value: i64::from(c.read_i8()?),
                    })
                })?;
                // `EDateTimeStyle::Custom` brings a pattern of its own, written in a way this
                // reader is not sure of.
                if matches!(part.value, PropertyValue::Int { value: 5 }) {
                    return Err(cursor.err(format!(
                        "a {} text with a custom {style}, whose pattern this reader is not sure of",
                        history_name(history)
                    )));
                }
                parts.push(part);
            }
            parts.push(spanned(cursor, "TimeZone", string_value)?);
            parts.push(spanned(cursor, "TargetCulture", string_value)?);
            Some(shown)
        }
        // TextGenerator: the generator's type, then for any but `None` the bytes it reads back.
        12 => {
            let id = spanned(cursor, "GeneratorTypeID", |c| {
                Ok(PropertyValue::Name {
                    value: c.read_name(ctx.names())?,
                })
            })?;
            let generator = id.value.summary();
            parts.push(id);
            if generator != "None" {
                let at = cursor.file_offset();
                let count = read_count(cursor, "generator content")?;
                let mut items = Vec::with_capacity(count);
                let mut elements = Vec::with_capacity(count);
                for _ in 0..count {
                    let start = cursor.file_offset();
                    items.push(byte_value(cursor)?);
                    elements.push((start, cursor.file_offset()));
                }
                record_container(
                    diagnostics,
                    ctx,
                    at,
                    at,
                    elements,
                    &PropertyInner::Byte,
                    None,
                );
                parts.push(PropertyEntry {
                    name: "GeneratorContents".into(),
                    element: None,
                    value: PropertyValue::Array { items },
                    span: Some((at, cursor.file_offset())),
                    slot: None,
                });
            }
            Some(format!("(generated by {generator})"))
        }
        11 => {
            diagnostics
                .native_leaves
                .push((cursor.file_offset(), NativeLeaf::StringTableId));
            let table = spanned(cursor, "TableId", |c| {
                Ok(PropertyValue::Name {
                    value: c.read_name(ctx.names())?,
                })
            })?;
            let key = spanned(cursor, "Key", string_value)?;
            let shown = format!("{}:{}", table.value.summary(), key.value.summary());
            parts.push(table);
            parts.push(key);
            Some(shown)
        }
        other => {
            return Err(cursor.err(format!(
                "unsupported FText history type {other} ({})",
                history_name(other)
            )));
        }
    };
    Ok(PropertyValue::Text {
        value,
        parts,
        namespace,
        key,
        display: None,
    })
}

/// A text nested in another, kept as a part so it can be edited as a text of its own; the
/// enclosing text shows what it displays.
fn text_part(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    depth: u32,
    name: &str,
    parts: &mut Vec<PropertyEntry>,
) -> Result<Option<String>, String> {
    let part = spanned(cursor, name, |c| read_text(c, ctx, diagnostics, depth + 1))?;
    let shown = match &part.value {
        PropertyValue::Text { value, .. } => value.clone(),
        other => Some(other.summary()),
    };
    parts.push(part);
    Ok(shown)
}

fn string_value(cursor: &mut Cursor<'_>) -> Result<PropertyValue, String> {
    Ok(PropertyValue::Str {
        value: cursor.read_string()?,
    })
}

fn byte_value(cursor: &mut Cursor<'_>) -> Result<PropertyValue, String> {
    Ok(PropertyValue::Byte {
        value: cursor.read_u8()?,
    })
}

/// `FNumberFormattingOptions`: two full-word bools, a rounding mode byte and four `int32` digit
/// counts.
fn number_options(cursor: &mut Cursor<'_>) -> Result<PropertyValue, String> {
    let flag = |c: &mut Cursor<'_>| {
        Ok(PropertyValue::Bool {
            value: c.read_bool32()?,
        })
    };
    let digits = |c: &mut Cursor<'_>| {
        Ok(PropertyValue::Int {
            value: i64::from(c.read_i32()?),
        })
    };
    let mut fields = vec![
        spanned(cursor, "AlwaysSign", flag)?,
        spanned(cursor, "UseGrouping", flag)?,
        spanned(cursor, "RoundingMode", |c| {
            Ok(PropertyValue::Int {
                value: i64::from(c.read_i8()?),
            })
        })?,
    ];
    for name in [
        "MinimumIntegralDigits",
        "MaximumIntegralDigits",
        "MinimumFractionalDigits",
        "MaximumFractionalDigits",
    ] {
        fields.push(spanned(cursor, name, digits)?);
    }
    Ok(PropertyValue::Struct {
        name: "NumberFormattingOptions".into(),
        fields,
    })
}

/// The value of an `FFormatArgumentValue` or `FFormatArgumentData` of type `kind`, as a part named
/// `name` spanning the value alone, so an edit keeps the type. Returns what it shows as.
fn argument_value(
    kind: i8,
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    depth: u32,
    name: &str,
    parts: &mut Vec<PropertyEntry>,
) -> Result<Option<String>, String> {
    let start = cursor.file_offset();
    let (value, shown) = match kind {
        0 => {
            let value = cursor.read_i64()?;
            (PropertyValue::Int { value }, value.to_string())
        }
        1 => {
            let value = cursor.read_u64()?;
            (PropertyValue::UInt { value }, value.to_string())
        }
        2 => {
            let value = cursor.read_f32()?;
            let shown = value.to_string();
            (
                PropertyValue::Float {
                    value: f64::from(value),
                },
                shown,
            )
        }
        3 => {
            let value = cursor.read_f64()?;
            (PropertyValue::Float { value }, value.to_string())
        }
        4 => return text_part(cursor, ctx, diagnostics, depth, name, parts),
        5 => {
            let value = cursor.read_u8()?;
            (PropertyValue::Byte { value }, format!("gender {value}"))
        }
        other => {
            return Err(cursor.err(format!("unknown FText format argument type {other}")));
        }
    };
    parts.push(PropertyEntry {
        name: name.to_string(),
        element: None,
        value,
        span: Some((start, cursor.file_offset())),
        slot: None,
    });
    Ok(Some(shown))
}

/// A format pattern with each `{argument}` it names replaced by what that argument shows.
pub(crate) fn format_pattern(pattern: &str, arguments: &[(String, String)]) -> String {
    let mut out = pattern.to_string();
    for (name, shown) in arguments {
        out = out.replace(&format!("{{{name}}}"), shown);
    }
    out
}

/// An `FDateTime`'s ticks, the hundred-nanosecond steps since 0001-01-01, as the moment they name.
pub(crate) fn ticks_text(ticks: i64) -> String {
    const PER_SECOND: i64 = 10_000_000;
    const PER_DAY: i64 = 86_400 * PER_SECOND;
    let days = ticks.div_euclid(PER_DAY);
    let seconds = ticks.rem_euclid(PER_DAY) / PER_SECOND;
    // Days since 0001-01-01 counted from 0000-03-01, as the civil calendar computes them.
    let z = days + 306;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted + 2) / 5 + 1;
    let month = if shifted < 10 {
        shifted + 3
    } else {
        shifted - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

pub(crate) fn history_name(history: i8) -> &'static str {
    match history {
        1 => "NamedFormat",
        2 => "OrderedFormat",
        3 => "ArgumentFormat",
        4 => "AsNumber",
        5 => "AsPercent",
        6 => "AsCurrency",
        7 => "AsDate",
        8 => "AsTime",
        9 => "AsDateTime",
        12 => "TextGenerator",
        _ => "unknown",
    }
}

fn soft_path(package: &str, asset: &str, sub_path: &str) -> String {
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

/// Containers are the only values whose parts have no property entry to hang a span on, so their
/// layout is recorded here instead. Only worth keeping while an edit might want it, which is any
/// time the caller asked for the layout at all.
#[allow(clippy::too_many_arguments)]
pub(crate) fn record_container(
    diagnostics: &mut Diagnostics,
    ctx: &Ctx<'_>,
    at: u64,
    count_at: u64,
    elements: Vec<(u64, u64)>,
    element: &PropertyInner,
    keys: Option<(Vec<(u64, u64)>, &PropertyInner)>,
) {
    record_container_width(diagnostics, ctx, at, count_at, 4, elements, element, keys);
}

/// The same, for a list a native struct writes behind a count narrower than four bytes.
#[allow(clippy::too_many_arguments)]
pub(crate) fn record_container_width(
    diagnostics: &mut Diagnostics,
    ctx: &Ctx<'_>,
    at: u64,
    count_at: u64,
    count_width: u8,
    elements: Vec<(u64, u64)>,
    element: &PropertyInner,
    keys: Option<(Vec<(u64, u64)>, &PropertyInner)>,
) {
    let (default_element, default_name, default_recipe) = element_default(element, ctx);
    diagnostics.containers.push(ContainerLayout {
        at,
        count_at,
        count_width,
        elements_at: None,
        absent: None,
        elements,
        element_kind: kind_name(element),
        default_element,
        element_is_enum: matches!(element, PropertyInner::Enum { .. }),
        element_enum: enum_type_of(element),
        default_name,
        default_recipe,
        keys: keys.map(|(spans, key)| {
            let (default, default_name, default_recipe) = element_default(key, ctx);
            MapKeys {
                spans,
                kind: kind_name(key),
                is_enum: matches!(key, PropertyInner::Enum { .. }),
                enum_type: enum_type_of(key),
                default,
                default_name,
                default_recipe,
            }
        }),
    });
}

/// A counted list of native structs, recorded as a container so it grows and shrinks like a
/// declared array: a new element copies a neighbour, or `recipe` when the list is empty.
pub(crate) fn native_list(
    cursor: &mut Cursor<'_>,
    diagnostics: &mut Diagnostics,
    name: &str,
    recipe: Vec<DefaultPart>,
    mut item: impl FnMut(&mut Cursor<'_>, &mut Diagnostics) -> Result<PropertyValue, String>,
) -> Result<PropertyEntry, String> {
    let at = cursor.file_offset();
    let count = list_count(cursor, name)?;
    let mut items = Vec::with_capacity(count);
    let mut elements = Vec::with_capacity(count);
    for _ in 0..count {
        let start = cursor.file_offset();
        items.push(item(cursor, diagnostics)?);
        elements.push((start, cursor.file_offset()));
    }
    let bytes_only = recipe
        .iter()
        .all(|part| matches!(part, DefaultPart::Bytes(_)));
    diagnostics.containers.push(ContainerLayout {
        at,
        count_at: at,
        count_width: 4,
        elements_at: None,
        absent: None,
        elements,
        element_kind: "Struct",
        default_element: bytes_only.then(|| {
            recipe
                .iter()
                .flat_map(|part| match part {
                    DefaultPart::Bytes(bytes) => bytes.clone(),
                    _ => Vec::new(),
                })
                .collect()
        }),
        element_is_enum: false,
        element_enum: None,
        default_name: None,
        default_recipe: (!bytes_only).then_some(recipe),
        keys: None,
    });
    Ok(PropertyEntry {
        name: name.to_string(),
        element: None,
        value: PropertyValue::Array { items },
        span: Some((at, cursor.file_offset())),
        slot: None,
    })
}

fn list_count(cursor: &mut Cursor<'_>, what: &str) -> Result<usize, String> {
    let count = cursor.read_i32()?;
    if count < 0 || count as usize > cursor.remaining() {
        return Err(cursor.err(format!("implausible {what} count {count}")));
    }
    Ok(count as usize)
}

fn enum_type_of(inner: &PropertyInner) -> Option<String> {
    match inner {
        PropertyInner::Enum { name, .. } => Some(name.clone()),
        _ => None,
    }
}

/// What a fresh element is written as: its bytes where the type alone settles them, the name it
/// spells where the package's name map has to be consulted at edit time, or a native struct's
/// recipe. None of them for the kinds whose default depends on something else, such as an
/// instanced struct's type.
type ElementDefault = (Option<Vec<u8>>, Option<String>, Option<Vec<DefaultPart>>);

fn element_default(inner: &PropertyInner, ctx: &Ctx<'_>) -> ElementDefault {
    let width = match inner {
        PropertyInner::Name => return (None, Some("None".to_string()), None),
        PropertyInner::Enum { name, .. } => return (None, ctx.enum_name(name, 0), None),
        PropertyInner::SoftObject | PropertyInner::AssetObject => {
            return (None, Some(String::new()), None);
        }
        PropertyInner::Bool | PropertyInner::Byte | PropertyInner::Int8 => 1,
        PropertyInner::Int16 | PropertyInner::UInt16 => 2,
        PropertyInner::Int | PropertyInner::UInt32 | PropertyInner::Float => 4,
        PropertyInner::Int64 | PropertyInner::UInt64 | PropertyInner::Double => 8,
        PropertyInner::Str | PropertyInner::Utf8Str | PropertyInner::AnsiStr => {
            return (Some(0i32.to_le_bytes().to_vec()), None, None);
        }
        other => {
            let (bytes, recipe) = split_default(unset_default(other, ctx));
            return (bytes, None, recipe);
        }
    };
    (Some(vec![0u8; width]), None, None)
}

/// An enum is stored as its underlying integer, so that is the type whose width matters when
/// a value has to be written from nothing.
fn storage_kind(inner: &PropertyInner) -> &'static str {
    match inner {
        PropertyInner::Enum { inner, .. } => kind_name(inner),
        other => kind_name(other),
    }
}

/// A value the header marked as all zero is not in the stream at all.
fn zero_value(inner: &PropertyInner, ctx: &Ctx<'_>) -> PropertyValue {
    match inner {
        PropertyInner::Bool => PropertyValue::Bool { value: false },
        PropertyInner::Int8 | PropertyInner::Int16 | PropertyInner::Int | PropertyInner::Int64 => {
            PropertyValue::Int { value: 0 }
        }
        PropertyInner::UInt16 | PropertyInner::UInt32 | PropertyInner::UInt64 => {
            PropertyValue::UInt { value: 0 }
        }
        PropertyInner::Byte => PropertyValue::Byte { value: 0 },
        PropertyInner::Float | PropertyInner::Double => PropertyValue::Float { value: 0.0 },
        PropertyInner::Str | PropertyInner::Utf8Str | PropertyInner::AnsiStr => {
            PropertyValue::Str {
                value: String::new(),
            }
        }
        PropertyInner::Name => PropertyValue::Name {
            value: "None".into(),
        },
        PropertyInner::Object | PropertyInner::WeakObject | PropertyInner::Interface => {
            PropertyValue::Object {
                index: 0,
                path: None,
            }
        }
        PropertyInner::LazyObject => PropertyValue::LazyObject {
            guid: "0".repeat(32),
        },
        PropertyInner::SoftObject | PropertyInner::AssetObject => PropertyValue::SoftObject {
            path: String::new(),
        },
        PropertyInner::Enum { name, .. } => PropertyValue::Enum {
            value: 0,
            name: ctx.enum_name(name, 0),
            enum_type: Some(name.clone()),
        },
        PropertyInner::Array { .. } | PropertyInner::MulticastDelegate => {
            PropertyValue::Array { items: Vec::new() }
        }
        PropertyInner::Set { .. } => PropertyValue::Set { items: Vec::new() },
        PropertyInner::Map { .. } => PropertyValue::Map {
            entries: Vec::new(),
        },
        _ => PropertyValue::Default {
            declared: Some(typed_as(inner)),
            fields: Vec::new(),
        },
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    use retoc::legacy_asset::FPackageNameMap;

    const NAMES: &[&str] = &["None", "OnFired", "Outer", "Inner"];

    fn header() -> FLegacyPackageHeader {
        FLegacyPackageHeader {
            name_map: FPackageNameMap::create_from_names(
                NAMES.iter().map(|n| (*n).to_string()).collect(),
            ),
            ..Default::default()
        }
    }

    fn write_name(out: &mut Vec<u8>, value: &str) {
        let index = NAMES
            .iter()
            .position(|n| *n == value)
            .expect("name is in the test name map") as i32;
        out.extend_from_slice(&index.to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
    }

    fn read_one(inner: &PropertyInner, data: &[u8]) -> Result<(PropertyValue, usize), String> {
        read_diagnosed(inner, data).map(|(value, consumed, _)| (value, consumed))
    }

    /// A package naming one import, so an instanced struct can declare a type by index.
    fn header_importing(name: &str) -> FLegacyPackageHeader {
        let mut names: Vec<String> = NAMES.iter().map(|n| (*n).to_string()).collect();
        names.push(name.to_string());
        let index = (names.len() - 1) as i32;
        FLegacyPackageHeader {
            name_map: FPackageNameMap::create_from_names(names),
            imports: vec![retoc::legacy_asset::FObjectImport {
                class_package: retoc::legacy_asset::FMinimalName {
                    index: 0,
                    number: 0,
                },
                class_name: retoc::legacy_asset::FMinimalName {
                    index: 0,
                    number: 0,
                },
                outer_index: FPackageIndex::create_null(),
                object_name: retoc::legacy_asset::FMinimalName { index, number: 0 },
                is_optional: false,
            }],
            ..Default::default()
        }
    }

    fn read_instanced(
        header: &FLegacyPackageHeader,
        data: &[u8],
    ) -> (PropertyValue, usize, Diagnostics) {
        let ctx = Ctx {
            mappings: None,
            header,
            fixups: None,
            synth: None,
            local: None,
        };
        let mut cursor = Cursor::new(data, 0);
        let mut diagnostics = Diagnostics::default();
        let value = read_value(
            &PropertyInner::Struct {
                name: "InstancedStruct".into(),
            },
            &mut cursor,
            &ctx,
            &mut diagnostics,
            0,
        )
        .expect("the length prefix always puts the cursor back");
        (value, cursor.position(), diagnostics)
    }

    /// A payload whose struct has no schema cannot be read, but its length prefix means the export
    /// walks on and reports as exact. Without a record of it the failure is invisible.
    #[test]
    fn an_instanced_struct_that_does_not_decode_is_counted_rather_than_hidden() {
        let header = header_importing("Broken");
        let mut data = (-1i32).to_le_bytes().to_vec();
        data.extend_from_slice(&3i32.to_le_bytes());
        data.extend_from_slice(&[0xAA, 0xBB, 0xCC]);
        let (value, consumed, diagnostics) = read_instanced(&header, &data);

        assert_eq!(
            consumed, 11,
            "the prefix puts the cursor on the payload end"
        );
        let PropertyValue::Struct { name, fields } = &value else {
            panic!("expected the placeholder struct, got {value:?}");
        };
        assert_eq!(name, "Broken");
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].name, TYPE_FIELD);
        assert_eq!(fields[1].name, UNDECODED_FIELD);
        assert!(
            matches!(fields[1].value, PropertyValue::Undecoded { bytes: 3, .. }),
            "the placeholder carries the payload length, not a stored string"
        );

        assert_eq!(diagnostics.undecoded.len(), 1);
        let payload = &diagnostics.undecoded[0];
        assert_eq!(payload.struct_name, "Broken");
        assert_eq!((payload.at, payload.end), (8, 11));
        assert!(payload.reason.contains("Broken"), "{}", payload.reason);
        assert_eq!(
            diagnostics.references.len(),
            1,
            "the type reference is read before the payload and stays"
        );
    }

    /// Anything the failed read recorded was read against a schema that did not fit, so it is
    /// forgotten. A reference kept at the wrong offset would be renumbered by a later removal,
    /// writing over bytes that mean something else.
    #[test]
    fn a_failed_payload_forgets_the_layouts_it_recorded() {
        let header = header_importing("SoftObjectPath");
        // A soft object path reads two names and a string; the string's length runs past the
        // payload, so the read fails after the names are already consumed.
        let mut payload = Vec::new();
        write_name(&mut payload, "Outer");
        write_name(&mut payload, "Inner");
        payload.extend_from_slice(&0x7fff_ffffi32.to_le_bytes());
        let mut data = (-1i32).to_le_bytes().to_vec();
        data.extend_from_slice(&(payload.len() as i32).to_le_bytes());
        data.extend_from_slice(&payload);

        let (_, consumed, diagnostics) = read_instanced(&header, &data);
        assert_eq!(consumed, 8 + payload.len());
        assert_eq!(diagnostics.undecoded.len(), 1);
        assert_eq!(
            diagnostics.references.len(),
            1,
            "only the type reference survives the rewind"
        );
        assert!(
            diagnostics.containers.is_empty(),
            "nothing recorded inside the failed payload is kept"
        );
    }

    fn read_diagnosed(
        inner: &PropertyInner,
        data: &[u8],
    ) -> Result<(PropertyValue, usize, Diagnostics), String> {
        let header = header();
        let ctx = Ctx {
            mappings: None,
            header: &header,
            fixups: None,
            synth: None,
            local: None,
        };
        let mut cursor = Cursor::new(data, 0);
        let mut diagnostics = Diagnostics::default();
        let value = read_value(inner, &mut cursor, &ctx, &mut diagnostics, 0)?;
        Ok((value, cursor.position(), diagnostics))
    }

    /// A set or map opens with the keys it removes from its inherited defaults, each a full key,
    /// before its own count. Skipping only the number takes the first removed key as the count.
    #[test]
    fn a_set_reads_the_keys_it_removes_before_its_own_elements() {
        let mut data = 1i32.to_le_bytes().to_vec();
        data.extend_from_slice(&7i32.to_le_bytes());
        data.extend_from_slice(&1i32.to_le_bytes());
        data.extend_from_slice(&9i32.to_le_bytes());
        let (value, consumed, diagnostics) = read_diagnosed(
            &PropertyInner::Set {
                key: Box::new(PropertyInner::Int),
            },
            &data,
        )
        .expect("set");
        assert_eq!(consumed, 16);
        assert!(matches!(&value, PropertyValue::Set { items }
            if items.len() == 1 && matches!(items[0], PropertyValue::Int { value: 9 })));
        let layout = diagnostics.containers.first().expect("layout");
        assert_eq!(
            (layout.at, layout.count_at),
            (0, 8),
            "the count sits after the removed keys"
        );
        assert_eq!(layout.elements, vec![(12, 16)]);
    }

    /// The shape a Niagara component's redirect map takes: two removed variables and no entries.
    #[test]
    fn a_map_with_removed_keys_and_no_entries_ends_after_the_count() {
        let mut data = 2i32.to_le_bytes().to_vec();
        write_name(&mut data, "Outer");
        write_name(&mut data, "Inner");
        data.extend_from_slice(&0i32.to_le_bytes());
        let (value, consumed, diagnostics) = read_diagnosed(
            &PropertyInner::Map {
                key: Box::new(PropertyInner::Name),
                value: Box::new(PropertyInner::Int),
            },
            &data,
        )
        .expect("map");
        assert_eq!(consumed, 24);
        assert!(matches!(&value, PropertyValue::Map { entries } if entries.is_empty()));
        let layout = diagnostics.containers.first().expect("layout");
        assert_eq!(layout.count_at, 20);
    }

    /// A struct's own enum property is the raw underlying integer, because the unversioned schema
    /// already names the type. Inside a container it is the enumerator's `FName` instead, which is
    /// eight bytes. Confusing the two desyncs everything after the array.
    #[test]
    fn an_enum_is_one_byte_on_its_own_and_a_name_inside_an_array() {
        let kind = PropertyInner::Enum {
            inner: Box::new(PropertyInner::Byte),
            name: "ETest".into(),
        };
        let (_, consumed) = read_one(&kind, &[3]).expect("bare enum");
        assert_eq!(consumed, 1);

        let mut data = 2i32.to_le_bytes().to_vec();
        write_name(&mut data, "OnFired");
        write_name(&mut data, "Outer");
        let (value, consumed) = read_one(
            &PropertyInner::Array {
                inner: Box::new(kind),
            },
            &data,
        )
        .expect("enum array");
        assert_eq!(consumed, data.len());
        let PropertyValue::Array { items } = value else {
            panic!("expected an array");
        };
        let names: Vec<_> = items
            .iter()
            .map(|item| match item {
                PropertyValue::Enum { name, .. } => name.clone().unwrap_or_default(),
                other => panic!("expected an enum, got {other:?}"),
            })
            .collect();
        assert_eq!(names, ["OnFired", "Outer"]);
    }

    /// The shape measured in a text render component: a double source value, the formatting
    /// options present, and an empty culture name.
    #[test]
    fn a_number_text_reads_its_source_value_options_and_culture() {
        let mut data = 0u32.to_le_bytes().to_vec();
        data.push(4);
        data.push(3);
        data.extend_from_slice(&2.0f64.to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());
        data.push(0);
        for digits in [1i32, 324, 0, 3] {
            data.extend_from_slice(&digits.to_le_bytes());
        }
        data.extend_from_slice(&0i32.to_le_bytes());
        let (value, consumed) = read_one(&PropertyInner::Text, &data).expect("text");
        assert_eq!(consumed, data.len());
        assert!(matches!(value, PropertyValue::Text { value: Some(text), .. } if text == "2"));
    }

    fn text_bytes(history: i8, body: &[u8]) -> Vec<u8> {
        let mut data = 0u32.to_le_bytes().to_vec();
        data.push(history as u8);
        data.extend_from_slice(body);
        data
    }

    fn fstring(out: &mut Vec<u8>, text: &str) {
        out.extend_from_slice(&(text.len() as i32 + 1).to_le_bytes());
        out.extend_from_slice(text.as_bytes());
        out.push(0);
    }

    /// A culture-invariant text, the simplest nested pattern.
    fn invariant(text: &str) -> Vec<u8> {
        let mut body = 1u32.to_le_bytes().to_vec();
        fstring(&mut body, text);
        text_bytes(-1, &body)
    }

    fn parts_of(value: PropertyValue) -> (Option<String>, Vec<PropertyEntry>) {
        match value {
            PropertyValue::Text { value, parts, .. } => (value, parts),
            other => panic!("expected a text, got {other:?}"),
        }
    }

    /// A localized text keeps the namespace and key its translations are filed under, next to the
    /// source it shows.
    #[test]
    fn a_localized_text_keeps_its_namespace_and_key() {
        let mut body = Vec::new();
        fstring(&mut body, "Menu");
        fstring(&mut body, "Quick");
        fstring(&mut body, "Quick Match");
        let data = text_bytes(0, &body);
        let (value, consumed) = read_one(&PropertyInner::Text, &data).expect("base");
        assert_eq!(consumed, data.len());
        let PropertyValue::Text {
            value,
            namespace,
            key,
            display,
            ..
        } = value
        else {
            panic!("a text");
        };
        assert_eq!(value.as_deref(), Some("Quick Match"));
        assert_eq!(namespace.as_deref(), Some("Menu"));
        assert_eq!(key.as_deref(), Some("Quick"));
        assert!(display.is_none(), "only a lookup after parsing fills it");
    }

    /// The transform and string table histories expose their pieces as parts.
    #[test]
    fn transformed_and_table_texts_read_their_pieces_as_parts() {
        let mut body = invariant("shout");
        body.push(2);
        let data = text_bytes(10, &body);
        let (value, consumed) = read_one(&PropertyInner::Text, &data).expect("transform");
        assert_eq!(consumed, data.len());
        let (shown, parts) = parts_of(value);
        assert_eq!(shown.as_deref(), Some("shout"));
        assert_eq!(parts[1].name, "TransformType");
        assert!(matches!(parts[1].value, PropertyValue::Byte { value: 2 }));

        let mut body = Vec::new();
        write_name(&mut body, "OnFired");
        fstring(&mut body, "Greeting");
        let data = text_bytes(11, &body);
        let (value, consumed) = read_one(&PropertyInner::Text, &data).expect("table entry");
        assert_eq!(consumed, data.len());
        let (shown, parts) = parts_of(value);
        assert_eq!(shown.as_deref(), Some("OnFired:Greeting"));
        assert_eq!(parts.len(), 2);
        assert!(matches!(&parts[0].value, PropertyValue::Name { value } if value == "OnFired"));
    }

    /// Every text read counts its flag word under its history, a nested one included, which is how
    /// the audit says what flags a rebuilt text should take.
    #[test]
    fn a_text_s_flags_are_counted_under_its_history() {
        let mut data = 0x8u32.to_le_bytes().to_vec();
        data.push(10);
        data.extend_from_slice(&invariant("shout"));
        data.push(1);
        let (_, consumed, diagnostics) = read_diagnosed(&PropertyInner::Text, &data).expect("read");
        assert_eq!(consumed, data.len());
        assert_eq!(diagnostics.text_flags.get(&(10, 0x8)), Some(&1));
        assert_eq!(diagnostics.text_flags.get(&(-1, 0)), Some(&1));
    }

    /// `FScriptDelegate` is a package index followed by the function name, twelve bytes in all.
    /// Pinning the width here is what stops a silent change from desynchronising every export
    /// that follows a delegate.
    #[test]
    fn a_delegate_is_an_object_index_followed_by_a_function_name() {
        let mut data = 0i32.to_le_bytes().to_vec();
        write_name(&mut data, "OnFired");
        let (value, consumed) = read_one(&PropertyInner::Delegate, &data).expect("delegate");
        assert_eq!(
            consumed, 12,
            "FScriptDelegate is 4 bytes of index plus an 8 byte FName"
        );
        assert!(
            matches!(value, PropertyValue::Delegate { object: None, function } if function == "OnFired")
        );
    }

    #[test]
    fn a_multicast_delegate_is_a_counted_list_of_delegates() {
        let mut data = 2i32.to_le_bytes().to_vec();
        for _ in 0..2 {
            data.extend_from_slice(&0i32.to_le_bytes());
            write_name(&mut data, "OnFired");
        }
        let (value, consumed) =
            read_one(&PropertyInner::MulticastDelegate, &data).expect("multicast");
        assert_eq!(consumed, 4 + 24);
        assert!(matches!(value, PropertyValue::Array { items } if items.len() == 2));
    }

    /// `FFieldPath` is a counted list of names plus the resolved owner index.
    #[test]
    fn a_field_path_is_a_counted_name_list_followed_by_its_owner() {
        let mut data = 2i32.to_le_bytes().to_vec();
        write_name(&mut data, "Outer");
        write_name(&mut data, "Inner");
        data.extend_from_slice(&0i32.to_le_bytes());
        let (value, consumed) = read_one(&PropertyInner::FieldPath, &data).expect("field path");
        assert_eq!(consumed, 4 + 16 + 4);
        assert!(
            matches!(value, PropertyValue::FieldPath { path, owner: None } if path == "Outer.Inner")
        );
    }

    #[test]
    fn an_empty_field_path_still_consumes_its_owner_index() {
        let mut data = 0i32.to_le_bytes().to_vec();
        data.extend_from_slice(&0i32.to_le_bytes());
        let (_, consumed) = read_one(&PropertyInner::FieldPath, &data).expect("field path");
        assert_eq!(consumed, 8);
    }

    /// TOptional is declared nowhere in the shipped mappings, so its layout was never confirmed.
    /// Failing by name is safer than emitting a value that looks plausible.
    #[test]
    fn an_optional_property_fails_by_name_rather_than_guessing_a_layout() {
        let data = [0u8; 8];
        let err = read_one(
            &PropertyInner::Optional {
                inner: Box::new(PropertyInner::Int),
            },
            &data,
        )
        .expect_err("should refuse");
        assert!(err.contains("TOptional"), "{err}");
    }

    /// A custom date or time style brings a pattern whose layout this reader is not sure of, so it
    /// fails by name rather than reading on through it.
    #[test]
    fn a_custom_date_style_is_refused_by_name() {
        let mut moment = 0i64.to_le_bytes().to_vec();
        moment.push(5);
        moment.extend_from_slice(&[0; 8]);
        let error = read_one(&PropertyInner::Text, &text_bytes(7, &moment)).expect_err("refused");
        assert!(error.contains("custom DateStyle"), "{error}");
    }

    #[test]
    fn ticks_text() {
        assert_eq!(super::ticks_text(0), "0001-01-01 00:00:00");
        assert_eq!(
            super::ticks_text(621_355_968_000_000_000),
            "1970-01-01 00:00:00"
        );
        assert_eq!(
            super::ticks_text(638_633_789_000_000_000),
            "2024-10-01 11:28:20"
        );
    }

    #[test]
    fn format_pattern() {
        let arguments = [
            ("Name".to_string(), "Hulk".to_string()),
            ("0".to_string(), "3".to_string()),
        ];
        assert_eq!(
            super::format_pattern("{Name} smashes {0} times, {Missing}", &arguments),
            "Hulk smashes 3 times, {Missing}"
        );
    }

    #[test]
    fn a_negative_container_count_is_rejected_before_it_is_used() {
        let data = (-1i32).to_le_bytes();
        let mut cursor = Cursor::new(&data, 0);
        let err = read_count(&mut cursor, "array").expect_err("should reject");
        assert!(err.contains("negative"), "{err}");
    }

    #[test]
    fn a_count_larger_than_the_export_is_rejected_before_allocating() {
        let data = 1_000_000i32.to_le_bytes();
        let mut cursor = Cursor::new(&data, 0);
        let err = read_count(&mut cursor, "array").expect_err("should reject");
        assert!(err.contains("exceeds"), "{err}");
    }

    /// A history no UE 5.3 text takes still fails by number, so the audit can rank it.
    #[test]
    fn an_unknown_history_still_fails_by_name() {
        let error =
            read_one(&PropertyInner::Text, &text_bytes(13, &[0; 16])).expect_err("not read");
        assert!(
            error.contains("unsupported FText history type 13 (unknown)"),
            "{error}"
        );
        assert_eq!(history_name(9), "AsDateTime");
    }

    /// A header that skips past every slot the struct declares cannot be this struct's header, and
    /// the failure must say so here rather than pages later.
    #[test]
    fn a_header_covering_more_slots_than_the_schema_is_refused_at_once() {
        use usmap::{Property, Struct};

        let mappings = crate::mappings::Mappings::from_structs(vec![Struct {
            name: "Two".into(),
            super_struct: None,
            properties: vec![
                Property {
                    name: "A".into(),
                    array_dim: 1,
                    index: 0,
                    inner: PropertyInner::Int,
                },
                Property {
                    name: "B".into(),
                    array_dim: 1,
                    index: 1,
                    inner: PropertyInner::Int,
                },
            ],
        }]);
        let schema = mappings.schema("Two").expect("schema");
        let header = header();
        let ctx = Ctx {
            mappings: Some(&mappings),
            header: &header,
            fixups: None,
            synth: None,
            local: None,
        };
        // Skip five slots, last fragment: the encoding UE uses for an all-skipped header.
        let data = [0x05u8, 0x01];
        let mut cursor = Cursor::new(&data, 0);
        let mut diagnostics = Diagnostics::default();
        let mut entries = Vec::new();
        let err = read_property_block(
            &mut cursor,
            &schema,
            &ctx,
            &mut diagnostics,
            0,
            &mut entries,
        )
        .expect_err("refused");
        assert!(err.contains("covers 5"), "{err}");
    }
}
