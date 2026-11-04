//! Rewrites a package a stock UE 5.3 editor cooked into the layout this game's engine reads: no
//! engine versions stamped in its header, a gameplay tag container after each skeletal mesh
//! material slot, sixteen-byte static mesh screen sizes and a static mesh's properties written
//! unversioned, a marker after each string table source, and the material tag plugins' classes
//! named as the engine class the game has.
//!
//! Only tagged packages are rewritten. Their properties are read by name, so the fields this game
//! inserted into its structs are no trouble; what differs is native data, which is spliced. An
//! unversioned stock cook would also need every header renumbered against a stock schema, so one
//! is refused with the reason.

use std::collections::HashMap;

use retoc::legacy_asset::{FLegacyPackageHeader, FObjectImport, FPackageNameMap};
use retoc::zen::FPackageIndex;
use serde::Serialize;

use crate::mappings::Mappings;
use crate::package::{AssetBundle, ExportStatus, ParsedPackage, parse_package, read_header};
use crate::stock::{StockGapKind, StockRender};
use crate::value::{PropertyEntry, PropertyValue};
use crate::write::{HeaderDraft, Splice, rewrite};

/// The script packages of the editor plugins that carry material tags and hidden-by-default slots
/// on a mesh. The game ships neither, so their classes are named as the engine's `AssetUserData`.
pub const CARRIER_PLUGINS: &[&str] = &[
    "/Script/MaterialTagPlugin",
    "/Script/RivalsMeshMaterialManager",
];

/// A package rewritten into this game's layout.
#[derive(Debug)]
pub struct Conformed {
    pub asset: Vec<u8>,
    pub exports: Vec<u8>,
    pub changes: Vec<ConformChange>,
}

/// One thing the rewrite changed, said the way a report prints it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConformChange {
    pub export: Option<u32>,
    pub change: String,
}

/// The package rewritten into this game's layout, `None` when it already matches, or why it
/// cannot be: a stamp from another engine version, an unversioned stock cook, a static mesh with
/// Nanite data or the editor's distance field, or a carrier whose tags fit no material slot.
pub fn conform_package(
    bundle: &AssetBundle<'_>,
    mappings: Option<&Mappings>,
) -> Result<Option<Conformed>, String> {
    let header = read_header(bundle)?;
    if header.summary.versioning_info.is_unversioned {
        return conform_unstamped(bundle, &header, mappings);
    }
    // Once unstamped the package is read under the game's versions, which only a 5.3 cook matches.
    let stamp = header.summary.versioning_info.package_file_version;
    let game = crate::package::FALLBACK_ENGINE_VERSION.package_file_version();
    if stamp != game {
        return Err(format!(
            "{} was cooked by an editor whose package version is UE4 {} and UE5 {}, not this \
             game's UE4 {} and UE5 {}; cook it in UE 5.3",
            header.summary.package_name,
            stamp.file_version_ue4,
            stamp.file_version_ue5,
            game.file_version_ue4,
            game.file_version_ue5
        ));
    }
    let unstamped = crate::write::unstamp(bundle)?;
    let stamp_change = ConformChange {
        export: None,
        change: "engine version stamp taken out of the header, as `-unversioned` cooks it".into(),
    };
    let inner = AssetBundle {
        asset: &unstamped.asset,
        exports: &unstamped.exports,
    };
    let converted = conform_unstamped(&inner, &read_header(&inner)?, mappings)?;
    Ok(Some(match converted {
        Some(mut done) => {
            done.changes.insert(0, stamp_change);
            done
        }
        None => Conformed {
            asset: unstamped.asset,
            exports: unstamped.exports,
            changes: vec![stamp_change],
        },
    }))
}

fn conform_unstamped(
    bundle: &AssetBundle<'_>,
    header: &FLegacyPackageHeader,
    mappings: Option<&Mappings>,
) -> Result<Option<Conformed>, String> {
    let carriers = carrier_imports(header);
    let classes: Vec<String> = header
        .exports
        .iter()
        .map(|export| crate::package::object_short_name(header, export.class_index))
        .map(Option::unwrap_or_default)
        .collect();
    let candidate = carriers.iter().any(Option::is_some)
        || classes.iter().any(|class| {
            matches!(
                class.as_str(),
                "SkeletalMesh" | "StaticMesh" | "StringTable"
            )
        });
    if !candidate {
        return Ok(None);
    }
    let unversioned = crate::package::package_info(header).unversioned_properties;
    if unversioned && mappings.is_none() {
        return Ok(None);
    }
    let parsed = parse_package(bundle, mappings)?;
    if unversioned {
        return match parsed.stock_gaps.first() {
            Some(gap) => Err(format!(
                "{} was cooked by a stock editor with unversioned properties ({}); recook it with \
                 tagged properties (`CanUseUnversionedPropertySerialization=False`) so it can be \
                 converted",
                parsed.info.package_name,
                gap.kind.describe()
            )),
            None => Ok(None),
        };
    }
    for gap in &parsed.stock_gaps {
        if let StockGapKind::RenderData { found } = &gap.kind
            && let Some(fix) = found.fix()
        {
            return Err(format!(
                "{}: {}; cook it again with {fix}",
                parsed.info.package_name,
                gap.kind.describe()
            ));
        }
    }
    let converted = convert(bundle, header, &carriers, &parsed, mappings)?;
    if !classes.iter().any(|class| class == "StaticMesh") {
        return Ok(converted);
    }
    // The game crashes on a static mesh whose properties are tagged.
    let mappings = mappings.ok_or_else(|| {
        format!(
            "{} is a static mesh with tagged properties, which the game crashes on, and writing \
             them unversioned needs the game's mappings",
            parsed.info.package_name
        )
    })?;
    let (asset, exports, mut changes) = match converted {
        Some(done) => (done.asset, done.exports, done.changes),
        None => (bundle.asset.to_vec(), bundle.exports.to_vec(), Vec::new()),
    };
    let done = crate::unversion::unversion_package(
        &AssetBundle {
            asset: &asset,
            exports: &exports,
        },
        mappings,
    )?;
    changes.push(ConformChange {
        export: None,
        change: "properties written unversioned, the form the game reads a static mesh in".into(),
    });
    changes.extend(done.dropped.into_iter().map(|property| ConformChange {
        export: None,
        change: format!("{property} left out, since the game's schema has no slot for it"),
    }));
    Ok(Some(Conformed {
        asset: done.asset,
        exports: done.exports,
        changes,
    }))
}

/// [`conform_package`] without its refusal of unversioned packages: the gaps a parse finds are
/// filled whatever the package's property form. For checking the conversion against packages the
/// game cooked, which are unversioned and already read in this game's layout.
pub fn conform_layout(
    bundle: &AssetBundle<'_>,
    mappings: Option<&Mappings>,
) -> Result<Option<Conformed>, String> {
    let header = read_header(bundle)?;
    let carriers = carrier_imports(&header);
    let parsed = parse_package(bundle, mappings)?;
    convert(bundle, &header, &carriers, &parsed, mappings)
}

fn convert(
    bundle: &AssetBundle<'_>,
    header: &FLegacyPackageHeader,
    carriers: &[Option<CarrierRole>],
    parsed: &ParsedPackage,
    mappings: Option<&Mappings>,
) -> Result<Option<Conformed>, String> {
    let mut names = header.name_map.clone();
    let mut changes = Vec::new();
    let mut splices = Vec::new();
    let slot_tags = material_slot_tags(parsed);
    let mut tagged_slots = 0;
    for gap in &parsed.stock_gaps {
        let bytes = match &gap.kind {
            StockGapKind::RenderData { found } => {
                if let StockRender::Unread { why } = found {
                    changes.push(ConformChange {
                        export: Some(gap.export),
                        change: format!(
                            "not checked for distance fields or Nanite, which the game needs off: \
                             its render data could not be read through ({why})"
                        ),
                    });
                }
                continue;
            }
            StockGapKind::MaterialTags { slot } => {
                let tags = slot_tags
                    .get(&slot.to_ascii_lowercase())
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                if !tags.is_empty() {
                    tagged_slots += 1;
                }
                tag_container(tags, &mut names)
            }
            other => other.filler(),
        };
        splices.push(Splice {
            start: gap.at,
            end: gap.at,
            bytes,
        });
    }
    if !slot_tags.is_empty() && tagged_slots < slot_tags.len() {
        let missing: Vec<&str> = slot_tags
            .keys()
            .filter(|slot| {
                !parsed.stock_gaps.iter().any(|gap| {
                    matches!(&gap.kind, StockGapKind::MaterialTags { slot: held }
                        if held.eq_ignore_ascii_case(slot))
                })
            })
            .map(String::as_str)
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "the material tags name slot(s) the mesh does not have: {}",
                missing.join(", ")
            ));
        }
    }
    for (export, count) in gap_counts(parsed) {
        changes.push(ConformChange {
            export: Some(export.0),
            change: export.1.to_string().replace("{n}", &count.to_string()),
        });
    }
    if tagged_slots > 0 {
        changes.push(ConformChange {
            export: None,
            change: format!("{tagged_slots} material slot(s) took their gameplay tags from the material tag data"),
        });
    }
    let hidden = hidden_material_splices(bundle, parsed, &mut names, &mut changes)?;
    splices.extend(hidden);

    let mut imports = header.imports.clone();
    for (index, role) in carriers.iter().enumerate() {
        let Some(role) = role else {
            continue;
        };
        let import = &mut imports[index];
        let from = crate::package::dotted_path(header, FPackageIndex::create_import(index as u32))
            .unwrap_or_default();
        match role {
            CarrierRole::Package => import.object_name = names.store("/Script/Engine"),
            CarrierRole::Class => import.object_name = names.store("AssetUserData"),
            CarrierRole::Default => {
                import.object_name = names.store("Default__AssetUserData");
                import.class_package = names.store("/Script/Engine");
                import.class_name = names.store("AssetUserData");
            }
        }
        changes.push(ConformChange {
            export: None,
            change: format!("import {from} named as {}", role.engine_path()),
        });
    }
    if splices.is_empty() && carriers.iter().all(Option::is_none) {
        return Ok(None);
    }
    splices.sort_by_key(|splice| (splice.start, splice.end));
    // A table handed over is written whether it changed or not, which widens the header's count
    // of names the exports reach, so one the conversion left alone stays as it was read.
    let names_grew = names.num_names() != header.name_map.num_names();
    let renamed = carriers.iter().any(Option::is_some);
    let rewritten = rewrite(
        bundle,
        &splices,
        HeaderDraft {
            names: names_grew.then_some(names),
            imports: renamed.then_some(imports),
            ..Default::default()
        },
    )?;
    verify(parsed, &rewritten.asset, &rewritten.exports, mappings)?;
    Ok(Some(Conformed {
        asset: rewritten.asset,
        exports: rewritten.exports,
        changes,
    }))
}

/// Whether the rewrite reads as this game's layout throughout: no gap left, every export reading
/// as it did, and each LOD that took hidden-material flags holding them.
fn verify(
    before: &ParsedPackage,
    asset: &[u8],
    exports: &[u8],
    mappings: Option<&Mappings>,
) -> Result<(), String> {
    let after = parse_package(&AssetBundle { asset, exports }, mappings)
        .map_err(|e| format!("the converted package does not read back: {e}"))?;
    if let Some(gap) = after.stock_gaps.first() {
        return Err(format!(
            "the converted package still reads as a stock cook: {}",
            gap.kind.describe()
        ));
    }
    if before.exports.len() != after.exports.len() {
        return Err("the conversion changed how many exports the package has".into());
    }
    for (was, is) in before.exports.iter().zip(&after.exports) {
        if status_kind(&was.status) != status_kind(&is.status) {
            return Err(format!(
                "{} reads as {} after the conversion where it read as {}",
                was.object_name,
                status_kind(&is.status),
                status_kind(&was.status)
            ));
        }
    }
    Ok(())
}

fn status_kind(status: &ExportStatus) -> &'static str {
    match status {
        ExportStatus::Complete => "complete",
        ExportStatus::Payload { kind, .. } => kind,
        ExportStatus::Partial { .. } => "partial",
        ExportStatus::Failed { .. } => "failed",
    }
}

/// How many gaps of each kind each export had, as report lines with `{n}` for the count.
fn gap_counts(parsed: &ParsedPackage) -> Vec<((u32, &'static str), usize)> {
    let mut counts: Vec<((u32, &'static str), usize)> = Vec::new();
    for gap in &parsed.stock_gaps {
        let line = match gap.kind {
            StockGapKind::MaterialTags { .. } => {
                "{n} material slot(s) given a gameplay tag container"
            }
            StockGapKind::ScreenSize { .. } => {
                "{n} LOD screen size(s) given the distance this game switches LODs at: 15, 45 \
                 and 130 m for LODs 1 to 3, as its own meshes hold"
            }
            StockGapKind::StringMarker { .. } => "{n} string table entr(ies) given a marker",
            StockGapKind::RenderData { .. } => continue,
        };
        match counts
            .iter_mut()
            .find(|(key, _)| *key == (gap.export, line))
        {
            Some((_, count)) => *count += 1,
            None => counts.push(((gap.export, line), 1)),
        }
    }
    counts
}

/// An `FGameplayTagContainer` holding `tags`.
fn tag_container(tags: &[String], names: &mut FPackageNameMap) -> Vec<u8> {
    let mut out = (tags.len() as i32).to_le_bytes().to_vec();
    for tag in tags {
        let name = names.store(tag);
        out.extend_from_slice(&name.index.to_le_bytes());
        out.extend_from_slice(&name.number.to_le_bytes());
    }
    out
}

/// The tags the material tag plugin lists for each slot, by slot name in lower case.
fn material_slot_tags(parsed: &ParsedPackage) -> HashMap<String, Vec<String>> {
    let mut out = HashMap::new();
    for export in parsed
        .exports
        .iter()
        .filter(|export| export.class_name.contains("MaterialTagAssetUserData"))
    {
        let Some(slots) = field(&export.properties, "MaterialSlotTags") else {
            continue;
        };
        for slot in items(&slots.value) {
            let PropertyValue::Struct { fields, .. } = slot else {
                continue;
            };
            let Some(name) = field(fields, "MaterialSlotName").map(|f| f.value.summary()) else {
                continue;
            };
            let tags: Vec<String> = field(fields, "GameplayTags")
                .map(|tags| tag_names(&tags.value))
                .unwrap_or_default();
            if !tags.is_empty() {
                out.insert(name.to_ascii_lowercase(), tags);
            }
        }
    }
    out
}

/// The tag names a container holds, whether it reads as the native list or as a tagged struct of
/// `GameplayTag`s.
fn tag_names(value: &PropertyValue) -> Vec<String> {
    match value {
        PropertyValue::Array { items } => items.iter().flat_map(tag_names).collect(),
        PropertyValue::Name { value } => {
            if value == "None" {
                Vec::new()
            } else {
                vec![value.clone()]
            }
        }
        PropertyValue::Struct { fields, .. } => fields
            .iter()
            .filter(|f| matches!(f.name.as_str(), "GameplayTags" | "TagName" | "Tag"))
            .flat_map(|f| tag_names(&f.value))
            .collect(),
        _ => Vec::new(),
    }
}

/// A `DefaultHiddenMaterials` tag for each LOD of each skeletal mesh that lacks one, from the
/// hidden-material data a carrier lists, with every tag size around it moved to cover it. Nothing
/// when no carrier lists any.
fn hidden_material_splices(
    bundle: &AssetBundle<'_>,
    parsed: &ParsedPackage,
    names: &mut FPackageNameMap,
    changes: &mut Vec<ConformChange>,
) -> Result<Vec<Splice>, String> {
    let lists = hidden_material_lists(parsed);
    if lists.is_empty() {
        return Ok(Vec::new());
    }
    let mut inserts = Vec::new();
    for mesh in parsed
        .exports
        .iter()
        .filter(|export| export.class_name == "SkeletalMesh")
    {
        let Some(lods) = field(&mesh.properties, "LODInfo") else {
            continue;
        };
        let mut done = 0;
        for (lod, element) in items(&lods.value).iter().enumerate() {
            let PropertyValue::Struct { fields, .. } = element else {
                continue;
            };
            if field(fields, "DefaultHiddenMaterials").is_some() {
                continue;
            }
            let Some(none_at) = block_end(parsed, fields) else {
                return Err(format!(
                    "LOD {lod} of {} has no tag to place its hidden materials beside",
                    mesh.object_name
                ));
            };
            let flags = &lists[lod.min(lists.len() - 1)];
            let mut value = (flags.len() as i32).to_le_bytes().to_vec();
            value.extend(flags.iter().map(|hidden| u8::from(*hidden)));
            let inner = usmap::PropertyInner::Array {
                inner: Box::new(usmap::PropertyInner::Bool),
            };
            let (bytes, _) =
                crate::edit::tag_bytes("DefaultHiddenMaterials", 0, &inner, &value, false, names)?;
            inserts.push(Splice {
                start: none_at,
                end: none_at,
                bytes,
            });
            done += 1;
        }
        if done > 0 {
            changes.push(ConformChange {
                export: Some(mesh.index),
                change: format!("{done} LOD(s) given their hidden-by-default material slots"),
            });
        }
    }
    let mut sizes = Vec::new();
    for layout in &parsed.instanced {
        let grown: i64 = inserts
            .iter()
            .filter(|insert| {
                layout.payload_start <= insert.start && insert.start < layout.payload_end
            })
            .map(|insert| insert.bytes.len() as i64)
            .sum();
        if grown != 0 {
            sizes.push((layout.size_at, grown));
        }
    }
    let mut splices = inserts;
    for (size_at, grown) in sizes {
        let was = word_at(bundle, size_at)?;
        let now = i32::try_from(i64::from(was) + grown).map_err(|_| "a tag grew too large")?;
        splices.push(Splice {
            start: size_at,
            end: size_at + 4,
            bytes: now.to_le_bytes().to_vec(),
        });
    }
    Ok(splices)
}

/// The word at file offset `at`, counted across the `.uasset` and the `.uexp` as spans are.
fn word_at(bundle: &AssetBundle<'_>, at: u64) -> Result<i32, String> {
    let at = usize::try_from(at).map_err(|_| "an offset out of range")?;
    let (bytes, at) = match at.checked_sub(bundle.asset.len()) {
        Some(inside) => (bundle.exports, inside),
        None => (bundle.asset, at),
    };
    bytes
        .get(at..at + 4)
        .and_then(|word| word.try_into().ok())
        .map(i32::from_le_bytes)
        .ok_or_else(|| format!("no word at {at:#X}"))
}

/// Where the tagged block holding `fields` ends: its terminating `None`, found through any tag in
/// it.
fn block_end(parsed: &ParsedPackage, fields: &[PropertyEntry]) -> Option<u64> {
    fields.iter().find_map(|entry| {
        let (start, _) = entry.span?;
        parsed
            .tag_bounds
            .iter()
            .find(|bounds| bounds.key == start)
            .map(|bounds| bounds.none_at)
    })
}

/// The hidden-by-default flags a carrier lists, one list per LOD.
fn hidden_material_lists(parsed: &ParsedPackage) -> Vec<Vec<bool>> {
    for export in parsed.exports.iter().filter(|export| {
        export.class_name.contains("MaterialTagAssetUserData")
            || export.class_name.contains("HiddenMaterials")
    }) {
        let Some(lods) = field(&export.properties, "LODHiddenMaterials") else {
            continue;
        };
        let lists: Vec<Vec<bool>> = items(&lods.value)
            .iter()
            .filter_map(|lod| match lod {
                PropertyValue::Struct { fields, .. } => field(fields, "HiddenMaterials"),
                _ => None,
            })
            .map(|list| {
                items(&list.value)
                    .iter()
                    .map(|flag| match flag {
                        PropertyValue::Bool { value } => *value,
                        PropertyValue::Byte { value } => *value != 0,
                        PropertyValue::Int { value } => *value != 0,
                        _ => false,
                    })
                    .collect()
            })
            .collect();
        if !lists.is_empty() {
            return lists;
        }
    }
    Vec::new()
}

fn field<'a>(fields: &'a [PropertyEntry], name: &str) -> Option<&'a PropertyEntry> {
    fields
        .iter()
        .find(|entry| entry.name.eq_ignore_ascii_case(name))
}

fn items(value: &PropertyValue) -> &[PropertyValue] {
    match value {
        PropertyValue::Array { items } => items,
        _ => &[],
    }
}

/// What an import under a material tag plugin's script package is, which says what it becomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CarrierRole {
    Package,
    Class,
    Default,
}

impl CarrierRole {
    fn engine_path(self) -> &'static str {
        match self {
            Self::Package => "/Script/Engine",
            Self::Class => "/Script/Engine.AssetUserData",
            Self::Default => "/Script/Engine.Default__AssetUserData",
        }
    }
}

/// The role of each import under a carrier plugin's package, by import position.
fn carrier_imports(header: &FLegacyPackageHeader) -> Vec<Option<CarrierRole>> {
    let name = |import: &FObjectImport| {
        header
            .name_map
            .get(import.object_name)
            .map(|name| name.into_owned())
            .unwrap_or_default()
    };
    let is_plugin = |text: &str| {
        CARRIER_PLUGINS
            .iter()
            .any(|plugin| text.eq_ignore_ascii_case(plugin))
    };
    header
        .imports
        .iter()
        .map(|import| {
            let own = name(import);
            if is_plugin(&own) {
                return Some(CarrierRole::Package);
            }
            let outer = usize::try_from(-import.outer_index.index - 1)
                .ok()
                .and_then(|at| header.imports.get(at))?;
            if !is_plugin(&name(outer)) {
                return None;
            }
            Some(if own.starts_with("Default__") {
                CarrierRole::Default
            } else {
                CarrierRole::Class
            })
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use retoc::legacy_asset::{
        EPackageFlags, FLegacyPackageFileSummary, FMinimalName, FObjectExport,
    };

    use super::*;
    use crate::package::FALLBACK_ENGINE_VERSION;

    const NAMES: &[&str] = &[
        "None",
        "/Script/CoreUObject",
        "Package",
        "Class",
        "/Script/Engine",
        "SkeletalMesh",
        "/Script/MaterialTagPlugin",
        "MaterialTagAssetUserData",
        "Default__MaterialTagAssetUserData",
        "/Game/Mat",
        "MaterialInstanceConstant",
        "M_Body",
        "SK_Test",
        "MaterialTagAssetUserData_0",
        "LODInfo",
        "ArrayProperty",
        "StructProperty",
        "SkeletalMeshLODInfo",
        "LODHysteresis",
        "FloatProperty",
        "Body",
        "Hair",
        "root",
        "MaterialSlotTags",
        "MaterialSlotTag",
        "MaterialSlotName",
        "NameProperty",
        "GameplayTags",
        "GameplayTagContainer",
        "MaterialTag.Glasses",
        "LODHiddenMaterials",
        "HiddenMaterials",
        "ByteProperty",
    ];

    fn index_of(value: &str) -> i32 {
        NAMES.iter().position(|n| *n == value).expect(value) as i32
    }

    fn name(out: &mut Vec<u8>, value: &str) {
        out.extend_from_slice(&index_of(value).to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
    }

    fn word(out: &mut Vec<u8>, value: i32) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    /// A tag's head: name, type, size and array index.
    fn head(out: &mut Vec<u8>, property: &str, kind: &str, size: usize) {
        name(out, property);
        name(out, kind);
        word(out, size as i32);
        word(out, 0);
    }

    /// An array of structs: its tag, the count, the inner tag naming the struct, the elements.
    fn struct_array(out: &mut Vec<u8>, property: &str, structure: &str, elements: &[Vec<u8>]) {
        let body: Vec<u8> = elements.concat();
        let mut value = Vec::new();
        word(&mut value, elements.len() as i32);
        head(&mut value, property, "StructProperty", body.len());
        name(&mut value, structure);
        value.extend_from_slice(&[0; 16]);
        value.push(0);
        value.extend_from_slice(&body);
        head(out, property, "ArrayProperty", value.len());
        name(out, "StructProperty");
        out.push(0);
        out.extend_from_slice(&value);
    }

    fn lod(hysteresis: f32) -> Vec<u8> {
        let mut out = Vec::new();
        head(&mut out, "LODHysteresis", "FloatProperty", 4);
        out.push(0);
        out.extend_from_slice(&hysteresis.to_le_bytes());
        name(&mut out, "None");
        out
    }

    /// A stock skeletal mesh: two LODs, then the native head with two slots and no tags.
    fn mesh() -> Vec<u8> {
        let mut out = Vec::new();
        struct_array(
            &mut out,
            "LODInfo",
            "SkeletalMeshLODInfo",
            &[lod(0.1), lod(0.2)],
        );
        name(&mut out, "None");
        word(&mut out, 0);
        out.extend_from_slice(&[1, 0]);
        for value in [0.0f64, 0.0, 50.0, 10.0, 10.0, 50.0, 52.0] {
            out.extend_from_slice(&value.to_le_bytes());
        }
        word(&mut out, 2);
        for slot in ["Body", "Hair"] {
            word(&mut out, -7);
            name(&mut out, slot);
            word(&mut out, 0);
            word(&mut out, 1);
            word(&mut out, 0);
            out.extend_from_slice(&[0; 16]);
        }
        word(&mut out, 1);
        name(&mut out, "root");
        word(&mut out, -1);
        word(&mut out, 1);
        out.extend_from_slice(&[0; 80]);
        word(&mut out, 1);
        name(&mut out, "root");
        word(&mut out, 0);
        out.extend_from_slice(&[0xEE; 8]);
        out
    }

    /// The plugin's data: the body slot tagged as glasses, and each LOD's hidden slots.
    fn carrier() -> Vec<u8> {
        let mut slot = Vec::new();
        head(&mut slot, "MaterialSlotName", "NameProperty", 8);
        slot.push(0);
        name(&mut slot, "Body");
        let mut tags = Vec::new();
        word(&mut tags, 1);
        name(&mut tags, "MaterialTag.Glasses");
        head(&mut slot, "GameplayTags", "StructProperty", tags.len());
        name(&mut slot, "GameplayTagContainer");
        slot.extend_from_slice(&[0; 16]);
        slot.push(0);
        slot.extend_from_slice(&tags);
        name(&mut slot, "None");
        let hidden = |flags: [u8; 2]| {
            let mut out = Vec::new();
            head(&mut out, "HiddenMaterials", "ArrayProperty", 4 + 2);
            name(&mut out, "ByteProperty");
            out.push(0);
            word(&mut out, 2);
            out.extend_from_slice(&flags);
            name(&mut out, "None");
            out
        };
        let mut out = Vec::new();
        struct_array(&mut out, "MaterialSlotTags", "MaterialSlotTag", &[slot]);
        struct_array(
            &mut out,
            "LODHiddenMaterials",
            "LODHiddenMaterials",
            &[hidden([1, 0]), hidden([0, 1])],
        );
        name(&mut out, "None");
        word(&mut out, 0);
        out
    }

    fn package() -> (Vec<u8>, Vec<u8>) {
        let minimal = |value: &str| FMinimalName {
            index: index_of(value),
            number: 0,
        };
        let import =
            |class_package: &str, class_name: &str, outer: i32, object: &str| FObjectImport {
                class_package: minimal(class_package),
                class_name: minimal(class_name),
                outer_index: FPackageIndex { index: outer },
                object_name: minimal(object),
                is_optional: false,
            };
        let (mesh, carrier) = (mesh(), carrier());
        let mut summary = FLegacyPackageFileSummary {
            package_name: "/Game/SK_Test".to_string(),
            ..Default::default()
        };
        summary.versioning_info.package_file_version =
            FALLBACK_ENGINE_VERSION.package_file_version();
        summary.versioning_info.total_header_size = 1024;
        summary.package_flags =
            EPackageFlags::Cooked as u32 | EPackageFlags::FilterEditorOnly as u32;
        let header = FLegacyPackageHeader {
            summary,
            name_map: FPackageNameMap::create_from_names(
                NAMES.iter().map(|n| (*n).to_string()).collect(),
            ),
            imports: vec![
                import("/Script/CoreUObject", "Package", 0, "/Script/Engine"),
                import("/Script/CoreUObject", "Class", -1, "SkeletalMesh"),
                import(
                    "/Script/CoreUObject",
                    "Package",
                    0,
                    "/Script/MaterialTagPlugin",
                ),
                import(
                    "/Script/CoreUObject",
                    "Class",
                    -3,
                    "MaterialTagAssetUserData",
                ),
                import(
                    "/Script/MaterialTagPlugin",
                    "MaterialTagAssetUserData",
                    -3,
                    "Default__MaterialTagAssetUserData",
                ),
                import("/Script/CoreUObject", "Package", 0, "/Game/Mat"),
                import("/Script/Engine", "MaterialInstanceConstant", -6, "M_Body"),
            ],
            exports: vec![
                FObjectExport {
                    object_name: minimal("SK_Test"),
                    class_index: FPackageIndex::create_import(1),
                    serial_offset: 0,
                    serial_size: mesh.len() as i64,
                    first_export_dependency_index: -1,
                    ..Default::default()
                },
                FObjectExport {
                    object_name: minimal("MaterialTagAssetUserData_0"),
                    class_index: FPackageIndex::create_import(3),
                    template_index: FPackageIndex::create_import(4),
                    outer_index: FPackageIndex::create_export(0),
                    serial_offset: mesh.len() as i64,
                    serial_size: carrier.len() as i64,
                    first_export_dependency_index: -1,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let mut asset = std::io::Cursor::new(Vec::new());
        header
            .serialize(&mut asset, Some(1024), &retoc::logging::Log::no_log())
            .expect("serialize the test header");
        (asset.into_inner(), [mesh, carrier].concat())
    }

    fn named<'a>(fields: &'a [PropertyEntry], name: &str) -> &'a PropertyEntry {
        field(fields, name).unwrap_or_else(|| panic!("no {name}"))
    }

    fn element(value: &PropertyValue, at: usize) -> &[PropertyEntry] {
        match &items(value)[at] {
            PropertyValue::Struct { fields, .. } => fields,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_stock_mesh_gains_its_tags_its_hidden_slots_and_the_engine_class() {
        let (asset, exports) = package();
        let bundle = AssetBundle {
            asset: &asset,
            exports: &exports,
        };
        let before = parse_package(&bundle, None).expect("parse");
        assert_eq!(before.stock_gaps.len(), 2, "{:?}", before.stock_gaps);

        let conformed = conform_package(&bundle, None)
            .expect("convert")
            .expect("something to change");
        let after_bundle = AssetBundle {
            asset: &conformed.asset,
            exports: &conformed.exports,
        };
        let after = parse_package(&after_bundle, None).expect("re-read");
        assert!(after.stock_gaps.is_empty());

        let mesh = &after.exports[0];
        let materials = &named(&mesh.properties, "Materials").value;
        let tags = |slot: usize| tag_names(&named(element(materials, slot), "MaterialTags").value);
        assert_eq!(tags(0), ["MaterialTag.Glasses"]);
        assert!(tags(1).is_empty());

        let lods = &named(&mesh.properties, "LODInfo").value;
        let hidden = |lod: usize| -> Vec<String> {
            items(&named(element(lods, lod), "DefaultHiddenMaterials").value)
                .iter()
                .map(PropertyValue::summary)
                .collect()
        };
        assert_eq!(hidden(0), ["true", "false"]);
        assert_eq!(hidden(1), ["false", "true"]);
        let hysteresis = named(element(lods, 1), "LODHysteresis").value.summary();
        assert!(hysteresis.starts_with("0.2"), "{hysteresis}");

        let paths: Vec<&str> = after.imports.iter().map(|i| i.path.as_str()).collect();
        assert!(paths.contains(&"/Script/Engine.AssetUserData"), "{paths:?}");
        assert!(
            paths.contains(&"/Script/Engine.Default__AssetUserData"),
            "{paths:?}"
        );
        assert!(
            !paths.iter().any(|p| p.contains("MaterialTagPlugin")),
            "{paths:?}"
        );

        assert!(
            conform_package(&after_bundle, None)
                .expect("convert again")
                .is_none(),
            "a converted package needs nothing more"
        );
    }

    /// Tags for a slot the mesh does not have are a mistake in the mod, not something to drop.
    #[test]
    fn tags_for_a_slot_the_mesh_lacks_are_refused() {
        let (asset, mut exports) = package();
        let mesh_len = mesh().len();
        let body = index_of("Body").to_le_bytes();
        let at = exports[mesh_len..]
            .windows(4)
            .position(|bytes| bytes == body)
            .expect("the carrier names the body slot")
            + mesh_len;
        exports[at..at + 4].copy_from_slice(&index_of("root").to_le_bytes());
        let bundle = AssetBundle {
            asset: &asset,
            exports: &exports,
        };
        let err = conform_package(&bundle, None).expect_err("refused");
        assert!(err.contains("root"), "{err}");
    }

    /// A tagged package stamped with a custom version, as an editor saves it without
    /// `-unversioned`, `ue5_offset` past the game's own package version.
    fn stamped(ue5_offset: i32) -> (Vec<u8>, Vec<u8>) {
        let (asset, exports) = crate::tagged_fixture::tagged_package();
        let bundle = AssetBundle {
            asset: &asset,
            exports: &exports,
        };
        let mut header = read_header(&bundle).expect("header");
        let total = header.summary.versioning_info.total_header_size as usize;
        let versions = &mut header.summary.versioning_info;
        versions.is_unversioned = false;
        versions.package_file_version.file_version_ue5 += ue5_offset;
        versions.custom_versions = vec![retoc::zen::FCustomVersion {
            key: Default::default(),
            version: 7,
        }];
        crate::write::to_relative(&mut header, total);
        let asset = crate::write::serialize_header(&header, 0).expect("write");
        (asset, exports)
    }

    fn values(parsed: &ParsedPackage) -> Vec<(String, String)> {
        parsed.exports[0]
            .properties
            .iter()
            .map(|p| (p.name.clone(), p.value.summary()))
            .collect()
    }

    #[test]
    fn a_stamp_comes_out_of_the_header_and_the_export_data_stays() {
        let (asset, exports) = stamped(0);
        let bundle = AssetBundle {
            asset: &asset,
            exports: &exports,
        };
        let conformed = conform_package(&bundle, None)
            .expect("convert")
            .expect("a stamp to take out");
        assert_eq!(conformed.changes.len(), 1, "{:?}", conformed.changes);
        assert_eq!(conformed.exports, exports);
        let after = AssetBundle {
            asset: &conformed.asset,
            exports: &conformed.exports,
        };
        let header = read_header(&after).expect("re-read");
        assert!(header.summary.versioning_info.is_unversioned);
        assert!(header.summary.versioning_info.custom_versions.is_empty());
        assert!(conformed.asset.len() < asset.len());
        assert_eq!(
            values(&parse_package(&after, None).expect("parse unstamped")),
            values(&parse_package(&bundle, None).expect("parse stamped"))
        );
        assert!(
            conform_package(&after, None)
                .expect("convert again")
                .is_none(),
            "an unstamped package needs nothing more"
        );
    }

    /// Unstamped, a package is read under the game's versions, which a newer engine's cook breaks.
    #[test]
    fn a_stamp_from_another_engine_is_refused() {
        let (asset, exports) = stamped(1);
        let bundle = AssetBundle {
            asset: &asset,
            exports: &exports,
        };
        let err = conform_package(&bundle, None).expect_err("refused");
        assert!(err.contains("cook it in UE 5.3"), "{err}");
    }
}
