//! Asset inspection subcommands, and the audit that measures parse coverage over a whole container.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use rivals_core::asset::{self, AssetSource};
use rivals_core::asset_edit::{self, AssetEditRequest};
use rivals_core::inherit;
use rivals_core::localization;
use rivals_core::mappings;
use rivals_core::schema_synth::{self, LayoutReader, PackageSource};
use rivals_uasset::{
    AssetBundle, ExportStatus, Mappings, PackageEdits, PackageInfo, ParseOptions, ParsedPackage,
    PropertyEntry, PropertyValue, RemovalPlan,
};
use serde::Serialize;

pub struct Request<'a> {
    pub game_root: &'a str,
    pub container: &'a str,
    pub entry: &'a str,
    pub usmap: Option<&'a str>,
    pub configured_usmap: Option<&'a str>,
    /// Also list the slots an export declares but does not store.
    pub declared: bool,
    /// What a write leaves behind. Reading commands ignore it.
    pub target: asset_edit::SaveTarget,
    /// Build a write on the mod's own copy when it already holds one.
    pub layer: bool,
    /// Save imports that point at nothing the game or an enabled mod has.
    pub allow_missing: bool,
    /// Save script edits that point at something whose kind could not be confirmed.
    pub allow_unchecked: bool,
    /// Save writes under this package name instead of the asset's own.
    pub save_as: Option<&'a str>,
    /// With `save_as`, keep the names of the objects named after the package.
    pub keep_object_names: bool,
    /// Leave the mod's other packages naming what a rename or a move changes.
    pub keep_referencers: bool,
}

fn source_of(container: &str) -> AssetSource {
    match Path::new(container).extension().and_then(|e| e.to_str()) {
        Some("utoc") => AssetSource::Utoc,
        _ => AssetSource::Pak,
    }
}

fn parse(request: &Request<'_>) -> Result<ParsedPackage, String> {
    let source = if request.container.is_empty() {
        AssetSource::Loose
    } else {
        source_of(request.container)
    };
    let bundle = asset::load_bundle(request.game_root, request.container, request.entry, source)?;
    // A tagged package carries its own type information, so missing mappings are only fatal for
    // the unversioned case, which parse_package reports itself.
    let schema = mappings::resolve(request.usmap, request.configured_usmap)
        .and_then(|path| mappings::load(&path))
        .ok();
    let parsed = schema_synth::parse_package_opts(
        &AssetBundle {
            asset: &bundle.asset_file_buffer,
            exports: &bundle.exports_file_buffer,
        },
        schema.as_deref(),
        &PackageSource {
            game_root: request.game_root,
            container: request.container,
            entry: request.entry,
            kind: source,
        },
        rivals_uasset::ParseOptions {
            declared_slots: request.declared,
            ..Default::default()
        },
    )?;
    if let Some(warning) = rivals_uasset::lost_import_warning(&parsed.imports) {
        eprintln!("warning: {warning}");
    }
    Ok(parsed)
}

fn kind_of(request: &Request<'_>) -> AssetSource {
    if request.container.is_empty() {
        AssetSource::Loose
    } else {
        source_of(request.container)
    }
}

fn edit_request<'a>(
    request: &Request<'a>,
    mod_name: &'a str,
    mut changes: PackageEdits,
) -> AssetEditRequest<'a> {
    changes.allow_missing |= request.allow_missing;
    changes.allow_unchecked |= request.allow_unchecked;
    if changes.save_as.is_none() {
        changes.save_as = request.save_as.map(|package| rivals_uasset::SaveAs {
            package: package.to_string(),
            rename_objects: !request.keep_object_names,
        });
    }
    AssetEditRequest {
        game_root: request.game_root,
        container: request.container,
        entry: request.entry,
        kind: kind_of(request),
        mod_name,
        changes,
    }
}

/// Writes new values into an asset and ships the result as a mod pak that overrides it.
pub fn set(
    request: &Request<'_>,
    changes: PackageEdits,
    mod_name: &str,
    replace: bool,
) -> Result<String, String> {
    write_edits(request, mod_name, replace, changes)
}

/// Points an import at another object, or adds one, and writes the result into a mod pak.
pub fn import_edit(
    request: &Request<'_>,
    edit: rivals_uasset::ImportEdit,
    mod_name: &str,
    replace: bool,
) -> Result<String, String> {
    write_edits(
        request,
        mod_name,
        replace,
        PackageEdits {
            imports: vec![edit],
            ..Default::default()
        },
    )
}

/// What `import_edit` would save, patched and verified in memory without writing anything.
pub fn preview_import_edit(
    request: &Request<'_>,
    edit: rivals_uasset::ImportEdit,
    mod_name: &str,
    replace: bool,
) -> Result<EditPreview, String> {
    preview_edits(
        request,
        mod_name,
        replace,
        PackageEdits {
            imports: vec![edit],
            ..Default::default()
        },
    )
}

/// What a save of `changes` would do, patched and verified in memory the way the save would be:
/// the changes it makes, and the notes it would carry, such as fields filled from an archetype or
/// imports a retarget leaves behind.
pub fn preview_edits(
    request: &Request<'_>,
    mod_name: &str,
    replace: bool,
    changes: PackageEdits,
) -> Result<EditPreview, String> {
    let schema = mappings::resolve(request.usmap, request.configured_usmap)
        .and_then(|path| mappings::load(&path))
        .ok();
    let outcome = asset_edit::preview_save(
        &edit_request(request, mod_name, changes),
        schema.as_deref(),
        &asset_edit::SaveOptions {
            replace,
            layer: request.layer,
            target: request.target,
            keep_referencers: request.keep_referencers,
            ..Default::default()
        },
    )?;
    match outcome {
        asset_edit::PreviewOutcome::Verified { applied, notes, .. } => {
            Ok(EditPreview { applied, notes })
        }
        asset_edit::PreviewOutcome::HoldsCopy { pak } => Err(format!(
            "{pak} already holds an edited copy of {}; pass --layer to build on it or --replace to start over",
            request.entry
        )),
    }
}

/// What an edit would change, for a dry run.
#[derive(Serialize)]
pub struct EditPreview {
    pub applied: Vec<rivals_uasset::AppliedEdit>,
    pub notes: Vec<String>,
}

/// What removing `exports` would do, without writing anything.
pub fn plan_removal(request: &Request<'_>, exports: &[u32]) -> Result<RemovalPlan, String> {
    let schema = mappings::resolve(request.usmap, request.configured_usmap)
        .and_then(|path| mappings::load(&path))
        .ok();
    asset_edit::plan_export_removal(
        &edit_request(
            request,
            "",
            PackageEdits {
                remove_exports: exports.to_vec(),
                ..Default::default()
            },
        ),
        schema.as_deref(),
    )
}

/// Removes `exports` and their subobjects, writing the result into a mod pak.
pub fn remove_exports(
    request: &Request<'_>,
    exports: &[u32],
    mod_name: &str,
    replace: bool,
) -> Result<String, String> {
    write_edits(
        request,
        mod_name,
        replace,
        PackageEdits {
            remove_exports: exports.to_vec(),
            ..Default::default()
        },
    )
}

/// Drops every value `export` stores so it inherits its class defaults, writing the result into a
/// mod pak.
pub fn reset_export(
    request: &Request<'_>,
    export: u32,
    mod_name: &str,
    replace: bool,
) -> Result<String, String> {
    write_edits(
        request,
        mod_name,
        replace,
        PackageEdits {
            reset_exports: vec![export],
            ..Default::default()
        },
    )
}

/// The names nothing in the package uses, as a save dropping them would find them.
pub fn unused_names(request: &Request<'_>, mod_name: &str) -> Result<Vec<String>, String> {
    let schema = mappings::resolve(request.usmap, request.configured_usmap)
        .and_then(|path| mappings::load(&path))
        .ok();
    asset_edit::unused_names(
        &edit_request(request, mod_name, PackageEdits::default()),
        schema.as_deref(),
        &asset_edit::SaveOptions {
            layer: request.layer,
            target: request.target,
            ..Default::default()
        },
    )
}

/// Drops the names nothing in the package uses and writes the result into a mod pak.
pub fn compact_names(
    request: &Request<'_>,
    mod_name: &str,
    replace: bool,
) -> Result<String, String> {
    write_edits(
        request,
        mod_name,
        replace,
        PackageEdits {
            compact_names: true,
            ..Default::default()
        },
    )
}

/// Adds an empty object of a class to the package and writes the result into a mod pak.
pub fn add_export(
    request: &Request<'_>,
    add: rivals_uasset::AddExport,
    mod_name: &str,
    replace: bool,
) -> Result<String, String> {
    write_edits(
        request,
        mod_name,
        replace,
        PackageEdits {
            add_exports: vec![add],
            ..Default::default()
        },
    )
}

/// Adds a component to a Blueprint by duplicating one and writes the result into a mod pak.
pub fn add_component(
    request: &Request<'_>,
    add: rivals_uasset::AddComponent,
    mod_name: &str,
    replace: bool,
) -> Result<String, String> {
    write_edits(
        request,
        mod_name,
        replace,
        PackageEdits {
            add_components: vec![add],
            ..Default::default()
        },
    )
}

/// Takes a component out of a Blueprint and writes the result into a mod pak.
pub fn remove_component(
    request: &Request<'_>,
    remove: rivals_uasset::RemoveComponent,
    mod_name: &str,
    replace: bool,
) -> Result<String, String> {
    write_edits(
        request,
        mod_name,
        replace,
        PackageEdits {
            remove_components: vec![remove],
            ..Default::default()
        },
    )
}

/// Writes the asset, unchanged but for its name, under another package name.
pub fn save_as(
    request: &Request<'_>,
    to: &str,
    mod_name: &str,
    replace: bool,
) -> Result<String, String> {
    write_edits(
        &Request {
            save_as: Some(to),
            ..*request
        },
        mod_name,
        replace,
        PackageEdits::default(),
    )
}

/// Moves a package a mod added to another path inside that mod, the mod being the container it is
/// read from.
pub fn rename_package(request: &Request<'_>, to: &str) -> Result<String, String> {
    let mod_name = Path::new(request.container)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|_| source_of(request.container) == AssetSource::Utoc)
        .ok_or("read the package from the mod's own .utoc to rename it there")?;
    let schema = mappings::resolve(request.usmap, request.configured_usmap)
        .and_then(|path| mappings::load(&path))
        .ok();
    let changes = PackageEdits::default();
    match asset_edit::rename_mod_package(
        &edit_request(
            &Request {
                save_as: Some(to),
                ..*request
            },
            mod_name,
            changes,
        ),
        schema.as_deref(),
        &asset_edit::SaveOptions {
            keep_referencers: request.keep_referencers,
            ..Default::default()
        },
    )? {
        asset_edit::SaveOutcome::Written {
            message, warnings, ..
        } => Ok(with_warnings(message, &warnings)),
        asset_edit::SaveOutcome::HoldsCopy { pak } => Err(format!("{pak} already holds {to}")),
    }
}

/// Adds, copies, renames or removes a DataTable row and writes the result into a mod pak.
pub fn row(
    request: &Request<'_>,
    edit: rivals_uasset::RowEdit,
    mod_name: &str,
    replace: bool,
) -> Result<String, String> {
    write_edits(
        request,
        mod_name,
        replace,
        PackageEdits {
            rows: vec![edit],
            ..Default::default()
        },
    )
}

/// Changes, adds or removes a StringTable entry and writes the result into a mod pak.
pub fn strings(
    request: &Request<'_>,
    edit: rivals_uasset::StringEdit,
    mod_name: &str,
    replace: bool,
) -> Result<String, String> {
    write_edits(
        request,
        mod_name,
        replace,
        PackageEdits {
            strings: vec![edit],
            ..Default::default()
        },
    )
}

pub fn keys(
    request: &Request<'_>,
    edit: rivals_uasset::KeyEdit,
    mod_name: &str,
    replace: bool,
) -> Result<String, String> {
    write_edits(
        request,
        mod_name,
        replace,
        PackageEdits {
            keys: vec![edit],
            ..Default::default()
        },
    )
}

/// Copies an export and its subobjects to the end of the table under `name`, writing the result
/// into a mod pak.
pub fn duplicate_export(
    request: &Request<'_>,
    export: u32,
    name: &str,
    into_level: Option<u32>,
    mod_name: &str,
    replace: bool,
) -> Result<String, String> {
    write_edits(
        request,
        mod_name,
        replace,
        PackageEdits {
            duplicate_exports: vec![rivals_uasset::DuplicateExport {
                export,
                name: name.to_string(),
                into_level,
            }],
            ..Default::default()
        },
    )
}

/// The bytes an export carries after its properties.
pub fn payload_bytes(request: &Request<'_>, export: u32) -> Result<Vec<u8>, String> {
    let schema = mappings::resolve(request.usmap, request.configured_usmap)
        .and_then(|path| mappings::load(&path))
        .ok();
    asset_edit::read_payload(
        &edit_request(request, "", PackageEdits::default()),
        schema.as_deref(),
        export,
    )
}

/// Sets one literal constant inside a function's bytecode and writes the result into a mod.
pub fn script_set(
    request: &Request<'_>,
    edit: rivals_uasset::ScriptConstEdit,
    mod_name: &str,
    replace: bool,
) -> Result<String, String> {
    write_edits(
        request,
        mod_name,
        replace,
        PackageEdits {
            scripts: vec![edit],
            ..Default::default()
        },
    )
}

/// The edits that widen every literal in `export`'s script, or in every script of the package.
fn widening(request: &Request<'_>, export: Option<u32>) -> Result<PackageEdits, String> {
    let parsed = parse(request)?;
    let scripts = rivals_uasset::widening_edits(&parsed, export, false);
    if scripts.is_empty() {
        return Err("no literal there has a wider form, or the script has to keep its size".into());
    }
    Ok(PackageEdits {
        scripts,
        ..Default::default()
    })
}

/// Widens every literal in a function, or in every function of the package, into a mod.
pub fn script_widen(
    request: &Request<'_>,
    export: Option<u32>,
    mod_name: &str,
    replace: bool,
) -> Result<String, String> {
    write_edits(request, mod_name, replace, widening(request, export)?)
}

/// What `script_widen` would change, patched and verified in memory without writing anything.
pub fn preview_script_widen(
    request: &Request<'_>,
    export: Option<u32>,
    mod_name: &str,
    replace: bool,
) -> Result<Vec<rivals_uasset::AppliedEdit>, String> {
    let changes = widening(request, export)?;
    Ok(preview_edits(request, mod_name, replace, changes)?.applied)
}

/// Writes a save of its own into a mod: a function's script from text, a new function, a new
/// variable.
pub fn save_changes(
    request: &Request<'_>,
    changes: PackageEdits,
    mod_name: &str,
    replace: bool,
) -> Result<String, String> {
    write_edits(request, mod_name, replace, changes)
}

/// What `script_set` would change, patched and verified in memory without writing anything.
pub fn preview_script_set(
    request: &Request<'_>,
    edit: rivals_uasset::ScriptConstEdit,
    mod_name: &str,
    replace: bool,
) -> Result<Vec<rivals_uasset::AppliedEdit>, String> {
    let changes = PackageEdits {
        scripts: vec![edit],
        ..Default::default()
    };
    Ok(preview_edits(request, mod_name, replace, changes)?.applied)
}

/// The bytes of one bulk data resource, wherever its payload sits.
pub fn bulk_bytes(request: &Request<'_>, resource: u32) -> Result<Vec<u8>, String> {
    asset_edit::read_bulk(
        &edit_request(request, "", PackageEdits::default()),
        resource,
    )
}

/// The bulk data table as the inspector shows it.
pub fn bulk_list(request: &Request<'_>) -> Result<Vec<rivals_uasset::ResourceInfo>, String> {
    Ok(parse(request)?.resources)
}

pub fn replace_payload(
    request: &Request<'_>,
    export: u32,
    bytes: Vec<u8>,
    mod_name: &str,
    replace: bool,
) -> Result<String, String> {
    write_edits(
        request,
        mod_name,
        replace,
        PackageEdits {
            payloads: vec![rivals_uasset::PayloadEdit { export, bytes }],
            ..Default::default()
        },
    )
}

pub fn replace_bulk(
    request: &Request<'_>,
    resource: u32,
    bytes: Vec<u8>,
    mod_name: &str,
    replace: bool,
) -> Result<String, String> {
    write_edits(
        request,
        mod_name,
        replace,
        PackageEdits {
            bulk: vec![rivals_uasset::BulkEdit { resource, bytes }],
            ..Default::default()
        },
    )
}

/// The packages importing an object or a package, from the import index.
pub fn importers(
    game_root: &str,
    path: &str,
    build: bool,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<rivals_core::import_index::Importers, String> {
    let index = match (build, rivals_core::import_index::load(game_root)?) {
        (false, Some(index)) => index,
        (false, None) => {
            return Err(format!(
                "no import index has been built yet (it would live at {}); pass --build to create one",
                rivals_core::import_index::cache_path(game_root)?.display()
            ));
        }
        (true, _) => rivals_core::import_index::build(game_root, progress)?,
    };
    if index.is_stale(game_root) {
        eprintln!("warning: {}", rivals_core::import_index::STALE_WARNING);
    }
    Ok(index.importers_of(path))
}

pub fn print_plan(plan: &RemovalPlan, out: &mut impl FnMut(String)) {
    out(format!("removing {} export(s):", plan.removed.len()));
    for export in &plan.removed {
        out(format!(
            "  [{}] {:<28} {}{}",
            export.index,
            export.class_name,
            export.path,
            if export.requested {
                ""
            } else {
                "  (subobject)"
            }
        ));
    }
    if !plan.cleared.is_empty() {
        out(format!("references set to None ({}):", plan.cleared.len()));
        for cleared in &plan.cleared {
            out(format!(
                "  {}.{} -> {}",
                cleared.export_name, cleared.property, cleared.target
            ));
        }
    }
    out(match plan.renumbered {
        0 => "no other export is renumbered".to_string(),
        moved => format!("{moved} later export(s) are renumbered"),
    });
    for importers in &plan.importers {
        out(match importers.packages.len() {
            0 => format!("no indexed package imports {}", importers.path),
            count => format!("{} is imported by {count} package(s):", importers.path),
        });
        for package in &importers.packages {
            out(format!("  {package}"));
        }
    }
    for blocker in &plan.blockers {
        out(format!("blocked: {blocker}"));
    }
    for warning in &plan.warnings {
        out(format!("warning: {warning}"));
    }
}

fn write_edits(
    request: &Request<'_>,
    mod_name: &str,
    replace: bool,
    changes: PackageEdits,
) -> Result<String, String> {
    let schema = mappings::resolve(request.usmap, request.configured_usmap)
        .and_then(|path| mappings::load(&path))
        .ok();
    match asset_edit::save_edits(
        &edit_request(request, mod_name, changes),
        schema.as_deref(),
        &asset_edit::SaveOptions {
            replace,
            layer: request.layer,
            target: request.target,
            keep_referencers: request.keep_referencers,
            ..Default::default()
        },
    )? {
        asset_edit::SaveOutcome::Written {
            message, warnings, ..
        } => Ok(with_warnings(message, &warnings)),
        asset_edit::SaveOutcome::HoldsCopy { pak } => Err(format!(
            "{pak} already holds an edited copy of {}; pass --replace to overwrite it",
            request.entry
        )),
    }
}

/// Sets the same properties across every package a filter matches, in one container rewrite.
pub fn sweep(
    usmap: Option<&str>,
    configured_usmap: Option<&str>,
    target: asset_edit::SaveTarget,
    request: &asset_edit::sweep::SweepRequest<'_>,
) -> Result<asset_edit::sweep::SweepReport, String> {
    let schema = mappings::resolve(usmap, configured_usmap)
        .and_then(|path| mappings::load(&path))
        .ok();
    asset_edit::sweep::sweep(
        request,
        schema.as_deref(),
        &asset_edit::SaveOptions {
            target,
            ..Default::default()
        },
    )
}

pub fn print_sweep(report: &asset_edit::sweep::SweepReport, out: &mut impl FnMut(String)) {
    for package in &report.changed {
        out(package.entry.clone());
        for change in &package.changes {
            out(format!("    {change}"));
        }
    }
    for failure in &report.failed {
        out(format!("!! {}: {}", failure.entry, failure.reason));
    }
    if !report.skipped.unsupported.is_empty() {
        out(String::new());
        out("matched by name, of a kind the sweep does not write:".into());
        for (what, count) in &report.skipped.unsupported {
            out(format!("    {what} x{count}"));
        }
    }
    if !report.skipped.inherited.is_empty() {
        out(String::new());
        out("matched by name, stored nowhere and left to the archetype:".into());
        for (name, count) in &report.skipped.inherited {
            out(format!("    {name} x{count}"));
        }
    }
    out(String::new());
    let not_that_class = if report.other_class > 0 {
        format!(", {} not of that class", report.other_class)
    } else {
        String::new()
    };
    out(format!(
        "{} package(s) matched{not_that_class}, {} changed, {} left alone, {} failed, {} value(s) set",
        report.matched,
        report.changed.len(),
        report.untouched,
        report.failed.len(),
        report.edits()
    ));
    if report.replaced > 0 {
        if report.layered {
            out(format!(
                "{} package(s) were built on the copies this mod already held.",
                report.replaced
            ));
        } else {
            out(format!(
                "! {} package(s) already in this mod were replaced by copies read fresh",
                report.replaced
            ));
            out("  from the source, so an earlier sweep's edits to them are gone. Pass".into());
            out("  --layer to build on what the mod already holds instead.".into());
        }
    }
    match &report.written {
        Some(utoc) => out(format!(
            "Wrote {} ({} chunk(s) carried over)",
            utoc.display(),
            report.carried_chunks
        )),
        None => out("Nothing written".into()),
    }
}

/// What one item of an apply run did. A failure is an outcome rather than an early return, so one
/// bad file does not strand the rest of a batch half applied.
#[derive(Serialize)]
pub struct ApplyItemReport {
    pub container: String,
    pub entry: String,
    pub mod_name: String,
    /// `written`, `verified` for a dry run, `holds_copy`, or `failed`.
    pub outcome: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub applied: Vec<rivals_uasset::AppliedEdit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Serialize)]
pub struct ApplyReport {
    pub items: Vec<ApplyItemReport>,
    pub failed: usize,
}

/// What the command line says regardless of what each item asked for.
pub struct ApplyOverrides<'a> {
    pub mod_name: Option<&'a str>,
    pub replace: bool,
    pub layer: bool,
    /// Apply items whose package no longer matches what they expected.
    pub allow_drift: bool,
    /// Save imports that point at nothing the game or an enabled mod has.
    pub allow_missing: bool,
    /// Save script edits that point at something whose kind could not be confirmed.
    pub allow_unchecked: bool,
    /// Patch and verify every item, then write nothing.
    pub dry_run: bool,
    /// What to write, when the command line said. An item's own target wins otherwise.
    pub target: Option<asset_edit::SaveTarget>,
}

/// Applies edit files. Items naming the same package for the same mod are merged into one patch,
/// since they were written against the same source, and every mod is written with one container
/// rewrite. Each item still reports whether its package landed.
pub fn apply(
    game_root: &str,
    items: &[(std::path::PathBuf, asset_edit::json::EditFile)],
    overrides: &ApplyOverrides<'_>,
    usmap: Option<&str>,
    configured_usmap: Option<&str>,
    default_mod: &str,
) -> ApplyReport {
    let mut reports: Vec<ApplyItemReport> = items
        .iter()
        .map(|(_, file)| ApplyItemReport {
            container: file.container.clone(),
            entry: file.entry.clone(),
            mod_name: overrides
                .mod_name
                .or(file.mod_name.as_deref())
                .unwrap_or(default_mod)
                .to_string(),
            outcome: "failed",
            applied: Vec::new(),
            message: None,
        })
        .collect();
    let schema = mappings::resolve(usmap, configured_usmap)
        .and_then(|path| mappings::load(&path))
        .ok();

    let mut groups: Vec<ApplyGroup> = Vec::new();
    for (index, (base, file)) in items.iter().enumerate() {
        match resolve_item(game_root, base, file, overrides) {
            Ok((key, changes)) => {
                let key = ApplyKey {
                    mod_name: reports[index].mod_name.clone(),
                    ..key
                };
                match groups.iter_mut().find(|group| group.key == key) {
                    Some(group) => {
                        group.changes.merge(changes);
                        group.items.push(index);
                    }
                    None => groups.push(ApplyGroup {
                        key,
                        changes,
                        items: vec![index],
                    }),
                }
            }
            Err(reason) => reports[index].message = Some(reason),
        }
    }

    let requests: Vec<AssetEditRequest<'_>> = groups
        .iter()
        .map(|group| AssetEditRequest {
            game_root,
            container: &group.key.container,
            entry: &group.key.entry,
            kind: if group.key.container.is_empty() {
                AssetSource::Loose
            } else {
                source_of(&group.key.container)
            },
            mod_name: &group.key.mod_name,
            changes: group.changes.clone(),
        })
        .collect();
    let mut outcomes: Vec<Option<Result<ApplyOutcome, String>>> =
        groups.iter().map(|_| None).collect();
    if overrides.dry_run {
        for (at, request) in requests.iter().enumerate() {
            let key = &groups[at].key;
            let options = asset_edit::SaveOptions {
                replace: key.replace,
                layer: key.layer,
                target: key.target,
                ..Default::default()
            };
            outcomes[at] = Some(
                asset_edit::preview_save(request, schema.as_deref(), &options).map(|outcome| {
                    match outcome {
                        asset_edit::PreviewOutcome::Verified { applied, notes, .. } => (
                            "verified",
                            applied,
                            (!notes.is_empty()).then(|| {
                                notes
                                    .iter()
                                    .map(|note| format!("note: {note}"))
                                    .collect::<Vec<_>>()
                                    .join("
")
                            }),
                        ),
                        asset_edit::PreviewOutcome::HoldsCopy { pak } => (
                            "holds_copy",
                            Vec::new(),
                            Some(format!(
                                "{pak} already holds an edited copy of {}; pass --layer to build on it or --replace to start over",
                                key.entry
                            )),
                        ),
                    }
                }),
            );
        }
    } else {
        // One batch per mod and way of writing, so each container is rewritten once.
        let mut done = vec![false; groups.len()];
        for first in 0..groups.len() {
            if done[first] {
                continue;
            }
            let members: Vec<usize> = (first..groups.len())
                .filter(|&at| !done[at] && groups[at].key.same_save(&groups[first].key))
                .collect();
            let options = asset_edit::SaveOptions {
                replace: groups[first].key.replace,
                layer: groups[first].key.layer,
                target: groups[first].key.target,
                ..Default::default()
            };
            let batch: Vec<AssetEditRequest<'_>> = members
                .iter()
                .map(|&at| AssetEditRequest {
                    changes: requests[at].changes.clone(),
                    ..requests[at]
                })
                .collect();
            let results = asset_edit::save_batch(&batch, schema.as_deref(), &options);
            for (&at, result) in members.iter().zip(results) {
                done[at] = true;
                outcomes[at] = Some(result.map(|outcome| match outcome {
                    asset_edit::SaveOutcome::Written {
                        message, warnings, ..
                    } => ("written", Vec::new(), Some(with_warnings(message, &warnings))),
                    asset_edit::SaveOutcome::HoldsCopy { pak } => (
                        "holds_copy",
                        Vec::new(),
                        Some(format!(
                            "{pak} already holds an edited copy of {}; pass --layer to build on it or --replace to start over",
                            groups[at].key.entry
                        )),
                    ),
                }));
            }
        }
    }

    for (group, outcome) in groups.iter().zip(outcomes) {
        let merged = group.items.len() > 1;
        for &index in &group.items {
            let report = &mut reports[index];
            match &outcome {
                Some(Ok((kind, applied, message))) => {
                    report.outcome = kind;
                    report.applied.clone_from(applied);
                    report.message = match (message, merged) {
                        (Some(message), true) => Some(format!(
                            "{message} (merged with {} other item(s) for this package)",
                            group.items.len() - 1
                        )),
                        (message, _) => message.clone(),
                    };
                }
                Some(Err(reason)) => report.message = Some(reason.clone()),
                None => report.message = Some("not attempted".to_string()),
            }
        }
    }
    let failed = reports
        .iter()
        .filter(|item| item.outcome == "failed")
        .count();
    ApplyReport {
        items: reports,
        failed,
    }
}

type ApplyOutcome = (
    &'static str,
    Vec<rivals_uasset::AppliedEdit>,
    Option<String>,
);

/// What makes two items land in the same patch: the same package, read the same way, written into
/// the same mod the same way.
#[derive(PartialEq)]
struct ApplyKey {
    mod_name: String,
    container: String,
    entry: String,
    target: asset_edit::SaveTarget,
    replace: bool,
    layer: bool,
}

impl ApplyKey {
    /// Whether two packages can share one container rewrite.
    fn same_save(&self, other: &ApplyKey) -> bool {
        self.mod_name == other.mod_name
            && self.target == other.target
            && self.replace == other.replace
            && self.layer == other.layer
    }
}

struct ApplyGroup {
    key: ApplyKey,
    changes: PackageEdits,
    items: Vec<usize>,
}

fn resolve_item(
    game_root: &str,
    base: &Path,
    file: &asset_edit::json::EditFile,
    overrides: &ApplyOverrides<'_>,
) -> Result<(ApplyKey, PackageEdits), String> {
    if file.edits.is_empty() {
        return Err("this edit file changes nothing".to_string());
    }
    let container = asset_edit::json::resolve_container(&file.container, base, game_root)?;
    let mut changes = file.edits.clone().resolve(base)?;
    changes.allow_drift = overrides.allow_drift;
    changes.allow_missing = overrides.allow_missing;
    changes.allow_unchecked = overrides.allow_unchecked;
    Ok((
        ApplyKey {
            mod_name: String::new(),
            container,
            entry: file.entry.clone(),
            target: overrides.target.or(file.target).unwrap_or_default(),
            replace: overrides.replace || file.replace,
            layer: overrides.layer || file.layer,
        },
        changes,
    ))
}

pub fn print_apply(report: &ApplyReport, out: &mut impl FnMut(String)) {
    for item in &report.items {
        let head = format!("{:<10} {} -> {}", item.outcome, item.entry, item.mod_name);
        out(head);
        if let Some(message) = &item.message {
            out(format!("           {message}"));
        }
        for applied in &item.applied {
            out(format!(
                "           {} {} -> {}",
                applied.name, applied.before, applied.after
            ));
        }
    }
    out(String::new());
    out(format!(
        "{} item(s), {} failed",
        report.items.len(),
        report.failed
    ));
}

/// A save's message with the other mods overriding the same asset listed under it.
fn with_warnings(message: String, warnings: &[String]) -> String {
    warnings
        .iter()
        .fold(message, |out, warning| format!("{out}\nwarning: {warning}"))
}

pub fn list(game_root: &str, container: &str, filter: Option<&str>) -> Result<Vec<String>, String> {
    let entries = match source_of(container) {
        AssetSource::Utoc => asset::list_packages(game_root, container)?
            .1
            .into_iter()
            .map(|(_, path)| path)
            .collect(),
        AssetSource::Pak | AssetSource::Loose => asset::list_package_entries(container)?,
    };
    let filter = asset::PathFilter::new(filter);
    let mut paths: Vec<String> = entries
        .into_iter()
        .filter(|path| filter.matches(path))
        .collect();
    paths.sort();
    Ok(paths)
}

#[derive(Serialize)]
pub struct InfoReport {
    #[serde(flatten)]
    info: PackageInfo,
    imports: Vec<rivals_uasset::ImportInfo>,
    exports: Vec<ExportSummary>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    schema_fixups: Vec<rivals_uasset::AppliedFixup>,
    twins: Vec<rivals_uasset::TwinChoice>,
    /// How many instanced struct payloads with contents the package holds, each guarded by a
    /// byte length.
    instanced_structs: usize,
    /// How many of those did not decode. Their exports still read as exact, because the length
    /// prefix puts the cursor back, so this is the only figure that shows them.
    undecoded_payloads: usize,
}

#[derive(Serialize)]
struct ExportSummary {
    index: u32,
    object_name: String,
    class_name: String,
    serial_size: i64,
    status: ExportStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    undecoded: Vec<rivals_uasset::UndecodedPayload>,
}

pub fn info(request: &Request<'_>) -> Result<InfoReport, String> {
    let parsed = parse(request)?;
    Ok(InfoReport {
        exports: parsed
            .exports
            .iter()
            .map(|e| ExportSummary {
                index: e.index,
                object_name: e.object_name.clone(),
                class_name: e.class_name.clone(),
                serial_size: e.serial_size,
                status: e.status.clone(),
                note: e.note.clone(),
                undecoded: e.undecoded.clone(),
            })
            .collect(),
        schema_fixups: parsed.schema_fixups,
        twins: parsed.twins,
        instanced_structs: parsed
            .instanced
            .iter()
            .filter(|layout| layout.payload_end > layout.payload_start)
            .count(),
        undecoded_payloads: parsed.exports.iter().map(|e| e.undecoded.len()).sum(),
        imports: parsed.imports,
        info: parsed.info,
    })
}

pub fn print_info(report: &InfoReport, out: &mut impl FnMut(String)) {
    out(format!("package  {}", report.info.package_name));
    out(format!(
        "flags    cooked={} unversioned_properties={}",
        report.info.cooked, report.info.unversioned_properties
    ));
    out(format!(
        "counts   {} names, {} imports, {} exports",
        report.info.name_count, report.info.import_count, report.info.export_count
    ));
    if report.undecoded_payloads > 0 {
        out(format!(
            "undecoded {} of {} instanced struct payload(s) did not decode and are kept as bytes",
            report.undecoded_payloads, report.instanced_structs
        ));
    }
    for choice in &report.twins {
        out(format!(
            "twin     {} read as mappings entry {} of the {} that share the name",
            choice.name, choice.entry, choice.of
        ));
    }
    for fixup in &report.schema_fixups {
        out(format!(
            "elided   {}.{} (slot {}), which the mappings declare but this build does not serialize",
            fixup.struct_name, fixup.property, fixup.slot
        ));
    }
    out(String::new());
    let class_width = report
        .imports
        .iter()
        .map(|i| i.class_name.len())
        .max()
        .unwrap_or(5);
    for import in &report.imports {
        let note = rivals_uasset::unresolved_import_note(&import.object_name)
            .map(|note| format!("  [{note}]"))
            .unwrap_or_default();
        out(format!(
            "  import {:>4}  {:class_width$}  {}{note}",
            import.index, import.class_name, import.path
        ));
    }
    if !report.imports.is_empty() {
        out(String::new());
    }
    let width = report
        .exports
        .iter()
        .map(|e| e.class_name.len())
        .max()
        .unwrap_or(5);
    for export in &report.exports {
        out(format!(
            "  [{}] {:width$}  {:>9}  {}  {}",
            export.index,
            export.class_name,
            export.serial_size,
            status_label(&export.status),
            export.object_name,
        ));
        if let Some(note) = &export.note {
            out(format!("        ! layout walk stopped: {note}"));
        }
        if !export.undecoded.is_empty() {
            out(format!(
                "        ! {} instanced payload(s) did not decode",
                export.undecoded.len()
            ));
        }
    }
}

/// Changes an export's row in the table and writes the result into a mod.
pub fn export_edit(
    request: &Request<'_>,
    edits: Vec<rivals_uasset::ExportEdit>,
    resets: Vec<u32>,
    mod_name: &str,
    replace: bool,
) -> Result<String, String> {
    write_edits(
        request,
        mod_name,
        replace,
        PackageEdits {
            exports: edits,
            reset_exports: resets,
            ..Default::default()
        },
    )
}

/// What those changes would do. Nothing is written.
pub fn plan_export_edits(
    request: &Request<'_>,
    edits: &[rivals_uasset::ExportEdit],
    resets: &[u32],
) -> Result<rivals_uasset::ExportEditPlan, String> {
    let schema = mappings::resolve(request.usmap, request.configured_usmap)
        .and_then(|path| mappings::load(&path))
        .ok();
    asset_edit::plan_export_edits(
        &edit_request(
            request,
            "",
            PackageEdits {
                exports: edits.to_vec(),
                reset_exports: resets.to_vec(),
                ..Default::default()
            },
        ),
        schema.as_deref(),
    )
}

pub fn print_export_plan(plan: &rivals_uasset::ExportEditPlan, out: &mut impl FnMut(String)) {
    for (was, now) in &plan.repathed {
        out(format!("repath   {was} -> {now}"));
    }
    for path in &plan.public {
        out(format!(
            "public   {path} is imported by hash, which its path decides; other packages will not follow"
        ));
    }
    for warning in &plan.warnings {
        out(format!("warning: {warning}"));
    }
    for blocker in &plan.blockers {
        out(format!("blocked: {blocker}"));
    }
}

/// One import with what names it, so a table can be tidied without reading all of it.
#[derive(Serialize)]
pub struct ImportRow {
    pub index: i32,
    pub class_name: String,
    pub path: String,
    #[serde(flatten)]
    pub usage: rivals_uasset::ImportUsage,
    /// Present only when the import is named by nothing at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removable: Option<bool>,
    /// Present only when retoc could not resolve the import; says what it was.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unresolved: Option<String>,
}

#[derive(Serialize)]
pub struct ImportsReport {
    pub imports: Vec<ImportRow>,
    pub unused: usize,
}

/// Every import and what uses it. With `unused_only`, just the ones nothing names.
pub fn imports(request: &Request<'_>, unused_only: bool) -> Result<ImportsReport, String> {
    let parsed = parse(request)?;
    let loaded = asset::load_bundle(
        request.game_root,
        request.container,
        request.entry,
        kind_of(request),
    )?;
    let header = rivals_uasset::read_header(&AssetBundle {
        asset: &loaded.asset_file_buffer,
        exports: &loaded.exports_file_buffer,
    })?;
    let unused = parsed
        .imports
        .iter()
        .filter(|info| info.usage.unused())
        .count();
    let removable: std::collections::BTreeMap<i32, bool> =
        rivals_uasset::unused_imports(&parsed, &header)
            .into_iter()
            .map(|import| (import.index, import.blocked.is_none()))
            .collect();
    let imports = parsed
        .imports
        .iter()
        .filter(|info| !unused_only || info.usage.unused())
        .map(|info| ImportRow {
            index: info.index,
            class_name: info.class_name.clone(),
            path: info.path.clone(),
            removable: info
                .usage
                .unused()
                .then(|| removable.get(&info.index).copied().unwrap_or(false)),
            unresolved: rivals_uasset::unresolved_import_note(&info.object_name),
            usage: info.usage.clone(),
        })
        .collect();
    Ok(ImportsReport { imports, unused })
}

pub fn print_imports(report: &ImportsReport, out: &mut impl FnMut(String)) {
    let width = report
        .imports
        .iter()
        .map(|row| row.class_name.len())
        .max()
        .unwrap_or(5);
    for row in &report.imports {
        let mut used: Vec<String> = Vec::new();
        if row.usage.references > 0 {
            used.push(format!("{} reference(s)", row.usage.references));
        }
        for role in &row.usage.roles {
            used.push((*role).to_string());
        }
        if !row.usage.outer_of.is_empty() {
            used.push(format!("outer of {} import(s)", row.usage.outer_of.len()));
        }
        if row.usage.preload > 0 {
            used.push(format!("{} dependency(s)", row.usage.preload));
        }
        if row.usage.resources > 0 {
            used.push(format!("{} bulk resource(s)", row.usage.resources));
        }
        let note = match (used.is_empty(), row.removable) {
            (true, Some(true)) => "unused, removable".to_string(),
            (true, Some(false)) => "unused, but something blocks removing it".to_string(),
            _ => used.join(", "),
        };
        let note = match (&row.unresolved, note.is_empty()) {
            (Some(unresolved), true) => unresolved.clone(),
            (Some(unresolved), false) => format!("{unresolved}, {note}"),
            (None, _) => note,
        };
        out(format!(
            "  {:>4}  {:width$}  {}  [{note}]",
            row.index, row.class_name, row.path
        ));
    }
    out(String::new());
    out(format!(
        "{} import(s), {} named by nothing",
        report.imports.len(),
        report.unused
    ));
}

/// Drops imports and writes the result into a mod, which is a save of its own.
pub fn import_remove(
    request: &Request<'_>,
    imports: &[u32],
    mod_name: &str,
    replace: bool,
) -> Result<String, String> {
    write_edits(
        request,
        mod_name,
        replace,
        PackageEdits {
            imports: imports
                .iter()
                .map(|&import| rivals_uasset::ImportEdit::Remove { import })
                .collect(),
            ..Default::default()
        },
    )
}

/// Compares an edited dump against the package it came from and reports the edit file that would
/// reproduce it. Nothing is written by this call; the file goes wherever the caller puts it.
pub fn diff(
    request: &Request<'_>,
    edited: &Path,
    mod_name: Option<&str>,
    default_mod_name: &str,
    target: asset_edit::SaveTarget,
) -> Result<asset_edit::json::EditFile, String> {
    let text =
        std::fs::read_to_string(edited).map_err(|e| format!("read {}: {e}", edited.display()))?;
    let json: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("{} is not JSON: {e}", edited.display()))?;
    // The editor's parse, whatever the caller asked for: an edit that stores an unset value is
    // addressed by the offset that slot would occupy, and only this parse records it.
    let declared = Request {
        declared: true,
        ..*request
    };
    // A layered save builds on the mod's own copy where it holds one, so the edits are addressed
    // into that copy rather than the source.
    let into = mod_name.unwrap_or(default_mod_name);
    let layered = asset_edit::layered_read(
        &edit_request(request, into, PackageEdits::default()),
        &asset_edit::SaveOptions {
            layer: request.layer,
            target,
            ..Default::default()
        },
    )?;
    let parsed = match &layered {
        Some((container, entry, _)) => parse(&Request {
            container: container.as_str(),
            entry: entry.as_str(),
            ..declared
        })?,
        None => parse(&declared)?,
    };
    let outcome = asset_edit::diff::diff_dump(&parsed, &json)?;
    Ok(asset_edit::json::EditFile {
        container: request.container.to_string(),
        entry: request.entry.to_string(),
        // The copy the edits were addressed into is the one they have to land on.
        mod_name: match layered {
            Some(_) => Some(into.to_string()),
            None => mod_name.map(str::to_string),
        },
        target: Some(target),
        replace: false,
        layer: request.layer,
        notes: outcome.notes,
        edits: outcome.edits,
    })
}

pub fn print_diff(file: &asset_edit::json::EditFile, out: &mut impl FnMut(String)) {
    let edits = &file.edits;
    let counts = [
        ("value", edits.values.len()),
        ("import", edits.imports.len()),
        ("row", edits.rows.len()),
        ("string", edits.strings.len()),
        ("key", edits.keys.len()),
    ];
    for (what, count) in counts {
        if count > 0 {
            out(format!("{count} {what} edit(s)"));
        }
    }
    if edits.is_empty() {
        out("no edits: the dump reads the same as the package".to_string());
    }
    for note in &file.notes {
        out(format!("note: {note}"));
    }
}

/// One export's preload dependency runs, and the paths they name.
#[derive(Serialize)]
pub struct DependencyReport {
    pub export: u32,
    pub path: String,
    pub runs: rivals_uasset::Runs,
    /// Each run's entries as object paths, in the same order.
    pub named: Vec<Vec<String>>,
}

/// The runs `export` declares now.
pub fn dependencies(request: &Request<'_>, export: u32) -> Result<DependencyReport, String> {
    let parsed = parse(request)?;
    let loaded = asset::load_bundle(
        request.game_root,
        request.container,
        request.entry,
        kind_of(request),
    )?;
    let header = rivals_uasset::read_header(&AssetBundle {
        asset: &loaded.asset_file_buffer,
        exports: &loaded.exports_file_buffer,
    })?;
    let runs = rivals_uasset::runs_of(&header)?
        .get(export as usize)
        .cloned()
        .ok_or_else(|| format!("this package has no export {export}"))?;
    let held = &parsed
        .exports
        .get(export as usize)
        .ok_or_else(|| format!("this package has no export {export}"))?;
    // A package index is the export's position plus one, or minus the import's position plus one.
    let name = |index: i32| {
        match index.cmp(&0) {
            std::cmp::Ordering::Greater => parsed
                .exports
                .get((index - 1) as usize)
                .map(|e| e.path.clone()),
            std::cmp::Ordering::Less => parsed
                .imports
                .get((-index - 1) as usize)
                .map(|i| i.path.clone()),
            std::cmp::Ordering::Equal => None,
        }
        .unwrap_or_else(|| index.to_string())
    };
    let named = [
        &runs.serialize_before_serialize,
        &runs.create_before_serialize,
        &runs.serialize_before_create,
        &runs.create_before_create,
    ]
    .iter()
    .map(|run| run.iter().map(|&index| name(index)).collect())
    .collect();
    Ok(DependencyReport {
        export,
        path: held.path.clone(),
        runs,
        named,
    })
}

pub fn print_dependencies(report: &DependencyReport, out: &mut impl FnMut(String)) {
    out(format!("[{}] {}", report.export, report.path));
    let labels = [
        "serialize before serialize",
        "create before serialize",
        "serialize before create",
        "create before create",
    ];
    let runs = [
        &report.runs.serialize_before_serialize,
        &report.runs.create_before_serialize,
        &report.runs.serialize_before_create,
        &report.runs.create_before_create,
    ];
    for ((label, run), named) in labels.iter().zip(runs).zip(&report.named) {
        if run.is_empty() {
            out(format!("  {label}: none"));
            continue;
        }
        out(format!("  {label}:"));
        for (index, path) in run.iter().zip(named) {
            out(format!("    {index:>6}  {path}"));
        }
    }
}

/// Replaces one export's runs and writes the result into a mod, which is a save of its own.
pub fn set_dependencies(
    request: &Request<'_>,
    export: u32,
    runs: rivals_uasset::Runs,
    mod_name: &str,
    replace: bool,
) -> Result<String, String> {
    write_edits(
        request,
        mod_name,
        replace,
        PackageEdits {
            dependencies: vec![rivals_uasset::DependencyEdit { export, runs }],
            ..Default::default()
        },
    )
}

/// What replacing those runs would do. Nothing is written.
pub fn plan_dependencies(
    request: &Request<'_>,
    export: u32,
    runs: rivals_uasset::Runs,
) -> Result<rivals_uasset::DependencyPlan, String> {
    let parsed = parse(request)?;
    let loaded = asset::load_bundle(
        request.game_root,
        request.container,
        request.entry,
        kind_of(request),
    )?;
    let header = rivals_uasset::read_header(&AssetBundle {
        asset: &loaded.asset_file_buffer,
        exports: &loaded.exports_file_buffer,
    })?;
    let mut plan = rivals_uasset::plan_dependency_edits(
        &parsed,
        &header,
        &[rivals_uasset::DependencyEdit {
            export,
            runs: runs.clone(),
        }],
    )?;
    if request.target == asset_edit::SaveTarget::IoStore {
        plan.blockers
            .extend(rivals_uasset::zen_losses(&parsed, export, &runs));
    }
    Ok(plan)
}

pub fn print_dependency_plan(plan: &rivals_uasset::DependencyPlan, out: &mut impl FnMut(String)) {
    for warning in &plan.warnings {
        out(format!("warning: {warning}"));
    }
    for blocker in &plan.blockers {
        out(format!("blocked: {blocker}"));
    }
    if plan.blockers.is_empty() {
        out("the runs are consistent and the load order stays walkable".to_string());
    }
}

/// One request to copy an export in from another package.
pub struct CopyArgs<'a> {
    pub from_container: &'a str,
    pub from_entry: &'a str,
    pub export: u32,
    pub into_outer: Option<u32>,
    pub name: &'a str,
    /// A level to list the copy in, so a copied actor actually spawns.
    pub into_level: Option<u32>,
}

fn copy_request<'a>(
    request: &'a Request<'a>,
    args: &'a CopyArgs<'a>,
    mod_name: &'a str,
    from: &'a asset_edit::CopyFrom,
) -> asset_edit::CopyRequest<'a> {
    asset_edit::CopyRequest {
        game_root: request.game_root,
        container: request.container,
        entry: request.entry,
        kind: kind_of(request),
        mod_name,
        sources: vec![from.clone()],
        copies: vec![rivals_uasset::CopyExport {
            from: from.key(),
            export: args.export,
            into_outer: args.into_outer,
            name: args.name.to_string(),
            into_level: args.into_level,
        }],
    }
}

/// What the copy would bring across. Nothing is written.
pub fn plan_copy(
    request: &Request<'_>,
    args: &CopyArgs<'_>,
) -> Result<rivals_uasset::CopyPlan, String> {
    let schema = mappings::resolve(request.usmap, request.configured_usmap)
        .and_then(|path| mappings::load(&path))
        .ok();
    let from = asset_edit::CopyFrom {
        container: args.from_container.to_string(),
        entry: args.from_entry.to_string(),
    };
    asset_edit::plan_copy(&copy_request(request, args, "", &from), schema.as_deref())
}

pub fn print_copy_plan(plan: &rivals_uasset::CopyPlan, out: &mut impl FnMut(String)) {
    for copy in &plan.copies {
        out(format!(
            "copy     [{:>4}] {}  {}{}",
            copy.index,
            copy.path,
            copy.class_name,
            if copy.requested { "" } else { ", subobject" }
        ));
    }
    for import in &plan.imports {
        out(format!("import   {import}"));
    }
    for warning in &plan.warnings {
        out(format!("warning: {warning}"));
    }
    for blocker in &plan.blockers {
        out(format!("blocked: {blocker}"));
    }
}

/// Copies an export in and writes the result into a mod, which is a save of its own.
pub fn copy_export(
    request: &Request<'_>,
    args: &CopyArgs<'_>,
    mod_name: &str,
    replace: bool,
) -> Result<String, String> {
    let schema = mappings::resolve(request.usmap, request.configured_usmap)
        .and_then(|path| mappings::load(&path))
        .ok();
    let from = asset_edit::CopyFrom {
        container: args.from_container.to_string(),
        entry: args.from_entry.to_string(),
    };
    match asset_edit::save_copy(
        &copy_request(request, args, mod_name, &from),
        schema.as_deref(),
        &asset_edit::SaveOptions {
            replace,
            layer: request.layer,
            target: request.target,
            keep_referencers: request.keep_referencers,
            ..Default::default()
        },
    )? {
        asset_edit::SaveOutcome::Written {
            message, warnings, ..
        } => Ok(with_warnings(message, &warnings)),
        asset_edit::SaveOutcome::HoldsCopy { pak } => Err(format!(
            "{pak} already holds an edited copy of {}; pass --replace to overwrite it",
            request.entry
        )),
    }
}

/// What dropping `imports` would do. Nothing is written.
pub fn plan_import_removal(
    request: &Request<'_>,
    imports: &[u32],
) -> Result<rivals_uasset::ImportRemovalPlan, String> {
    let schema = mappings::resolve(request.usmap, request.configured_usmap)
        .and_then(|path| mappings::load(&path))
        .ok();
    asset_edit::plan_import_removal(
        &edit_request(
            request,
            "",
            PackageEdits {
                imports: imports
                    .iter()
                    .map(|&import| rivals_uasset::ImportEdit::Remove { import })
                    .collect(),
                ..Default::default()
            },
        ),
        schema.as_deref(),
    )
}

pub fn print_import_plan(plan: &rivals_uasset::ImportRemovalPlan, out: &mut impl FnMut(String)) {
    for import in &plan.removed {
        let along = if import.requested {
            ""
        } else {
            "  (an outer left with nothing under it)"
        };
        out(format!(
            "remove   {:>4}  {}{along}",
            import.index, import.path
        ));
    }
    if plan.renumbered > 0 {
        out(format!(
            "renumber {} import(s) move down a place",
            plan.renumbered
        ));
    }
    if plan.dropped_dependencies > 0 {
        out(format!(
            "preload  {} dependency entry(s) dropped",
            plan.dropped_dependencies
        ));
    }
    for cleared in &plan.cleared {
        out(format!("clear    {cleared}"));
    }
    for warning in &plan.warnings {
        out(format!("warning: {warning}"));
    }
    for blocker in &plan.blockers {
        out(format!("blocked: {blocker}"));
    }
}

/// The package's name table, in order: a name's index is its place in the list.
pub fn names(request: &Request<'_>) -> Result<Vec<String>, String> {
    let source = if request.container.is_empty() {
        AssetSource::Loose
    } else {
        source_of(request.container)
    };
    let bundle = asset::load_bundle(request.game_root, request.container, request.entry, source)?;
    let header = rivals_uasset::read_header(&AssetBundle {
        asset: &bundle.asset_file_buffer,
        exports: &bundle.exports_file_buffer,
    })?;
    Ok(rivals_uasset::package_names(&header))
}

/// Prints the byte range every property consumed, which is how a desync is located: the first
/// range that does not line up with the next property is where the reader went wrong.
pub fn trace(
    request: &Request<'_>,
    only: Option<u32>,
) -> Result<Vec<rivals_uasset::TraceEntry>, String> {
    let source = if request.container.is_empty() {
        AssetSource::Loose
    } else {
        source_of(request.container)
    };
    let bundle = asset::load_bundle(request.game_root, request.container, request.entry, source)?;
    let schema = mappings::resolve(request.usmap, request.configured_usmap)
        .and_then(|path| mappings::load(&path))
        .ok();
    let (parsed, trace) = schema_synth::parse_package_traced(
        &AssetBundle {
            asset: &bundle.asset_file_buffer,
            exports: &bundle.exports_file_buffer,
        },
        schema.as_deref(),
        &PackageSource {
            game_root: request.game_root,
            container: request.container,
            entry: request.entry,
            kind: source,
        },
    )?;
    let window = only.and_then(|index| {
        let export = parsed.exports.iter().find(|e| e.index == index)?;
        let start = u64::try_from(export.serial_offset).ok()?;
        Some(start..start + u64::try_from(export.serial_size).ok()?)
    });
    Ok(trace
        .into_iter()
        .filter(|e| window.as_ref().is_none_or(|w| w.contains(&e.start)))
        .collect())
}

/// The trace a line per property, indented by depth. A long name pushes its value along rather
/// than being cut.
pub fn print_trace(entries: &[rivals_uasset::TraceEntry], out: &mut impl FnMut(String)) {
    for e in entries {
        out(format!(
            "{:indent$}0x{:<6X}..0x{:<6X} {:>4}b  {:<14} {:<44} {}",
            "",
            e.start,
            e.end,
            e.end - e.start,
            e.kind,
            e.name,
            e.value,
            indent = (e.depth as usize) * 2
        ));
    }
}

/// Raw bytes of one export, at the offsets traces and failure messages quote.
/// The field records a class or struct export declares, one line per field, for checking a
/// recovered definition against the data it is read with.
/// One export's bytecode, disassembled.
#[derive(Serialize)]
pub struct ScriptReport {
    pub export: u32,
    pub object_name: String,
    pub class_name: String,
    pub script: rivals_uasset::Script,
    /// The events that enter this function when it is a Blueprint's Ubergraph, by start offset.
    pub entries: Vec<(u32, String)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<rivals_uasset::FunctionSignature>,
    /// The functions in this package that call this one, with the offset of each call.
    pub callers: Vec<(String, u32)>,
    /// Each statement as the assembler text writes it, with the offsets of what an edit can
    /// address in it: its literals, object constants, texts, calls and conditions, as
    /// `script-set --at` takes them, and where its jumps lead.
    pub lines: Vec<rivals_uasset::ScriptLine>,
    /// Labels at the very end of the script, where a jump past its last statement lands.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub end_labels: Vec<rivals_uasset::TextLabel>,
    /// The whole script as assembler text, which `asset script-assemble` reads back.
    pub text: String,
}

pub fn script(request: &Request<'_>, export: u32) -> Result<ScriptReport, String> {
    let parsed = parse(request)?;
    let found = parsed
        .exports
        .iter()
        .find(|e| e.index == export)
        .ok_or_else(|| format!("no export {export}"))?;
    let Some(script) = &found.script else {
        return Err(format!(
            "export {export} ({}) carries no bytecode this reader measured",
            found.class_name
        ));
    };
    let scripts = || {
        parsed
            .exports
            .iter()
            .filter_map(|e| Some((e.object_name.as_str(), e.script.as_ref()?)))
    };
    let entries = rivals_uasset::ubergraph_entries(scripts())
        .remove(&found.object_name)
        .unwrap_or_default();
    let callers = rivals_uasset::call_sites(scripts())
        .remove(&found.object_name)
        .unwrap_or_default();
    let text = rivals_uasset::print_script(&parsed, export)
        .ok_or_else(|| format!("export {export} carries no bytecode"))?;
    Ok(ScriptReport {
        export,
        object_name: found.object_name.clone(),
        class_name: found.class_name.clone(),
        script: script.clone(),
        entries,
        signature: found.signature.clone(),
        callers,
        lines: rivals_uasset::script_lines(&parsed, export),
        end_labels: text.end_labels.clone(),
        text: text.text(),
    })
}

pub fn print_script(report: &ScriptReport, expressions: bool, out: &mut impl FnMut(String)) {
    let script = &report.script;
    out(format!(
        "{} ({})  {} bytes stored, {} loaded, {} statement(s){}",
        report.object_name,
        report.class_name,
        script.storage_size,
        script.buffer_size,
        script.script_len(),
        if script.complete() {
            String::new()
        } else {
            ", stopped".to_string()
        }
    ));
    if let Some(signature) = &report.signature {
        out(signature.render(&report.object_name));
        let locals: Vec<String> = signature
            .locals
            .iter()
            .map(|l| format!("{}: {}", l.name, l.kind))
            .collect();
        if !locals.is_empty() {
            out(format!("locals: {}", locals.join(", ")));
        }
    }
    if !report.callers.is_empty() {
        let callers: Vec<String> = report
            .callers
            .iter()
            .map(|(caller, at)| format!("{caller} 0x{at:04X}"))
            .collect();
        out(format!("called by: {}", callers.join(", ")));
    }
    if script.complete()
        && let Some(lock) = script.resize_lock()
    {
        out(format!("keeps its size: {lock}"));
    }
    out(String::new());
    let label = |label: &rivals_uasset::TextLabel| match &label.note {
        Some(note) => format!("        @{}:  ; {note}", label.name),
        None => format!("        @{}:", label.name),
    };
    for line in &report.lines {
        for each in &line.labels {
            out(label(each));
        }
        match &line.note {
            Some(note) => out(format!("0x{:04X}  {}  ; {note}", line.offset, line.text)),
            None => out(format!("0x{:04X}  {}", line.offset, line.text)),
        }
        if expressions {
            for slot in &line.expressions {
                out(format!(
                    "          0x{:04X}  {:<9} {:<18} {}",
                    slot.at,
                    slot.kind.label(),
                    slot.token,
                    slot.text
                ));
            }
        }
    }
    for each in &report.end_labels {
        out(label(each));
    }
    // The walk's own account of where it stopped closes the listing.
    for stop in rivals_uasset::render_script(script)
        .lines()
        .filter(|line| line.starts_with("!!"))
    {
        out(stop.to_string());
    }
}

/// The field records a class or struct export declares.
#[derive(Serialize)]
pub struct FieldsReport {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub super_struct: Option<String>,
    pub fields: Vec<FieldRecord>,
}

#[derive(Serialize)]
pub struct FieldRecord {
    pub index: u16,
    pub name: String,
    /// The type as a reader would write it: `Int`, `Array<Vector>`, `Map<Name, Object>`.
    #[serde(rename = "type")]
    pub kind: String,
    /// How many slots the one name covers: more than one for a static array.
    pub array_dim: u8,
}

pub fn print_fields(report: &FieldsReport, out: &mut impl FnMut(String)) {
    out(format!(
        "{} : {}  ({} fields)",
        report.name,
        report.super_struct.as_deref().unwrap_or("-"),
        report.fields.len()
    ));
    for field in &report.fields {
        let dim = if field.array_dim > 1 {
            format!("[{}]", field.array_dim)
        } else {
            String::new()
        };
        out(format!(
            "  {:>4}  {}{dim}: {}",
            field.index, field.name, field.kind
        ));
    }
}

pub fn fields(request: &Request<'_>, export: u32) -> Result<FieldsReport, String> {
    let parsed = parse(request)?;
    let found = parsed
        .exports
        .iter()
        .find(|e| e.index == export)
        .ok_or_else(|| format!("no export {export}"))?;
    let Some(definition) = &found.struct_definition else {
        return Err(format!(
            "export {export} ({}) carries no field records this reader recovered",
            found.class_name
        ));
    };
    Ok(FieldsReport {
        name: definition.name.clone(),
        super_struct: definition.super_struct.clone(),
        fields: definition
            .properties
            .iter()
            .map(|property| FieldRecord {
                index: property.index,
                name: property.name.clone(),
                kind: rivals_uasset::property_type_text(&property.inner),
                array_dim: property.array_dim,
            })
            .collect(),
    })
}

pub fn hex(
    request: &Request<'_>,
    export: u32,
    from: Option<u64>,
) -> Result<Vec<rivals_uasset::HexRow>, String> {
    let source = if request.container.is_empty() {
        AssetSource::Loose
    } else {
        source_of(request.container)
    };
    let bundle = asset::load_bundle(request.game_root, request.container, request.entry, source)?;
    let bundle = AssetBundle {
        asset: &bundle.asset_file_buffer,
        exports: &bundle.exports_file_buffer,
    };
    let header = rivals_uasset::read_header(&bundle)?;
    let (bytes, offset) = rivals_uasset::export_bytes(&bundle, &header, export)?;
    let base = u64::try_from(offset).map_err(|_| "export has a negative offset")?;
    Ok(rivals_uasset::hex_rows(bytes, base)
        .into_iter()
        .filter(|row| from.is_none_or(|want| row.offset + rivals_uasset::ROW_BYTES as u64 > want))
        .collect())
}

pub fn print_hex(rows: &[rivals_uasset::HexRow], out: &mut impl FnMut(String)) {
    for row in rows {
        out(format!("0x{:<8X} {}  {}", row.offset, row.hex, row.ascii));
    }
}

/// The language texts are shown in. One asked for by name has to exist; the default falls back to
/// what the package stores when the game has no translations for it.
pub struct Culture {
    pub name: String,
    pub asked: bool,
}

/// Fills in what the game shows for each text, in `culture`.
fn show_game_text(
    parsed: &mut ParsedPackage,
    request: &Request<'_>,
    culture: &Culture,
) -> Result<(), String> {
    let localizer = match localization::Localizer::for_culture(request.game_root, &culture.name) {
        Ok(localizer) => localizer,
        Err(reason) if culture.asked => return Err(reason),
        Err(_) => return Ok(()),
    };
    let schema = mappings::resolve(request.usmap, request.configured_usmap)
        .and_then(|path| mappings::load(&path))
        .ok();
    localization::localize(
        parsed,
        &localizer,
        &localization::TableSource {
            game_root: request.game_root,
            container: request.container,
            mappings: schema.as_deref(),
        },
    );
    Ok(())
}

/// A dump, with the inherited values it was asked for, keyed by export.
#[derive(Serialize)]
pub struct DumpReport<'a> {
    #[serde(flatten)]
    pub package: &'a ParsedPackage,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub inherited: &'a BTreeMap<u32, inherit::InheritReport>,
}

/// The values each export in `parsed` does not store, as its archetypes hold them.
pub fn inherited(
    request: &Request<'_>,
    parsed: &ParsedPackage,
) -> Result<BTreeMap<u32, inherit::InheritReport>, String> {
    let schema = mappings::resolve(request.usmap, request.configured_usmap)
        .and_then(|path| mappings::load(&path))
        .ok();
    let source = inherit::ArchetypeSource {
        game_root: request.game_root,
        container: request.container,
        mappings: schema.as_deref(),
    };
    parsed
        .exports
        .iter()
        .map(|export| {
            Ok((
                export.index,
                inherit::inherited_for(parsed, export.index, &source)?,
            ))
        })
        .collect()
}

/// How a value reads when shown: a text as the game shows it, what it stores after it.
fn shown(value: &PropertyValue) -> String {
    match value {
        PropertyValue::Text {
            display: Some(display),
            value: Some(stored),
            ..
        } => format!("{display}  ({stored})"),
        other => other.summary(),
    }
}

pub fn dump(
    request: &Request<'_>,
    only: Option<u32>,
    culture: &Culture,
) -> Result<ParsedPackage, String> {
    let mut parsed = parse(request)?;
    show_game_text(&mut parsed, request, culture)?;
    if let Some(index) = only {
        parsed.exports.retain(|e| e.index == index);
        if parsed.exports.is_empty() {
            return Err(format!("no export with index {index}"));
        }
    }
    Ok(parsed)
}

pub fn print_dump(
    parsed: &ParsedPackage,
    inherited: &BTreeMap<u32, inherit::InheritReport>,
    out: &mut impl FnMut(String),
) {
    for export in &parsed.exports {
        let from: HashMap<String, &inherit::Inherited> = inherited
            .get(&export.index)
            .map(|report| {
                report
                    .values
                    .iter()
                    .map(|value| (value.path.join("."), value))
                    .collect()
            })
            .unwrap_or_default();
        out(format!(
            "[{}] {} ({})  {}",
            export.index,
            export.object_name,
            export.class_name,
            status_label(&export.status)
        ));
        if let ExportStatus::Failed { reason } = &export.status {
            out(format!("     ! {reason}"));
        }
        if let Some(note) = &export.note {
            out(format!("     ! layout walk stopped: {note}"));
        }
        // Rows are not printed as cells, so without this an undecoded payload inside a DataTable
        // row leaves no trace in the text dump at all.
        for payload in &export.undecoded {
            out(format!(
                "     ! undecoded {} payload {:#X}..{:#X}: {}",
                payload.struct_name, payload.at, payload.end, payload.reason
            ));
        }
        print_entries(&export.properties, 1, &Inherits::of(&from), out);
        if let Some(why) = inherited
            .get(&export.index)
            .and_then(|report| report.stopped.as_deref())
        {
            out(format!("     not inherited further: {why}"));
        }
        if !export.defaults.is_empty() {
            out("     defaults:".to_string());
            print_entries(&export.defaults, 2, &Inherits::none(), out);
        }
        if let Some(table) = &export.data_table {
            out(format!(
                "     rows: {} of {}",
                table.rows.len(),
                table.row_struct
            ));
        }
        if let Some(table) = &export.string_table {
            out(format!(
                "     string table {}: {} entries",
                table.namespace,
                table.entries.len()
            ));
            for entry in &table.entries {
                out(format!("       {} = {:?}", entry.key, entry.source));
            }
        }
        if !export.trailing_hex.is_empty() {
            out("     unconsumed bytes:".to_string());
            for line in export.trailing_hex.lines() {
                out(format!("       {line}"));
            }
        }
        out(String::new());
    }
    if !parsed.unresolved_structs.is_empty() {
        out(format!(
            "unresolved structs: {}",
            parsed.unresolved_structs.join(", ")
        ));
    }
}

/// The inherited values a dump prints in place of "not stored", by the path from the export, and
/// the path the entries being printed sit at.
struct Inherits<'a> {
    values: Option<&'a HashMap<String, &'a inherit::Inherited>>,
    prefix: Vec<String>,
}

impl<'a> Inherits<'a> {
    fn of(values: &'a HashMap<String, &'a inherit::Inherited>) -> Self {
        Self {
            values: Some(values),
            prefix: Vec::new(),
        }
    }

    fn none() -> Self {
        Self {
            values: None,
            prefix: Vec::new(),
        }
    }

    fn inside(&self, segment: &str) -> Self {
        let mut prefix = self.prefix.clone();
        prefix.push(segment.to_string());
        Self {
            values: self.values,
            prefix,
        }
    }

    fn at(&self, segment: &str) -> Option<&'a inherit::Inherited> {
        let mut path = self.prefix.clone();
        path.push(segment.to_string());
        self.values?.get(&path.join(".")).copied()
    }
}

fn print_entries(
    entries: &[PropertyEntry],
    depth: usize,
    inherits: &Inherits<'_>,
    out: &mut impl FnMut(String),
) {
    for entry in entries {
        let label = match entry.element {
            Some(index) => format!("{}[{index}]", entry.name),
            None => entry.name.clone(),
        };
        if let (PropertyValue::Unset { .. }, Some(inherited)) = (&entry.value, inherits.at(&label))
        {
            let note = format!("  (inherited from {})", inherited.from);
            print_value(
                &label,
                &inherited.value,
                depth,
                &Inherits::none(),
                &note,
                out,
            );
            continue;
        }
        print_value(
            &label,
            &entry.value,
            depth,
            &inherits.inside(&label),
            "",
            out,
        );
    }
}

/// One value under its label. A struct, a list or a map opens into a line for each field, item
/// or entry beneath it, as deep as it goes; anything else is the one line. A list item is labelled
/// by its index and a map entry by its key, or by `key` and `value` beneath it when the key is a
/// struct of its own.
fn print_value(
    label: &str,
    value: &PropertyValue,
    depth: usize,
    inherits: &Inherits<'_>,
    note: &str,
    out: &mut impl FnMut(String),
) {
    let pad = "  ".repeat(depth + 1);
    // An item reads `[3] value`, as it always has; anything else `name: value`.
    let line = |text: &str| {
        if label.starts_with('[') {
            format!("{pad}{label} {text}{note}")
        } else {
            format!("{pad}{label}: {text}{note}")
        }
    };
    match value {
        PropertyValue::Struct { name, fields } if value.summary().contains('{') => {
            out(line(name));
            print_entries(fields, depth + 1, inherits, out);
        }
        PropertyValue::Text { parts, .. } if !parts.is_empty() => {
            out(line(&shown(value)));
            print_entries(parts, depth + 1, &Inherits::none(), out);
        }
        PropertyValue::Array { items } | PropertyValue::Set { items } if !items.is_empty() => {
            out(line(&format!("[{}]", items.len())));
            for (index, item) in items.iter().enumerate() {
                print_value(
                    &format!("[{index}]"),
                    item,
                    depth + 1,
                    &Inherits::none(),
                    "",
                    out,
                );
            }
        }
        PropertyValue::Map { entries } if !entries.is_empty() => {
            out(line(&format!("{{{} entries}}", entries.len())));
            for entry in entries {
                if opens(&entry.key) {
                    out(format!("{pad}  {{entry}}"));
                    let inner = Inherits::none();
                    print_value("key", &entry.key, depth + 2, &inner, "", out);
                    print_value("value", &entry.value, depth + 2, &inner, "", out);
                } else {
                    print_value(
                        &shown(&entry.key),
                        &entry.value,
                        depth + 1,
                        &Inherits::none(),
                        "",
                        out,
                    );
                }
            }
        }
        other => out(line(&shown(other))),
    }
}

/// Whether a value prints over several lines rather than as one.
fn opens(value: &PropertyValue) -> bool {
    match value {
        PropertyValue::Struct { .. } => value.summary().contains('{'),
        PropertyValue::Text { parts, .. } => !parts.is_empty(),
        PropertyValue::Array { items } | PropertyValue::Set { items } => !items.is_empty(),
        PropertyValue::Map { entries } => !entries.is_empty(),
        _ => false,
    }
}

#[derive(Serialize)]
pub struct TableReport {
    pub export: u32,
    pub row_struct: String,
    pub columns: Vec<String>,
    /// Each row's values as plain JSON, nested values whole: see [`plain_value`].
    pub rows: Vec<BTreeMap<String, serde_json::Value>>,
    /// The same rows as one-line summaries, for the printed table.
    #[serde(skip)]
    pub summaries: Vec<BTreeMap<String, String>>,
    /// One row was asked for, so it prints in full rather than as a line of the table.
    #[serde(skip)]
    pub picked: bool,
}

/// A DataTable's rows, or with `row` only the row of that name.
pub fn table(
    request: &Request<'_>,
    culture: &Culture,
    row: Option<&str>,
) -> Result<TableReport, String> {
    let mut parsed = parse(request)?;
    show_game_text(&mut parsed, request, culture)?;
    let export = parsed
        .exports
        .iter()
        .find(|e| e.data_table.is_some())
        .ok_or_else(|| failure_reason(&parsed))?;
    let Some(table) = &export.data_table else {
        return Err("no data table in this asset".into());
    };

    let picked: Vec<_> = match row {
        Some(name) => table
            .rows
            .iter()
            .filter(|candidate| candidate.name.eq_ignore_ascii_case(name.trim()))
            .collect(),
        None => table.rows.iter().collect(),
    };
    if let Some(name) = row
        && picked.is_empty()
    {
        let some: Vec<&str> = table.rows.iter().take(5).map(|r| r.name.as_str()).collect();
        return Err(format!(
            "no row {name} in this table, which has {} rows, such as {}",
            table.rows.len(),
            some.join(", ")
        ));
    }
    let mut rows = Vec::with_capacity(picked.len());
    let mut summaries = Vec::with_capacity(picked.len());
    for row in picked {
        let mut values = BTreeMap::new();
        let mut cells = BTreeMap::new();
        values.insert(
            "__row".to_string(),
            serde_json::Value::String(row.name.clone()),
        );
        cells.insert("__row".to_string(), row.name.clone());
        for field in &row.fields {
            let cell = match &field.value {
                PropertyValue::Text {
                    display: Some(display),
                    ..
                } => display.clone(),
                other => other.summary(),
            };
            values.insert(field.label(), plain_value(&field.value));
            cells.insert(field.label(), cell);
        }
        rows.push(values);
        summaries.push(cells);
    }

    Ok(TableReport {
        export: export.index,
        row_struct: table.row_struct.clone(),
        columns: table.columns.clone(),
        rows,
        summaries,
        picked: row.is_some(),
    })
}

/// A value as plain JSON: a struct as an object of its fields, an array or set as a list, a map
/// as an object when its keys are names, strings or numbers and as `{key, value}` pairs when they
/// are not, a text as what the game shows, an enum by its name and an object by its path. A value
/// the row holds at its default is that type's zero where its type says, and one it does not
/// store at all is `null`.
pub fn plain_value(value: &PropertyValue) -> serde_json::Value {
    use serde_json::{Value, json};

    let fields = |fields: &[rivals_uasset::PropertyEntry]| {
        Value::Object(
            fields
                .iter()
                .map(|field| (field.label(), plain_value(&field.value)))
                .collect(),
        )
    };
    match value {
        PropertyValue::Bool { value } => Value::Bool(*value),
        PropertyValue::Int { value } => json!(value),
        PropertyValue::UInt { value } => json!(value),
        PropertyValue::Byte { value } => json!(value),
        PropertyValue::Float { value } => serde_json::Number::from_f64(*value)
            .map_or_else(|| Value::String(value.to_string()), Value::Number),
        PropertyValue::Str { value } | PropertyValue::Name { value } => {
            Value::String(value.clone())
        }
        PropertyValue::Text {
            display: Some(shown),
            ..
        }
        | PropertyValue::Text {
            value: Some(shown), ..
        } => Value::String(shown.clone()),
        PropertyValue::Enum {
            name: Some(name), ..
        } => Value::String(name.clone()),
        PropertyValue::Enum { value, .. } => json!(value),
        PropertyValue::Object { path, .. } => path.clone().map_or(Value::Null, Value::String),
        PropertyValue::SoftObject { path } => Value::String(path.clone()),
        PropertyValue::FieldPath { .. } => Value::String(value.summary()),
        PropertyValue::LazyObject { guid } => Value::String(guid.clone()),
        PropertyValue::Array { items } | PropertyValue::Set { items } => {
            Value::Array(items.iter().map(plain_value).collect())
        }
        PropertyValue::Map { entries } => {
            let keys: Option<Vec<String>> = entries.iter().map(|e| key_text(&e.key)).collect();
            let unique = keys.as_ref().is_some_and(|keys| {
                keys.iter().collect::<std::collections::HashSet<_>>().len() == keys.len()
            });
            match keys {
                Some(keys) if unique => Value::Object(
                    keys.into_iter()
                        .zip(entries)
                        .map(|(key, entry)| (key, plain_value(&entry.value)))
                        .collect(),
                ),
                _ => Value::Array(
                    entries
                        .iter()
                        .map(|e| json!({ "key": plain_value(&e.key), "value": plain_value(&e.value) }))
                        .collect(),
                ),
            }
        }
        PropertyValue::Struct { fields: held, .. } => fields(held),
        PropertyValue::Default {
            fields: held,
            declared,
        } => {
            if held.is_empty() {
                zero_of(*declared)
            } else {
                fields(held)
            }
        }
        PropertyValue::Unset { .. } => Value::Null,
        other => Value::String(other.summary()),
    }
}

/// A map key that can stand as a JSON object key.
fn key_text(key: &PropertyValue) -> Option<String> {
    Some(match key {
        PropertyValue::Str { value } | PropertyValue::Name { value } => value.clone(),
        PropertyValue::Enum {
            name: Some(name), ..
        } => name.clone(),
        PropertyValue::Enum { value, .. } | PropertyValue::Int { value } => value.to_string(),
        PropertyValue::UInt { value } => value.to_string(),
        PropertyValue::Byte { value } => value.to_string(),
        PropertyValue::Bool { value } => value.to_string(),
        _ => return None,
    })
}

/// What a value of a stored kind holds at its default.
fn zero_of(declared: Option<&str>) -> serde_json::Value {
    use serde_json::{Value, json};

    match declared {
        Some("Bool") => Value::Bool(false),
        Some(
            "Int" | "Int8" | "Int16" | "Int64" | "UInt16" | "UInt32" | "UInt64" | "Byte" | "Enum",
        ) => json!(0),
        Some("Float" | "Double") => json!(0.0),
        Some("Str" | "Name" | "Text") => Value::String(String::new()),
        Some("Array" | "Set") => Value::Array(Vec::new()),
        Some("Map") => Value::Object(serde_json::Map::new()),
        _ => Value::Null,
    }
}

/// A missing table is nearly always a failed parse, so surface that instead of a bare "not found".
fn failure_reason(parsed: &ParsedPackage) -> String {
    for export in &parsed.exports {
        if let ExportStatus::Failed { reason } = &export.status {
            return format!("{} did not parse: {reason}", export.class_name);
        }
    }
    "this asset contains no DataTable export".to_string()
}

pub fn print_table(report: &TableReport, out: &mut impl FnMut(String)) {
    if report.picked {
        for row in &report.rows {
            let name = row.get("__row").and_then(|n| n.as_str()).unwrap_or("?");
            out(format!("{name} ({})", report.row_struct));
            for column in &report.columns {
                if let Some(value) = row.get(column) {
                    print_tree(column, value, 1, out);
                }
            }
        }
        return;
    }
    out(format!(
        "{} rows of {}",
        report.rows.len(),
        report.row_struct
    ));
    let mut headers = vec!["__row".to_string()];
    headers.extend(report.columns.iter().cloned());
    let widths: Vec<usize> = headers
        .iter()
        .map(|h| {
            report
                .summaries
                .iter()
                .map(|r| r.get(h).map_or(0, String::len))
                .chain(std::iter::once(h.len()))
                .max()
                .unwrap_or(0)
                .min(40)
        })
        .collect();

    let line = |cells: Vec<String>| {
        cells
            .iter()
            .zip(&widths)
            .map(|(cell, width)| {
                let mut text = cell.clone();
                // Widths are bytes, and a cut inside a multi-byte character would panic.
                let mut cut = (*width).min(text.len());
                while !text.is_char_boundary(cut) {
                    cut -= 1;
                }
                text.truncate(cut);
                format!("{text:width$}", width = width)
            })
            .collect::<Vec<_>>()
            .join("  ")
    };

    out(line(headers.clone()));
    for row in &report.summaries {
        out(line(
            headers
                .iter()
                .map(|h| row.get(h).cloned().unwrap_or_default())
                .collect(),
        ));
    }
}

/// One value of a row as an indented tree: a struct or map field per line under its name, a list
/// item per line under its index.
fn print_tree(name: &str, value: &serde_json::Value, depth: usize, out: &mut impl FnMut(String)) {
    use serde_json::Value;

    let pad = "  ".repeat(depth);
    match value {
        Value::Object(fields) if !fields.is_empty() => {
            out(format!("{pad}{name}:"));
            for (field, inner) in fields {
                print_tree(field, inner, depth + 1, out);
            }
        }
        Value::Array(items) if !items.is_empty() => {
            out(format!("{pad}{name}: [{} items]", items.len()));
            for (index, inner) in items.iter().enumerate() {
                print_tree(&format!("[{index}]"), inner, depth + 1, out);
            }
        }
        Value::Object(_) => out(format!("{pad}{name}: {{}}")),
        Value::Array(_) => out(format!("{pad}{name}: []")),
        Value::String(text) => out(format!("{pad}{name}: {text}")),
        other => out(format!("{pad}{name}: {other}")),
    }
}

#[derive(Serialize)]
pub struct AuditReport {
    pub container: String,
    /// The mappings predate the game's newest patch, so a class changed since reads wrong.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mappings_warning: Option<String>,
    pub packages_scanned: usize,
    /// Exports left out because `--skip-blueprint` was passed and their class is generated.
    /// Counted rather than silently dropped, so the figures still say what was not looked at.
    #[serde(skip_serializing_if = "is_zero")]
    pub exports_blueprint_skipped: usize,
    pub exports_total: usize,
    pub exports_complete: usize,
    pub exports_partial: usize,
    /// Exports whose remainder is a named bulk payload rather than an unexplained tail.
    pub exports_payload: usize,
    pub exports_failed: usize,
    /// Failures caused by the mappings file not covering a class or struct. These need a fuller
    /// .usmap, not a parser change, so they are worth separating from real decode failures.
    pub failed_missing_from_mappings: usize,
    /// Distinct failure causes seen, so a truncated histogram does not read as the whole story.
    pub distinct_failure_causes: usize,
    /// Exports whose property block decoded without error. The rest of an export is often
    /// class-specific binary data (mesh, texture and shader payloads) that carries no properties.
    pub decoded_percent: f64,
    /// The stricter measure: exports that also landed exactly on their declared serial size.
    pub exact_percent: f64,
    /// Property types actually read, so an unexercised decoder can be told from an unreachable one.
    pub property_kinds: Vec<Count>,
    pub top_failures: Vec<Count>,
    /// Where to look for each of the top failures: a few `package#export` locations.
    pub failure_examples: Vec<ClassExamples>,
    pub unresolved_structs: Vec<Count>,
    pub partial_classes: Vec<Count>,
    /// Where to look for each class with unexplained tails: a few `package#export` locations.
    pub partial_examples: Vec<ClassExamples>,
    pub payload_kinds: Vec<Count>,
    /// Per class, how its exports came out. This is what says whether a class is fully understood
    /// or only mostly, which a single overall percentage hides.
    pub status_by_class: Vec<ClassStatus>,
    /// Packages that only parsed after a schema slot was elided. A high count means the mappings
    /// describe a different build from the data.
    pub packages_repaired: usize,
    /// Which slots the repair search removed, most common first.
    pub schema_fixups: Vec<Count>,
    /// Every name the mappings file fails to describe, ranked by how many exports it blocked.
    pub mappings_gaps: Vec<MappingsGap>,
    /// Unversioned headers re-encoded, and how many did not come back byte for byte. Writing
    /// into a package means re-emitting its headers, so anything but zero here blocks that.
    pub headers_checked: usize,
    pub headers_differing: usize,
    /// How many headers took each shape the audit counts. One the game's data never takes is a
    /// shape a save may not write.
    pub header_shapes: Vec<Count>,
    /// Packages whose own header did not come back byte for byte when re-serialized. Editing
    /// re-emits that header, so anything but zero here is a package that cannot be written.
    pub packages_header_broken: usize,
    /// Bulk data resources by where their payload lives. An inline payload sits in the export data,
    /// addressed from its export's start with its table index as the word before it; one the
    /// writer cannot place that way is one it cannot keep in step with an edit.
    pub resources_separate: usize,
    pub resources_inline: usize,
    pub inline_placed: usize,
    pub inline_unplaced: usize,
    /// Class, function and struct exports whose layout walk stopped short. Each is measured as a
    /// payload, and a class among them loses the definition its instances are read with, so zero
    /// is the only good number.
    pub failed_class_recovery: usize,
    pub class_recovery_failures: Vec<Count>,
    pub class_recovery_examples: Vec<ClassExamples>,
    /// `ETextHistoryType` values read, which tells a text layout the data uses from one it never does.
    pub text_histories: Vec<Count>,
    /// The `ETextFlag` words texts carry under each history, which says what a rebuilt text takes.
    pub text_flags: Vec<Count>,
    /// Packages storing tagged properties rather than unversioned ones, which the structural
    /// editor treats differently.
    pub packages_tagged: usize,
    /// Packages that read only under a mappings entry other than the default one for a name the
    /// file holds twice, and which names those were.
    pub packages_twinned: usize,
    pub twins: Vec<Count>,
    /// Instanced struct payloads that did not decode. Their exports still count as exact, because
    /// each payload's length prefix puts the cursor back, so nothing else in this report shows
    /// them and a reader gap can sit here unnoticed.
    pub undecoded_payloads: usize,
    pub undecoded_causes: Vec<Count>,
    pub undecoded_examples: Vec<ClassExamples>,
    /// Bytecode scripts read, and how many were followed to their end. A script that stopped is
    /// not trusted for the objects it names, so its export still gates removal and duplication.
    pub scripts_total: usize,
    pub scripts_complete: usize,
    pub scripts_stopped: usize,
    pub script_stops: Vec<Count>,
    pub script_stop_examples: Vec<ClassExamples>,
    /// Every bytecode token read, so a token this build adds shows up as `unknown`.
    pub script_tokens: Vec<Count>,
    /// What a change of script size relies on, and every place it does not hold.
    pub relocation: Vec<Count>,
    pub relocation_examples: Vec<ClassExamples>,
}

#[derive(Serialize)]
pub struct SynthCheckReport {
    pub packages_scanned: usize,
    pub structs_read: usize,
    /// Structs the mappings also describe, which is where a comparison is possible.
    pub compared: usize,
    pub identical: usize,
    pub differing: Vec<SynthDifference>,
    /// Structs the mappings do not cover, so nothing to compare against.
    pub only_synthesised: Vec<String>,
}

#[derive(Serialize)]
pub struct SynthDifference {
    pub name: String,
    pub detail: String,
}

/// Reads every Blueprint struct definition out of the packages themselves and compares it with the
/// mappings entry of the same name.
///
/// The mappings are ground truth for any struct they cover, so this checks the reader against tens
/// of structs instead of the handful that happen to be missing. A width read wrong for one property
/// type shows up here immediately, where a targeted test only covers the types its samples use.
pub fn synth_check(
    game_root: &str,
    container: &str,
    limit: Option<usize>,
    filter: Option<&str>,
    usmap: Option<&str>,
    configured_usmap: Option<&str>,
    mut progress: impl FnMut(usize, usize),
) -> Result<SynthCheckReport, String> {
    let path = mappings::resolve(usmap, configured_usmap)?;
    let schema = mappings::load(&path)?;
    let filter = asset::PathFilter::new(filter);
    let (store, packages) = asset::list_packages(game_root, container)?;
    let packages: Vec<_> = packages
        .into_iter()
        .filter(|(_, path)| filter.matches(path))
        .collect();
    let total = limit.map_or(packages.len(), |l| l.min(packages.len()));

    let mut report = SynthCheckReport {
        packages_scanned: 0,
        structs_read: 0,
        compared: 0,
        identical: 0,
        differing: Vec::new(),
        only_synthesised: Vec::new(),
    };
    let converter = asset::PackageConverter::new(&*store);
    for (index, (package_id, path)) in packages.iter().take(total).enumerate() {
        progress(index + 1, total);
        report.packages_scanned += 1;
        let Ok(bundle) = converter.convert(*package_id, path) else {
            continue;
        };
        let bundle = AssetBundle {
            asset: &bundle.asset_file_buffer,
            exports: &bundle.exports_file_buffer,
        };
        let found = match rivals_uasset::read_struct_definitions(&bundle, Some(&schema)) {
            Ok(found) => found,
            Err(reason) => {
                report.differing.push(SynthDifference {
                    name: path.clone(),
                    detail: format!("could not read: {reason}"),
                });
                continue;
            }
        };
        if found.is_empty() {
            continue;
        }
        report.structs_read += found.len();
        let names: Vec<String> = found.iter().map(|d| d.name().to_string()).collect();
        let synth = rivals_uasset::mappings_from_definitions_with(found, Some(&schema));
        for name in names {
            let (Some(theirs), Some(ours)) = (schema.schema(&name), synth.schema(&name)) else {
                report.only_synthesised.push(name);
                continue;
            };
            report.compared += 1;
            match compare(&ours, &theirs) {
                None => report.identical += 1,
                Some(detail) => report.differing.push(SynthDifference { name, detail }),
            }
        }
    }
    Ok(report)
}

/// Compares flattened slots rather than raw property indices: the flattener expands static arrays
/// and sorts, so only the resulting slot sequence has to agree.
fn compare(ours: &rivals_uasset::Schema<'_>, theirs: &rivals_uasset::Schema<'_>) -> Option<String> {
    if ours.len() != theirs.len() {
        return Some(format!(
            "{} slots, mappings say {}",
            ours.len(),
            theirs.len()
        ));
    }
    for index in 0..ours.len() {
        let (Some(a), Some(b)) = (ours.slot(index), theirs.slot(index)) else {
            return Some(format!("slot {index} missing"));
        };
        if a.property.name != b.property.name {
            return Some(format!(
                "slot {index} is {}, mappings say {}",
                a.property.name, b.property.name
            ));
        }
        let (ak, bk) = (
            rivals_uasset::kind_name(&a.property.inner),
            rivals_uasset::kind_name(&b.property.inner),
        );
        if ak != bk {
            return Some(format!(
                "slot {index} {} is {ak}, mappings say {bk}",
                a.property.name
            ));
        }
        if a.property.array_dim != b.property.array_dim {
            return Some(format!(
                "slot {index} {} has dim {}, mappings say {}",
                a.property.name, a.property.array_dim, b.property.array_dim
            ));
        }
    }
    None
}

pub fn print_synth_check(report: &SynthCheckReport, out: &mut impl FnMut(String)) {
    out(format!(
        "{} packages, {} struct(s) read, {} compared against the mappings",
        report.packages_scanned, report.structs_read, report.compared
    ));
    out(format!(
        "{} identical, {} differing, {} not in the mappings",
        report.identical,
        report.differing.len(),
        report.only_synthesised.len()
    ));
    for difference in report.differing.iter().take(30) {
        out(format!("  {}: {}", difference.name, difference.detail));
    }
    for name in report.only_synthesised.iter().take(15) {
        out(format!("  only synthesised: {name}"));
    }
}

/// One name the mappings file does not describe well enough to decode against.
///
/// Everything here is a property of the `.usmap`, not of the parser, so this is the list to hand
/// to a dumper. The exception is `short`: an entry can be short because the dump missed properties
/// *or* because the type has a custom serializer whose bytes do not follow its reflected
/// properties at all, and only reading the bytes tells the two apart.
#[derive(Serialize)]
pub struct MappingsGap {
    pub name: String,
    /// `class` and `row struct` are absent from the file entirely. `struct` is present but
    /// declares fewer properties than the data asks for.
    pub kind: &'static str,
    /// Highest slot the data asked for, when the entry exists but is short.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wanted_slot: Option<u32>,
    /// Declared slot count, when the entry exists but is short.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declared_slots: Option<u32>,
    pub exports: usize,
}

/// Pulls the name out of the failure causes that name a mappings shortfall.
fn gap_of(cause: &str) -> Option<(String, &'static str, Option<u32>, Option<u32>)> {
    let last = cause.rsplit(": ").next()?;
    for (prefix, kind) in [("class ", "class"), ("row struct ", "row struct")] {
        if let Some(name) = last
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_suffix(" is not in the mappings file"))
        {
            return Some((name.to_string(), kind, None, None));
        }
    }
    if let Some(name) = last.strip_prefix("struct ").and_then(|rest| {
        rest.strip_suffix(" has no native layout and no schema in the mappings file")
    }) {
        return Some((name.to_string(), "struct", None, None));
    }
    let (name, rest) = last.split_once(" has ")?;
    let (declared, rest) = rest.split_once(" schema slots but the header asked for slot ")?;
    let wanted = rest.split('.').next()?;
    Some((
        name.to_string(),
        "struct",
        wanted.parse().ok(),
        declared.parse().ok(),
    ))
}

#[derive(Serialize, Default, Clone)]
pub struct ClassStatus {
    pub name: String,
    pub exact: usize,
    pub payload: usize,
    pub unexplained: usize,
    pub failed: usize,
}

#[derive(Serialize)]
pub struct Count {
    pub name: String,
    pub count: usize,
}

#[derive(Serialize)]
pub struct ClassExamples {
    pub name: String,
    /// `package path#export index`, at most [`PARTIAL_EXAMPLES`] per class.
    pub examples: Vec<String>,
}

/// Enough to open one and see the bytes without turning the report into a package list.
const PARTIAL_EXAMPLES: usize = 3;

/// Walks an extracted corpus on disk. Far faster than the container path because it skips both
/// the merged store open and the zen to legacy conversion.
pub fn audit_dir(
    dir: &str,
    limit: Option<usize>,
    filter: Option<&str>,
    usmap: Option<&str>,
    configured_usmap: Option<&str>,
    skip_blueprint: bool,
    mut progress: impl FnMut(usize, usize),
) -> Result<AuditReport, String> {
    let path = mappings::resolve(usmap, configured_usmap)?;
    let schema = mappings::load(&path)?;
    let filter = asset::PathFilter::new(filter);
    let files: Vec<_> = asset::list_loose_packages(Path::new(dir))?
        .into_iter()
        .filter(|p| filter.matches(&p.to_string_lossy()))
        .collect();
    let total = limit.map_or(files.len(), |l| l.min(files.len()));

    let mut acc = Accumulator::new(dir.to_string(), skip_blueprint);
    for (index, file) in files.iter().take(total).enumerate() {
        progress(index + 1, total);
        acc.report.packages_scanned += 1;
        let Ok(bundle) = asset::load_from_disk(file) else {
            acc.note_failure(
                "could not read from disk",
                Some(file.to_string_lossy().into_owned()),
            );
            continue;
        };
        acc.absorb(
            &bundle.asset_file_buffer,
            &bundle.exports_file_buffer,
            &schema,
            &PackageSource {
                game_root: "",
                container: "",
                entry: &file.to_string_lossy(),
                kind: AssetSource::Loose,
            },
            None,
        );
    }
    Ok(acc.finish())
}

#[allow(clippy::too_many_arguments)]
pub fn audit(
    game_root: &str,
    container: &str,
    limit: Option<usize>,
    filter: Option<&str>,
    usmap: Option<&str>,
    configured_usmap: Option<&str>,
    skip_blueprint: bool,
    relocation_check: bool,
    text_check: bool,
    mut progress: impl FnMut(usize, usize),
) -> Result<AuditReport, String> {
    let path = mappings::resolve(usmap, configured_usmap)?;
    let schema = mappings::load(&path)?;
    let filter = asset::PathFilter::new(filter);
    let (store, packages) = asset::list_packages(game_root, container)?;
    let packages: Vec<_> = packages
        .into_iter()
        .filter(|(_, path)| filter.matches(path))
        .collect();
    let total = limit.map_or(packages.len(), |l| l.min(packages.len()));

    let mut acc = Accumulator::new(container.to_string(), skip_blueprint);
    acc.relocation_check = relocation_check;
    acc.text_check = text_check;
    acc.report.mappings_warning = mappings::drift_warning(game_root, &path);
    let converter = asset::PackageConverter::new(&*store);
    // Blueprint layouts are read through the same converter, which already holds the headers
    // their packages import.
    let layouts = LayoutReader::through(&*store, &converter);
    for (index, (package_id, path)) in packages.iter().take(total).enumerate() {
        progress(index + 1, total);
        acc.report.packages_scanned += 1;
        match converter.convert(*package_id, path) {
            Ok(bundle) => acc.absorb(
                &bundle.asset_file_buffer,
                &bundle.exports_file_buffer,
                &schema,
                &PackageSource {
                    game_root,
                    container,
                    entry: path,
                    kind: source_of(container),
                },
                Some(&layouts),
            ),
            Err(_) => acc.note_failure("legacy conversion failed", Some(path.clone())),
        }
    }
    Ok(acc.finish())
}

/// Parses a package the way a save checks one, recovering Blueprint layouts through `layouts` when
/// the walk holds a converter for them.
fn parse_checked(
    bundle: &AssetBundle<'_>,
    schema: &Mappings,
    source: &PackageSource<'_>,
    layouts: Option<&LayoutReader<'_>>,
) -> Result<ParsedPackage, String> {
    match layouts {
        Some(layouts) => schema_synth::parse_package_through(
            bundle,
            Some(schema),
            source,
            layouts,
            ParseOptions {
                check_headers: true,
                ..Default::default()
            },
        ),
        None => schema_synth::parse_package_checked(bundle, Some(schema), source),
    }
}

/// Shared tallying so the container walk and the directory walk cannot report differently.
fn is_zero(value: &usize) -> bool {
    *value == 0
}

/// Whether an export is an instance of a Blueprint-generated class.
///
/// Unreal names a generated class `<Blueprint>_C`, and only a mappings dump taken with that
/// Blueprint loaded describes it. A native-only dump leaves the reader recovering the layout from
/// the game's own packages, which mostly works and sometimes does not, so a run that only cares
/// about native coverage wants these out of the figures rather than counted as gaps.
fn is_blueprint_class(class_name: &str) -> bool {
    class_name.ends_with("_C")
}

struct Accumulator {
    skip_blueprint: bool,
    /// Widen every script in memory, verify the result and narrow it back, as `--relocation-check`
    /// asks.
    relocation_check: bool,
    /// Print every script as text and assemble it again, as `--text-check` asks.
    text_check: bool,
    report: AuditReport,
    kinds: BTreeMap<String, usize>,
    failures: BTreeMap<String, usize>,
    failure_examples: BTreeMap<String, Vec<String>>,
    unresolved: BTreeMap<String, usize>,
    partial: BTreeMap<String, usize>,
    partial_examples: BTreeMap<String, Vec<String>>,
    payloads: BTreeMap<String, usize>,
    classes: BTreeMap<String, ClassStatus>,
    fixups: BTreeMap<String, usize>,
    recovery: BTreeMap<String, usize>,
    recovery_examples: BTreeMap<String, Vec<String>>,
    histories: BTreeMap<String, usize>,
    text_flags: BTreeMap<String, usize>,
    twinned: BTreeMap<String, usize>,
    undecoded: BTreeMap<String, usize>,
    undecoded_examples: BTreeMap<String, Vec<String>>,
    stops: BTreeMap<String, usize>,
    relocation: BTreeMap<String, usize>,
    relocation_examples: BTreeMap<String, Vec<String>>,
    stop_examples: BTreeMap<String, Vec<String>>,
    tokens: BTreeMap<String, usize>,
    shapes: BTreeMap<String, usize>,
}

impl Accumulator {
    fn new(source: String, skip_blueprint: bool) -> Self {
        Self {
            skip_blueprint,
            relocation_check: false,
            text_check: false,
            report: AuditReport {
                container: source,
                mappings_warning: None,
                packages_scanned: 0,
                exports_blueprint_skipped: 0,
                exports_total: 0,
                exports_complete: 0,
                exports_partial: 0,
                exports_payload: 0,
                exports_failed: 0,
                failed_missing_from_mappings: 0,
                distinct_failure_causes: 0,
                decoded_percent: 0.0,
                exact_percent: 0.0,
                property_kinds: Vec::new(),
                top_failures: Vec::new(),
                failure_examples: Vec::new(),
                unresolved_structs: Vec::new(),
                partial_classes: Vec::new(),
                partial_examples: Vec::new(),
                payload_kinds: Vec::new(),
                status_by_class: Vec::new(),
                packages_repaired: 0,
                schema_fixups: Vec::new(),
                mappings_gaps: Vec::new(),
                headers_checked: 0,
                headers_differing: 0,
                header_shapes: Vec::new(),
                packages_header_broken: 0,
                resources_separate: 0,
                resources_inline: 0,
                inline_placed: 0,
                inline_unplaced: 0,
                failed_class_recovery: 0,
                class_recovery_failures: Vec::new(),
                class_recovery_examples: Vec::new(),
                text_histories: Vec::new(),
                text_flags: Vec::new(),
                packages_tagged: 0,
                packages_twinned: 0,
                twins: Vec::new(),
                undecoded_payloads: 0,
                undecoded_causes: Vec::new(),
                undecoded_examples: Vec::new(),
                scripts_total: 0,
                scripts_complete: 0,
                scripts_stopped: 0,
                script_stops: Vec::new(),
                script_stop_examples: Vec::new(),
                relocation: Vec::new(),
                relocation_examples: Vec::new(),
                script_tokens: Vec::new(),
            },
            kinds: BTreeMap::new(),
            failures: BTreeMap::new(),
            failure_examples: BTreeMap::new(),
            unresolved: BTreeMap::new(),
            partial: BTreeMap::new(),
            partial_examples: BTreeMap::new(),
            payloads: BTreeMap::new(),
            classes: BTreeMap::new(),
            fixups: BTreeMap::new(),
            recovery: BTreeMap::new(),
            recovery_examples: BTreeMap::new(),
            histories: BTreeMap::new(),
            text_flags: BTreeMap::new(),
            twinned: BTreeMap::new(),
            undecoded: BTreeMap::new(),
            undecoded_examples: BTreeMap::new(),
            stops: BTreeMap::new(),
            stop_examples: BTreeMap::new(),
            relocation: BTreeMap::new(),
            relocation_examples: BTreeMap::new(),
            tokens: BTreeMap::new(),
            shapes: BTreeMap::new(),
        }
    }

    /// `at` is where the failure was seen, kept for the first few so the report says where to
    /// open a hex view rather than only how often something went wrong.
    fn note_failure(&mut self, cause: &str, at: Option<String>) {
        *self.failures.entry(cause.to_string()).or_default() += 1;
        if let Some(at) = at {
            let seen = self.failure_examples.entry(cause.to_string()).or_default();
            if seen.len() < PARTIAL_EXAMPLES {
                seen.push(at);
            }
        }
    }

    fn absorb(
        &mut self,
        asset: &[u8],
        exports: &[u8],
        schema: &Mappings,
        source: &PackageSource<'_>,
        layouts: Option<&LayoutReader<'_>>,
    ) {
        let parsed = match parse_checked(&AssetBundle { asset, exports }, schema, source, layouts) {
            Ok(parsed) => parsed,
            Err(reason) => {
                self.note_failure(&short_reason(&reason), Some(source.entry.to_string()));
                return;
            }
        };
        if rivals_uasset::header_round_trips(&AssetBundle { asset, exports }).is_err() {
            self.report.packages_header_broken += 1;
        }
        if !parsed.info.unversioned_properties {
            self.report.packages_tagged += 1;
        }
        if !parsed.twins.is_empty() {
            self.report.packages_twinned += 1;
            for choice in &parsed.twins {
                *self.twinned.entry(choice.name.clone()).or_default() += 1;
            }
        }
        if let Ok(header) = rivals_uasset::read_header(&AssetBundle { asset, exports }) {
            for (index, resource) in header.data_resources.iter().enumerate() {
                if resource.legacy_bulk_data_flags & rivals_uasset::SEPARATE_PAYLOAD_FLAGS != 0 {
                    self.report.resources_separate += 1;
                    continue;
                }
                self.report.resources_inline += 1;
                if rivals_uasset::locate_inline_payload(&header, exports, index).is_some() {
                    self.report.inline_placed += 1;
                } else {
                    self.report.inline_unplaced += 1;
                }
            }
        }
        self.report.headers_checked += parsed.header_check.checked;
        self.report.headers_differing += parsed.header_check.differing;
        for (shape, count) in &parsed.header_check.shapes {
            *self.shapes.entry(shape.clone()).or_default() += count;
        }
        for (kind, count) in &parsed.property_kinds {
            *self.kinds.entry((*kind).to_string()).or_default() += count;
        }
        for (token, count) in &parsed.script_tokens {
            *self.tokens.entry(token.clone()).or_default() += count;
        }
        for (history, count) in &parsed.text_histories {
            *self
                .histories
                .entry(format!("{history} {}", text_history_label(*history)))
                .or_default() += count;
        }
        for (key, count) in &parsed.text_flags {
            let (history, flags) = key.split_once(' ').unwrap_or((key.as_str(), ""));
            let label = history.parse::<i8>().map_or("unknown", text_history_label);
            *self
                .text_flags
                .entry(format!("{history} {label} flags {flags}"))
                .or_default() += count;
        }
        for name in &parsed.unresolved_structs {
            *self.unresolved.entry(name.clone()).or_default() += 1;
        }
        if !parsed.schema_fixups.is_empty() {
            self.report.packages_repaired += 1;
            for fixup in &parsed.schema_fixups {
                let key = format!("{}.{}", fixup.struct_name, fixup.property);
                *self.fixups.entry(key).or_default() += 1;
            }
        }
        for export in &parsed.exports {
            if self.skip_blueprint && is_blueprint_class(&export.class_name) {
                self.report.exports_blueprint_skipped += 1;
                continue;
            }
            self.report.exports_total += 1;
            let entry = self.classes.entry(export.class_name.clone()).or_default();
            entry.name = export.class_name.clone();
            match &export.status {
                ExportStatus::Complete => entry.exact += 1,
                ExportStatus::Payload { .. } => entry.payload += 1,
                ExportStatus::Partial { .. } => entry.unexplained += 1,
                ExportStatus::Failed { .. } => entry.failed += 1,
            }
            match &export.status {
                ExportStatus::Complete => self.report.exports_complete += 1,
                ExportStatus::Payload { kind, .. } => {
                    self.report.exports_payload += 1;
                    *self.payloads.entry((*kind).to_string()).or_default() += 1;
                }
                ExportStatus::Partial { .. } => {
                    self.report.exports_partial += 1;
                    *self.partial.entry(export.class_name.clone()).or_default() += 1;
                    let seen = self
                        .partial_examples
                        .entry(export.class_name.clone())
                        .or_default();
                    if seen.len() < PARTIAL_EXAMPLES {
                        seen.push(format!("{}#{}", source.entry, export.index));
                    }
                }
                ExportStatus::Failed { reason } => {
                    self.report.exports_failed += 1;
                    self.note_failure(
                        &short_reason(reason),
                        Some(format!("{}#{}", source.entry, export.index)),
                    );
                }
            }
            if let Some(note) = &export.note {
                self.report.failed_class_recovery += 1;
                let cause = short_reason(note);
                *self.recovery.entry(cause.clone()).or_default() += 1;
                let seen = self.recovery_examples.entry(cause).or_default();
                if seen.len() < PARTIAL_EXAMPLES {
                    seen.push(format!("{}#{}", source.entry, export.index));
                }
            }
            if let Some(script) = &export.script {
                self.report.scripts_total += 1;
                match &script.stopped {
                    None => self.report.scripts_complete += 1,
                    Some(stop) => {
                        self.report.scripts_stopped += 1;
                        let cause = format!(
                            "{:#04X} {}: {}",
                            stop.token,
                            rivals_uasset::token_name(stop.token).unwrap_or("unknown"),
                            short_reason(&stop.reason)
                        );
                        *self.stops.entry(cause.clone()).or_default() += 1;
                        let seen = self.stop_examples.entry(cause).or_default();
                        if seen.len() < PARTIAL_EXAMPLES {
                            seen.push(format!("{}#{}@{:#X}", source.entry, export.index, stop.at));
                        }
                    }
                }
            }
            for payload in &export.undecoded {
                self.report.undecoded_payloads += 1;
                let cause = format!("{}: {}", payload.struct_name, short_reason(&payload.reason));
                *self.undecoded.entry(cause.clone()).or_default() += 1;
                let seen = self.undecoded_examples.entry(cause).or_default();
                if seen.len() < PARTIAL_EXAMPLES {
                    seen.push(format!(
                        "{}#{}@{:#X}",
                        source.entry, export.index, payload.at
                    ));
                }
            }
        }
        if self.relocation_check {
            self.check_relocation(asset, exports, schema, source, layouts, &parsed);
        }
        if self.text_check {
            self.check_text(asset, exports, schema, source, layouts, &parsed);
        }
        let census = rivals_uasset::script_census(&parsed);
        for (line, count) in census.counts {
            *self.relocation.entry(line).or_default() += count;
        }
        for (line, detail) in census.examples {
            let seen = self.relocation_examples.entry(line).or_default();
            if seen.len() < PARTIAL_EXAMPLES {
                seen.push(format!("{}{detail}", source.entry));
            }
        }
    }

    /// Widens every literal in the package's scripts in memory and verifies the patch the way a
    /// save is verified, counting each package that holds up and each that does not.
    fn check_relocation(
        &mut self,
        asset: &[u8],
        exports: &[u8],
        schema: &Mappings,
        source: &PackageSource<'_>,
        layouts: Option<&LayoutReader<'_>>,
        parsed: &rivals_uasset::ParsedPackage,
    ) {
        let scripts = rivals_uasset::widening_edits(parsed, None, true);
        if scripts.is_empty() {
            return;
        }
        let widened = scripts.len();
        let changes = PackageEdits {
            scripts,
            ..Default::default()
        };
        let checked = rivals_uasset::patch_package_with(
            &AssetBundle { asset, exports },
            rivals_uasset::Sidecars {
                bulk: None,
                optional_bulk: None,
            },
            parsed,
            &changes,
            Some(schema),
        )
        .and_then(|patched| {
            let after = parse_checked(
                &AssetBundle {
                    asset: &patched.asset,
                    exports: &patched.exports,
                },
                schema,
                source,
                layouts,
            )?;
            rivals_uasset::verify_patch(parsed, &after, &changes, &patched.applied)?;
            // Narrowing each literal back has to give the bytes the package started as: every
            // offset the widening moved moves back to exactly where it was.
            let narrowing = PackageEdits {
                scripts: rivals_uasset::narrowing_edits(parsed, &after),
                ..Default::default()
            };
            if narrowing.scripts.len() != widened {
                return Err(format!(
                    "{} of the {widened} widened literals narrow back",
                    narrowing.scripts.len()
                ));
            }
            let back = rivals_uasset::patch_package_with(
                &AssetBundle {
                    asset: &patched.asset,
                    exports: &patched.exports,
                },
                rivals_uasset::Sidecars {
                    bulk: None,
                    optional_bulk: None,
                },
                &after,
                &narrowing,
                Some(schema),
            )?;
            same_bytes("header", asset, &back.asset)?;
            same_bytes("exports", exports, &back.exports)
        });
        match checked {
            Ok(()) => {
                *self
                    .relocation
                    .entry(
                        "relocation check: packages widened, verified and narrowed back byte for byte"
                            .to_string(),
                    )
                    .or_default() += 1;
                *self
                    .relocation
                    .entry("relocation check: literals widened".to_string())
                    .or_default() += widened;
            }
            Err(reason) => {
                let line = format!("relocation check failed: {}", short_reason(&reason));
                *self.relocation.entry(line.clone()).or_default() += 1;
                let seen = self.relocation_examples.entry(line).or_default();
                if seen.len() < PARTIAL_EXAMPLES {
                    seen.push(format!("{}: {reason}", source.entry));
                }
            }
        }
    }

    /// Prints every whole script in the package as text and assembles it again, counting the
    /// functions that come back byte for byte and why the others do not. Then saves the package
    /// with every one that did written from its own text, the way a text edit is saved, which has
    /// to verify and give back the package's own bytes.
    fn check_text(
        &mut self,
        asset: &[u8],
        exports: &[u8],
        schema: &Mappings,
        source: &PackageSource<'_>,
        layouts: Option<&LayoutReader<'_>>,
        parsed: &rivals_uasset::ParsedPackage,
    ) {
        let mut count = |line: String, example: Option<String>| {
            *self.relocation.entry(line.clone()).or_default() += 1;
            if let Some(example) = example {
                let seen = self.relocation_examples.entry(line).or_default();
                if seen.len() < PARTIAL_EXAMPLES {
                    seen.push(example);
                }
            }
        };
        let results =
            match rivals_uasset::script_round_trips(&AssetBundle { asset, exports }, parsed) {
                Ok(results) => results,
                Err(reason) => {
                    count(
                        format!("text check failed: {}", short_reason(&reason)),
                        Some(format!("{}: {reason}", source.entry)),
                    );
                    return;
                }
            };
        let mut texts = Vec::new();
        for (export, result) in results {
            match result {
                Ok(()) => {
                    count(
                        "text check: functions back byte for byte from their own text".to_string(),
                        None,
                    );
                    if let Some(text) = rivals_uasset::print_script(parsed, export) {
                        texts.push(rivals_uasset::ScriptTextEdit {
                            export,
                            text: text.text(),
                            was: None,
                        });
                    }
                }
                Err(failure) if failure.cause == "did not decode" => {}
                Err(failure) => count(
                    format!("text check failed: {}", failure.cause),
                    Some(format!("{}#{export}: {}", source.entry, failure.detail)),
                ),
            }
        }
        if texts.is_empty() {
            return;
        }
        let changes = PackageEdits {
            script_texts: texts,
            ..Default::default()
        };
        let saved = rivals_uasset::patch_package_with(
            &AssetBundle { asset, exports },
            rivals_uasset::Sidecars::default(),
            parsed,
            &changes,
            Some(schema),
        )
        .and_then(|patched| {
            let after = parse_checked(
                &AssetBundle {
                    asset: &patched.asset,
                    exports: &patched.exports,
                },
                schema,
                source,
                layouts,
            )?;
            rivals_uasset::verify_patch(parsed, &after, &changes, &patched.applied)?;
            same_bytes("header", asset, &patched.asset)?;
            same_bytes("exports", exports, &patched.exports)
        });
        match saved {
            Ok(()) => count(
                "text check: packages saved from their own text, verified and unchanged"
                    .to_string(),
                None,
            ),
            Err(reason) => count(
                format!("text check save failed: {}", short_reason(&reason)),
                Some(format!("{}: {reason}", source.entry)),
            ),
        }
    }

    fn finish(mut self) -> AuditReport {
        if self.report.exports_total > 0 {
            let total = self.report.exports_total as f64;
            let decoded = (self.report.exports_complete
                + self.report.exports_partial
                + self.report.exports_payload) as f64;
            self.report.decoded_percent = decoded * 100.0 / total;
            self.report.exact_percent = self.report.exports_complete as f64 * 100.0 / total;
        }
        self.report.property_kinds = rank_all(self.kinds);
        self.report.header_shapes = rank_all(self.shapes);
        self.report.distinct_failure_causes = self.failures.len();
        self.report.failed_missing_from_mappings = self
            .failures
            .iter()
            .filter(|(cause, _)| is_mappings_gap(cause))
            .map(|(_, count)| *count)
            .sum();
        let failures = std::mem::take(&mut self.failures);
        self.report.unresolved_structs = rank(self.unresolved);
        self.report.partial_classes = rank(self.partial);
        self.report.partial_examples = self
            .report
            .partial_classes
            .iter()
            .filter_map(|class| {
                self.partial_examples
                    .remove(&class.name)
                    .map(|examples| ClassExamples {
                        name: class.name.clone(),
                        examples,
                    })
            })
            .collect();
        self.report.payload_kinds = rank(self.payloads);
        self.report.schema_fixups = rank_all(self.fixups);
        self.report.class_recovery_failures = rank_all(self.recovery);
        self.report.class_recovery_examples = self
            .report
            .class_recovery_failures
            .iter()
            .filter_map(|cause| {
                self.recovery_examples
                    .remove(&cause.name)
                    .map(|examples| ClassExamples {
                        name: cause.name.clone(),
                        examples,
                    })
            })
            .collect();
        self.report.script_stops = rank_all(self.stops);
        self.report.script_stop_examples = self
            .report
            .script_stops
            .iter()
            .filter_map(|cause| {
                self.stop_examples
                    .remove(&cause.name)
                    .map(|examples| ClassExamples {
                        name: cause.name.clone(),
                        examples,
                    })
            })
            .collect();
        self.report.script_tokens = rank_all(self.tokens);
        self.report.relocation = rank_all(self.relocation);
        self.report.relocation_examples = self
            .report
            .relocation
            .iter()
            .filter_map(|line| {
                self.relocation_examples
                    .remove(&line.name)
                    .map(|examples| ClassExamples {
                        name: line.name.clone(),
                        examples,
                    })
            })
            .collect();
        self.report.undecoded_causes = rank_all(self.undecoded);
        self.report.undecoded_examples = self
            .report
            .undecoded_causes
            .iter()
            .filter_map(|cause| {
                self.undecoded_examples
                    .remove(&cause.name)
                    .map(|examples| ClassExamples {
                        name: cause.name.clone(),
                        examples,
                    })
            })
            .collect();
        self.report.text_histories = rank_all(self.histories);
        self.report.text_flags = rank_all(self.text_flags);
        self.report.twins = rank_all(self.twinned);
        let mut gaps: BTreeMap<String, MappingsGap> = BTreeMap::new();
        for (cause, count) in &failures {
            let Some((name, kind, wanted_slot, declared_slots)) = gap_of(cause) else {
                continue;
            };
            let gap = gaps.entry(name.clone()).or_insert(MappingsGap {
                name,
                kind,
                wanted_slot,
                declared_slots,
                exports: 0,
            });
            gap.exports += count;
            gap.wanted_slot = gap.wanted_slot.max(wanted_slot);
        }
        let mut gaps: Vec<MappingsGap> = gaps.into_values().collect();
        gaps.sort_by(|a, b| b.exports.cmp(&a.exports).then(a.name.cmp(&b.name)));
        self.report.mappings_gaps = gaps;
        // Failures are what a reader hunts through, so the tail of singletons stays visible.
        self.report.top_failures = rank_top(failures, 100);
        self.report.failure_examples = self
            .report
            .top_failures
            .iter()
            .filter_map(|cause| {
                self.failure_examples
                    .remove(&cause.name)
                    .map(|examples| ClassExamples {
                        name: cause.name.clone(),
                        examples,
                    })
            })
            .collect();
        let mut classes: Vec<ClassStatus> = self.classes.into_values().collect();
        classes.sort_by_key(|c| std::cmp::Reverse(c.exact + c.payload + c.unexplained + c.failed));
        classes.truncate(30);
        self.report.status_by_class = classes;
        self.report
    }
}

/// A class or struct absent from the mappings is a coverage problem in the .usmap, which no
/// amount of parser work can fix.
fn is_mappings_gap(cause: &str) -> bool {
    cause.contains("is not in the mappings file")
        || cause.contains("no native layout and no schema")
}

/// Like [`rank`] but keeps every entry, for censuses where the tail is the interesting part.
fn rank_all(counts: BTreeMap<String, usize>) -> Vec<Count> {
    let mut ranked: Vec<Count> = counts
        .into_iter()
        .map(|(name, count)| Count { name, count })
        .collect();
    ranked.sort_by(|a, b| b.count.cmp(&a.count).then(a.name.cmp(&b.name)));
    ranked
}

fn rank(counts: BTreeMap<String, usize>) -> Vec<Count> {
    rank_top(counts, 25)
}

fn rank_top(counts: BTreeMap<String, usize>, keep: usize) -> Vec<Count> {
    let mut ranked: Vec<Count> = counts
        .into_iter()
        .map(|(name, count)| Count { name, count })
        .collect();
    ranked.sort_by(|a, b| b.count.cmp(&a.count).then(a.name.cmp(&b.name)));
    ranked.truncate(keep);
    ranked
}

/// `ETextHistoryType` by name, for the histogram of text layouts the data uses.
fn text_history_label(history: i8) -> &'static str {
    match history {
        -1 => "None",
        0 => "Base",
        1 => "NamedFormat",
        2 => "OrderedFormat",
        3 => "ArgumentFormat",
        4 => "AsNumber",
        5 => "AsPercent",
        6 => "AsCurrency",
        7 => "AsDate",
        8 => "AsTime",
        9 => "AsDateTime",
        10 => "Transform",
        11 => "StringTableEntry",
        12 => "TextGenerator",
        _ => "unknown",
    }
}

/// Collapse per-asset detail (offsets, row numbers) so the histogram groups real causes.
/// Where two runs of bytes that should match first part.
fn same_bytes(what: &str, was: &[u8], now: &[u8]) -> Result<(), String> {
    if was == now {
        return Ok(());
    }
    let at = was
        .iter()
        .zip(now)
        .position(|(a, b)| a != b)
        .unwrap_or(was.len().min(now.len()));
    Err(format!(
        "the {what} came back {} bytes long where it was {}, first differing at {at:#X}",
        now.len(),
        was.len()
    ))
}

fn short_reason(reason: &str) -> String {
    let trimmed = reason.split(" at offset ").next().unwrap_or(reason);
    let trimmed = match trimmed.find("): ") {
        Some(index) if trimmed.starts_with("row ") => &trimmed[index + 3..],
        _ => trimmed,
    };
    // A read inside a struct names the property path it was in before the cause, which would file
    // one cause under as many names as there are paths to it.
    let mut cause = trimmed;
    while let Some((path, rest)) = cause.split_once(": ") {
        if path.contains('.') && !path.contains(' ') {
            cause = rest;
        } else {
            break;
        }
    }
    cause.chars().take(120).collect()
}

/// The game's newest patch container, which declares every package the game loads. A read through
/// it resolves each package from the container that wins, so walking it walks the game as it loads.
pub fn newest_patch(game_root: &str) -> Result<String, String> {
    rivals_core::asset::newest_patch(game_root)
}

pub fn print_audit(report: &AuditReport, out: &mut impl FnMut(String)) {
    out(format!("container   {}", report.container));
    if let Some(warning) = &report.mappings_warning {
        out(format!("warning     {warning}"));
    }
    out(format!("packages    {}", report.packages_scanned));
    out(format!(
        "exports     {} total, {} exact, {} known payload, {} unexplained, {} failed",
        report.exports_total,
        report.exports_complete,
        report.exports_payload,
        report.exports_partial,
        report.exports_failed
    ));
    if report.exports_blueprint_skipped > 0 {
        out(format!(
            "            {} Blueprint-class export(s) left out by --skip-blueprint",
            report.exports_blueprint_skipped
        ));
    }
    out(format!(
        "decoded     {:.2}% of property blocks ({:.2}% also consumed every declared byte)",
        report.decoded_percent, report.exact_percent
    ));
    out(format!(
        "failures    {} from gaps in the mappings file, {} from decoding, across {} distinct causes",
        report.failed_missing_from_mappings,
        report.exports_failed - report.failed_missing_from_mappings,
        report.distinct_failure_causes
    ));
    print_counts("property types read", &report.property_kinds, out);
    print_counts("top failures", &report.top_failures, out);
    print_examples("failure examples", &report.failure_examples, out);
    print_counts("unresolved structs", &report.unresolved_structs, out);
    print_class_status(&report.status_by_class, out);
    print_counts("payload kinds", &report.payload_kinds, out);
    if report.scripts_total > 0 {
        out(String::new());
        out(format!(
            "scripts     {} read, {} to their end, {} stopped",
            report.scripts_total, report.scripts_complete, report.scripts_stopped
        ));
        print_counts("script stops", &report.script_stops, out);
        print_examples("script stop examples", &report.script_stop_examples, out);
        print_counts("bytecode tokens", &report.script_tokens, out);
        print_counts("bytecode relocation", &report.relocation, out);
        print_examples(
            "bytecode relocation examples",
            &report.relocation_examples,
            out,
        );
    }
    if report.undecoded_payloads > 0 {
        out(String::new());
        out(format!(
            "{} instanced struct payload(s) did not decode; their exports still read as exact, because each payload's length prefix puts the cursor back",
            report.undecoded_payloads
        ));
        print_counts("undecoded payload causes", &report.undecoded_causes, out);
        print_examples(
            "undecoded payload examples",
            &report.undecoded_examples,
            out,
        );
    }
    if report.failed_class_recovery > 0 {
        out(String::new());
        out(format!(
            "{} class, function or struct layout(s) could not be followed to their end; each is measured as a payload and a class among them loses the definition its instances need",
            report.failed_class_recovery
        ));
        print_counts("layout walk failures", &report.class_recovery_failures, out);
        print_examples(
            "layout walk failure examples",
            &report.class_recovery_examples,
            out,
        );
    }
    print_counts("FText histories read", &report.text_histories, out);
    print_counts("FText flags by history", &report.text_flags, out);
    if report.packages_tagged > 0 {
        out(format!(
            "{} package(s) store tagged properties rather than unversioned ones",
            report.packages_tagged
        ));
    }
    if report.packages_twinned > 0 {
        out(format!(
            "{} package(s) read under a mappings entry other than the default for a name the file holds twice",
            report.packages_twinned
        ));
        print_counts("twins chosen", &report.twins, out);
    }
    if report.headers_checked > 0 {
        out(format!(
            "unversioned headers re-encoded: {} checked, {} differing",
            report.headers_checked, report.headers_differing
        ));
        out(format!(
            "package headers re-serialized: {} of {} did not round trip",
            report.packages_header_broken, report.packages_scanned
        ));
        print_counts("unversioned header shapes", &report.header_shapes, out);
    }
    if report.resources_separate + report.resources_inline > 0 {
        out(format!(
            "bulk data resources: {} in separate files, {} inline ({} placed in their export by the index word before them, {} not placed)",
            report.resources_separate,
            report.resources_inline,
            report.inline_placed,
            report.inline_unplaced
        ));
    }
    if !report.mappings_gaps.is_empty() {
        out(String::new());
        out(format!(
            "{} name(s) the mappings file does not describe, blocking {} export(s):",
            report.mappings_gaps.len(),
            report
                .mappings_gaps
                .iter()
                .map(|g| g.exports)
                .sum::<usize>()
        ));
        for gap in report.mappings_gaps.iter().take(40) {
            let detail = match (gap.declared_slots, gap.wanted_slot) {
                (Some(declared), Some(wanted)) => {
                    format!(
                        " (declares {declared} properties, data wants at least {})",
                        wanted + 1
                    )
                }
                _ => " (absent)".to_string(),
            };
            out(format!(
                "  {:>7}  {:<12} {}{detail}",
                gap.exports, gap.kind, gap.name
            ));
        }
    }
    if report.packages_repaired > 0 {
        out(format!(
            "{} package(s) needed a schema slot elided; the mappings declare properties this build does not serialize",
            report.packages_repaired
        ));
        print_counts("elided slots", &report.schema_fixups, out);
    }
    print_counts("unexplained tails by class", &report.partial_classes, out);
    print_examples("unexplained examples", &report.partial_examples, out);
}

fn print_examples(title: &str, examples: &[ClassExamples], out: &mut impl FnMut(String)) {
    if examples.is_empty() {
        return;
    }
    out(String::new());
    out(format!("{title} (package#export):"));
    for group in examples {
        out(format!("  {}: {}", group.name, group.examples.join(", ")));
    }
}

fn print_class_status(classes: &[ClassStatus], out: &mut impl FnMut(String)) {
    if classes.is_empty() {
        return;
    }
    out(String::new());
    out("by class (exact / payload / unexplained / failed):".to_string());
    let width = classes
        .iter()
        .map(|c| c.name.len())
        .max()
        .unwrap_or(0)
        .min(44);
    for class in classes {
        let mut name = class.name.clone();
        name.truncate(width);
        out(format!(
            "  {name:width$}  {:>7} {:>7} {:>7} {:>7}",
            class.exact, class.payload, class.unexplained, class.failed
        ));
    }
}

fn print_counts(title: &str, counts: &[Count], out: &mut impl FnMut(String)) {
    if counts.is_empty() {
        return;
    }
    out(String::new());
    out(format!("{title}:"));
    let width = counts
        .iter()
        .map(|c| c.count.to_string().len())
        .max()
        .unwrap_or(1);
    for entry in counts {
        out(format!("  {:>width$}  {}", entry.count, entry.name));
    }
}

fn status_label(status: &ExportStatus) -> String {
    match status {
        ExportStatus::Complete => "ok".to_string(),
        ExportStatus::Payload {
            payload_bytes,
            kind,
            ..
        } => format!("{kind} ({payload_bytes} bytes)"),
        ExportStatus::Partial { consumed, expected } => {
            format!("unexplained {consumed}/{expected}")
        }
        ExportStatus::Failed { .. } => "failed".to_string(),
    }
}

/// How far past a failing struct the sweep follows nested struct types.
const NESTED_DEPTH: u32 = 2;

/// Greedy rounds of elision per struct. More than a handful of missing properties means the
/// mappings are wrong about the struct wholesale, which no amount of searching repairs.
const MAX_ROUNDS: usize = 6;

const MAX_EXAMPLES: usize = 3;

fn fixups_of(name: &str, slots: &[usize]) -> rivals_uasset::SchemaFixups {
    let mut fixups = rivals_uasset::SchemaFixups::default();
    for slot in slots {
        fixups.add(name, *slot);
    }
    fixups
}

/// Turns a slot index in the probed (already-elided) numbering back into the original numbering.
fn shift_into(elided: &[usize], slot: usize) -> usize {
    let mut index = slot;
    for skipped in elided {
        if index >= *skipped {
            index += 1;
        }
    }
    index
}

#[derive(Serialize)]
pub struct DiagnoseReport {
    pub source: String,
    pub packages_scanned: usize,
    pub baseline_exact: usize,
    pub baseline_failed: usize,
    pub structs: Vec<StructDiagnosis>,
}

#[derive(Serialize)]
pub struct StructDiagnosis {
    pub name: String,
    pub slots: usize,
    /// Failing exports whose innermost reported struct was this one.
    pub attributed: usize,
    /// Example packages, to make a finding checkable by hand.
    pub examples: Vec<String>,
    /// One entry per greedy round: the best slot to elide given the ones already elided.
    pub rounds: Vec<SlotCandidate>,
    /// Every slot tried in the first round, for spotting ties.
    pub candidates: Vec<SlotCandidate>,
}

#[derive(Serialize, Clone)]
pub struct SlotCandidate {
    pub slot: usize,
    pub property: String,
    pub kind: String,
    /// The struct in the inheritance chain that declares the property.
    pub owner: String,
    /// Change in failing exports across the whole scan. Negative is an improvement.
    pub failed_delta: i64,
    pub exact_delta: i64,
}

/// Tries every pair of slots. Only reached when no single elision helps, and only up to
/// `MAX_PAIR_SLOTS` so a wide struct cannot turn into a quadratic scan of the whole corpus.
const MAX_PAIR_SLOTS: usize = 60;

fn best_pair(
    slots: usize,
    score: &impl Fn(&[usize]) -> (i64, i64),
    describe: &impl Fn(&[usize], usize, i64, i64) -> SlotCandidate,
    progress: &mut impl FnMut(&str, usize, usize),
    target: &str,
) -> Option<Vec<SlotCandidate>> {
    if slots > MAX_PAIR_SLOTS {
        return None;
    }
    let mut best: Option<(i64, i64, usize, usize)> = None;
    for first in 0..slots {
        progress(target, first + 1, slots);
        for second in first..slots {
            let pair = [first, second + 1];
            let (failed_delta, exact_delta) = score(&pair);
            if failed_delta >= 0 && exact_delta <= 0 {
                continue;
            }
            let better = best.is_none_or(|(f, e, _, _)| {
                failed_delta < f || (failed_delta == f && exact_delta > e)
            });
            if better {
                best = Some((failed_delta, exact_delta, first, second + 1));
            }
        }
    }
    let (failed_delta, exact_delta, first, second) = best?;
    Some(vec![
        describe(&[], first, failed_delta, exact_delta),
        describe(&[first], second - 1, failed_delta, exact_delta),
    ])
}

/// Reasons are chained as `Outer.Prop: Inner.Prop: cause`, so the last `Struct.Property` segment
/// names the struct the reader was inside when it gave up.
fn failing_struct(reason: &str) -> Option<&str> {
    let plain = |s: &str| {
        !s.is_empty()
            && s.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    reason
        .split(": ")
        .filter_map(|segment| {
            let (owner, property) = segment.split_once('.')?;
            let property = property.split('[').next().unwrap_or(property);
            (plain(owner) && plain(property)).then_some(owner)
        })
        .last()
}

struct Tally {
    exact: usize,
    failed: usize,
    structs: Vec<String>,
}

impl Tally {
    fn empty() -> Self {
        Self {
            exact: 0,
            failed: 0,
            structs: Vec::new(),
        }
    }
}

fn tally(parsed: &ParsedPackage) -> Tally {
    let mut out = Tally::empty();
    for export in &parsed.exports {
        match &export.status {
            ExportStatus::Complete => out.exact += 1,
            ExportStatus::Failed { reason } => {
                out.failed += 1;
                if let Some(name) = failing_struct(reason) {
                    out.structs.push(name.to_string());
                }
            }
            _ => {}
        }
    }
    out
}

/// Re-parses every package with one schema slot elided, to find properties the mappings declare
/// that the shipped build does not serialize.
pub struct DiagnoseRequest<'a> {
    /// Folder of extracted packages to walk, instead of a container.
    pub dir: Option<&'a str>,
    pub game_root: &'a str,
    pub container: &'a str,
    pub limit: Option<usize>,
    pub filter: Option<&'a str>,
    /// Sweep only this struct.
    pub only: Option<&'a str>,
    /// How many slots to elide at once when no single one helps.
    pub depth: usize,
    pub usmap: Option<&'a str>,
    pub configured_usmap: Option<&'a str>,
}

pub fn diagnose(
    request: &DiagnoseRequest<'_>,
    mut progress: impl FnMut(&str, usize, usize),
) -> Result<DiagnoseReport, String> {
    let &DiagnoseRequest {
        dir,
        game_root,
        container,
        limit,
        filter,
        only,
        depth,
        usmap,
        configured_usmap,
    } = request;
    let path = mappings::resolve(usmap, configured_usmap)?;
    let schema = mappings::load(&path)?;
    let filter = asset::PathFilter::new(filter);
    let matches = |path: &str| filter.matches(path);

    let mut cached: Vec<(String, Vec<u8>, Vec<u8>)> = Vec::new();
    let source = match dir {
        Some(dir) => {
            let files: Vec<_> = asset::list_loose_packages(Path::new(dir))?
                .into_iter()
                .filter(|p| matches(&p.to_string_lossy()))
                .collect();
            let total = limit.map_or(files.len(), |l| l.min(files.len()));
            for (index, file) in files.iter().take(total).enumerate() {
                progress("loading", index + 1, total);
                if let Ok(bundle) = asset::load_from_disk(file) {
                    cached.push((
                        file.to_string_lossy().into_owned(),
                        bundle.asset_file_buffer,
                        bundle.exports_file_buffer,
                    ));
                }
            }
            dir.to_string()
        }
        None => {
            let (store, packages) = asset::list_packages(game_root, container)?;
            let packages: Vec<_> = packages.into_iter().filter(|(_, p)| matches(p)).collect();
            let total = limit.map_or(packages.len(), |l| l.min(packages.len()));
            let converter = asset::PackageConverter::new(&*store);
            for (index, (package_id, path)) in packages.iter().take(total).enumerate() {
                progress("loading", index + 1, total);
                if let Ok(bundle) = converter.convert(*package_id, path) {
                    cached.push((
                        path.clone(),
                        bundle.asset_file_buffer,
                        bundle.exports_file_buffer,
                    ));
                }
            }
            container.to_string()
        }
    };

    let bundles: Vec<AssetBundle<'_>> = cached
        .iter()
        .map(|(_, asset, exports)| AssetBundle { asset, exports })
        .collect();
    // The Blueprint layouts recovered from the game's own packages, which `audit` and every other
    // command read with. Without them each instance of a class the mappings lack would count here
    // as a failure no schema slot can explain.
    let synths: Vec<Option<std::sync::Arc<rivals_uasset::Mappings>>> = cached
        .iter()
        .zip(&bundles)
        .enumerate()
        .map(|(index, ((entry, _, _), bundle))| {
            progress("synthesising", index + 1, bundles.len());
            let source = PackageSource {
                game_root,
                container: if dir.is_some() { "" } else { container },
                entry,
                kind: if dir.is_some() {
                    AssetSource::Loose
                } else {
                    AssetSource::Utoc
                },
            };
            schema_synth::synthesised(bundle, Some(&schema), &source)
                .ok()
                .flatten()
        })
        .collect();

    let mut baseline = Vec::with_capacity(bundles.len());
    let mut attributed: BTreeMap<String, usize> = BTreeMap::new();
    let mut examples: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (index, bundle) in bundles.iter().enumerate() {
        progress("baseline", index + 1, bundles.len());
        let synth = synths.get(index).cloned().flatten();
        let result = rivals_uasset::parse_package_with(bundle, Some(&schema), synth.as_deref())
            .map(|parsed| tally(&parsed))
            .unwrap_or_else(|_| Tally::empty());
        for name in &result.structs {
            *attributed.entry(name.clone()).or_default() += 1;
            let seen = examples.entry(name.clone()).or_default();
            let path = cached
                .get(index)
                .map(|(p, _, _)| p.clone())
                .unwrap_or_default();
            if seen.len() < MAX_EXAMPLES && !seen.contains(&path) {
                seen.push(path);
            }
        }
        baseline.push(result);
    }

    // A nested struct that reads the wrong width desyncs its parent without erroring itself, so
    // the sweep has to cover the structs reachable from a failing one, not just the failing one.
    let mut widened: BTreeMap<String, usize> = attributed.clone();
    for name in attributed.keys() {
        for nested in schema.struct_references(name, NESTED_DEPTH) {
            widened.entry(nested).or_default();
        }
    }
    let mut targets: Vec<(String, usize)> = widened
        .into_iter()
        .filter(|(name, _)| only.is_none_or(|s| s == name))
        .collect();
    targets.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let mut structs = Vec::new();
    for (target, count) in targets {
        // Probing every package for every slot dominates the runtime, and only packages that
        // already fail can be fixed, so the search runs on those and the winner is verified
        // against the whole set afterwards.
        let relevant: Vec<usize> = (0..bundles.len())
            .filter(|i| {
                baseline
                    .get(*i)
                    .is_some_and(|t| t.structs.contains(&target))
            })
            .collect();
        let relevant: Vec<usize> = if relevant.is_empty() {
            (0..bundles.len())
                .filter(|i| baseline.get(*i).is_some_and(|t| t.failed > 0))
                .collect()
        } else {
            relevant
        };
        let score = |elided: &[usize]| {
            let (mut failed_delta, mut exact_delta) = (0i64, 0i64);
            for index in &relevant {
                let (Some(bundle), Some(before)) = (bundles.get(*index), baseline.get(*index))
                else {
                    continue;
                };
                let Ok(parsed) = rivals_uasset::parse_package_probed(
                    bundle,
                    Some(&schema),
                    synths.get(*index).and_then(Option::as_deref),
                    &fixups_of(&target, elided),
                ) else {
                    continue;
                };
                let after = tally(&parsed);
                failed_delta += after.failed as i64 - before.failed as i64;
                exact_delta += after.exact as i64 - before.exact as i64;
            }
            (failed_delta, exact_delta)
        };
        let describe = |elided: &[usize], slot: usize, failed_delta, exact_delta| {
            let current = fixups_of(&target, elided);
            let probed = schema.schema_fixed(&target, Some(&current));
            let entry = probed.as_ref().and_then(|s| s.slot(slot));
            SlotCandidate {
                slot: shift_into(elided, slot),
                property: entry
                    .as_ref()
                    .map_or_else(String::new, |e| e.property.name.clone()),
                kind: entry
                    .as_ref()
                    .map_or("?", |e| rivals_uasset::kind_name(&e.property.inner))
                    .to_string(),
                owner: entry.as_ref().map_or("?", |e| e.owner).to_string(),
                failed_delta,
                exact_delta,
            }
        };

        let mut elided: Vec<usize> = Vec::new();
        let mut rounds: Vec<SlotCandidate> = Vec::new();
        let mut first_round: Vec<SlotCandidate> = Vec::new();
        let mut slots = schema.schema(&target).map_or(0, |s| s.len());
        for round in 0..MAX_ROUNDS {
            let current = fixups_of(&target, &elided);
            let Some(probed) = schema.schema_fixed(&target, Some(&current)) else {
                break;
            };
            slots = probed.len();
            let mut candidates = Vec::new();
            for slot in 0..slots {
                progress(&target, slot + 1, slots);
                let mut trial = elided.clone();
                trial.push(shift_into(&elided, slot));
                trial.sort_unstable();
                let (failed_delta, exact_delta) = score(&trial);
                candidates.push(describe(&elided, slot, failed_delta, exact_delta));
            }
            candidates.retain(|c| c.failed_delta < 0 || c.exact_delta > 0);
            candidates.sort_by(|a, b| {
                a.failed_delta
                    .cmp(&b.failed_delta)
                    .then_with(|| b.exact_delta.cmp(&a.exact_delta))
            });
            if round == 0 {
                first_round.clone_from(&candidates);
            }
            let best = match candidates.into_iter().next() {
                Some(best) => best,
                // Two properties can be missing at once, in which case removing either alone still
                // desyncs; only the pair shows up as an improvement.
                None if round == 0 && depth >= 2 => {
                    match best_pair(slots, &score, &describe, &mut progress, &target) {
                        Some(pair) => {
                            first_round = pair.clone();
                            elided.extend(pair.iter().map(|c| c.slot));
                            elided.sort_unstable();
                            rounds.extend(pair);
                            continue;
                        }
                        None => break,
                    }
                }
                None => break,
            };
            elided.push(best.slot);
            elided.sort_unstable();
            let done = best.failed_delta <= -(count as i64);
            rounds.push(best);
            if done {
                break;
            }
        }

        // The search only saw failing packages, so re-score the winner over everything to catch
        // exports it breaks.
        if !elided.is_empty() {
            let (mut failed_delta, mut exact_delta) = (0i64, 0i64);
            for ((bundle, before), synth) in bundles.iter().zip(&baseline).zip(&synths) {
                let Ok(parsed) = rivals_uasset::parse_package_probed(
                    bundle,
                    Some(&schema),
                    synth.as_deref(),
                    &fixups_of(&target, &elided),
                ) else {
                    continue;
                };
                let after = tally(&parsed);
                failed_delta += after.failed as i64 - before.failed as i64;
                exact_delta += after.exact as i64 - before.exact as i64;
            }
            if let Some(last) = rounds.last_mut() {
                last.failed_delta = failed_delta;
                last.exact_delta = exact_delta;
            }
        }

        structs.push(StructDiagnosis {
            slots,
            attributed: count,
            examples: examples.get(&target).cloned().unwrap_or_default(),
            name: target,
            rounds,
            candidates: first_round,
        });
    }

    Ok(DiagnoseReport {
        source,
        packages_scanned: bundles.len(),
        baseline_exact: baseline.iter().map(|t| t.exact).sum(),
        baseline_failed: baseline.iter().map(|t| t.failed).sum(),
        structs,
    })
}

pub fn print_diagnose(report: &DiagnoseReport, out: &mut impl FnMut(String)) {
    out(format!(
        "{}: {} packages, baseline {} exact / {} failed",
        report.source, report.packages_scanned, report.baseline_exact, report.baseline_failed
    ));
    let row = |c: &SlotCandidate| {
        format!(
            "  {:>4}  {:>+6}  {:>+5}  {} : {} ({})",
            c.slot, c.failed_delta, c.exact_delta, c.property, c.kind, c.owner
        )
    };
    for entry in &report.structs {
        if entry.attributed == 0 && entry.rounds.is_empty() {
            continue;
        }
        out(String::new());
        out(format!(
            "{} ({} slots, {} failing exports)",
            entry.name, entry.slots, entry.attributed
        ));
        for path in &entry.examples {
            out(format!("  eg {path}"));
        }
        if entry.rounds.is_empty() {
            out("  no elided slot fixes any export".to_string());
            continue;
        }
        out("  elide in order:".to_string());
        out("  slot  failed  exact  property".to_string());
        for candidate in &entry.rounds {
            out(row(candidate));
        }
        let ties: Vec<_> = entry
            .candidates
            .iter()
            .filter(|c| Some(c.failed_delta) == entry.rounds.first().map(|r| r.failed_delta))
            .collect();
        if ties.len() > 1 {
            out(format!("  {} slots tie in round 1:", ties.len()));
            for candidate in ties {
                out(row(candidate));
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_container_path_selects_the_iostore_loader() {
        assert!(matches!(source_of("a/b/pakchunk0.utoc"), AssetSource::Utoc));
        assert!(matches!(source_of("a/b/MyMod.pak"), AssetSource::Pak));
    }

    #[test]
    fn failure_reasons_lose_their_per_asset_offsets_so_they_group() {
        assert_eq!(
            short_reason("struct Foo has no native layout at offset 0x1234"),
            "struct Foo has no native layout"
        );
    }

    /// A cause reached inside a struct is filed under the cause, not under each path to it, so it
    /// groups whatever class it turned up in and still reads as a mappings gap.
    #[test]
    fn a_nested_cause_groups_under_itself() {
        let deep = "/Game/A/Thing.Thing_C.Points: PointParameters.Order: struct PointParameters \
                    has no native layout and no schema in the mappings file at offset 0x2F0A";
        let cause = short_reason(deep);
        assert_eq!(
            cause,
            "struct PointParameters has no native layout and no schema in the mappings file"
        );
        assert!(is_mappings_gap(&cause));
        assert_eq!(
            short_reason("parse package header: bad magic"),
            "parse package header: bad magic"
        );
    }

    #[test]
    fn a_missing_class_is_classified_as_a_mappings_gap_not_a_decode_failure() {
        assert!(is_mappings_gap(
            "class SM_Grass_C is not in the mappings file"
        ));
        assert!(is_mappings_gap(
            "struct Foo has no native layout and no schema in the mappings file"
        ));
        assert!(!is_mappings_gap("negative array count -7"));
    }

    #[test]
    fn ranking_puts_the_most_common_cause_first() {
        let mut counts = BTreeMap::new();
        counts.insert("rare".to_string(), 1);
        counts.insert("common".to_string(), 9);
        let ranked = rank(counts);
        assert_eq!(ranked[0].name, "common");
    }

    /// A row reads as plain JSON whole: nested structs as objects, lists, maps keyed by name or as
    /// pairs, a default as its type's zero and an unstored value as null.
    #[test]
    fn a_table_value_reads_as_plain_nested_json() {
        use rivals_uasset::MapEntry;
        use serde_json::json;

        let entry = |name: &str, value: PropertyValue| PropertyEntry {
            name: name.into(),
            element: None,
            value,
            span: None,
            slot: None,
        };
        let skins = PropertyValue::Struct {
            name: "SkinList".into(),
            fields: vec![entry(
                "SkinIDs",
                PropertyValue::Array {
                    items: vec![
                        PropertyValue::Int { value: 309 },
                        PropertyValue::Int { value: 312 },
                    ],
                },
            )],
        };
        let row = PropertyValue::Struct {
            name: "Row".into(),
            fields: vec![
                entry(
                    "HeroSkinsMap",
                    PropertyValue::Map {
                        entries: vec![MapEntry {
                            key: PropertyValue::Int { value: 1047 },
                            value: skins.clone(),
                        }],
                    },
                ),
                entry(
                    "ByStruct",
                    PropertyValue::Map {
                        entries: vec![MapEntry {
                            key: skins,
                            value: PropertyValue::Bool { value: true },
                        }],
                    },
                ),
                entry(
                    "Slot",
                    PropertyValue::Enum {
                        value: 3,
                        name: Some("HERO_UPGRADED_SKIN_FX".into()),
                        enum_type: None,
                    },
                ),
                entry(
                    "Count",
                    PropertyValue::Default {
                        declared: Some("Int"),
                        fields: Vec::new(),
                    },
                ),
                entry(
                    "Inherited",
                    PropertyValue::Unset {
                        declared: "Struct",
                        enum_type: None,
                        fields: Vec::new(),
                    },
                ),
            ],
        };
        assert_eq!(
            plain_value(&row),
            json!({
                "HeroSkinsMap": { "1047": { "SkinIDs": [309, 312] } },
                "ByStruct": [{ "key": { "SkinIDs": [309, 312] }, "value": true }],
                "Slot": "HERO_UPGRADED_SKIN_FX",
                "Count": 0,
                "Inherited": null
            })
        );
    }

    /// A dump opens every map, set and nested struct into lines beneath it: a map entry under its
    /// key, or under `key` and `value` when the key is a struct of its own.
    #[test]
    fn a_dump_prints_maps_and_sets_whole() {
        use rivals_uasset::MapEntry;

        let entry = |name: &str, value: PropertyValue| PropertyEntry {
            name: name.into(),
            element: None,
            value,
            span: None,
            slot: None,
        };
        // A string field keeps it from reading inline the way a vector does.
        let point = |x: i64| PropertyValue::Struct {
            name: "Point".into(),
            fields: vec![
                entry("X", PropertyValue::Int { value: x }),
                entry(
                    "Label",
                    PropertyValue::Str {
                        value: "spawn".into(),
                    },
                ),
            ],
        };
        let entries = vec![
            entry(
                "Icons",
                PropertyValue::Map {
                    entries: vec![MapEntry {
                        key: PropertyValue::Enum {
                            value: 0,
                            name: Some("EHeroRole::Tank".into()),
                            enum_type: None,
                        },
                        value: PropertyValue::Str {
                            value: "icon_front".into(),
                        },
                    }],
                },
            ),
            entry(
                "ByPoint",
                PropertyValue::Map {
                    entries: vec![MapEntry {
                        key: point(1),
                        value: PropertyValue::Bool { value: true },
                    }],
                },
            ),
            entry(
                "Tags",
                PropertyValue::Set {
                    items: vec![PropertyValue::Name {
                        value: "Hero".into(),
                    }],
                },
            ),
            entry(
                "Points",
                PropertyValue::Array {
                    items: vec![point(2)],
                },
            ),
        ];
        let mut lines = Vec::new();
        print_entries(&entries, 0, &Inherits::none(), &mut |line| lines.push(line));
        assert_eq!(
            lines,
            [
                "  Icons: {1 entries}",
                "    EHeroRole::Tank: icon_front",
                "  ByPoint: {1 entries}",
                "    {entry}",
                "      key: Point",
                "        X: 1",
                "        Label: spawn",
                "      value: true",
                "  Tags: [1]",
                "    [0] Hero",
                "  Points: [1]",
                "    [0] Point",
                "      X: 2",
                "      Label: spawn",
            ]
        );
    }

    #[test]
    fn field_records_print_their_types_as_a_reader_writes_them() {
        let report = FieldsReport {
            name: "Thing_C".into(),
            super_struct: Some("Actor".into()),
            fields: vec![FieldRecord {
                index: 0,
                name: "Offsets".into(),
                kind: "Array<Vector>".into(),
                array_dim: 2,
            }],
        };
        let mut lines = Vec::new();
        print_fields(&report, &mut |line| lines.push(line));
        assert_eq!(
            lines,
            [
                "Thing_C : Actor  (1 fields)",
                "     0  Offsets[2]: Array<Vector>"
            ]
        );
    }
}
