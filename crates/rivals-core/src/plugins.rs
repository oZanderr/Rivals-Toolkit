//! Lists, switches, installs and deletes ASI plugins and the proxy DLLs that load them in the game's Binaries/Win64 folder.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::mod_library::{BulkFailure, BulkOpResult};
use crate::paths::{binaries_dir, plugins_dir};

const DISABLED: &str = ".disabled";

/// How deep a folder or archive being installed is looked through for plugins.
const INSTALL_DEPTH: usize = 8;

/// The names an ASI loader goes by in an x64 game: every proxy Minimal ASI Loader and Ultimate ASI
/// Loader build as. The game ships none of them beside its executable.
pub const PROXY_DLL_NAMES: &[&str] = &[
    "version.dll",
    "dsound.dll",
    "dxgi.dll",
    "winmm.dll",
    "dinput8.dll",
    "wininet.dll",
    "winhttp.dll",
    "d3d9.dll",
    "d3d10.dll",
    "d3d11.dll",
    "d3d12.dll",
    "xinput1_1.dll",
    "xinput1_2.dll",
    "xinput1_3.dll",
    "xinput1_4.dll",
    "xinput9_1_0.dll",
    "xinputuap.dll",
    "binkw64.dll",
    "bink2w64.dll",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoaderKind {
    MinimalAsiLoader,
    UltimateAsiLoader,
    /// Some other proxy, such as ReShade, or a loader not recognised.
    Unknown,
}

/// A file beside a plugin or loader that carries its name, such as its `.ini` or `.log`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Companion {
    pub file_name: String,
    pub path: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Loader {
    pub file_name: String,
    pub path: String,
    /// The DLL it stands in for: its file name without `.disabled`.
    pub proxy_for: String,
    pub enabled: bool,
    pub size: u64,
    /// Seconds since the Unix epoch.
    pub modified: Option<u64>,
    pub kind: LoaderKind,
    /// Its version resource's product name and version, when it has one.
    pub description: Option<String>,
    pub companions: Vec<Companion>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plugin {
    /// Its file name without `.asi` or `.disabled`.
    pub name: String,
    pub file_name: String,
    pub path: String,
    /// The folder it is in relative to `Win64`, `/`-separated: empty beside the game, else such
    /// as `plugins`.
    pub location: String,
    pub enabled: bool,
    pub size: u64,
    /// Seconds since the Unix epoch.
    pub modified: Option<u64>,
    pub companions: Vec<Companion>,
    /// The copy of the same name beside the game, which Minimal ASI Loader loads in its place.
    pub shadowed_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginsStatus {
    pub win64_dir: String,
    pub plugins_dir: String,
    pub win64_exists: bool,
    pub plugins_dir_exists: bool,
    pub loaders: Vec<Loader>,
    pub plugins: Vec<Plugin>,
    /// The Steam launch option Proton needs to load the switched-on loaders, none on Windows.
    pub wine_launch_option: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skipped {
    pub file: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginInstall {
    /// The file, folder or archive given.
    pub source: String,
    /// Files written into `plugins`.
    pub installed: Vec<String>,
    /// A plugin's files already in `plugins`, left as they were so its settings survive an update.
    pub kept: Vec<String>,
    /// Plugins already installed, switched on or off. Nothing from the source was written; install
    /// it again with `replace` to write over them.
    pub existing: Vec<String>,
    pub skipped: Vec<Skipped>,
    pub error: Option<String>,
}

/// A file or folder as listed.
#[derive(Debug, Clone)]
struct Listed {
    name: String,
    path: PathBuf,
    is_dir: bool,
    size: u64,
    modified: Option<u64>,
}

fn list(dir: &Path) -> Vec<Listed> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut listed: Vec<Listed> = entries
        .flatten()
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            Some(Listed {
                name: entry.file_name().to_string_lossy().into_owned(),
                path: entry.path(),
                is_dir: meta.is_dir(),
                size: if meta.is_dir() { 0 } else { meta.len() },
                modified: meta
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                    .map(|since| since.as_secs()),
            })
        })
        .collect();
    listed.sort_by_key(|file| file.name.to_ascii_lowercase());
    listed
}

fn strip_suffix_ci<'a>(name: &'a str, suffix: &str) -> Option<&'a str> {
    let cut = name.len().checked_sub(suffix.len())?;
    (name.is_char_boundary(cut) && name[cut..].eq_ignore_ascii_case(suffix)).then(|| &name[..cut])
}

/// `name` without a trailing `.disabled`, and whether it had one.
fn split_disabled(name: &str) -> (&str, bool) {
    match strip_suffix_ci(name, DISABLED) {
        Some(base) if !base.is_empty() => (base, true),
        _ => (name, false),
    }
}

/// The name of a plugin's file without `.asi`, switched on or off; none for any other file.
fn plugin_stem(name: &str) -> Option<&str> {
    strip_suffix_ci(split_disabled(name).0, ".asi").filter(|stem| !stem.is_empty())
}

fn is_proxy_name(name: &str) -> bool {
    let base = split_disabled(name).0;
    PROXY_DLL_NAMES
        .iter()
        .any(|proxy| proxy.eq_ignore_ascii_case(base))
}

/// A plugin's or loader's name without its extension: `PigeonPass` for `PigeonPass.asi.disabled`,
/// `dsound` for `dsound.dll`.
fn owner_stem(name: &str) -> &str {
    let base = split_disabled(name).0;
    strip_suffix_ci(base, ".asi")
        .or_else(|| strip_suffix_ci(base, ".dll"))
        .unwrap_or(base)
}

/// Which of `stems` (lowercase) the file belongs to: the longest its name starts with followed by
/// a dot, so `Foo.Bar.ini` goes with `Foo.Bar.asi` rather than `Foo.asi`.
fn owner_of(file: &Listed, stems: &[String]) -> Option<usize> {
    if file.is_dir {
        return None;
    }
    let lower = file.name.to_ascii_lowercase();
    let mut best: Option<usize> = None;
    for (at, stem) in stems.iter().enumerate() {
        let claims = lower
            .strip_prefix(stem.as_str())
            .is_some_and(|rest| rest.len() > 1 && rest.starts_with('.'));
        if claims && best.is_none_or(|held| stems[held].len() < stem.len()) {
            best = Some(at);
        }
    }
    best
}

/// A folder's plugins (and with `loaders_here` its loaders), each with the files carrying its
/// name.
struct Folder<'a> {
    owners: Vec<(&'a Listed, Vec<&'a Listed>)>,
}

fn sort_folder(listed: &[Listed], loaders_here: bool) -> Folder<'_> {
    let is_owner = |file: &Listed| {
        !file.is_dir
            && (plugin_stem(&file.name).is_some() || (loaders_here && is_proxy_name(&file.name)))
    };
    let owners: Vec<&Listed> = listed.iter().filter(|file| is_owner(file)).collect();
    let stems: Vec<String> = owners
        .iter()
        .map(|file| owner_stem(&file.name).to_ascii_lowercase())
        .collect();
    let mut grouped: Vec<Vec<&Listed>> = vec![Vec::new(); owners.len()];
    for file in listed.iter().filter(|file| !is_owner(file)) {
        if let Some(at) = owner_of(file, &stems) {
            grouped[at].push(file);
        }
    }
    Folder {
        owners: owners.into_iter().zip(grouped).collect(),
    }
}

fn companions(files: &[&Listed]) -> Vec<Companion> {
    files
        .iter()
        .map(|file| Companion {
            file_name: file.name.clone(),
            path: file.path.display().to_string(),
            size: file.size,
        })
        .collect()
}

/// Which loader a proxy DLL is, by its export directory, and its version resource's product and
/// version when it has one.
pub fn identify(bytes: &[u8]) -> (LoaderKind, Option<String>) {
    use pelite::pe64::{Pe, PeFile};

    let Ok(file) = PeFile::from_bytes(bytes) else {
        return (LoaderKind::Unknown, None);
    };
    let kind = match file.exports() {
        Ok(exports) => {
            let dll_name = exports
                .dll_name()
                .ok()
                .and_then(|name| name.to_str().ok())
                .unwrap_or_default()
                .to_ascii_lowercase();
            let marked = exports
                .by()
                .is_ok_and(|by| by.name("IsUltimateASILoader").is_ok());
            if marked || matches!(dll_name.as_str(), "ual" | "ual.dll") {
                LoaderKind::UltimateAsiLoader
            } else if matches!(
                dll_name.as_str(),
                "minimal_asi_loader.dll" | "oxiloader.dll"
            ) {
                LoaderKind::MinimalAsiLoader
            } else {
                LoaderKind::Unknown
            }
        }
        Err(_) => LoaderKind::Unknown,
    };
    (kind, version_label(file))
}

fn version_label(file: pelite::pe64::PeFile) -> Option<String> {
    use pelite::pe64::Pe;

    let info = file.resources().ok()?.version_info().ok()?;
    let lang = *info.translation().first()?;
    let value = |key: &str| {
        info.value(lang, key)
            .map(|text| {
                text.trim_matches(|c: char| c == '\0' || c.is_whitespace())
                    .to_string()
            })
            .filter(|text| !text.is_empty())
    };
    let name = value("ProductName").or_else(|| value("FileDescription"))?;
    Some(
        match value("ProductVersion").or_else(|| value("FileVersion")) {
            Some(version) => format!("{name} {version}"),
            None => name,
        },
    )
}

fn loader_of(file: &Listed, companions_of: &[&Listed]) -> Loader {
    let (base, disabled) = split_disabled(&file.name);
    let (kind, description) = fs::read(&file.path)
        .map(|bytes| identify(&bytes))
        .unwrap_or((LoaderKind::Unknown, None));
    Loader {
        file_name: file.name.clone(),
        path: file.path.display().to_string(),
        proxy_for: base.to_string(),
        enabled: !disabled,
        size: file.size,
        modified: file.modified,
        kind,
        description,
        companions: companions(companions_of),
    }
}

fn plugin_of(file: &Listed, location: &str, companions_of: &[&Listed]) -> Plugin {
    let (base, disabled) = split_disabled(&file.name);
    Plugin {
        name: owner_stem(base).to_string(),
        file_name: file.name.clone(),
        path: file.path.display().to_string(),
        location: location.to_string(),
        enabled: !disabled,
        size: file.size,
        modified: file.modified,
        companions: companions(companions_of),
        shadowed_by: None,
    }
}

fn folder_at(win64: &Path, location: &str) -> PathBuf {
    location
        .split('/')
        .filter(|part| !part.is_empty())
        .fold(win64.to_path_buf(), |path, part| path.join(part))
}

/// The loaders beside the game and the plugins they would load.
///
/// Plugins are looked for where the loaders present look: beside the game and in `plugins` for
/// Minimal ASI Loader, and with Ultimate ASI Loader also in `scripts` and one folder down in both.
pub fn scan(game_root: &str) -> PluginsStatus {
    let win64 = binaries_dir(game_root);
    let plugins = plugins_dir(game_root);

    let root_listed = list(&win64);
    let root = sort_folder(&root_listed, true);
    let mut loaders = Vec::new();
    let mut found = Vec::new();
    for (file, beside) in &root.owners {
        if plugin_stem(&file.name).is_some() {
            found.push(plugin_of(file, "", beside));
        } else {
            loaders.push(loader_of(file, beside));
        }
    }

    let ultimate = loaders
        .iter()
        .any(|loader| loader.enabled && loader.kind == LoaderKind::UltimateAsiLoader);
    let mut locations = vec!["plugins".to_string()];
    if ultimate {
        locations.push("scripts".to_string());
        for base in ["plugins", "scripts"] {
            for sub in list(&win64.join(base))
                .into_iter()
                .filter(|file| file.is_dir)
            {
                locations.push(format!("{base}/{}", sub.name));
            }
        }
    }

    for location in &locations {
        let listed = list(&folder_at(&win64, location));
        for (file, beside) in &sort_folder(&listed, false).owners {
            found.push(plugin_of(file, location, beside));
        }
    }

    // Minimal ASI Loader loads a name once, from beside the game first; Ultimate loads both.
    if !ultimate {
        let beside_game: Vec<(String, String)> = found
            .iter()
            .filter(|plugin| plugin.location.is_empty() && plugin.enabled)
            .map(|plugin| (plugin.file_name.to_ascii_lowercase(), plugin.path.clone()))
            .collect();
        for plugin in found
            .iter_mut()
            .filter(|plugin| !plugin.location.is_empty() && plugin.enabled)
        {
            let name = plugin.file_name.to_ascii_lowercase();
            plugin.shadowed_by = beside_game
                .iter()
                .find(|(other, _)| *other == name)
                .map(|(_, path)| path.clone());
        }
    }

    PluginsStatus {
        win64_dir: win64.display().to_string(),
        plugins_dir: plugins.display().to_string(),
        win64_exists: win64.is_dir(),
        plugins_dir_exists: plugins.is_dir(),
        wine_launch_option: wine_launch_option(&loaders),
        loaders,
        plugins: found,
    }
}

/// `WINEDLLOVERRIDES` for the switched-on loaders. Wine has its own copy of every proxy name but
/// Bink's, and loads that over the one beside the game unless told to prefer the native file.
fn wine_launch_option(loaders: &[Loader]) -> Option<String> {
    if cfg!(windows) {
        return None;
    }
    let mut names: Vec<String> = loaders
        .iter()
        .filter(|loader| loader.enabled)
        .map(|loader| owner_stem(&loader.proxy_for).to_ascii_lowercase())
        .filter(|name| !name.starts_with("bink"))
        .collect();
    names.sort();
    names.dedup();
    (!names.is_empty()).then(|| format!("WINEDLLOVERRIDES=\"{}=n,b\" %command%", names.join(",")))
}

/// The `.ini` files plugins and loaders keep their settings in: every one in `plugins`, and those
/// beside the game carrying a plugin's or loader's name, such as Ultimate ASI Loader's `dsound.ini`.
pub fn ini_files(game_root: &str) -> Vec<PathBuf> {
    let is_ini = |file: &Listed| !file.is_dir && strip_suffix_ci(&file.name, ".ini").is_some();
    let mut found: Vec<PathBuf> = list(&plugins_dir(game_root))
        .into_iter()
        .filter(is_ini)
        .map(|file| file.path)
        .collect();
    let root = list(&binaries_dir(game_root));
    for (_, beside) in sort_folder(&root, true).owners {
        found.extend(
            beside
                .into_iter()
                .filter(|file| is_ini(file))
                .map(|file| file.path.clone()),
        );
    }
    found.sort_by_key(|path| {
        path.file_name()
            .map(|name| name.to_string_lossy().to_ascii_lowercase())
    });
    found
}

fn file_name_of(path: &Path) -> Result<String, String> {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| format!("{} names no file", path.display()))
}

fn inside(base: &Path, path: &Path) -> bool {
    match (fs::canonicalize(base), fs::canonicalize(path)) {
        (Ok(base), Ok(full)) => full != base && full.starts_with(&base),
        _ => false,
    }
}

/// `path`, once it is known to be inside the game's `Win64` folder, so a path sent from the
/// frontend can never reach anything else.
pub fn contained(game_root: &str, path: &Path) -> Result<PathBuf, String> {
    let win64 = binaries_dir(game_root);
    if !inside(&win64, path) {
        return Err(format!(
            "{} is not inside {}",
            path.display(),
            win64.display()
        ));
    }
    Ok(path.to_path_buf())
}

fn is_plugin_or_loader(path: &Path, name: &str) -> bool {
    path.is_file() && (plugin_stem(name).is_some() || is_proxy_name(name))
}

/// Switches a plugin or loader on or off by adding or dropping `.disabled`, as the mods folder
/// does, which every loader skips. Says where it now is and whether anything changed; a file
/// already at the name it would take is never written over.
pub fn set_enabled(game_root: &str, path: &Path, enabled: bool) -> Result<(PathBuf, bool), String> {
    let path = contained(game_root, path)?;
    let name = file_name_of(&path)?;
    if !is_plugin_or_loader(&path, &name) {
        return Err(format!("{name} is not an ASI plugin or a loader DLL"));
    }
    let (base, disabled) = split_disabled(&name);
    if disabled != enabled {
        return Ok((path, false));
    }
    let target = path.with_file_name(if enabled {
        base.to_string()
    } else {
        format!("{name}{DISABLED}")
    });
    if target.exists() {
        return Err(format!(
            "{} is already there, so switching {base} {} would write over it",
            target.display(),
            if enabled { "on" } else { "off" }
        ));
    }
    crate::mods::rename_all(&[(path, target.clone())])?;
    Ok((target, true))
}

/// Windows refuses to delete a DLL a process has loaded with access denied rather than a sharing
/// violation, which [`crate::mods::held_open_hint`] does not cover.
fn delete_hint(error: &std::io::Error) -> &'static str {
    const ACCESS_DENIED: i32 = 5;
    if cfg!(windows)
        && error.raw_os_error() == Some(ACCESS_DENIED)
        && crate::game_status::is_game_running()
    {
        ". Marvel Rivals has it loaded: switch it off now and delete it once the game is closed."
    } else {
        crate::mods::held_open_hint(error)
    }
}

/// The files beside a plugin or loader that carry its name.
fn companions_of(path: &Path) -> Vec<PathBuf> {
    let Some(dir) = path.parent() else {
        return Vec::new();
    };
    let listed = list(dir);
    let folder = sort_folder(&listed, true);
    folder
        .owners
        .iter()
        .find(|(file, _)| file.path == path)
        .map(|(_, beside)| beside.iter().map(|file| file.path.clone()).collect())
        .unwrap_or_default()
}

/// Deletes a plugin or loader, and with `with_companions` the files beside it carrying its name.
/// Says what went.
pub fn delete(game_root: &str, path: &Path, with_companions: bool) -> Result<Vec<PathBuf>, String> {
    let path = contained(game_root, path)?;
    let name = file_name_of(&path)?;
    if !is_plugin_or_loader(&path, &name) {
        return Err(format!("{name} is not an ASI plugin or a loader DLL"));
    }
    let mut doomed = vec![path.clone()];
    if with_companions {
        doomed.extend(companions_of(&path));
    }
    let mut gone = Vec::new();
    for target in doomed {
        if let Err(error) = fs::remove_file(&target) {
            return Err(format!(
                "Could not delete {}: {error}{}",
                target.display(),
                delete_hint(&error)
            ));
        }
        gone.push(target);
    }
    Ok(gone)
}

/// [`set_enabled`] for each path, the failures collected rather than stopping at the first.
pub fn set_enabled_many(game_root: &str, paths: &[PathBuf], enabled: bool) -> BulkOpResult {
    bulk(paths, |path| {
        set_enabled(game_root, path, enabled).map(|_| ())
    })
}

/// [`delete`] for each path, the failures collected rather than stopping at the first.
pub fn delete_many(game_root: &str, paths: &[PathBuf], with_companions: bool) -> BulkOpResult {
    bulk(paths, |path| {
        delete(game_root, path, with_companions).map(|_| ())
    })
}

fn bulk(paths: &[PathBuf], mut each: impl FnMut(&Path) -> Result<(), String>) -> BulkOpResult {
    let mut result = BulkOpResult::default();
    for path in paths {
        match each(path) {
            Ok(()) => result.successes += 1,
            Err(error) => result.failures.push(BulkFailure {
                full_name: path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string()),
                error,
            }),
        }
    }
    result
}

/// A plugin found in what is being installed, with the files beside it carrying its name.
struct Incoming {
    asi: PathBuf,
    file_name: String,
    companions: Vec<PathBuf>,
}

/// What building a plugin leaves beside its `.asi`, which the game never loads: the same binary as
/// a `.dll`, its debug symbols and the linker's by-products.
const BUILD_OUTPUTS: [&str; 5] = [".dll", ".pdb", ".lib", ".exp", ".ilk"];

fn is_build_output(name: &str) -> bool {
    let base = split_disabled(name).0;
    BUILD_OUTPUTS
        .iter()
        .any(|extension| strip_suffix_ci(base, extension).is_some_and(|stem| !stem.is_empty()))
}

/// The plugin `file` and the files beside it that go in with it: its settings and data, not what
/// its build left there.
fn incoming(file: &Listed, beside: &[&Listed]) -> Incoming {
    Incoming {
        asi: file.path.clone(),
        file_name: file.name.clone(),
        companions: beside
            .iter()
            .filter(|file| !is_build_output(&file.name))
            .map(|file| file.path.clone())
            .collect(),
    }
}

/// The file a source installs from: for a `.dll` with the `.asi` of its name beside it, that
/// `.asi`, which is the plugin its build made twice.
fn plugin_file(source: &Path) -> PathBuf {
    let name = source
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if let Some(stem) = strip_suffix_ci(&name, ".dll").filter(|stem| !stem.is_empty()) {
        let asi = source.with_file_name(format!("{stem}.asi"));
        if asi.is_file() {
            return asi;
        }
    }
    source.to_path_buf()
}

/// Installs the plugins each of `sources` holds into `plugins`: an `.asi` with the files beside it
/// carrying its name, a `.dll` the same way under the `.asi` name (or the `.asi` beside it of its
/// name, which is the plugin), or every `.asi` in a folder or zip, 7z or rar archive. What a
/// plugin's build leaves beside it, its `.dll`, `.pdb` and linker files, stays behind. A plugin's
/// files already there are kept, and a source holding a plugin already installed writes nothing
/// unless `replace`. A plugin named by several of its files goes in once.
pub fn install(game_root: &str, sources: &[PathBuf], replace: bool) -> Vec<PluginInstall> {
    let plugins = plugins_dir(game_root);
    let mut seen: Vec<PathBuf> = Vec::new();
    sources
        .iter()
        .map(|source| {
            let mut outcome = PluginInstall {
                source: source.display().to_string(),
                ..PluginInstall::default()
            };
            let from = plugin_file(source);
            if from.is_file() {
                let key = from.canonicalize().unwrap_or_else(|_| from.clone());
                if seen.contains(&key) {
                    return outcome;
                }
                seen.push(key);
            }
            if let Err(error) = install_from(&plugins, &from, replace, &mut outcome) {
                outcome.error = Some(error);
            }
            outcome
        })
        .collect()
}

fn install_from(
    plugins: &Path,
    source: &Path,
    replace: bool,
    outcome: &mut PluginInstall,
) -> Result<(), String> {
    let name = file_name_of(source)?;
    let incoming = if source.is_dir() {
        gather(source, outcome)
    } else if source.is_file() && plugin_stem(&name).is_some() && !split_disabled(&name).1 {
        let dir = source.parent().unwrap_or(Path::new(""));
        let listed = list(dir);
        let folder = sort_folder(&listed, false);
        folder
            .owners
            .iter()
            .filter(|(file, _)| file.path == source)
            .map(|(file, beside)| incoming(file, beside))
            .collect()
    } else if source.is_file()
        && !split_disabled(&name).1
        && let Some(stem) = strip_suffix_ci(&name, ".dll").filter(|stem| !stem.is_empty())
    {
        if is_proxy_name(&name) {
            return Err("is an ASI loader, which is not installed with plugins".to_string());
        }
        // An ASI plugin is a DLL by another extension, so a dropped one goes in as the .asi it is.
        let dir = source.parent().unwrap_or(Path::new(""));
        let as_asi = format!("{stem}.asi");
        let mut listed = list(dir);
        for file in listed.iter_mut().filter(|file| file.path == source) {
            file.name.clone_from(&as_asi);
        }
        let folder = sort_folder(&listed, false);
        folder
            .owners
            .iter()
            .filter(|(file, _)| file.path == source)
            .map(|(file, beside)| incoming(file, beside))
            .collect()
    } else if source.is_file() && crate::mod_library::archive_kind(source).is_ok() {
        let unpacked = crate::mod_library::unpack_archive(source)?;
        return place(plugins, gather(unpacked.dir(), outcome), replace, outcome);
    } else if source.is_file() && is_build_output(&name) {
        outcome.skipped.push(Skipped {
            file: name,
            reason: "left by a plugin's build, which the game does not load".to_string(),
        });
        return Ok(());
    } else if !source.exists() {
        return Err("does not exist".to_string());
    } else {
        return Err(
            "is not an .asi or .dll plugin, a folder or a zip, 7z or rar archive".to_string(),
        );
    };
    place(plugins, incoming, replace, outcome)
}

/// Every plugin under `root`, with DLLs in it reported as skipped.
fn gather(root: &Path, outcome: &mut PluginInstall) -> Vec<Incoming> {
    let mut found: Vec<Incoming> = Vec::new();
    let dirs = walkdir::WalkDir::new(root)
        .max_depth(INSTALL_DEPTH)
        .sort_by_file_name()
        .into_iter()
        .flatten()
        .filter(|entry| entry.file_type().is_dir());
    for dir in dirs {
        let listed = list(dir.path());
        let shown = |path: &Path| {
            path.strip_prefix(root)
                .unwrap_or(path)
                .display()
                .to_string()
        };
        for file in &listed {
            if file.is_dir || strip_suffix_ci(&file.name, ".dll").is_none() {
                continue;
            }
            let reason = if is_proxy_name(&file.name) {
                "an ASI loader, which is not installed with plugins"
            } else {
                "a DLL rather than an .asi plugin"
            };
            outcome.skipped.push(Skipped {
                file: shown(&file.path),
                reason: reason.to_string(),
            });
        }
        let folder = sort_folder(&listed, false);
        for (file, beside) in &folder.owners {
            if split_disabled(&file.name).1 {
                outcome.skipped.push(Skipped {
                    file: shown(&file.path),
                    reason: "switched off".to_string(),
                });
                continue;
            }
            if found
                .iter()
                .any(|other| other.file_name.eq_ignore_ascii_case(&file.name))
            {
                outcome.skipped.push(Skipped {
                    file: shown(&file.path),
                    reason: format!("another {} comes first", file.name),
                });
                continue;
            }
            found.push(incoming(file, beside));
        }
    }
    found
}

fn copy_into(from: &Path, to: &Path) -> Result<(), String> {
    crate::mods::copy_over(from, to).map_err(|error| {
        format!(
            "Could not write {}: {error}{}",
            to.display(),
            crate::mods::held_open_hint(&error)
        )
    })
}

fn place(
    plugins: &Path,
    incoming: Vec<Incoming>,
    replace: bool,
    outcome: &mut PluginInstall,
) -> Result<(), String> {
    if incoming.is_empty() {
        return Err("holds no .asi plugin".to_string());
    }
    let twin = |file_name: &str| plugins.join(format!("{file_name}{DISABLED}"));
    let existing: Vec<String> = incoming
        .iter()
        .filter(|plugin| {
            plugins.join(&plugin.file_name).exists() || twin(&plugin.file_name).exists()
        })
        .map(|plugin| plugin.file_name.clone())
        .collect();
    if !existing.is_empty() && !replace {
        outcome.existing = existing;
        return Ok(());
    }
    fs::create_dir_all(plugins)
        .map_err(|e| format!("Could not create {}: {e}", plugins.display()))?;
    for plugin in &incoming {
        copy_into(&plugin.asi, &plugins.join(&plugin.file_name))?;
        outcome.installed.push(plugin.file_name.clone());
        let switched_off = twin(&plugin.file_name);
        if switched_off.exists() {
            fs::remove_file(&switched_off).map_err(|error| {
                format!(
                    "Could not delete {}: {error}{}",
                    switched_off.display(),
                    delete_hint(&error)
                )
            })?;
        }
        for companion in &plugin.companions {
            let name = file_name_of(companion)?;
            let to = plugins.join(&name);
            if to.exists() {
                outcome.kept.push(name);
            } else {
                copy_into(companion, &to)?;
                outcome.installed.push(name);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "rivals-plugins-{tag}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        /// A game root with an empty `Win64`.
        fn game(tag: &str) -> Self {
            let scratch = Self::new(tag);
            fs::create_dir_all(binaries_dir(scratch.root())).unwrap();
            scratch
        }

        fn root(&self) -> &str {
            self.0.to_str().unwrap()
        }

        fn win64(&self) -> PathBuf {
            binaries_dir(self.root())
        }

        fn plugins(&self) -> PathBuf {
            plugins_dir(self.root())
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn touch(path: &Path, body: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn names(plugins: &[Plugin]) -> Vec<&str> {
        plugins
            .iter()
            .map(|plugin| plugin.file_name.as_str())
            .collect()
    }

    fn companion_names(companions: &[Companion]) -> Vec<&str> {
        companions
            .iter()
            .map(|companion| companion.file_name.as_str())
            .collect()
    }

    /// Proton loads Wine's own copy of a proxy unless told to prefer the one beside the game, and
    /// has none of Bink's.
    #[test]
    fn scan_names_the_overrides_proton_needs() {
        let game = Scratch::game("overrides");
        touch(&game.win64().join("DSOUND.dll"), b"MZ");
        touch(&game.win64().join("version.dll.disabled"), b"MZ");
        touch(&game.win64().join("winmm.dll"), b"MZ");
        touch(&game.win64().join("binkw64.dll"), b"MZ");

        let option = scan(game.root()).wine_launch_option;
        if cfg!(windows) {
            assert_eq!(option, None);
        } else {
            assert_eq!(
                option.as_deref(),
                Some(r#"WINEDLLOVERRIDES="dsound,winmm=n,b" %command%"#)
            );
        }
    }

    /// Each plugin takes the files carrying its name, the longest name winning, whatever their
    /// case; a file no plugin names, such as a config left behind, is no one's.
    #[test]
    fn scan_groups_companions_by_name() {
        let game = Scratch::game("scan");
        let plugins = game.plugins();
        touch(&game.win64().join("dsound.dll"), b"MZ");
        touch(&game.win64().join("dsound.ini"), b"");
        touch(&game.win64().join("steam_api64.dll"), b"MZ");
        touch(&plugins.join("PigeonPass.asi"), b"MZ");
        touch(&plugins.join("PigeonPass.ini"), b"");
        touch(&plugins.join("pigeonpass.LOG"), b"");
        touch(&plugins.join("RivalsDbg.ASI.disabled"), b"MZ");
        touch(&plugins.join("Foo.asi"), b"MZ");
        touch(&plugins.join("Foo.Bar.asi"), b"MZ");
        touch(&plugins.join("Foo.Bar.ini"), b"");
        touch(&plugins.join("SigeonLoader.ini"), b"");
        touch(&plugins.join("scripts").join("probe.py"), b"");

        let status = scan(game.root());

        assert_eq!(status.loaders.len(), 1);
        let loader = &status.loaders[0];
        assert_eq!(loader.proxy_for, "dsound.dll");
        assert!(loader.enabled);
        assert_eq!(loader.kind, LoaderKind::Unknown);
        assert_eq!(companion_names(&loader.companions), ["dsound.ini"]);

        assert_eq!(
            names(&status.plugins),
            [
                "Foo.asi",
                "Foo.Bar.asi",
                "PigeonPass.asi",
                "RivalsDbg.ASI.disabled"
            ]
        );
        let by_name = |name: &str| {
            status
                .plugins
                .iter()
                .find(|plugin| plugin.name == name)
                .unwrap()
        };
        assert!(by_name("Foo").companions.is_empty());
        assert_eq!(
            companion_names(&by_name("Foo.Bar").companions),
            ["Foo.Bar.ini"]
        );
        assert_eq!(
            companion_names(&by_name("PigeonPass").companions),
            ["PigeonPass.ini", "pigeonpass.LOG"]
        );
        let switched_off = by_name("RivalsDbg");
        assert!(!switched_off.enabled);
        assert_eq!(switched_off.location, "plugins");
        assert!(
            status
                .plugins
                .iter()
                .flat_map(|plugin| &plugin.companions)
                .all(|companion| companion.file_name != "SigeonLoader.ini")
        );
    }

    #[test]
    fn ini_files_are_the_plugins_configs_and_a_loaders_own() {
        let game = Scratch::game("ini-files");
        let plugins = game.plugins();
        touch(&game.win64().join("dsound.dll"), b"MZ");
        touch(&game.win64().join("dsound.ini"), b"");
        touch(&game.win64().join("Engine.ini"), b"");
        touch(&plugins.join("PigeonPass.asi"), b"MZ");
        touch(&plugins.join("PigeonPass.ini"), b"");
        touch(&plugins.join("PigeonPass.log"), b"");
        touch(&plugins.join("SigeonLoader.ini"), b"");
        touch(&plugins.join("scripts").join("Nested.ini"), b"");

        let names: Vec<String> = ini_files(game.root())
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();

        assert_eq!(names, ["dsound.ini", "PigeonPass.ini", "SigeonLoader.ini"]);
    }

    #[test]
    fn a_plugin_beside_the_game_shadows_its_namesake_in_plugins() {
        let game = Scratch::game("shadow");
        touch(&game.win64().join("Twin.asi"), b"MZ");
        touch(&game.plugins().join("twin.asi"), b"MZ");

        let status = scan(game.root());

        let in_plugins = status
            .plugins
            .iter()
            .find(|plugin| plugin.location == "plugins")
            .unwrap();
        assert_eq!(
            in_plugins.shadowed_by.as_deref(),
            Some(game.win64().join("Twin.asi").display().to_string().as_str())
        );
        let beside = status
            .plugins
            .iter()
            .find(|plugin| plugin.location.is_empty())
            .unwrap();
        assert!(beside.shadowed_by.is_none());
    }

    #[test]
    fn switching_renames_and_never_writes_over_a_file() {
        let game = Scratch::game("switch");
        let on = game.plugins().join("Mod.asi");
        touch(&on, b"on");

        let (off, changed) = set_enabled(game.root(), &on, false).expect("off");
        assert!(changed);
        assert_eq!(off, game.plugins().join("Mod.asi.disabled"));
        assert!(!on.exists());
        let (_, again) = set_enabled(game.root(), &off, false).expect("already off");
        assert!(!again);

        touch(&on, b"another");
        let refused = set_enabled(game.root(), &off, true).expect_err("would write over Mod.asi");
        assert!(refused.contains("would write over"), "{refused}");
        assert_eq!(fs::read(&on).unwrap(), b"another");
        assert!(off.exists());

        let loader = game.win64().join("dsound.dll");
        touch(&loader, b"MZ");
        let (loader_off, _) = set_enabled(game.root(), &loader, false).expect("loader off");
        assert_eq!(loader_off, game.win64().join("dsound.dll.disabled"));
    }

    #[test]
    fn only_plugins_and_loaders_inside_win64_are_touched() {
        let game = Scratch::game("contain");
        let exe = game.win64().join("Marvel-Win64-Shipping.exe");
        touch(&exe, b"MZ");
        let elsewhere = Scratch::new("elsewhere");
        let outside = elsewhere.0.join("Stray.asi");
        touch(&outside, b"MZ");

        assert!(set_enabled(game.root(), &exe, false).is_err());
        assert!(delete(game.root(), &exe, false).is_err());
        assert!(exe.exists());
        assert!(contained(game.root(), &outside).is_err());
        assert!(set_enabled(game.root(), &outside, false).is_err());
        assert!(delete(game.root(), &outside, false).is_err());
        assert!(outside.exists());
        assert!(contained(game.root(), &game.win64()).is_err());
    }

    #[test]
    fn deleting_takes_companions_only_when_asked() {
        let game = Scratch::game("delete");
        let plugins = game.plugins();
        touch(&plugins.join("Keep.asi"), b"MZ");
        touch(&plugins.join("Keep.ini"), b"key");
        touch(&plugins.join("Gone.asi"), b"MZ");
        touch(&plugins.join("Gone.ini"), b"");
        touch(&plugins.join("Gone.log"), b"");
        touch(&plugins.join("Gone.Too.asi"), b"MZ");
        touch(&plugins.join("Gone.Too.ini"), b"");

        delete(game.root(), &plugins.join("Keep.asi"), false).expect("delete");
        assert!(!plugins.join("Keep.asi").exists());
        assert!(plugins.join("Keep.ini").exists());

        let gone = delete(game.root(), &plugins.join("Gone.asi"), true).expect("delete");
        assert_eq!(gone.len(), 3);
        assert!(!plugins.join("Gone.ini").exists());
        assert!(!plugins.join("Gone.log").exists());
        assert!(plugins.join("Gone.Too.ini").exists(), "belongs to Gone.Too");

        touch(&plugins.join("scripts").join("probe.py"), b"");
        assert!(delete(game.root(), &plugins.join("Keep.ini"), false).is_err());
        assert!(delete(game.root(), &plugins.join("scripts"), false).is_err());
        assert!(plugins.join("Keep.ini").exists());
        assert!(plugins.join("scripts").join("probe.py").exists());
    }

    #[test]
    fn installing_keeps_settings_and_asks_before_replacing() {
        let game = Scratch::game("install");
        let source = Scratch::new("install-source");
        let asi = source.0.join("Cool.asi");
        touch(&asi, b"v1");
        touch(&source.0.join("Cool.ini"), b"defaults");
        touch(&source.0.join("readme.txt"), b"");

        let first = install(game.root(), std::slice::from_ref(&asi), false);
        assert_eq!(first[0].error, None);
        assert_eq!(first[0].installed, ["Cool.asi", "Cool.ini"]);
        fs::write(game.plugins().join("Cool.ini"), b"mine").unwrap();
        set_enabled(game.root(), &game.plugins().join("Cool.asi"), false).expect("off");

        touch(&asi, b"v2");
        let asked = install(game.root(), std::slice::from_ref(&asi), false);
        assert_eq!(asked[0].existing, ["Cool.asi"]);
        assert!(asked[0].installed.is_empty());
        assert!(!game.plugins().join("Cool.asi").exists());

        let replaced = install(game.root(), std::slice::from_ref(&asi), true);
        assert_eq!(replaced[0].error, None);
        assert_eq!(replaced[0].installed, ["Cool.asi"]);
        assert_eq!(replaced[0].kept, ["Cool.ini"]);
        assert_eq!(fs::read(game.plugins().join("Cool.asi")).unwrap(), b"v2");
        assert_eq!(fs::read(game.plugins().join("Cool.ini")).unwrap(), b"mine");
        assert!(!game.plugins().join("Cool.asi.disabled").exists());
    }

    #[test]
    fn a_dropped_dll_goes_in_as_an_asi_and_a_loader_does_not() {
        let game = Scratch::game("install-dll");
        let source = Scratch::new("install-dll-source");
        let dll = source.0.join("Cool.dll");
        touch(&dll, b"MZ plugin");
        touch(&source.0.join("Cool.ini"), b"defaults");
        touch(&source.0.join("dsound.dll"), b"MZ loader");

        let outcome = install(game.root(), std::slice::from_ref(&dll), false);
        assert_eq!(outcome[0].error, None);
        assert_eq!(outcome[0].installed, ["Cool.asi", "Cool.ini"]);
        assert_eq!(
            fs::read(game.plugins().join("Cool.asi")).unwrap(),
            b"MZ plugin"
        );
        assert!(!game.plugins().join("Cool.dll").exists());

        let loader = install(game.root(), &[source.0.join("dsound.dll")], false);
        assert!(
            loader[0]
                .error
                .as_deref()
                .is_some_and(|error| error.contains("ASI loader")),
            "{:?}",
            loader[0].error
        );
        assert!(!game.plugins().join("dsound.asi").exists());
    }

    /// A plugin's build leaves it as a `.dll` too, with its symbols: only the `.asi` and its
    /// settings go in, however it is dropped, and once when several of its files are.
    #[test]
    fn only_the_asi_of_a_build_goes_in() {
        let source = Scratch::new("install-build-source");
        let build = source.0.join("x64").join("Release");
        touch(&build.join("Cool.asi"), b"MZ asi");
        touch(&build.join("Cool.dll"), b"MZ dll");
        for leftover in ["Cool.pdb", "Cool.lib", "Cool.exp"] {
            touch(&build.join(leftover), b"");
        }
        touch(&build.join("Cool.ini"), b"[General]");
        let plugins_of = |game: &Scratch| -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(game.plugins())
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        };

        let all = Scratch::game("install-build-all");
        let named = ["Cool.asi", "Cool.dll", "Cool.pdb"].map(|name| build.join(name));
        let outcomes = install(all.root(), &named, false);
        assert!(outcomes.iter().all(|o| o.error.is_none()), "{outcomes:?}");
        assert!(
            outcomes.iter().all(|o| o.existing.is_empty()),
            "{outcomes:?}"
        );
        assert_eq!(outcomes[0].installed, ["Cool.asi", "Cool.ini"]);
        assert_eq!(outcomes[2].skipped.len(), 1);
        assert_eq!(plugins_of(&all), ["Cool.asi", "Cool.ini"]);

        let dll = Scratch::game("install-build-dll");
        let outcome = install(dll.root(), &[build.join("Cool.dll")], false);
        assert_eq!(outcome[0].installed, ["Cool.asi", "Cool.ini"]);
        assert_eq!(fs::read(dll.plugins().join("Cool.asi")).unwrap(), b"MZ asi");

        let folder = Scratch::game("install-build-folder");
        let outcome = install(folder.root(), std::slice::from_ref(&source.0), false);
        assert_eq!(outcome[0].error, None);
        assert_eq!(plugins_of(&folder), ["Cool.asi", "Cool.ini"]);
    }

    #[test]
    fn installing_a_folder_skips_its_dlls() {
        let game = Scratch::game("install-folder");
        let source = Scratch::new("install-folder-source");
        touch(&source.0.join("plugins").join("Deep.asi"), b"MZ");
        touch(&source.0.join("plugins").join("Deep.json"), b"{}");
        touch(&source.0.join("dinput8.dll"), b"MZ");
        touch(&source.0.join("helper.dll"), b"MZ");

        let outcome = install(game.root(), std::slice::from_ref(&source.0), false);

        assert_eq!(outcome[0].error, None);
        assert_eq!(outcome[0].installed, ["Deep.asi", "Deep.json"]);
        let skipped: Vec<&str> = outcome[0]
            .skipped
            .iter()
            .map(|skip| skip.file.as_str())
            .collect();
        assert_eq!(skipped, ["dinput8.dll", "helper.dll"]);
        assert!(!game.plugins().join("dinput8.dll").exists());

        let empty = Scratch::new("install-empty");
        let nothing = install(game.root(), std::slice::from_ref(&empty.0), false);
        assert_eq!(nothing[0].error.as_deref(), Some("holds no .asi plugin"));
    }

    #[cfg(feature = "archives")]
    #[test]
    fn installing_a_zip_unpacks_its_plugins() {
        use std::io::Write;

        let game = Scratch::game("install-zip");
        let source = Scratch::new("install-zip-source");
        let archive = source.0.join("Pack.zip");
        let mut zip = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        for (name, body) in [
            ("Pack/plugins/Zipped.asi", b"MZ".as_slice()),
            ("Pack/plugins/Zipped.ini", b"[General]".as_slice()),
            ("Pack/dsound.dll", b"MZ".as_slice()),
        ] {
            zip.start_file(name, options).unwrap();
            zip.write_all(body).unwrap();
        }
        zip.finish().unwrap();

        let outcome = install(game.root(), std::slice::from_ref(&archive), false);

        assert_eq!(outcome[0].error, None);
        assert_eq!(outcome[0].installed, ["Zipped.asi", "Zipped.ini"]);
        assert_eq!(outcome[0].skipped.len(), 1);
        assert!(game.plugins().join("Zipped.ini").exists());
    }
}
