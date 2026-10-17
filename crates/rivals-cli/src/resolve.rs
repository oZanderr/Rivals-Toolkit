//! Turns the loose `--game-root` argument and the names a command gives a mod into concrete,
//! existing paths.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rivals_core::mod_ops::ModFiles;
use rivals_core::paths::mods_dir;

use crate::settings::{self, AppSettings};

/// The game root to work against: the flag if given, written whole so the paths built on it print
/// with one separator, otherwise whatever the desktop app saved.
pub fn game_root(flag: Option<&str>, app: &AppSettings) -> Result<String, String> {
    let root = match flag {
        Some(path) => whole(Path::new(path)).to_string_lossy().into_owned(),
        None => app.game_path.clone().ok_or_else(|| {
            let hint = settings::settings_path()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "the app's settings".to_string());
            format!("no game root: pass --game-root, or set the game path in the app ({hint})")
        })?,
    };
    if !mods_dir(&root).is_dir() {
        return Err(format!(
            "{} has no MarvelGame\\Marvel\\Content\\Paks\\~mods folder",
            root
        ));
    }
    Ok(root)
}

/// `path` written whole, from the drive down with one kind of separator, as the paths the toolkit
/// joins onto it are; one that cannot be is kept as given.
fn whole(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The suffixes a mod's name takes in `~mods`, in the order a bare name is looked up.
const SUFFIXES: [&str; 5] = ["", ".pak", ".utoc", ".pak.disabled", ".utoc.disabled"];

/// A file of the mod `arg` names, switched on or off: a path to one of its files, or its exact name
/// in `~mods`, with or without an extension and in any case, as Windows names files. Nothing short
/// of the whole name is taken: a near miss is refused with the names it may have meant.
pub fn installed_mod(
    arg: &str,
    root_flag: Option<&str>,
    app: &AppSettings,
) -> Result<PathBuf, String> {
    let direct = PathBuf::from(arg);
    if direct.is_file() {
        return Ok(whole(&direct));
    }
    if direct.components().count() > 1 || direct.is_absolute() {
        return Err(format!("{arg} does not exist"));
    }
    in_mods(arg, &mods_dir(&game_root(root_flag, app)?))
}

/// How a hint names a mod so the command it suggests acts on this same copy: by its name when it
/// sits at the top of `mods`, where a name finds it, and by a path to it anywhere else, where its
/// name would find the installed mod of that name instead.
pub fn shown_mod(files: &ModFiles, mods: &Path) -> String {
    let at_top = matches!(
        (files.dir.canonicalize(), mods.canonicalize()),
        (Ok(dir), Ok(mods)) if dir == mods
    );
    if at_top {
        return files.stem.clone();
    }
    let part = files.part(if files.is_iostore() { "utoc" } else { "pak" });
    whole(&part).display().to_string()
}

/// The container a command names: a path as [`as_container`] reads it, a game container in `Paks/`
/// by its name with or without `.utoc`, or an installed mod by its name, as its `.utoc` when it has
/// one and its `.pak` otherwise.
pub fn container<'a>(given: &'a str, game_root: &str) -> Result<Cow<'a, str>, String> {
    let path = Path::new(given);
    if given.is_empty() || path.exists() || path.components().count() > 1 || path.is_absolute() {
        return Ok(as_container(given));
    }
    let paks = rivals_core::paths::paks_dir(game_root);
    for name in [given.to_string(), format!("{given}.utoc")] {
        let held = paks.join(name);
        if held.is_file() {
            return Ok(Cow::Owned(held.to_string_lossy().into_owned()));
        }
    }
    let files = ModFiles::of(&in_mods(given, &mods_dir(game_root))?)?;
    let part = files.part(if files.is_iostore() { "utoc" } else { "pak" });
    Ok(Cow::Owned(part.to_string_lossy().into_owned()))
}

/// The container a path names: an IoStore mod's `.pak`, switched on or off, is the `.utoc` beside
/// it, which holds the mod's packages. A file there is written whole, so it is not taken for a name
/// and prints with one separator; a path naming nothing is as given.
fn as_container(given: &str) -> Cow<'_, str> {
    let path = Path::new(given);
    let name = given.strip_suffix(".disabled").unwrap_or(given);
    if name.ends_with(".pak")
        && let Ok(files) = ModFiles::of(path)
        && files.is_iostore()
    {
        return Cow::Owned(whole(&files.part("utoc")).to_string_lossy().into_owned());
    }
    if path.is_file() {
        return Cow::Owned(whole(path).to_string_lossy().into_owned());
    }
    Cow::Borrowed(given)
}

/// Whether a source names the game as a whole rather than one container.
pub fn is_game(given: &str) -> bool {
    given.eq_ignore_ascii_case("game")
}

/// What `--from` names: `game` for the game's own copy, otherwise a container as [`container`]
/// finds it.
pub fn source(given: &str, game_root: &str) -> Result<String, String> {
    if is_game(given) {
        return rivals_core::asset::game_container(game_root);
    }
    if given.trim().is_empty() {
        return Err("--from names no source".into());
    }
    container(given, game_root).map(Cow::into_owned)
}

/// The copy of `asset` the game loads: the first switched-on IoStore mod holding it, in winning
/// order and leaving out the container `skip`, else the game's own.
pub fn loaded_copy(asset: &str, game_root: &str, skip: Option<&Path>) -> Result<String, String> {
    let paks = rivals_core::paths::paks_dir(game_root);
    for utoc in rivals_core::pak::containers::mod_utocs(&paks) {
        if skip.is_some_and(|skip| same_mod(&utoc, skip)) {
            continue;
        }
        if rivals_core::asset::container_holds(&utoc, asset).unwrap_or(false) {
            return Ok(utoc.to_string_lossy().into_owned());
        }
    }
    rivals_core::asset::game_container(game_root)
}

/// Whether two files are the same mod's: the name its files share, switched on or off, in the same
/// folder, so a mod of that name elsewhere is another mod.
fn same_mod(a: &Path, b: &Path) -> bool {
    let stem = rivals_core::pak::containers::container_stem;
    let folder = |path: &Path| {
        let dir = path
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf())
    };
    stem(a)
        .zip(stem(b))
        .is_some_and(|(a, b)| a.eq_ignore_ascii_case(&b))
        && folder(a) == folder(b)
}

/// Where an asset is read from, and what it is asked for there.
pub struct Source {
    /// A mod's or the game's container, a plain pak, or nothing for a file on disk.
    pub container: String,
    /// The package name or path inside the container, or the file on disk.
    pub entry: String,
}

impl Source {
    /// `asset` as `container` is asked for it: a plain pak knows its files by path only.
    fn of(container: String, asset: &str, game_root: &str) -> Result<Self, String> {
        let name = container.strip_suffix(".disabled").unwrap_or(&container);
        let entry = if name.ends_with(".pak") && asset.starts_with('/') {
            rivals_core::asset::entry_for_package(game_root, asset)?
        } else {
            asset.to_string()
        };
        Ok(Self { container, entry })
    }

    /// Where it is read, as a person names it: the file on disk, a mod by its name, the game's own
    /// copy for any of the game's containers, or a container elsewhere by its path.
    pub fn describe(&self, game_root: &str) -> String {
        let container = Path::new(&self.container);
        if self.container.is_empty() {
            return self.entry.clone();
        }
        if container.starts_with(mods_dir(game_root)) {
            return rivals_core::pak::containers::container_stem(container)
                .unwrap_or_else(|| self.container.clone());
        }
        if container.starts_with(rivals_core::paths::paks_dir(game_root)) {
            return "the game's copy".into();
        }
        self.container.clone()
    }
}

/// Where a read of `asset` goes: `from` when given, a file on disk as it is, and otherwise the
/// copy the game loads.
pub fn read_source(asset: &str, from: Option<&str>, game_root: &str) -> Result<Source, String> {
    if asset.trim().is_empty() {
        return Err("name the asset".into());
    }
    let container = match from {
        Some(from) => source(from, game_root)?,
        None if Path::new(asset).is_file() => String::new(),
        None => loaded_copy(asset, game_root, None)?,
    };
    Source::of(container, asset, game_root)
}

/// Where a save into the mod whose pak is `into` reads `asset`, and whether that is the mod's own
/// copy, which the save builds on.
pub struct SaveSource {
    pub source: Source,
    /// The save reads the mod's own copy.
    pub layer: bool,
}

/// With `from` the save reads there, and a mod holding a copy of its own is refused unless
/// `overwrite`. Without it, a mod holding the asset is built on; otherwise, or with `overwrite`,
/// the save starts from the game's copy, and is refused when another switched-on mod overrides the
/// asset, since starting from the game's copy and building on that mod's are both reasonable and
/// make different mods. A file on disk is a source named as plainly as `from`.
pub fn save_source(
    asset: &str,
    from: Option<&str>,
    game_root: &str,
    into: &Path,
    overwrite: bool,
    format: rivals_core::asset_edit::SaveTarget,
    choose: &str,
) -> Result<SaveSource, String> {
    use rivals_core::asset_edit::SaveTarget;
    if asset.trim().is_empty() {
        return Err("name the asset".into());
    }
    // The mod's own file in the format the save writes: a container, or a plain pak.
    let own = match format {
        SaveTarget::IoStore => into.with_extension("utoc"),
        SaveTarget::Pak => into.with_extension("pak"),
    };
    let held = || {
        own.is_file()
            && match format {
                SaveTarget::IoStore => rivals_core::asset::container_holds(&own, asset),
                SaveTarget::Pak => rivals_core::asset::pak_holds(game_root, &own, asset),
            }
            .unwrap_or(false)
    };
    let (container, layer) = match from {
        Some(from) => {
            let container = source(from, game_root)?;
            let layer = same_mod(Path::new(&container), &own);
            (container, layer)
        }
        None if Path::new(asset).is_file() => (String::new(), false),
        None if !overwrite && held() => (own.to_string_lossy().into_owned(), true),
        None => {
            let loaded = loaded_copy(asset, game_root, Some(&own))?;
            (
                unnamed_source(asset, loaded, &mods_dir(game_root), overwrite, choose)?,
                false,
            )
        }
    };
    Ok(SaveSource {
        source: Source::of(container, asset, game_root)?,
        layer,
    })
}

/// What a save naming no source reads when it does not build on its mod's copy: `loaded`, the copy
/// the game loads without that mod, when that is the game's own; one in a mod under `mods` is
/// refused, naming both ways to choose with `choose`, the flag or field the caller takes it by.
fn unnamed_source(
    asset: &str,
    loaded: String,
    mods: &Path,
    overwrite: bool,
    choose: &str,
) -> Result<String, String> {
    if !Path::new(&loaded).starts_with(mods) {
        return Ok(loaded);
    }
    let other = rivals_core::pak::containers::container_stem(Path::new(&loaded)).unwrap_or(loaded);
    let start = if overwrite {
        "start again from"
    } else {
        "start from"
    };
    Err(format!(
        "{other} also overrides {asset}, so which copy to {start} is not clear; name it with \
         {choose}: `game` for the game's own copy, or `{other}` for that mod's"
    ))
}

/// The ending every mod a save goes into has, as the desktop app names the mods it saves into.
const SAVE_SUFFIX: &str = "_9999999_P";

/// The mod a save goes into, by the name the save takes it by, and a note when the save makes it.
#[derive(Debug)]
pub struct Into {
    pub name: String,
    pub note: Option<String>,
}

/// The mod `given` names for a save, or without it `default`: an installed mod by a path to one of
/// its files or by its whole name, an IoStore mod anywhere else by a path to one of its files, or
/// with `make`, a name no installed mod has, which the save makes, said so with the installed names
/// close to it. A name the save completes, `MyMod` for `MyMod_9999999_P`, finds that mod as the
/// desktop app does. Saves go into a switched-on mod whose name ends `_9999999_P`, so one named
/// otherwise is refused with the reason. One at the top of `~mods` is taken by its name, and one
/// anywhere else by its whole path, since a mod of its name in `~mods` is another mod.
pub fn into_mod(
    given: Option<&str>,
    default: &str,
    game_root: &str,
    make: bool,
) -> Result<Into, String> {
    let named = given.unwrap_or(default).trim();
    let mods = mods_dir(game_root);
    let path = Path::new(named);
    let found = if path.is_file() {
        Some(path.to_path_buf())
    } else if path.components().count() > 1 || path.is_absolute() {
        return Err(format!("{named} does not exist"));
    } else {
        in_mods(named, &mods).ok()
    };
    if let Some(file) = found {
        let files = ModFiles::of(&file)?;
        let at_top = files.dir.canonicalize().ok() == mods.canonicalize().ok();
        if !at_top && !files.is_iostore() {
            return Err(format!(
                "{} is a plain pak mod outside {}; a save goes into an IoStore mod anywhere, or \
                 into a plain pak in {}",
                file.display(),
                mods.display(),
                mods.display()
            ));
        }
        // How a hint names the mod: by its name in `~mods`, by its path anywhere else.
        let shown = if at_top {
            files.stem.clone()
        } else {
            file.display().to_string()
        };
        if !files
            .stem
            .to_ascii_lowercase()
            .ends_with(&SAVE_SUFFIX.to_ascii_lowercase())
        {
            return Err(format!(
                "{stem} does not end in {SAVE_SUFFIX} as the mods a save goes into do; `mods rename \
                 {shown} --to {stem}{SAVE_SUFFIX}` renames it so",
                stem = files.stem
            ));
        }
        if files.disabled {
            return Err(format!(
                "{stem} is switched off; `mods enable {shown}` switches it on to save into it",
                stem = files.stem
            ));
        }
        let name = if at_top {
            files.stem
        } else {
            rivals_core::asset_edit::mod_name_of(&files.part("utoc"))?
        };
        return Ok(Into { name, note: None });
    }
    let pak = rivals_core::asset_edit::mod_pak(game_root, named)?;
    if pak.is_file() || pak.with_extension("utoc").is_file() {
        return Ok(Into {
            name: named.to_string(),
            note: None,
        });
    }
    let files = files_in(&mods)?;
    if !make {
        return Err(missed(named, &mods, &files));
    }
    let made = pak.file_stem().unwrap_or_default().to_string_lossy();
    let note = match close_names(named, &files).as_slice() {
        [] => format!("no mod named {named} is installed, so the save makes {made}"),
        close => format!(
            "no mod named {named} is installed, so the save makes {made}; did you mean {}?",
            close.join(" or ")
        ),
    };
    Ok(Into {
        name: named.to_string(),
        note: Some(note),
    })
}

/// The files at the top of `mods`.
fn files_in(mods: &Path) -> Result<Vec<PathBuf>, String> {
    Ok(std::fs::read_dir(mods)
        .map_err(|e| format!("could not read {}: {e}", mods.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.is_file())
        .collect())
}

/// The installed mods whose names hold what was typed.
fn close_names(name: &str, files: &[PathBuf]) -> Vec<String> {
    let stems: BTreeSet<String> = files
        .iter()
        .filter_map(|file| ModFiles::of(file).ok())
        .map(|files| files.stem)
        .collect();
    let typed = name.trim_start_matches('!').to_lowercase();
    stems
        .into_iter()
        .filter(|stem| !typed.is_empty() && stem.to_lowercase().contains(&typed))
        .collect()
}

/// The file of the mod named `name` in `mods`, or why there is none.
fn in_mods(name: &str, mods: &Path) -> Result<PathBuf, String> {
    let files = files_in(mods)?;
    let file_name = |path: &PathBuf| {
        path.file_name()
            .map(|name| name.to_string_lossy().to_lowercase())
    };
    for suffix in SUFFIXES {
        let wanted = format!("{name}{suffix}").to_lowercase();
        if let Some(found) = files
            .iter()
            .find(|file| file_name(file).as_deref() == Some(&wanted))
        {
            return Ok(found.clone());
        }
    }
    Err(missed(name, mods, &files))
}

/// A lookup that found no mod: the installed names holding what was typed, or where to see them all.
fn missed(name: &str, mods: &Path, files: &[PathBuf]) -> String {
    match close_names(name, files).as_slice() {
        [] => format!(
            "no mod named '{name}' in {}; `mods list` lists the installed ones",
            mods.display()
        ),
        [one] => format!(
            "no mod named '{name}' in {}; did you mean {one}?",
            mods.display()
        ),
        many => format!(
            "no mod named '{name}' in {}; did you mean one of {}?",
            mods.display(),
            many.join(", ")
        ),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    /// A folder of its own, gone when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos();
            Self(
                std::env::temp_dir().join(format!("rivals-resolve-{}-{stamp}", std::process::id())),
            )
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A `~mods` folder with one mod switched on and one switched off.
    fn mods() -> (Scratch, PathBuf) {
        let dir = Scratch::new();
        let mods = dir.path().join("mods");
        std::fs::create_dir_all(&mods).unwrap();
        for file in [
            "!ProjectSigeon_9999999_P.pak",
            "!ProjectSigeon_9999999_P.utoc",
            "!ProjectSigeon_9999999_P.ucas",
            "!ProjectGalacta_9999999_P.pak.disabled",
            "!ProjectGalacta_9999999_P.utoc.disabled",
            "!ProjectGalacta_9999999_P.ucas.disabled",
        ] {
            std::fs::write(mods.join(file), b"").unwrap();
        }
        (dir, mods)
    }

    /// A mod named by a path spelt with forward slashes comes back written whole with the system's
    /// own, so what is joined onto it prints with one kind of separator.
    #[cfg(windows)]
    #[test]
    fn a_path_given_comes_back_with_one_separator() {
        let (_dir, mods) = mods();
        let spelt = mods
            .join("!ProjectSigeon_9999999_P.utoc")
            .to_string_lossy()
            .replace('\\', "/");
        let found = installed_mod(&spelt, None, &AppSettings::default()).expect("the mod");
        assert!(
            !found.to_string_lossy().contains('/'),
            "{}",
            found.display()
        );
        assert_eq!(found, mods.join("!ProjectSigeon_9999999_P.utoc"));
    }

    /// A container named by a path spelt with forward slashes is read, and so described, by the
    /// path written whole with the system's own.
    #[cfg(windows)]
    #[test]
    fn a_container_path_given_comes_back_with_one_separator() {
        let (_dir, mods) = mods();
        let utoc = mods.join("!ProjectSigeon_9999999_P.utoc");
        let spelt = utoc.to_string_lossy().replace('\\', "/");
        let named = container(&spelt, "unused").expect("the container");
        assert_eq!(Path::new(&*named), utoc);
        assert!(!named.contains('/'), "{named}");
    }

    /// A hint names the installed mod by its name and a copy of it elsewhere by its path, since
    /// the name alone finds the installed one.
    #[test]
    fn a_hint_names_a_copy_outside_mods_by_its_path() {
        let (dir, mods) = mods();
        let installed = ModFiles::of(&mods.join("!ProjectSigeon_9999999_P.utoc")).unwrap();
        assert_eq!(shown_mod(&installed, &mods), "!ProjectSigeon_9999999_P");
        let build = dir.path().join("build");
        std::fs::create_dir_all(&build).unwrap();
        for part in ["pak", "utoc", "ucas"] {
            std::fs::write(build.join(format!("!ProjectSigeon_9999999_P.{part}")), b"").unwrap();
        }
        let copy = ModFiles::of(&build.join("!ProjectSigeon_9999999_P.pak")).unwrap();
        assert_eq!(
            shown_mod(&copy, &mods),
            whole(&build.join("!ProjectSigeon_9999999_P.utoc"))
                .display()
                .to_string()
        );
        let off = ModFiles::of(&mods.join("!ProjectGalacta_9999999_P.pak.disabled")).unwrap();
        assert_eq!(shown_mod(&off, &mods), "!ProjectGalacta_9999999_P");
    }

    /// A whole name finds its mod in any case, with or without an extension and switched off.
    #[test]
    fn a_whole_name_finds_its_mod_in_any_case_and_state() {
        let (_dir, mods) = mods();
        let found = |name: &str| in_mods(name, &mods).expect(name);
        assert!(found("!ProjectSigeon_9999999_P").ends_with("!ProjectSigeon_9999999_P.pak"));
        assert!(found("!projectsigeon_9999999_p").ends_with("!ProjectSigeon_9999999_P.pak"));
        assert!(found("!ProjectSigeon_9999999_P.utoc").ends_with("!ProjectSigeon_9999999_P.utoc"));
        assert!(
            found("!ProjectGalacta_9999999_P").ends_with("!ProjectGalacta_9999999_P.pak.disabled")
        );
    }

    /// Part of a name is refused, never guessed at, and the refusal names what it may have meant.
    #[test]
    fn part_of_a_name_is_refused_with_what_it_may_have_meant() {
        let (_dir, mods) = mods();
        let one = in_mods("sigeon", &mods).expect_err("partial");
        assert!(
            one.contains("did you mean !ProjectSigeon_9999999_P?"),
            "{one}"
        );
        let both = in_mods("project", &mods).expect_err("partial");
        assert!(
            both.contains("!ProjectGalacta_9999999_P") && both.contains("!ProjectSigeon_9999999_P"),
            "{both}"
        );
        let none = in_mods("nothing", &mods).expect_err("absent");
        assert!(none.contains("`mods list`"), "{none}");
    }

    /// A container is a path as given, or an installed mod's `.utoc`, switched off as it is.
    #[test]
    fn a_container_is_a_path_or_an_installed_mod_s_container() {
        let (dir, mods) = mods();
        let root = dir.path().join("root");
        let paks = rivals_core::paths::mods_dir(&root.to_string_lossy());
        std::fs::create_dir_all(paks.parent().unwrap()).unwrap();
        std::fs::rename(&mods, &paks).unwrap();
        let root = root.to_string_lossy();
        let named = container("!ProjectGalacta_9999999_P", &root).expect("named");
        assert!(
            named.ends_with("!ProjectGalacta_9999999_P.utoc.disabled"),
            "{named}"
        );
        let given = paks.join("!ProjectSigeon_9999999_P.utoc");
        let given = given.to_string_lossy();
        assert_eq!(container(&given, &root).expect("a path"), given);
        assert!(container("sigeon", &root).is_err());

        // The game's own containers sit in `Paks/`, above `~mods`.
        let game = paks.parent().unwrap().join("pakchunk0-Windows.utoc");
        std::fs::write(&game, b"").unwrap();
        for name in ["pakchunk0-Windows.utoc", "pakchunk0-Windows"] {
            assert_eq!(
                container(name, &root).expect(name),
                game.to_string_lossy(),
                "{name}"
            );
        }
    }

    /// An IoStore mod's `.pak` names the container beside it, switched on or off and wherever the
    /// mod is, so a package is asked for there by its name; a plain pak stays the pak it is.
    #[test]
    fn a_mod_s_pak_names_its_container() {
        let (dir, root) = game();
        let mods = mods_dir(&root);
        let named = |path: &Path| {
            container(&path.to_string_lossy(), &root)
                .unwrap()
                .into_owned()
        };
        let on = named(&mods.join("!ProjectSigeon_9999999_P.pak"));
        assert!(on.ends_with("!ProjectSigeon_9999999_P.utoc"), "{on}");
        let off = named(&mods.join("!ProjectGalacta_9999999_P.pak.disabled"));
        assert!(
            off.ends_with("!ProjectGalacta_9999999_P.utoc.disabled"),
            "{off}"
        );
        let build = dir.path().join("build");
        std::fs::create_dir_all(&build).unwrap();
        for part in ["pak", "utoc", "ucas"] {
            std::fs::write(build.join(format!("!ProjectSigeon_9999999_P.{part}")), b"").unwrap();
        }
        let outside = named(&build.join("!ProjectSigeon_9999999_P.pak"));
        assert_eq!(
            Path::new(&outside),
            build.join("!ProjectSigeon_9999999_P.utoc")
        );
        let plain = mods.join("Plain_9999999_P.pak");
        std::fs::write(&plain, b"").unwrap();
        assert_eq!(named(&plain), plain.to_string_lossy());

        let pak = build.join("!ProjectSigeon_9999999_P.pak");
        let read = read_source("/Game/X", Some(&pak.to_string_lossy()), &root).unwrap();
        assert!(read.container.ends_with(".utoc") && read.entry == "/Game/X");
    }

    /// A game root whose `Paks/` holds one of the game's containers and whose `~mods` holds the
    /// two mods of [`mods`].
    fn game() -> (Scratch, String) {
        let (dir, mods) = mods();
        let root = dir.path().join("root");
        let into = rivals_core::paths::mods_dir(&root.to_string_lossy());
        std::fs::create_dir_all(into.parent().unwrap()).unwrap();
        std::fs::rename(&mods, &into).unwrap();
        std::fs::write(into.parent().unwrap().join("pakchunk0-Windows.utoc"), b"").unwrap();
        (dir, root.to_string_lossy().into_owned())
    }

    /// `--from` takes `game`, a mod or a container; a file on disk is read as it is; and a read
    /// naming no source reads the copy the game loads, here the game's own.
    #[test]
    fn a_read_goes_where_from_says_or_to_the_copy_the_game_loads() {
        let (dir, root) = game();
        let chunk = |source: &Source| source.container.ends_with("pakchunk0-Windows.utoc");
        let from_game = read_source("/Game/X", Some("game"), &root).unwrap();
        assert!(chunk(&from_game) && from_game.entry == "/Game/X");
        let from_mod = read_source("/Game/X", Some("!ProjectSigeon_9999999_P"), &root).unwrap();
        assert!(
            from_mod
                .container
                .ends_with("!ProjectSigeon_9999999_P.utoc")
        );
        assert!(read_source("/Game/X", Some("sigeon"), &root).is_err());
        assert!(chunk(&read_source("/Game/X", None, &root).unwrap()));
        let file = dir.path().join("Thing.uasset");
        std::fs::write(&file, b"").unwrap();
        let loose = read_source(&file.to_string_lossy(), None, &root).unwrap();
        assert!(loose.container.is_empty());
    }

    /// A save reading the mod it goes into builds on its copy; one reading anywhere else does
    /// not, and a file on disk is a source as plainly named as `--from`.
    #[test]
    fn a_save_builds_on_its_mod_only_when_it_reads_it() {
        let (dir, root) = game();
        let into = mods_dir(&root).join("!ProjectSigeon_9999999_P.pak");
        let save = |asset: &str, from: Option<&str>| {
            save_source(
                asset,
                from,
                &root,
                &into,
                false,
                Default::default(),
                "--from",
            )
            .expect("a source")
        };
        let own = save("/Game/X", Some("!projectsigeon_9999999_p"));
        assert!(own.layer);
        assert!(!save("/Game/X", Some("game")).layer);
        assert!(!save("/Game/X", Some("!ProjectGalacta_9999999_P")).layer);
        let build = dir.path().join("build");
        std::fs::create_dir_all(&build).unwrap();
        for part in ["pak", "utoc", "ucas"] {
            std::fs::write(build.join(format!("!ProjectSigeon_9999999_P.{part}")), b"").unwrap();
        }
        let elsewhere = build.join("!ProjectSigeon_9999999_P.utoc");
        assert!(
            !save("/Game/X", Some(&elsewhere.to_string_lossy())).layer,
            "a mod of its name elsewhere is another mod"
        );
        let here = std::env::current_dir().unwrap().join("A_9999999_P.pak");
        assert!(same_mod(Path::new("A_9999999_P.utoc"), &here));
        let default = save("/Game/X", None);
        assert!(!default.layer && default.source.container.ends_with("pakchunk0-Windows.utoc"));
        let file = dir.path().join("Thing.uasset");
        std::fs::write(&file, b"").unwrap();
        let loose = save(&file.to_string_lossy(), None);
        assert!(loose.source.container.is_empty() && !loose.layer);
    }

    /// A save goes into an installed mod named by a path or its whole name, or the name the app
    /// completes; a name no mod has makes one, saying so with the close names, and a path that is
    /// not a mod's, or a mod a save cannot go into, is refused.
    #[test]
    fn a_save_goes_into_a_mod_named_as_every_mod_is() {
        let (dir, root) = game();
        let mods = mods_dir(&root);
        let into = |given: &str, make: bool| into_mod(Some(given), "AssetEdits", &root, make);
        let by_path = mods.join("!ProjectSigeon_9999999_P.utoc");
        for given in [
            by_path.to_string_lossy().into_owned(),
            "!projectsigeon_9999999_p".into(),
            "!ProjectSigeon".into(),
        ] {
            let found = into(&given, true).expect("an installed mod");
            let pak = rivals_core::asset_edit::mod_pak(&root, &found.name).unwrap();
            assert!(pak.ends_with("!ProjectSigeon_9999999_P.pak"), "{given}");
            assert!(found.note.is_none(), "{given}");
        }
        let made = into("Sigeon", true).expect("a new mod");
        let note = made.note.expect("said so");
        assert!(
            note.contains("Sigeon_9999999_P") && note.contains("!ProjectSigeon_9999999_P"),
            "{note}"
        );
        assert!(into("Sigeon", false).is_err(), "a mod that has to be there");
        assert!(
            into("!ProjectGalacta_9999999_P", true)
                .unwrap_err()
                .contains("switched off")
        );
        assert!(
            into(&dir.path().join("nowhere/M.pak").to_string_lossy(), true)
                .unwrap_err()
                .contains("does not exist")
        );
        // Elsewhere a plain pak is refused, and an IoStore mod is taken by its whole path, which
        // names it and not a mod of its name in `~mods`.
        let outside = dir.path().join("Elsewhere_9999999_P.pak");
        std::fs::write(&outside, b"").unwrap();
        let plain = into(&outside.to_string_lossy(), true).unwrap_err();
        assert!(plain.contains("plain pak mod outside"), "{plain}");
        std::fs::write(outside.with_extension("utoc"), b"").unwrap();
        let found = into(&outside.to_string_lossy(), true).expect("an IoStore mod elsewhere");
        assert!(Path::new(&found.name).is_absolute(), "{}", found.name);
        assert_eq!(
            rivals_core::asset_edit::mod_pak(&root, &found.name).unwrap(),
            outside
        );
        let unnamed = dir.path().join("Unnamed.utoc");
        std::fs::write(&unnamed, b"").unwrap();
        let refused = into(&unnamed.to_string_lossy(), true).unwrap_err();
        assert!(
            refused.contains("mods rename") && refused.contains(&*unnamed.to_string_lossy()),
            "{refused}"
        );
        let off = dir.path().join("Off_9999999_P.utoc.disabled");
        std::fs::write(&off, b"").unwrap();
        let refused = into(&off.to_string_lossy(), true).unwrap_err();
        assert!(refused.contains("switched off"), "{refused}");
        std::fs::write(mods.join("Plain.pak"), b"").unwrap();
        assert!(into("Plain", true).unwrap_err().contains("mods rename"));
        assert_eq!(
            into_mod(None, "AssetEdits", &root, true).unwrap().name,
            "AssetEdits"
        );
    }

    /// A save naming no source starts from the game's copy, and is refused when another mod
    /// overrides the asset, since either copy could be meant; the refusal names both.
    #[test]
    fn a_save_naming_no_source_is_refused_when_another_mod_overrides_it() {
        let (_dir, root) = game();
        let mods = mods_dir(&root);
        let game = rivals_core::asset::game_container(&root).unwrap();
        assert_eq!(
            unnamed_source("/Game/X", game.clone(), &mods, false, "--from"),
            Ok(game)
        );
        let other = mods
            .join("Other_9999999_P.utoc")
            .to_string_lossy()
            .into_owned();
        for overwrite in [false, true] {
            let refused = unnamed_source("/Game/X", other.clone(), &mods, overwrite, "--from")
                .expect_err("another mod overrides it");
            assert!(
                refused.contains("Other_9999999_P also overrides /Game/X")
                    && refused.contains("`game`")
                    && refused.contains("--from"),
                "{refused}"
            );
        }
    }

    /// A save in plain pak format into a plain pak mod builds on the copy the pak holds, and starts
    /// from the copy the game loads when it holds none.
    #[test]
    fn a_plain_pak_save_builds_on_the_plain_pak_mod_s_copy() {
        use rivals_core::asset_edit::SaveTarget;
        let (dir, root) = game();
        let loose = dir.path().join("loose");
        let held = loose.join("Marvel/Content/Mods/Thing.uasset");
        std::fs::create_dir_all(held.parent().unwrap()).unwrap();
        std::fs::write(&held, b"asset").unwrap();
        let into = mods_dir(&root).join("PlainMod_9999999_P.pak");
        rivals_core::pak::plain::pack_dir(
            &loose,
            &into,
            &rivals_core::pak::plain::PakOptions {
                compression: rivals_core::pak::profile::PackCompression::Zlib,
                oodle_level: None,
            },
        )
        .unwrap();
        let save = |asset: &str, format| {
            save_source(asset, None, &root, &into, false, format, "--from").expect("a source")
        };
        for asset in ["/Game/Mods/Thing", "Marvel/Content/Mods/Thing.uasset"] {
            let built_on = save(asset, SaveTarget::Pak);
            assert!(built_on.layer, "{asset}");
            assert!(
                built_on
                    .source
                    .container
                    .ends_with("PlainMod_9999999_P.pak")
            );
        }
        assert!(!save("/Game/Mods/Other", SaveTarget::Pak).layer);
        assert!(
            !save("/Game/Mods/Thing", SaveTarget::IoStore).layer,
            "an IoStore save looks in the mod's container, which it has none of"
        );
    }
}
