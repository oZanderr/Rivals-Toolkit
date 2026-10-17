//! What two IoStore mods hold differently, package by package: the packages only one holds, how
//! many both hold byte for byte the same, and for each the two hold differently, what differs in it:
//! its objects, class members, function scripts, values and imports.
//!
//! Packages pair by id, which comes from the package name, and one whose chunks match is not read.
//! Within a package, objects pair by path rather than by index, since two packages number their
//! imports and exports each in their own way. A script is compared as the text `asset script
//! --as-text` prints, with its labels, which are code offsets, paired between the two, so code that
//! only moved is not shown as changed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use retoc::{EIoChunkType, FIoChunkId, FPackageId};
use rivals_uasset::{Mappings, ParsedPackage};
use serde::Serialize;
use similar::{Algorithm, DiffTag};

use crate::asset::{PackageConverter, list_packages};
use crate::override_check::{self, FindingKind, Side};
use crate::pak::containers::open_utoc;
use crate::pak::profile::strip_mount_prefix;
use crate::schema_synth::LayoutReader;

/// Which of the two mods something is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Only {
    A,
    B,
}

/// Two mods compared.
#[derive(Debug, Clone, Serialize)]
pub struct ModDiff {
    /// The two containers, as they were read.
    pub a: String,
    pub b: String,
    /// The packages only one holds, by entry.
    pub only_a: Vec<String>,
    pub only_b: Vec<String>,
    /// How many packages both hold byte for byte the same.
    pub identical: usize,
    pub changed: Vec<PackageDiff>,
    /// The files the mods' paks hold beside their containers that only one holds, or the two hold
    /// differently.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub pak_files: Vec<FileDiff>,
}

/// A file of a mod's pak that only one of the two holds, or both hold differently.
#[derive(Debug, Clone, Serialize)]
pub struct FileDiff {
    pub file: String,
    /// The mod only holding it; none when both do, differently.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub only: Option<Only>,
}

/// One package both mods hold differently.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PackageDiff {
    /// Its entry, mount-relative.
    pub package: String,
    /// What its asset is: Blueprint, Widget Blueprint, DataTable and so on, when it was read.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub kind: String,
    /// The parts of it that differ: `uasset` for the package itself, `ubulk`, `uptnl` and `m.ubulk`
    /// for its bulk data, with the mod holding a part the other lacks.
    pub chunks: Vec<String>,
    /// Objects, members, rows, columns, string keys and imports only one copy has, or both hold
    /// differently.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub differences: Vec<Difference>,
    /// The functions whose scripts read differently.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub scripts: Vec<ScriptDiff>,
    /// The functions whose scripts read the same and only sit at other offsets, such as an event
    /// whose graph grew before the code it enters at.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub moved: Vec<String>,
    /// Each value the two copies hold differently.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<ValueDifference>,
    /// Why the two copies could not be compared, when they could not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unreadable: Option<String>,
}

/// One thing only one copy of a package has, or both hold differently.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Difference {
    pub kind: FindingKind,
    /// The export's path, the member's, row's or key's name, or the import's path.
    pub what: String,
    /// The mod only holding it; none when both do, differently.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub only: Option<Only>,
    /// Each copy's form of it, where it has one: a class, a type, a parameter list, a string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub a: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub b: Option<String>,
}

/// One value the two copies hold differently.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ValueDifference {
    /// The export's path.
    pub export: String,
    /// The table row, for a value in one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row: Option<String>,
    /// Where in the export or row, as `A.B`, `A[2]` or `A[Key]`.
    pub path: String,
    pub a: String,
    pub b: String,
}

/// A function whose script reads differently, as the lines that differ with a little around them.
#[derive(Debug, Clone, Serialize)]
pub struct ScriptDiff {
    pub function: String,
    /// How many statements each copy's script has.
    pub statements: [usize; 2],
    /// Each run of lines that differ, with the lines around it that read the same.
    pub hunks: Vec<Vec<DiffLine>>,
}

/// A line of a script's text: one only one copy has, or one both have, around those.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiffLine {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub only: Option<Only>,
    pub text: String,
}

/// The chunks a package owns, by the file each would be loose.
const CHUNKS: [(EIoChunkType, &str); 4] = [
    (EIoChunkType::ExportBundleData, "uasset"),
    (EIoChunkType::BulkData, "ubulk"),
    (EIoChunkType::OptionalBulkData, "uptnl"),
    (EIoChunkType::MemoryMappedBulkData, "m.ubulk"),
];

/// How many lines around a run of changed script lines are shown with it.
const CONTEXT: usize = 2;

/// The two mods' packages paired, and which differ in which chunks, without reading any package,
/// so neither the game's containers nor mappings are needed.
pub fn pair_mods(a: &Path, b: &Path) -> Result<ModDiff, String> {
    pair(a, b).map(|(diff, _)| diff)
}

/// Each changed package, with whether the package itself differs and not only its bulk data.
type Changed = Vec<(FPackageId, bool)>;

fn pair(a: &Path, b: &Path) -> Result<(ModDiff, Changed), String> {
    let (store_a, store_b) = (
        open_utoc(&a.to_string_lossy())?,
        open_utoc(&b.to_string_lossy())?,
    );
    let packages =
        |store: &dyn retoc::iostore::IoStoreTrait| -> BTreeMap<u64, (FPackageId, String)> {
            store
                .packages()
                .filter_map(|package| {
                    let id = package.id();
                    let chunk = FIoChunkId::from_package_id(id, 0, EIoChunkType::ExportBundleData);
                    let entry = strip_mount_prefix(&store.chunk_path(chunk)?);
                    Some((id.0, (id, entry)))
                })
                .collect()
        };
    let (in_a, in_b) = (packages(store_a.as_ref()), packages(store_b.as_ref()));
    let mut diff = ModDiff {
        a: a.to_string_lossy().into_owned(),
        b: b.to_string_lossy().into_owned(),
        only_a: Vec::new(),
        only_b: Vec::new(),
        identical: 0,
        changed: Vec::new(),
        pak_files: pak_files(a, b)?,
    };
    let mut changed = Vec::new();
    for (key, (id, entry)) in &in_a {
        if !in_b.contains_key(key) {
            diff.only_a.push(entry.clone());
            continue;
        }
        let mut chunks = Vec::new();
        for (kind, file) in CHUNKS {
            // Ids made from a package id only: a bare one has no version bits to hash by.
            let chunk = FIoChunkId::from_package_id(*id, 0, kind);
            match (store_a.has_chunk_id(chunk), store_b.has_chunk_id(chunk)) {
                (false, false) => {}
                (true, true) => {
                    let read = |store: &dyn retoc::iostore::IoStoreTrait| {
                        store.read(chunk).map_err(|e| format!("{entry}: {e}"))
                    };
                    if read(store_a.as_ref())? != read(store_b.as_ref())? {
                        chunks.push(file.to_string());
                    }
                }
                (true, false) => chunks.push(format!("{file} (only in A)")),
                (false, true) => chunks.push(format!("{file} (only in B)")),
            }
        }
        if chunks.is_empty() {
            diff.identical += 1;
            continue;
        }
        changed.push((*id, chunks.iter().any(|chunk| chunk.starts_with("uasset"))));
        diff.changed.push(PackageDiff {
            package: entry.clone(),
            chunks,
            ..Default::default()
        });
    }
    for (key, (_, entry)) in &in_b {
        if !in_a.contains_key(key) {
            diff.only_b.push(entry.clone());
        }
    }
    diff.only_a.sort();
    diff.only_b.sort();
    Ok((diff, changed))
}

/// The files the two mods' paks hold that only one does, or both hold differently. A mod with no
/// pak beside its container holds none.
fn pak_files(a: &Path, b: &Path) -> Result<Vec<FileDiff>, String> {
    let files = |utoc: &Path| -> Result<BTreeMap<String, Vec<u8>>, String> {
        let Ok(mod_files) = crate::mod_ops::ModFiles::of(utoc) else {
            return Ok(BTreeMap::new());
        };
        let pak = mod_files.part("pak");
        if !pak.is_file() {
            return Ok(BTreeMap::new());
        }
        Ok(crate::pak::plain::read_files(&pak, &|_| true)?
            .into_iter()
            .map(|(name, bytes)| (name.to_ascii_lowercase(), bytes))
            .collect())
    };
    let (in_a, in_b) = (files(a)?, files(b)?);
    let names: BTreeSet<&String> = in_a.keys().chain(in_b.keys()).collect();
    Ok(names
        .into_iter()
        .filter_map(|name| {
            let only = match (in_a.get(name), in_b.get(name)) {
                (Some(a), Some(b)) if a == b => return None,
                (Some(_), Some(_)) => None,
                (Some(_), None) => Some(Only::A),
                _ => Some(Only::B),
            };
            Some(FileDiff {
                file: name.clone(),
                only,
            })
        })
        .collect())
}

/// [`pair_mods`], then each package whose own bytes differ read from both mods, around the game's
/// containers, and compared.
pub fn mod_diff(
    game_root: &str,
    a: &Path,
    b: &Path,
    mappings: Option<&Mappings>,
) -> Result<ModDiff, String> {
    let (mut diff, changed) = pair(a, b)?;
    if !changed.iter().any(|(_, own)| *own) {
        return Ok(diff);
    }
    let (container_a, container_b) = (diff.a.clone(), diff.b.clone());
    let (store_a, _) = list_packages(game_root, &container_a)?;
    let (store_b, _) = list_packages(game_root, &container_b)?;
    let (converter_a, converter_b) = (
        PackageConverter::new(store_a.as_ref()),
        PackageConverter::new(store_b.as_ref()),
    );
    let (layouts_a, layouts_b) = (
        LayoutReader::through(store_a.as_ref(), &converter_a),
        LayoutReader::through(store_b.as_ref(), &converter_b),
    );
    let side_a = Side {
        converter: &converter_a,
        layouts: &layouts_a,
        container: &container_a,
    };
    let side_b = Side {
        converter: &converter_b,
        layouts: &layouts_b,
        container: &container_b,
    };
    for ((id, own), package) in changed.iter().zip(diff.changed.iter_mut()) {
        if !own {
            continue;
        }
        let read = |side: &Side<'_>| {
            override_check::read_copy_with_bundle(side, *id, &package.package, game_root, mappings)
        };
        match (read(&side_a), read(&side_b)) {
            (Ok((kind, loaded_a, a)), Ok((_, loaded_b, b))) => {
                let compared = compare_packages(&a, &b);
                let bundle_a = rivals_uasset::AssetBundle {
                    asset: &loaded_a.asset_file_buffer,
                    exports: &loaded_a.exports_file_buffer,
                };
                let bundle_b = rivals_uasset::AssetBundle {
                    asset: &loaded_b.asset_file_buffer,
                    exports: &loaded_b.exports_file_buffer,
                };
                package.kind = kind;
                package.differences = compared.differences;
                package
                    .differences
                    .extend(payload_differences((&bundle_a, &a), (&bundle_b, &b)));
                package.scripts = compared.scripts;
                package.moved = compared.moved;
                package.values = compared.values;
            }
            (Err(error), _) => package.unreadable = Some(format!("A's copy: {error}")),
            (_, Err(error)) => package.unreadable = Some(format!("B's copy: {error}")),
        }
    }
    Ok(diff)
}

/// How two copies of a package differ: objects, members, rows, columns, string keys and imports
/// only one has or both hold differently, the functions whose scripts read differently or only
/// moved, and every value they hold differently.
pub fn compare_packages(a: &ParsedPackage, b: &ParsedPackage) -> PackageDiff {
    // What A has that B lacks or holds differently, then what B alone has.
    let mut differences: Vec<Difference> = override_check::compare(a, b)
        .findings
        .into_iter()
        .map(|finding| Difference {
            kind: finding.kind,
            what: finding.what,
            only: finding.ours.is_none().then_some(Only::A),
            a: finding.game,
            b: finding.ours,
        })
        .collect();
    differences.extend(
        override_check::compare(b, a)
            .findings
            .into_iter()
            .filter(|finding| finding.ours.is_none())
            .map(|finding| Difference {
                kind: finding.kind,
                what: finding.what,
                only: Some(Only::B),
                a: None,
                b: finding.game,
            }),
    );
    differences.extend(string_differences(a, b));
    differences.sort_by(|x, y| x.kind.cmp(&y.kind).then_with(|| x.what.cmp(&y.what)));

    // A row only one copy has is a difference already.
    let values = crate::value_diff::value_changes(a, b)
        .into_iter()
        .filter(|change| !(change.row.is_some() && change.path.is_empty()))
        .map(|change| ValueDifference {
            export: change.export,
            row: change.row,
            path: change.path,
            a: change.game,
            b: change.ours,
        })
        .collect();

    let (mut scripts, mut moved) = (Vec::new(), Vec::new());
    let (printer_a, printer_b) = (
        rivals_uasset::ScriptPrinter::new(a),
        rivals_uasset::ScriptPrinter::new(b),
    );
    let in_b: BTreeMap<String, u32> = b
        .exports
        .iter()
        .enumerate()
        .filter(|(_, export)| export.script.is_some())
        .map(|(at, export)| (export.path.to_ascii_lowercase(), at as u32))
        .collect();
    for (at, export) in a.exports.iter().enumerate() {
        let Some(&theirs) = in_b.get(&export.path.to_ascii_lowercase()) else {
            continue;
        };
        let (Some(text_a), Some(text_b)) = (printer_a.print(at as u32), printer_b.print(theirs))
        else {
            continue;
        };
        let (whole_a, whole_b) = (text_a.text(), text_b.text());
        if whole_a == whole_b {
            continue;
        }
        let lines_a: Vec<&str> = whole_a.lines().collect();
        let lines_b: Vec<&str> = whole_b.lines().collect();
        let hunks = line_diff(&lines_a, &lines_b);
        if hunks.is_empty() {
            moved.push(export.object_name.clone());
        } else {
            scripts.push(ScriptDiff {
                function: export.object_name.clone(),
                statements: [text_a.lines.len(), text_b.lines.len()],
                hunks,
            });
        }
    }
    PackageDiff {
        differences,
        scripts,
        moved,
        values,
        ..Default::default()
    }
}

/// What the exports both copies hold at one path hold differently beyond their values and
/// scripts: a texture's format, size, mips and inline pixels, and the bytes the parse measures but
/// does not decode, a class's payload or an unread rest. Mips kept in bulk files show as the chunks
/// that differ. Bytes of one size holding other values may be no more than names or objects the
/// two number otherwise, which is said when the two do not number them alike.
pub fn payload_differences(
    a: (&rivals_uasset::AssetBundle<'_>, &ParsedPackage),
    b: (&rivals_uasset::AssetBundle<'_>, &ParsedPackage),
) -> Vec<Difference> {
    let ((bundle_a, parsed_a), (bundle_b, parsed_b)) = (a, b);
    let in_b: BTreeMap<String, u32> = parsed_b
        .exports
        .iter()
        .enumerate()
        .map(|(at, export)| (export.path.to_ascii_lowercase(), at as u32))
        .collect();
    let numbered_alike = parsed_a
        .names
        .iter()
        .zip(&parsed_b.names)
        .all(|(x, y)| x == y)
        && parsed_a
            .imports
            .iter()
            .zip(&parsed_b.imports)
            .all(|(x, y)| x.path == y.path)
        && parsed_a
            .exports
            .iter()
            .zip(&parsed_b.exports)
            .all(|(x, y)| x.path == y.path);
    let (header_a, header_b) = (
        rivals_uasset::read_header(bundle_a).ok(),
        rivals_uasset::read_header(bundle_b).ok(),
    );
    let mut out = Vec::new();
    for (at, export) in parsed_a.exports.iter().enumerate() {
        let Some(&theirs) = in_b.get(&export.path.to_ascii_lowercase()) else {
            continue;
        };
        let other = &parsed_b.exports[theirs as usize];
        let differs = |a: String, b: String| Difference {
            kind: FindingKind::Payload,
            what: export.path.clone(),
            only: None,
            a: Some(a),
            b: Some(b),
        };
        if rivals_uasset::is_texture_class(&export.class_name)
            && rivals_uasset::is_texture_class(&other.class_name)
        {
            let read =
                |bundle: &rivals_uasset::AssetBundle<'_>, parsed: &ParsedPackage, at: u32| {
                    rivals_uasset::read_texture(bundle, parsed, at).ok()
                };
            if let (Some(mine), Some(yours)) = (
                read(bundle_a, parsed_a, at as u32),
                read(bundle_b, parsed_b, theirs),
            ) && let Some((x, y)) = texture_difference(&mine, bundle_a, &yours, bundle_b)
            {
                out.push(differs(x, y));
            }
            continue;
        }
        let (Some(header_a), Some(header_b)) = (&header_a, &header_b) else {
            continue;
        };
        let rest = |bundle, header, at, export| unread_bytes(bundle, header, at, export);
        match (
            rest(bundle_a, header_a, at as u32, export),
            rest(bundle_b, header_b, theirs, other),
        ) {
            (Some((kind, mine)), Some((_, yours))) if mine != yours => {
                let note = if mine.len() == yours.len() && !numbered_alike {
                    ", which may be no more than names or objects numbered otherwise"
                } else {
                    ""
                };
                out.push(differs(
                    format!("{kind}, {} bytes", mine.len()),
                    format!(
                        "{} bytes, first differing at byte {}{note}",
                        yours.len(),
                        mine.iter().zip(yours).take_while(|(x, y)| x == y).count()
                    ),
                ));
            }
            (Some(_), Some(_)) => {}
            (mine, yours) if mine.is_some() || yours.is_some() => {
                out.push(differs(status_of(export), status_of(other)));
            }
            _ => {}
        }
    }
    out
}

/// The bytes an export keeps that the parse measures but does not decode, with what they are: a
/// class's payload, which for bytecode is the script and compared as text, or what is left after
/// what did decode.
fn unread_bytes<'a>(
    bundle: &rivals_uasset::AssetBundle<'a>,
    header: &retoc::legacy_asset::FLegacyPackageHeader,
    at: u32,
    export: &rivals_uasset::ParsedExport,
) -> Option<(&'static str, &'a [u8])> {
    let (bytes, _) = rivals_uasset::export_bytes(bundle, header, at).ok()?;
    let slice = |from: u64, to: u64| bytes.get(from as usize..(to as usize).min(bytes.len()));
    match &export.status {
        rivals_uasset::ExportStatus::Payload {
            consumed,
            payload_bytes,
            kind,
        } if *kind != "bytecode" => Some((kind, slice(*consumed, consumed + payload_bytes)?)),
        rivals_uasset::ExportStatus::Partial { consumed, .. } => {
            Some(("unread bytes", slice(*consumed, bytes.len() as u64)?))
        }
        _ => None,
    }
}

/// How an export read, for a difference in it.
fn status_of(export: &rivals_uasset::ParsedExport) -> String {
    match &export.status {
        rivals_uasset::ExportStatus::Complete => "read whole".into(),
        rivals_uasset::ExportStatus::Payload { kind, .. } => format!("holding {kind}"),
        rivals_uasset::ExportStatus::Partial { .. } => "with bytes left unread".into(),
        rivals_uasset::ExportStatus::Failed { reason } => format!("not read: {reason}"),
    }
}

/// How two textures differ, as each one's form: their formats, sizes and mips, or with those alike,
/// the inline pixels.
fn texture_difference(
    a: &rivals_uasset::TextureLayout,
    bundle_a: &rivals_uasset::AssetBundle<'_>,
    b: &rivals_uasset::TextureLayout,
    bundle_b: &rivals_uasset::AssetBundle<'_>,
) -> Option<(String, String)> {
    let form = |layout: &rivals_uasset::TextureLayout| {
        layout
            .platforms
            .iter()
            .map(|platform| {
                let mips: Vec<String> = platform
                    .mips
                    .iter()
                    .map(|mip| format!("{}x{} {}", mip.size_x, mip.size_y, mip.placement))
                    .collect();
                format!(
                    "{} {}x{}, {} slice(s){}, first mip {}, mips {}",
                    platform.format,
                    platform.size_x,
                    platform.size_y,
                    platform.slices,
                    if platform.cubemap { ", cubemap" } else { "" },
                    platform.first_mip,
                    mips.join(", ")
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    };
    let (form_a, form_b) = (form(a), form(b));
    if form_a != form_b {
        return Some((form_a, form_b));
    }
    let pixels = |layout: &rivals_uasset::TextureLayout,
                  bundle: &rivals_uasset::AssetBundle<'_>| {
        layout
            .platforms
            .iter()
            .flat_map(|platform| &platform.mips)
            .filter_map(|mip| {
                let at = mip.inline_at?;
                bundle.exports.get(at..at + usize::try_from(mip.size).ok()?)
            })
            .flatten()
            .copied()
            .collect::<Vec<u8>>()
    };
    let (mine, yours) = (pixels(a, bundle_a), pixels(b, bundle_b));
    (mine != yours).then(|| {
        (
            format!("inline pixels, {} bytes", mine.len()),
            format!(
                "{} bytes, first differing at byte {}",
                yours.len(),
                mine.iter().zip(&yours).take_while(|(x, y)| x == y).count()
            ),
        )
    })
}

/// The string table entries both copies have under one key, with another source string.
fn string_differences(a: &ParsedPackage, b: &ParsedPackage) -> Vec<Difference> {
    let key = |text: &str| text.to_ascii_lowercase();
    let tables_b: BTreeMap<String, &rivals_uasset::StringTable> = b
        .exports
        .iter()
        .filter_map(|export| Some((key(&export.path), export.string_table.as_ref()?)))
        .collect();
    let mut out = Vec::new();
    for export in &a.exports {
        let (Some(table), Some(theirs)) = (&export.string_table, tables_b.get(&key(&export.path)))
        else {
            continue;
        };
        let sources: BTreeMap<String, &str> = theirs
            .entries
            .iter()
            .map(|entry| (key(&entry.key), entry.source.as_str()))
            .collect();
        for entry in &table.entries {
            if let Some(&source) = sources.get(&key(&entry.key))
                && source != entry.source
            {
                out.push(Difference {
                    kind: FindingKind::String,
                    what: entry.key.clone(),
                    only: None,
                    a: Some(entry.source.clone()),
                    b: Some(source.to_string()),
                });
            }
        }
    }
    out
}

/// The runs of lines in which two scripts' texts differ, each with [`CONTEXT`] lines around it, as
/// B prints them.
///
/// A label is a code offset, so a statement put in renames every label after it. The texts are
/// first compared with every label made one, and the lines that then read the same pair each label
/// B defines with A's at the same place; compared again with B's labels named as A's, a jump that
/// lands elsewhere still differs, and one whose code only moved does not. The offset a stub enters
/// its event graph at, and an offset printed raw, are left out the same way.
pub fn line_diff(a: &[&str], b: &[&str]) -> Vec<Vec<DiffLine>> {
    // Labels pair first where the code they label reads the same, then where only the lines around
    // them do, as when a statement was put in right after one.
    let mut as_a: BTreeMap<String, String> = BTreeMap::new();
    let mut taken: BTreeSet<String> = BTreeSet::new();
    for (keys_a, keys_b) in [
        (anchored(a, true), anchored(b, true)),
        (anchored(a, false), anchored(b, false)),
    ] {
        for op in similar::capture_diff_slices(Algorithm::Myers, &keys_a, &keys_b) {
            let (tag, old, new) = op.as_tag_tuple();
            if tag != DiffTag::Equal {
                continue;
            }
            for (x, y) in old.zip(new) {
                for (theirs, ours) in defined(b[y]).into_iter().zip(defined(a[x])) {
                    if !as_a.contains_key(&theirs) && !taken.contains(&ours) {
                        taken.insert(ours.clone());
                        as_a.insert(theirs, ours);
                    }
                }
            }
        }
    }
    let defined_b: BTreeSet<String> = b.iter().flat_map(|line| defined(line)).collect();
    let keys_a: Vec<String> = a.iter().map(|line| entry_literals(line)).collect();
    let keys_b: Vec<String> = b
        .iter()
        .map(|line| {
            relabel(&entry_literals(line), &mut |label| match (
                defined_b.contains(label),
                as_a.get(label),
            ) {
                (true, Some(theirs)) => theirs.clone(),
                (true, None) => format!("@+{}", &label[1..]),
                (false, _) => label.to_string(),
            })
        })
        .collect();
    let ops = similar::capture_diff_slices(Algorithm::Myers, &keys_a, &keys_b);
    if ops.iter().all(|op| op.as_tag_tuple().0 == DiffTag::Equal) {
        return Vec::new();
    }
    similar::group_diff_ops(ops, CONTEXT)
        .into_iter()
        .map(|group| {
            let mut lines = Vec::new();
            for op in group {
                let (tag, old, new) = op.as_tag_tuple();
                let line = |only, text: &str| DiffLine {
                    only,
                    text: text.to_string(),
                };
                match tag {
                    DiffTag::Equal => lines.extend(new.map(|y| line(None, b[y]))),
                    DiffTag::Delete => lines.extend(old.map(|x| line(Some(Only::A), a[x]))),
                    DiffTag::Insert => lines.extend(new.map(|y| line(Some(Only::B), b[y]))),
                    DiffTag::Replace => {
                        lines.extend(old.map(|x| line(Some(Only::A), a[x])));
                        lines.extend(new.map(|y| line(Some(Only::B), b[y])));
                    }
                }
            }
            lines
        })
        .collect()
}

/// Each line of a script's text with every label made one. With `anchor`, a line that only defines
/// a label also carries the statement it labels, so it pairs with the label standing at the same
/// code, not with whichever label comes first.
fn anchored(lines: &[&str], anchor: bool) -> Vec<String> {
    let blank: Vec<String> = lines
        .iter()
        .map(|line| relabel(&entry_literals(line), &mut |_| "@".to_string()))
        .collect();
    let label_only = |line: &str| {
        let rest = line.trim_start();
        rest.starts_with('@')
            && defined(rest).len() == 1
            && rest.split_once(':').is_some_and(|(_, after)| {
                let after = after.trim_start();
                after.is_empty() || after.starts_with(';')
            })
    };
    (0..lines.len())
        .map(|at| {
            if !anchor || !label_only(lines[at]) {
                return blank[at].clone();
            }
            let labelled = (at + 1..lines.len())
                .find(|&next| !label_only(lines[next]))
                .map_or("", |next| blank[next].as_str());
            format!(
                "{}
{labelled}",
                blank[at]
            )
        })
        .collect()
}

/// `line` with each label in it, `@` and four or more upper-case hex digits as the printer names
/// them, given by `rename`.
fn relabel(line: &str, rename: &mut dyn FnMut(&str) -> String) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = rest.find('@') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let digits = after
            .bytes()
            .take_while(|byte| byte.is_ascii_digit() || (b'A'..=b'F').contains(byte))
            .count();
        let ends = after
            .as_bytes()
            .get(digits)
            .is_none_or(|byte| !byte.is_ascii_alphanumeric() && *byte != b'_');
        if digits >= 4 && ends {
            out.push_str(&rename(&rest[at..at + 1 + digits]));
        } else {
            out.push('@');
            out.push_str(&after[..digits]);
        }
        rest = &after[digits..];
    }
    out.push_str(rest);
    out
}

/// The labels `line` defines, in order: one at its start or before an expression inside it, each
/// followed by a colon.
fn defined(line: &str) -> Vec<String> {
    let mut found = Vec::new();
    relabel(line, &mut |label| {
        found.push(label.to_string());
        label.to_string()
    });
    let mut out = Vec::new();
    let mut from = 0;
    for label in found {
        if let Some(at) = line[from..].find(&label) {
            let end = from + at + label.len();
            if line[end..].starts_with(':') {
                out.push(label);
            }
            from = end;
        }
    }
    out
}

/// `line` with the offset a stub enters its event graph at, `ExecuteUbergraph_X(N)`, and any
/// offset printed raw, `#0x…`, each made one, since they move with the code.
fn entry_literals(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    loop {
        let graph = rest.find("ExecuteUbergraph_");
        let raw = rest.find("#0x");
        let Some(at) = graph.into_iter().chain(raw).min() else {
            break;
        };
        if Some(at) == raw {
            let digits = rest[at + 3..]
                .bytes()
                .take_while(u8::is_ascii_hexdigit)
                .count();
            out.push_str(&rest[..at]);
            out.push_str("#0x…");
            rest = &rest[at + 3 + digits..];
            continue;
        }
        let name = rest[at..]
            .bytes()
            .take_while(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
            .count();
        let call = at + name;
        let digits = rest[call..].strip_prefix('(').map_or(0, |args| {
            args.bytes().take_while(u8::is_ascii_digit).count()
        });
        if digits > 0 && rest[call + 1 + digits..].starts_with(')') {
            out.push_str(&rest[..call]);
            out.push_str("(entry)");
            rest = &rest[call + 2 + digits..];
        } else {
            out.push_str(&rest[..call]);
            rest = &rest[call..];
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use rivals_uasset::script_fixture::{event_graph, event_graph_two_events, local};

    /// The event graph's text with `Let LocalVariable(Count) = 6` put in after its first jump,
    /// saved and read back.
    fn graph_with_a_statement_more() -> ParsedPackage {
        let built = event_graph().build();
        let before = built.parsed();
        let text = rivals_uasset::print_script(&before, 1)
            .expect("a script")
            .text();
        let grown = text.replacen(
            "Jump LocalVariable(EntryPoint)\n",
            "Jump LocalVariable(EntryPoint)\nLet LocalVariable(Count) = 6\n",
            1,
        );
        let patched = rivals_uasset::patch_package(
            &built.bundle(),
            &before,
            &rivals_uasset::PackageEdits {
                script_texts: vec![rivals_uasset::ScriptTextEdit {
                    export: 1,
                    text: grown,
                    was: None,
                    object: None,
                }],
                ..Default::default()
            },
            None,
        )
        .expect("patch");
        rivals_uasset::parse_package(
            &rivals_uasset::AssetBundle {
                asset: &patched.asset,
                exports: &patched.exports,
            },
            None,
        )
        .expect("parse")
    }

    /// A package compared with itself differs in nothing.
    #[test]
    fn a_package_differs_from_itself_in_nothing() {
        let parsed = event_graph().build().parsed();
        let diff = compare_packages(&parsed, &parsed);
        assert!(diff.differences.is_empty(), "{:?}", diff.differences);
        assert!(diff.scripts.is_empty() && diff.moved.is_empty() && diff.values.is_empty());
    }

    /// A statement put in shows as that one line, though every label after it was renamed, and
    /// the event entering the graph after it is said to have only moved.
    #[test]
    fn a_statement_put_in_is_one_line_and_the_event_after_it_only_moved() {
        let before = event_graph().build().parsed();
        let after = graph_with_a_statement_more();
        let diff = compare_packages(&before, &after);
        assert!(diff.differences.is_empty(), "{:?}", diff.differences);
        let [script] = diff.scripts.as_slice() else {
            panic!("{:?}", diff.scripts);
        };
        let changed: Vec<&DiffLine> = script
            .hunks
            .iter()
            .flatten()
            .filter(|line| line.only.is_some())
            .collect();
        assert_eq!(
            changed,
            [&DiffLine {
                only: Some(Only::B),
                text: "Let LocalVariable(Count) = 6".into(),
            }]
        );
        assert_eq!(script.statements[1], script.statements[0] + 1);
        assert_eq!(diff.moved, ["ReceiveBeginPlay"]);
    }

    /// A variable and an event only one copy has are said to be only there, and the event's
    /// entry into the graph shows in the graph's text.
    #[test]
    fn members_only_one_copy_has_are_said_to_be_only_there() {
        let mut with_variable = event_graph();
        with_variable.names.push("bShowAll".into());
        with_variable
            .members
            .push(local("bShowAll", "BoolProperty"));
        let base = event_graph().build().parsed();
        let diff = compare_packages(&base, &with_variable.build().parsed());
        assert!(
            diff.differences.iter().any(|difference| {
                difference.kind == FindingKind::Variable
                    && difference.what == "bShowAll"
                    && difference.only == Some(Only::B)
            }),
            "{:?}",
            diff.differences
        );
        let two = event_graph_two_events().build().parsed();
        let diff = compare_packages(&base, &two);
        assert!(
            diff.differences.iter().any(|difference| {
                difference.kind == FindingKind::Function
                    && difference.what == "ReceiveTick"
                    && difference.only == Some(Only::B)
            }),
            "{:?}",
            diff.differences
        );
        assert!(
            diff.scripts
                .iter()
                .flat_map(|script| script.hunks.iter().flatten())
                .any(|line| line.only == Some(Only::B)
                    && line.text.contains("ReceiveTick enters here")),
            "{:?}",
            diff.scripts
        );
        let back = compare_packages(&two, &base);
        assert!(back.differences.iter().any(|difference| {
            difference.what == "ReceiveTick" && difference.only == Some(Only::A)
        }));
    }

    /// A Blueprint struct's default values are compared as `defaults.Field`, and left alone when one
    /// copy's did not read.
    #[test]
    fn struct_defaults_are_compared_with_the_values() {
        let field = |value: i64| rivals_uasset::PropertyEntry {
            name: "Damage".into(),
            element: None,
            value: rivals_uasset::PropertyValue::Int { value },
            span: None,
            slot: None,
        };
        let mut a = event_graph().build().parsed();
        let mut b = a.clone();
        a.exports[0].defaults = vec![field(3)];
        b.exports[0].defaults = vec![field(5)];
        a.exports[0].status = rivals_uasset::ExportStatus::Complete;
        b.exports[0].status = rivals_uasset::ExportStatus::Complete;
        let values = compare_packages(&a, &b).values;
        assert_eq!(
            values
                .iter()
                .map(|value| (value.path.as_str(), value.a.as_str(), value.b.as_str()))
                .collect::<Vec<_>>(),
            [("defaults.Damage", "3", "5")]
        );
        // Defaults that did not read are not "not stored".
        b.exports[0].defaults.clear();
        b.exports[0].status = rivals_uasset::ExportStatus::Partial {
            consumed: 0,
            expected: 1,
        };
        assert!(compare_packages(&a, &b).values.is_empty());
    }

    /// The bytes an export keeps that the parse does not decode are compared: two copies whose
    /// unread rests differ give one payload difference, and the same rests give none.
    #[test]
    fn unread_bytes_that_differ_are_a_payload_difference() {
        use rivals_uasset::tagged_fixture::{int, name, package_of};
        let with_rest = |rest: &[u8]| {
            let mut e = Vec::new();
            int(&mut e, "Damage", 1);
            name(&mut e, "None");
            e.extend_from_slice(&0i32.to_le_bytes());
            e.extend_from_slice(rest);
            package_of(e)
        };
        let ((asset_a, exports_a), (asset_b, exports_b)) =
            (with_rest(&[9, 9, 9, 9]), with_rest(&[9, 9, 8, 9]));
        let bundle_a = rivals_uasset::AssetBundle {
            asset: &asset_a,
            exports: &exports_a,
        };
        let bundle_b = rivals_uasset::AssetBundle {
            asset: &asset_b,
            exports: &exports_b,
        };
        let parse = |bundle: &rivals_uasset::AssetBundle<'_>| {
            rivals_uasset::parse_package(bundle, None).expect("parses")
        };
        let (a, b) = (parse(&bundle_a), parse(&bundle_b));
        assert!(
            matches!(
                a.exports[0].status,
                rivals_uasset::ExportStatus::Partial { .. }
            ),
            "{:?}",
            a.exports[0].status
        );
        let found = payload_differences((&bundle_a, &a), (&bundle_b, &b));
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].kind, FindingKind::Payload);
        assert!(
            found[0]
                .b
                .as_deref()
                .is_some_and(|b| b.contains("first differing at byte 2")),
            "{found:?}"
        );
        assert!(payload_differences((&bundle_a, &a), (&bundle_a, &a)).is_empty());
    }

    /// Labels renamed because code moved make no difference, a jump landing on another statement
    /// does, and a string spelt like a label is only a string.
    #[test]
    fn a_label_counts_only_where_its_code_does() {
        let a = [
            "Jump @0010 unless LocalVariable(Go)",
            "@0008:",
            "Let LocalVariable(N) = 1",
            "@0010:",
            "Return Nothing",
        ];
        let shifted = [
            "Jump @0020 unless LocalVariable(Go)",
            "@0018:",
            "Let LocalVariable(N) = 1",
            "@0020:",
            "Return Nothing",
        ];
        assert!(line_diff(&a, &shifted).is_empty());
        let retargeted = [
            "Jump @0008 unless LocalVariable(Go)",
            "@0008:",
            "Let LocalVariable(N) = 1",
            "@0010:",
            "Return Nothing",
        ];
        let changed: Vec<DiffLine> = line_diff(&a, &retargeted)
            .into_iter()
            .flatten()
            .filter(|line| line.only.is_some())
            .collect();
        assert_eq!(
            changed,
            [
                DiffLine {
                    only: Some(Only::A),
                    text: a[0].into(),
                },
                DiffLine {
                    only: Some(Only::B),
                    text: retargeted[0].into(),
                },
            ]
        );
        let said = ["AppendLog(\"@ABCD: hi\")", "Return Nothing"];
        assert!(line_diff(&said, &said).is_empty());
        let entered = ["LocalFinalFunction /Game/X.BP_C:ExecuteUbergraph_BP(40)"];
        let moved = ["LocalFinalFunction /Game/X.BP_C:ExecuteUbergraph_BP(52)"];
        assert!(line_diff(&entered, &moved).is_empty());
    }
}

/// Set `RIVALS_GAME_ROOT`, `RIVALS_USMAP` and `OODLE_LIB_PATH` to run these. Skipped otherwise.
#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod game_data_tests {
    use super::*;
    use crate::asset::AssetSource;
    use crate::schema_synth::PackageSource;

    const TABLE: &str = "Marvel/Content/Marvel/Data/DataTable/UI/CardTable.uasset";
    const TITLES: &str = "Marvel/Content/Marvel/Data/DataAsset/Career/MarvelHeroTitleData.uasset";
    /// A texture whose mips are all inline.
    const TEXTURE: &str = "Marvel/Content/Marvel/AbilitySystem/Cue/T_SprayPaint_Default.uasset";

    /// A texture whose inline pixels change at the same size shows as a payload difference, which
    /// its values alone would not.
    #[test]
    fn inline_pixels_that_differ_are_a_payload_difference() {
        let (Ok(root), Ok(usmap)) = (
            std::env::var("RIVALS_GAME_ROOT"),
            std::env::var("RIVALS_USMAP"),
        ) else {
            return;
        };
        if std::env::var_os("OODLE_LIB_PATH").is_none() {
            return;
        }
        let mappings = crate::mappings::load(Path::new(&usmap)).expect("mappings");
        let container = format!(
            "{}/MarvelGame/Marvel/Content/Paks/pakchunk0-Windows.utoc",
            root.replace('\\', "/")
        );
        let loaded =
            crate::asset::load_bundle(&root, &container, TEXTURE, AssetSource::Utoc).expect("load");
        let bundle = rivals_uasset::AssetBundle {
            asset: &loaded.asset_file_buffer,
            exports: &loaded.exports_file_buffer,
        };
        let source = PackageSource {
            game_root: &root,
            container: &container,
            entry: TEXTURE,
            kind: AssetSource::Utoc,
            mod_container: None,
        };
        let parsed =
            crate::schema_synth::parse_package(&bundle, Some(&mappings), &source).expect("parse");
        let texture = parsed
            .exports
            .iter()
            .position(|export| rivals_uasset::is_texture_class(&export.class_name))
            .expect("a texture") as u32;
        let layout = rivals_uasset::read_texture(&bundle, &parsed, texture).expect("its mips");
        let mip = layout.platforms[0]
            .mips
            .iter()
            .find(|mip| mip.placement == "inline")
            .expect("an inline mip");
        let patched = rivals_uasset::patch_package(
            &bundle,
            &parsed,
            &rivals_uasset::PackageEdits {
                bulk: vec![rivals_uasset::BulkEdit {
                    resource: mip.resource as u32,
                    bytes: vec![0x7F; mip.size as usize],
                }],
                ..Default::default()
            },
            Some(&mappings),
        )
        .expect("new pixels");
        let after_bundle = rivals_uasset::AssetBundle {
            asset: &patched.asset,
            exports: &patched.exports,
        };
        let after = crate::schema_synth::parse_package(&after_bundle, Some(&mappings), &source)
            .expect("parse");
        assert!(compare_packages(&parsed, &after).values.is_empty());
        let found = payload_differences((&bundle, &parsed), (&after_bundle, &after));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0]
                .a
                .as_deref()
                .is_some_and(|a| a.starts_with("inline pixels")),
            "{found:?}"
        );
    }
    const STRINGS: &str =
        "Marvel/Content/Marvel/Data/DataTable/UI/HeroSkin/UIHeroKillTipsTable.uasset";

    /// Two mods share a package byte for byte and a table one renames a row of, and the second
    /// holds a package more: the shared one is counted the same, the extra one is only in B, and
    /// the table's row is only in A under its old name and only in B under its new one. Pairing
    /// alone reads no package and finds the same packages.
    #[test]
    fn two_mods_are_compared_package_by_package() {
        let (Ok(root), Ok(usmap)) = (
            std::env::var("RIVALS_GAME_ROOT"),
            std::env::var("RIVALS_USMAP"),
        ) else {
            return;
        };
        if std::env::var_os("OODLE_LIB_PATH").is_none() {
            return;
        }
        let mappings = crate::mappings::load(Path::new(&usmap)).expect("mappings");
        let container = format!(
            "{}/MarvelGame/Marvel/Content/Paks/pakchunk0-Windows.utoc",
            root.replace('\\', "/")
        );
        let load = |entry: &str| {
            crate::asset::load_bundle(&root, &container, entry, AssetSource::Utoc).expect("load")
        };
        let table = load(TABLE);
        let bundle = rivals_uasset::AssetBundle {
            asset: &table.asset_file_buffer,
            exports: &table.exports_file_buffer,
        };
        let parsed = crate::schema_synth::parse_package(
            &bundle,
            Some(&mappings),
            &PackageSource {
                game_root: &root,
                container: &container,
                entry: TABLE,
                kind: AssetSource::Utoc,
                mod_container: None,
            },
        )
        .expect("parse");
        let (export, old) = parsed
            .exports
            .iter()
            .find_map(|export| {
                let row = export.data_table.as_ref()?.rows.first()?;
                Some((export.index, row.name.clone()))
            })
            .expect("a row");
        const RENAMED: &str = "RivalsProbeRenamedRow";
        let renamed = rivals_uasset::patch_package(
            &bundle,
            &parsed,
            &rivals_uasset::PackageEdits {
                rows: vec![rivals_uasset::RowEdit {
                    export,
                    object: None,
                    op: rivals_uasset::RowOp::Rename {
                        name: old.clone(),
                        to: RENAMED.into(),
                    },
                }],
                ..Default::default()
            },
            Some(&mappings),
        )
        .expect("rename the row");
        let (titles, strings) = (load(TITLES), load(STRINGS));

        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rivals-mod-diff-{stamp}"));
        let package =
            |entry: &str, asset: &[u8], exports: &[u8]| crate::pak::iostore_out::PackageBytes {
                entry: entry.into(),
                asset: asset.to_vec(),
                exports: exports.to_vec(),
                bulk: None,
                optional_bulk: None,
                memory_mapped_bulk: None,
                shader_map_hashes: Vec::new(),
            };
        let write = |utoc: &Path, packages: Vec<crate::pak::iostore_out::PackageBytes>| {
            let mut packages: Vec<Option<_>> = packages.into_iter().map(Some).collect();
            crate::pak::iostore_out::write_batch_into_iostore(
                utoc,
                packages.len(),
                |at| packages.get_mut(at).and_then(Option::take),
                &crate::pak::iostore_out::IoStoreOptions {
                    compression: crate::pak::profile::PackCompression::Zlib,
                    ..Default::default()
                },
            )
            .expect("write a probe");
        };
        std::fs::create_dir_all(dir.join("a")).expect("scratch");
        std::fs::create_dir_all(dir.join("b")).expect("scratch");
        // The same name in both, as a backup and its build would have.
        let (a, b) = (
            dir.join("a/DiffProbe_9999999_P.utoc"),
            dir.join("b/DiffProbe_9999999_P.utoc"),
        );
        let shared = || {
            package(
                TITLES,
                &titles.asset_file_buffer,
                &titles.exports_file_buffer,
            )
        };
        write(
            &a,
            vec![
                shared(),
                package(TABLE, &table.asset_file_buffer, &table.exports_file_buffer),
            ],
        );
        write(
            &b,
            vec![
                shared(),
                package(TABLE, &renamed.asset, &renamed.exports),
                package(
                    STRINGS,
                    &strings.asset_file_buffer,
                    &strings.exports_file_buffer,
                ),
            ],
        );

        let paired = pair_mods(&a, &b);
        let compared = mod_diff(&root, &a, &b, Some(&mappings));
        crate::pak::containers::drop_cached_store();
        let _ = std::fs::remove_dir_all(&dir);
        let (paired, compared) = (paired.expect("pair"), compared.expect("compare"));
        for diff in [&paired, &compared] {
            assert_eq!(diff.identical, 1);
            assert!(diff.only_a.is_empty(), "{:?}", diff.only_a);
            assert_eq!(diff.only_b, [STRINGS]);
            assert_eq!(diff.changed.len(), 1);
            assert_eq!(diff.changed[0].package, TABLE);
            assert_eq!(diff.changed[0].chunks, ["uasset"]);
        }
        let rows: Vec<(&str, Option<Only>)> = compared.changed[0]
            .differences
            .iter()
            .filter(|difference| difference.kind == FindingKind::Row)
            .map(|difference| (difference.what.as_str(), difference.only))
            .collect();
        assert!(rows.contains(&(old.as_str(), Some(Only::A))), "{rows:?}");
        assert!(rows.contains(&(RENAMED, Some(Only::B))), "{rows:?}");
        assert!(
            paired.changed[0].differences.is_empty(),
            "pairing reads nothing"
        );
    }
}
