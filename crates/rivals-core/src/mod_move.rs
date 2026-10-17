//! Moving the packages a mod added, or whole folders of them, to other paths inside it.
//!
//! Each move is a package rename inside the mod: the package takes its new name, its class and
//! default object with it, and the mod's other packages are pointed at it. A Blueprint whose class
//! is renamed has its event graph renamed after it, and strings still naming a moved folder follow
//! last, as paths.

use std::path::Path;

use rivals_uasset::Mappings;
use serde::Serialize;

use crate::asset::{self, AssetSource};
use crate::asset_edit::{AssetEditRequest, SaveOptions, SaveOutcome, rename_mod_package};

#[derive(Debug, Clone, Default)]
pub struct MoveOptions {
    /// Folders to move, every package under each, old to new.
    pub folders: Vec<(String, String)>,
    /// Single packages to move, old to new.
    pub maps: Vec<(String, String)>,
    /// Also rename the moved packages whose short name starts with the first, to start with the
    /// second.
    pub name_prefix: Option<(String, String)>,
    pub keep_object_names: bool,
    pub keep_referencers: bool,
    /// How the mod's container is written.
    pub iostore: crate::pak::iostore_out::IoStoreOptions,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlannedMove {
    pub from: String,
    pub to: String,
    #[serde(skip)]
    pub entry: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MoveReport {
    pub moves: Vec<PlannedMove>,
    /// Event graphs renamed after their classes, old name to new.
    pub graphs: Vec<(String, String)>,
    /// Strings naming a moved folder, rewritten to name the new one.
    pub strings: usize,
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pak: Option<String>,
}

fn short_of(package: &str) -> &str {
    package.rsplit('/').next().unwrap_or(package)
}

fn trimmed(path: &str) -> String {
    path.trim().trim_end_matches('/').to_string()
}

/// Where the package `name` goes: the path a `--package` gives it, else its folder's new path, its
/// short name then given the new prefix when it starts with the old one.
fn destination(
    name: &str,
    folders: &[(String, String)],
    options: &MoveOptions,
    any_move: bool,
) -> Option<String> {
    let mut to = options
        .maps
        .iter()
        .find(|(from, _)| from.trim().eq_ignore_ascii_case(name))
        .map(|(_, to)| to.trim().to_string());
    if to.is_none() {
        to = folders.iter().find_map(|(from, new)| {
            name.to_ascii_lowercase()
                .starts_with(&format!("{}/", from.to_ascii_lowercase()))
                .then(|| format!("{new}{}", &name[from.len()..]))
        });
    }
    if let Some((old, new)) = &options.name_prefix
        && (to.is_some() || !any_move)
    {
        let base = to.clone().unwrap_or_else(|| name.to_string());
        let short = short_of(&base);
        if short.starts_with(old.as_str()) {
            let folder = &base[..base.len() - short.len()];
            to = Some(format!("{folder}{new}{}", &short[old.len()..]));
        }
    }
    to
}

/// Where each package of the mod at `utoc` goes, refusing a move onto a package that is there or
/// that the game ships, and a move of one the game ships.
pub fn plan_moves(
    game_root: &str,
    utoc: &Path,
    options: &MoveOptions,
) -> Result<Vec<PlannedMove>, String> {
    let container = utoc.to_string_lossy().into_owned();
    let (_, packages) = asset::list_packages(game_root, &container)?;
    let named: Vec<(String, String)> = packages
        .iter()
        .filter_map(|(_, entry)| Some((asset::package_name_of_entry(entry)?, entry.clone())))
        .collect();
    for (from, _) in &options.maps {
        if !named
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(from.trim()))
        {
            return Err(format!("the mod holds no package {from}"));
        }
    }
    let folders: Vec<(String, String)> = options
        .folders
        .iter()
        .map(|(from, to)| (trimmed(from), trimmed(to)))
        .collect();
    let any_move = !folders.is_empty() || !options.maps.is_empty();
    let mut moves = Vec::new();
    for (name, entry) in &named {
        let to = destination(name, &folders, options, any_move);
        let Some(to) = to.filter(|to| !to.eq_ignore_ascii_case(name)) else {
            continue;
        };
        if asset::base_game_ships(game_root, name) {
            return Err(format!(
                "the game ships {name}, which the mod only overrides, so it cannot move"
            ));
        }
        moves.push(PlannedMove {
            from: name.clone(),
            to,
            entry: entry.clone(),
        });
    }
    for planned in &moves {
        let taken = named.iter().any(|(name, _)| {
            name.eq_ignore_ascii_case(&planned.to)
                && !moves
                    .iter()
                    .any(|other| other.from.eq_ignore_ascii_case(name))
        });
        if taken || asset::base_game_ships(game_root, &planned.to) {
            return Err(format!(
                "{} would move onto {}, which is already there",
                planned.from, planned.to
            ));
        }
        if moves
            .iter()
            .filter(|other| other.to.eq_ignore_ascii_case(&planned.to))
            .count()
            > 1
        {
            return Err(format!("two packages would move to {}", planned.to));
        }
    }
    if moves.is_empty() {
        return Err("nothing in the mod moves".into());
    }
    Ok(moves)
}

/// Moves packages inside the mod at `utoc` as `options` say.
pub fn move_in_mod(
    game_root: &str,
    utoc: &Path,
    mappings: Option<&Mappings>,
    options: &MoveOptions,
    allow_unchecked: bool,
    dry_run: bool,
) -> Result<MoveReport, String> {
    let moves = plan_moves(game_root, utoc, options)?;
    let mut report = MoveReport {
        moves: moves.clone(),
        graphs: Vec::new(),
        strings: 0,
        warnings: Vec::new(),
        pak: None,
    };
    if dry_run {
        return Ok(report);
    }
    let container = utoc.to_string_lossy().into_owned();
    // The mod is named by its path, so the move stays in it wherever it is.
    let mod_name = crate::asset_edit::mod_name_of(utoc)?;
    for planned in &moves {
        let request = AssetEditRequest {
            game_root,
            container: &container,
            entry: &planned.entry,
            kind: AssetSource::Utoc,
            mod_name: &mod_name,
            changes: rivals_uasset::PackageEdits {
                save_as: Some(rivals_uasset::SaveAs {
                    package: planned.to.clone(),
                    rename_objects: !options.keep_object_names,
                }),
                allow_unchecked,
                ..Default::default()
            },
        };
        match rename_mod_package(
            &request,
            mappings,
            &SaveOptions {
                keep_referencers: options.keep_referencers,
                iostore: options.iostore,
                ..Default::default()
            },
        )
        .map_err(|e| format!("moving {} to {}: {e}", planned.from, planned.to))?
        {
            SaveOutcome::Written { warnings, pak, .. } => {
                report.warnings.extend(warnings);
                report.pak = Some(pak.to_string_lossy().into_owned());
            }
            SaveOutcome::HoldsCopy { pak } => {
                return Err(format!("{pak} already holds {}", planned.to));
            }
        }
    }

    // A class renamed with its package takes its event graph's name along.
    if !options.keep_object_names {
        for planned in &moves {
            let (old, new) = (short_of(&planned.from), short_of(&planned.to));
            if old == new {
                continue;
            }
            let entry = asset::entry_like(
                asset::entry_for_package(game_root, &planned.to)?,
                &planned.entry,
            );
            let request = AssetEditRequest {
                game_root,
                container: &container,
                entry: &entry,
                kind: AssetSource::Utoc,
                mod_name: &mod_name,
                changes: rivals_uasset::PackageEdits {
                    allow_unchecked,
                    ..Default::default()
                },
            };
            let graph = format!("ExecuteUbergraph_{old}");
            let Ok(report_of) =
                crate::members::member_report(&request, mappings, None, None, Some(&graph))
            else {
                continue;
            };
            if !report_of.members.iter().any(|m| m.member.graph) {
                continue;
            }
            let renamed = format!("ExecuteUbergraph_{new}");
            match crate::members::rename_member(
                &request,
                mappings,
                &SaveOptions {
                    iostore: options.iostore,
                    ..Default::default()
                },
                None,
                None,
                &graph,
                &renamed,
                rivals_uasset::PossibleUses::Leave,
                false,
                false,
            ) {
                Ok(done) => {
                    report.warnings.extend(
                        done.warnings
                            .into_iter()
                            .filter(|w| !w.starts_with("native code and Python")),
                    );
                    report.graphs.push((graph, renamed));
                }
                Err(e) => report.warnings.push(format!(
                    "{} keeps its event graph's name, {graph}: {e}",
                    planned.to
                )),
            }
        }
    }

    // Strings still naming a moved folder, which name what moved with it.
    for (from, to) in &options.folders {
        let replaced = crate::mod_replace::replace_in_mod(
            &AssetEditRequest {
                game_root,
                container: &container,
                entry: "",
                kind: AssetSource::Utoc,
                mod_name: &mod_name,
                changes: rivals_uasset::PackageEdits {
                    allow_unchecked,
                    ..Default::default()
                },
            },
            mappings,
            &crate::mod_replace::ReplaceOptions {
                find: format!("{}/", trimmed(from)),
                to: format!("{}/", trimmed(to)),
                paths: true,
                ..Default::default()
            },
            &options.iostore,
            false,
        )?;
        report.strings += replaced
            .packages
            .iter()
            .map(|p| p.changes.len())
            .sum::<usize>();
        report.warnings.extend(replaced.warnings);
        for package in replaced.packages {
            report.warnings.extend(
                package
                    .left
                    .into_iter()
                    .map(|left| format!("{}: {left}", package.entry)),
            );
        }
    }
    collapse_payload_notes(&mut report.warnings);
    Ok(report)
}

/// The notes that a texture's or a sound's data was not read for names, one per package moved,
/// said once.
fn collapse_payload_notes(warnings: &mut Vec<String>) {
    let payload = |note: &String| note.contains(", which the reader does not follow, so a name of");
    let held: Vec<String> = warnings
        .iter()
        .filter(|note| payload(note))
        .filter_map(|note| note.split(" holds ").next().map(str::to_string))
        .collect();
    warnings.retain(|note| !payload(note));
    if !held.is_empty() {
        warnings.push(format!(
            "{} package(s) hold data the reader does not follow, such as mips or audio, so a path inside it, if any, was left as it was: {}",
            held.len(),
            held.join(", ")
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_package_goes_where_its_map_or_its_folder_says_with_its_prefix_changed() {
        let options = MoveOptions {
            folders: vec![("/Game/Old".into(), "/Game/New".into())],
            maps: vec![("/Game/Old/UI/W_Old".into(), "/Game/New/UI/W_New".into())],
            name_prefix: Some(("GAL_".into(), "SIG_".into())),
            ..Default::default()
        };
        let folders = options.folders.clone();
        let to = |name: &str| destination(name, &folders, &options, true);
        assert_eq!(
            to("/Game/Old/Blueprints/GAL_Loader").as_deref(),
            Some("/Game/New/Blueprints/SIG_Loader")
        );
        assert_eq!(
            to("/Game/Old/Textures/T_Title").as_deref(),
            Some("/Game/New/Textures/T_Title")
        );
        assert_eq!(
            to("/Game/Old/UI/W_Old").as_deref(),
            Some("/Game/New/UI/W_New")
        );
        assert_eq!(to("/Game/Elsewhere/GAL_Thing"), None);
        assert_eq!(to("/Game/Older/X"), None);
    }

    #[test]
    fn a_prefix_alone_renames_the_whole_mod() {
        let options = MoveOptions {
            name_prefix: Some(("GAL_".into(), "SIG_".into())),
            ..Default::default()
        };
        assert_eq!(
            destination("/Game/Mod/GAL_Loader", &[], &options, false).as_deref(),
            Some("/Game/Mod/SIG_Loader")
        );
    }
}
