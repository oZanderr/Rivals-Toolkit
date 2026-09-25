//! Splits a legacy package into exports and parses each against its own declared byte range.

use std::collections::BTreeSet;
use std::io::Cursor as IoCursor;

use retoc::legacy_asset::{
    EPackageFlags, FLegacyPackageHeader, FObjectExport, FObjectImport, FPackageNameMap,
};
use retoc::version::EngineVersion;
use retoc::zen::FPackageIndex;
use serde::Serialize;

use crate::datatable;
use crate::mappings::{Mappings, SchemaFixups};
use crate::props::{Ctx, Diagnostics, read_index, read_property_block, read_struct};
use crate::reader::Cursor;
use crate::stringtable;
use crate::tagged;
use crate::tails;
use crate::value::PropertyEntry;

/// Marvel Rivals ships UE 5.3.2, and cooked packages carry no version of their own.
pub(crate) const FALLBACK_ENGINE_VERSION: EngineVersion = EngineVersion::UE5_3;

/// How many rows of unconsumed bytes to keep for display when an export does not parse cleanly.
const PREVIEW_ROWS: usize = 16;

/// The `.uasset` and `.uexp` halves of one cooked package.
pub struct AssetBundle<'a> {
    pub asset: &'a [u8],
    pub exports: &'a [u8],
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ExportStatus {
    /// Parsing landed exactly on the end of the declared range.
    Complete,
    /// Properties decoded; the remainder is this class's own bulk payload, which is named and
    /// measured rather than decoded.
    Payload {
        consumed: u64,
        payload_bytes: u64,
        kind: &'static str,
    },
    /// Properties decoded but the remainder is not attributable to anything known. Unlike
    /// `Payload`, this is a real signal that something may have gone wrong.
    Partial {
        consumed: u64,
        expected: u64,
    },
    Failed {
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct ParsedExport {
    pub index: u32,
    pub object_name: String,
    pub class_name: String,
    pub serial_offset: i64,
    pub serial_size: i64,
    /// The export table's references, as raw `FPackageIndex` values: negative for an import,
    /// positive for an export, zero for none.
    pub outer_index: i32,
    pub class_index: i32,
    pub super_index: i32,
    pub template_index: i32,
    pub object_flags: u32,
    /// Whether the cooker gave the export a public hash, which is how other packages import it.
    pub generate_public_hash: bool,
    /// UE's dotted path for the object, `/Package/Path.Object:Sub`.
    pub path: String,
    pub status: ExportStatus,
    pub properties: Vec<PropertyEntry>,
    /// Where the property block ended, before the guid word and any class tail. Present when the
    /// block was read without error; it is the range a reset replaces.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties_end: Option<u64>,
    /// How many slots the class schema declares, which sizes the header a reset writes.
    #[serde(skip)]
    pub schema_slots: Option<u32>,
    /// Present only when the export is a DataTable that parsed far enough to reach its rows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_table: Option<datatable::DataTable>,
    /// Present only when the export is a StringTable whose entries were read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub string_table: Option<stringtable::StringTable>,
    /// The field records a script struct export declares, which is the schema for its own values.
    #[serde(skip)]
    pub struct_definition: Option<usmap::Struct>,
    /// Hex preview of the bytes parsing did not account for. On a failure it starts a little
    /// before the break so the run-up is visible, with `|` marking where parsing stopped.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub trailing_hex: String,
    /// Why a class, function or struct layout could not be followed to its end, when it could not.
    /// The export then keeps a payload status naming the layout, and this says what stopped the walk.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Where every FName inside this export's bytes was read from. Copying it into another
    /// package rewrites each of them, and nothing in the bytes says which pairs are names.
    #[serde(skip)]
    pub name_refs: Vec<u64>,
    /// Where the export's `SuperStruct` index sits, for a class, function or struct whose layout
    /// was walked. `None` for everything else, which is what makes a reparent refusable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub super_struct_at: Option<u64>,
    /// The bytecode this export stores, disassembled. Present for a class or function whose
    /// layout walk reached its script.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script: Option<crate::kismet::Script>,
    /// Instanced struct payloads inside this export that did not decode. The export can still be
    /// `Complete`, since each payload's length prefix puts the cursor back, so this is the only
    /// place such a failure is counted.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub undecoded: Vec<crate::props::UndecodedPayload>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackageInfo {
    pub package_name: String,
    pub cooked: bool,
    pub unversioned_properties: bool,
    pub name_count: usize,
    pub import_count: usize,
    pub export_count: usize,
}

/// The names retoc leaves in place of an import it could not resolve when converting from zen.
pub fn is_unresolved_import_name(name: &str) -> bool {
    name == "UnknownExport"
        || name.starts_with(retoc::asset_conversion::UNRESOLVED_EXPORT_HASH_PREFIX)
        || name.starts_with(retoc::asset_conversion::UNRESOLVED_SCRIPT_HASH_PREFIX)
}

/// How to describe such an import: what kind of reference it was and the hash it carries, if any.
pub fn unresolved_import_note(name: &str) -> Option<String> {
    if let Some(index) = retoc::asset_conversion::decode_unresolved_script_import_name(name) {
        return Some(format!(
            "unresolved script import 0x{:016x}",
            index.to_raw()
        ));
    }
    if let Some(hex) = name.strip_prefix(retoc::asset_conversion::UNRESOLVED_EXPORT_HASH_PREFIX) {
        return Some(format!("unresolved export hash 0x{hex}"));
    }
    (name == "UnknownExport").then(|| "unresolved".to_string())
}

/// The paths of every import retoc left unnamed, read from the header alone so no mappings file
/// is needed.
pub fn unresolved_imports(bundle: &AssetBundle<'_>) -> Result<Vec<String>, String> {
    let header = read_header(bundle)?;
    Ok(header
        .imports
        .iter()
        .enumerate()
        .filter_map(|(at, import)| {
            let name = header.name_map.get(import.object_name).ok()?;
            is_unresolved_import_name(&name)
                .then(|| dotted_path(&header, FPackageIndex::create_import(at as u32)))
                .flatten()
        })
        .collect())
}

/// A bare `UnknownExport` import lost its hash to a converter that predates keeping it, so nothing
/// can name it again: the package has to be converted afresh from the container it came from.
pub fn lost_import_warning(imports: &[ImportInfo]) -> Option<String> {
    let lost = imports
        .iter()
        .filter(|import| import.object_name == "UnknownExport")
        .count();
    (lost > 0).then(|| {
        format!(
            "{lost} import(s) were extracted by a converter that dropped their hash; \
             re-extract this package from its container to recover them"
        )
    })
}

/// One row of the import table, with its references resolved to a path.
#[derive(Debug, Clone, Serialize)]
pub struct ImportInfo {
    /// The raw `FPackageIndex`, always negative.
    pub index: i32,
    pub class_package: String,
    pub class_name: String,
    pub outer_index: i32,
    pub object_name: String,
    pub path: String,
    /// retoc could not resolve the object when converting from zen and left a hash in its place.
    pub unresolved: bool,
    /// What names this import, which is what decides whether it can be dropped.
    pub usage: crate::import_remove::ImportUsage,
}

#[derive(Debug, Clone, Serialize)]
pub struct ParsedPackage {
    #[serde(flatten)]
    pub info: PackageInfo,
    pub names: Vec<String>,
    pub imports: Vec<ImportInfo>,
    pub exports: Vec<ParsedExport>,
    /// The preload dependency runs each export declares, in export order. `None` where the table
    /// could not be read, which is the one case a dependency edit refuses outright.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<Vec<crate::dependency::Runs>>,
    /// Structs encountered with neither a native layout nor a schema entry.
    pub unresolved_structs: Vec<String>,
    /// How many values of each property type were read while parsing this package.
    pub property_kinds: std::collections::BTreeMap<&'static str, usize>,
    /// Slots the mappings declare that this build does not serialize, found while parsing. Empty
    /// on a clean parse, and worth surfacing when not: it means the mappings are ahead of the data.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub schema_fixups: Vec<AppliedFixup>,
    /// Schemas the mappings file did not cover, with the package that defines each. A caller that
    /// can load those packages can synthesise them and parse again.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub missing_schemas: Vec<crate::props::MissingSchema>,
    /// Populated only when the caller asked for the header check.
    #[serde(skip_serializing_if = "HeaderCheck::is_empty")]
    pub header_check: HeaderCheck,
    /// Where each container's elements sit. Needed only to edit one, so it is not serialized.
    #[serde(skip)]
    pub containers: Vec<crate::props::ContainerLayout>,
    /// Declared slots the headers skipped, with what storing each would write. Filled only when
    /// `ParseOptions::declared_slots` is set, and needed only to edit one.
    #[serde(skip)]
    pub unset: Vec<crate::props::UnsetSlot>,
    /// Where every decoded object reference sits, for edits that renumber exports.
    #[serde(skip)]
    pub references: Vec<crate::props::IndexRef>,
    /// Where each string table's strings sit, for edits that change one.
    #[serde(skip)]
    pub string_tables: Vec<stringtable::StringTableLayout>,
    /// Every instanced struct payload with its length prefix, for edits that change its width.
    #[serde(skip)]
    pub instanced: Vec<crate::props::InstancedLayout>,
    /// Where each DataTable's rows sit, for edits that add, drop or rename one.
    #[serde(skip)]
    pub tables: Vec<datatable::DataTableLayout>,
    /// Where each MovieScene channel's key arrays sit, for edits that add or drop a key.
    #[serde(skip)]
    pub channels: Vec<crate::props::ChannelLayout>,
    /// How many of each bytecode token the package's scripts hold, named where the reader knows
    /// the name. A token with no name is one this build adds.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub script_tokens: std::collections::BTreeMap<String, usize>,
    /// How many texts of each `ETextHistoryType` this package holds.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub text_histories: std::collections::BTreeMap<i8, usize>,
    /// Names the mappings file holds more than one entry for, resolved to an entry other than
    /// the default because that is the one this package reads under.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub twins: Vec<TwinChoice>,
    /// The bulk data table: where each payload sits and whether its bytes can be replaced.
    pub resources: Vec<crate::write::ResourceInfo>,
}

/// How many unversioned headers were re-encoded, and how many did not come back byte for byte.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct HeaderCheck {
    pub checked: usize,
    pub differing: usize,
}

impl HeaderCheck {
    fn is_empty(&self) -> bool {
        self.checked == 0
    }
}

pub fn read_header(bundle: &AssetBundle<'_>) -> Result<FLegacyPackageHeader, String> {
    FLegacyPackageHeader::deserialize(
        &mut IoCursor::new(bundle.asset),
        Some(FALLBACK_ENGINE_VERSION.package_file_version()),
    )
    .map_err(|e| format!("parse package header: {e:#}"))
}

/// The package name table, which is what FName indices in export data resolve against.
pub fn package_names(header: &FLegacyPackageHeader) -> Vec<String> {
    header.name_map.copy_raw_names()
}

pub fn package_info(header: &FLegacyPackageHeader) -> PackageInfo {
    PackageInfo {
        package_name: header.summary.package_name.clone(),
        cooked: header.summary.has_package_flags(EPackageFlags::Cooked),
        unversioned_properties: header
            .summary
            .has_package_flags(EPackageFlags::UsesUnversionedProperties),
        name_count: header.name_map.num_names(),
        import_count: header.imports.len(),
        export_count: header.exports.len(),
    }
}

/// `mappings` is required only for unversioned packages. A tagged package carries its own type
/// information, so it can be read with no mappings file at all.
pub fn parse_package(
    bundle: &AssetBundle<'_>,
    mappings: Option<&Mappings>,
) -> Result<ParsedPackage, String> {
    parse_inner(bundle, mappings, None, None, ParseOptions::default()).map(|(parsed, _)| parsed)
}

/// Same parse, but also returns the byte range each property consumed. Used to find where a
/// desynced export first read the wrong number of bytes.
pub fn parse_package_traced(
    bundle: &AssetBundle<'_>,
    mappings: Option<&Mappings>,
) -> Result<(ParsedPackage, Vec<crate::props::TraceEntry>), String> {
    parse_inner(
        bundle,
        mappings,
        None,
        None,
        ParseOptions {
            trace: true,
            ..Default::default()
        },
    )
}

/// Parses with structs recovered from other packages available as a fallback for names the
/// mappings file has no entry for. See [`crate::ustruct`].
pub fn parse_package_with(
    bundle: &AssetBundle<'_>,
    mappings: Option<&Mappings>,
    synth: Option<&Mappings>,
) -> Result<ParsedPackage, String> {
    parse_inner(bundle, mappings, synth, None, ParseOptions::default()).map(|(parsed, _)| parsed)
}

/// [`parse_package_with`] with the caller choosing what the parse records.
pub fn parse_package_opts(
    bundle: &AssetBundle<'_>,
    mappings: Option<&Mappings>,
    synth: Option<&Mappings>,
    options: ParseOptions,
) -> Result<ParsedPackage, String> {
    parse_inner(bundle, mappings, synth, None, options).map(|(parsed, _)| parsed)
}

/// Same as [`parse_package_with`], but also returns the byte range each property consumed.
pub fn parse_package_traced_with(
    bundle: &AssetBundle<'_>,
    mappings: Option<&Mappings>,
    synth: Option<&Mappings>,
) -> Result<(ParsedPackage, Vec<crate::props::TraceEntry>), String> {
    parse_inner(
        bundle,
        mappings,
        synth,
        None,
        ParseOptions {
            trace: true,
            ..Default::default()
        },
    )
}

/// Parses with a fixed set of slots elided and no repair search, so a caller can test a schema
/// hypothesis of its own. Used by `rivals-cli asset diagnose`.
pub fn parse_package_probed(
    bundle: &AssetBundle<'_>,
    mappings: Option<&Mappings>,
    fixups: &SchemaFixups,
) -> Result<ParsedPackage, String> {
    parse_inner(
        bundle,
        mappings,
        None,
        Some(fixups),
        ParseOptions::default(),
    )
    .map(|(parsed, _)| parsed)
}

/// Parses and additionally re-encodes every unversioned header, comparing each with the bytes it
/// came from. This is the precondition for writing: a header the writer cannot reproduce must not
/// be re-emitted.
pub fn parse_package_checked(
    bundle: &AssetBundle<'_>,
    mappings: Option<&Mappings>,
    synth: Option<&Mappings>,
) -> Result<ParsedPackage, String> {
    parse_inner(
        bundle,
        mappings,
        synth,
        None,
        ParseOptions {
            check_headers: true,
            ..Default::default()
        },
    )
    .map(|(parsed, _)| parsed)
}

/// What a caller wants the parse to record beyond the values themselves.
#[derive(Debug, Clone, Copy, Default)]
pub struct ParseOptions {
    /// Record the byte range each property consumed.
    pub trace: bool,
    /// Re-encode every unversioned header and count the ones that differ.
    pub check_headers: bool,
    /// Emit an entry for every declared slot an export does not store.
    pub declared_slots: bool,
    /// Leave a name the mappings file holds twice on its default entry even when an export fails
    /// under it. A caller that can recover a class from its own package tries that first.
    pub skip_twins: bool,
}

fn parse_inner(
    bundle: &AssetBundle<'_>,
    mappings: Option<&Mappings>,
    synth: Option<&Mappings>,
    fixups: Option<&SchemaFixups>,
    options: ParseOptions,
) -> Result<(ParsedPackage, Vec<crate::props::TraceEntry>), String> {
    let header = read_header(bundle)?;
    let info = package_info(&header);
    if info.unversioned_properties && mappings.is_none() {
        return Err(
            "this package stores unversioned properties, which cannot be read without a .usmap mappings file".into(),
        );
    }
    let tagged = !info.unversioned_properties;
    let (mut exports, mut diagnostics) =
        parse_exports(bundle, &header, mappings, synth, fixups, tagged, options);

    // A name with twins in the file is switched before slots are elided: a wrong twin is a whole
    // wrong chain, which no elision repairs.
    let mut twins_chosen = Vec::new();
    let mut owned_mappings: Option<Mappings> = None;
    let mut owned_synth: Option<Mappings> = None;
    if let Some(base) = mappings
        && fixups.is_none()
        && !tagged
        && !options.skip_twins
    {
        for _ in 0..MAX_REPAIR_ROUNDS {
            if failed_count(&exports) == 0 {
                break;
            }
            let current = owned_mappings.as_ref().unwrap_or(base);
            let current_synth = owned_synth.as_ref().or(synth);
            let Some((alt, alt_synth, choice)) = search_twins(
                bundle,
                &header,
                current,
                current_synth,
                &exports,
                tagged,
                options,
            ) else {
                break;
            };
            let (repaired, repaired_diagnostics) = parse_exports(
                bundle,
                &header,
                Some(&alt),
                alt_synth.as_ref(),
                None,
                tagged,
                options,
            );
            exports = repaired;
            diagnostics = repaired_diagnostics;
            owned_mappings = Some(alt);
            owned_synth = alt_synth;
            twins_chosen.push(choice);
        }
    }
    let mappings = owned_mappings.as_ref().or(mappings);
    let synth = owned_synth.as_ref().or(synth);

    // A caller that supplied its own fixups is testing a hypothesis, so leave it alone.
    let mut schema_fixups = Vec::new();
    if let Some(mappings) = mappings
        && fixups.is_none()
        && !tagged
        && failed_count(&exports) > 0
    {
        let (found, applied) = search_fixups(bundle, &header, mappings, synth);
        if !found.is_empty() {
            let (repaired, repaired_diagnostics) = parse_exports(
                bundle,
                &header,
                Some(mappings),
                synth,
                Some(&found),
                tagged,
                options,
            );
            if failed_count(&repaired) < failed_count(&exports)
                && complete_count(&repaired) >= complete_count(&exports)
            {
                exports = repaired;
                diagnostics = repaired_diagnostics;
                schema_fixups = applied;
            }
        }
    }

    let mut usage = crate::import_remove::usage_from(&diagnostics.references, &header);
    let imports = header
        .imports
        .iter()
        .enumerate()
        .map(|(at, import)| {
            let name = |name| {
                header
                    .name_map
                    .get(name)
                    .map(|n| n.into_owned())
                    .unwrap_or_default()
            };
            let index = FPackageIndex::create_import(at as u32);
            let object_name = name(import.object_name);
            ImportInfo {
                index: index.index,
                class_package: name(import.class_package),
                class_name: name(import.class_name),
                outer_index: import.outer_index.index,
                unresolved: is_unresolved_import_name(&object_name),
                path: dotted_path(&header, index).unwrap_or_default(),
                object_name,
                usage: std::mem::take(&mut usage[at]),
            }
        })
        .collect();
    Ok((
        ParsedPackage {
            info,
            names: header.name_map.copy_raw_names(),
            imports,
            exports,
            dependencies: crate::dependency::runs_of(&header).ok(),
            unresolved_structs: diagnostics.unresolved_structs.into_iter().collect(),
            property_kinds: diagnostics.property_kinds,
            schema_fixups,
            missing_schemas: diagnostics.missing_schemas,
            header_check: HeaderCheck {
                checked: diagnostics.headers_checked,
                differing: diagnostics.headers_differing,
            },
            containers: diagnostics.containers,
            unset: diagnostics.unset,
            references: diagnostics.references,
            string_tables: diagnostics.string_tables,
            instanced: diagnostics.instanced,
            tables: diagnostics.tables,
            channels: diagnostics.channels,
            script_tokens: diagnostics
                .script_tokens
                .iter()
                .map(|(token, count)| {
                    (
                        format!(
                            "{token:#04X} {}",
                            crate::kismet::token_name(*token).unwrap_or("unknown")
                        ),
                        *count,
                    )
                })
                .collect(),
            text_histories: diagnostics.text_histories,
            twins: twins_chosen,
            resources: crate::write::resource_infos(&header, bundle.exports),
        },
        diagnostics.trace.unwrap_or_default(),
    ))
}

/// Extra properties tend to arrive as one feature landing in one struct, so a handful of rounds
/// covers the real cases without letting a hopeless search run away.
const MAX_REPAIR_ROUNDS: usize = 4;

/// A same-named mappings entry chosen over the default one, because the package reads under it.
#[derive(Debug, Clone, Serialize)]
pub struct TwinChoice {
    pub name: String,
    /// The entry's index in the mappings file.
    pub entry: u32,
    /// How many entries the name has.
    pub of: usize,
}

/// How many twin switches a package is allowed before the search gives up on it.
const MAX_TWIN_ATTEMPTS: usize = 8;

/// A name the mappings file holds more than one entry for resolves to the fullest by default. When
/// an export fails and such a name sits in its class chain or among the structs it failed inside,
/// the other entries are tried, and the first that reads more of the package is kept: the export
/// sizes are the oracle, as for the slot search.
fn search_twins(
    bundle: &AssetBundle<'_>,
    header: &FLegacyPackageHeader,
    mappings: &Mappings,
    synth: Option<&Mappings>,
    exports: &[ParsedExport],
    tagged: bool,
    options: ParseOptions,
) -> Option<(Mappings, Option<Mappings>, TwinChoice)> {
    let failed = failed_count(exports);
    let complete = complete_count(exports);
    let fixups = SchemaFixups::default();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut tried = 0usize;
    for export in exports
        .iter()
        .filter(|e| matches!(e.status, ExportStatus::Failed { .. }))
    {
        let class_path = header
            .exports
            .get(export.index as usize)
            .and_then(|e| dotted_path(header, e.class_index));
        let ctx = Ctx {
            mappings: Some(mappings),
            header,
            fixups: None,
            synth,
            local: None,
        };
        let mut names = ctx.ancestry_at(&export.class_name, class_path.as_deref());
        names.extend(candidate_structs(
            bundle,
            header,
            mappings,
            synth,
            &fixups,
            export.index,
        ));
        for name in names {
            if !seen.insert(name.clone()) {
                continue;
            }
            let twins = mappings.twins(&name);
            let chosen = mappings.chosen(&name);
            for &index in twins {
                if Some(index) == chosen {
                    continue;
                }
                tried += 1;
                if tried > MAX_TWIN_ATTEMPTS {
                    return None;
                }
                let alt = mappings.with_twin(&name, index);
                let alt_synth = synth.map(|held| crate::ustruct::rebase_synth(held, &alt));
                let (repaired, _) = parse_exports(
                    bundle,
                    header,
                    Some(&alt),
                    alt_synth.as_ref(),
                    None,
                    tagged,
                    options,
                );
                if failed_count(&repaired) < failed && complete_count(&repaired) >= complete {
                    return Some((
                        alt,
                        alt_synth,
                        TwinChoice {
                            name: name.clone(),
                            entry: index,
                            of: twins.len(),
                        },
                    ));
                }
            }
        }
    }
    None
}

/// A schema slot the reader concluded this build does not serialize.
#[derive(Debug, Clone, Serialize)]
pub struct AppliedFixup {
    pub struct_name: String,
    pub slot: usize,
    pub property: String,
}

fn parse_exports(
    bundle: &AssetBundle<'_>,
    header: &FLegacyPackageHeader,
    mappings: Option<&Mappings>,
    synth: Option<&Mappings>,
    fixups: Option<&SchemaFixups>,
    tagged: bool,
    options: ParseOptions,
) -> (Vec<ParsedExport>, Diagnostics) {
    let ctx = Ctx {
        mappings,
        header,
        fixups,
        synth,
        local: None,
    };
    let mut diagnostics = Diagnostics {
        trace: options.trace.then(Vec::new),
        check_headers: options.check_headers,
        declared_slots: options.declared_slots,
        ..Default::default()
    };
    let exports = (0..header.exports.len())
        .map(|index| parse_one(bundle, header, &ctx, index as u32, tagged, &mut diagnostics))
        .collect();
    (exports, diagnostics)
}

fn failed_count(exports: &[ParsedExport]) -> usize {
    exports
        .iter()
        .filter(|e| matches!(e.status, ExportStatus::Failed { .. }))
        .count()
}

fn complete_count(exports: &[ParsedExport]) -> usize {
    exports
        .iter()
        .filter(|e| matches!(e.status, ExportStatus::Complete))
        .count()
}

/// Searches for schema slots whose removal makes failing exports land exactly on their declared
/// size. The export table's size field is the oracle: a wrong elision overruns or undershoots it,
/// so an exact landing is evidence rather than a guess. Ties are possible and are confined to
/// properties that occupy no bytes, so the values reported are the same either way.
fn search_fixups(
    bundle: &AssetBundle<'_>,
    header: &FLegacyPackageHeader,
    mappings: &Mappings,
    synth: Option<&Mappings>,
) -> (SchemaFixups, Vec<AppliedFixup>) {
    let mut fixups = SchemaFixups::default();
    let mut applied = Vec::new();
    let mut exhausted: BTreeSet<String> = BTreeSet::new();

    for _ in 0..MAX_REPAIR_ROUNDS {
        let current = (!fixups.is_empty()).then_some(&fixups);
        let (exports, _) = parse_exports(
            bundle,
            header,
            Some(mappings),
            synth,
            current,
            false,
            ParseOptions::default(),
        );
        let mut found = None;
        for export in exports
            .iter()
            .filter(|e| matches!(e.status, ExportStatus::Failed { .. }))
        {
            for name in candidate_structs(bundle, header, mappings, synth, &fixups, export.index) {
                if exhausted.contains(&name) {
                    continue;
                }
                match winning_slot(
                    bundle,
                    header,
                    mappings,
                    synth,
                    &fixups,
                    export.index,
                    &name,
                ) {
                    Some(slot) => {
                        found = Some((name, slot));
                        break;
                    }
                    None => {
                        exhausted.insert(name);
                    }
                }
            }
            if found.is_some() {
                break;
            }
        }
        let Some((name, slot)) = found else { break };
        let property = mappings
            .schema_fixed(&name, Some(&fixups))
            .and_then(|s| s.slot(slot))
            .map_or_else(String::new, |s| s.property.name.clone());
        let original = shift_into(fixups.get(&name), slot);
        fixups.add(&name, original);
        applied.push(AppliedFixup {
            struct_name: name,
            slot: original,
            property,
        });
    }
    (fixups, applied)
}

/// Turns a slot index in the already-elided numbering back into the original numbering.
fn shift_into(elided: &[usize], slot: usize) -> usize {
    let mut index = slot;
    for skipped in elided {
        if index >= *skipped {
            index += 1;
        }
    }
    index
}

/// The structs the reader was inside when it gave up, innermost first.
fn candidate_structs(
    bundle: &AssetBundle<'_>,
    header: &FLegacyPackageHeader,
    mappings: &Mappings,
    synth: Option<&Mappings>,
    fixups: &SchemaFixups,
    index: u32,
) -> Vec<String> {
    let ctx = Ctx {
        mappings: Some(mappings),
        header,
        fixups: Some(fixups),
        synth,
        local: None,
    };
    let mut diagnostics = Diagnostics::default();
    parse_one(bundle, header, &ctx, index, false, &mut diagnostics);
    diagnostics.failing_structs
}

/// The highest slot of `name` whose removal makes the export land exactly on its declared end.
///
/// Several neighbouring slots can qualify when the properties between them are all zero, since a
/// zero occupies no bytes whichever field it lands in. Taking the highest keeps every slot below
/// it correctly labelled, so at most one zero-valued property ends up under its neighbour's name.
fn winning_slot(
    bundle: &AssetBundle<'_>,
    header: &FLegacyPackageHeader,
    mappings: &Mappings,
    synth: Option<&Mappings>,
    fixups: &SchemaFixups,
    index: u32,
    name: &str,
) -> Option<usize> {
    let slots = mappings.schema_fixed(name, Some(fixups))?.len();
    (0..slots).rev().find(|slot| {
        let mut trial = fixups.clone();
        trial.add(name, shift_into(fixups.get(name), *slot));
        let ctx = Ctx {
            mappings: Some(mappings),
            header,
            fixups: Some(&trial),
            synth,
            local: None,
        };
        let mut diagnostics = Diagnostics::default();
        let export = parse_one(bundle, header, &ctx, index, false, &mut diagnostics);
        matches!(export.status, ExportStatus::Complete)
    })
}

/// One export, with the undecoded payloads found while reading it charged to it. The diagnostics
/// are per package, so the drain is what turns them into a per-export figure whatever path the
/// body took out.
fn parse_one(
    bundle: &AssetBundle<'_>,
    header: &FLegacyPackageHeader,
    ctx: &Ctx<'_>,
    index: u32,
    tagged: bool,
    diagnostics: &mut Diagnostics,
) -> ParsedExport {
    let from = diagnostics.undecoded.len();
    let mut parsed = parse_one_inner(bundle, header, ctx, index, tagged, diagnostics);
    parsed.undecoded = diagnostics.undecoded.drain(from..).collect();
    parsed
}

fn parse_one_inner(
    bundle: &AssetBundle<'_>,
    header: &FLegacyPackageHeader,
    ctx: &Ctx<'_>,
    index: u32,
    tagged: bool,
    diagnostics: &mut Diagnostics,
) -> ParsedExport {
    let Some(export) = header.exports.get(index as usize) else {
        return failed(skeleton(index, header, None), "export index out of range");
    };
    let mut base = skeleton(index, header, Some(export));
    let slice = match export_slice(bundle, header, base.serial_offset, base.serial_size) {
        Ok(slice) => slice,
        Err(reason) => return failed(base, &reason),
    };
    let class_name = base.class_name.clone();
    let class_path = dotted_path(header, export.class_index);
    if let Some(kind) = tails::opaque_kind(
        &class_name,
        &ctx.ancestry_at(&class_name, class_path.as_deref()),
        ctx.missing_ancestor(&class_name, class_path.as_deref())
            .as_deref(),
    ) {
        return ParsedExport {
            status: ExportStatus::Payload {
                consumed: 0,
                payload_bytes: base.serial_size.max(0) as u64,
                kind,
            },
            ..base
        };
    }

    let mut cursor = Cursor::new(slice, base.serial_offset as u64);
    let mut properties = Vec::new();
    let outcome = if tagged {
        tagged::read_tagged_block(&mut cursor, ctx, diagnostics, 0, &mut properties)
    } else {
        match ctx.class_schema_at(&class_name, class_path.as_deref()) {
            Some(schema) => {
                base.schema_slots = Some(schema.len() as u32);
                read_property_block(&mut cursor, &schema, ctx, diagnostics, 0, &mut properties)
            }
            None => Err(format!("class {class_name} is not in the mappings file")),
        }
    };
    if let Err(reason) = outcome {
        return ParsedExport {
            status: ExportStatus::Failed { reason },
            properties,
            trailing_hex: hex_preview_at(&cursor),
            ..base
        };
    }
    let properties_end = Some(cursor.file_offset());

    if let Err(reason) = read_object_guid(&mut cursor) {
        return ParsedExport {
            status: ExportStatus::Failed { reason },
            properties,
            properties_end,
            trailing_hex: hex_preview_at(&cursor),
            ..base
        };
    }

    // Only animation Blueprint classes declare sparse class data, and it is written with their
    // default object, whose own chain runs through `AnimInstance`.
    let chain = ctx.ancestry_at(&class_name, class_path.as_deref());
    let mut sparse_payload = None;
    if base.object_flags & RF_CLASS_DEFAULT_OBJECT != 0
        && chain.iter().any(|step| step == "AnimInstance")
    {
        sparse_payload = read_sparse_class_data(&mut cursor, header, ctx, diagnostics);
    }

    let mut data_table = None;
    // A tagged package reads without mappings, so its tables are known by their class name.
    let is_table = ctx
        .mappings
        .is_some_and(|m| m.inherits_from(&class_name, "DataTable"))
        || (tagged && matches!(class_name.as_str(), "DataTable" | "CompositeDataTable"));
    if is_table {
        match datatable::read_rows(&mut cursor, &properties, ctx, index, tagged, diagnostics) {
            Ok(table) => {
                if let Some(reason) = table.truncated.clone() {
                    return ParsedExport {
                        status: ExportStatus::Failed { reason },
                        properties,
                        properties_end,
                        data_table: Some(table),
                        trailing_hex: hex_preview_at(&cursor),
                        ..base
                    };
                }
                data_table = Some(table);
            }
            Err(reason) => {
                return ParsedExport {
                    status: ExportStatus::Failed { reason },
                    properties,
                    properties_end,
                    trailing_hex: hex_preview_at(&cursor),
                    ..base
                };
            }
        }
    }

    let mut string_table = None;
    if ctx
        .mappings
        .is_some_and(|m| m.inherits_from(&class_name, "StringTable"))
    {
        match stringtable::read_string_table(&mut cursor, ctx, index, diagnostics) {
            Ok(table) => string_table = Some(table),
            Err(reason) => {
                return ParsedExport {
                    status: ExportStatus::Failed { reason },
                    properties,
                    properties_end,
                    trailing_hex: hex_preview_at(&cursor),
                    ..base
                };
            }
        }
    }

    // A table has already consumed its rows or entries, and a sparse data block that did not decode
    // owns the rest; anything else may still owe a class tail.
    let block_consumed = data_table.is_some() || string_table.is_some() || sparse_payload.is_some();
    let mut tail_payload = sparse_payload;
    if !block_consumed && cursor.remaining() > 0 {
        let restore = cursor.position();
        let named = cursor.names_len();
        match tails::read_class_tail(&chain, &mut cursor, ctx, diagnostics, &mut properties) {
            Ok(tails::TailOutcome::Consumed) => {}
            Ok(tails::TailOutcome::Payload(kind)) => tail_payload = Some(kind),
            Err(reason) => {
                base.note = Some(reason);
                cursor.seek_to(restore).ok();
                cursor.truncate_names(named);
            }
        }
    }

    // A class, function or struct export's layout is walked for the references it holds. Bytecode
    // is stepped over, not read, so an export that has any keeps a payload status naming it.
    let mut bytecode = None;
    let mut script = None;
    let mut struct_definition = None;
    let mut super_struct_at = None;
    if !block_consumed
        && cursor.remaining() > 0
        && let Some(mappings) = ctx.mappings
    {
        let chain = mappings.ancestry(&class_name);
        if chain.contains(&"Class")
            || chain.contains(&"Function")
            || chain.contains(&"ScriptStruct")
        {
            let restore = cursor.position();
            let named = cursor.names_len();
            match crate::ustruct::scan_struct_tail(&mut cursor, header, &chain) {
                Ok(tail) => {
                    diagnostics.references.extend(tail.references);
                    bytecode = tail.bytecode;
                    super_struct_at = Some(tail.super_struct_at);
                    if let Some((from, to)) = tail.bytecode {
                        let base_at = base.serial_offset.max(0) as u64;
                        let range = (from - base_at) as usize..(to - base_at) as usize;
                        if let Some(bytes) = slice.get(range) {
                            script = Some(crate::kismet::read_script(
                                bytes,
                                from,
                                tail.sizes_at,
                                Some(tail.buffer_size),
                                (to - from) as u32,
                                ctx,
                                diagnostics,
                            ));
                        }
                    }
                    if let Some(properties) = tail.definition {
                        let super_struct = (tail.super_struct != 0)
                            .then(|| {
                                parent_name(
                                    header,
                                    FPackageIndex {
                                        index: tail.super_struct,
                                    },
                                )
                            })
                            .flatten();
                        let definition = usmap::Struct {
                            name: base.object_name.clone(),
                            super_struct,
                            properties,
                        };
                        if chain.contains(&"UserDefinedStruct") && cursor.remaining() > 0 {
                            read_default_instance(&mut cursor, ctx, diagnostics, &definition);
                        }
                        struct_definition = Some(definition);
                    }
                }
                Err(reason) => {
                    base.note = Some(reason);
                    cursor.seek_to(restore).ok();
                    cursor.truncate_names(named);
                }
            }
        }
    }

    base.script = script;
    base.super_struct_at = super_struct_at;
    base.name_refs = cursor.take_names();
    let consumed = cursor.position() as u64;
    let expected = base.serial_size.max(0) as u64;
    let status = if let Some((start, end)) = bytecode {
        ExportStatus::Payload {
            consumed: start.saturating_sub(base.serial_offset.max(0) as u64),
            payload_bytes: end - start,
            kind: "bytecode",
        }
    } else if consumed == expected {
        ExportStatus::Complete
    } else if let Some(kind) = tail_payload.or_else(|| tails::payload_kind(&chain)) {
        ExportStatus::Payload {
            consumed,
            payload_bytes: expected.saturating_sub(consumed),
            kind,
        }
    } else {
        ExportStatus::Partial { consumed, expected }
    };
    let trailing_hex = match status {
        ExportStatus::Complete | ExportStatus::Payload { .. } => String::new(),
        _ => hex_preview(cursor.rest(), cursor.file_offset()),
    };

    ParsedExport {
        status,
        properties,
        properties_end,
        data_table,
        string_table,
        struct_definition,
        trailing_hex,
        ..base
    }
}

/// A Blueprint struct stores its default instance as a block of its own fields, so the definition
/// just scanned is the schema to read it with. A block that does not read cleanly is left where it
/// was, and the export reports the bytes as unexplained.
fn read_default_instance(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    definition: &usmap::Struct,
) {
    let local = Mappings::from_structs(vec![definition.clone()]);
    let Some(schema) = local.schema(&definition.name) else {
        return;
    };
    let local_ctx = Ctx {
        mappings: ctx.mappings,
        header: ctx.header,
        fixups: ctx.fixups,
        synth: ctx.synth,
        local: Some(&local),
    };
    let restore = cursor.position();
    let named = cursor.names_len();
    let mut fields = Vec::new();
    if read_property_block(cursor, &schema, &local_ctx, diagnostics, 0, &mut fields).is_err() {
        cursor.seek_to(restore).ok();
        cursor.truncate_names(named);
    }
}

/// Everything an export table row says about an export before a byte of it is read. The parse
/// fills in the rest; a failure keeps this and adds its reason.
fn skeleton(
    index: u32,
    header: &FLegacyPackageHeader,
    export: Option<&FObjectExport>,
) -> ParsedExport {
    let mut base = ParsedExport {
        index,
        object_name: String::new(),
        class_name: String::new(),
        serial_offset: 0,
        serial_size: 0,
        outer_index: 0,
        class_index: 0,
        super_index: 0,
        template_index: 0,
        object_flags: 0,
        generate_public_hash: false,
        path: String::new(),
        status: ExportStatus::Failed {
            reason: String::new(),
        },
        properties: Vec::new(),
        properties_end: None,
        schema_slots: None,
        data_table: None,
        string_table: None,
        struct_definition: None,
        trailing_hex: String::new(),
        note: None,
        script: None,
        super_struct_at: None,
        name_refs: Vec::new(),
        undecoded: Vec::new(),
    };
    if let Some(export) = export {
        base.object_name = header
            .name_map
            .get(export.object_name)
            .map(|n| n.into_owned())
            .unwrap_or_default();
        base.class_name = object_short_name(header, export.class_index).unwrap_or_default();
        base.serial_offset = export.serial_offset;
        base.serial_size = export.serial_size;
        base.outer_index = export.outer_index.index;
        base.class_index = export.class_index.index;
        base.super_index = export.super_index.index;
        base.template_index = export.template_index.index;
        base.object_flags = export.object_flags;
        base.generate_public_hash = export.generate_public_hash;
        base.path = dotted_path(header, FPackageIndex::create_export(index)).unwrap_or_default();
    }
    base
}

/// How a class definition names its parent: a Blueprint parent by object path, since the mappings
/// never hold one and two can share a name; a native parent by the short name the mappings key on.
fn parent_name(header: &FLegacyPackageHeader, index: FPackageIndex) -> Option<String> {
    let blueprint = if index.is_import() {
        let import = header.imports.get(index.to_import_index() as usize)?;
        header
            .name_map
            .get(import.class_name)
            .is_ok_and(|class| class.ends_with("BlueprintGeneratedClass"))
    } else {
        index.is_export()
    };
    if blueprint {
        dotted_path(header, index)
    } else {
        object_short_name(header, index)
    }
}

fn failed(mut base: ParsedExport, reason: &str) -> ParsedExport {
    base.status = ExportStatus::Failed {
        reason: reason.to_string(),
    };
    base
}

/// UE's dotted path for an import or export: the package, `.` the outermost object, then `:` for
/// each subobject. `None` for a null index or one that points outside the tables.
pub fn dotted_path(header: &FLegacyPackageHeader, index: FPackageIndex) -> Option<String> {
    path_from(
        &header.name_map,
        &header.imports,
        &header.exports,
        &header.summary.package_name,
        index,
    )
}

/// [`dotted_path`] over tables that may differ from the header's, which is what an edit in
/// progress has.
pub(crate) fn path_from(
    names: &FPackageNameMap,
    imports: &[FObjectImport],
    exports: &[FObjectExport],
    package_name: &str,
    index: FPackageIndex,
) -> Option<String> {
    let mut chain = Vec::new();
    let mut current = index;
    let package = loop {
        if chain.len() > 64 {
            return None;
        }
        if current.is_import() {
            let import = imports.get(current.to_import_index() as usize)?;
            let name = names.get(import.object_name).ok()?.into_owned();
            if import.outer_index.is_null() {
                break name;
            }
            chain.push(name);
            current = import.outer_index;
        } else if current.is_export() {
            let export = exports.get(current.to_export_index() as usize)?;
            chain.push(names.get(export.object_name).ok()?.into_owned());
            if export.outer_index.is_null() {
                break package_name.to_string();
            }
            current = export.outer_index;
        } else {
            return None;
        }
    };
    let mut out = package;
    for (depth, name) in chain.iter().rev().enumerate() {
        out.push(if depth == 0 { '.' } else { ':' });
        out.push_str(name);
    }
    Some(out)
}

/// `UObject::Serialize` follows the script properties with a flag word, and a GUID when it is set.
/// Missing this leaves every export four bytes short. A serialized bool is only ever 0 or 1, so
/// anything else means the class writes its own data here and nothing should be consumed.
/// `RF_ClassDefaultObject`.
const RF_CLASS_DEFAULT_OBJECT: u32 = 0x10;

/// `UClass::SerializeSparseClassData`, which an animation Blueprint's default object carries after
/// its guid: the sparse data struct as an object reference, then one property block of it. The
/// struct is a Blueprint struct of the same package, so the block reads only once the package's
/// own definitions are in play. A block that still does not read is named as a payload: the
/// engine's `AnimNodeFunctionRef` carries more slots than the mappings file declares.
fn read_sparse_class_data(
    cursor: &mut Cursor<'_>,
    header: &FLegacyPackageHeader,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
) -> Option<&'static str> {
    let index = match read_index(cursor, diagnostics) {
        Ok(index) => index,
        Err(_) => return None,
    };
    if index == 0 {
        return None;
    }
    let block = cursor.position();
    let marks = diagnostics.marks();
    let readable = object_short_name(header, FPackageIndex { index })
        .ok_or_else(|| "the sparse class data struct is unresolved".to_string())
        .and_then(|name| read_struct(&name, cursor, ctx, diagnostics, 0));
    match readable {
        Ok(_) => None,
        Err(_) => {
            cursor.seek_to(block).ok();
            diagnostics.rewind(&marks);
            Some("sparse class data")
        }
    }
}

fn read_object_guid(cursor: &mut Cursor<'_>) -> Result<(), String> {
    match cursor.peek_u32() {
        Some(0) => cursor.skip(4),
        Some(1) => {
            cursor.skip(4)?;
            if cursor.remaining() >= 16 {
                cursor.skip(16)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Where the package header ends and the exports file begins, which is what decides whether a
/// byte offset lands in the `.uasset` or the `.uexp`.
pub fn header_size(bundle: &AssetBundle<'_>) -> Result<u64, String> {
    let header = read_header(bundle)?;
    u64::try_from(header.summary.versioning_info.total_header_size)
        .map_err(|_| "package declares a negative header size".to_string())
}

/// The raw bytes of one export, for reading a layout off the disk when the decoder disagrees
/// with the mappings.
pub fn export_bytes<'a>(
    bundle: &AssetBundle<'a>,
    header: &FLegacyPackageHeader,
    index: u32,
) -> Result<(&'a [u8], i64), String> {
    let export = header
        .exports
        .get(index as usize)
        .ok_or_else(|| format!("no export {index}"))?;
    let slice = export_slice(bundle, header, export.serial_offset, export.serial_size)?;
    Ok((slice, export.serial_offset))
}

fn export_slice<'a>(
    bundle: &AssetBundle<'a>,
    header: &FLegacyPackageHeader,
    serial_offset: i64,
    serial_size: i64,
) -> Result<&'a [u8], String> {
    if serial_offset < 0 || serial_size < 0 {
        return Err(format!(
            "export declares a negative range (offset {serial_offset}, size {serial_size})"
        ));
    }
    let header_size = i64::from(header.summary.versioning_info.total_header_size);
    let (buffer, start) = if serial_offset >= header_size {
        (bundle.exports, (serial_offset - header_size) as usize)
    } else {
        (bundle.asset, serial_offset as usize)
    };
    let end = start
        .checked_add(serial_size as usize)
        .ok_or_else(|| "export range overflows".to_string())?;
    buffer.get(start..end).ok_or_else(|| {
        format!(
            "export range {start}..{end} falls outside the {} byte buffer",
            buffer.len()
        )
    })
}

pub(crate) fn object_short_name(
    header: &FLegacyPackageHeader,
    index: FPackageIndex,
) -> Option<String> {
    if index.is_import() {
        let import = header.imports.get(index.to_import_index() as usize)?;
        header
            .name_map
            .get(import.object_name)
            .ok()
            .map(|n| n.into_owned())
    } else if index.is_export() {
        let export = header.exports.get(index.to_export_index() as usize)?;
        header
            .name_map
            .get(export.object_name)
            .ok()
            .map(|n| n.into_owned())
    } else {
        None
    }
}

/// Bytes leading up to the stopping point make a desync far easier to read than the tail alone.
const HEX_CONTEXT_BYTES: usize = 48;

fn hex_preview(bytes: &[u8], base: u64) -> String {
    crate::hex::render(bytes, base, Some(PREVIEW_ROWS))
}

fn hex_preview_at(cursor: &Cursor<'_>) -> String {
    let (window, marker) = cursor.window(HEX_CONTEXT_BYTES);
    crate::hex::render(
        window,
        cursor.file_offset() - marker as u64,
        Some(PREVIEW_ROWS),
    )
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_hex_preview_wraps_every_sixteen_bytes_and_labels_the_offsets() {
        let bytes: Vec<u8> = (0..20).collect();
        let preview = hex_preview(&bytes, 0x100);
        assert_eq!(preview.lines().count(), 2);
        assert!(preview.starts_with("0x100      00 01"), "{preview}");
        assert!(preview.contains("0x110"), "{preview}");
    }

    #[test]
    fn a_preview_longer_than_the_cap_says_how_much_was_elided() {
        let bytes = vec![0u8; PREVIEW_ROWS * 16 + 10];
        let preview = hex_preview(&bytes, 0);
        assert!(preview.contains("10 more bytes"), "{preview}");
    }

    #[test]
    fn every_name_retoc_leaves_for_an_unresolved_import_is_recognised() {
        for name in [
            "UnknownExport",
            "__zenrawexporthash_00000000deadbeef",
            "__zenrawscripthash_46a3791039776701",
        ] {
            assert!(is_unresolved_import_name(name), "{name}");
        }
        assert!(!is_unresolved_import_name("MountPak"));
        assert!(!is_unresolved_import_name("/Engine/UnknownPackage"));

        assert_eq!(
            unresolved_import_note("__zenrawscripthash_46a3791039776701").as_deref(),
            Some("unresolved script import 0x46a3791039776701")
        );
        assert_eq!(
            unresolved_import_note("__zenrawexporthash_00000000deadbeef").as_deref(),
            Some("unresolved export hash 0x00000000deadbeef")
        );
        assert_eq!(
            unresolved_import_note("UnknownExport").as_deref(),
            Some("unresolved")
        );
        assert_eq!(unresolved_import_note("MountPak"), None);
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tagged_package_tests {
    use super::*;
    use retoc::legacy_asset::{FLegacyPackageFileSummary, FObjectExport, FPackageNameMap};

    const HEADER_SIZE: usize = 1024;
    const NAMES: &[&str] = &[
        "None",
        "TestPackage",
        "TestObject",
        "TestClass",
        "Damage",
        "IntProperty",
        "/Script/Engine",
        "/Script/CoreUObject",
        "Package",
        "Class",
        "ScriptStruct",
        "DataTable",
        "CompositeDataTable",
        "TestRow",
        "RowStruct",
        "ObjectProperty",
        "ParentTables",
        "ArrayProperty",
        "Label",
        "StrProperty",
        "RowA",
        "RowB",
    ];

    fn name_index(value: &str) -> i32 {
        NAMES
            .iter()
            .position(|n| *n == value)
            .expect("name is in the test name map") as i32
    }

    fn minimal_name(value: &str) -> retoc::legacy_asset::FMinimalName {
        retoc::legacy_asset::FMinimalName {
            index: name_index(value),
            number: 0,
        }
    }

    fn write_name(out: &mut Vec<u8>, value: &str) {
        out.extend_from_slice(&name_index(value).to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
    }

    /// Builds a real cooked package whose flags say tagged, so the whole `parse_package` path is
    /// exercised rather than just the tagged block reader.
    fn tagged_package() -> (Vec<u8>, Vec<u8>) {
        let mut exports = Vec::new();
        write_name(&mut exports, "Damage");
        write_name(&mut exports, "IntProperty");
        exports.extend_from_slice(&4i32.to_le_bytes());
        exports.extend_from_slice(&0i32.to_le_bytes());
        exports.push(0);
        exports.extend_from_slice(&99i32.to_le_bytes());
        write_name(&mut exports, "None");

        let mut summary = FLegacyPackageFileSummary {
            package_name: "/Game/TestPackage".to_string(),
            ..Default::default()
        };
        summary.versioning_info.package_file_version =
            FALLBACK_ENGINE_VERSION.package_file_version();
        summary.versioning_info.total_header_size = HEADER_SIZE as i32;
        summary.package_flags = EPackageFlags::Cooked as u32;

        let header = FLegacyPackageHeader {
            summary,
            name_map: FPackageNameMap::create_from_names(
                NAMES.iter().map(|n| (*n).to_string()).collect(),
            ),
            // The serializer rebases this by the header size when it writes the export map.
            exports: vec![FObjectExport {
                object_name: minimal_name("TestObject"),
                serial_offset: 0,
                serial_size: exports.len() as i64,
                ..Default::default()
            }],
            ..Default::default()
        };

        let mut asset = std::io::Cursor::new(Vec::new());
        header
            .serialize(
                &mut asset,
                Some(HEADER_SIZE),
                &retoc::logging::Log::no_log(),
            )
            .expect("serialize the test package header");
        (asset.into_inner(), exports)
    }

    #[test]
    fn a_tagged_package_parses_with_no_mappings_file_at_all() {
        let (asset, exports) = tagged_package();
        let parsed = parse_package(
            &AssetBundle {
                asset: &asset,
                exports: &exports,
            },
            None,
        )
        .expect("a tagged package needs no mappings");

        assert!(!parsed.info.unversioned_properties);
        let export = &parsed.exports[0];
        assert!(
            matches!(export.status, ExportStatus::Complete),
            "expected every byte accounted for, got {:?}",
            export.status
        );
        assert_eq!(export.properties.len(), 1);
        assert_eq!(export.properties[0].name, "Damage");
        assert!(matches!(
            export.properties[0].value,
            crate::value::PropertyValue::Int { value: 99 }
        ));
    }

    fn write_tag(out: &mut Vec<u8>, name: &str, kind: &str, size: i32) {
        write_name(out, name);
        write_name(out, kind);
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
    }

    fn import(class: &str, outer: FPackageIndex, name: &str) -> retoc::legacy_asset::FObjectImport {
        retoc::legacy_asset::FObjectImport {
            class_package: minimal_name("/Script/CoreUObject"),
            class_name: minimal_name(class),
            outer_index: outer,
            object_name: minimal_name(name),
            is_optional: false,
        }
    }

    /// A tagged table of class `class`: a `RowStruct` pointing at an imported struct, then two
    /// rows, the second storing a field the first leaves out. A composite also lists its parents.
    fn tagged_table_package(class: &str) -> (Vec<u8>, Vec<u8>) {
        let mut exports = Vec::new();
        if class == "CompositeDataTable" {
            write_tag(&mut exports, "ParentTables", "ArrayProperty", 4);
            write_name(&mut exports, "ObjectProperty");
            exports.push(0);
            exports.extend_from_slice(&0i32.to_le_bytes());
        }
        write_tag(&mut exports, "RowStruct", "ObjectProperty", 4);
        exports.push(0);
        exports.extend_from_slice(&FPackageIndex::create_import(2).index.to_le_bytes());
        write_name(&mut exports, "None");
        exports.extend_from_slice(&0i32.to_le_bytes());

        exports.extend_from_slice(&2i32.to_le_bytes());
        write_name(&mut exports, "RowA");
        write_tag(&mut exports, "Damage", "IntProperty", 4);
        exports.push(0);
        exports.extend_from_slice(&7i32.to_le_bytes());
        write_name(&mut exports, "None");
        write_name(&mut exports, "RowB");
        write_tag(&mut exports, "Damage", "IntProperty", 4);
        exports.push(0);
        exports.extend_from_slice(&9i32.to_le_bytes());
        write_tag(&mut exports, "Label", "StrProperty", 7);
        exports.push(0);
        exports.extend_from_slice(&3i32.to_le_bytes());
        exports.extend_from_slice(b"hi\0");
        write_name(&mut exports, "None");

        let mut summary = FLegacyPackageFileSummary {
            package_name: "/Game/TestPackage".to_string(),
            ..Default::default()
        };
        summary.versioning_info.package_file_version =
            FALLBACK_ENGINE_VERSION.package_file_version();
        summary.versioning_info.total_header_size = HEADER_SIZE as i32;
        summary.package_flags = EPackageFlags::Cooked as u32;

        let engine = FPackageIndex::create_import(0);
        let header = FLegacyPackageHeader {
            summary,
            name_map: FPackageNameMap::create_from_names(
                NAMES.iter().map(|n| (*n).to_string()).collect(),
            ),
            imports: vec![
                import("Package", FPackageIndex::create_null(), "/Script/Engine"),
                import("Class", engine, class),
                import("ScriptStruct", engine, "TestRow"),
            ],
            exports: vec![FObjectExport {
                class_index: FPackageIndex::create_import(1),
                object_name: minimal_name("TestObject"),
                serial_offset: 0,
                serial_size: exports.len() as i64,
                ..Default::default()
            }],
            ..Default::default()
        };

        let mut asset = std::io::Cursor::new(Vec::new());
        header
            .serialize(
                &mut asset,
                Some(HEADER_SIZE),
                &retoc::logging::Log::no_log(),
            )
            .expect("serialize the test package header");
        (asset.into_inner(), exports)
    }

    fn parse_table(class: &str) -> ParsedPackage {
        let (asset, exports) = tagged_table_package(class);
        parse_package(
            &AssetBundle {
                asset: &asset,
                exports: &exports,
            },
            None,
        )
        .expect("a tagged table needs no mappings")
    }

    #[test]
    fn a_tagged_data_table_reads_its_rows_with_no_mappings_file() {
        let parsed = parse_table("DataTable");
        let export = &parsed.exports[0];
        assert!(
            matches!(export.status, ExportStatus::Complete),
            "expected every byte accounted for, got {:?}",
            export.status
        );
        let table = export.data_table.as_ref().expect("rows are read");
        assert_eq!(table.row_struct, "TestRow");
        assert_eq!(table.columns, ["Damage", "Label"]);
        let names: Vec<&str> = table.rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["RowA", "RowB"]);
        assert_eq!(table.rows[1].fields.len(), 2);

        let layout = &parsed.tables[0];
        assert!(layout.tagged);
        assert_eq!(layout.rows.len(), 2);
        assert_eq!(
            layout.rows[1].end as i64,
            HEADER_SIZE as i64 + export.serial_size
        );
    }

    #[test]
    fn a_tagged_composite_table_is_read_as_a_table_too() {
        let parsed = parse_table("CompositeDataTable");
        let export = &parsed.exports[0];
        assert!(matches!(export.status, ExportStatus::Complete));
        assert!(export.properties.iter().any(|p| p.name == "ParentTables"));
        assert_eq!(export.data_table.as_ref().expect("rows").rows.len(), 2);
    }

    /// Re-serializing a header is not symmetrical: `deserialize` hands back offsets with the header
    /// size folded in, and `serialize` adds it again. Anything that re-emits a header has to rebase
    /// first, so the helper here does what `crate::write` does.
    fn reserialize(
        asset: &[u8],
        exports: &[u8],
        edit: impl FnOnce(&mut FLegacyPackageHeader),
    ) -> Vec<u8> {
        let mut header = read_header(&AssetBundle { asset, exports }).expect("header");
        for export in &mut header.exports {
            export.serial_offset -= i64::from(header.summary.versioning_info.total_header_size);
        }
        edit(&mut header);
        let mut out = std::io::Cursor::new(Vec::new());
        header
            .serialize(&mut out, Some(HEADER_SIZE), &retoc::logging::Log::no_log())
            .expect("reserialize");
        out.into_inner()
    }

    #[test]
    fn an_unversioned_package_without_mappings_says_so_plainly() {
        let (asset, exports) = tagged_package();
        // Flip the flag in the serialized summary rather than rebuilding it.
        let asset = reserialize(&asset, &exports, |header| {
            header.summary.package_flags |= EPackageFlags::UsesUnversionedProperties as u32;
        });

        let err = parse_package(
            &AssetBundle {
                asset: &asset,
                exports: &exports,
            },
            None,
        )
        .expect_err("unversioned without mappings cannot be read");
        assert!(err.contains(".usmap"), "{err}");
    }

    /// The export table is the one thing a rewrite has to get right, and a doubled offset is
    /// invisible until an export is sliced. Reading the package back is what catches it.
    #[test]
    fn a_header_that_is_read_and_written_again_still_points_at_its_exports() {
        let (asset, exports) = tagged_package();
        let again = reserialize(&asset, &exports, |_| {});
        assert_eq!(again, asset, "an untouched header must come back unchanged");

        let bundle = AssetBundle {
            asset: &again,
            exports: &exports,
        };
        let header = read_header(&bundle).expect("header");
        assert_eq!(
            header.exports[0].serial_offset, HEADER_SIZE as i64,
            "the first export still starts where the header ends"
        );
        let parsed = parse_package(&bundle, None).expect("parse");
        assert!(matches!(parsed.exports[0].status, ExportStatus::Complete));
    }

    /// Appending to something at the very end of an export lands exactly on the boundary it shares
    /// with the next export. Charging that to the wrong side leaves both the wrong length, and the
    /// only symptom is a package that stops decoding partway.
    #[test]
    fn an_insertion_at_an_export_boundary_extends_the_export_it_came_from() {
        let (asset, exports) = tagged_package();
        let header = read_header(&AssetBundle {
            asset: &asset,
            exports: &exports,
        })
        .expect("header");
        let was = header.exports[0].serial_size;
        let at = header.exports[0].serial_offset + was;

        let rewritten = crate::write::rewrite(
            &AssetBundle {
                asset: &asset,
                exports: &exports,
            },
            &[crate::write::Splice {
                start: at as u64,
                end: at as u64,
                bytes: vec![0xAA, 0xBB],
            }],
            crate::write::HeaderDraft::default(),
        )
        .expect("rewrite");

        let after = read_header(&AssetBundle {
            asset: &rewritten.asset,
            exports: &rewritten.exports,
        })
        .expect("header");
        assert_eq!(
            after.exports[0].serial_size,
            was + 2,
            "the two bytes belong to the export they were appended to"
        );
        assert_eq!(rewritten.exports.len(), exports.len() + 2);
    }

    /// Two edits covering the same bytes cannot both be right, and applying either silently would
    /// discard the other. Refusing is the only honest answer.
    #[test]
    fn two_edits_over_the_same_bytes_are_refused_rather_than_resolved() {
        let (asset, exports) = tagged_package();
        let header = read_header(&AssetBundle {
            asset: &asset,
            exports: &exports,
        })
        .expect("header");
        let at = header.exports[0].serial_offset as u64;

        let error = crate::write::rewrite(
            &AssetBundle {
                asset: &asset,
                exports: &exports,
            },
            &[
                crate::write::Splice {
                    start: at,
                    end: at + 4,
                    bytes: vec![1, 2, 3, 4],
                },
                crate::write::Splice {
                    start: at + 2,
                    end: at + 6,
                    bytes: vec![5, 6, 7, 8],
                },
            ],
            crate::write::HeaderDraft::default(),
        )
        .expect_err("overlapping edits");
        assert!(error.contains("same bytes"), "{error}");
    }

    /// Rebasing is easy to leave out, and the symptom is an offset that has grown by exactly the
    /// header size. This pins that mistake so it cannot come back quietly.
    #[test]
    fn forgetting_to_rebase_doubles_every_export_offset() {
        let (asset, exports) = tagged_package();
        let mut header = read_header(&AssetBundle {
            asset: &asset,
            exports: &exports,
        })
        .expect("header");
        let was = header.exports[0].serial_offset;
        let mut out = std::io::Cursor::new(Vec::new());
        header
            .serialize(&mut out, Some(HEADER_SIZE), &retoc::logging::Log::no_log())
            .expect("serialize");
        let broken = out.into_inner();
        header = read_header(&AssetBundle {
            asset: &broken,
            exports: &exports,
        })
        .expect("header");
        assert_eq!(header.exports[0].serial_offset, was + HEADER_SIZE as i64);
    }
}
