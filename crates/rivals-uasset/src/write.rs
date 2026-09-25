//! Rewrites a package around edits that change how many bytes an export occupies.
//!
//! Unversioned property serialization carries no byte-length prefix at any level, so a value that
//! grows or shrinks needs no fixup inside the export. The only recorded size is the export table's,
//! which means widening a value comes down to splicing the export data and re-emitting the header
//! with the offsets that follow moved by the same amount.

use serde::Serialize;
use std::io::Cursor as IoCursor;

use retoc::legacy_asset::{
    FLegacyPackageHeader, FObjectDataResource, FObjectExport, FObjectImport, FPackageNameMap,
};
use retoc::logging::Log;
use retoc::zen::FPackageIndex;

use crate::package::{AssetBundle, read_header};

/// A range of package bytes to replace. Offsets are absolute across the `.uasset` and `.uexp`, the
/// same space [`crate::PropertyEntry::span`] reports.
#[derive(Debug, Clone)]
pub struct Splice {
    pub start: u64,
    pub end: u64,
    pub bytes: Vec<u8>,
}

impl Splice {
    pub(crate) fn delta(&self) -> i64 {
        self.bytes.len() as i64 - (self.end.saturating_sub(self.start)) as i64
    }
}

/// `EBulkDataFlags` bits that put a payload in a sidecar file rather than in the export data:
/// `PayloadInSeperateFile`, `OptionalPayload`, `MemoryMappedPayload`.
pub const SEPARATE_PAYLOAD_FLAGS: u32 = 0x100 | 0x800 | 0x1000;
/// `BULKDATA_PayloadInSeperateFile`: the payload sits in the `.ubulk`.
pub const IN_SEPARATE_FILE: u32 = 0x100;
/// `BULKDATA_OptionalPayload`: the payload sits in the `.uptnl`.
pub const OPTIONAL_PAYLOAD: u32 = 0x800;
/// `BULKDATA_MemoryMappedPayload`: the payload sits in the `.m.ubulk`, aligned for mapping.
pub const MEMORY_MAPPED: u32 = 0x1000;
/// `BULKDATA_SerializeCompressed`.
const COMPRESSED: u32 = 0x2;
/// `BULKDATA_DuplicateNonOptionalPayload`.
const DUPLICATE_PAYLOAD: u32 = 0x4000;
/// `BULKDATA_PayloadAtEndOfFile`, a layout this game's inline payloads never carry.
const PAYLOAD_AT_END_OF_FILE: u32 = 0x1;

/// An inline bulk payload placed in the export data.
///
/// The bulk data table gives an inline resource's offset relative to the export it belongs to,
/// and cooked data writes the resource's table index as the word right before the payload. The
/// owner is whichever export makes that word read the index, which is how a zen package, whose
/// table carries no outer, still places every payload. Measured on a texture (seven inline mips)
/// and a static mesh (three exports, one inline payload each).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InlinePayload {
    pub resource: usize,
    pub owner: usize,
    /// Where the payload starts within the export data; the index word sits right before it.
    pub start: i64,
    pub size: i64,
}

/// One bulk data resource as the inspector shows it: where its payload sits and whether the
/// bytes can be replaced.
#[derive(Debug, Clone, Serialize)]
pub struct ResourceInfo {
    pub index: u32,
    pub serial_offset: i64,
    pub serial_size: i64,
    pub raw_size: i64,
    /// The legacy bulk data flags.
    pub flags: u32,
    /// `inline`, `ubulk`, `uptnl`, `m.ubulk`, or `unplaced` for an inline entry no export holds.
    pub placement: &'static str,
    /// The export holding an inline payload.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<u32>,
    /// Why the bytes cannot be replaced, when they cannot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locked: Option<String>,
}

/// Which file a resource's payload sits in, by its flags.
pub fn placement(flags: u32) -> &'static str {
    if flags & MEMORY_MAPPED != 0 {
        "m.ubulk"
    } else if flags & OPTIONAL_PAYLOAD != 0 {
        "uptnl"
    } else if flags & IN_SEPARATE_FILE != 0 {
        "ubulk"
    } else {
        "inline"
    }
}

/// Why a resource's bytes cannot be replaced: a layout this editor would have to decode or align
/// to do it safely.
pub fn bulk_lock(resource: &FObjectDataResource) -> Option<String> {
    let flags = resource.legacy_bulk_data_flags;
    if flags & COMPRESSED != 0 || resource.raw_size != resource.serial_size {
        return Some("This payload is stored compressed, so its bytes cannot be swapped.".into());
    }
    if flags & MEMORY_MAPPED != 0 {
        return Some("This payload is memory-mapped, and its alignment cannot be kept.".into());
    }
    if flags & DUPLICATE_PAYLOAD != 0 {
        return Some("This payload is duplicated across files.".into());
    }
    if resource.cooked_index.is_some_and(|index| index != 0) {
        return Some(
            "This payload sits in a numbered bulk file this editor does not write.".into(),
        );
    }
    None
}

/// The bulk data table as the inspector shows it.
pub fn resource_infos(header: &FLegacyPackageHeader, exports: &[u8]) -> Vec<ResourceInfo> {
    header
        .data_resources
        .iter()
        .enumerate()
        .map(|(index, resource)| {
            let flags = resource.legacy_bulk_data_flags;
            let inline = flags & SEPARATE_PAYLOAD_FLAGS == 0;
            let owner = inline
                .then(|| locate_inline_payload(header, exports, index))
                .flatten()
                .map(|payload| payload.owner as u32);
            let placement = if inline && owner.is_none() {
                "unplaced"
            } else {
                placement(flags)
            };
            let locked = bulk_lock(resource).or_else(|| {
                (placement == "unplaced")
                    .then(|| "No export holds this payload where the table points.".to_string())
            });
            ResourceInfo {
                index: index as u32,
                serial_offset: resource.serial_offset,
                serial_size: resource.serial_size,
                raw_size: resource.raw_size,
                flags,
                placement,
                owner,
                locked,
            }
        })
        .collect()
}

/// Every inline resource placed in its export, or the first one that cannot be.
pub fn inline_payloads(
    header: &FLegacyPackageHeader,
    exports: &[u8],
) -> Result<Vec<InlinePayload>, String> {
    let mut placed = Vec::new();
    for index in 0..header.data_resources.len() {
        if let Some(payload) = require_inline_payload(header, exports, index)? {
            placed.push(payload);
        }
    }
    Ok(placed)
}

/// Whether every inline resource still sits where the table points, with its index word in front.
pub(crate) fn check_inline_bulk(bundle: &AssetBundle<'_>) -> Result<(), String> {
    let header = read_header(bundle)?;
    for index in 0..header.data_resources.len() {
        require_inline_payload(&header, bundle.exports, index)?;
    }
    Ok(())
}

fn require_inline_payload(
    header: &FLegacyPackageHeader,
    exports: &[u8],
    index: usize,
) -> Result<Option<InlinePayload>, String> {
    let resource = &header.data_resources[index];
    if resource.legacy_bulk_data_flags & SEPARATE_PAYLOAD_FLAGS != 0 {
        return Ok(None);
    }
    if resource.legacy_bulk_data_flags & PAYLOAD_AT_END_OF_FILE != 0 {
        return Err(format!(
            "bulk data resource {index} is inline yet flagged as sitting at the end of the file, a layout this editor does not know"
        ));
    }
    locate_inline_payload(header, exports, index)
        .map(Some)
        .ok_or_else(|| {
            format!(
                "bulk data resource {index} is inline, but no export holds its index word where the table points"
            )
        })
}

/// The export whose data holds inline resource `index`, by the index word before the payload.
pub fn locate_inline_payload(
    header: &FLegacyPackageHeader,
    exports: &[u8],
    index: usize,
) -> Option<InlinePayload> {
    let resource = header.data_resources.get(index)?;
    let total = i64::from(header.summary.versioning_info.total_header_size);
    header
        .exports
        .iter()
        .enumerate()
        .find_map(|(owner, export)| {
            let first = export.serial_offset - total;
            let start = first + resource.serial_offset;
            let fits = start >= 4 && start + resource.serial_size <= first + export.serial_size;
            (fits && word_at(exports, start - 4) == Some(index as i32)).then_some(InlinePayload {
                resource: index,
                owner,
                start,
                size: resource.serial_size,
            })
        })
}

fn word_at(bytes: &[u8], at: i64) -> Option<i32> {
    let at = usize::try_from(at).ok()?;
    let word = bytes.get(at..at + 4)?;
    Some(i32::from_le_bytes([word[0], word[1], word[2], word[3]]))
}

/// Header tables an edit replaced. `None` leaves the table as it was read.
#[derive(Debug, Default)]
pub struct HeaderDraft {
    pub names: Option<FPackageNameMap>,
    pub imports: Option<Vec<FObjectImport>>,
    /// The export table with its references renumbered. It still holds the exports being removed,
    /// so the splice deleting each one has an entry to charge; offsets are as read.
    pub exports: Option<Vec<FObjectExport>>,
    /// Positions in the export table to drop once the splices have been charged.
    pub drop_exports: Vec<usize>,
    pub preload_dependencies: Option<Vec<FPackageIndex>>,
    /// The bulk data table, offsets as read; `rewrite` moves the inline ones with their bytes.
    pub data_resources: Option<Vec<FObjectDataResource>>,
    /// Export data added right after the last export, in front of whatever trails it. The drafted
    /// export table may then be longer than the one read, by the entries that own these bytes.
    pub appended: Vec<u8>,
}

/// The `.uasset` and `.uexp` of a package after rewriting.
#[derive(Debug)]
pub struct RewrittenPackage {
    pub asset: Vec<u8>,
    pub exports: Vec<u8>,
}

/// Confirms the package header re-serializes to exactly the bytes it was read from.
///
/// Editing means re-emitting this header, so a package whose header does not survive a round trip
/// must not be written: the difference would be silent and would touch every export.
pub fn header_round_trips(bundle: &AssetBundle<'_>) -> Result<(), String> {
    let mut header = read_header(bundle)?;
    let total = header_size(&header)?;
    to_relative(&mut header, total);
    let rewritten = serialize_header(&header, total)?;
    if rewritten == bundle.asset {
        return Ok(());
    }
    let at = rewritten
        .iter()
        .zip(bundle.asset)
        .position(|(a, b)| a != b)
        .unwrap_or(bundle.asset.len().min(rewritten.len()));
    Err(format!(
        "this package's header does not survive being re-written ({} bytes in rather than {}, first difference at {at:#X}), so it cannot be edited safely",
        rewritten.len(),
        bundle.asset.len()
    ))
}

/// Applies `splices` to the package and repairs the export table around them.
///
/// Splices must be ascending and must not overlap. Each one is charged to the export whose byte
/// range contains it: that export's `serial_size` moves by the delta, and every export that starts
/// after it slides by the same amount.
///
/// `draft` replaces header tables an edit changed, for edits that had to add a name or an import.
/// Both sit before the export table, so a longer one pushes the whole header out and moves every
/// export. That is handled rather than avoided: the serializer recomputes the offsets from the
/// rebased ones, which is why the rebase has to happen first.
pub fn rewrite(
    bundle: &AssetBundle<'_>,
    splices: &[Splice],
    draft: HeaderDraft,
) -> Result<RewrittenPackage, String> {
    let mut header = read_header(bundle)?;
    let total = header_size(&header)?;
    let payloads = inline_payloads(&header, bundle.exports)?;
    let mut resource_shift = vec![0i64; header.data_resources.len()];
    let HeaderDraft {
        names,
        imports,
        exports: drafted_exports,
        drop_exports,
        preload_dependencies,
        data_resources,
        appended,
    } = draft;

    for pair in splices.windows(2) {
        if pair[1].start < pair[0].end {
            return Err("two edits cover the same bytes".into());
        }
    }

    // Where appended bytes go: the end of the last export as read, in the export data's frame.
    let original_end = header
        .exports
        .iter()
        .map(|export| export.serial_offset + export.serial_size)
        .max()
        .unwrap_or(total as i64)
        - total as i64;
    if let Some(exports) = drafted_exports {
        let grown = exports.len() > header.exports.len() && !appended.is_empty();
        if exports.len() != header.exports.len() && !grown {
            return Err("the drafted export table does not match the package".into());
        }
        header.exports = exports;
    }
    to_relative(&mut header, total);

    // Splices address the package as it was read, so ownership is settled against the ranges the
    // exports had before any splice moved them.
    let ranges: Vec<(i64, i64)> = header
        .exports
        .iter()
        .map(|export| {
            (
                export.serial_offset,
                export.serial_offset + export.serial_size,
            )
        })
        .collect();
    for splice in splices {
        let start = i64::try_from(splice.start).map_err(|_| "edit offset does not fit")?;
        let relative = start - total as i64;
        if relative < 0 {
            return Err("edits to the package header are not supported".into());
        }
        let delta = splice.delta();
        let owner = ranges
            .iter()
            .position(|&(first, end)| {
                if splice.start == splice.end {
                    // An insertion at the very end of an export sits on the boundary it shares
                    // with the next one. It extends the export it came out of, not the one that
                    // happens to start there.
                    relative > first && relative <= end
                } else {
                    relative >= first && relative < end
                }
            })
            .ok_or_else(|| {
                format!(
                    "the bytes at {:#X} do not belong to any export",
                    splice.start
                )
            })?;

        // An inline payload is addressed from its export's start, so only an edit inside that
        // export and ahead of the index word moves it. An edit over the payload is refused unless
        // it replaces exactly the payload (the drafted table then carries its new size), rewrites
        // just its index word, or deletes the whole export, whose table entries go with it.
        let relative_end =
            i64::try_from(splice.end).map_err(|_| "edit offset does not fit")? - total as i64;
        let (first, last) = ranges[owner];
        for payload in payloads.iter().filter(|payload| payload.owner == owner) {
            let word = payload.start - 4;
            let replaces =
                relative == payload.start && relative_end == payload.start + payload.size;
            // A removal renumbers the table, and the index word follows it.
            let renumbers = relative == word && relative_end == payload.start && delta == 0;
            let deletes = relative <= first && relative_end >= last;
            let allowed = replaces || renumbers || deletes;
            if relative_end <= word {
                resource_shift[payload.resource] += delta;
            } else if relative < payload.start + payload.size && !allowed {
                return Err(format!(
                    "the edit at {:#X} overlaps inline bulk data of export {owner}; that payload is replaced through the bulk data editor",
                    splice.start
                ));
            }
        }

        let boundary = header.exports[owner].serial_offset;
        header.exports[owner].serial_size += delta;
        for export in &mut header.exports {
            if export.serial_offset > boundary {
                export.serial_offset += delta;
            }
        }
    }

    if !drop_exports.is_empty() {
        let mut position = 0usize;
        header.exports.retain(|_| {
            let keep = !drop_exports.contains(&position);
            position += 1;
            keep
        });
    }
    if let Some(dependencies) = preload_dependencies {
        header.preload_dependencies = dependencies;
    }
    let mut resources = data_resources.unwrap_or_else(|| header.data_resources.clone());
    if resources.len() != resource_shift.len() && resource_shift.iter().any(|shift| *shift != 0) {
        return Err("the drafted bulk data table does not match the package".into());
    }
    for (resource, shift) in resources.iter_mut().zip(&resource_shift) {
        resource.serial_offset += shift;
    }
    header.data_resources = resources;
    if names.is_some() || imports.is_some() {
        if let Some(names) = names {
            header.name_map = names;
        }
        if let Some(imports) = imports {
            header.imports = imports;
        }
        // Anything past this count is treated as header-only, and a name an export now points at
        // is not. Widening it costs nothing and keeps a freshly added name reachable.
        header.summary.names_referenced_from_export_data_count = header.name_map.num_names() as i32;
    }

    let mut exports = apply(bundle.exports, splices, total)?;
    if !appended.is_empty() {
        let shift: i64 = splices
            .iter()
            .filter(|splice| splice.end as i64 - total as i64 <= original_end)
            .map(Splice::delta)
            .sum();
        let at = usize::try_from(original_end + shift)
            .ok()
            .filter(|&at| at <= exports.len())
            .ok_or(
                "the export data ends before its last export does, so nothing can be appended",
            )?;
        exports.splice(at..at, appended);
    }
    let asset = serialize_header(&header, total)?;
    Ok(RewrittenPackage { asset, exports })
}

/// Splices the export data, keeping whatever trails the last export exactly where it is.
fn apply(exports: &[u8], splices: &[Splice], total: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(exports.len());
    let mut copied = 0usize;
    for splice in splices {
        let start = usize::try_from(splice.start)
            .ok()
            .and_then(|s| s.checked_sub(total))
            .ok_or("edit offset does not fit in the export data")?;
        let end = usize::try_from(splice.end)
            .ok()
            .and_then(|s| s.checked_sub(total))
            .ok_or("edit offset does not fit in the export data")?;
        if start < copied || end > exports.len() || start > end {
            return Err(format!(
                "the edit at {:#X} lies outside the package",
                splice.start
            ));
        }
        out.extend_from_slice(&exports[copied..start]);
        out.extend_from_slice(&splice.bytes);
        copied = end;
    }
    out.extend_from_slice(&exports[copied..]);
    Ok(out)
}

/// The export table is read with the header size folded into every offset but written without
/// it, because the serializer adds it back. Re-emitting without this doubles every offset, and
/// the only visible symptom is a package that loads nothing.
fn to_relative(header: &mut FLegacyPackageHeader, total: usize) {
    for export in &mut header.exports {
        export.serial_offset -= total as i64;
    }
}

fn header_size(header: &FLegacyPackageHeader) -> Result<usize, String> {
    usize::try_from(header.summary.versioning_info.total_header_size)
        .map_err(|_| "this package reports an implausible header size".to_string())
}

/// `desired_header_size` is a floor, not a target: it pads a header that came out shorter and lets
/// one that grew a name entry keep its new length, which is what rebases the export offsets.
fn serialize_header(header: &FLegacyPackageHeader, floor: usize) -> Result<Vec<u8>, String> {
    let mut out = IoCursor::new(Vec::new());
    header
        .serialize(&mut out, Some(floor), &Log::no_log())
        .map_err(|e| format!("could not write the package header: {e}"))?;
    Ok(out.into_inner())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn splice(start: u64, end: u64, bytes: &[u8]) -> Splice {
        Splice {
            start,
            end,
            bytes: bytes.to_vec(),
        }
    }

    #[test]
    fn a_widening_splice_reports_the_bytes_it_added() {
        assert_eq!(splice(0, 4, &[1, 2, 3, 4, 5, 6]).delta(), 2);
        assert_eq!(splice(0, 8, &[1, 2]).delta(), -6);
        assert_eq!(splice(0, 4, &[1, 2, 3, 4]).delta(), 0);
    }

    #[test]
    fn splices_are_applied_in_order_and_the_tail_is_kept() {
        let exports = [0u8, 1, 2, 3, 4, 5, 6, 7];
        let out = apply(
            &exports,
            &[splice(101, 103, &[0xAA]), splice(105, 106, &[0xBB, 0xCC])],
            100,
        )
        .expect("apply");
        assert_eq!(out, [0, 0xAA, 3, 4, 0xBB, 0xCC, 6, 7]);
    }

    use retoc::legacy_asset::{
        EPackageFlags, FLegacyPackageFileSummary, FMinimalName, FObjectDataResource, FObjectExport,
        FObjectImport, FPackageNameMap,
    };

    const HEADER: i64 = 0x400;
    const INLINE_FLAGS: u32 = 0x48;
    const SEPARATE_FLAGS: u32 = 0x10501;

    fn name(index: i32) -> FMinimalName {
        FMinimalName { index, number: 0 }
    }

    /// One 48-byte export holding an inline payload of 8 bytes at offset 20, its index word right
    /// before it, plus a separate-file resource that must never move.
    fn bundle_with_inline_bulk(index_word: i32) -> (Vec<u8>, Vec<u8>) {
        let mut summary = FLegacyPackageFileSummary {
            package_name: "/Game/Test".to_string(),
            ..Default::default()
        };
        summary.versioning_info.package_file_version =
            crate::package::FALLBACK_ENGINE_VERSION.package_file_version();
        summary.versioning_info.total_header_size = HEADER as i32;
        summary.package_flags = EPackageFlags::Cooked as u32
            | EPackageFlags::FilterEditorOnly as u32
            | EPackageFlags::UsesUnversionedProperties as u32;
        let header = FLegacyPackageHeader {
            summary,
            name_map: FPackageNameMap::create_from_names(vec![
                "None".into(),
                "Thing".into(),
                "Object".into(),
                "/Script/CoreUObject".into(),
            ]),
            imports: vec![FObjectImport {
                class_package: name(3),
                class_name: name(2),
                outer_index: FPackageIndex::create_null(),
                object_name: name(2),
                is_optional: false,
            }],
            exports: vec![FObjectExport {
                class_index: FPackageIndex::create_import(0),
                object_name: name(1),
                serial_offset: 0,
                serial_size: 48,
                ..Default::default()
            }],
            data_resources: vec![
                FObjectDataResource {
                    legacy_bulk_data_flags: INLINE_FLAGS,
                    serial_offset: 20,
                    duplicate_serial_offset: -1,
                    serial_size: 8,
                    raw_size: 8,
                    ..Default::default()
                },
                FObjectDataResource {
                    legacy_bulk_data_flags: SEPARATE_FLAGS,
                    serial_offset: 0,
                    duplicate_serial_offset: -1,
                    serial_size: 100,
                    raw_size: 100,
                    ..Default::default()
                },
            ],
            data_resource_version: Some(Default::default()),
            ..Default::default()
        };
        let mut asset = IoCursor::new(Vec::new());
        header
            .serialize(&mut asset, Some(HEADER as usize), &Log::no_log())
            .expect("serialize");
        let mut exports: Vec<u8> = (0u8..48).collect();
        exports[16..20].copy_from_slice(&index_word.to_le_bytes());
        exports.extend_from_slice(&[0x9E, 0x2A, 0x83, 0xC1]);
        (asset.into_inner(), exports)
    }

    fn rewritten_table(
        splices: &[Splice],
        index_word: i32,
    ) -> Result<Vec<FObjectDataResource>, String> {
        let (asset, exports) = bundle_with_inline_bulk(index_word);
        let bundle = AssetBundle {
            asset: &asset,
            exports: &exports,
        };
        let rewritten = rewrite(&bundle, splices, HeaderDraft::default())?;
        let after = read_header(&AssetBundle {
            asset: &rewritten.asset,
            exports: &rewritten.exports,
        })?;
        inline_payloads(&after, &rewritten.exports)?;
        Ok(after.data_resources)
    }

    /// The table addresses an inline payload from its export's start, so bytes added ahead of
    /// it inside that export push it along; a separate-file resource keeps its sidecar offset.
    #[test]
    fn a_splice_before_an_inline_payload_moves_its_table_offset() {
        let at = HEADER as u64 + 8;
        let table = rewritten_table(&[splice(at, at, &[1, 2, 3, 4])], 0).expect("rewrite");
        assert_eq!(table[0].serial_offset, 24);
        assert_eq!(table[1].serial_offset, 0);
    }

    #[test]
    fn a_splice_after_an_inline_payload_leaves_it_alone() {
        let at = HEADER as u64 + 40;
        let table = rewritten_table(&[splice(at, at, &[1, 2, 3, 4])], 0).expect("rewrite");
        assert_eq!(table[0].serial_offset, 20);
    }

    /// Replacing exactly the payload is the bulk editor's move: the bytes change width, the
    /// drafted table carries the new size, and the export grows with it.
    #[test]
    fn a_splice_replacing_an_inline_payload_exactly_is_allowed_with_a_drafted_size() {
        let (asset, exports) = bundle_with_inline_bulk(0);
        let bundle = AssetBundle {
            asset: &asset,
            exports: &exports,
        };
        let header = read_header(&bundle).expect("header");
        let mut table = header.data_resources.clone();
        table[0].serial_size = 12;
        table[0].raw_size = 12;
        let at = HEADER as u64 + 20;
        let rewritten = rewrite(
            &bundle,
            &[splice(at, at + 8, &[7u8; 12])],
            HeaderDraft {
                data_resources: Some(table),
                ..Default::default()
            },
        )
        .expect("rewrite");
        let after = read_header(&AssetBundle {
            asset: &rewritten.asset,
            exports: &rewritten.exports,
        })
        .expect("header after");
        assert_eq!(after.data_resources[0].serial_offset, 20);
        assert_eq!(after.data_resources[0].serial_size, 12);
        assert_eq!(after.exports[0].serial_size, 52);
        let placed = inline_payloads(&after, &rewritten.exports).expect("placed");
        assert_eq!(placed[0].size, 12);
        assert_eq!(&rewritten.exports[20..32], &[7u8; 12]);
    }

    #[test]
    fn a_splice_overlapping_an_inline_payload_is_refused() {
        let at = HEADER as u64 + 22;
        let error = rewritten_table(&[splice(at, at + 2, &[1, 2, 3, 4])], 0).expect_err("refused");
        assert!(error.contains("overlaps inline bulk data"), "{error}");
    }

    /// A package whose inline payload is not where the table says cannot be edited without
    /// guessing, so it is refused before anything moves.
    #[test]
    fn an_inline_payload_no_export_holds_is_refused() {
        let at = HEADER as u64 + 8;
        let error = rewritten_table(&[splice(at, at, &[1, 2, 3, 4])], 5).expect_err("refused");
        assert!(error.contains("no export holds its index word"), "{error}");
    }

    #[test]
    fn an_edit_past_the_end_of_the_export_data_is_refused() {
        let exports = [0u8; 4];
        let error = apply(&exports, &[splice(100, 200, &[0])], 100).expect_err("refused");
        assert!(error.contains("outside the package"), "{error}");
    }
}
