//! Whole-mod operations: copying a mod under a new name, renaming one, taking packages out of one,
//! and switching one off or on.
//!
//! An IoStore container's id comes from its file name, so a container copied or renamed on disk
//! alone still mounts under its old id, and two containers with one id clash. Copying and renaming
//! rebuild the container under the name it ends up with.

use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::path::{Path, PathBuf};

use retoc::version::EngineVersion;
use retoc::zen::FZenPackageHeader;
use retoc::{EIoChunkType, FIoChunkId, FPackageId};
use serde::Serialize;

use crate::asset::{PathFilter, package_name_of_entry};
use crate::pak::containers::{list_utoc_paths, open_utoc};
use crate::pak::iostore_out::{self, IoStoreOptions, IoStoreReport};
use crate::pak::plain::PakOptions;
use crate::pak::profile::strip_mount_prefix;

const ENGINE: EngineVersion = EngineVersion::UE5_3;

/// The files a mod is made of. A pak-only mod has the first alone.
const PARTS: [&str; 3] = ["pak", "utoc", "ucas"];

/// A mod's files, found from any one of them, and whether it is switched off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModFiles {
    pub dir: PathBuf,
    pub stem: String,
    pub disabled: bool,
}

impl ModFiles {
    /// The mod `path` belongs to: its `.pak`, `.utoc` or `.ucas`, with `.disabled` or without.
    pub fn of(path: &Path) -> Result<Self, String> {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| format!("{} names no file", path.display()))?;
        let (base, disabled) = match name.strip_suffix(".disabled") {
            Some(base) => (base, true),
            None => (name.as_str(), false),
        };
        let stem = PARTS
            .iter()
            .find_map(|extension| base.strip_suffix(&format!(".{extension}")))
            .ok_or_else(|| format!("{name} is not a mod's .pak, .utoc or .ucas"))?;
        let files = Self {
            dir: path.parent().map(Path::to_path_buf).unwrap_or_default(),
            stem: stem.to_string(),
            disabled,
        };
        if !files.part("pak").is_file() && !files.part("utoc").is_file() {
            return Err(format!(
                "no mod named {} in {}",
                files.stem,
                files.dir.display()
            ));
        }
        Ok(files)
    }

    /// Where one of its files is, in the state the mod is in.
    pub fn part(&self, extension: &str) -> PathBuf {
        self.part_as(extension, self.disabled)
    }

    /// Whether the game mounts it: switched on and inside `mods`, the folder it mounts mods from,
    /// when that is known. The folders are compared resolved, so two spellings of one path agree;
    /// one not made yet is compared as written.
    pub fn mounts(&self, mods: &Path) -> bool {
        if self.disabled || mods.as_os_str().is_empty() {
            return false;
        }
        match (self.dir.canonicalize(), mods.canonicalize()) {
            (Ok(dir), Ok(mods)) => dir.starts_with(mods),
            _ => self.dir.starts_with(mods),
        }
    }

    fn part_as(&self, extension: &str, disabled: bool) -> PathBuf {
        let suffix = if disabled { ".disabled" } else { "" };
        self.dir.join(format!("{}.{extension}{suffix}", self.stem))
    }

    /// The files it has, in its state.
    pub fn files(&self) -> Vec<PathBuf> {
        PARTS
            .iter()
            .map(|extension| self.part(extension))
            .filter(|path| path.is_file())
            .collect()
    }

    pub fn is_iostore(&self) -> bool {
        self.part("utoc").is_file()
    }

    /// Every file of its name, switched on or off, which another mod would be written over.
    fn any_files(&self) -> Vec<PathBuf> {
        PARTS
            .iter()
            .flat_map(|extension| {
                [
                    self.part_as(extension, false),
                    self.part_as(extension, true),
                ]
            })
            .filter(|path| path.is_file())
            .collect()
    }

    fn shown(&self) -> String {
        self.part("pak").to_string_lossy().into_owned()
    }
}

/// One package of a mod, with what it needs to load.
#[derive(Debug, Clone)]
pub struct ModPackage {
    pub id: FPackageId,
    /// As the container names it, mount-relative.
    pub entry: String,
    pub name: Option<String>,
    /// The packages it imports, which the loader has to find for it to load at all.
    pub imports: Vec<FPackageId>,
    /// The packages its names mention by path, which it may load when it runs, or may not.
    pub mentions: Vec<String>,
    /// The names its header gives the packages it imports, which is the only place a package
    /// nothing has any more is still named.
    pub import_names: HashMap<FPackageId, String>,
}

impl ModPackage {
    fn shown(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.entry)
    }
}

/// Every package in the container at `utoc`, by entry, with what each imports and mentions. The
/// headers alone are read, so no mappings are needed.
pub fn mod_packages(utoc: &Path) -> Result<Vec<ModPackage>, String> {
    let store = open_utoc(&utoc.to_string_lossy())?;
    let versions = store
        .container_file_version()
        .zip(store.container_header_version());
    let mut out = Vec::new();
    for package in store.packages() {
        let id = package.id();
        let chunk = FIoChunkId::from_package_id(id, 0, EIoChunkType::ExportBundleData);
        let Some(path) = store.chunk_path(chunk) else {
            continue;
        };
        let entry = strip_mount_prefix(&path);
        let name = package_name_of_entry(&entry);
        let store_entry = store.package_store_entry(id);
        let imports = store_entry
            .as_ref()
            .map(|entry| entry.imported_packages.clone())
            .unwrap_or_default();
        let mut mentions = Vec::new();
        let mut import_names = HashMap::new();
        if let Some((toc_version, header_version)) = versions
            && let Ok(data) = store.read(chunk)
            && let Ok(header) = FZenPackageHeader::deserialize(
                &mut Cursor::new(&data),
                store_entry,
                toc_version,
                header_version,
                Some(ENGINE.package_file_version()),
            )
        {
            import_names.extend(
                header
                    .imported_packages
                    .iter()
                    .copied()
                    .zip(header.imported_package_names.iter().cloned()),
            );
            for text in header.name_map.copy_raw_names() {
                if !text.starts_with('/') || text.starts_with("/Script/") {
                    continue;
                }
                let mentioned = text.split(['.', ':']).next().unwrap_or(&text);
                if name
                    .as_deref()
                    .is_none_or(|own| !own.eq_ignore_ascii_case(mentioned))
                {
                    mentions.push(mentioned.to_string());
                }
            }
        }
        mentions.sort();
        mentions.dedup();
        out.push(ModPackage {
            id,
            entry,
            name,
            imports,
            mentions,
            import_names,
        });
    }
    out.sort_by(|a, b| a.entry.cmp(&b.entry));
    Ok(out)
}

/// A package the mod keeps that needs one it leaves out.
#[derive(Debug, Clone, Serialize)]
pub struct Needed {
    pub package: String,
    pub needs: String,
    /// It imports it, so it fails to load without it. Otherwise it names its path, which only
    /// matters if it loads it.
    pub imported: bool,
}

/// What each kept package needs of the dropped ones, imports first.
fn needed(packages: &[ModPackage], dropped: &HashSet<FPackageId>) -> Vec<Needed> {
    let by_id: HashMap<FPackageId, &ModPackage> = packages
        .iter()
        .filter(|package| dropped.contains(&package.id))
        .map(|package| (package.id, package))
        .collect();
    let mut out = Vec::new();
    for package in packages.iter().filter(|p| !dropped.contains(&p.id)) {
        let mut imported = HashSet::new();
        for id in &package.imports {
            if let Some(gone) = by_id.get(id) {
                imported.insert(gone.id);
                out.push(Needed {
                    package: package.shown().to_string(),
                    needs: gone.shown().to_string(),
                    imported: true,
                });
            }
        }
        for mention in &package.mentions {
            let gone = by_id.values().find(|gone| {
                !imported.contains(&gone.id)
                    && gone
                        .name
                        .as_deref()
                        .is_some_and(|name| name.eq_ignore_ascii_case(mention))
            });
            if let Some(gone) = gone {
                out.push(Needed {
                    package: package.shown().to_string(),
                    needs: gone.shown().to_string(),
                    imported: false,
                });
            }
        }
    }
    out.sort_by_key(|need| !need.imported);
    out
}

/// Refuses a selection that leaves a package unable to load, unless the caller accepts it. A dry
/// run is told instead, so it still lists everything.
fn check_needed(
    needs: &[Needed],
    allow_missing: bool,
    dry_run: bool,
    warnings: &mut Vec<String>,
) -> Result<(), String> {
    let imported: Vec<String> = needs
        .iter()
        .filter(|need| need.imported)
        .map(|need| format!("{} imports {}", need.package, need.needs))
        .collect();
    if imported.is_empty() || allow_missing {
        return Ok(());
    }
    let refusal = format!(
        "a package kept would no longer load, since it imports one left out: {}. Keep those too, \
         or pass --allow-missing when they will be taken out of what imports them",
        imported.join("; ")
    );
    if dry_run {
        warnings.push(format!("this would be refused: {refusal}"));
        return Ok(());
    }
    Err(refusal)
}

/// Whether `package` is the one `entry` names: its container path or its package name.
fn is_entry(package: &ModPackage, entry: &str) -> bool {
    let wanted = strip_mount_prefix(&entry.replace('\\', "/"));
    let wanted = wanted.trim_end_matches(".uasset").trim_end_matches(".umap");
    package
        .entry
        .trim_end_matches(".uasset")
        .trim_end_matches(".umap")
        .eq_ignore_ascii_case(wanted)
        || package
            .name
            .as_deref()
            .is_some_and(|name| name.eq_ignore_ascii_case(entry.trim()))
}

/// Removes a directory when it goes out of scope.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The folder of loose files a pack reads: `input` itself, or the plain pak `input` names,
/// switched on or off, unpacked into a scratch folder that goes when the [`Scratch`] beside it
/// does. The pak beside an IoStore container holds only part of its mod, so it is refused.
fn pack_source(input: &Path) -> Result<(PathBuf, Option<Scratch>), String> {
    let name = input
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let is_pak = name.ends_with(".pak") || name.ends_with(".pak.disabled");
    if input.is_dir() || !is_pak || !input.is_file() {
        return Ok((input.to_path_buf(), None));
    }
    let files = ModFiles::of(input)?;
    if files.is_iostore() {
        return Err(format!(
            "{} is the pak beside the IoStore mod {}, which holds only part of it; `mods copy` \
             copies a whole mod",
            input.display(),
            files.stem
        ));
    }
    let scratch = fresh_dir(std::env::temp_dir().join(format!(
        "rivals-unpacked-{}-{}",
        files.stem,
        std::process::id()
    )))?;
    crate::pak::plain::extract(input, &scratch.0, &|name| {
        !crate::pak::plain::is_unsupported_entry(name)
    })?;
    Ok((scratch.0.clone(), Some(scratch)))
}

fn fresh_dir(path: PathBuf) -> Result<Scratch, String> {
    if path.exists() {
        std::fs::remove_dir_all(&path).ok();
    }
    std::fs::create_dir_all(&path)
        .map_err(|e| format!("Could not create {}: {e}", path.display()))?;
    Ok(Scratch(path))
}

/// Builds the container at `source` as the mod `to`, in `to`'s state and without `dropped`, and
/// swaps it in over whatever `to` was.
fn rebuild(
    source: &Path,
    to: &ModFiles,
    dropped: &HashSet<FPackageId>,
    options: &IoStoreOptions,
) -> Result<IoStoreReport, String> {
    let out = fresh_dir(to.dir.join(format!(".{}_rebuilt", to.stem)))?;
    let built = out.0.join(format!("{}.utoc", to.stem));
    let mut report = iostore_out::copy_container(source, &built, dropped, options)?;
    let suffix = if to.disabled { ".disabled" } else { "" };
    let mut extensions = Vec::new();
    for extension in PARTS {
        let staged = out.0.join(format!("{}.{extension}", to.stem));
        if !staged.is_file() {
            continue;
        }
        let named = format!("{extension}{suffix}");
        if !suffix.is_empty() {
            std::fs::rename(&staged, out.0.join(format!("{}.{named}", to.stem)))
                .map_err(|e| format!("Could not name {}: {e}", staged.display()))?;
        }
        extensions.push(named);
    }
    let extensions: Vec<&str> = extensions.iter().map(String::as_str).collect();
    iostore_out::swap_into_place(&to.dir, &to.stem, &out.0, &extensions)?;
    report.utoc = to.part("utoc");
    Ok(report)
}

/// The name a mod is copied or renamed to, given as a name or a path, with or without an
/// extension. A bare name goes in `home`, and a folder given is written whole, as the mod's own
/// folder is, so the two compare alike. Where a path is allowed, one naming a folder that exists,
/// or ending in a separator, is that folder, as `cp` takes one: the mod goes in under `own_name`,
/// and without one the folder is refused, since nothing names the mod.
fn destination(
    to: &str,
    home: &Path,
    allow_folder: bool,
    own_name: Option<&str>,
) -> Result<ModFiles, String> {
    let given = to.trim();
    let path = Path::new(given);
    let as_folder = given.ends_with(['/', '\\']);
    if allow_folder
        && (as_folder || ((path.components().count() > 1 || path.is_absolute()) && path.is_dir()))
    {
        let Some(name) = own_name else {
            return Err(format!(
                "{to} is a folder: give the mod's name too, such as {}",
                path.join("MyMod_9999999_P").display()
            ));
        };
        if !path.is_dir() {
            return Err(format!("{to} names a folder that does not exist"));
        }
        return Ok(ModFiles {
            dir: std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()),
            stem: name.to_string(),
            disabled: false,
        });
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| format!("{to} is not a mod name"))?;
    let name = name.strip_suffix(".disabled").unwrap_or(&name);
    let stem = PARTS
        .iter()
        .find_map(|extension| name.strip_suffix(&format!(".{extension}")))
        .unwrap_or(name);
    if stem.is_empty() {
        return Err(format!("{to} is not a mod name"));
    }
    let dir = match path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        Some(dir) if allow_folder => std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf()),
        Some(_) => {
            return Err(format!(
                "{to} names a folder: a mod is renamed where it is, so give a name alone"
            ));
        }
        None => home.to_path_buf(),
    };
    Ok(ModFiles {
        dir,
        stem: stem.to_string(),
        disabled: false,
    })
}

/// A name the game mounts below every patch, so the game's own copy of whatever it overrides wins.
fn priority_note(files: &ModFiles) -> Option<String> {
    crate::mods::pak_priority(&format!("{}.pak", files.stem))
        .is_none()
        .then(|| {
            format!(
                "{} has no _<number>_P suffix, so it mounts below the game's patches and their \
                 copies of anything it overrides win",
                files.stem
            )
        })
}

#[derive(Debug, Clone, Default)]
pub struct CopyOptions {
    /// Keep only the packages whose path or name contains one of these, ignoring case. Empty
    /// keeps every one.
    pub only: Vec<String>,
    pub allow_missing: bool,
    pub replace: bool,
    pub dry_run: bool,
}

#[derive(Debug, Serialize)]
pub struct CopyReport {
    pub from: String,
    pub to: String,
    pub kept: Vec<String>,
    pub dropped: Vec<String>,
    pub needed: Vec<Needed>,
    pub warnings: Vec<String>,
    pub written: bool,
}

/// Copies the mod `from` to `to`, rebuilt under the new name so it mounts as a mod of its own,
/// keeping only what `options.only` picks. A bare name goes in `mods`, where the game and every
/// command look for a mod by name, wherever `from` is. The copy is switched on.
pub fn copy_mod(
    from: &ModFiles,
    to: &str,
    mods: &Path,
    options: &CopyOptions,
    io: &IoStoreOptions,
) -> Result<CopyReport, String> {
    let dest = destination(to, mods, true, Some(&from.stem))?;
    if dest.dir == from.dir && dest.stem.eq_ignore_ascii_case(&from.stem) {
        return Err(format!("{} is the mod being copied", dest.stem));
    }
    let taken = dest.any_files();
    if !taken.is_empty() && !options.replace {
        return Err(format!(
            "{} is already there: pass --overwrite to write over it",
            taken[0].display()
        ));
    }
    let mut report = CopyReport {
        from: from.shown(),
        to: dest.shown(),
        kept: Vec::new(),
        dropped: Vec::new(),
        needed: Vec::new(),
        warnings: priority_note(&dest).into_iter().collect(),
        written: false,
    };

    if !from.is_iostore() {
        if !options.only.is_empty() {
            return Err(format!(
                "{} is a plain pak, which this copies whole: --filter picks packages of an IoStore \
                 mod",
                from.stem
            ));
        }
        if !options.dry_run {
            std::fs::create_dir_all(&dest.dir)
                .map_err(|e| format!("Could not create {}: {e}", dest.dir.display()))?;
            std::fs::copy(from.part("pak"), dest.part("pak"))
                .map_err(|e| format!("Could not copy {}: {e}", from.stem))?;
            remove_others(&dest, &taken, &mut report.warnings);
            report.written = true;
        }
        return Ok(report);
    }

    let source = from.part("utoc");
    let packages = mod_packages(&source)?;
    let filters: Vec<PathFilter> = options
        .only
        .iter()
        .map(|text| PathFilter::new(Some(text)))
        .collect();
    let dropped: HashSet<FPackageId> = packages
        .iter()
        .filter(|package| {
            !filters.is_empty() && !filters.iter().any(|filter| filter.matches(&package.entry))
        })
        .map(|package| package.id)
        .collect();
    // A mod holding no package, its files all in its pak, copies whole.
    if !filters.is_empty() && dropped.len() == packages.len() {
        return Err(format!(
            "--filter picks nothing in {}: it holds {} package(s)",
            from.stem,
            packages.len()
        ));
    }
    for package in &packages {
        let list = if dropped.contains(&package.id) {
            &mut report.dropped
        } else {
            &mut report.kept
        };
        list.push(package.shown().to_string());
    }
    report.needed = needed(&packages, &dropped);
    check_needed(
        &report.needed,
        options.allow_missing,
        options.dry_run,
        &mut report.warnings,
    )?;
    // The two compete only when the game mounts both.
    if from.mounts(mods) && dest.mounts(mods) {
        report.warnings.push(format!(
            "{} is still switched on and holds the same packages, so whichever mounts later wins: \
             switch one off",
            from.stem
        ));
    }
    if options.dry_run {
        return Ok(report);
    }
    std::fs::create_dir_all(&dest.dir)
        .map_err(|e| format!("Could not create {}: {e}", dest.dir.display()))?;
    rebuild(&source, &dest, &dropped, io)?;
    remove_others(&dest, &taken, &mut report.warnings);
    report.written = true;
    Ok(report)
}

#[derive(Debug, Serialize)]
pub struct IntoReport {
    pub from: String,
    pub into: String,
    /// The packages written into the mod, each as it was stored where it came from.
    pub copied: Vec<String>,
    /// Those the mod held a copy of already, which theirs replaces.
    pub replaced: Vec<String>,
    pub needed: Vec<Needed>,
    pub warnings: Vec<String>,
    pub written: bool,
}

impl IntoReport {
    fn new(from: String, into: &ModFiles) -> Self {
        Self {
            from,
            into: into.shown(),
            copied: Vec::new(),
            replaced: Vec::new(),
            needed: Vec::new(),
            warnings: Vec::new(),
            written: false,
        }
    }
}

fn iostore_target(into: &ModFiles) -> Result<Vec<ModPackage>, String> {
    if !into.is_iostore() {
        return Err(format!(
            "{} is a plain pak, which loads no packages: copy them into an IoStore mod",
            into.stem
        ));
    }
    mod_packages(&into.part("utoc"))
}

/// Copies the packages of the mod `from` that `options.only` picks, every one when it picks none,
/// into the IoStore mod `into`, in one rewrite that keeps everything else `into` holds. Each goes in
/// as `from` stores it, its chunks and store entry byte for byte, over `into`'s own copy of it. A
/// package copied that imports one left behind is refused as `copy_mod` refuses it, unless `into`
/// ships that one already. `options.replace` has no part in it. `mods` is the folder the game
/// mounts mods from, where two copies of a package compete.
pub fn copy_into_mod(
    from: &ModFiles,
    into: &ModFiles,
    mods: &Path,
    options: &CopyOptions,
    io: &IoStoreOptions,
) -> Result<IntoReport, String> {
    if from.dir == into.dir && from.stem.eq_ignore_ascii_case(&into.stem) {
        return Err(format!("{} is the mod being copied from", into.stem));
    }
    if !from.is_iostore() {
        return Err(format!(
            "{} is a plain pak, which holds no packages to copy",
            from.stem
        ));
    }
    let target = iostore_target(into)?;
    let source = from.part("utoc");
    let packages = mod_packages(&source)?;
    let filters: Vec<PathFilter> = options
        .only
        .iter()
        .map(|text| PathFilter::new(Some(text)))
        .collect();
    let left: HashSet<FPackageId> = packages
        .iter()
        .filter(|package| {
            !filters.is_empty() && !filters.iter().any(|filter| filter.matches(&package.entry))
        })
        .map(|package| package.id)
        .collect();
    if packages.is_empty() {
        return Err(format!(
            "{} holds no package to copy, only the files in its pak",
            from.stem
        ));
    }
    if left.len() == packages.len() {
        return Err(format!(
            "--filter picks nothing in {}: it holds {} package(s)",
            from.stem,
            packages.len()
        ));
    }
    let held: HashSet<FPackageId> = target.iter().map(|package| package.id).collect();
    let mut report = IntoReport::new(from.shown(), into);
    for package in packages.iter().filter(|p| !left.contains(&p.id)) {
        report.copied.push(package.shown().to_string());
        if held.contains(&package.id) {
            report.replaced.push(package.shown().to_string());
        }
    }
    // What `into` ships already is there for a copied package to import.
    let shipped: HashSet<String> = target
        .iter()
        .map(|package| package.shown().to_lowercase())
        .collect();
    report.needed = needed(&packages, &left)
        .into_iter()
        .filter(|need| !shipped.contains(&need.needs.to_lowercase()))
        .collect();
    check_needed(
        &report.needed,
        options.allow_missing,
        options.dry_run,
        &mut report.warnings,
    )?;
    if from.mounts(mods) && into.mounts(mods) {
        report.warnings.push(format!(
            "{} is still switched on and holds the same packages, so whichever mounts later wins: \
             switch one off",
            from.stem
        ));
    }
    let pak = from.part("pak");
    if pak.is_file() {
        let own: Vec<String> = crate::pak::plain::list_files(&pak)?
            .into_iter()
            .filter(|name| !crate::pak::plain::is_unsupported_entry(name))
            .collect();
        if !own.is_empty() {
            report.warnings.push(format!(
                "the {} file(s) {}'s pak carries stay behind: {}",
                own.len(),
                from.stem,
                own.join(", ")
            ));
        }
    }
    let libraries: Vec<String> = list_utoc_paths(&source)?
        .into_iter()
        .filter(|path| crate::pak::shader_library::library_name(path).is_some())
        .collect();
    if !libraries.is_empty() {
        report.warnings.push(format!(
            "the shader libraries {} holds stay behind, so a copied material finds its shaders \
             only while {} is switched on: {}",
            from.stem,
            from.stem,
            libraries.join(", ")
        ));
    }
    if options.dry_run {
        return Ok(report);
    }
    let store = open_utoc(&source.to_string_lossy())?;
    let staged = iostore_out::stage_bringing_into_iostore(
        &into.part("utoc"),
        &iostore_out::PackagesFrom {
            store: &*store,
            packages: packages
                .iter()
                .map(|package| package.id)
                .filter(|id| !left.contains(id))
                .collect(),
        },
        io,
    )?
    .ok_or("nothing to copy")?;
    drop(store);
    crate::pak::containers::drop_cached_store();
    staged.commit()?;
    report.written = true;
    Ok(report)
}

/// Packs the loose packages under `input` into the IoStore mod `into`, in one rewrite that keeps
/// everything else it holds, each over `into`'s own copy of it. Files that are not packages are
/// named rather than added, since they belong in the mod's pak.
pub fn pack_into_mod(
    input: &Path,
    into: &ModFiles,
    io: &IoStoreOptions,
    dry_run: bool,
) -> Result<IntoReport, String> {
    let target = iostore_target(into)?;
    let (source, _unpacked) = pack_source(input)?;
    let folder = crate::pak::pack::LooseFolder::read(&source)?;
    folder.refuse_strays()?;
    let entries = folder.packages;
    if entries.is_empty() {
        return Err(format!(
            "{} holds no .uasset or .umap with its .uexp beside it",
            input.display()
        ));
    }
    let held: HashSet<FPackageId> = target.iter().map(|package| package.id).collect();
    let mut report = IntoReport::new(input.display().to_string(), into);
    for entry in &entries {
        let shown = package_name_of_entry(entry).unwrap_or_else(|| entry.clone());
        if held.contains(&FPackageId::from_name(&shown)) {
            report.replaced.push(shown.clone());
        }
        report.copied.push(shown);
    }
    let (shaders, loose): (Vec<String>, Vec<String>) = folder
        .others
        .iter()
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .partition(|path| crate::pak::shader_library::library_of(path).is_some());
    if !loose.is_empty() {
        report.warnings.push(format!(
            "{} file(s) that are not packages are left out, since they belong in a pak: {}",
            loose.len(),
            loose.join(", ")
        ));
    }
    if !shaders.is_empty() {
        report.warnings.push(format!(
            "--into adds packages only, so the shader archive files are left out: {}; pack the \
             mod anew to give it their libraries",
            shaders.join(", ")
        ));
    }
    if dry_run {
        return Ok(report);
    }
    let mut failed = None;
    let staged = iostore_out::stage_batch_into_iostore(
        &into.part("utoc"),
        entries.len(),
        |at| match crate::pak::pack::read_loose_package(&source, &entries[at]) {
            Ok(package) => Some(package),
            Err(e) => {
                failed.get_or_insert(e);
                None
            }
        },
        io,
    )?;
    if let Some(e) = failed {
        return Err(e);
    }
    let staged = staged.ok_or("nothing to pack")?;
    crate::pak::containers::drop_cached_store();
    staged.commit()?;
    report.written = true;
    Ok(report)
}

/// Takes away the files of `dest`'s name that the write did not replace, such as a switched-off
/// copy beside the new one.
fn remove_others(dest: &ModFiles, taken: &[PathBuf], warnings: &mut Vec<String>) {
    let written = dest.files();
    for stale in taken.iter().filter(|path| !written.contains(path)) {
        if let Err(e) = std::fs::remove_file(stale) {
            warnings.push(format!("{} could not be removed: {e}", stale.display()));
        }
    }
}

#[derive(Debug, Serialize)]
pub struct RenameReport {
    pub from: String,
    pub to: String,
    /// Rebuilt under the new name, rather than only its files renamed.
    pub rebuilt: bool,
    pub warnings: Vec<String>,
}

/// Renames the mod `from` to `to` where it is, keeping it switched on or off. An IoStore mod is
/// rebuilt under the new name, which its container's id comes from, before the old one goes; an
/// old file that will not move takes the new copy away again, so both never mount.
pub fn rename_mod(from: &ModFiles, to: &str, io: &IoStoreOptions) -> Result<RenameReport, String> {
    let (dest, report) = rename_plan(from, to)?;
    if !report.rebuilt {
        let moves: Vec<(PathBuf, PathBuf)> = PARTS
            .iter()
            .map(|extension| (from.part(extension), dest.part(extension)))
            .filter(|(old, _)| old.is_file())
            .collect();
        crate::mods::rename_all(&moves)?;
        return Ok(report);
    }

    rebuild(&from.part("utoc"), &dest, &HashSet::new(), io)?;
    let aside: Vec<(PathBuf, PathBuf)> = from
        .files()
        .into_iter()
        .map(|old| {
            let mut backup = old.clone().into_os_string();
            backup.push(".bak");
            (old, PathBuf::from(backup))
        })
        .collect();
    if let Err(e) = crate::mods::rename_all(&aside) {
        for new in dest.files() {
            let _ = std::fs::remove_file(new);
        }
        return Err(e);
    }
    for (_, backup) in &aside {
        let _ = std::fs::remove_file(backup);
    }
    Ok(report)
}

/// What renaming the mod `from` to `to` would do, refused where the rename would be, and nothing
/// written. `rebuilt` says whether its container would be rebuilt under the new name.
pub fn plan_rename(from: &ModFiles, to: &str) -> Result<RenameReport, String> {
    Ok(rename_plan(from, to)?.1)
}

fn rename_plan(from: &ModFiles, to: &str) -> Result<(ModFiles, RenameReport), String> {
    let mut dest = destination(to, &from.dir, false, None)?;
    dest.disabled = from.disabled;
    if dest.stem == from.stem {
        return Err(format!("{} already has that name", from.stem));
    }
    // The id is the name's hash, ignoring case, so a change of case alone keeps it.
    let same_id = dest.stem.eq_ignore_ascii_case(&from.stem);
    if !same_id && let Some(taken) = dest.any_files().first() {
        return Err(format!("{} is already there", taken.display()));
    }
    let report = RenameReport {
        from: from.shown(),
        to: dest.shown(),
        rebuilt: from.is_iostore() && !same_id,
        warnings: priority_note(&dest).into_iter().collect(),
    };
    Ok((dest, report))
}

#[derive(Debug, Clone, Default)]
pub struct RemoveOptions {
    /// Packages to take out, by container path or package name.
    pub entries: Vec<String>,
    /// Packages to take out, by any part of their path or name, ignoring case.
    pub filters: Vec<String>,
    pub allow_missing: bool,
    pub dry_run: bool,
}

#[derive(Debug, Serialize)]
pub struct RemoveReport {
    #[serde(rename = "mod")]
    pub mod_file: String,
    pub removed: Vec<String>,
    pub kept: usize,
    pub needed: Vec<Needed>,
    pub warnings: Vec<String>,
    pub written: bool,
}

/// Takes the packages `options` names out of the mod, in one rewrite. Refused while a package it
/// keeps imports one it takes out, unless `allow_missing`.
pub fn remove_packages(
    files: &ModFiles,
    options: &RemoveOptions,
    io: &IoStoreOptions,
) -> Result<RemoveReport, String> {
    if !files.is_iostore() {
        return Err(format!(
            "{} is a plain pak, which holds no packages the game loads",
            files.stem
        ));
    }
    let source = files.part("utoc");
    let packages = mod_packages(&source)?;
    let filters: Vec<PathFilter> = options
        .filters
        .iter()
        .map(|text| PathFilter::new(Some(text)))
        .collect();
    for entry in &options.entries {
        if !packages.iter().any(|package| is_entry(package, entry)) {
            return Err(format!("{} holds no package {entry}", files.stem));
        }
    }
    let dropped: HashSet<FPackageId> = packages
        .iter()
        .filter(|package| {
            options.entries.iter().any(|entry| is_entry(package, entry))
                || filters.iter().any(|filter| filter.matches(&package.entry))
        })
        .map(|package| package.id)
        .collect();
    if dropped.is_empty() {
        return Err(format!("nothing in {} matches", files.stem));
    }
    if dropped.len() == packages.len() {
        return Err(format!(
            "that takes out every package {} holds: delete the mod instead",
            files.stem
        ));
    }
    let needed = needed(&packages, &dropped);
    let mut warnings = Vec::new();
    check_needed(
        &needed,
        options.allow_missing,
        options.dry_run,
        &mut warnings,
    )?;
    let mut report = RemoveReport {
        mod_file: files.shown(),
        removed: packages
            .iter()
            .filter(|package| dropped.contains(&package.id))
            .map(|package| package.shown().to_string())
            .collect(),
        kept: packages.len() - dropped.len(),
        needed,
        warnings,
        written: false,
    };
    if options.dry_run {
        return Ok(report);
    }
    rebuild(&source, files, &dropped, io)?;
    report.written = true;
    Ok(report)
}

/// Switches the mod on or off, all its files as one step. Says whether anything changed, since
/// a mod already in that state is left as it is.
pub fn set_enabled(files: &ModFiles, enabled: bool) -> Result<(ModFiles, bool), String> {
    switch(files, enabled, false)
}

/// What switching a mod on or off would make it, and whether that would change anything, refused
/// as the switch would be, with nothing renamed.
pub fn plan_set_enabled(files: &ModFiles, enabled: bool) -> Result<(ModFiles, bool), String> {
    switch(files, enabled, true)
}

fn switch(files: &ModFiles, enabled: bool, dry_run: bool) -> Result<(ModFiles, bool), String> {
    if files.disabled != enabled {
        return Ok((files.clone(), false));
    }
    let moves: Vec<(PathBuf, PathBuf)> = PARTS
        .iter()
        .map(|extension| (files.part(extension), files.part_as(extension, !enabled)))
        .filter(|(from, _)| from.is_file())
        .collect();
    if let Some((_, taken)) = moves.iter().find(|(_, to)| to.exists()) {
        return Err(format!(
            "{} is already there, so switching {} {} would write over it",
            taken.display(),
            files.stem,
            if enabled { "on" } else { "off" }
        ));
    }
    if !dry_run {
        crate::mods::rename_all(&moves)?;
    }
    Ok((
        ModFiles {
            disabled: !enabled,
            ..files.clone()
        },
        true,
    ))
}

#[derive(Debug, Serialize)]
pub struct PackModReport {
    #[serde(rename = "mod")]
    pub mod_file: String,
    pub packages: usize,
    /// Files that are not packages, carried in the mod's pak.
    pub loose: Vec<String>,
    /// Shader archives written into the container as libraries.
    pub shader_libraries: Vec<String>,
    pub warnings: Vec<String>,
}

/// Packs the loose packages under `input`, of the files `take` takes, into the IoStore mod `to`, a
/// name beside `mods` or a path, built aside and swapped in so a failure leaves whatever was
/// there. `game_libraries` names the game's shader libraries, which a shader archive in `input` is
/// kept from replacing.
#[allow(clippy::too_many_arguments)]
pub fn pack_mod(
    input: &Path,
    take: crate::pak::pack::Take,
    to: &str,
    mods: &Path,
    replace: bool,
    io: &IoStoreOptions,
    game_libraries: crate::pak::shader_library::GameLibraries,
    dry_run: bool,
) -> Result<PackModReport, String> {
    let dest = destination(to, mods, true, None)?;
    let taken = dest.any_files();
    if !taken.is_empty() && !replace {
        return Err(format!(
            "{} is already there: pass --overwrite to write over it",
            taken[0].display()
        ));
    }
    let (source, _unpacked) = pack_source(input)?;
    let staging = pack_staging(&dest, dry_run)?;
    let packed = crate::pak::pack::pack_folder(
        &source,
        take,
        &staging.0.join(format!("{}.utoc", dest.stem)),
        io,
        &Default::default(),
        game_libraries,
        rayon::current_num_threads(),
        &std::sync::atomic::AtomicBool::new(false),
        &|_, _| {},
    )?;
    let mut warnings: Vec<String> = priority_note(&dest).into_iter().collect();
    warnings.extend(packed.warnings);
    if !dry_run {
        iostore_out::swap_into_place(&dest.dir, &dest.stem, &staging.0, &PARTS)?;
        remove_others(&dest, &taken, &mut warnings);
    }
    Ok(PackModReport {
        mod_file: dest.shown(),
        packages: packed.packages,
        loose: packed.loose,
        shader_libraries: packed.shader_libraries,
        warnings,
    })
}

/// Where a pack is built before it goes in: beside the mod, or for a dry run somewhere it never
/// will, so trying one leaves the destination as it was.
fn pack_staging(dest: &ModFiles, dry_run: bool) -> Result<Scratch, String> {
    if dry_run {
        return fresh_dir(std::env::temp_dir().join(format!(
            "rivals-pack-{}-{}",
            dest.stem,
            std::process::id()
        )));
    }
    std::fs::create_dir_all(&dest.dir)
        .map_err(|e| format!("Could not create {}: {e}", dest.dir.display()))?;
    fresh_dir(dest.dir.join(format!(".{}_pack", dest.stem)))
}

/// Packs every file under `input` into the plain pak mod `to`, a name beside `mods` or a path,
/// built aside and swapped in so a failure leaves whatever was there. A plain pak only delivers
/// loose files, so packages the folder holds are named: the game loads none from one.
pub fn pack_plain_mod(
    input: &Path,
    to: &str,
    mods: &Path,
    replace: bool,
    options: &PakOptions,
    dry_run: bool,
) -> Result<PackModReport, String> {
    let dest = destination(to, mods, true, None)?;
    let taken = dest.any_files();
    if !taken.is_empty() && !replace {
        return Err(format!(
            "{} is already there: pass --overwrite to write over it",
            taken[0].display()
        ));
    }
    let (source, _unpacked) = pack_source(input)?;
    let staging = pack_staging(&dest, dry_run)?;
    let loose = crate::pak::plain::pack_dir(
        &source,
        &staging.0.join(format!("{}.pak", dest.stem)),
        options,
    )?;
    // Only the pak is staged, so a container the name had before is set aside and not restored.
    if !dry_run {
        iostore_out::swap_into_place(&dest.dir, &dest.stem, &staging.0, &PARTS)?;
    }
    let mut warnings: Vec<String> = priority_note(&dest).into_iter().collect();
    let packages = loose
        .iter()
        .filter(|path| {
            let lower = path.to_ascii_lowercase();
            lower.ends_with(".uasset") || lower.ends_with(".umap")
        })
        .count();
    if packages > 0 {
        warnings.push(format!(
            "the folder holds {packages} package(s), which the game does not load from a plain \
             pak; pack it with --format iostore to put them in a container"
        ));
    }
    if !dry_run {
        remove_others(&dest, &taken, &mut warnings);
    }
    Ok(PackModReport {
        mod_file: dest.shown(),
        packages: 0,
        loose,
        shader_libraries: Vec::new(),
        warnings,
    })
}

/// How the game comes to load a package of a mod.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reach {
    /// The game ships it, so it loads the mod's copy wherever it would load its own.
    Override,
    /// Something reached imports it, so it loads with that.
    Imported,
    /// Something reached names its path without importing it, so it loads only if that asks
    /// for it.
    Named,
    /// Nothing the game loads leads to it.
    Unreached,
}

#[derive(Debug, Serialize)]
pub struct PackageDeps {
    pub package: String,
    pub reach: Reach,
    /// The mod's packages it imports.
    pub imports: Vec<String>,
    /// How many packages it imports from the game.
    pub game_imports: usize,
    /// Packages it imports that neither the mod nor the game has, which another mod would have
    /// to provide.
    pub missing: Vec<String>,
    /// The mod's packages it names by path without importing them.
    pub names: Vec<String>,
    /// The mod's packages that import it.
    pub imported_by: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ModDeps {
    #[serde(rename = "mod")]
    pub mod_file: String,
    pub packages: Vec<PackageDeps>,
    pub overrides: usize,
    pub unreached: Vec<String>,
    pub missing: usize,
}

/// For each package, the mod's packages it names by path without importing them.
fn named_links(packages: &[ModPackage]) -> Vec<Vec<usize>> {
    let by_name: HashMap<String, usize> = packages
        .iter()
        .enumerate()
        .filter_map(|(index, package)| Some((package.name.as_deref()?.to_lowercase(), index)))
        .collect();
    packages
        .iter()
        .map(|package| {
            package
                .mentions
                .iter()
                .filter_map(|mention| by_name.get(&mention.to_lowercase()).copied())
                .filter(|index| !package.imports.contains(&packages[*index].id))
                .collect()
        })
        .collect()
}

/// How the game reaches each package, from the ones that override its own: what they import
/// first, since that loads for certain, then what anything reached names, and what that imports.
fn reach_of(packages: &[ModPackage], named: &[Vec<usize>], overrides: &[bool]) -> Vec<Reach> {
    let at: HashMap<FPackageId, usize> = packages
        .iter()
        .enumerate()
        .map(|(index, package)| (package.id, index))
        .collect();
    let mut reach: Vec<Reach> = overrides
        .iter()
        .map(|&shipped| {
            if shipped {
                Reach::Override
            } else {
                Reach::Unreached
            }
        })
        .collect();
    for soft in [false, true] {
        let mut queue: Vec<usize> = (0..packages.len())
            .filter(|index| reach[*index] != Reach::Unreached)
            .collect();
        while let Some(index) = queue.pop() {
            let kind = if soft || reach[index] == Reach::Named {
                Reach::Named
            } else {
                Reach::Imported
            };
            let mut next: Vec<(usize, Reach)> = packages[index]
                .imports
                .iter()
                .filter_map(|id| at.get(id).map(|target| (*target, kind)))
                .collect();
            if soft {
                next.extend(named[index].iter().map(|&target| (target, Reach::Named)));
            }
            for (target, how) in next {
                if reach[target] == Reach::Unreached {
                    reach[target] = how;
                    queue.push(target);
                }
            }
        }
    }
    reach
}

/// What each package of the mod imports and names, what imports it, and how the game reaches it:
/// from the packages the mod overrides, through what they import, then what anything reached
/// names by path.
pub fn mod_deps(game_root: &str, files: &ModFiles) -> Result<ModDeps, String> {
    if !files.is_iostore() {
        return Err(format!(
            "{} is a plain pak, which holds no packages the game loads",
            files.stem
        ));
    }
    let source = files.part("utoc");
    let packages = mod_packages(&source)?;
    let game = crate::pak::containers::open_base_game_paks(&crate::paths::paks_dir(game_root), "")?;
    let in_game = |id: FPackageId| {
        game.chunk_path(FIoChunkId::from_package_id(
            id,
            0,
            EIoChunkType::ExportBundleData,
        ))
        .is_some()
    };
    let at: HashMap<FPackageId, usize> = packages
        .iter()
        .enumerate()
        .map(|(index, package)| (package.id, index))
        .collect();
    let named = named_links(&packages);
    let overrides: Vec<bool> = packages.iter().map(|p| in_game(p.id)).collect();
    let reach = reach_of(&packages, &named, &overrides);

    let shown = |index: usize| packages[index].shown().to_string();
    let mut out = ModDeps {
        mod_file: files.shown(),
        packages: Vec::new(),
        overrides: reach.iter().filter(|r| **r == Reach::Override).count(),
        unreached: Vec::new(),
        missing: 0,
    };
    for (index, package) in packages.iter().enumerate() {
        let mut deps = PackageDeps {
            package: shown(index),
            reach: reach[index],
            imports: Vec::new(),
            game_imports: 0,
            missing: Vec::new(),
            names: named[index].iter().copied().map(shown).collect(),
            imported_by: packages
                .iter()
                .enumerate()
                .filter(|(_, other)| other.imports.contains(&package.id))
                .map(|(other, _)| shown(other))
                .collect(),
        };
        for id in &package.imports {
            if let Some(target) = at.get(id) {
                deps.imports.push(shown(*target));
            } else if in_game(*id) {
                deps.game_imports += 1;
            } else {
                let name = package
                    .import_names
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| format!("the package with id {:016x}", id.0));
                deps.missing.push(name);
            }
        }
        // The order a header lists imports in is the cook's, which a repack need not keep.
        deps.imports.sort();
        deps.missing.sort();
        out.missing += deps.missing.len();
        if deps.reach == Reach::Unreached {
            out.unreached.push(deps.package.clone());
        }
        out.packages.push(deps);
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rivals-mod-ops-{tag}-{stamp}"));
        std::fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    /// An IoStore mod holding no package, its INI in its pak, copies whole: the container with
    /// nothing in it and the pak beside it. Copying its packages into another mod is refused.
    #[test]
    fn a_mod_without_packages_copies_whole() {
        let dir = scratch("no-packages");
        let input = dir.join("in");
        std::fs::create_dir_all(input.join("Marvel/Config")).unwrap();
        std::fs::write(input.join("Marvel/Config/DefaultGame.ini"), b"[A]\nB=1\n").unwrap();
        let zlib = IoStoreOptions {
            compression: crate::pak::profile::PackCompression::Zlib,
            ..Default::default()
        };
        let mods = dir.join("~mods");
        let source = mods.join("Config_9999999_P.utoc");
        crate::pak::pack::pack_folder(
            &input,
            crate::pak::pack::Take::Everything,
            &source,
            &zlib,
            &Default::default(),
            &|| Ok(Default::default()),
            1,
            &std::sync::atomic::AtomicBool::new(false),
            &|_, _| {},
        )
        .expect("pack");
        let from = ModFiles::of(&source).unwrap();
        let options = CopyOptions {
            only: Vec::new(),
            allow_missing: false,
            replace: false,
            dry_run: false,
        };
        copy_mod(&from, "Copy_9999999_P", &mods, &options, &zlib).expect("copy");
        let copy = ModFiles::of(&mods.join("Copy_9999999_P.utoc")).unwrap();
        assert!(mod_packages(&copy.part("utoc")).unwrap().is_empty());
        assert_eq!(
            crate::pak::plain::list_files(&copy.part("pak")).unwrap(),
            ["Marvel/Config/DefaultGame.ini"]
        );
        let refused = copy_into_mod(&from, &copy, &mods, &options, &zlib).unwrap_err();
        assert!(refused.contains("holds no package to copy"), "{refused}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A plain pak packs straight from its file, mounted wherever it was, without the entries the
    /// game does not support; the pak beside a container is refused, being part of a mod.
    #[test]
    fn a_plain_pak_packs_straight_from_its_file() {
        let dir = scratch("from-pak");
        let source = dir.join("Old_9999999_P.pak.disabled");
        crate::pak::plain::mounted_pak(
            &source,
            "../../../Marvel/",
            &[
                ("Config/DefaultGame.ini", b"[A]\nB=1\n"),
                ("chunknames", b"x"),
            ],
        );
        let zlib = PakOptions {
            compression: crate::pak::profile::PackCompression::Zlib,
            oodle_level: None,
        };
        let to = dir.join("out").join("New_9999999_P");
        let report = pack_plain_mod(
            &source,
            &to.to_string_lossy(),
            Path::new(""),
            false,
            &zlib,
            false,
        )
        .expect("pack");
        assert_eq!(report.loose, ["Marvel/Config/DefaultGame.ini"]);
        assert_eq!(
            crate::pak::plain::list_files(&to.with_extension("pak")).unwrap(),
            ["Marvel/Config/DefaultGame.ini"]
        );

        let beside = dir.join("Box_9999999_P.pak");
        crate::pak::plain::write_files(&beside, Vec::new(), &zlib).unwrap();
        std::fs::write(beside.with_extension("utoc"), b"toc").unwrap();
        let refused = pack_plain_mod(
            &beside,
            &dir.join("out").join("Other_9999999_P").to_string_lossy(),
            Path::new(""),
            false,
            &zlib,
            false,
        )
        .unwrap_err();
        assert!(
            refused.contains("beside the IoStore mod Box_9999999_P"),
            "{refused}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn package(name: &str, imports: &[&str], mentions: &[&str]) -> ModPackage {
        ModPackage {
            id: FPackageId::from_name(name),
            entry: format!(
                "Marvel/Content/{}.uasset",
                name.trim_start_matches("/Game/")
            ),
            name: Some(name.to_string()),
            imports: imports.iter().map(|i| FPackageId::from_name(i)).collect(),
            mentions: mentions.iter().map(|m| m.to_string()).collect(),
            import_names: HashMap::new(),
        }
    }

    #[test]
    fn a_mod_is_found_from_any_of_its_files_on_or_off() {
        let dir = scratch("of");
        std::fs::write(dir.join("Thing_9999999_P.pak.disabled"), b"").unwrap();
        std::fs::write(dir.join("Thing_9999999_P.utoc.disabled"), b"").unwrap();
        let files = ModFiles::of(&dir.join("Thing_9999999_P.utoc.disabled")).expect("found");
        assert_eq!(files.stem, "Thing_9999999_P");
        assert!(files.disabled);
        assert!(files.is_iostore());
        assert_eq!(files.files().len(), 2);
        assert!(ModFiles::of(&dir.join("Thing_9999999_P.pak")).is_err());
        assert!(ModFiles::of(&dir.join("Thing_9999999_P.txt")).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn switching_a_mod_off_and_on_moves_every_file_and_refuses_to_write_over_one() {
        let dir = scratch("enable");
        for extension in PARTS {
            std::fs::write(dir.join(format!("Mod_P.{extension}")), extension).unwrap();
        }
        let on = ModFiles::of(&dir.join("Mod_P.pak")).unwrap();
        let (off, changed) = set_enabled(&on, false).expect("off");
        assert!(changed);
        assert!(off.disabled);
        assert_eq!(off.files().len(), 3);
        assert!(on.files().is_empty());
        let (_, again) = set_enabled(&off, false).expect("already off");
        assert!(!again);

        std::fs::write(dir.join("Mod_P.utoc"), b"another").unwrap();
        assert!(
            set_enabled(&off, true).is_err(),
            "would write over Mod_P.utoc"
        );
        assert_eq!(off.files().len(), 3, "nothing moved");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_pak_only_mod_is_renamed_as_files_keeping_its_state() {
        let dir = scratch("rename");
        std::fs::write(dir.join("Old_P.pak.disabled"), b"pak").unwrap();
        let old = ModFiles::of(&dir.join("Old_P.pak.disabled")).unwrap();
        let report = rename_mod(&old, "New_P.pak", &IoStoreOptions::default()).expect("rename");
        assert!(!report.rebuilt);
        assert!(dir.join("New_P.pak.disabled").is_file());
        assert!(!dir.join("Old_P.pak.disabled").exists());
        assert!(rename_mod(&old, "Elsewhere/New_P", &IoStoreOptions::default()).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_kept_package_importing_a_dropped_one_is_refused_and_one_naming_it_is_listed() {
        let packages = vec![
            package("/Game/Mod/Hub", &["/Game/Mod/Swapper"], &[]),
            package("/Game/Mod/Loader", &[], &["/Game/Mod/Swapper"]),
            package("/Game/Mod/Swapper", &["/Game/Mod/Loader"], &[]),
        ];
        let dropped = HashSet::from([FPackageId::from_name("/Game/Mod/Swapper")]);
        let needs = needed(&packages, &dropped);
        assert_eq!(needs.len(), 2);
        assert!(needs[0].imported);
        assert_eq!(needs[0].package, "/Game/Mod/Hub");
        assert!(!needs[1].imported);
        assert_eq!(needs[1].package, "/Game/Mod/Loader");
        let mut warnings = Vec::new();
        assert!(check_needed(&needs, false, false, &mut warnings).is_err());
        assert!(check_needed(&needs, true, false, &mut warnings).is_ok());
        assert!(
            check_needed(&needs[1..], false, false, &mut warnings).is_ok(),
            "a mention alone"
        );
        assert!(warnings.is_empty());
        assert!(check_needed(&needs, false, true, &mut warnings).is_ok());
        assert_eq!(warnings.len(), 1, "a dry run is told");
    }

    #[test]
    fn the_game_reaches_what_an_override_imports_then_what_it_names() {
        let packages = vec![
            package("/Game/UI/Hub", &["/Game/Mod/Menu"], &[]),
            package("/Game/Mod/Menu", &["/Game/Mod/Icon"], &["/Game/Mod/Loader"]),
            package("/Game/Mod/Icon", &[], &[]),
            package("/Game/Mod/Loader", &["/Game/Mod/Log"], &[]),
            package("/Game/Mod/Log", &[], &[]),
            package("/Game/Mod/Orphan", &["/Game/Mod/Icon"], &[]),
        ];
        let named = named_links(&packages);
        assert_eq!(named[1], vec![3]);
        let reach = reach_of(
            &packages,
            &named,
            &[true, false, false, false, false, false],
        );
        assert_eq!(
            reach,
            vec![
                Reach::Override,
                Reach::Imported,
                Reach::Imported,
                Reach::Named,
                Reach::Named,
                Reach::Unreached,
            ]
        );
    }

    #[test]
    fn an_entry_is_named_by_its_path_or_its_package() {
        let hub = package("/Game/Mod/Hub", &[], &[]);
        assert!(is_entry(&hub, "/Game/Mod/Hub"));
        assert!(is_entry(&hub, "Marvel/Content/Mod/Hub.uasset"));
        assert!(is_entry(&hub, "../../../Marvel/Content/Mod/Hub"));
        assert!(!is_entry(&hub, "/Game/Mod/Hub2"));
    }

    #[test]
    fn a_destination_goes_home_unless_it_names_a_folder() {
        let home = PathBuf::from("mods");
        let to = destination("To_P.utoc.disabled", &home, false, None).unwrap();
        assert_eq!(to.dir, home);
        assert_eq!(to.stem, "To_P");
        assert!(!to.disabled);
        let elsewhere = destination("out/To_P", &home, true, None).unwrap();
        assert_eq!(elsewhere.dir, std::path::absolute("out").unwrap());
        assert!(destination("out/To_P", &home, false, None).is_err());
    }

    /// A folder that exists, or a path ending in a separator, takes a copy under the mod's own
    /// name, as `cp` puts a file into a folder, while a bare name still goes home. Where nothing
    /// names the mod, as for a pack, the folder is refused.
    #[test]
    fn a_folder_takes_the_mod_under_its_own_name() {
        let dir = scratch("into-folder");
        let home = PathBuf::from("mods");
        let folder = dir.join("out");
        std::fs::create_dir_all(&folder).unwrap();
        for given in [
            folder.display().to_string(),
            format!("{}{}", folder.display(), std::path::MAIN_SEPARATOR),
        ] {
            let into = destination(&given, &home, true, Some("From_P")).unwrap();
            assert_eq!(into.dir, std::path::absolute(&folder).unwrap(), "{given}");
            assert_eq!(into.stem, "From_P", "{given}");
            assert!(destination(&given, &home, true, None).is_err(), "{given}");
        }
        let named = destination(
            &folder.join("To_P").display().to_string(),
            &home,
            true,
            Some("From_P"),
        )
        .unwrap();
        assert_eq!(
            (named.dir, named.stem.as_str()),
            (std::path::absolute(&folder).unwrap(), "To_P")
        );
        let missing = format!(
            "{}{}",
            dir.join("missing").display(),
            std::path::MAIN_SEPARATOR
        );
        assert!(
            destination(&missing, &home, true, Some("From_P"))
                .unwrap_err()
                .contains("does not exist")
        );
        assert!(destination(&folder.display().to_string(), &home, false, Some("From_P")).is_err());
        assert_eq!(
            destination("out", &home, true, Some("From_P")).unwrap().dir,
            home
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    const SHAKE: &str = "Marvel/Content/Marvel/AbilitySystem/1011/101111/CameraShake_101111.uasset";
    const SHAKE_HIT: &str =
        "Marvel/Content/Marvel/AbilitySystem/1011/101111/CameraShake_101111_Hit.uasset";
    const TABLE: &str = "Marvel/Content/Marvel/Data/DataTable/MarvelHeroTable.uasset";

    /// The game root to read probe packages from, when the game data and Oodle are both there.
    fn game_data() -> Option<String> {
        let root = std::env::var("RIVALS_GAME_ROOT").ok()?;
        std::env::var_os("OODLE_LIB_PATH").map(|_| root)
    }

    /// Writes a Zlib container at `utoc` holding the game's own copies of `entries`.
    fn write_probe(root: &str, utoc: &Path, entries: &[&str]) {
        let source = format!(
            "{}/MarvelGame/Marvel/Content/Paks/pakchunk0-Windows.utoc",
            root.replace('\\', "/")
        );
        iostore_out::write_batch_into_iostore(
            utoc,
            entries.len(),
            |index| {
                let loaded = crate::asset::load_bundle(
                    root,
                    &source,
                    entries[index],
                    crate::asset::AssetSource::Utoc,
                )
                .ok()?;
                Some(iostore_out::PackageBytes {
                    entry: entries[index].to_string(),
                    asset: loaded.asset_file_buffer,
                    exports: loaded.exports_file_buffer,
                    bulk: loaded.bulk_data_buffer,
                    optional_bulk: None,
                    memory_mapped_bulk: None,
                    shader_map_hashes: Vec::new(),
                })
            },
            &zlib(),
        )
        .expect("write the probe");
    }

    fn zlib() -> IoStoreOptions {
        IoStoreOptions {
            compression: crate::pak::profile::PackCompression::Zlib,
            ..Default::default()
        }
    }

    /// Each package's export bundle as the container stores it, and its store entry, by package.
    fn stored(utoc: &Path) -> HashMap<FPackageId, (Vec<u8>, String)> {
        let store = open_utoc(&utoc.to_string_lossy()).expect("open");
        store
            .packages()
            .map(|package| {
                let id = package.id();
                let chunk = FIoChunkId::from_package_id(id, 0, EIoChunkType::ExportBundleData);
                let entry = format!("{:?}", store.package_store_entry(id));
                (id, (store.read(chunk).expect("read"), entry))
            })
            .collect()
    }

    /// Packages copied into another mod go in as the first stores them, chunk and store entry, over
    /// the second's own copy, and the second keeps its other packages and its pak's files. A
    /// switched-off mod takes them and stays switched off, and so does a folder packed into one.
    #[test]
    fn packages_are_copied_or_packed_into_a_mod_as_they_are_stored() {
        use std::sync::atomic::AtomicBool;
        let Some(root) = game_data() else {
            return;
        };
        let dir = scratch("into");
        let from_utoc = dir.join("IntoFrom_P.utoc");
        let into_utoc = dir.join("IntoTarget_P.utoc");
        write_probe(&root, &from_utoc, &[SHAKE, TABLE]);
        write_probe(&root, &into_utoc, &[SHAKE_HIT, TABLE]);
        crate::pak::plain::write_files(
            &into_utoc.with_extension("pak"),
            vec![
                ("chunknames".into(), Vec::new()),
                ("readme.txt".into(), b"kept".to_vec()),
            ],
            &zlib().pak(),
        )
        .expect("the target's pak");
        let from = ModFiles::of(&from_utoc).unwrap();
        let into = ModFiles::of(&into_utoc).unwrap();
        let before = stored(&into_utoc);
        let options = CopyOptions {
            only: Vec::new(),
            allow_missing: false,
            replace: false,
            dry_run: true,
        };
        assert!(
            copy_into_mod(&from, &from, &dir, &options, &zlib()).is_err(),
            "the same mod"
        );
        let planned = copy_into_mod(&from, &into, &dir, &options, &zlib()).expect("dry run");
        assert!(!planned.written);
        let competing = |warnings: &[String]| {
            warnings
                .iter()
                .any(|warning| warning.contains("still switched on"))
        };
        assert!(competing(&planned.warnings), "both mount from {dir:?}");
        let elsewhere = dir.join("mods");
        let apart = copy_into_mod(&from, &into, &elsewhere, &options, &zlib()).expect("dry run");
        assert!(!competing(&apart.warnings), "neither is in the mods folder");
        let copy = copy_mod(&from, "IntoCopy_P", &dir, &options, &zlib()).expect("dry run");
        assert!(competing(&copy.warnings), "the copy goes beside it");
        let away = scratch("into-away");
        let copy = copy_mod(
            &from,
            &away.join("IntoCopy_P").to_string_lossy(),
            &dir,
            &options,
            &zlib(),
        )
        .expect("dry run");
        assert!(
            !competing(&copy.warnings),
            "the copy goes outside the mods folder"
        );
        let _ = std::fs::remove_dir_all(&away);
        assert_eq!(planned.copied.len(), 2);
        assert_eq!(
            planned.replaced,
            vec!["/Game/Marvel/Data/DataTable/MarvelHeroTable"]
        );
        assert_eq!(stored(&into_utoc).len(), 2, "a dry run writes nothing");

        let options = CopyOptions {
            dry_run: false,
            ..options
        };
        let report = copy_into_mod(&from, &into, &dir, &options, &zlib()).expect("copy");
        assert!(report.written);
        let theirs = stored(&from_utoc);
        let after = stored(&into_utoc);
        assert_eq!(after.len(), 3);
        for (id, copy) in &theirs {
            assert_eq!(after.get(id), Some(copy), "copied as it is stored");
        }
        let hit =
            FPackageId::from_name("/Game/Marvel/AbilitySystem/1011/101111/CameraShake_101111_Hit");
        assert_eq!(after.get(&hit), before.get(&hit), "its own package kept");
        let pak = crate::pak::plain::read_files(&into_utoc.with_extension("pak"), &|_| true)
            .expect("the target's pak");
        assert!(
            pak.iter()
                .any(|(name, bytes)| name == "readme.txt" && bytes == b"kept")
        );

        // A switched-off mod takes a folder of packages and stays switched off.
        let loose = dir.join("loose");
        crate::pak::extract::extract_legacy(
            &from_utoc,
            &root,
            &loose,
            &[],
            &AtomicBool::new(false),
            &|_, _| {},
        )
        .expect("extract");
        crate::pak::containers::drop_cached_store();
        let (off, _) = set_enabled(&into, false).expect("switch off");
        let packed = pack_into_mod(&loose, &off, &zlib(), false).expect("pack into");
        assert_eq!(packed.copied.len(), 2);
        assert_eq!(packed.replaced.len(), 2);
        assert!(
            off.part("utoc").is_file() && !into_utoc.is_file(),
            "still switched off"
        );
        assert!(!into_utoc.with_extension("pak").is_file());
        assert_eq!(stored(&off.part("utoc")).len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A switched-off mod is read where it is and under the name it has switched on: its packages
    /// list, load and extract as they do switched on, and nothing is linked beside it to read it.
    #[test]
    fn a_switched_off_mod_reads_where_it_is() {
        use std::sync::atomic::AtomicBool;
        let Some(root) = game_data() else {
            return;
        };
        let entries = [SHAKE, TABLE];
        let dir = scratch("off");
        let on = dir.join("OffProbe_P.utoc");
        write_probe(&root, &on, &entries);

        // Everything a reader sees of the mod, each extracted file with its bytes.
        let seen = |utoc: &Path, out: &Path| {
            let container = utoc.to_string_lossy().into_owned();
            let (_, listed) = crate::asset::list_packages(&root, &container).expect("list");
            let mut listed: Vec<String> = listed.into_iter().map(|(_, path)| path).collect();
            listed.sort();
            let loaded = crate::asset::load_bundle(
                &root,
                &container,
                entries[1],
                crate::asset::AssetSource::Utoc,
            )
            .expect("load")
            .exports_file_buffer;
            let extracted = crate::pak::extract::extract_legacy(
                utoc,
                &root,
                out,
                &[],
                &AtomicBool::new(false),
                &|_, _| {},
            )
            .expect("extract");
            assert!(extracted.errors.is_empty(), "{:?}", extracted.errors);
            let files: Vec<(String, Vec<u8>)> = extracted
                .extracted
                .iter()
                .map(|path| (path.clone(), std::fs::read(out.join(path)).expect("read")))
                .collect();
            let packages = mod_packages(utoc).expect("packages");
            crate::pak::containers::drop_cached_store();
            (listed, loaded, files, packages.len())
        };
        let switched_on = seen(&on, &dir.join("on"));

        let (off, _) = set_enabled(&ModFiles::of(&on).unwrap(), false).expect("switch off");
        assert!(off.disabled);
        let utoc = off.part("utoc");
        assert_eq!(
            crate::pak::containers::container_stem(&utoc).as_deref(),
            Some("OffProbe_P")
        );
        let switched_off = seen(&utoc, &dir.join("off"));
        assert_eq!(switched_off.0, switched_on.0, "the same packages listed");
        assert_eq!(switched_off.1, switched_on.1, "the same package loaded");
        assert_eq!(switched_off.2, switched_on.2, "the same files extracted");
        assert_eq!(switched_off.3, entries.len());
        assert!(
            !dir.join(".OffProbe_P_view").exists(),
            "nothing is linked to read it"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
