//! Applies the same value changes to every package in a container that matches a filter.
//!
//! Editing hundreds of assets one save at a time rewrites the mod container once per asset. A
//! sweep patches and verifies each package the same way a single save does, then puts the whole
//! batch in with one container rewrite.

use std::collections::BTreeMap;
use std::path::PathBuf;

use retoc::legacy_asset::FSerializedAssetBundle;
use rivals_uasset::{
    EditOp, Mappings, PackageEdits, PatchedBundle, PropertyEntry, PropertyValue, ValueEdit, kind_of,
};

use super::{AssetEditRequest, SaveOptions, SaveTarget, preview_edits};
use crate::asset::{self, AssetSource};

/// Holding every patched package in memory is what lets the batch go in with one rewrite, so a
/// filter that matches half the game has to be refused rather than swallowed.
const STAGED_BUDGET: u64 = 2 << 30;

/// One property name and the value every match takes.
#[derive(Debug, Clone)]
pub struct SweepSet {
    /// Matched against a property's name at any depth, case-insensitively.
    pub name: String,
    /// Written in the value's own kind, as [`EditOp::Set`] reads it.
    pub text: String,
}

pub struct SweepRequest<'a> {
    pub game_root: &'a str,
    pub container: &'a str,
    /// Substring of the package path, as `asset list --filter` matches it.
    pub filter: Option<&'a str>,
    pub mod_name: &'a str,
    pub sets: Vec<SweepSet>,
    /// Stop after this many matching packages, for trying a sweep out on a handful first.
    pub limit: Option<usize>,
    /// Patch and verify everything, write nothing.
    pub dry_run: bool,
}

/// One package the sweep changed, and what it changed in it.
#[derive(Debug, serde::Serialize)]
pub struct SweptPackage {
    pub entry: String,
    /// `name = before -> after`, one per value written.
    pub changes: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct SweepFailure {
    pub entry: String,
    pub reason: String,
}

#[derive(Debug, serde::Serialize)]
pub struct SweepReport {
    /// Packages the filter matched, before any of them were read.
    pub matched: usize,
    pub changed: Vec<SweptPackage>,
    /// Matched packages holding no stored value under any of the swept names.
    pub untouched: usize,
    pub failed: Vec<SweepFailure>,
    /// What was matched by name and left alone anyway. A sweep that changes nothing is usually a
    /// misspelt name, and this is where that shows.
    pub skipped: Skipped,
    pub written: Option<PathBuf>,
    pub carried_chunks: usize,
}

impl SweepReport {
    pub fn edits(&self) -> usize {
        self.changed.iter().map(|held| held.changes.len()).sum()
    }
}

/// Kinds [`EditOp::Set`] writes over in place. A container or a struct has no single value to
/// give, and a reference written by name would need a target the sweep cannot pick.
fn settable(kind: &str) -> bool {
    matches!(
        kind,
        "bool" | "int" | "uint" | "float" | "byte" | "str" | "name" | "enum"
    )
}

/// Every stored value under one of the swept names, wherever it sits in the export's properties.
fn matches_in(entries: &[PropertyEntry], sets: &[SweepSet], found: &mut Vec<Match>) {
    for entry in entries {
        if let Some(set) = sets
            .iter()
            .find(|set| set.name.eq_ignore_ascii_case(&entry.name))
        {
            let kind = kind_of(&entry.value);
            // A defaulted or unset slot occupies no bytes, and writing one would turn a value the
            // object inherits into one it states. A sweep changes what an asset says, never what
            // it leaves to its archetype.
            match entry.span.filter(|span| span.1 > span.0) {
                Some(span) if settable(&kind) => found.push(Match::Set {
                    offset: span.0,
                    name: entry.name.clone(),
                    element: entry.element,
                    kind,
                    before: entry.value.summary(),
                    text: set.text.clone(),
                }),
                Some(_) => found.push(Match::Unsupported {
                    name: entry.name.clone(),
                    kind,
                }),
                None => found.push(Match::Inherited {
                    name: entry.name.clone(),
                }),
            }
        }
        walk_value(&entry.value, sets, found);
    }
}

fn walk_value(value: &PropertyValue, sets: &[SweepSet], found: &mut Vec<Match>) {
    match value {
        PropertyValue::Struct { fields, .. } => matches_in(fields, sets, found),
        PropertyValue::Text { parts, .. } => matches_in(parts, sets, found),
        PropertyValue::Array { items } | PropertyValue::Set { items } => {
            for item in items {
                walk_value(item, sets, found);
            }
        }
        PropertyValue::Map { entries } => {
            for pair in entries {
                walk_value(&pair.key, sets, found);
                walk_value(&pair.value, sets, found);
            }
        }
        _ => {}
    }
}

enum Match {
    Set {
        offset: u64,
        name: String,
        element: Option<u32>,
        kind: String,
        before: String,
        text: String,
    },
    /// Matched by name, but of a kind the sweep will not write. Carried through rather than
    /// dropped so the report can say why a filter that looked right changed nothing.
    Unsupported { name: String, kind: String },
    /// Matched by name, but the package stores nothing for it, so the value comes from the
    /// archetype. Counted for the same reason: a silent skip reads as a broken filter.
    Inherited { name: String },
}

/// Whether a value already reads as what the sweep would write. Numbers are compared as numbers,
/// since `0` and `0.0` are the same value written two ways.
fn already_set(before: &str, text: &str) -> bool {
    match (before.parse::<f64>(), text.parse::<f64>()) {
        (Ok(before), Ok(text)) => before == text,
        _ => before == text,
    }
}

/// What the sweep matched by name but did not write, counted so a filter that changes nothing
/// says why.
#[derive(Debug, Default, serde::Serialize)]
pub struct Skipped {
    /// Keyed `name: kind`, for matches of a kind [`EditOp::Set`] does not write.
    pub unsupported: BTreeMap<String, usize>,
    /// Keyed by name, for slots the package leaves to its archetype.
    pub inherited: BTreeMap<String, usize>,
}

impl Skipped {
    pub fn is_empty(&self) -> bool {
        self.unsupported.is_empty() && self.inherited.is_empty()
    }
}

/// The edits one package takes, and the matches it could not write.
fn edits_for<'a>(
    exports: impl Iterator<Item = &'a [PropertyEntry]>,
    sets: &[SweepSet],
    skipped: &mut Skipped,
) -> Vec<ValueEdit> {
    let mut found = Vec::new();
    for properties in exports {
        matches_in(properties, sets, &mut found);
    }
    let mut edits: Vec<ValueEdit> = Vec::new();
    for held in found {
        let Match::Set {
            offset,
            name,
            element,
            kind,
            before,
            text,
        } = held
        else {
            match held {
                Match::Unsupported { name, kind } => {
                    *skipped
                        .unsupported
                        .entry(format!("{name}: {kind}"))
                        .or_default() += 1;
                }
                Match::Inherited { name } => *skipped.inherited.entry(name).or_default() += 1,
                Match::Set { .. } => {}
            }
            continue;
        };
        // Writing a value it already holds would make the mod carry the asset for nothing.
        if already_set(&before, &text) {
            continue;
        }
        edits.push(ValueEdit {
            offset,
            expect_name: name,
            expect_element: element,
            expect_kind: kind,
            op: EditOp::Set { text },
        });
    }
    edits.sort_by_key(|edit| edit.offset);
    edits.dedup_by_key(|edit| edit.offset);
    edits
}

/// A package patched and verified, held until the whole batch goes in together.
struct Staged {
    entry: String,
    patched: PatchedBundle,
    loaded: FSerializedAssetBundle,
    changes: Vec<String>,
}

impl Staged {
    fn bytes(&self) -> u64 {
        let sidecar = |held: &Option<Vec<u8>>| held.as_ref().map_or(0, Vec::len) as u64;
        (self.patched.asset.len() + self.patched.exports.len()) as u64
            + sidecar(&self.loaded.bulk_data_buffer)
            + sidecar(&self.loaded.optional_bulk_data_buffer)
            + sidecar(&self.loaded.memory_mapped_bulk_data_buffer)
    }
}

/// Patches every matching package and writes them into the mod with a single container rewrite.
pub fn sweep(
    request: &SweepRequest<'_>,
    mappings: Option<&Mappings>,
    options: &SaveOptions,
) -> Result<SweepReport, String> {
    if request.sets.is_empty() {
        return Err("No values to set".into());
    }
    if options.target != SaveTarget::IoStore {
        return Err(
            "A sweep writes an IoStore mod, since the game reads packages only from a container"
                .into(),
        );
    }
    let entries = matching(request)?;
    let mut report = SweepReport {
        matched: entries.len(),
        changed: Vec::new(),
        untouched: 0,
        failed: Vec::new(),
        skipped: Skipped::default(),
        written: None,
        carried_chunks: 0,
    };
    let mut staged: Vec<Staged> = Vec::new();
    let mut bytes = 0u64;
    for entry in entries {
        match stage(request, &entry, mappings, &mut report.skipped) {
            Ok(Some(one)) => {
                bytes += one.bytes();
                if bytes > STAGED_BUDGET {
                    return Err(format!(
                        "The sweep matched more than {} GiB of packages, which is more than it \
                         will hold in memory at once. Narrow the filter or use --limit.",
                        STAGED_BUDGET >> 30
                    ));
                }
                staged.push(one);
            }
            Ok(None) => report.untouched += 1,
            Err(reason) => report.failed.push(SweepFailure { entry, reason }),
        }
    }
    for one in &staged {
        report.changed.push(SweptPackage {
            entry: one.entry.clone(),
            changes: one.changes.clone(),
        });
    }
    if request.dry_run || staged.is_empty() {
        return Ok(report);
    }

    let utoc = super::mod_pak_path(request.game_root, request.mod_name)?.with_extension("utoc");
    if utoc.with_extension("pak").is_file() && !utoc.is_file() {
        return Err(format!(
            "{} is a plain pak mod. Repack it in place as IoStore first, or choose another name.",
            utoc.with_extension("pak").display()
        ));
    }
    let files: Vec<crate::pak::iostore_out::PackageFiles<'_>> = staged
        .iter()
        .map(|one| crate::pak::iostore_out::PackageFiles {
            entry: &one.entry,
            asset: &one.patched.asset,
            exports: &one.patched.exports,
            bulk: one
                .patched
                .bulk
                .as_deref()
                .or(one.loaded.bulk_data_buffer.as_deref()),
            optional_bulk: one
                .patched
                .optional_bulk
                .as_deref()
                .or(one.loaded.optional_bulk_data_buffer.as_deref()),
            memory_mapped_bulk: one.loaded.memory_mapped_bulk_data_buffer.as_deref(),
            shader_map_hashes: Vec::new(),
        })
        .collect();
    // A read earlier in this session may still hold the container open, and it is about to be
    // replaced underneath.
    crate::pak::containers::drop_cached_store();
    let written =
        crate::pak::iostore_out::write_many_into_iostore(&utoc, &files, &options.iostore)?;
    report.carried_chunks = written.carried_chunks;
    report.written = Some(written.utoc);
    Ok(report)
}

/// Reads one package and patches it, or `None` when it holds nothing the sweep would change.
fn stage(
    request: &SweepRequest<'_>,
    entry: &str,
    mappings: Option<&Mappings>,
    skipped: &mut Skipped,
) -> Result<Option<Staged>, String> {
    let mut edit = AssetEditRequest {
        game_root: request.game_root,
        container: request.container,
        entry,
        kind: source_of(request.container),
        mod_name: request.mod_name,
        changes: PackageEdits::default(),
    };
    let (_, parsed) = super::read_package(&edit, mappings)?;
    let edits = edits_for(
        parsed.exports.iter().map(|export| &export.properties[..]),
        &request.sets,
        skipped,
    );
    if edits.is_empty() {
        return Ok(None);
    }
    edit.changes = PackageEdits {
        values: edits,
        ..PackageEdits::default()
    };
    let (patched, loaded) = preview_edits(&edit, mappings)?;
    let changes = patched
        .applied
        .iter()
        .map(|applied| format!("{} = {} -> {}", applied.name, applied.before, applied.after))
        .collect();
    Ok(Some(Staged {
        entry: entry.to_string(),
        patched,
        loaded,
        changes,
    }))
}

/// The packages the filter names, in path order so a limited run is repeatable.
fn matching(request: &SweepRequest<'_>) -> Result<Vec<String>, String> {
    let all: Vec<String> = match source_of(request.container) {
        AssetSource::Utoc => asset::list_packages(request.game_root, request.container)?
            .1
            .into_iter()
            .map(|(_, path)| path)
            .collect(),
        AssetSource::Pak | AssetSource::Loose => asset::list_pak_entries(request.container)?,
    };
    let needle = request.filter.map(str::to_lowercase);
    let mut paths: Vec<String> = all
        .into_iter()
        .filter(|path| {
            needle
                .as_ref()
                .is_none_or(|needle| path.to_lowercase().contains(needle.as_str()))
        })
        .collect();
    paths.sort();
    if let Some(limit) = request.limit {
        paths.truncate(limit);
    }
    Ok(paths)
}

fn source_of(container: &str) -> AssetSource {
    match std::path::Path::new(container)
        .extension()
        .and_then(|e| e.to_str())
    {
        Some("utoc") => AssetSource::Utoc,
        Some("pak") => AssetSource::Pak,
        _ => AssetSource::Loose,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use rivals_uasset::MapEntry;

    type Skips = Skipped;

    const BACKSLASH: char = '\\';

    fn entry(name: &str, value: PropertyValue, span: Option<(u64, u64)>) -> PropertyEntry {
        PropertyEntry {
            name: name.into(),
            element: None,
            value,
            span,
            slot: None,
        }
    }

    fn float(value: f64) -> PropertyValue {
        PropertyValue::Float { value }
    }

    fn found(entries: &[PropertyEntry], sets: &[SweepSet]) -> Vec<Match> {
        let mut out = Vec::new();
        matches_in(entries, sets, &mut out);
        out
    }

    fn set(name: &str, text: &str) -> SweepSet {
        SweepSet {
            name: name.into(),
            text: text.into(),
        }
    }

    #[test]
    fn matches_a_name_nested_in_structs() {
        let inner = PropertyValue::Struct {
            name: "FOscillator".into(),
            fields: vec![
                entry("Amplitude", float(5.0), Some((40, 44))),
                entry("Frequency", float(20.0), Some((44, 48))),
            ],
        };
        let outer = PropertyValue::Struct {
            name: "ROscillator".into(),
            fields: vec![entry("Pitch", inner, Some((40, 48)))],
        };
        let hits = found(
            &[entry("RotOscillation", outer, Some((40, 48)))],
            &[set("Amplitude", "0")],
        );
        assert_eq!(hits.len(), 1);
        let Match::Set {
            offset,
            kind,
            before,
            ..
        } = &hits[0]
        else {
            panic!("the nested amplitude is writable");
        };
        assert_eq!(*offset, 40);
        assert_eq!(kind, "float");
        assert_eq!(before, "5.0");
    }

    #[test]
    fn matches_inside_arrays_and_maps() {
        let item = PropertyValue::Struct {
            name: "FShake".into(),
            fields: vec![entry("Amplitude", float(1.0), Some((8, 12)))],
        };
        let mapped = PropertyValue::Struct {
            name: "FShake".into(),
            fields: vec![entry("Amplitude", float(2.0), Some((20, 24)))],
        };
        let entries = vec![
            entry(
                "Shakes",
                PropertyValue::Array { items: vec![item] },
                Some((4, 16)),
            ),
            entry(
                "ByName",
                PropertyValue::Map {
                    entries: vec![MapEntry {
                        key: PropertyValue::Name { value: "a".into() },
                        value: mapped,
                    }],
                },
                Some((16, 24)),
            ),
        ];
        let hits = found(&entries, &[set("Amplitude", "0")]);
        let offsets: Vec<u64> = hits
            .iter()
            .filter_map(|held| match held {
                Match::Set { offset, .. } => Some(*offset),
                Match::Unsupported { .. } | Match::Inherited { .. } => None,
            })
            .collect();
        assert_eq!(offsets, vec![8, 20]);
    }

    #[test]
    fn the_name_match_ignores_case() {
        let hits = found(
            &[entry("AnimScale", float(0.5), Some((0, 4)))],
            &[set("animscale", "0")],
        );
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn a_slot_storing_nothing_is_left_alone() {
        let entries = vec![
            entry("Amplitude", PropertyValue::Default, Some((4, 4))),
            entry(
                "Frequency",
                PropertyValue::Unset {
                    declared: "Float",
                    enum_type: None,
                },
                Some((4, 4)),
            ),
        ];
        let sets = [set("Amplitude", "0"), set("Frequency", "0")];
        let hits = found(&entries, &sets);
        assert!(
            hits.iter()
                .all(|held| matches!(held, Match::Inherited { .. })),
            "matched, and reported as inherited rather than written"
        );
        let (edits, skipped) = edits(&entries, &sets);
        assert!(edits.is_empty());
        assert_eq!(skipped.inherited.get("Amplitude"), Some(&1));
        assert_eq!(skipped.inherited.get("Frequency"), Some(&1));
    }

    #[test]
    fn a_kind_the_sweep_cannot_write_is_reported_not_written() {
        let hits = found(
            &[entry(
                "Amplitude",
                PropertyValue::Array { items: Vec::new() },
                Some((4, 8)),
            )],
            &[set("Amplitude", "0")],
        );
        assert_eq!(hits.len(), 1);
        let Match::Unsupported { kind, .. } = &hits[0] else {
            panic!("an array is not writable by the sweep");
        };
        assert_eq!(kind, "array");
    }

    fn edits(properties: &[PropertyEntry], sets: &[SweepSet]) -> (Vec<ValueEdit>, Skips) {
        let mut skipped = Skipped::default();
        let edits = edits_for(std::iter::once(properties), sets, &mut skipped);
        (edits, skipped)
    }

    #[test]
    fn a_value_already_at_the_target_is_not_written() {
        let (edits, skipped) = edits(
            &[
                entry("Amplitude", float(0.0), Some((4, 8))),
                entry("AnimScale", float(0.5), Some((8, 12))),
            ],
            &[set("Amplitude", "0"), set("AnimScale", "0")],
        );
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].offset, 8);
        assert!(skipped.is_empty());
    }

    #[test]
    fn unsupported_kinds_are_counted_by_name_and_kind() {
        let (edits, skipped) = edits(
            &[entry(
                "Amplitude",
                PropertyValue::Struct {
                    name: "FVector".into(),
                    fields: Vec::new(),
                },
                Some((4, 16)),
            )],
            &[set("Amplitude", "0")],
        );
        assert!(edits.is_empty());
        assert_eq!(skipped.unsupported.get("Amplitude: struct"), Some(&1));
    }

    /// The sweep reads and patches real packages, so a dry run over a handful is the check that
    /// name matching, the offsets it produces and the verifier all agree on game data.
    #[test]
    fn a_dry_run_zeroes_camera_shakes_without_writing() {
        let (Ok(root), Ok(usmap)) = (
            std::env::var("RIVALS_GAME_ROOT"),
            std::env::var("RIVALS_USMAP"),
        ) else {
            return;
        };
        if std::env::var_os("OODLE_LIB_PATH").is_none() {
            return;
        }
        let usmap = crate::mappings::resolve(Some(&usmap), None).expect("find the mappings");
        let mappings = crate::mappings::load(&usmap).expect("load the mappings");
        let container = format!(
            "{}/MarvelGame/Marvel/Content/Paks/pakchunk0-Windows.utoc",
            root.replace(BACKSLASH, "/")
        );
        let report = sweep(
            &SweepRequest {
                game_root: &root,
                container: &container,
                filter: Some("CameraShake"),
                mod_name: "RivalsTestSweep",
                sets: vec![set("Amplitude", "0"), set("AnimScale", "0")],
                limit: Some(12),
                dry_run: true,
            },
            Some(&mappings),
            &super::super::SaveOptions::default(),
        )
        .expect("sweep");
        assert_eq!(report.matched, 12);
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert!(report.written.is_none(), "a dry run writes nothing");
        assert!(report.edits() > 0, "camera shakes store amplitudes");
        for package in &report.changed {
            for change in &package.changes {
                assert!(change.ends_with("-> 0"), "{} in {}", change, package.entry);
            }
        }
    }

    /// The same property reached twice, once as itself and once through its enclosing struct, is
    /// one value and has to be written once.
    #[test]
    fn one_offset_is_edited_once() {
        let shared = entry("Amplitude", float(3.0), Some((12, 16)));
        let (edits, _) = edits(
            &[
                shared.clone(),
                entry(
                    "Wrapper",
                    PropertyValue::Struct {
                        name: "FOscillator".into(),
                        fields: vec![shared],
                    },
                    Some((12, 16)),
                ),
            ],
            &[set("Amplitude", "0")],
        );
        assert_eq!(edits.len(), 1);
    }
}
