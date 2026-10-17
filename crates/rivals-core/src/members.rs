//! A Blueprint class's variables and functions with everything that names each one: in the
//! package declaring them and, when that package is in a mod, in the mod's other packages.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use rivals_uasset::{Mappings, Member, MemberKind, MemberTarget, MemberUses, ParsedPackage};
use serde::Serialize;

use crate::asset::{self, AssetSource};
use crate::asset_edit::{AssetEditRequest, bundle_of, read_package};

/// A member's uses in one package of the mod other than the one declaring it.
#[derive(Debug, Clone, Serialize)]
pub struct PackageUses {
    pub entry: String,
    pub uses: MemberUses,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemberSummary {
    #[serde(flatten)]
    pub member: Member,
    /// Its uses in the package declaring it.
    pub here: MemberUses,
    /// Its uses in the mod's other packages that name it.
    pub elsewhere: Vec<PackageUses>,
}

impl MemberSummary {
    /// Whether nothing the packages hold runs or reads the member: no call, no field path, no
    /// binding, no literal naming it, nothing that could not be placed, and it is not something
    /// the engine reaches by its name or by the class's layout.
    pub fn unused(&self) -> bool {
        let quiet =
            |uses: &MemberUses| uses.live().next().is_none() && uses.unclassified.is_empty();
        self.removable()
            && quiet(&self.here)
            && self.elsewhere.iter().all(|other| quiet(&other.uses))
    }

    /// Whether a variable or local is only ever stored into: every use that would keep it is an
    /// assignment statement, so nothing reads what it holds.
    pub fn write_only(&self) -> bool {
        let quiet =
            |uses: &MemberUses| uses.reads().next().is_none() && uses.unclassified.is_empty();
        let assigned = |uses: &MemberUses| {
            uses.uses
                .iter()
                .any(|one| one.role == rivals_uasset::UseRole::Assigned)
        };
        self.member.kind != MemberKind::Function
            && self.removable()
            && quiet(&self.here)
            && self.elsewhere.iter().all(|other| quiet(&other.uses))
            && (assigned(&self.here) || self.elsewhere.iter().any(|other| assigned(&other.uses)))
    }

    /// Not something the engine reaches by its name or by the class's layout, whatever its uses.
    /// The event graph is reached only through the events entering it, which are its uses, so one
    /// no event enters is code nothing runs.
    fn removable(&self) -> bool {
        let bound_widget = self.member.kind == MemberKind::Variable && self.member.export.is_some();
        // The event graph's frame, which the class's layout holds and every event writes to. It
        // goes with the graph, never alone.
        let frame = self.member.detail == "PointerToUberGraphFrame";
        !self.member.overrides
            && !self.member.parameter
            && !frame
            && !bound_widget
            && !self.anim_graph()
            && self.member.detail != "delegate signature"
    }

    /// One of an Anim Blueprint's graph nodes or subsystems, which its class runs from lists of
    /// its own by where each sits among the class's variables, with no bytecode naming them.
    fn anim_graph(&self) -> bool {
        self.member.kind == MemberKind::Variable
            && (self.member.detail.starts_with("AnimNode_")
                || self.member.detail.starts_with("AnimSubsystem")
                || self.member.name.starts_with("AnimGraphNode_")
                || self.member.name.starts_with("AnimBlueprintExtension_"))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MemberReport {
    /// The class's path, or the function's for its parameters and locals.
    pub owner: String,
    pub members: Vec<MemberSummary>,
    /// How many of the mod's other packages were read for uses.
    pub others_read: usize,
    /// The mod's packages that could not be read, which may name a member unseen.
    pub unreadable: Vec<String>,
}

/// The class export a report is about: the one named, else the package's first generated class.
pub fn class_export(parsed: &ParsedPackage, class: Option<u32>) -> Result<u32, String> {
    match class {
        Some(index) => {
            let export = parsed
                .exports
                .get(index as usize)
                .ok_or_else(|| format!("this package has no export {index}"))?;
            if export.layout.is_none() {
                return Err(format!(
                    "{} is a {}, not a class whose layout was read",
                    export.path, export.class_name
                ));
            }
            Ok(index)
        }
        None => parsed
            .exports
            .iter()
            .find(|export| export.class_name.ends_with("GeneratedClass") && export.layout.is_some())
            .map(|export| export.index)
            .ok_or_else(|| "this package holds no Blueprint class; name one with --export".into()),
    }
}

/// Whether `container` is a mod's, whose other packages a member may be named from: any IoStore
/// container but the game's own in `Paks`, wherever its files are and switched on or off.
pub fn is_mod_container(game_root: &str, container: &str) -> bool {
    let path = Path::new(container.strip_suffix(".disabled").unwrap_or(container));
    if !path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("utoc"))
    {
        return false;
    }
    // A bare name is one of the game's containers, which is how one is named.
    let Some(folder) = path
        .parent()
        .filter(|folder| !folder.as_os_str().is_empty())
    else {
        return false;
    };
    let paks = crate::paths::paks_dir(game_root);
    let in_paks = match (folder.canonicalize(), paks.canonicalize()) {
        (Ok(folder), Ok(paks)) => folder == paks,
        _ => folder == paks,
    };
    !in_paks
}

/// The target a member of `owner` stands for.
fn target_of(
    parsed: &ParsedPackage,
    owner: u32,
    member: &Member,
    mappings: Option<&Mappings>,
) -> MemberTarget {
    let owner_path = parsed.exports[owner as usize].path.clone();
    match member.kind {
        MemberKind::Local => {
            let outer = parsed.exports[owner as usize].outer_index;
            let (class, ancestors) = if outer > 0 {
                let class = outer as u32 - 1;
                (
                    parsed.exports[class as usize].path.clone(),
                    rivals_uasset::ancestors_of(parsed, class, mappings),
                )
            } else {
                (String::new(), Vec::new())
            };
            MemberTarget {
                class,
                function: Some(owner_path),
                name: member.name.clone(),
                kind: MemberKind::Local,
                ancestors,
            }
        }
        kind => MemberTarget {
            class: owner_path,
            function: None,
            name: member.name.clone(),
            kind,
            ancestors: rivals_uasset::ancestors_of(parsed, owner, mappings),
        },
    }
}

/// The members of a class, or of a function with `function`, and what names each, reading the
/// mod's other packages when the package is in one. `only` keeps one member by name.
pub fn member_report(
    request: &AssetEditRequest<'_>,
    mappings: Option<&Mappings>,
    class: Option<u32>,
    function: Option<u32>,
    only: Option<&str>,
) -> Result<MemberReport, String> {
    let scan = (request.kind == AssetSource::Utoc
        && is_mod_container(request.game_root, request.container))
    .then_some(request.container);
    member_report_in(request, mappings, class, function, only, scan)
}

/// The same, reading the other packages of the mod at `scan` for uses.
fn member_report_in(
    request: &AssetEditRequest<'_>,
    mappings: Option<&Mappings>,
    class: Option<u32>,
    function: Option<u32>,
    only: Option<&str>,
    scan: Option<&str>,
) -> Result<MemberReport, String> {
    let (loaded, parsed) = read_package(request, mappings)?;
    let (owner, mut members) = match function {
        Some(function) => (function, rivals_uasset::locals_of(&parsed, function)?),
        None => {
            let class = class_export(&parsed, class)?;
            (class, rivals_uasset::members_of(&parsed, class)?)
        }
    };
    if let Some(only) = only {
        members.retain(|member| member.name.eq_ignore_ascii_case(only));
        if members.is_empty() {
            return Err(format!(
                "{} declares no member named {only}",
                parsed.exports[owner as usize].path
            ));
        }
    }
    let targets: Vec<MemberTarget> = members
        .iter()
        .map(|member| target_of(&parsed, owner, member, mappings))
        .collect();
    let bundle = bundle_of(&loaded);
    let mut summaries = Vec::with_capacity(members.len());
    for (member, target) in members.into_iter().zip(&targets) {
        summaries.push(MemberSummary {
            here: rivals_uasset::member_uses(&bundle, &parsed, target)?,
            member,
            elsewhere: Vec::new(),
        });
    }

    let mut others_read = 0;
    let mut unreadable = Vec::new();
    if let Some(scan) = scan {
        // The package itself is none of the mod's others, however the request names it: a package
        // name is the container path the save resolves it to.
        let own = crate::asset_edit::save_entry(request).ok();
        let (_, packages) = asset::list_packages(request.game_root, scan)?;
        for (_, entry) in &packages {
            if entry.eq_ignore_ascii_case(request.entry)
                || own
                    .as_deref()
                    .is_some_and(|own| entry.eq_ignore_ascii_case(own))
            {
                continue;
            }
            let other = AssetEditRequest {
                game_root: request.game_root,
                container: scan,
                entry,
                kind: AssetSource::Utoc,
                mod_name: "",
                changes: Default::default(),
            };
            let Ok((loaded, parsed)) = read_package(&other, mappings) else {
                unreadable.push(entry.clone());
                continue;
            };
            others_read += 1;
            let bundle = bundle_of(&loaded);
            for (summary, target) in summaries.iter_mut().zip(&targets) {
                let uses = rivals_uasset::member_uses(&bundle, &parsed, target)?;
                if !uses.uses.is_empty()
                    || !uses.possible.is_empty()
                    || !uses.unclassified.is_empty()
                {
                    summary.elsewhere.push(PackageUses {
                        entry: entry.clone(),
                        uses,
                    });
                }
            }
        }
    }
    Ok(MemberReport {
        owner: parsed.exports[owner as usize].path.clone(),
        members: summaries,
        others_read,
        unreadable,
    })
}

/// What renaming a member did, or would do: each package it changes, with the uses it renames.
#[derive(Debug, Clone, Serialize)]
pub struct RenameReport {
    pub member: String,
    pub to: String,
    pub owner: String,
    pub packages: Vec<RenamedPackage>,
    pub warnings: Vec<String>,
    /// Where the result was written, when it was.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pak: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RenamedPackage {
    pub entry: String,
    pub renamed: Vec<rivals_uasset::MemberUse>,
    pub left: Vec<rivals_uasset::MemberUse>,
}

/// Renames a member in one package, checked use by use against the package it leaves.
fn rename_in(
    request: &AssetEditRequest<'_>,
    mappings: Option<&Mappings>,
    loaded: &retoc::legacy_asset::FSerializedAssetBundle,
    parsed: &ParsedPackage,
    rename: &rivals_uasset::MemberRename,
    allow_unchecked: bool,
) -> Result<(rivals_uasset::RenamePlan, rivals_uasset::PatchedBundle), String> {
    let bundle = bundle_of(loaded);
    let plan = rivals_uasset::plan_member_rename(&bundle, parsed, rename, allow_unchecked)?;
    let edits = rivals_uasset::PackageEdits {
        rename_members: vec![rename.clone()],
        allow_unchecked,
        ..Default::default()
    };
    let patched = rivals_uasset::patch_package_with(
        &bundle,
        rivals_uasset::Sidecars {
            bulk: loaded.bulk_data_buffer.as_deref(),
            optional_bulk: loaded.optional_bulk_data_buffer.as_deref(),
        },
        parsed,
        &edits,
        mappings,
    )?;
    let after_bundle = rivals_uasset::AssetBundle {
        asset: &patched.asset,
        exports: &patched.exports,
    };
    let after = crate::schema_synth::parse_package_opts(
        &after_bundle,
        mappings,
        &crate::asset_edit::source_of(request),
        crate::asset_edit::editor_options(),
    )?;
    rivals_uasset::verify_patch(parsed, &after, &edits, &patched.applied)?;
    rivals_uasset::verify_member_rename(
        (&bundle, parsed),
        (&after_bundle, &after),
        rename,
        allow_unchecked,
    )?;
    crate::asset_edit::check_load_order(&edits, parsed, &after)?;
    Ok((plan, patched))
}

/// A package read with its bytes, or why it could not be.
type Read = Result<(retoc::legacy_asset::FSerializedAssetBundle, ParsedPackage), String>;

/// The packages outside the mod that import the class, each read with its bytes, from the game or
/// the enabled mod holding it.
fn outside_importers(
    request: &AssetEditRequest<'_>,
    mappings: Option<&Mappings>,
    class: &str,
    mod_stem: &str,
) -> Result<Vec<(String, Read)>, String> {
    let index = crate::import_index::current(request.game_root)?;
    let mods =
        crate::import_index::enabled_mod_containers(&crate::paths::paks_dir(request.game_root));
    let mut out = Vec::new();
    for listed in index.importers_of(class).packages {
        let (entry, in_mod) = match listed.split_once(" (in ") {
            Some((entry, name)) => (
                entry.to_string(),
                Some(name.trim_end_matches(')').to_string()),
            ),
            None => (listed.clone(), None),
        };
        if in_mod
            .as_deref()
            .is_some_and(|name| name.eq_ignore_ascii_case(mod_stem))
        {
            continue;
        }
        let container = match &in_mod {
            Some(name) => mods
                .iter()
                .find(|container| {
                    container.file_stem().and_then(|stem| stem.to_str()) == Some(name.as_str())
                })
                .map(|container| container.to_string_lossy().into_owned()),
            None => Some("pakchunk0-Windows.utoc".to_string()),
        };
        let read = container
            .ok_or_else(|| "its mod is not enabled".to_string())
            .and_then(|container| {
                let other = AssetEditRequest {
                    game_root: request.game_root,
                    container: &container,
                    entry: &entry,
                    kind: AssetSource::Utoc,
                    mod_name: "",
                    changes: Default::default(),
                };
                read_package(&other, mappings)
            });
        out.push((listed, read));
    }
    Ok(out)
}

/// Renames a Blueprint class's variable or function, or with `function` one of that function's
/// parameters or locals, along with every use of it in the package and in the rest of the mod it
/// is saved into, all in one write. A package outside the mod that uses it is refused, since the
/// rename cannot reach it.
#[allow(clippy::too_many_arguments)]
pub fn rename_member(
    request: &AssetEditRequest<'_>,
    mappings: Option<&Mappings>,
    options: &crate::asset_edit::SaveOptions,
    class: Option<u32>,
    function: Option<u32>,
    member: &str,
    to: &str,
    possible: rivals_uasset::PossibleUses,
    dry_run: bool,
    check_outside: bool,
) -> Result<RenameReport, String> {
    let allow_unchecked = request.changes.allow_unchecked;
    let (loaded, parsed) = read_package(request, mappings)?;
    let (owner, members) = match function {
        Some(function) => (function, rivals_uasset::locals_of(&parsed, function)?),
        None => {
            let class = class_export(&parsed, class)?;
            (class, rivals_uasset::members_of(&parsed, class)?)
        }
    };
    let found = members
        .iter()
        .find(|m| m.name.eq_ignore_ascii_case(member))
        .ok_or_else(|| {
            format!(
                "{} declares no member named {member}",
                parsed.exports[owner as usize].path
            )
        })?;
    let target = target_of(&parsed, owner, found, mappings);
    let rename = rivals_uasset::MemberRename {
        target: target.clone(),
        to: to.trim().to_string(),
        possible,
        declares: true,
    };
    let (plan, patched) = rename_in(
        request,
        mappings,
        &loaded,
        &parsed,
        &rename,
        allow_unchecked,
    )?;
    let entry = crate::asset_edit::save_entry(request)?;
    let mut report = RenameReport {
        member: target.name.clone(),
        to: rename.to.clone(),
        owner: parsed.exports[owner as usize].path.clone(),
        packages: vec![RenamedPackage {
            entry: entry.clone(),
            renamed: plan.renamed,
            left: plan.left,
        }],
        warnings: patched.notes.clone(),
        pak: None,
    };

    // The mod's other packages, each renamed where it uses the member.
    let utoc = crate::asset_edit::mod_pak_path(request.game_root, request.mod_name)?
        .with_extension("utoc");
    let mod_stem = utoc
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
        .to_string();
    let others_rename = rivals_uasset::MemberRename {
        declares: false,
        ..rename.clone()
    };
    let mut followers = Vec::new();
    if utoc.is_file() {
        let container = utoc.to_string_lossy().into_owned();
        let (_, packages) = asset::list_packages(request.game_root, &container)?;
        for (_, other_entry) in &packages {
            if other_entry.eq_ignore_ascii_case(&entry) {
                continue;
            }
            let other = AssetEditRequest {
                game_root: request.game_root,
                container: &container,
                entry: other_entry,
                kind: AssetSource::Utoc,
                mod_name: request.mod_name,
                changes: Default::default(),
            };
            let (other_loaded, other_parsed) = match read_package(&other, mappings) {
                Ok(read) => read,
                Err(e) if allow_unchecked => {
                    report.warnings.push(format!(
                        "{other_entry} could not be read, so a use there was not renamed: {e}"
                    ));
                    continue;
                }
                Err(e) => {
                    return Err(format!(
                        "{other_entry} in the mod could not be read, so a use of {} there would be missed: {e}. Pass --allow-unchecked to rename the rest",
                        target.name
                    ));
                }
            };
            let uses =
                rivals_uasset::member_uses(&bundle_of(&other_loaded), &other_parsed, &target)?;
            if uses.uses.is_empty() && uses.possible.is_empty() && uses.unclassified.is_empty() {
                continue;
            }
            let (plan, patched) = rename_in(
                &other,
                mappings,
                &other_loaded,
                &other_parsed,
                &others_rename,
                allow_unchecked,
            )
            .map_err(|e| format!("{other_entry}: {e}"))?;
            report.warnings.extend(patched.notes.iter().cloned());
            report.packages.push(RenamedPackage {
                entry: other_entry.clone(),
                renamed: plan.renamed,
                left: plan.left,
            });
            followers.push(crate::pak::iostore_out::PackageBytes {
                entry: other_entry.clone(),
                asset: patched.asset,
                exports: patched.exports,
                bulk: patched.bulk.or(other_loaded.bulk_data_buffer),
                optional_bulk: patched
                    .optional_bulk
                    .or(other_loaded.optional_bulk_data_buffer),
                memory_mapped_bulk: other_loaded.memory_mapped_bulk_data_buffer,
                shader_map_hashes: crate::asset_edit::source_shader_maps(
                    &other,
                    &other_loaded.asset_file_buffer,
                ),
            });
        }
    }

    // Packages outside the mod that import the class and use the member, which this cannot reach.
    let outside = if check_outside {
        outside_importers(request, mappings, &target.class, &mod_stem)
    } else {
        Ok(Vec::new())
    };
    match outside {
        Ok(importers) => {
            let mut outside = Vec::new();
            for (listed, read) in importers {
                match read {
                    Ok((other_loaded, other_parsed)) => {
                        let uses = rivals_uasset::member_uses(
                            &bundle_of(&other_loaded),
                            &other_parsed,
                            &target,
                        )?;
                        if !uses.uses.is_empty() {
                            outside.push(listed);
                        }
                    }
                    Err(e) => report.warnings.push(format!(
                        "{listed} imports {} and could not be read: {e}",
                        target.class
                    )),
                }
            }
            if !outside.is_empty() {
                return Err(format!(
                    "{} outside {mod_stem} use(s) {}, and a rename there cannot reach them: {}",
                    outside.len(),
                    target.name,
                    outside.join(", ")
                ));
            }
        }
        Err(e) => report.warnings.push(format!(
            "packages outside the mod that import {} were not checked: {e}",
            target.class
        )),
    }
    if found.kind == MemberKind::Function {
        report.warnings.push(format!(
            "native code and Python call functions by name, and nothing in the packages shows \
             where; {} is renamed wherever a package names it",
            target.name
        ));
    }
    if dry_run {
        return Ok(report);
    }

    if !options.replace
        && !crate::asset_edit::same_file(&utoc, request.container)
        && crate::pak::iostore_out::utoc_holds_entry(&utoc, &entry).unwrap_or(false)
    {
        return Err(format!(
            "{} already holds {entry}; leave out --from to rename it in that copy, or pass --overwrite to start again from the source",
            utoc.file_name().unwrap_or_default().to_string_lossy()
        ));
    }
    crate::pak::containers::drop_cached_store();
    let written = crate::pak::iostore_out::write_replacing_in_iostore(
        &utoc,
        crate::pak::iostore_out::PackageFiles {
            entry: &entry,
            asset: &patched.asset,
            exports: &patched.exports,
            bulk: patched
                .bulk
                .as_deref()
                .or(loaded.bulk_data_buffer.as_deref()),
            optional_bulk: patched
                .optional_bulk
                .as_deref()
                .or(loaded.optional_bulk_data_buffer.as_deref()),
            memory_mapped_bulk: loaded.memory_mapped_bulk_data_buffer.as_deref(),
            shader_map_hashes: crate::asset_edit::source_shader_maps(
                request,
                &loaded.asset_file_buffer,
            ),
        },
        followers,
        &std::collections::HashSet::new(),
        &options.iostore,
    )?;
    report.pak = Some(written.utoc.to_string_lossy().into_owned());
    Ok(report)
}

/// What removing members did, or would do.
#[derive(Debug, Clone, Serialize)]
pub struct RemoveReport {
    pub owner: String,
    /// The members taken out.
    pub removed: Vec<String>,
    /// The binding entries and stored values that went with them, labelled.
    pub followed: Vec<String>,
    /// The packages changed.
    pub packages: Vec<String>,
    /// The event graph code only the events removed reached, which went with them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub graph_code: Vec<rivals_uasset::GraphTrim>,
    /// The event graph locals that left unused, as `graph.local`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub locals: Vec<String>,
    /// The imports the removal left named by nothing, dropped unless `--keep-orphaned-imports`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub imports: Vec<String>,
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pak: Option<String>,
}

/// A package's `.uasset` and `.uexp` bytes.
type Bytes = (Vec<u8>, Vec<u8>);

/// One stage of a removal on bytes already patched, read again and checked the way a save is.
fn stage(
    request: &AssetEditRequest<'_>,
    mappings: Option<&Mappings>,
    bytes: Bytes,
    parsed: &ParsedPackage,
    sidecars: (Option<&[u8]>, Option<&[u8]>),
    edits: &rivals_uasset::PackageEdits,
) -> Result<(Bytes, ParsedPackage, Vec<String>), String> {
    let bundle = rivals_uasset::AssetBundle {
        asset: &bytes.0,
        exports: &bytes.1,
    };
    let patched = rivals_uasset::patch_package_with(
        &bundle,
        rivals_uasset::Sidecars {
            bulk: sidecars.0,
            optional_bulk: sidecars.1,
        },
        parsed,
        edits,
        mappings,
    )?;
    let after = crate::schema_synth::parse_package_opts(
        &rivals_uasset::AssetBundle {
            asset: &patched.asset,
            exports: &patched.exports,
        },
        mappings,
        &crate::asset_edit::source_of(request),
        crate::asset_edit::editor_options(),
    )?;
    rivals_uasset::verify_patch(parsed, &after, edits, &patched.applied)?;
    Ok(((patched.asset, patched.exports), after, patched.notes))
}

/// The removal's stages in the declaring package, each read again and checked the way a save is.
struct Staged {
    bytes: Bytes,
    followed: Vec<String>,
    graph_code: Vec<rivals_uasset::GraphTrim>,
    locals: Vec<String>,
    imports: Vec<String>,
    warnings: Vec<String>,
}

/// The event graph locals a removal left unused: each had a use before and has none now, a
/// parameter excepted. One unused before is the caller's to take out on purpose.
fn graph_locals_left_unused(
    before: (&rivals_uasset::AssetBundle<'_>, &ParsedPackage),
    after: (&rivals_uasset::AssetBundle<'_>, &ParsedPackage),
    graph_path: &str,
    mappings: Option<&Mappings>,
) -> Result<Vec<String>, String> {
    let index_of = |parsed: &ParsedPackage| {
        parsed
            .exports
            .iter()
            .find(|export| export.path == graph_path)
            .map(|export| export.index)
    };
    let (Some(was), Some(now)) = (index_of(before.1), index_of(after.1)) else {
        return Ok(Vec::new());
    };
    Ok(locals_after((before.0, before.1, was), (after.0, after.1, now), mappings)?.left_unused)
}

/// What a save did to a function's locals, a parameter being neither.
#[derive(Debug, Default)]
pub(crate) struct LocalsAfter {
    /// Each had a use before and has none now.
    pub left_unused: Vec<String>,
    /// Each had no use before either.
    pub unused_before: Vec<String>,
}

/// [`LocalsAfter`] for the function at `before.2` as read, which is `after.2` once saved. A local
/// whose use cannot be told is neither.
pub(crate) fn locals_after(
    before: (&rivals_uasset::AssetBundle<'_>, &ParsedPackage, u32),
    after: (&rivals_uasset::AssetBundle<'_>, &ParsedPackage, u32),
    mappings: Option<&Mappings>,
) -> Result<LocalsAfter, String> {
    let (was, now) = (before.2, after.2);
    let still = rivals_uasset::locals_of(after.1, now)?;
    let mut out = LocalsAfter::default();
    for local in rivals_uasset::locals_of(before.1, was)?
        .into_iter()
        .filter(|local| !local.parameter)
    {
        let Some(kept) = still.iter().find(|other| other.name == local.name) else {
            continue;
        };
        let used = rivals_uasset::member_uses(
            before.0,
            before.1,
            &target_of(before.1, was, &local, mappings),
        )?;
        if used.live().next().is_none() {
            if used.unclassified.is_empty() {
                out.unused_before.push(local.name.clone());
            }
            continue;
        }
        let uses =
            rivals_uasset::member_uses(after.0, after.1, &target_of(after.1, now, kept, mappings))?;
        if uses.live().next().is_none() && uses.unclassified.is_empty() {
            out.left_unused.push(local.name.clone());
        }
    }
    Ok(out)
}

/// What one package of a mod holds that nothing in the mod uses.
#[derive(Debug, Clone, Serialize)]
pub struct UnusedInPackage {
    pub entry: String,
    /// Its Blueprint classes' variables and functions no package of the mod runs or reads, as
    /// `asset members --unused` lists them, `Class.Member`, a variable only stored into marked so.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<String>,
    /// Its functions' locals nothing in them names, as `Function.Local`, which `mods tidy` drops.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub locals: Vec<String>,
}

/// What the mod at `container` holds that nothing in it uses, package by package: each Blueprint
/// class's members no package of the mod runs or reads, and each function's locals nothing in it
/// names. Every package is read twice, for its own members and then for the others' uses, so the
/// mod is read whole once per pass rather than once per class. The packages that could not be read
/// come back too, since a use in one of them is not seen.
pub fn mod_unused_members(
    game_root: &str,
    container: &str,
    mappings: Option<&Mappings>,
) -> Result<(Vec<UnusedInPackage>, Vec<String>), String> {
    struct Owned {
        entry: String,
        summaries: Vec<(String, MemberSummary, MemberTarget)>,
        locals: Vec<String>,
    }
    let (_, packages) = asset::list_packages(game_root, container)?;
    let read = |entry: &str| {
        read_package(
            &AssetEditRequest {
                game_root,
                container,
                entry,
                kind: AssetSource::Utoc,
                mod_name: "",
                changes: Default::default(),
            },
            mappings,
        )
    };
    let mut owned = Vec::new();
    let mut unreadable = Vec::new();
    for (_, entry) in &packages {
        let Ok((loaded, parsed)) = read(entry) else {
            unreadable.push(entry.clone());
            continue;
        };
        let bundle = bundle_of(&loaded);
        let locals = unused_locals(&bundle, &parsed, mappings)?
            .into_iter()
            .flat_map(|(function, names)| {
                let function = parsed.exports[function as usize].object_name.clone();
                names
                    .into_iter()
                    .map(move |name| format!("{function}.{name}"))
            })
            .collect();
        let mut summaries = Vec::new();
        for class in parsed.exports.iter().filter(|export| {
            export.class_name.ends_with("GeneratedClass") && export.layout.is_some()
        }) {
            for member in rivals_uasset::members_of(&parsed, class.index)? {
                let target = target_of(&parsed, class.index, &member, mappings);
                let here = rivals_uasset::member_uses(&bundle, &parsed, &target)?;
                summaries.push((
                    class.object_name.clone(),
                    MemberSummary {
                        member,
                        here,
                        elsewhere: Vec::new(),
                    },
                    target,
                ));
            }
        }
        owned.push(Owned {
            entry: entry.clone(),
            summaries,
            locals,
        });
    }
    if owned.iter().any(|one| !one.summaries.is_empty()) {
        for (_, entry) in &packages {
            let Ok((loaded, parsed)) = read(entry) else {
                continue;
            };
            let bundle = bundle_of(&loaded);
            for one in owned
                .iter_mut()
                .filter(|one| !one.entry.eq_ignore_ascii_case(entry))
            {
                for (_, summary, target) in &mut one.summaries {
                    let uses = rivals_uasset::member_uses(&bundle, &parsed, target)?;
                    if !uses.uses.is_empty()
                        || !uses.possible.is_empty()
                        || !uses.unclassified.is_empty()
                    {
                        summary.elsewhere.push(PackageUses {
                            entry: entry.clone(),
                            uses,
                        });
                    }
                }
            }
        }
    }
    let found = owned
        .into_iter()
        .map(|one| UnusedInPackage {
            members: one
                .summaries
                .iter()
                .filter_map(|(class, summary, _)| {
                    let name = format!("{class}.{}", summary.member.name);
                    if summary.unused() {
                        Some(name)
                    } else if summary.write_only() {
                        Some(format!("{name} (written, never read)"))
                    } else {
                        None
                    }
                })
                .collect(),
            locals: one.locals,
            entry: one.entry,
        })
        .filter(|one| !one.members.is_empty() || !one.locals.is_empty())
        .collect();
    Ok((found, unreadable))
}

/// Each function's locals that nothing in it names, by the function's export: no statement reads,
/// writes or otherwise names them, so they go with nothing else changing. A function's locals are
/// its own, so no other package can name them. A local whose uses cannot be told stays.
pub(crate) fn unused_locals(
    bundle: &rivals_uasset::AssetBundle<'_>,
    parsed: &ParsedPackage,
    mappings: Option<&Mappings>,
) -> Result<Vec<(u32, Vec<String>)>, String> {
    let mut out = Vec::new();
    for function in parsed
        .exports
        .iter()
        .filter(|export| export.signature.is_some() && export.class_name.ends_with("Function"))
    {
        let mut unused = Vec::new();
        for local in rivals_uasset::locals_of(parsed, function.index)?
            .into_iter()
            .filter(|local| !local.parameter)
        {
            let uses = rivals_uasset::member_uses(
                bundle,
                parsed,
                &target_of(parsed, function.index, &local, mappings),
            )?;
            if uses.live().next().is_none() && uses.unclassified.is_empty() {
                unused.push(local.name);
            }
        }
        if !unused.is_empty() {
            out.push((function.index, unused));
        }
    }
    Ok(out)
}

/// Removes the members from their package in stages: the values its objects store for them, the
/// event graph code only the events going reached, the records, the functions, the graph locals
/// that leaves unused, and with `drop_orphans` the imports nothing names any more.
#[allow(clippy::too_many_arguments)]
fn removal_stages(
    request: &AssetEditRequest<'_>,
    mappings: Option<&Mappings>,
    loaded: &retoc::legacy_asset::FSerializedAssetBundle,
    parsed: &ParsedPackage,
    owner: u32,
    chosen: &[&MemberSummary],
    going_functions: &[u32],
    trims: &rivals_uasset::Trims,
    assignments: &[rivals_uasset::GraphTrim],
    drop_bindings: bool,
    drop_orphans: bool,
    allow_unchecked: bool,
) -> Result<Staged, String> {
    let mut followed = Vec::new();
    let mut warnings = Vec::new();
    let here: Vec<&rivals_uasset::MemberUses> =
        chosen.iter().map(|summary| &summary.here).collect();
    let (mut value_edits, emptied) = unhooking(parsed, &here, drop_bindings, &mut followed);
    // The class names its event graph, which the engine runs every event through; a class whose
    // graph goes names none.
    if chosen.iter().any(|summary| summary.member.graph)
        && let Some(class) = parsed.exports.get(owner as usize)
        && let Some(entry) = class
            .properties
            .iter()
            .find(|entry| entry.name == "UberGraphFunction")
        && let Some((offset, _)) = entry.span
    {
        followed.push(format!(
            "{}.UberGraphFunction (stored value)",
            class.object_name
        ));
        value_edits.push(rivals_uasset::ValueEdit {
            offset,
            expect_name: entry.name.clone(),
            expect_element: entry.element,
            expect_kind: rivals_uasset::kind_of(&entry.value),
            op: rivals_uasset::EditOp::Unset,
        });
    }
    let going_exports: Vec<u32> = going_functions.iter().copied().chain(emptied).collect();
    let sidecars = (
        loaded.bulk_data_buffer.as_deref(),
        loaded.optional_bulk_data_buffer.as_deref(),
    );
    let mut bytes = (
        loaded.asset_file_buffer.clone(),
        loaded.exports_file_buffer.clone(),
    );
    let mut current = parsed.clone();
    if !value_edits.is_empty() {
        let edits = rivals_uasset::PackageEdits {
            values: value_edits,
            allow_unchecked,
            ..Default::default()
        };
        let (next, after, notes) = stage(request, mappings, bytes, &current, sidecars, &edits)?;
        warnings.extend(notes);
        bytes = next;
        current = after;
    }

    // The assignments storing into what goes, before the graphs are trimmed again over what is left.
    let mut graph_code = Vec::new();
    for trim in assignments {
        let statements = |parsed: &ParsedPackage| {
            parsed.exports[trim.graph as usize]
                .script
                .as_ref()
                .map_or(0, |script| script.statements.len())
        };
        let was = statements(&current);
        let edits = rivals_uasset::PackageEdits {
            script_texts: vec![rivals_uasset::ScriptTextEdit {
                export: trim.graph,
                text: trim.text.clone(),
                was: None,
                object: None,
            }],
            allow_unchecked,
            ..Default::default()
        };
        let (next, after, notes) = stage(request, mappings, bytes, &current, sidecars, &edits)?;
        if was - statements(&after) != trim.dropped.len() {
            return Err(format!(
                "{} kept {} of its {was} statements where {} assignment(s) went",
                trim.name,
                statements(&after),
                trim.dropped.len()
            ));
        }
        warnings.extend(notes);
        bytes = next;
        current = after;
        graph_code.push(trim.clone());
    }

    // The graph text before the stubs go: dead code may call a stub by object, which keeps the stub.
    if !trims.graphs.is_empty() {
        let restaged = rivals_uasset::trim_event_graphs(
            &rivals_uasset::AssetBundle {
                asset: &bytes.0,
                exports: &bytes.1,
            },
            &current,
            going_functions,
        );
        for trim in &restaged.graphs {
            let was = current.exports[trim.graph as usize]
                .script
                .as_ref()
                .map_or(0, |script| script.statements.len());
            let edits = rivals_uasset::PackageEdits {
                script_texts: vec![rivals_uasset::ScriptTextEdit {
                    export: trim.graph,
                    text: trim.text.clone(),
                    was: None,
                    object: None,
                }],
                allow_unchecked,
                ..Default::default()
            };
            let (next, after, notes) = stage(request, mappings, bytes, &current, sidecars, &edits)?;
            let now = after.exports[trim.graph as usize]
                .script
                .as_ref()
                .map_or(0, |script| script.statements.len());
            if was - now != trim.dropped.len() {
                return Err(format!(
                    "{} kept {now} of its {was} statements where the trim took {}",
                    trim.name,
                    trim.dropped.len()
                ));
            }
            warnings.extend(notes);
            bytes = next;
            current = after;
            graph_code.push(trim.clone());
        }
    }

    let records: Vec<String> = chosen
        .iter()
        .filter(|m| m.member.kind != MemberKind::Function)
        .map(|m| m.member.name.clone())
        .collect();
    if !records.is_empty() {
        let edits = rivals_uasset::PackageEdits {
            remove_records: vec![rivals_uasset::RemoveRecords {
                owner,
                names: records,
            }],
            ..Default::default()
        };
        let (next, after, notes) = stage(request, mappings, bytes, &current, sidecars, &edits)?;
        warnings.extend(notes);
        bytes = next;
        current = after;
    }
    if !going_exports.is_empty() {
        let edits = rivals_uasset::PackageEdits {
            remove_exports: going_exports.clone(),
            ..Default::default()
        };
        let plan = rivals_uasset::plan_removal(&current, &going_exports)?;
        if !plan.blockers.is_empty() {
            return Err(plan.blockers.join("; "));
        }
        warnings.extend(plan.warnings.iter().cloned());
        let (next, after, notes) = stage(request, mappings, bytes, &current, sidecars, &edits)?;
        warnings.extend(notes);
        bytes = next;
        current = after;
    }

    // Locals only the code taken out used, the stubs' writes into the graph's frame among them.
    let original = crate::asset_edit::bundle_of(loaded);
    let mut locals = Vec::new();
    for trim in &graph_code {
        let path = parsed.exports[trim.graph as usize].path.clone();
        let names = graph_locals_left_unused(
            (&original, parsed),
            (
                &rivals_uasset::AssetBundle {
                    asset: &bytes.0,
                    exports: &bytes.1,
                },
                &current,
            ),
            &path,
            mappings,
        )?;
        let Some(graph_now) = current
            .exports
            .iter()
            .find(|export| export.path == path)
            .map(|export| export.index)
        else {
            continue;
        };
        if names.is_empty() {
            continue;
        }
        let edits = rivals_uasset::PackageEdits {
            remove_records: vec![rivals_uasset::RemoveRecords {
                owner: graph_now,
                names: names.clone(),
            }],
            ..Default::default()
        };
        let (next, after, notes) = stage(request, mappings, bytes, &current, sidecars, &edits)?;
        warnings.extend(notes);
        bytes = next;
        current = after;
        locals.extend(names.iter().map(|name| format!("{}.{name}", trim.name)));
    }

    // Imports nothing names any more, against the package as it was read.
    let names = crate::asset_edit::import_names(&original, parsed)?;
    let orphans = crate::asset_edit::save_orphans(
        &names,
        &rivals_uasset::AssetBundle {
            asset: &bytes.0,
            exports: &bytes.1,
        },
        &current,
    )?;
    let mut imports = Vec::new();
    if !orphans.is_empty() && !drop_orphans {
        warnings.push(format!(
            "{} import(s) the removal left named by nothing stay, as --keep-orphaned-imports asks: {}",
            orphans.len(),
            orphans
                .iter()
                .map(|orphan| format!("{} {}", orphan.index, orphan.path))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    } else if !orphans.is_empty() {
        let drop = crate::asset_edit::orphan_drop(
            &orphans,
            &rivals_uasset::AssetBundle {
                asset: &bytes.0,
                exports: &bytes.1,
            },
            &current,
        )?;
        match drop {
            Some(crate::asset_edit::OrphanDrop::Drop { edits, removed }) => {
                let (next, after, notes) =
                    stage(request, mappings, bytes, &current, sidecars, &edits)?;
                warnings.extend(notes);
                bytes = next;
                current = after;
                imports = removed;
            }
            Some(crate::asset_edit::OrphanDrop::Kept(note)) => warnings.push(note),
            None => {}
        }
    }
    crate::asset_edit::check_load_order(
        &rivals_uasset::PackageEdits {
            allow_unchecked,
            ..Default::default()
        },
        parsed,
        &current,
    )?;
    Ok(Staged {
        bytes,
        followed,
        graph_code,
        locals,
        imports,
        warnings,
    })
}

/// The assignments only storing into the variables or locals in `chosen`, outside the code `going`
/// says goes anyway, as each function's text without them.
fn assignment_drops(
    loaded: &retoc::legacy_asset::FSerializedAssetBundle,
    parsed: &ParsedPackage,
    chosen: &[&MemberSummary],
    going: impl Fn(u64) -> bool,
) -> Result<Vec<rivals_uasset::GraphTrim>, String> {
    let mut assigning: BTreeMap<u32, BTreeSet<usize>> = BTreeMap::new();
    for summary in chosen
        .iter()
        .filter(|m| m.member.kind != MemberKind::Function)
    {
        for one in summary
            .here
            .uses
            .iter()
            .filter(|one| one.role == rivals_uasset::UseRole::Assigned)
        {
            let rivals_uasset::UseSite::Name { at } = one.site else {
                continue;
            };
            if going(at) {
                continue;
            }
            if let Some((export, statement)) = statement_holding(parsed, at) {
                assigning.entry(export).or_default().insert(statement);
            }
        }
    }
    assigning
        .iter()
        .map(|(&export, statements)| {
            rivals_uasset::drop_assignments(&bundle_of(loaded), parsed, export, statements)
        })
        .collect()
}

/// The function and statement whose bytes hold file offset `at`.
fn statement_holding(parsed: &ParsedPackage, at: u64) -> Option<(u32, usize)> {
    parsed.exports.iter().find_map(|export| {
        let script = export.script.as_ref()?;
        if at < script.start || at >= script.end {
            return None;
        }
        let statement = script
            .statements
            .partition_point(|statement| statement.at <= at)
            .checked_sub(1)?;
        Some((export.index, statement))
    })
}

/// The value edits taking out what stores or binds the members going: a tag an object stores a
/// variable's value under, and with `drop_bindings` the binding entry naming a function. A binding
/// object left with no entries goes too: its element of the class's `DynamicBindingObjects` is
/// taken out here, and the object itself is handed back for the exports going.
fn unhooking(
    parsed: &ParsedPackage,
    uses: &[&rivals_uasset::MemberUses],
    drop_bindings: bool,
    followed: &mut Vec<String>,
) -> (Vec<rivals_uasset::ValueEdit>, Vec<u32>) {
    let mut edits = Vec::new();
    let mut removals: Vec<(u64, String, u32)> = Vec::new();
    // Each binding object losing entries: how many it holds, and which go.
    let mut bindings: BTreeMap<u32, (usize, BTreeSet<u32>)> = BTreeMap::new();
    for one in uses.iter().flat_map(|uses| &uses.uses) {
        let rivals_uasset::UseSite::Name { at } = one.site else {
            continue;
        };
        match &one.role {
            rivals_uasset::UseRole::Tag => {
                let Some(tag) = parsed.tag_bounds.iter().find(|tag| tag.tag_at == at) else {
                    continue;
                };
                let Some((export, entry)) = parsed.exports.iter().find_map(|export| {
                    export
                        .properties
                        .iter()
                        .find(|entry| entry.span.is_some_and(|(start, _)| start == tag.key))
                        .map(|entry| (export, entry))
                }) else {
                    continue;
                };
                followed.push(format!(
                    "{}.{} (stored value)",
                    export.object_name, entry.name
                ));
                edits.push(rivals_uasset::ValueEdit {
                    offset: tag.key,
                    expect_name: entry.name.clone(),
                    expect_element: entry.element,
                    expect_kind: rivals_uasset::kind_of(&entry.value),
                    op: rivals_uasset::EditOp::Unset,
                });
            }
            rivals_uasset::UseRole::Binding { .. } if drop_bindings => {
                if let Some((export, entry, index)) = rivals_uasset::element_holding(parsed, at)
                    && let Some((start, _)) = entry.span
                {
                    followed.push(format!("{} (binding)", one.label));
                    removals.push((start, entry.name.clone(), index));
                    let held = match &entry.value {
                        rivals_uasset::PropertyValue::Array { items } => items.len(),
                        _ => 0,
                    };
                    bindings
                        .entry(export)
                        .or_insert_with(|| (held, BTreeSet::new()))
                        .1
                        .insert(index);
                }
            }
            _ => {}
        }
    }
    let emptied: Vec<u32> = bindings
        .into_iter()
        .filter(|(_, (held, going))| *held > 0 && going.len() == *held)
        .map(|(export, _)| export)
        .collect();
    for &binding in &emptied {
        for export in &parsed.exports {
            for entry in export
                .properties
                .iter()
                .filter(|entry| entry.name == "DynamicBindingObjects")
            {
                let (rivals_uasset::PropertyValue::Array { items }, Some((start, _))) =
                    (&entry.value, entry.span)
                else {
                    continue;
                };
                let Some(index) = items.iter().position(|item| {
                    matches!(item, rivals_uasset::PropertyValue::Object { index, .. }
                        if *index == binding as i32 + 1)
                }) else {
                    continue;
                };
                followed.push(format!(
                    "{}, its last binding gone, and {}.DynamicBindingObjects[{index}]",
                    parsed.exports[binding as usize].object_name, export.object_name
                ));
                removals.push((start, entry.name.clone(), index as u32));
            }
        }
    }
    // Several entries out of one array go from the last, so each index still names its element.
    removals.sort_by_key(|a| (a.0, std::cmp::Reverse(a.2)));
    removals.dedup();
    for (offset, name, index) in removals {
        edits.push(rivals_uasset::ValueEdit {
            offset,
            expect_name: name,
            expect_element: None,
            expect_kind: "array".into(),
            op: rivals_uasset::EditOp::Remove { index },
        });
    }
    (edits, emptied)
}

/// Takes Blueprint members out of a class, or locals out of a function with `function`, once
/// nothing runs or reads them: a variable with the values its objects store for it, a function with
/// its export and its class's entries for it, and with `drop_bindings` the binding entries naming
/// it. `unused` takes every member `asset members --unused` lists. One write covers the package
/// and the mod's other packages storing a removed variable.
#[allow(clippy::too_many_arguments)]
pub fn remove_members(
    request: &AssetEditRequest<'_>,
    mappings: Option<&Mappings>,
    options: &crate::asset_edit::SaveOptions,
    class: Option<u32>,
    function: Option<u32>,
    names: &[String],
    unused: bool,
    drop_bindings: bool,
    dry_run: bool,
) -> Result<RemoveReport, String> {
    let allow_unchecked = request.changes.allow_unchecked;
    let report_request = AssetEditRequest {
        changes: Default::default(),
        ..*request
    };
    let utoc = crate::asset_edit::mod_pak_path(request.game_root, request.mod_name)?
        .with_extension("utoc");
    let mod_stem = utoc
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
        .to_string();
    // The members and their uses across the mod the removal is saved into.
    let scan = utoc.is_file().then(|| utoc.to_string_lossy().into_owned());
    let report = member_report_in(
        &report_request,
        mappings,
        class,
        function,
        None,
        scan.as_deref(),
    )?;
    let mut chosen: Vec<&MemberSummary> = if unused {
        report
            .members
            .iter()
            .filter(|m| m.unused() || m.write_only())
            .collect()
    } else {
        names
            .iter()
            .map(|name| {
                report
                    .members
                    .iter()
                    .find(|m| m.member.name.eq_ignore_ascii_case(name))
                    .ok_or_else(|| format!("{} declares no member named {name}", report.owner))
            })
            .collect::<Result<_, _>>()?
    };
    if chosen.is_empty() {
        return Err(format!("{} has no member nothing uses", report.owner));
    }
    // The event graph's frame goes with the graph.
    let graph_goes = chosen.iter().any(|m| m.member.graph);
    if graph_goes
        && let Some(frame) = report
            .members
            .iter()
            .find(|m| m.member.detail == "PointerToUberGraphFrame")
        && !chosen.iter().any(|m| std::ptr::eq(*m, frame))
    {
        chosen.push(frame);
    }

    let (loaded, parsed) = read_package(request, mappings)?;
    let owner = match function {
        Some(function) => function,
        None => class_export(&parsed, class)?,
    };
    // The bytes of the functions going, whose own uses of anything go with them.
    let going_functions: Vec<u32> = chosen
        .iter()
        .filter(|m| m.member.kind == MemberKind::Function)
        .filter_map(|m| m.member.export)
        .collect();
    // The event graph code only the events going enter goes with them, and so do its uses.
    let mut trims = if function.is_none() && !going_functions.is_empty() {
        rivals_uasset::trim_event_graphs(&bundle_of(&loaded), &parsed, &going_functions)
    } else {
        rivals_uasset::Trims::default()
    };
    // A graph going whole needs none of its code trimmed first.
    trims
        .graphs
        .retain(|trim| !going_functions.contains(&trim.graph));
    let dead: Vec<(u64, u64)> = trims
        .graphs
        .iter()
        .flat_map(|trim| trim.ranges.iter().copied())
        .collect();
    let within_going = |at: u64| {
        dead.iter().any(|&(start, end)| (start..end).contains(&at))
            || going_functions.iter().any(|&index| {
                parsed.exports.get(index as usize).is_some_and(|e| {
                    let start = e.serial_offset.max(0) as u64;
                    (start..start + e.serial_size.max(0) as u64).contains(&at)
                })
            })
    };
    let site_at = |site: &rivals_uasset::UseSite| match *site {
        rivals_uasset::UseSite::Name { at }
        | rivals_uasset::UseSite::Index { at }
        | rivals_uasset::UseSite::Script { at, .. }
        | rivals_uasset::UseSite::Value { at, .. } => Some(at),
        _ => None,
    };
    let assignments = assignment_drops(&loaded, &parsed, &chosen, within_going)?;
    let assigned: Vec<(u64, u64)> = assignments
        .iter()
        .flat_map(|trim| trim.ranges.iter().copied())
        .collect();
    let goes = |at: u64| {
        within_going(at)
            || assigned
                .iter()
                .any(|&(start, end)| (start..end).contains(&at))
    };

    let mut refusals = Vec::new();
    for summary in &chosen {
        let member = &summary.member;
        let name = &member.name;
        if member.detail == "PointerToUberGraphFrame" {
            if !graph_goes {
                refusals.push(format!(
                    "{name} is the event graph's frame, which goes only with the graph"
                ));
            }
            continue;
        }
        if member.kind == MemberKind::Variable && member.export.is_some() {
            refusals.push(format!(
                "{name} is bound to a widget of the same name; take the widget out of the tree with asset remove-export first"
            ));
            continue;
        }
        if summary.anim_graph() {
            refusals.push(format!(
                "{name} is a node of the anim graph, which the anim class runs by where it sits \
                 among its variables"
            ));
            continue;
        }
        let blocking: Vec<String> = summary
            .here
            .references()
            .filter(|one| {
                let goes_with = match one.role {
                    rivals_uasset::UseRole::Tag => true,
                    rivals_uasset::UseRole::Binding { .. } => drop_bindings,
                    _ => false,
                };
                !goes_with && !site_at(&one.site).is_some_and(goes)
            })
            .map(|one| one.label.clone())
            .chain(summary.elsewhere.iter().flat_map(|other| {
                other
                    .uses
                    .references()
                    .filter(|one| !matches!(one.role, rivals_uasset::UseRole::Tag))
                    .map(move |one| format!("{} in {}", one.label, other.entry))
            }))
            .collect();
        if summary.here.uses.iter().any(|u| {
            matches!(&u.role, rivals_uasset::UseRole::Binding { field } if field == "InternalVariableName")
        }) {
            refusals.push(format!(
                "{name} is a component; take it out with asset remove-component"
            ));
            continue;
        }
        if !blocking.is_empty() {
            refusals.push(format!(
                "{name} is still used: {}",
                blocking
                    .iter()
                    .take(5)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
            continue;
        }
        let unsure = summary
            .here
            .possible
            .iter()
            .filter(|one| !site_at(&one.site).is_some_and(goes))
            .count()
            + summary.here.unclassified.len()
            + summary
                .elsewhere
                .iter()
                .map(|other| other.uses.possible.len() + other.uses.unclassified.len())
                .sum::<usize>();
        if unsure > 0 && !allow_unchecked {
            refusals.push(format!(
                "{name} may still be used in {unsure} place(s) this cannot place; asset members --member {name} lists them, and --allow-unchecked removes it anyway"
            ));
        }
    }
    if !refusals.is_empty() {
        return Err(refusals.join("\n"));
    }

    // Packages outside the mod that import the class and use a member going, which this cannot
    // reach.
    let class_path = parsed
        .exports
        .get(class_export(&parsed, class).unwrap_or(owner) as usize)
        .map(|e| e.path.clone())
        .unwrap_or_default();
    let mut warnings = Vec::new();
    match outside_importers(request, mappings, &class_path, &mod_stem) {
        Ok(importers) => {
            for (listed, read) in importers {
                let Ok((other_loaded, other_parsed)) = read else {
                    warnings.push(format!(
                        "{listed} imports {class_path} and could not be read"
                    ));
                    continue;
                };
                for summary in &chosen {
                    let target = target_of(&parsed, owner, &summary.member, mappings);
                    let uses = rivals_uasset::member_uses(
                        &bundle_of(&other_loaded),
                        &other_parsed,
                        &target,
                    )?;
                    if uses.references().next().is_some() {
                        return Err(format!(
                            "{listed}, outside {mod_stem}, uses {}, which a removal here cannot reach",
                            summary.member.name
                        ));
                    }
                }
            }
        }
        Err(e) => warnings.push(format!(
            "packages outside the mod that import {class_path} were not checked: {e}"
        )),
    }

    warnings.extend(trims.kept.iter().cloned());
    let staged = removal_stages(
        request,
        mappings,
        &loaded,
        &parsed,
        owner,
        &chosen,
        &going_functions,
        &trims,
        &assignments,
        drop_bindings,
        options.drop_orphaned_imports,
        allow_unchecked,
    )?;
    let bytes = staged.bytes;
    let mut followed = staged.followed;
    warnings.extend(staged.warnings);

    // The mod's other packages storing a removed variable for one of their objects.
    let entry = crate::asset_edit::save_entry(request)?;
    let mut packages = vec![entry.clone()];
    let mut followers = Vec::new();
    if utoc.is_file() {
        let container = utoc.to_string_lossy().into_owned();
        for summary in &chosen {
            for other in &summary.elsewhere {
                if other
                    .uses
                    .uses
                    .iter()
                    .all(|u| u.role != rivals_uasset::UseRole::Tag)
                {
                    continue;
                }
                let other_request = AssetEditRequest {
                    game_root: request.game_root,
                    container: &container,
                    entry: &other.entry,
                    kind: AssetSource::Utoc,
                    mod_name: request.mod_name,
                    changes: Default::default(),
                };
                let (other_loaded, other_parsed) = read_package(&other_request, mappings)?;
                let (edits, _) = unhooking(&other_parsed, &[&other.uses], false, &mut followed);
                if edits.is_empty() {
                    continue;
                }
                let (next, after, notes) = stage(
                    &other_request,
                    mappings,
                    (
                        other_loaded.asset_file_buffer.clone(),
                        other_loaded.exports_file_buffer.clone(),
                    ),
                    &other_parsed,
                    (
                        other_loaded.bulk_data_buffer.as_deref(),
                        other_loaded.optional_bulk_data_buffer.as_deref(),
                    ),
                    &rivals_uasset::PackageEdits {
                        values: edits,
                        ..Default::default()
                    },
                )?;
                crate::asset_edit::check_load_order(
                    &rivals_uasset::PackageEdits {
                        allow_unchecked,
                        ..Default::default()
                    },
                    &other_parsed,
                    &after,
                )?;
                warnings.extend(notes);
                packages.push(other.entry.clone());
                followers.push(crate::pak::iostore_out::PackageBytes {
                    entry: other.entry.clone(),
                    asset: next.0,
                    exports: next.1,
                    bulk: other_loaded.bulk_data_buffer.clone(),
                    optional_bulk: other_loaded.optional_bulk_data_buffer.clone(),
                    memory_mapped_bulk: other_loaded.memory_mapped_bulk_data_buffer.clone(),
                    shader_map_hashes: crate::asset_edit::source_shader_maps(
                        &other_request,
                        &other_loaded.asset_file_buffer,
                    ),
                });
            }
        }
    }
    let mut out = RemoveReport {
        owner: report.owner.clone(),
        removed: chosen.iter().map(|m| m.member.name.clone()).collect(),
        followed,
        packages,
        graph_code: staged.graph_code,
        locals: staged.locals,
        imports: staged.imports,
        warnings,
        pak: None,
    };
    if dry_run {
        return Ok(out);
    }
    if !options.replace
        && !crate::asset_edit::same_file(&utoc, request.container)
        && crate::pak::iostore_out::utoc_holds_entry(&utoc, &entry).unwrap_or(false)
    {
        return Err(format!(
            "{} already holds {entry}; leave out --from to remove them from that copy, or pass --overwrite to start again from the source",
            utoc.file_name().unwrap_or_default().to_string_lossy()
        ));
    }
    crate::pak::containers::drop_cached_store();
    let written = crate::pak::iostore_out::write_replacing_in_iostore(
        &utoc,
        crate::pak::iostore_out::PackageFiles {
            entry: &entry,
            asset: &bytes.0,
            exports: &bytes.1,
            bulk: loaded.bulk_data_buffer.as_deref(),
            optional_bulk: loaded.optional_bulk_data_buffer.as_deref(),
            memory_mapped_bulk: loaded.memory_mapped_bulk_data_buffer.as_deref(),
            shader_map_hashes: crate::asset_edit::source_shader_maps(
                request,
                &loaded.asset_file_buffer,
            ),
        },
        followers,
        &std::collections::HashSet::new(),
        &options.iostore,
    )?;
    out.pak = Some(written.utoc.to_string_lossy().into_owned());
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use rivals_uasset::script_fixture::two_events_laid;

    /// A local nothing in its function names is found for taking out; the locals the code reads or
    /// stores into are not.
    #[test]
    fn a_local_nothing_names_is_unused() {
        let built = two_events_laid();
        let before = built.parsed();
        let text = rivals_uasset::print_script(&before, 1)
            .expect("a script")
            .text();
        let edits = rivals_uasset::PackageEdits {
            script_texts: vec![rivals_uasset::ScriptTextEdit {
                export: 1,
                text: format!(
                    "local Spare: Int
{text}"
                ),
                was: None,
                object: None,
            }],
            ..Default::default()
        };
        let patched = rivals_uasset::patch_package_with(
            &built.bundle(),
            rivals_uasset::Sidecars::default(),
            &before,
            &edits,
            None,
        )
        .expect("patch");
        let bundle = rivals_uasset::AssetBundle {
            asset: &patched.asset,
            exports: &patched.exports,
        };
        let after = rivals_uasset::parse_package(&bundle, None).expect("parse");
        let found = unused_locals(&bundle, &after, None).expect("locals");
        assert_eq!(found, vec![(1, vec!["Spare".to_string()])]);
    }

    /// A mod is any container but the game's own in `Paks`: in `~mods` switched on or off, or a
    /// copy anywhere else, whose other packages are then read for uses.
    #[test]
    fn a_mod_container_is_any_but_the_game_s_own() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rivals-mod-container-{stamp}"));
        let paks = crate::paths::paks_dir(&root.to_string_lossy());
        std::fs::create_dir_all(paks.join("~mods")).expect("paks");
        let game_root = root.to_string_lossy().into_owned();
        let is_mod = |path: &std::path::Path| is_mod_container(&game_root, &path.to_string_lossy());
        assert!(!is_mod(&paks.join("pakchunk0-Windows.utoc")));
        assert!(!is_mod(std::path::Path::new("pakchunk0-Windows.utoc")));
        assert!(is_mod(&paks.join("~mods").join("Thing_9999999_P.utoc")));
        assert!(is_mod(
            &paks.join("~mods").join("Thing_9999999_P.utoc.disabled")
        ));
        assert!(is_mod(&root.join("builds").join("Thing_9999999_P.utoc")));
        assert!(!is_mod(&root.join("builds").join("Thing_9999999_P.pak")));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The laid-out two-event graph as loose files, and a request reading them.
    struct Loose {
        dir: std::path::PathBuf,
        entry: String,
    }

    impl Loose {
        fn new(tag: &str) -> Self {
            Self::of(two_events_laid(), tag)
        }

        fn of(built: rivals_uasset::script_fixture::Built, tag: &str) -> Self {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos();
            let dir = std::env::temp_dir().join(format!("rivals-members-{tag}-{stamp}"));
            std::fs::create_dir_all(&dir).expect("scratch");
            let asset = dir.join("BP_Test.uasset");
            std::fs::write(&asset, &built.asset).expect("write");
            std::fs::write(dir.join("BP_Test.uexp"), &built.exports).expect("write");
            Self {
                entry: asset.to_string_lossy().into_owned(),
                dir,
            }
        }

        fn request(&self) -> AssetEditRequest<'_> {
            AssetEditRequest {
                game_root: "",
                container: "",
                entry: &self.entry,
                kind: AssetSource::Loose,
                mod_name: "",
                changes: Default::default(),
            }
        }
    }

    impl Drop for Loose {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.dir).ok();
        }
    }

    fn stages(loose: &Loose, event: &str, drop_orphans: bool) -> Staged {
        let request = loose.request();
        let report = member_report_in(&request, None, Some(0), None, None, None).expect("report");
        let chosen: Vec<&MemberSummary> = report
            .members
            .iter()
            .filter(|m| m.member.name == event)
            .collect();
        assert_eq!(chosen.len(), 1, "{event} is a member");
        let (loaded, parsed) = read_package(&request, None).expect("read");
        let going: Vec<u32> = chosen.iter().filter_map(|m| m.member.export).collect();
        let trims = rivals_uasset::trim_event_graphs(&bundle_of(&loaded), &parsed, &going);
        let owner = class_export(&parsed, Some(0)).expect("class");
        removal_stages(
            &request,
            None,
            &loaded,
            &parsed,
            owner,
            &chosen,
            &going,
            &trims,
            &[],
            false,
            drop_orphans,
            false,
        )
        .expect("stages")
    }

    /// The two-event graph with nothing reading `Count` any more, so the local is only stored into:
    /// once where Begin Play enters, and once after the return, where nothing reaches.
    fn count_only_assigned() -> rivals_uasset::script_fixture::Built {
        let built = rivals_uasset::script_fixture::event_graph_two_events().build();
        let text = rivals_uasset::script_fixture::TWO_EVENTS.replace(
            "SwitchValue(LocalVariable(Count), 1 => IntOne, default => IntZero)",
            "Nothing",
        );
        let patched = rivals_uasset::patch_package(
            &built.bundle(),
            &built.parsed(),
            &rivals_uasset::PackageEdits {
                script_texts: vec![rivals_uasset::ScriptTextEdit {
                    export: 1,
                    text,
                    was: None,
                    object: None,
                }],
                ..Default::default()
            },
            None,
        )
        .expect("the graph without the read");
        rivals_uasset::script_fixture::Built {
            asset: patched.asset,
            exports: patched.exports,
        }
    }

    /// A local only ever stored into is written, never read, and goes with its assignments: the
    /// label the first carried, where Begin Play enters, moves to the line after it, and the
    /// local's record goes too.
    #[test]
    fn a_write_only_local_goes_with_its_assignments() {
        let loose = Loose::of(count_only_assigned(), "write-only");
        let request = loose.request();
        let report =
            member_report_in(&request, None, Some(0), Some(1), None, None).expect("report");
        let count = report
            .members
            .iter()
            .find(|m| m.member.name == "Count")
            .expect("Count");
        assert!(count.write_only() && !count.unused(), "{count:?}");
        let chosen = vec![count];
        let (loaded, parsed) = read_package(&request, None).expect("read");
        let drops = assignment_drops(&loaded, &parsed, &chosen, |_| false).expect("drops");
        let [drop] = drops.as_slice() else {
            panic!("one function: {drops:?}");
        };
        assert_eq!(drop.dropped.len(), 2, "{:?}", drop.dropped);
        let staged = removal_stages(
            &request,
            None,
            &loaded,
            &parsed,
            1,
            &chosen,
            &[],
            &rivals_uasset::Trims::default(),
            &drops,
            false,
            true,
            false,
        )
        .expect("stages");
        let after = rivals_uasset::parse_package(
            &rivals_uasset::AssetBundle {
                asset: &staged.bytes.0,
                exports: &staged.bytes.1,
            },
            None,
        )
        .expect("read back");
        let text = rivals_uasset::print_script(&after, 1)
            .expect("graph")
            .text();
        assert!(!text.contains("Count"), "{text}");
        assert!(text.contains("ReceiveBeginPlay enters here"), "{text}");
        let signature = after.exports[1].signature.as_ref().expect("signature");
        assert!(signature.locals.iter().all(|local| local.name != "Count"));
    }

    /// Removing Tick takes the Delay only it ran, the local only its resume read, and with the
    /// flag the Delay's imports: the code Begin Play still reaches stays.
    #[test]
    fn an_event_goes_with_the_code_locals_and_imports_only_it_used() {
        let loose = Loose::new("tick");
        let staged = stages(&loose, "ReceiveTick", true);
        let [trim] = staged.graph_code.as_slice() else {
            panic!("one graph trimmed: {:?}", staged.graph_code);
        };
        assert_eq!(trim.dropped.len(), 3);
        assert_eq!(
            staged.locals,
            vec!["ExecuteUbergraph_BP_Test.Flag".to_string()]
        );
        assert!(
            staged
                .imports
                .iter()
                .any(|import| import.ends_with(":Delay")),
            "{:?}",
            staged.imports
        );
        let after = rivals_uasset::parse_package(
            &rivals_uasset::AssetBundle {
                asset: &staged.bytes.0,
                exports: &staged.bytes.1,
            },
            None,
        )
        .expect("the result reads");
        assert!(after.exports.iter().all(|e| e.object_name != "ReceiveTick"));
        assert!(after.imports.iter().all(|i| !i.path.ends_with(":Delay")));
        let text = rivals_uasset::print_script(&after, 1)
            .expect("graph")
            .text();
        assert!(text.contains("SwitchValue("), "{text}");
        assert!(text.contains("ReceiveBeginPlay enters here"), "{text}");
    }

    /// A rewrite of the event graph that drops the Delay block, saved with `drop_unused_locals`,
    /// takes out the local only that block read, in the same write and with the Delay's import
    /// when `drop_orphaned_imports` asks too. A local unused before the save stays and is named,
    /// and with both kept nothing but the script changes.
    #[test]
    fn a_rewrite_drops_the_locals_it_left_unused() {
        use rivals_uasset::script_fixture::two_events_laid;
        let graph_text = |asset: &[u8], exports: &[u8]| {
            let parsed =
                rivals_uasset::parse_package(&rivals_uasset::AssetBundle { asset, exports }, None)
                    .expect("reads");
            rivals_uasset::print_script(&parsed, 1)
                .expect("graph")
                .text()
        };
        // The graph with a local nothing reads, before the rewrite.
        let laid = two_events_laid();
        let spare = rivals_uasset::patch_package_with(
            &laid.bundle(),
            rivals_uasset::Sidecars::default(),
            &laid.parsed(),
            &rivals_uasset::PackageEdits {
                script_texts: vec![rivals_uasset::ScriptTextEdit {
                    export: 1,
                    text: format!(
                        "local Spare: Int\n{}",
                        graph_text(&laid.asset, &laid.exports)
                    ),
                    was: None,
                    object: None,
                }],
                ..Default::default()
            },
            None,
        )
        .expect("a spare local");
        let text = graph_text(&spare.asset, &spare.exports);
        let loose = Loose::of(
            rivals_uasset::script_fixture::Built {
                asset: spare.asset,
                exports: spare.exports,
            },
            "locals",
        );
        // The Delay, its resume point and the branch only the resume reaches give way to the jump
        // the Delay's statement made past them.
        let lines: Vec<&str> = text.lines().collect();
        let delay = lines
            .iter()
            .position(|l| l.contains(":Delay("))
            .expect("the Delay");
        let resumed = lines
            .iter()
            .position(|l| l.contains("unless LocalVariable(Flag)"))
            .expect("the resume");
        let mut kept: Vec<&str> = lines[..delay].to_vec();
        kept.push(lines[delay + 1]);
        kept.extend(&lines[resumed + 1..]);
        let without_delay = kept.join("\n") + "\n";
        let preview = |drop_unused_locals: bool, drop_orphaned_imports: bool| {
            let request = AssetEditRequest {
                mod_name: "RivalsTestLocals",
                changes: rivals_uasset::PackageEdits {
                    script_texts: vec![rivals_uasset::ScriptTextEdit {
                        export: 1,
                        text: without_delay.clone(),
                        was: None,
                        object: None,
                    }],
                    ..Default::default()
                },
                ..loose.request()
            };
            match crate::asset_edit::preview_save(
                &request,
                None,
                &crate::asset_edit::SaveOptions {
                    drop_unused_locals,
                    drop_orphaned_imports,
                    ..Default::default()
                },
            )
            .expect("preview")
            {
                crate::asset_edit::PreviewOutcome::Verified { notes, .. } => notes,
                crate::asset_edit::PreviewOutcome::HoldsCopy { pak } => panic!("{pak} holds it"),
            }
        };

        let kept = preview(false, false);
        assert!(!kept.iter().any(|n| n.starts_with("dropped")), "{kept:?}");
        assert!(
            !kept.iter().any(|n| n.contains("unused before")),
            "{kept:?}"
        );

        let notes = preview(true, false);
        let dropped = notes
            .iter()
            .find(|n| n.starts_with("dropped") && n.contains("local(s)"))
            .unwrap_or_else(|| panic!("no local dropped: {notes:?}"));
        assert!(
            dropped.ends_with("ExecuteUbergraph_BP_Test.Flag"),
            "{dropped}"
        );
        assert!(
            notes.iter().any(
                |n| n.contains("unused before") && n.contains("ExecuteUbergraph_BP_Test.Spare")
            ),
            "{notes:?}"
        );
        assert!(!notes.iter().any(|n| n.contains("import(s)")), "{notes:?}");

        let both = preview(true, true);
        assert!(
            both.iter().any(|n| n.starts_with("dropped")
                && n.contains("import(s)")
                && n.contains(":Delay")),
            "{both:?}"
        );
    }

    /// Asked to keep them, the imports stay, and the removal says which ones it left.
    #[test]
    fn kept_on_asking_the_orphaned_imports_are_named() {
        let loose = Loose::new("note");
        let staged = stages(&loose, "ReceiveTick", false);
        assert!(staged.imports.is_empty());
        assert!(
            staged
                .warnings
                .iter()
                .any(|w| w.contains("--keep-orphaned-imports") && w.contains(":Delay")),
            "{:?}",
            staged.warnings
        );
    }

    /// A game widget's event bound to its button goes with its binding, and the binding object,
    /// left with no entries, goes too, out of the class's list of them.
    #[test]
    fn an_emptied_binding_object_goes_with_the_event_it_bound() {
        let (Ok(root), Ok(usmap)) = (
            std::env::var("RIVALS_GAME_ROOT"),
            std::env::var("RIVALS_USMAP"),
        ) else {
            return;
        };
        if std::env::var_os("OODLE_LIB_PATH").is_none() {
            return;
        }
        let schema = crate::mappings::load(Path::new(&usmap)).expect("mappings");
        let container = format!(
            "{}/MarvelGame/Marvel/Content/Paks/pakchunk0-Windows.utoc",
            root.replace('\\', "/")
        );
        let request = AssetEditRequest {
            game_root: &root,
            container: &container,
            entry: "Marvel/Content/Marvel/UI/Blueprints/Audio/Debug/WBP_DebugAudioAbility.uasset",
            kind: AssetSource::Utoc,
            mod_name: "",
            changes: Default::default(),
        };
        let report =
            member_report_in(&request, Some(&schema), None, None, None, None).expect("report");
        let (loaded, parsed) = read_package(&request, Some(&schema)).expect("read");
        let owner = class_export(&parsed, None).expect("class");
        let bindings = |parsed: &ParsedPackage| {
            parsed
                .exports
                .iter()
                .filter(|export| export.class_name == "ComponentDelegateBinding")
                .count()
        };
        let listed = |parsed: &ParsedPackage| {
            parsed.exports[owner as usize]
                .properties
                .iter()
                .find(|entry| entry.name == "DynamicBindingObjects")
                .map_or(0, |entry| match &entry.value {
                    rivals_uasset::PropertyValue::Array { items } => items.len(),
                    _ => 0,
                })
        };
        assert_eq!((bindings(&parsed), listed(&parsed)), (1, 1));
        let bound = report
            .members
            .iter()
            .find(|m| {
                m.member.kind == MemberKind::Function
                    && m.here.uses.iter().any(|one| {
                        matches!(&one.role, rivals_uasset::UseRole::Binding { field }
                            if field == "FunctionNameToBind")
                    })
            })
            .expect("an event bound to the button");
        let stub = bound.member.export.expect("its export");
        let trims = rivals_uasset::trim_event_graphs(&bundle_of(&loaded), &parsed, &[stub]);
        let staged = removal_stages(
            &request,
            Some(&schema),
            &loaded,
            &parsed,
            owner,
            &[bound],
            &[stub],
            &trims,
            &[],
            true,
            true,
            false,
        )
        .expect("stages");
        assert!(
            staged
                .followed
                .iter()
                .any(|line| line.contains("its last binding gone")),
            "{:?}",
            staged.followed
        );
        let after = rivals_uasset::parse_package(
            &rivals_uasset::AssetBundle {
                asset: &staged.bytes.0,
                exports: &staged.bytes.1,
            },
            Some(&schema),
        )
        .expect("reads");
        let owner_after = class_export(&after, None).expect("class");
        assert_eq!(bindings(&after), 0);
        assert_eq!(
            after.exports[owner_after as usize]
                .properties
                .iter()
                .find(|entry| entry.name == "DynamicBindingObjects")
                .map_or(0, |entry| match &entry.value {
                    rivals_uasset::PropertyValue::Array { items } => items.len(),
                    _ => 0,
                }),
            0
        );
    }

    /// A game Blueprint's event goes with the graph code only it reached: every other event still
    /// lands, the graph still assembles from its text, and it is as many statements shorter.
    #[test]
    fn a_game_event_goes_with_the_graph_code_only_it_reached() {
        let (Ok(root), Ok(usmap)) = (
            std::env::var("RIVALS_GAME_ROOT"),
            std::env::var("RIVALS_USMAP"),
        ) else {
            return;
        };
        if std::env::var_os("OODLE_LIB_PATH").is_none() {
            return;
        }
        let schema = crate::mappings::load(Path::new(&usmap)).expect("mappings");
        let container = format!(
            "{}/MarvelGame/Marvel/Content/Paks/pakchunk0-Windows.utoc",
            root.replace('\\', "/")
        );
        let mut trimmed = 0;
        for entry in [
            "Marvel/Content/Marvel/UI/Blueprints/Audio/Debug/WBP_DebugAudioAbility.uasset",
            "Marvel/Content/Marvel/Blueprints/LevelGameplay/Base/MarvelPlayerControllerBP.uasset",
            "Marvel/Content/Marvel/Maps/Battle/SimpleSelectHeroLevel.umap",
        ] {
            let request = AssetEditRequest {
                game_root: &root,
                container: &container,
                entry,
                kind: AssetSource::Utoc,
                mod_name: "",
                changes: Default::default(),
            };
            let Ok(report) = member_report_in(&request, Some(&schema), None, None, None, None)
            else {
                continue;
            };
            let (loaded, parsed) = read_package(&request, Some(&schema)).expect("read");
            let owner = class_export(&parsed, None).expect("class");
            for summary in report.members.iter().filter(|m| m.member.event) {
                let Some(stub) = summary.member.export else {
                    continue;
                };
                if summary.here.live().next().is_some() {
                    continue;
                }
                let trims = rivals_uasset::trim_event_graphs(&bundle_of(&loaded), &parsed, &[stub]);
                let Some(trim) = trims.graphs.first() else {
                    continue;
                };
                let staged = removal_stages(
                    &request,
                    Some(&schema),
                    &loaded,
                    &parsed,
                    owner,
                    &[summary],
                    &[stub],
                    &trims,
                    &[],
                    false,
                    true,
                    false,
                )
                .unwrap_or_else(|e| panic!("{entry} {}: {e}", summary.member.name));
                let bundle = rivals_uasset::AssetBundle {
                    asset: &staged.bytes.0,
                    exports: &staged.bytes.1,
                };
                let after = rivals_uasset::parse_package(&bundle, Some(&schema)).expect("reads");
                let graph = after
                    .exports
                    .iter()
                    .find(|e| e.path == parsed.exports[trim.graph as usize].path)
                    .expect("the graph");
                let was = parsed.exports[trim.graph as usize]
                    .script
                    .as_ref()
                    .unwrap()
                    .statements
                    .len();
                let now = graph.script.as_ref().unwrap().statements.len();
                assert_eq!(was - now, trim.dropped.len(), "{entry}");
                let text = rivals_uasset::print_script(&after, graph.index)
                    .expect("prints")
                    .text();
                for other in report.members.iter().filter(|m| m.member.event) {
                    if other.member.name != summary.member.name
                        && after
                            .exports
                            .iter()
                            .any(|e| e.object_name == other.member.name)
                    {
                        assert!(
                            text.contains(&format!("{} enters here", other.member.name)),
                            "{entry}: {} no longer lands",
                            other.member.name
                        );
                    }
                }
                let round = rivals_uasset::script_round_trips(&bundle, &after).expect("trips");
                for (export, result) in round {
                    assert!(result.is_ok(), "{entry} #{export}: {result:?}");
                }
                trimmed += 1;
                break;
            }
        }
        assert!(
            trimmed > 0,
            "no pinned Blueprint had an event whose removal frees code"
        );
    }

    /// An event graph no event enters is code nothing runs: it is listed with what nothing uses
    /// and goes whole, taking the class's own reference to it. One that events enter is used.
    #[test]
    fn an_event_graph_no_event_enters_goes_with_the_class_s_reference_to_it() {
        let loose = Loose::of(
            rivals_uasset::script_fixture::lone_event_graph(),
            "lone-graph",
        );
        let request = loose.request();
        let report = member_report_in(&request, None, Some(0), None, None, None).expect("report");
        let graph = report
            .members
            .iter()
            .find(|m| m.member.graph)
            .expect("the graph");
        assert!(graph.unused(), "{graph:?}");
        let (loaded, parsed) = read_package(&request, None).expect("read");
        let going = graph.member.export.expect("the graph's export");
        let staged = removal_stages(
            &request,
            None,
            &loaded,
            &parsed,
            0,
            &[graph],
            &[going],
            &rivals_uasset::Trims::default(),
            &[],
            false,
            true,
            false,
        )
        .expect("stages");
        assert!(
            staged
                .followed
                .contains(&"BP_Test_C.UberGraphFunction (stored value)".to_string()),
            "{:?}",
            staged.followed
        );
        let after = rivals_uasset::parse_package(
            &rivals_uasset::AssetBundle {
                asset: &staged.bytes.0,
                exports: &staged.bytes.1,
            },
            None,
        )
        .expect("the result reads");
        assert!(
            after
                .exports
                .iter()
                .all(|e| e.object_name != "ExecuteUbergraph_BP_Test")
        );
        assert!(
            after.exports[0]
                .properties
                .iter()
                .all(|entry| entry.name != "UberGraphFunction"),
            "{:?}",
            after.exports[0].properties
        );

        let entered = Loose::new("entered-graph");
        let report =
            member_report_in(&entered.request(), None, Some(0), None, None, None).expect("report");
        let graph = report
            .members
            .iter()
            .find(|m| m.member.graph)
            .expect("the graph");
        assert!(!graph.unused(), "{graph:?}");
    }

    /// The code only Begin Play reached calls Begin Play by object, which would keep its stub;
    /// that code goes first, so the stub can go after it.
    #[test]
    fn an_event_its_own_dead_code_calls_still_goes() {
        let loose = Loose::new("begin");
        let staged = stages(&loose, "ReceiveBeginPlay", true);
        assert_eq!(staged.graph_code[0].dropped.len(), 3);
        let after = rivals_uasset::parse_package(
            &rivals_uasset::AssetBundle {
                asset: &staged.bytes.0,
                exports: &staged.bytes.1,
            },
            None,
        )
        .expect("the result reads");
        assert!(
            after
                .exports
                .iter()
                .all(|e| e.object_name != "ReceiveBeginPlay")
        );
        let text = rivals_uasset::print_script(&after, 1)
            .expect("graph")
            .text();
        assert!(text.contains("Delay("), "{text}");
    }
}
