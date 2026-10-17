//! Installs mods into `~mods`: a pak with the container beside it, a mod's loose files packed into
//! a container, and with the `archives` feature, every mod a zip, 7z or rar archive holds. A folder
//! or archive holding several mods, in any mix and at any depth, installs each of them.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::pak::iostore_out::IoStoreOptions;
use crate::pak::pack::{GAME_FOLDERS, Take, holds_game_folder, is_game_folder};
use crate::pak::shader_library::GameLibraries;

/// The files beside a pak that are part of its mod.
const COMPANION_EXTS: [&str; 2] = ["ucas", "utoc"];

/// The archives mods come in, by extension.
const ARCHIVE_EXTS: [&str; 3] = ["zip", "7z", "rar"];

/// How many archives deep a mod is looked for, so an archive packed inside itself ends.
#[cfg(feature = "archives")]
const ARCHIVE_DEPTH: usize = 4;

/// The patch number a mod packed from a folder gets when the folder's name has none, so it mounts
/// above the game's patches, as every mod has to.
const MOD_SUFFIX: &str = "_9999999_P";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallResult {
    pub file_name: String,
    /// A switched-off copy of the mod was there, and went.
    pub replaced_disabled: bool,
    /// A switched-on copy was there, and was written over.
    pub replaced_enabled: bool,
    /// The entries taken out of its pak as it was installed, or on a dry run, to be: the files
    /// Windows and macOS leave in folders, and for an IoStore mod the listings an older packer
    /// left beside its container, which the game does not support there.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stripped: Vec<String>,
    /// It was packed into a container as it went in, rather than copied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub packed: Option<Packed>,
}

/// What packing a mod into a container as it went in made.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Packed {
    pub from: PackedFrom,
    /// Packages put in the container.
    pub packages: usize,
    /// Files that are not packages, carried in the pak beside it.
    pub loose: Vec<String>,
    /// Shader archives written into the container as libraries.
    pub shader_libraries: Vec<String>,
    pub warnings: Vec<String>,
}

/// What a mod was packed from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackedFrom {
    /// A folder of its loose files, as a pak unpacks: what is under the `Marvel` and `Engine`
    /// folders in it.
    Folder,
    /// A plain pak holding packages, which the game loads none of from one.
    PlainPak,
}

#[derive(Clone, Copy)]
pub struct InstallOptions<'a> {
    /// Write over a mod of the same name already installed, switched on or off.
    pub replace: bool,
    /// Say what would be installed, and copy nothing.
    pub dry_run: bool,
    /// How a mod packed as it goes in is written, and a pak written again without what does not
    /// belong in it.
    pub io: IoStoreOptions,
    /// The game's shader libraries, which a shader archive in a mod packed as it goes in is kept
    /// from replacing.
    pub game_libraries: GameLibraries<'a>,
}

impl Default for InstallOptions<'_> {
    fn default() -> Self {
        Self {
            replace: false,
            dry_run: false,
            io: IoStoreOptions::default(),
            game_libraries: &unknown_libraries,
        }
    }
}

/// The game's shader libraries where nobody said what they are, so a shader archive is not packed
/// unchecked.
fn unknown_libraries() -> Result<HashSet<String>, String> {
    Err("the game's own shader libraries are not known here".to_string())
}

/// One mod found in what was given, and what installing it did.
#[derive(Debug)]
pub struct Found {
    /// Where it was: the file given, or its path in the folder or archive given, such as
    /// `D:\Downloads\Pack.zip\Skins\A_9999999_P.pak`.
    pub at: String,
    pub result: Result<InstallResult, String>,
}

/// Installs the mods at `source` into `mods`: a `.pak`, with the `.utoc` and `.ucas` beside it; a
/// `.utoc` or `.ucas`, for the `.pak` beside it; an archive; or a folder. The error is for a
/// `source` that holds no mod at all; each mod it holds has its own result.
pub fn install(mods: &Path, source: &Path, options: &InstallOptions) -> Result<Vec<Found>, String> {
    install_each(mods, &[source.to_path_buf()], options)
        .pop()
        .map_or_else(|| Ok(Vec::new()), |each| each.result)
}

/// The file an install of `source` reads: the `.pak` beside a `.utoc` or `.ucas`, else `source`
/// itself. A mod's three files give the same pak.
pub fn install_source(source: &Path) -> PathBuf {
    match extension(source).as_str() {
        "utoc" | "ucas" => source.with_extension("pak"),
        _ => source.to_path_buf(),
    }
}

fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default()
}

/// What became of one mod, archive or folder [`install_each`] was given.
#[derive(Debug)]
pub struct NamedInstall {
    /// The files given that named it, in the order given.
    pub named: Vec<PathBuf>,
    pub result: Result<Vec<Found>, String>,
}

/// Installs every mod `sources` names into `mods`, each once however many of its files name it,
/// in the order they are first named. Everything is looked through before anything is written, so
/// two mods that would go in under one name are both refused rather than one writing over the
/// other.
pub fn install_each(
    mods: &Path,
    sources: &[PathBuf],
    options: &InstallOptions,
) -> Vec<NamedInstall> {
    let mut mods_named: Vec<(PathBuf, Vec<PathBuf>)> = Vec::new();
    for source in sources {
        let read = install_source(source);
        let key = read.canonicalize().unwrap_or(read);
        match mods_named.iter_mut().find(|(seen, _)| *seen == key) {
            Some((_, named)) => named.push(source.clone()),
            None => mods_named.push((key, vec![source.clone()])),
        }
    }
    // The archives' files stay until every mod in them is in.
    let mut unpacked = Vec::new();
    let looked: Vec<_> = mods_named
        .into_iter()
        .map(|(_, named)| {
            let found = look_through(mods, &named[0], &mut unpacked);
            (named, found)
        })
        .collect();
    let clashes = clashes(looked.iter().filter_map(|(_, found)| found.as_ref().ok()));
    looked
        .into_iter()
        .map(|(named, found)| NamedInstall {
            named,
            result: found.map(|found| {
                found
                    .into_iter()
                    .map(|located| install_located(mods, located, &clashes, options))
                    .collect()
            }),
        })
        .collect()
}

/// A mod found in what was given, before it is installed.
struct Located {
    at: PathBuf,
    what: What,
}

enum What {
    /// A mod's pak, with its container beside it when it has one.
    Pak(PathBuf),
    /// A folder of a mod's loose files, packed as the mod `name`.
    Loose { dir: PathBuf, name: String },
    /// Something that cannot go in, saying why.
    Refused(String),
}

impl Located {
    fn refused(at: PathBuf, why: impl Into<String>) -> Self {
        Self {
            at,
            what: What::Refused(why.into()),
        }
    }

    /// The name it goes in under.
    fn stem(&self) -> Option<String> {
        match &self.what {
            What::Pak(pak) => Some(pak.file_stem()?.to_string_lossy().into_owned()),
            What::Loose { name, .. } => Some(name.clone()),
            What::Refused(_) => None,
        }
    }
}

/// The mods at `source`, one given to install.
fn look_through(
    mods: &Path,
    source: &Path,
    unpacked: &mut Vec<Scratch>,
) -> Result<Vec<Located>, String> {
    if !source.exists() {
        return Err(format!("{} does not exist", source.display()));
    }
    let mut found = Vec::new();
    if source.is_dir() {
        refuse_mods_folder(mods, source)?;
        let name = source
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        if is_game_folder(&name) && !holds_game_folder(source) {
            return Err(format!(
                "{} is the {name} folder of a mod's loose files: give the folder that holds it",
                source.display()
            ));
        }
        look_in(source, source, &name, 0, true, unpacked, &mut found);
    } else {
        match extension(source).as_str() {
            "pak" => found.push(Located {
                at: source.to_path_buf(),
                what: What::Pak(source.to_path_buf()),
            }),
            "utoc" | "ucas" => {
                let pak = install_source(source);
                if !pak.is_file() {
                    return Err(format!(
                        "{} has no .pak beside it, which every mod has",
                        source.display()
                    ));
                }
                found.push(Located {
                    at: source.to_path_buf(),
                    what: What::Pak(pak),
                });
            }
            ext if ARCHIVE_EXTS.contains(&ext) => {
                open_archive(source, source, 0, unpacked, &mut found)?;
            }
            _ => {
                return Err(format!(
                    "{} is not a mod's .pak, .utoc or .ucas, an archive or a folder",
                    source.display()
                ));
            }
        }
    }
    if found.is_empty() {
        return Err(format!(
            "{} holds no mod: no .pak, no archive holding one, and no folder holding a {} folder",
            source.display(),
            GAME_FOLDERS.join(" or ")
        ));
    }
    Ok(found)
}

/// Refuses a folder that is the mods folder or inside it, whose mods are installed already, and
/// one holding it, whose game paks would be taken for mods.
fn refuse_mods_folder(mods: &Path, dir: &Path) -> Result<(), String> {
    let (mods, dir) = (resolved(mods), resolved(dir));
    if dir.starts_with(&mods) {
        return Err(format!(
            "{} is in {}, the folder mods are installed into",
            dir.display(),
            mods.display()
        ));
    }
    if mods.starts_with(&dir) {
        return Err(format!(
            "{} holds {}, the folder mods are installed into: give the folder the mods are in",
            dir.display(),
            mods.display()
        ));
    }
    Ok(())
}

/// `path` with the part of it that exists resolved, so two spellings of one folder compare alike
/// whether it is made yet or not.
fn resolved(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut rest = Vec::new();
    let mut at = absolute.as_path();
    loop {
        if let Ok(real) = at.canonicalize() {
            return rest.iter().rev().fold(real, |path, part| path.join(part));
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                at = parent;
            }
            _ => return absolute,
        }
    }
}

/// Looks through the folder at `dir`, shown as `at`, for mods: the folder itself when it holds a
/// game folder, as the mod `name`, then every pak, archive and folder of loose files in it, at
/// any depth. Packages no game folder holds are named, a `root` naming them for each folder in it,
/// and a folder below naming them to the one above.
fn look_in(
    dir: &Path,
    at: &Path,
    name: &str,
    depth: usize,
    root: bool,
    unpacked: &mut Vec<Scratch>,
    found: &mut Vec<Located>,
) -> usize {
    // A game folder holding mods is a copy of the game's own folders, not a mod's files.
    let loose = holds_game_folder(dir) && !game_folders_hold_mods(dir);
    if loose {
        found.push(Located {
            at: at.to_path_buf(),
            what: What::Loose {
                dir: dir.to_path_buf(),
                name: mod_name(name),
            },
        });
    }
    // A link to a folder is not followed, so one pointing back up the tree cannot loop.
    let mut entries: Vec<(PathBuf, bool)> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|entry| {
                let folder = entry.file_type().is_ok_and(|kind| kind.is_dir());
                (entry.path(), folder)
            })
            .collect(),
        Err(e) => {
            found.push(Located::refused(
                at.to_path_buf(),
                format!("could not be read: {e}"),
            ));
            return 0;
        }
    };
    entries.sort();
    let mut unplaced = 0;
    for (path, folder) in entries {
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let here = at.join(&file_name);
        if folder {
            if loose && is_game_folder(&file_name) {
                continue;
            }
            let below = look_in(&path, &here, &file_name, depth, false, unpacked, found);
            if root && below > 0 {
                found.push(unplaced_packages(here, below));
            } else {
                unplaced += below;
            }
            continue;
        }
        let beside = |ext: &str| path.with_extension(ext).is_file();
        match extension(&path).as_str() {
            "pak" => found.push(Located {
                at: here,
                what: What::Pak(path),
            }),
            "utoc" if !beside("pak") => found.push(Located::refused(
                here,
                "has no .pak beside it, which every mod has",
            )),
            "ucas" if !beside("pak") && !beside("utoc") => found.push(Located::refused(
                here,
                "has no .pak or .utoc beside it, which every mod has",
            )),
            "uasset" | "umap" => unplaced += 1,
            ext if ARCHIVE_EXTS.contains(&ext) => {
                if let Err(e) = open_archive(&path, &here, depth + 1, unpacked, found) {
                    found.push(Located::refused(here, e));
                }
            }
            _ => {}
        }
    }
    if root && unplaced > 0 {
        found.push(unplaced_packages(at.to_path_buf(), unplaced));
        return 0;
    }
    unplaced
}

fn unplaced_packages(at: PathBuf, count: usize) -> Located {
    Located::refused(
        at,
        format!(
            "{count} package(s) here sit in no {} folder, so where they go in the game is not \
             known: put them under the folders they have in the game, as a pak unpacks",
            GAME_FOLDERS.join(" or ")
        ),
    )
}

/// Whether the game folders in `dir` hold a pak, container or archive, which a mod's loose files
/// never do.
fn game_folders_hold_mods(dir: &Path) -> bool {
    GAME_FOLDERS.iter().any(|folder| {
        walkdir::WalkDir::new(dir.join(folder))
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .any(|entry| {
                let ext = extension(entry.path());
                ext == "pak"
                    || COMPANION_EXTS.contains(&ext.as_str())
                    || ARCHIVE_EXTS.contains(&ext.as_str())
            })
    })
}

/// The name the mod packed from the folder `name` goes in under: the folder's, with a patch number
/// when it has none.
fn mod_name(name: &str) -> String {
    if crate::mods::pak_priority(&format!("{name}.pak")).is_some() {
        name.to_string()
    } else {
        format!("{name}{MOD_SUFFIX}")
    }
}

/// Each name more than one mod found would go in under, ignoring case as Windows does, with where
/// those mods were.
fn clashes<'a>(found: impl Iterator<Item = &'a Vec<Located>>) -> HashMap<String, Vec<PathBuf>> {
    let mut by_name: HashMap<String, Vec<PathBuf>> = HashMap::new();
    for located in found.flatten() {
        if let Some(stem) = located.stem() {
            by_name
                .entry(stem.to_ascii_lowercase())
                .or_default()
                .push(located.at.clone());
        }
    }
    by_name.retain(|_, at| at.len() > 1);
    by_name
}

fn install_located(
    mods: &Path,
    located: Located,
    clashes: &HashMap<String, Vec<PathBuf>>,
    options: &InstallOptions,
) -> Found {
    let clash = located
        .stem()
        .and_then(|stem| Some((clashes.get(&stem.to_ascii_lowercase())?, stem)));
    let result = match (located.what, clash) {
        (What::Refused(why), _) => Err(why),
        (_, Some((all, stem))) => {
            let others: Vec<String> = all
                .iter()
                .filter(|at| **at != located.at)
                .map(|at| at.display().to_string())
                .collect();
            Err(format!(
                "{} would go in as {stem} too: install the one you want on its own",
                others.join(" and ")
            ))
        }
        (What::Pak(pak), None) => install_pak(mods, &pak, options),
        (What::Loose { dir, name }, None) => install_packed(
            mods,
            &dir,
            Take::GameFolders,
            &name,
            PackedFrom::Folder,
            options,
        ),
    };
    Found {
        at: located.at.display().to_string(),
        result,
    }
}

/// Copies the pak at `source` into `mods`, the `.utoc` and `.ucas` beside it with it. A container
/// the installed copy had and the new one has not is removed, so the two cannot mix. The installed
/// pak loses the files folders leave, and that of an IoStore mod the entries the game does not
/// support beside a container. A plain pak holding packages, which the game loads none of from
/// one, is packed into a container.
pub fn install_pak(
    mods: &Path,
    source: &Path,
    options: &InstallOptions,
) -> Result<InstallResult, String> {
    let file_name = source
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("{} names no file", source.display()))?
        .to_string();
    let stem = file_name
        .strip_suffix(".pak")
        .ok_or_else(|| format!("Not a pak file: {file_name}"))?
        .to_string();
    if !source.is_file() {
        return Err(format!("{} does not exist", source.display()));
    }
    let dest = mods.join(&file_name);
    let same = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    if same(source, &dest) {
        return Err(format!("{file_name} is already the installed copy"));
    }
    let from = source.parent().unwrap_or(Path::new(""));
    let iostore = from.join(format!("{stem}.utoc")).is_file();
    if !iostore && holds_packages(source) {
        return install_packed(
            mods,
            source,
            Take::Everything,
            &stem,
            PackedFrom::PlainPak,
            options,
        );
    }

    let disabled = format!("{file_name}.disabled");
    let replaced_disabled = mods.join(&disabled).exists();
    let replaced_enabled = dest.exists();
    if (replaced_disabled || replaced_enabled) && !options.replace {
        return Err(format!(
            "{stem} is already installed: pass --overwrite to write over it"
        ));
    }
    if options.dry_run {
        return Ok(InstallResult {
            file_name,
            replaced_disabled,
            replaced_enabled,
            stripped: if iostore {
                crate::pak::plain::unsupported_entries(source).unwrap_or_default()
            } else {
                crate::pak::plain::system_files(source).unwrap_or_default()
            },
            packed: None,
        });
    }
    std::fs::create_dir_all(mods)
        .map_err(|e| format!("Failed to create mods folder {}: {e}", mods.display()))?;
    if replaced_disabled {
        crate::mods::delete_mod(mods, &disabled)?;
    }
    std::fs::copy(source, &dest).map_err(|e| format!("Failed to copy {file_name}: {e}"))?;

    for extension in COMPANION_EXTS {
        let name = format!("{stem}.{extension}");
        let (given, installed) = (from.join(&name), mods.join(&name));
        if given.is_file() {
            std::fs::copy(&given, &installed).map_err(|e| format!("Failed to copy {name}: {e}"))?;
        } else if installed.exists() {
            std::fs::remove_file(&installed)
                .map_err(|e| format!("Failed to remove the old {name}: {e}"))?;
        }
    }
    // A pak that does not read, or a rewrite that fails, leaves the mod as it came.
    let pak = options.io.pak();
    let stripped = if iostore {
        crate::pak::plain::strip_unsupported(&dest, &pak)
    } else {
        crate::pak::plain::strip_system_files(&dest, &pak)
    }
    .unwrap_or_default();
    Ok(InstallResult {
        file_name,
        replaced_disabled,
        replaced_enabled,
        stripped,
        packed: None,
    })
}

/// Whether the plain pak at `pak` holds packages. One that does not read is taken not to, and goes
/// in as it came.
fn holds_packages(pak: &Path) -> bool {
    crate::pak::plain::list_files(pak).is_ok_and(|files| {
        files.iter().any(|file| {
            let lower = file.to_ascii_lowercase();
            lower.ends_with(".uasset") || lower.ends_with(".umap")
        })
    })
}

/// Packs the files at `input` that `take` takes, a folder or a plain pak, into the IoStore mod
/// `stem` in `mods`.
fn install_packed(
    mods: &Path,
    input: &Path,
    take: Take,
    stem: &str,
    from: PackedFrom,
    options: &InstallOptions,
) -> Result<InstallResult, String> {
    let held = |suffix: &str| {
        ["pak", "utoc", "ucas"]
            .iter()
            .any(|ext| mods.join(format!("{stem}.{ext}{suffix}")).exists())
    };
    let (replaced_enabled, replaced_disabled) = (held(""), held(".disabled"));
    if (replaced_enabled || replaced_disabled) && !options.replace {
        return Err(format!(
            "{stem} is already installed: pass --overwrite to write over it"
        ));
    }
    let report = crate::mod_ops::pack_mod(
        input,
        take,
        stem,
        mods,
        options.replace,
        &options.io,
        options.game_libraries,
        options.dry_run,
    )?;
    Ok(InstallResult {
        file_name: format!("{stem}.pak"),
        replaced_disabled,
        replaced_enabled,
        stripped: Vec::new(),
        packed: Some(Packed {
            from,
            packages: report.packages,
            loose: report.loose,
            shader_libraries: report.shader_libraries,
            warnings: report.warnings,
        }),
    })
}

/// A folder an archive is unpacked into, removed once its mods are installed.
#[cfg_attr(not(feature = "archives"), allow(dead_code))]
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// An archive unpacked into a scratch folder of its own, removed when this drops.
pub struct UnpackedArchive(Scratch);

impl UnpackedArchive {
    pub fn dir(&self) -> &Path {
        &self.0.0
    }
}

/// Unpacks the zip, 7z or rar at `archive` into a scratch folder.
#[cfg(feature = "archives")]
pub fn unpack_archive(archive: &Path) -> Result<UnpackedArchive, String> {
    unpack(archive).map(UnpackedArchive)
}

#[cfg(not(feature = "archives"))]
pub fn unpack_archive(archive: &Path) -> Result<UnpackedArchive, String> {
    Err(format!(
        "{}: this build does not read archives",
        archive.display()
    ))
}

#[cfg(feature = "archives")]
fn unpack(archive: &Path) -> Result<Scratch, String> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);

    let kind = super::archive_kind(archive)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let scratch = Scratch(std::env::temp_dir().join(format!(
        "rivals_archive_{}_{}_{stamp}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )));
    std::fs::create_dir_all(&scratch.0).map_err(|e| format!("Failed to create temp dir: {e}"))?;
    match kind {
        super::ArchiveKind::Zip => extract_zip(archive, &scratch.0)?,
        super::ArchiveKind::SevenZ => extract_7z(archive, &scratch.0)?,
        super::ArchiveKind::Rar => extract_rar(archive, &scratch.0)?,
    }
    Ok(scratch)
}

/// Unpacks the archive at `archive`, shown as `at`, and looks through it for mods as through a
/// folder named after it, `depth` archives deep.
#[cfg(feature = "archives")]
fn open_archive(
    archive: &Path,
    at: &Path,
    depth: usize,
    unpacked: &mut Vec<Scratch>,
    found: &mut Vec<Located>,
) -> Result<(), String> {
    if depth > ARCHIVE_DEPTH {
        return Err(format!(
            "is inside {ARCHIVE_DEPTH} other archives, deeper than mods are looked for"
        ));
    }
    let scratch = unpack(archive)?;
    let dir = scratch.0.clone();
    unpacked.push(scratch);
    let name = archive
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    look_in(&dir, at, &name, depth, true, unpacked, found);
    Ok(())
}

#[cfg(not(feature = "archives"))]
fn open_archive(
    archive: &Path,
    _at: &Path,
    _depth: usize,
    _unpacked: &mut Vec<Scratch>,
    _found: &mut Vec<Located>,
) -> Result<(), String> {
    Err(format!(
        "{}: this build does not read archives",
        archive.display()
    ))
}

/// Where the archive entry `name` unpacks to under `into`, or nowhere for one whose path reaches
/// outside it or names a drive.
#[cfg(feature = "archives")]
fn entry_path(into: &Path, name: &str) -> Option<PathBuf> {
    let mut path = into.to_path_buf();
    let mut named = false;
    for part in name.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => return None,
            _ if part.contains(':') => return None,
            _ => {
                path.push(part);
                named = true;
            }
        }
    }
    named.then_some(path)
}

/// Creates the file at `path`, and the folders it is in.
#[cfg(feature = "archives")]
fn create_file(path: &Path) -> std::io::Result<std::fs::File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::File::create(path)
}

#[cfg(feature = "archives")]
fn extract_zip(archive: &Path, into: &Path) -> Result<(), String> {
    let file = std::fs::File::open(archive).map_err(|e| format!("Failed to open zip: {e}"))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("Invalid zip file: {e}"))?;
    for at in 0..zip.len() {
        let mut entry = zip.by_index(at).map_err(|e| e.to_string())?;
        if !entry.is_file() {
            continue;
        }
        let name = entry.name().to_string();
        let Some(path) = entry_path(into, &name) else {
            continue;
        };
        let mut out = create_file(&path).map_err(|e| format!("Failed to create {name}: {e}"))?;
        std::io::copy(&mut entry, &mut out)
            .map_err(|e| format!("Failed to extract {name}: {e}"))?;
    }
    Ok(())
}

#[cfg(feature = "archives")]
fn extract_7z(archive: &Path, into: &Path) -> Result<(), String> {
    let file = std::fs::File::open(archive).map_err(|e| format!("Failed to open 7z: {e}"))?;
    sevenz_rust2::decompress_with_extract_fn(file, into, |entry, reader, _| {
        if entry.is_directory() {
            return Ok(true);
        }
        let name = entry.name();
        let Some(path) = entry_path(into, name) else {
            return Ok(true);
        };
        let mut out = create_file(&path).map_err(|e| {
            sevenz_rust2::Error::Other(format!("Failed to create {name}: {e}").into())
        })?;
        std::io::copy(reader, &mut out).map_err(|e| {
            sevenz_rust2::Error::Other(format!("Failed to extract {name}: {e}").into())
        })?;
        Ok(true)
    })
    .map_err(|e| format!("Failed to read 7z archive: {e}"))
}

/// UnRAR source code may be used in any software to handle RAR archives without limitations free of
/// charge, but cannot be used to develop RAR (WinRAR) compatible archiver and to re-create RAR
/// compression algorithm, which is proprietary. Distribution of modified UnRAR source code in
/// separate form or as a part of other software is permitted, provided that full text of this
/// paragraph, starting from "UnRAR source code" words, is included in license, or in documentation
/// if license is not available, and in source code comments of resulting package.
#[cfg(feature = "rar")]
fn extract_rar(archive: &Path, into: &Path) -> Result<(), String> {
    let mut rar = unrar::Archive::new(archive)
        .open_for_processing()
        .map_err(|e| format!("Invalid rar file: {e}"))?;
    while let Some(header) = rar
        .read_header()
        .map_err(|e| format!("Failed to read rar entry: {e}"))?
    {
        let entry = header.entry();
        let name = entry.filename.to_string_lossy().into_owned();
        let path = entry_path(into, &name).filter(|_| entry.is_file());
        rar = match path {
            Some(path) => {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("Failed to create a folder for {name}: {e}"))?;
                }
                header
                    .extract_to(&path)
                    .map_err(|e| format!("Failed to extract {name}: {e}"))?
            }
            None => header
                .skip()
                .map_err(|e| format!("Failed to skip rar entry: {e}"))?,
        };
    }
    Ok(())
}

#[cfg(all(feature = "archives", not(feature = "rar")))]
fn extract_rar(archive: &Path, _into: &Path) -> Result<(), String> {
    Err(format!(
        "{}: this build does not read rar archives",
        archive.display()
    ))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::pak::plain::{PakOptions, list_files, write_files};
    use crate::pak::profile::PackCompression;
    use std::path::PathBuf;

    fn scratch(tag: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rivals-install-{tag}-{stamp}"));
        std::fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    fn trio(dir: &Path, stem: &str, body: &str) {
        for ext in ["pak", "utoc", "ucas"] {
            std::fs::write(dir.join(format!("{stem}.{ext}")), body).unwrap();
        }
    }

    fn put(root: &Path, rel: &str, bytes: &[u8]) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    const ZLIB: PakOptions = PakOptions {
        compression: PackCompression::Zlib,
        oodle_level: None,
    };

    /// Installs written with Zlib, so no Oodle library is needed.
    fn zlib() -> InstallOptions<'static> {
        InstallOptions {
            io: IoStoreOptions {
                compression: PackCompression::Zlib,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// What each mod found came to, by where it was found under `under`: the file it went in as,
    /// or the error.
    fn outcomes(found: &[Found], under: &Path) -> Vec<(String, Result<String, String>)> {
        found
            .iter()
            .map(|found| {
                let at = Path::new(&found.at)
                    .strip_prefix(under)
                    .map(|rel| rel.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_else(|_| found.at.clone());
                let result = match &found.result {
                    Ok(installed) => Ok(installed.file_name.clone()),
                    Err(e) => Err(e.clone()),
                };
                (at, result)
            })
            .collect()
    }

    /// An IoStore mod goes in without the entries an older packer left in its pak, which the game
    /// does not support beside a container, and a dry run says it would take them out; a plain pak
    /// mod is installed as it is.
    #[test]
    fn an_iostore_mod_s_pak_loses_what_the_game_does_not_support() {
        let from = scratch("unsupported-from");
        let mods = scratch("unsupported-mods");
        let listing = vec![(
            "chunknames".to_string(),
            b"Marvel/Content/A.uasset".to_vec(),
        )];
        write_files(&from.join("M_9999999_P.pak"), listing.clone(), &ZLIB).expect("pak");
        for ext in ["utoc", "ucas"] {
            std::fs::write(from.join(format!("M_9999999_P.{ext}")), b"").unwrap();
        }
        write_files(&from.join("Plain_9999999_P.pak"), listing, &ZLIB).expect("pak");
        let pak = from.join("M_9999999_P.pak");
        let dry = install(
            &mods,
            &pak,
            &InstallOptions {
                dry_run: true,
                ..Default::default()
            },
        )
        .expect("dry run");
        assert_eq!(dry[0].result.as_ref().unwrap().stripped, ["chunknames"]);
        assert_eq!(
            list_files(&pak).unwrap(),
            ["chunknames"],
            "the source is untouched"
        );
        let done = install(&mods, &pak, &InstallOptions::default()).expect("install");
        assert_eq!(done[0].result.as_ref().unwrap().stripped, ["chunknames"]);
        assert!(
            list_files(&mods.join("M_9999999_P.pak"))
                .unwrap()
                .is_empty()
        );
        let plain = install(
            &mods,
            &from.join("Plain_9999999_P.pak"),
            &InstallOptions::default(),
        )
        .expect("install");
        let plain = plain[0].result.as_ref().unwrap();
        assert!(plain.stripped.is_empty() && plain.packed.is_none());
        assert_eq!(
            list_files(&mods.join("Plain_9999999_P.pak")).unwrap(),
            ["chunknames"]
        );
        std::fs::remove_dir_all(&from).ok();
        std::fs::remove_dir_all(&mods).ok();
    }

    /// A plain pak goes in without the files folders leave, and keeps the rest, the listings an
    /// older packer left among them; an IoStore mod's pak loses both.
    #[test]
    fn a_pak_loses_the_files_folders_leave() {
        let from = scratch("system-from");
        let mods = scratch("system-mods");
        let listing = vec![
            ("Marvel/Config/DefaultGame.ini".to_string(), b"[A]".to_vec()),
            (
                "Marvel/Config/desktop.ini".to_string(),
                b"[.ShellClassInfo]".to_vec(),
            ),
            (
                "chunknames".to_string(),
                b"Marvel/Content/A.uasset".to_vec(),
            ),
        ];
        write_files(&from.join("Plain_9999999_P.pak"), listing.clone(), &ZLIB).expect("pak");
        write_files(&from.join("M_9999999_P.pak"), listing, &ZLIB).expect("pak");
        for ext in ["utoc", "ucas"] {
            std::fs::write(from.join(format!("M_9999999_P.{ext}")), b"").unwrap();
        }
        let installed = |name: &str| {
            let found = install(&mods, &from.join(name), &zlib()).expect("install");
            let mut stripped = found[0].result.as_ref().unwrap().stripped.clone();
            stripped.sort();
            let mut kept = list_files(&mods.join(name)).unwrap();
            kept.sort();
            (stripped, kept)
        };
        assert_eq!(
            installed("Plain_9999999_P.pak"),
            (
                vec!["Marvel/Config/desktop.ini".to_string()],
                vec![
                    "Marvel/Config/DefaultGame.ini".to_string(),
                    "chunknames".to_string()
                ]
            )
        );
        assert_eq!(
            installed("M_9999999_P.pak"),
            (
                vec![
                    "Marvel/Config/desktop.ini".to_string(),
                    "chunknames".to_string()
                ],
                vec!["Marvel/Config/DefaultGame.ini".to_string()]
            )
        );
        std::fs::remove_dir_all(&from).ok();
        std::fs::remove_dir_all(&mods).ok();
    }

    /// A pak comes with its container, from the pak or from either half of the container, and a
    /// second install of it is refused unless asked to replace.
    #[test]
    fn a_mod_installs_with_its_container() {
        let from = scratch("from");
        let mods = scratch("mods");
        trio(&from, "M_9999999_P", "new");
        let installed = install(
            &mods,
            &from.join("M_9999999_P.utoc"),
            &InstallOptions::default(),
        )
        .expect("install");
        assert_eq!(
            installed[0].result.as_ref().unwrap().file_name,
            "M_9999999_P.pak"
        );
        for ext in ["pak", "utoc", "ucas"] {
            assert_eq!(
                std::fs::read_to_string(mods.join(format!("M_9999999_P.{ext}"))).unwrap(),
                "new"
            );
        }
        let again = install(
            &mods,
            &from.join("M_9999999_P.pak"),
            &InstallOptions::default(),
        )
        .expect("found");
        assert!(
            again[0]
                .result
                .as_ref()
                .unwrap_err()
                .contains("--overwrite")
        );
        // A copy cannot be installed over itself.
        let itself = install_pak(
            &mods,
            &mods.join("M_9999999_P.pak"),
            &InstallOptions {
                replace: true,
                ..Default::default()
            },
        );
        assert!(itself.unwrap_err().contains("already the installed copy"));
        std::fs::remove_dir_all(&from).ok();
        std::fs::remove_dir_all(&mods).ok();
    }

    /// A mod named by its pak, container and cas goes in once, and a second mod named after them
    /// goes in too, so naming every file of a mod needs no `--overwrite`.
    #[test]
    fn a_mod_named_by_each_of_its_files_installs_once() {
        let from = scratch("each");
        let mods = scratch("mods-each");
        trio(&from, "M_9999999_P", "new");
        trio(&from, "N_9999999_P", "other");
        assert_eq!(
            install_source(&from.join("M_9999999_P.UTOC")),
            from.join("M_9999999_P.pak")
        );
        let named: Vec<PathBuf> = [
            "M_9999999_P.pak",
            "M_9999999_P.utoc",
            "N_9999999_P.ucas",
            "M_9999999_P.ucas",
        ]
        .iter()
        .map(|name| from.join(name))
        .collect();
        let results = install_each(&mods, &named, &InstallOptions::default());
        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0].named,
            vec![named[0].clone(), named[1].clone(), named[3].clone()]
        );
        assert_eq!(results[1].named, vec![named[2].clone()]);
        for each in &results {
            let found = each.result.as_ref().expect("found");
            assert!(found.iter().all(|found| found.result.is_ok()), "{each:?}");
        }
        assert_eq!(
            std::fs::read_to_string(mods.join("N_9999999_P.utoc")).unwrap(),
            "other"
        );
        std::fs::remove_dir_all(&from).ok();
        std::fs::remove_dir_all(&mods).ok();
    }

    /// A plain pak installed over an IoStore mod takes its container away, and a switched-off copy
    /// of it goes.
    #[test]
    fn a_replacement_leaves_no_stale_companion() {
        let from = scratch("plain");
        let mods = scratch("mods-stale");
        trio(&mods, "M_9999999_P", "old");
        std::fs::write(mods.join("M_9999999_P.pak.disabled"), "off").unwrap();
        std::fs::write(from.join("M_9999999_P.pak"), "plain").unwrap();
        let result = install_pak(
            &mods,
            &from.join("M_9999999_P.pak"),
            &InstallOptions {
                replace: true,
                ..Default::default()
            },
        )
        .expect("install");
        assert!(result.replaced_enabled && result.replaced_disabled);
        assert_eq!(
            std::fs::read_to_string(mods.join("M_9999999_P.pak")).unwrap(),
            "plain"
        );
        assert!(!mods.join("M_9999999_P.utoc").exists());
        assert!(!mods.join("M_9999999_P.ucas").exists());
        assert!(!mods.join("M_9999999_P.pak.disabled").exists());
        std::fs::remove_dir_all(&from).ok();
        std::fs::remove_dir_all(&mods).ok();
    }

    /// A folder holding a mix of mods installs each: an IoStore mod and a plain pak as they are,
    /// at any depth, and a folder of a mod's loose files packed into a container named after it,
    /// with only what is under its game folders. Packages under no game folder, and a container
    /// with no pak, are named and left out; the rest still goes in.
    #[test]
    fn a_mixed_folder_installs_each_mod_in_it() {
        let from = scratch("mixed");
        let mods = scratch("mods-mixed");
        trio(&from, "A_9999999_P", "a");
        std::fs::create_dir_all(from.join("Nested/Deeper")).unwrap();
        write_files(
            &from.join("Nested/Deeper/Plain_9999999_P.pak"),
            vec![("Marvel/Config/DefaultGame.ini".to_string(), b"[A]".to_vec())],
            &ZLIB,
        )
        .expect("pak");
        put(&from, "Skin/Marvel/Content/WwiseAudio/Skin.bnk", b"bank");
        put(
            &from,
            "Skin/Marvel/Content/WwiseAudio/desktop.ini",
            b"[.ShellClassInfo]",
        );
        put(&from, "Skin/readme.txt", b"by someone");
        put(&from, "Skin/preview.png", b"png");
        put(&from, "Loose/Content/Thing.uasset", b"asset");
        put(&from, "Loose/Content/Thing.uexp", b"exports");
        put(&from, "Orphan_9999999_P.utoc", b"toc");
        put(&from, "notes.txt", b"ignored");

        let found = install(&mods, &from, &zlib()).expect("install");
        let seen = outcomes(&found, &from);
        let ok = |at: &str, file: &str| (at.to_string(), Ok(file.to_string()));
        assert_eq!(seen[0], ok("A_9999999_P.pak", "A_9999999_P.pak"));
        assert_eq!(
            seen[1].0, "Loose",
            "the packages no game folder holds: {seen:?}"
        );
        assert!(
            seen[1]
                .1
                .as_ref()
                .unwrap_err()
                .contains("no Marvel or Engine folder")
        );
        assert_eq!(
            seen[2],
            ok("Nested/Deeper/Plain_9999999_P.pak", "Plain_9999999_P.pak")
        );
        assert_eq!(seen[3].0, "Orphan_9999999_P.utoc");
        assert!(
            seen[3]
                .1
                .as_ref()
                .unwrap_err()
                .contains("no .pak beside it")
        );
        assert_eq!(seen[4], ok("Skin", "Skin_9999999_P.pak"));
        assert_eq!(seen.len(), 5, "{seen:?}");

        let skin = found[4].result.as_ref().unwrap();
        let packed = skin.packed.as_ref().expect("packed");
        assert_eq!(packed.from, PackedFrom::Folder);
        assert_eq!(packed.loose, ["Marvel/Content/WwiseAudio/Skin.bnk"]);
        assert_eq!(
            list_files(&mods.join("Skin_9999999_P.pak")).unwrap(),
            ["Marvel/Content/WwiseAudio/Skin.bnk"],
            "the readme and preview stay out"
        );
        assert!(mods.join("Skin_9999999_P.utoc").is_file());
        assert!(mods.join("Skin_9999999_P.ucas").is_file());
        assert_eq!(
            std::fs::read_to_string(mods.join("A_9999999_P.ucas")).unwrap(),
            "a"
        );
        assert!(mods.join("Plain_9999999_P.pak").is_file());
        assert!(!mods.join("Orphan_9999999_P.utoc").exists());

        // A dry run says the same and writes nothing.
        let elsewhere = scratch("mods-mixed-dry");
        let dry = install(
            &elsewhere,
            &from,
            &InstallOptions {
                dry_run: true,
                ..zlib()
            },
        )
        .expect("dry run");
        assert_eq!(outcomes(&dry, &from), seen);
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
        std::fs::remove_dir_all(&from).ok();
        std::fs::remove_dir_all(&mods).ok();
        std::fs::remove_dir_all(&elsewhere).ok();
    }

    /// Two mods that would go in under one name, ignoring case, are both refused, naming each
    /// other, and neither is written; the others still go in.
    #[test]
    fn two_mods_that_would_take_one_name_are_both_refused() {
        let from = scratch("clash");
        let mods = scratch("mods-clash");
        std::fs::create_dir_all(from.join("Red")).unwrap();
        std::fs::create_dir_all(from.join("Blue")).unwrap();
        trio(&from.join("Red"), "Color_9999999_P", "red");
        trio(&from.join("Blue"), "color_9999999_p", "blue");
        trio(&from, "Other_9999999_P", "other");
        let found = install(&mods, &from, &InstallOptions::default()).expect("install");
        let seen = outcomes(&found, &from);
        assert!(seen[0].1.as_ref().unwrap_err().contains("Red"), "{seen:?}");
        assert!(seen[2].1.as_ref().unwrap_err().contains("Blue"));
        assert_eq!(seen[1].1, Ok("Other_9999999_P.pak".to_string()));
        assert!(!mods.join("Color_9999999_P.pak").exists());
        std::fs::remove_dir_all(&from).ok();
        std::fs::remove_dir_all(&mods).ok();
    }

    /// The mods folder, a folder in it and one holding it are refused, as is a mod's game folder
    /// given without the folder holding it, and a folder with no mod in it.
    #[test]
    fn folders_that_are_no_set_of_mods_are_refused() {
        let root = scratch("refused");
        let mods = root.join("Paks/~mods");
        std::fs::create_dir_all(mods.join("sub")).unwrap();
        let refused = |source: &Path| install(&mods, source, &InstallOptions::default());
        assert!(refused(&mods).unwrap_err().contains("is in"));
        assert!(refused(&mods.join("sub")).unwrap_err().contains("is in"));
        assert!(refused(&root.join("Paks")).unwrap_err().contains("holds"));
        put(&root, "Skin/Marvel/Content/A.bnk", b"bank");
        assert!(
            refused(&root.join("Skin/Marvel"))
                .unwrap_err()
                .contains("give the folder that holds it")
        );
        put(&root, "Empty/notes.txt", b"nothing");
        assert!(
            refused(&root.join("Empty"))
                .unwrap_err()
                .contains("holds no mod")
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// A copy of the game's folders with mods in its `Paks` is no mod's loose files: the mods in
    /// it go in as they are.
    #[test]
    fn a_game_folder_holding_mods_is_looked_through() {
        let from = scratch("copy");
        let mods = scratch("mods-copy");
        let paks = from.join("Marvel/Content/Paks/~mods");
        std::fs::create_dir_all(&paks).unwrap();
        trio(&paks, "M_9999999_P", "m");
        let found = install(&mods, &from, &InstallOptions::default()).expect("install");
        assert_eq!(
            outcomes(&found, &from),
            [(
                "Marvel/Content/Paks/~mods/M_9999999_P.pak".to_string(),
                Ok("M_9999999_P.pak".to_string())
            )]
        );
        std::fs::remove_dir_all(&from).ok();
        std::fs::remove_dir_all(&mods).ok();
    }

    #[cfg(feature = "archives")]
    fn zip(path: &Path, files: &[(&str, &[u8])]) {
        use std::io::Write;
        let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        for (name, body) in files {
            zip.start_file(*name, options).unwrap();
            zip.write_all(body).unwrap();
        }
        zip.finish().unwrap();
    }

    /// A zip's mods install from any folder in it, and an entry whose path climbs out of the
    /// archive lands nowhere outside the folder it unpacks into.
    #[cfg(feature = "archives")]
    #[test]
    fn a_zip_installs_its_mods_from_any_folder_in_it() {
        let from = scratch("zip");
        let mods = scratch("mods-zip").join("~mods");
        let archive = from.join("Pack.zip");
        zip(
            &archive,
            &[
                ("Nested/Deep/A_9999999_P.pak", b"a"),
                ("Nested/Deep/A_9999999_P.utoc", b"a"),
                ("Nested\\Deep\\A_9999999_P.ucas", b"a"),
                ("readme.txt", b"ignored"),
                ("../evil_9999999_P.pak", b"evil"),
            ],
        );
        let installed = install(&mods, &archive, &InstallOptions::default()).expect("install");
        assert_eq!(
            outcomes(&installed, &archive),
            [(
                "Nested/Deep/A_9999999_P.pak".to_string(),
                Ok("A_9999999_P.pak".to_string())
            )]
        );
        for ext in ["pak", "utoc", "ucas"] {
            assert!(mods.join(format!("A_9999999_P.{ext}")).is_file());
        }
        assert!(!mods.join("readme.txt").exists());
        assert!(!mods.parent().unwrap().join("evil_9999999_P.pak").exists());
        std::fs::remove_dir_all(&from).ok();
        std::fs::remove_dir_all(mods.parent().unwrap()).ok();
    }

    /// A zip of a mod's loose files packs into a mod named after the zip, with a patch number when
    /// the name has none, and a zip inside it is opened too.
    #[cfg(feature = "archives")]
    #[test]
    fn a_zip_of_loose_files_installs_packed() {
        let from = scratch("zip-loose");
        let mods = scratch("mods-zip-loose");
        let inner = from.join("inner.zip");
        zip(&inner, &[("Inner_9999999_P.pak", b"inner")]);
        let inner = std::fs::read(&inner).unwrap();
        let archive = from.join("Cool Skin.zip");
        zip(
            &archive,
            &[
                ("Marvel/Content/WwiseAudio/Skin.bnk", b"bank"),
                ("readme.txt", b"by someone"),
                ("Extras/Inner.zip", &inner),
            ],
        );
        let installed = install(&mods, &archive, &zlib()).expect("install");
        assert_eq!(
            outcomes(&installed, &archive),
            [
                (String::new(), Ok("Cool Skin_9999999_P.pak".to_string())),
                (
                    "Extras/Inner.zip/Inner_9999999_P.pak".to_string(),
                    Ok("Inner_9999999_P.pak".to_string())
                ),
            ]
        );
        assert_eq!(
            list_files(&mods.join("Cool Skin_9999999_P.pak")).unwrap(),
            ["Marvel/Content/WwiseAudio/Skin.bnk"]
        );
        std::fs::remove_dir_all(&from).ok();
        std::fs::remove_dir_all(&mods).ok();
    }

    /// A 7z keeps its folders as it unpacks, so a mod's loose files in a folder of it pack into a
    /// mod named after that folder.
    #[cfg(feature = "archives")]
    #[test]
    fn a_7z_keeps_its_folders() {
        let from = scratch("7z");
        let mods = scratch("mods-7z");
        put(&from, "files/Skin.bnk", b"bank");
        put(&from, "files/readme.txt", b"by someone");
        let archive = from.join("Pack.7z");
        let mut writer = sevenz_rust2::ArchiveWriter::create(&archive).unwrap();
        for (file, name) in [
            ("Skin.bnk", "Skin/Marvel/Content/WwiseAudio/Skin.bnk"),
            ("readme.txt", "Skin/readme.txt"),
        ] {
            let path = from.join("files").join(file);
            let entry = sevenz_rust2::ArchiveEntry::from_path(&path, name.to_string());
            writer
                .push_archive_entry(entry, Some(std::fs::File::open(&path).unwrap()))
                .unwrap();
        }
        writer.finish().unwrap();
        let installed = install(&mods, &archive, &zlib()).expect("install");
        assert_eq!(
            outcomes(&installed, &archive),
            [("Skin".to_string(), Ok("Skin_9999999_P.pak".to_string()))]
        );
        assert_eq!(
            list_files(&mods.join("Skin_9999999_P.pak")).unwrap(),
            ["Marvel/Content/WwiseAudio/Skin.bnk"]
        );
        std::fs::remove_dir_all(&from).ok();
        std::fs::remove_dir_all(&mods).ok();
    }

    /// A plain pak holding packages, which the game loads none of from one, goes in packed into a
    /// container, as does a folder holding the same package; each holds it.
    #[test]
    fn packages_go_in_packed_into_a_container() {
        let Ok(root) = std::env::var("RIVALS_GAME_ROOT") else {
            return;
        };
        if std::env::var_os("OODLE_LIB_PATH").is_none() {
            return;
        }
        let source = format!(
            "{}/MarvelGame/Marvel/Content/Paks/pakchunk0-Windows.utoc",
            root.replace('\\', "/")
        );
        let entry = "Marvel/Content/Marvel/Data/DataTable/MarvelHeroTable.uasset";
        let loaded =
            crate::asset::load_bundle(&root, &source, entry, crate::asset::AssetSource::Utoc)
                .expect("load");
        let uexp = entry.replace(".uasset", ".uexp");
        let from = scratch("packages");
        let mods = scratch("mods-packages");
        write_files(
            &from.join("Table_9999999_P.pak"),
            vec![
                (entry.to_string(), loaded.asset_file_buffer.clone()),
                (uexp.clone(), loaded.exports_file_buffer.clone()),
                ("Marvel/Config/DefaultGame.ini".to_string(), b"[A]".to_vec()),
            ],
            &ZLIB,
        )
        .expect("pak");
        put(&from, &format!("Folder/{entry}"), &loaded.asset_file_buffer);
        put(
            &from,
            &format!("Folder/{uexp}"),
            &loaded.exports_file_buffer,
        );

        let found = install(&mods, &from, &zlib()).expect("install");
        assert_eq!(found.len(), 2, "{found:?}");
        for (found, stem, from) in [
            (&found[0], "Folder_9999999_P", PackedFrom::Folder),
            (&found[1], "Table_9999999_P", PackedFrom::PlainPak),
        ] {
            let installed = found.result.as_ref().expect("installed");
            assert_eq!(installed.file_name, format!("{stem}.pak"));
            let packed = installed.packed.as_ref().expect("packed");
            assert_eq!((packed.from, packed.packages), (from, 1));
            let held = crate::mod_ops::mod_packages(&mods.join(format!("{stem}.utoc")))
                .expect("container");
            assert_eq!(held.len(), 1);
        }
        assert_eq!(
            list_files(&mods.join("Table_9999999_P.pak")).unwrap(),
            ["Marvel/Config/DefaultGame.ini"],
            "the pak keeps the files beside the packages"
        );
        std::fs::remove_dir_all(&from).ok();
        std::fs::remove_dir_all(&mods).ok();
    }
}
