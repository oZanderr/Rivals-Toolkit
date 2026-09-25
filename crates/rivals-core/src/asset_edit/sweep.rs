//! Applies the same value changes to every package in a container that matches a filter.
//!
//! Editing hundreds of assets one save at a time rewrites the mod container once per asset. A
//! sweep patches and verifies each package the same way a single save does, then puts the whole
//! batch in with one container rewrite.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use retoc::iostore::IoStoreTrait;
use retoc::legacy_asset::FSerializedAssetBundle;
use rivals_uasset::{
    EditOp, Mappings, PackageEdits, PatchedBundle, PropertyEntry, PropertyValue, ValueEdit, kind_of,
};

use super::{AssetEditRequest, SaveOptions, SaveTarget};
use crate::asset::{self, AssetSource};

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
    /// Keep only packages holding an export of this class. Narrows the path filter rather than
    /// replacing it, and costs a parse of every package the path filter let through.
    pub class: Option<&'a str>,
    pub mod_name: &'a str,
    pub sets: Vec<SweepSet>,
    /// Stop after this many matching packages, for trying a sweep out on a handful first.
    pub limit: Option<usize>,
    /// Read a package from the mod when it already holds one, so repeated sweeps build on each
    /// other instead of each starting from the source container again.
    pub layer: bool,
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
    /// Matched packages dropped by `--class`, which only a class filter produces.
    pub other_class: usize,
    /// Written packages the mod already carried. Without `layer` each one replaced what was there,
    /// which is how an earlier sweep's edits are lost; with it, each was read and built on.
    pub replaced: usize,
    /// Whether those packages were built on rather than replaced.
    pub layered: bool,
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

/// A package patched and verified, on its way straight into the container.
struct Staged {
    entry: String,
    patched: PatchedBundle,
    loaded: FSerializedAssetBundle,
    changes: Vec<String>,
    shader_map_hashes: Vec<retoc::FSHAHash>,
}

impl Staged {
    /// The bytes to write, with the sidecars the patch did not touch carried through from the
    /// package as it was read.
    fn bytes(self) -> crate::pak::iostore_out::PackageBytes {
        let sidecar = |patched: Option<Vec<u8>>, read: Option<Vec<u8>>| patched.or(read);
        crate::pak::iostore_out::PackageBytes {
            entry: self.entry,
            asset: self.patched.asset,
            exports: self.patched.exports,
            bulk: sidecar(self.patched.bulk, self.loaded.bulk_data_buffer),
            optional_bulk: sidecar(
                self.patched.optional_bulk,
                self.loaded.optional_bulk_data_buffer,
            ),
            memory_mapped_bulk: self.loaded.memory_mapped_bulk_data_buffer,
            shader_map_hashes: self.shader_map_hashes,
        }
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
    let utoc = super::mod_pak_path(request.game_root, request.mod_name)?.with_extension("utoc");
    if !request.dry_run && utoc.with_extension("pak").is_file() && !utoc.is_file() {
        return Err(format!(
            "{} is a plain pak mod. Repack it in place as IoStore first, or choose another name.",
            utoc.with_extension("pak").display()
        ));
    }
    // Sweeping a mod as its own source is layering by another name: what it reads is what it is
    // about to replace, so nothing is lost and the warning would be wrong.
    let layered = request.layer || same_file(&utoc, request.container);
    let (store, entries) = matching(request, &utoc)?;
    let mut report = SweepReport {
        matched: entries.len(),
        changed: Vec::new(),
        untouched: 0,
        other_class: 0,
        replaced: 0,
        layered,
        failed: Vec::new(),
        skipped: Skipped::default(),
        written: None,
        carried_chunks: 0,
    };

    // The converter holds the store's conversion caches, which is what makes reading hundreds of
    // packages cost about what one of them used to.
    let converter = store
        .as_ref()
        .map(|store| asset::PackageConverter::new(&**store));
    // One package at a time: patched, written, and let go before the next is read. Nothing here
    // grows with how many the filter matched.
    let mut prepare = |index: usize| -> Option<crate::pak::iostore_out::PackageBytes> {
        let (id, entry) = &entries[index];
        let resolved = converter.as_ref().zip(*id);
        match stage(request, resolved, entry, mappings, &mut report.skipped) {
            Ok(Prepared::Ready(mut one)) => {
                one.shader_map_hashes = store
                    .as_ref()
                    .zip(*id)
                    .and_then(|(store, id)| store.package_store_entry(id))
                    .map(|held| held.shader_map_hashes)
                    .unwrap_or_default();
                report.changed.push(SweptPackage {
                    entry: one.entry.clone(),
                    changes: one.changes.clone(),
                });
                Some(one.bytes())
            }
            Ok(Prepared::NothingToChange) => {
                report.untouched += 1;
                None
            }
            Ok(Prepared::OtherClass) => {
                report.other_class += 1;
                None
            }
            Err(reason) => {
                report.failed.push(SweepFailure {
                    entry: entry.clone(),
                    reason,
                });
                None
            }
        }
    };

    if request.dry_run {
        for index in 0..entries.len() {
            drop(prepare(index));
        }
        report.replaced = already_held(&utoc, &report.changed);
        return Ok(report);
    }
    // A read earlier in this session may still hold the container open, and it is about to be
    // replaced underneath.
    crate::pak::containers::drop_cached_store();
    let staged = crate::pak::iostore_out::stage_batch_into_iostore(
        &utoc,
        entries.len(),
        prepare,
        &options.iostore,
    )?;
    // Nothing to write is not a failure to report as one: the report already says why.
    let Some(staged) = staged else {
        return Ok(report);
    };
    report.replaced = staged.report().replaced;
    report.carried_chunks = staged.report().carried_chunks;
    // The swap renames the mod, so everything read along the way has to be closed first. Under
    // `layer` the mod is one of the containers that was being read.
    drop(converter);
    drop(store);
    report.written = Some(staged.commit()?.utoc);
    Ok(report)
}

/// Whether two paths name the same container, compared the way the file system would.
fn same_file(utoc: &std::path::Path, container: &str) -> bool {
    let canonical =
        |path: &std::path::Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    utoc.is_file() && canonical(utoc) == canonical(std::path::Path::new(container))
}

/// How many of these the mod already carries, for a dry run to say what a real one would replace.
fn already_held(utoc: &std::path::Path, changed: &[SweptPackage]) -> usize {
    if !utoc.is_file() {
        return 0;
    }
    let Ok(held) = crate::pak::iostore_out::utoc_entries(utoc) else {
        return 0;
    };
    changed
        .iter()
        .filter(|package| held.contains(&package.entry.replace('\\', "/").to_ascii_lowercase()))
        .count()
}

/// What reading one package came to.
enum Prepared {
    Ready(Box<Staged>),
    /// Nothing under any of the swept names is both stored and different.
    NothingToChange,
    /// A class filter is set and this package holds no export of that class.
    OtherClass,
}

/// Reads one package and patches it, when it holds anything the sweep would change.
fn stage(
    request: &SweepRequest<'_>,
    resolved: Option<(&asset::PackageConverter<'_>, retoc::FPackageId)>,
    entry: &str,
    mappings: Option<&Mappings>,
    skipped: &mut Skipped,
) -> Result<Prepared, String> {
    let mut edit = AssetEditRequest {
        game_root: request.game_root,
        container: request.container,
        entry,
        kind: source_of(request.container),
        mod_name: request.mod_name,
        changes: PackageEdits::default(),
    };
    let loaded = match resolved {
        Some((converter, id)) => converter.convert(id, entry)?,
        None => asset::load_bundle(request.game_root, request.container, entry, edit.kind)?,
    };
    let parsed = super::parse_loaded(&edit, mappings, &loaded)?;
    if let Some(wanted) = request.class
        && !parsed
            .exports
            .iter()
            .any(|export| export.class_name.eq_ignore_ascii_case(wanted))
    {
        return Ok(Prepared::OtherClass);
    }
    let edits = edits_for(
        parsed.exports.iter().map(|export| &export.properties[..]),
        &request.sets,
        skipped,
    );
    if edits.is_empty() {
        return Ok(Prepared::NothingToChange);
    }
    edit.changes = PackageEdits {
        values: edits,
        ..PackageEdits::default()
    };
    let (patched, loaded) = super::preview_read_edits(&edit, mappings, loaded, &parsed)?;
    let changes = patched
        .applied
        .iter()
        .map(|applied| format!("{} = {} -> {}", applied.name, applied.before, applied.after))
        .collect();
    Ok(Prepared::Ready(Box::new(Staged {
        // A loose file is read from disk, and the mod has to name it by the game's path.
        entry: super::save_entry(&edit)?,
        patched,
        loaded,
        changes,
        shader_map_hashes: Vec::new(),
    })))
}

/// A container also hands back the store it was listed from: resolving each package by path
/// again is the slowest thing a run over hundreds of them can do.
type Listing = (
    Option<Arc<dyn IoStoreTrait>>,
    Vec<(Option<retoc::FPackageId>, String)>,
);

/// The packages the filter names, in path order so a limited run is repeatable.
fn matching(request: &SweepRequest<'_>, utoc: &std::path::Path) -> Result<Listing, String> {
    let (store, all): Listing = match source_of(request.container) {
        AssetSource::Utoc => {
            // Opening around the mod is what makes its copies win over the source container's, so
            // a layered run reads what the last one left and the source for everything else.
            let open_as = if request.layer && utoc.is_file() {
                utoc.to_string_lossy().into_owned()
            } else {
                request.container.to_string()
            };
            let (store, packages) =
                asset::list_packages_via(request.game_root, &open_as, request.container)?;
            let listed = packages
                .into_iter()
                .map(|(id, path)| (Some(id), path))
                .collect();
            (Some(store), listed)
        }
        AssetSource::Pak | AssetSource::Loose => (
            None,
            asset::list_package_entries(request.container)?
                .into_iter()
                .map(|path| (None, path))
                .collect(),
        ),
    };
    let needle = request.filter.map(str::to_lowercase);
    let mut paths: Vec<(Option<retoc::FPackageId>, String)> = all
        .into_iter()
        .filter(|(_, path)| {
            needle
                .as_ref()
                .is_none_or(|needle| path.to_lowercase().contains(needle.as_str()))
        })
        .collect();
    paths.sort_by(|left, right| left.1.cmp(&right.1));
    if let Some(limit) = request.limit {
        paths.truncate(limit);
    }
    Ok((store, paths))
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
    /// How many camera shakes a sweep is allowed to touch. The assertions below compare against
    /// it rather than a shipped total, which moves with the game.
    const LIMIT: usize = 12;

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
            entry(
                "Amplitude",
                PropertyValue::Default { fields: Vec::new() },
                Some((4, 4)),
            ),
            entry(
                "Frequency",
                PropertyValue::Unset {
                    declared: "Float",
                    enum_type: None,
                    fields: Vec::new(),
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
                class: None,
                mod_name: "RivalsTestSweep",
                sets: vec![set("Amplitude", "0"), set("AnimScale", "0")],
                limit: Some(12),
                layer: false,
                dry_run: true,
            },
            Some(&mappings),
            &super::super::SaveOptions::default(),
        )
        .expect("sweep");
        assert_eq!(report.matched, LIMIT);
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert!(report.written.is_none(), "a dry run writes nothing");
        assert!(report.edits() > 0, "camera shakes store amplitudes");
        for package in &report.changed {
            for change in &package.changes {
                assert!(change.ends_with("-> 0"), "{} in {}", change, package.entry);
            }
        }
    }

    /// A class filter narrows the path filter, so a class no matching package holds drops all of
    /// them rather than silently sweeping the wrong assets.
    #[test]
    fn a_class_filter_drops_packages_that_do_not_hold_it() {
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
        let sweep_with = |class| {
            sweep(
                &SweepRequest {
                    game_root: &root,
                    container: &container,
                    filter: Some("CameraShake"),
                    class,
                    mod_name: "RivalsTestSweep",
                    sets: vec![set("Amplitude", "0")],
                    limit: Some(12),
                    layer: false,
                    dry_run: true,
                },
                Some(&mappings),
                &super::super::SaveOptions::default(),
            )
            .expect("sweep")
        };
        let held = sweep_with(Some("LegacyCameraShakePattern"));
        assert_eq!(held.other_class, 0, "every camera shake holds one");
        assert!(held.edits() > 0);

        let missing = sweep_with(Some("StaticMesh"));
        assert_eq!(
            missing.other_class, LIMIT,
            "no camera shake holds a StaticMesh"
        );
        assert!(missing.changed.is_empty());
        assert_eq!(missing.edits(), 0);
    }

    /// Two sweeps over the same packages: without `layer` the second replaces what the first wrote,
    /// with it the two compose. This is the only test that writes, since the behaviour only exists
    /// once something is in the mod.
    #[test]
    fn a_second_sweep_replaces_unless_it_is_told_to_layer() {
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
        let entry = "Marvel/Content/Marvel/AbilitySystem/1011/101111/CameraShake_101111.uasset";

        let run = |mod_name: &str, property: &str, value: &str, layer: bool| {
            sweep(
                &SweepRequest {
                    game_root: &root,
                    container: &container,
                    filter: Some("AbilitySystem/1011/101111"),
                    class: None,
                    mod_name,
                    sets: vec![set(property, value)],
                    limit: None,
                    layer,
                    dry_run: false,
                },
                Some(&mappings),
                &super::super::SaveOptions::default(),
            )
            .expect("sweep")
        };
        // Reads one value back out of the written mod, by name at any depth.
        let read_back = |mod_name: &str, property: &str| -> Option<String> {
            let utoc = super::super::mod_pak_path(&root, mod_name)
                .expect("mod path")
                .with_extension("utoc");
            let loaded = crate::asset::load_bundle(
                &root,
                &utoc.to_string_lossy(),
                entry,
                crate::asset::AssetSource::Utoc,
            )
            .expect("read the mod back");
            let parsed = crate::schema_synth::parse_package(
                &rivals_uasset::AssetBundle {
                    asset: &loaded.asset_file_buffer,
                    exports: &loaded.exports_file_buffer,
                },
                Some(&mappings),
                &crate::schema_synth::PackageSource {
                    game_root: &root,
                    container: &utoc.to_string_lossy(),
                    entry,
                    kind: crate::asset::AssetSource::Utoc,
                },
            )
            .expect("parse");
            let mut found = Vec::new();
            for export in &parsed.exports {
                matches_in(&export.properties, &[set(property, "")], &mut found);
            }
            found.iter().find_map(|held| match held {
                Match::Set { before, .. } => Some(before.clone()),
                _ => None,
            })
        };
        let clean = |mod_name: &str| {
            if let Ok(pak) = super::super::mod_pak_path(&root, mod_name) {
                for extension in ["pak", "utoc", "ucas"] {
                    std::fs::remove_file(pak.with_extension(extension)).ok();
                }
            }
        };

        for (mod_name, layer) in [("RivalsTestPlain", false), ("RivalsTestLayer", true)] {
            clean(mod_name);
            let first = run(mod_name, "Amplitude", "0", false);
            assert!(first.failed.is_empty(), "{:?}", first.failed);
            assert_eq!(first.replaced, 0, "the mod was empty");
            assert_eq!(read_back(mod_name, "Amplitude").as_deref(), Some("0.0"));

            let second = run(mod_name, "OscillationDuration", "0.5", layer);
            assert!(second.failed.is_empty(), "{:?}", second.failed);
            assert!(second.replaced > 0, "the mod already held these");
            assert_eq!(second.layered, layer);
            assert_eq!(
                read_back(mod_name, "OscillationDuration").as_deref(),
                Some("0.5"),
                "the second sweep landed either way"
            );

            let amplitude = read_back(mod_name, "Amplitude");
            if layer {
                assert_eq!(
                    amplitude.as_deref(),
                    Some("0.0"),
                    "layering keeps what the first sweep wrote"
                );
            } else {
                assert_ne!(
                    amplitude.as_deref(),
                    Some("0.0"),
                    "without layering the second sweep read the source again"
                );
            }
            clean(mod_name);
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
