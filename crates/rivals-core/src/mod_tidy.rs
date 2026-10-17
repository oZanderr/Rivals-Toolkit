//! Tidies a whole mod in one write: what its packages hold that nothing in them uses, which is what
//! `mods check` warns about under "unused", "unused locals" and "load order". Each package is patched and checked
//! the way a save is, a stage at a time, and every package that changed goes back in together. The
//! entries an older packer left in the pak beside the container, which the game does not support,
//! go too: under "pak entries".

use std::collections::BTreeMap;
use std::path::Path;

use retoc::legacy_asset::FSerializedAssetBundle;
use rivals_uasset::{
    AssetBundle, DependencyEdit, EdgeFinding, ImportEdit, Mappings, PackageEdits, ParsedPackage,
    PatchedBundle, Runs,
};
use serde::Serialize;

use crate::asset::{self, AssetSource};
use crate::asset_edit::{
    AssetEditRequest, bundle_of, check_load_order, patch_pass, read_package, source_shader_maps,
};

/// What one package lost.
#[derive(Debug, Clone, Serialize)]
pub struct TidiedPackage {
    pub entry: String,
    /// Each edge dropped, as `mods check` names it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub edges: Vec<String>,
    /// Each local dropped, as `Function.Local`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub locals: Vec<String>,
    /// Each import dropped, by path, the outers left holding nothing among them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub imports: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct TidyReport {
    pub packages: Vec<TidiedPackage>,
    /// The entries taken out of the pak beside the container, which the game does not support there.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub pak_entries: Vec<String>,
    /// What was left alone, and why.
    pub warnings: Vec<String>,
    /// What was written: the container, or the pak beside it when only that changed; `None` on a
    /// dry run or when nothing needed tidying.
    pub pak: Option<String>,
}

/// What a tidy of a mod's pak alone found, and did.
#[derive(Debug, Clone, Serialize)]
pub struct PakTidy {
    /// The pak beside the container.
    pub pak: String,
    /// The entries in it the game does not support there: taken out, or on a dry run, to be.
    pub entries: Vec<String>,
    /// Whether the pak was written again.
    pub written: bool,
}

/// Takes the entries the game does not support out of the pak beside the container at `utoc`,
/// switched on or off, and nothing else: no package is read or written, so it suits a mod someone
/// else made. Its other files are kept, compressed as `options` say. A dry run says what it would
/// take out.
pub fn tidy_pak(
    utoc: &Path,
    options: &crate::pak::plain::PakOptions,
    dry_run: bool,
) -> Result<PakTidy, String> {
    let pak = crate::mod_ops::ModFiles::of(utoc)?.part("pak");
    let shown = pak.to_string_lossy().into_owned();
    if !pak.is_file() {
        return Ok(PakTidy {
            pak: shown,
            entries: Vec::new(),
            written: false,
        });
    }
    let entries = crate::pak::plain::unsupported_entries(&pak)?;
    let written = !dry_run && !entries.is_empty();
    if written {
        // A read earlier may still hold the mod's files open.
        crate::pak::containers::drop_cached_store();
        crate::pak::plain::strip_unsupported(&pak, options)?;
    }
    Ok(PakTidy {
        pak: shown,
        entries,
        written,
    })
}

/// Tidies every package of the mod at `utoc`, switched on or off, writing the ones that change
/// back into it in one write. A dry run patches and checks everything and writes nothing.
pub fn tidy_mod(
    game_root: &str,
    utoc: &Path,
    mappings: Option<&Mappings>,
    iostore: &crate::pak::iostore_out::IoStoreOptions,
    dry_run: bool,
) -> Result<TidyReport, String> {
    let container = utoc.to_string_lossy().into_owned();
    let (_, packages) = asset::list_packages(game_root, &container)?;
    let mut report = TidyReport::default();
    let mut written = Vec::new();
    for (_, entry) in &packages {
        let request = AssetEditRequest {
            game_root,
            container: &container,
            entry,
            kind: AssetSource::Utoc,
            mod_name: "",
            changes: Default::default(),
        };
        let (loaded, parsed) = match read_package(&request, mappings) {
            Ok(read) => read,
            Err(e) => {
                report.warnings.push(format!(
                    "{entry} could not be read, so nothing in it changed: {e}"
                ));
                continue;
            }
        };
        let Some((tidied, patched)) =
            tidy_package(&request, mappings, &loaded, &parsed, &mut report.warnings)
                .map_err(|e| format!("{entry}: {e}"))?
        else {
            continue;
        };
        written.push(crate::pak::iostore_out::PackageBytes {
            entry: entry.clone(),
            asset: patched.asset,
            exports: patched.exports,
            bulk: patched.bulk.or(loaded.bulk_data_buffer),
            optional_bulk: patched.optional_bulk.or(loaded.optional_bulk_data_buffer),
            memory_mapped_bulk: loaded.memory_mapped_bulk_data_buffer,
            shader_map_hashes: source_shader_maps(&request, &loaded.asset_file_buffer),
        });
        report.packages.push(tidied);
    }
    let pak = crate::mod_ops::ModFiles::of(utoc)?.part("pak");
    if pak.is_file() {
        match crate::pak::plain::unsupported_entries(&pak) {
            Ok(found) => report.pak_entries = found,
            Err(e) => report.warnings.push(format!(
                "the pak beside the container could not be read, so nothing in it changed: {e}"
            )),
        }
    }
    if dry_run {
        return Ok(report);
    }
    let mut written = written.into_iter();
    // Writing the container writes the pak beside it again, without those entries.
    let Some(first) = written.next() else {
        if !report.pak_entries.is_empty() {
            report.pak = Some(tidy_pak(utoc, &iostore.pak(), false)?.pak);
        }
        return Ok(report);
    };
    crate::pak::containers::drop_cached_store();
    let done = crate::pak::iostore_out::write_replacing_in_iostore(
        utoc,
        crate::pak::iostore_out::PackageFiles {
            entry: &first.entry,
            asset: &first.asset,
            exports: &first.exports,
            bulk: first.bulk.as_deref(),
            optional_bulk: first.optional_bulk.as_deref(),
            memory_mapped_bulk: first.memory_mapped_bulk.as_deref(),
            shader_map_hashes: first.shader_map_hashes.clone(),
        },
        written.collect(),
        &std::collections::HashSet::new(),
        iostore,
    )?;
    report.pak = Some(done.utoc.to_string_lossy().into_owned());
    Ok(report)
}

/// One package tidied, a stage per kind of leftover since each is a save of its own: what went and
/// the bytes it left, or `None` when there was nothing to drop.
fn tidy_package(
    request: &AssetEditRequest<'_>,
    mappings: Option<&Mappings>,
    loaded: &FSerializedAssetBundle,
    first: &ParsedPackage,
    warnings: &mut Vec<String>,
) -> Result<Option<(TidiedPackage, PatchedBundle)>, String> {
    let entry = request.entry;
    let mut tidied = TidiedPackage {
        entry: entry.to_string(),
        edges: Vec::new(),
        locals: Vec::new(),
        imports: Vec::new(),
        names: Vec::new(),
    };
    let mut bytes: Option<PatchedBundle> = None;
    let mut current = std::sync::Arc::new(first.clone());
    let stage = |changes: PackageEdits,
                 bytes: &mut Option<PatchedBundle>,
                 current: &mut std::sync::Arc<ParsedPackage>|
     -> Result<(), String> {
        let (patched, after) = {
            let bundle = match bytes.as_ref() {
                Some(patched) => AssetBundle {
                    asset: &patched.asset,
                    exports: &patched.exports,
                },
                None => bundle_of(loaded),
            };
            patch_pass(
                &AssetEditRequest {
                    changes: changes.clone(),
                    ..*request
                },
                mappings,
                &bundle,
                rivals_uasset::Sidecars {
                    bulk: loaded.bulk_data_buffer.as_deref(),
                    optional_bulk: loaded.optional_bulk_data_buffer.as_deref(),
                },
                current,
                &changes,
            )?
        };
        *bytes = Some(patched);
        *current = after;
        Ok(())
    };
    let header_of = |bytes: &Option<PatchedBundle>| {
        rivals_uasset::read_header(&match bytes.as_ref() {
            Some(patched) => AssetBundle {
                asset: &patched.asset,
                exports: &patched.exports,
            },
            None => bundle_of(loaded),
        })
    };

    // Edges to another export nothing in the waiting export names, one edit per export.
    if let Some(runs) = current.dependencies.clone() {
        let mut edits: BTreeMap<u32, Runs> = BTreeMap::new();
        let mut lines = Vec::new();
        for finding in rivals_uasset::edge_findings(&current) {
            let EdgeFinding::Unnamed { export, target } = finding else {
                continue;
            };
            edits
                .entry(export)
                .or_insert_with(|| runs[export as usize].clone())
                .create_before_serialize
                .retain(|index| *index != target);
            lines.push(finding.render(&current));
        }
        let losses: Vec<String> = edits
            .iter()
            .flat_map(|(export, runs)| rivals_uasset::zen_losses(&current, *export, runs))
            .collect();
        if !losses.is_empty() {
            warnings.push(format!(
                "{entry}: its stray edges stay, since the container would not hold the runs left: {}",
                losses.join("; ")
            ));
        } else if !edits.is_empty() {
            let changes = PackageEdits {
                dependencies: edits
                    .into_iter()
                    .map(|(export, runs)| DependencyEdit { export, runs })
                    .collect(),
                ..Default::default()
            };
            stage(changes, &mut bytes, &mut current)?;
            tidied.edges = lines;
        }
    }

    // Locals nothing in their function names, before the imports, so one only such a local's type
    // named goes in the same write.
    let found = {
        let bundle = match bytes.as_ref() {
            Some(patched) => AssetBundle {
                asset: &patched.asset,
                exports: &patched.exports,
            },
            None => bundle_of(loaded),
        };
        crate::members::unused_locals(&bundle, &current, mappings)
    };
    match found {
        Ok(found) if !found.is_empty() => {
            let locals: Vec<String> = found
                .iter()
                .flat_map(|(function, names)| {
                    let function = &current.exports[*function as usize].object_name;
                    names.iter().map(move |name| format!("{function}.{name}"))
                })
                .collect();
            let changes = PackageEdits {
                remove_records: found
                    .into_iter()
                    .map(|(owner, names)| rivals_uasset::RemoveRecords { owner, names })
                    .collect(),
                ..Default::default()
            };
            stage(changes, &mut bytes, &mut current)?;
            tidied.locals = locals;
        }
        Ok(_) => {}
        Err(reason) => warnings.push(format!(
            "{entry}: its unused locals cannot be known, so they stay: {reason}"
        )),
    }

    // Imports nothing names, taken out together with the outers that leaves holding nothing.
    let header = header_of(&bytes)?;
    let unused: Vec<u32> = rivals_uasset::unused_imports(&current, &header)
        .into_iter()
        .filter(|import| import.blocked.is_none())
        .map(|import| (-import.index - 1) as u32)
        .collect();
    if !unused.is_empty() {
        let plan = rivals_uasset::plan_import_removal(&current, &header, &unused)?;
        if plan.blockers.is_empty() {
            let changes = PackageEdits {
                imports: unused
                    .iter()
                    .map(|&import| ImportEdit::Remove { import, from: None })
                    .collect(),
                ..Default::default()
            };
            stage(changes, &mut bytes, &mut current)?;
            tidied.imports = plan.removed.into_iter().map(|gone| gone.path).collect();
        } else {
            warnings.push(format!(
                "{entry}: its unused imports stay: {}",
                plan.blockers.join("; ")
            ));
        }
    }

    // Names nothing uses, the ones the import removal just freed among them.
    let header = header_of(&bytes)?;
    let exports = match bytes.as_ref() {
        Some(patched) => &patched.exports[..],
        None => &loaded.exports_file_buffer[..],
    };
    let total = u64::try_from(header.summary.versioning_info.total_header_size)
        .map_err(|_| "package declares a negative header size".to_string())?;
    match rivals_uasset::unused_names(&current, &header, exports, total) {
        Ok(names) if !names.is_empty() => {
            let changes = PackageEdits {
                compact_names: true,
                ..Default::default()
            };
            stage(changes, &mut bytes, &mut current)?;
            tidied.names = names;
        }
        Ok(_) => {}
        Err(reason) => warnings.push(format!(
            "{entry}: its unused names cannot be known, so they stay: {reason}"
        )),
    }

    let Some(patched) = bytes else {
        return Ok(None);
    };
    check_load_order(&PackageEdits::default(), first, &current)?;
    Ok(Some((tidied, patched)))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::pak::plain::{PakOptions, list_files, read_files, write_files};

    /// A tidy of the pak alone takes out only the entries the game does not support, keeps the
    /// rest, and needs nothing but the mod's files; a dry run writes nothing.
    #[test]
    fn a_pak_alone_loses_only_what_the_game_does_not_support() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rivals-tidy-pak-only-{stamp}"));
        std::fs::create_dir_all(&dir).expect("scratch");
        let utoc = dir.join("Other_9999999_P.utoc");
        std::fs::write(&utoc, b"").expect("container");
        let zlib = PakOptions {
            compression: crate::pak::profile::PackCompression::Zlib,
            oodle_level: None,
        };
        let pak = dir.join("Other_9999999_P.pak");
        let ini = ("Marvel/Config/a.ini".to_string(), b"a".to_vec());
        write_files(
            &pak,
            vec![
                ini.clone(),
                ("chunknames".into(), b"listing".to_vec()),
                ("patched_files".into(), b"x".to_vec()),
            ],
            &zlib,
        )
        .expect("the old pak");
        let dry = tidy_pak(&utoc, &zlib, true).expect("dry run");
        assert!(!dry.written);
        assert_eq!(
            list_files(&pak).expect("list").len(),
            3,
            "a dry run writes nothing"
        );
        let mut entries = dry.entries.clone();
        entries.sort();
        assert_eq!(entries, ["chunknames", "patched_files"]);
        let done = tidy_pak(&utoc, &zlib, false).expect("tidy");
        assert!(done.written && done.pak == pak.to_string_lossy());
        assert_eq!(read_files(&pak, &|_| true).expect("read"), vec![ini]);
        let again = tidy_pak(&utoc, &zlib, false).expect("again");
        assert!(again.entries.is_empty() && !again.written);
        std::fs::remove_dir_all(&dir).ok();
    }
}

/// Set `RIVALS_GAME_ROOT`, `RIVALS_USMAP` and `OODLE_LIB_PATH` to run these. Skipped otherwise.
#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod game_data_tests {
    use super::*;

    const TABLE: &str = "Marvel/Content/Marvel/Data/DataTable/UI/CardTable.uasset";

    /// What an older packer left in the pak beside a container is tidied away with the rest: a dry
    /// run names it and changes nothing, and the tidy leaves the pak without it.
    #[test]
    fn the_entries_the_game_does_not_support_are_tidied_out_of_the_pak() {
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
        let loaded = asset::load_bundle(&root, &container, TABLE, AssetSource::Utoc).expect("load");
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rivals-tidy-pak-{stamp}"));
        std::fs::create_dir_all(&dir).expect("scratch");
        let utoc = dir.join("TidyProbe_9999999_P.utoc");
        let zlib = crate::pak::iostore_out::IoStoreOptions {
            compression: crate::pak::profile::PackCompression::Zlib,
            ..Default::default()
        };
        let mut package = Some(crate::pak::iostore_out::PackageBytes {
            entry: TABLE.into(),
            asset: loaded.asset_file_buffer,
            exports: loaded.exports_file_buffer,
            bulk: None,
            optional_bulk: None,
            memory_mapped_bulk: None,
            shader_map_hashes: Vec::new(),
        });
        crate::pak::iostore_out::write_batch_into_iostore(&utoc, 1, |_| package.take(), &zlib)
            .expect("write the probe");
        // The pak an older packer would have written beside it.
        let pak = utoc.with_extension("pak");
        crate::pak::plain::write_files(
            &pak,
            vec![("chunknames".into(), TABLE.as_bytes().to_vec())],
            &zlib.pak(),
        )
        .expect("the old pak");
        let dry = tidy_mod(&root, &utoc, Some(&mappings), &zlib, true);
        let done = tidy_mod(&root, &utoc, Some(&mappings), &zlib, false);
        let left = crate::pak::plain::list_files(&pak);
        crate::pak::containers::drop_cached_store();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(dry.expect("dry run").pak_entries, ["chunknames"]);
        let done = done.expect("tidy");
        assert_eq!(done.pak_entries, ["chunknames"]);
        assert!(done.pak.is_some());
        assert!(left.expect("the pak").is_empty());
    }
}
