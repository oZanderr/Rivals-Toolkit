//! Plain paks: the `.pak` files the game reads loose files from, beside the IoStore containers it
//! loads packages from. Every one written here takes the game's profile, key and mount point.

use std::collections::HashSet;
use std::fs;
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use super::crypto::{make_aes_key, open_pak};
use super::profile::{PackCompression, RIVALS_PROFILE, strip_mount_prefix};

/// How a pak is compressed.
#[derive(Debug, Clone, Copy, Default)]
pub struct PakOptions {
    pub compression: PackCompression,
    /// How hard Oodle works, `None` for repak's own default. Zlib ignores it.
    pub oodle_level: Option<retoc::OodleCompressionLevel>,
}

type Writer = repak::PakWriter<BufWriter<fs::File>>;

/// Writes a pak at `out`, its entries put in by `fill`.
fn write_with(
    out: &Path,
    options: &PakOptions,
    fill: impl FnOnce(&mut Writer) -> Result<(), String>,
) -> Result<(), String> {
    write_mounted(out, RIVALS_PROFILE.mount_point(), options, fill)
}

/// [`write_with`] at the mount point `mount`, which a pak written here never takes but one
/// another packer wrote may.
fn write_mounted(
    out: &Path,
    mount: &str,
    options: &PakOptions,
    fill: impl FnOnce(&mut Writer) -> Result<(), String>,
) -> Result<(), String> {
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let file = fs::File::create(out).map_err(|e| format!("create {}: {e}", out.display()))?;
    let mut builder = repak::PakBuilder::new()
        .profile(RIVALS_PROFILE.repak_profile())
        .key(make_aes_key()?)
        .compression(options.compression.repak());
    if let Some(level) = options.oodle_level {
        builder = builder.oodle_level(level);
    }
    let mut writer = builder.writer(
        BufWriter::new(file),
        RIVALS_PROFILE.pak_version(),
        mount.to_string(),
        None,
    );
    fill(&mut writer)?;
    writer.write_index().map_err(|e| e.to_string())?;
    Ok(())
}

/// `name`, a pak entry, as a path from the game root such as `Marvel/Content/...`. A pak names its
/// entries from its own mount point: the game root's `../../../` for nearly every mod and most of
/// the game's own paks, but deeper for one such as the translations pak, which mounts at
/// `../../../Marvel/Content/Localization/Game/` and holds `en/Game.locres`, or one UnrealPak
/// wrote, which mounts a pak at the folder all its files share.
pub(crate) fn from_game_root(mount: &str, name: &str) -> String {
    let name = strip_mount_prefix(name);
    strip_mount_prefix(&format!("{}/{name}", mount.trim_end_matches('/')))
}

/// What a mod's pak calls `name`: its path from the game root, but for one of the
/// [`UNSUPPORTED_ENTRIES`], a listing an older packer left at the pak's root and no file of the
/// mod's, which keeps its bare name wherever the pak mounts so it is known as what it is.
fn entry_path(mount: &str, name: &str) -> String {
    let bare = strip_mount_prefix(name);
    if UNSUPPORTED_ENTRIES
        .iter()
        .any(|unsupported| bare.eq_ignore_ascii_case(unsupported))
    {
        return bare;
    }
    from_game_root(mount, name)
}

/// Each entry of the pak `reader` opened, by its path from the game root, beside the name its
/// index gives it, which reading it takes.
fn entries(reader: &repak::PakReader) -> Vec<(String, String)> {
    let mount = reader.mount_point();
    reader
        .files()
        .into_iter()
        .map(|name| (entry_path(mount, &name), name))
        .collect()
}

/// Every entry the pak at `pak` holds, by its path from the game root.
pub fn list_files(pak: &Path) -> Result<Vec<String>, String> {
    Ok(entries(&open_pak(pak)?)
        .into_iter()
        .map(|(path, _)| path)
        .collect())
}

/// The name `entry`, a path from the game root or the index's own name for it, goes by in the
/// index of the pak `reader` opened.
fn index_name(reader: &repak::PakReader, entry: &str) -> Option<String> {
    let wanted = strip_mount_prefix(entry);
    entries(reader)
        .into_iter()
        .find(|(path, name)| path.eq_ignore_ascii_case(&wanted) || name == entry)
        .map(|(_, name)| name)
}

/// The bytes of `entry`, a path from the game root, in the pak at `pak`; `None` when it holds no
/// such entry.
pub fn read_one(pak: &Path, entry: &str) -> Result<Option<Vec<u8>>, String> {
    let reader = open_pak(pak)?;
    let Some(name) = index_name(&reader, entry) else {
        return Ok(None);
    };
    let mut file = BufReader::new(fs::File::open(pak).map_err(|e| e.to_string())?);
    let mut bytes = Vec::new();
    reader
        .read_file(&name, &mut file, &mut bytes)
        .map_err(|e| format!("{entry}: {e}"))?;
    Ok(Some(bytes))
}

/// `path`, a path from the game root, checked to stay inside a folder it is written under.
fn inside(path: &str) -> Result<&str, String> {
    if path.split(['/', '\\']).any(|part| part == "..") || Path::new(path).is_absolute() {
        return Err(format!("{path} would be written outside the folder"));
    }
    Ok(path)
}

/// The entries packers used to put in the pak beside an IoStore container, which the game does not
/// support and current packers no longer write: a listing of the container's files, and a list of
/// patched files. Neither is any mod's content.
pub const UNSUPPORTED_ENTRIES: [&str; 2] = ["chunknames", "patched_files"];

/// The files Windows and macOS leave in folders, which a mod packed from a folder picks up: a
/// folder's view settings and its thumbnails. None is any mod's content, and a `desktop.ini` in a
/// mod's pak has been reported to get players kicked.
pub const SYSTEM_FILES: [&str; 3] = ["desktop.ini", "Thumbs.db", ".DS_Store"];

/// Whether `entry`, a path in any folder, is one of [`SYSTEM_FILES`], in any case.
pub fn is_system_file(entry: &str) -> bool {
    let name = entry.rsplit(['/', '\\']).next().unwrap_or(entry);
    SYSTEM_FILES
        .iter()
        .any(|system| name.eq_ignore_ascii_case(system))
}

/// Whether `entry`, a pak entry with its mount prefix or without, does not belong in the pak beside
/// a container: one of [`UNSUPPORTED_ENTRIES`], or one of [`SYSTEM_FILES`] in any folder.
pub fn is_unsupported_entry(entry: &str) -> bool {
    let name = strip_mount_prefix(entry);
    UNSUPPORTED_ENTRIES
        .iter()
        .any(|unsupported| name.eq_ignore_ascii_case(unsupported))
        || is_system_file(&name)
}

/// Why `entry`, one [`is_unsupported_entry`] takes, does not belong in the pak beside a container.
pub fn unsupported_why(entry: &str) -> &'static str {
    if is_system_file(entry) {
        "a file Windows or macOS leaves in a folder, which is no part of the mod"
    } else {
        "the game does not support it in the pak beside a container"
    }
}

/// The entries of the pak at `pak` that `matches` takes.
fn entries_matching(pak: &Path, matches: &dyn Fn(&str) -> bool) -> Result<Vec<String>, String> {
    Ok(list_files(pak)?
        .into_iter()
        .filter(|name| matches(name))
        .collect())
}

/// The entries the pak at `pak` holds that do not belong beside a container, as
/// [`is_unsupported_entry`] says.
pub fn unsupported_entries(pak: &Path) -> Result<Vec<String>, String> {
    entries_matching(pak, &is_unsupported_entry)
}

/// The [`SYSTEM_FILES`] the pak at `pak` holds, in any folder.
pub fn system_files(pak: &Path) -> Result<Vec<String>, String> {
    entries_matching(pak, &is_system_file)
}

/// Writes the pak at `pak` again without the entries that do not belong beside a container, as
/// [`is_unsupported_entry`] says, and gives back what it took out. A pak holding none is left as it
/// is, and one holding nothing else becomes an empty pak, as the game's own beside a container are.
pub fn strip_unsupported(pak: &Path, options: &PakOptions) -> Result<Vec<String>, String> {
    strip_matching(pak, options, &is_unsupported_entry)
}

/// Writes the plain pak at `pak` again without its [`SYSTEM_FILES`], and gives back what it took
/// out. Every other entry stays, since the game reads a plain pak's files as they are.
pub fn strip_system_files(pak: &Path, options: &PakOptions) -> Result<Vec<String>, String> {
    strip_matching(pak, options, &is_system_file)
}

/// Writes the pak at `pak` again without the entries `matches` takes, its other files kept, those
/// it stores raw stored raw again and the rest compressed as `options` say. A pak holding none is
/// left as it is.
fn strip_matching(
    pak: &Path,
    options: &PakOptions,
    matches: &dyn Fn(&str) -> bool,
) -> Result<Vec<String>, String> {
    let found = entries_matching(pak, matches)?;
    if found.is_empty() {
        return Ok(found);
    }
    let raw: HashSet<String> = uncompressed_entries(pak)?.into_iter().collect();
    let kept = read_files(pak, &|name| !matches(name))?;
    let name = pak
        .file_name()
        .ok_or_else(|| format!("{} names no file", pak.display()))?
        .to_string_lossy()
        .into_owned();
    // Written beside it first, so a failed write leaves the pak as it was.
    let staged = pak.with_file_name(format!("{name}.stripped"));
    let written = write_with(&staged, options, |writer| {
        for (path, bytes) in kept {
            let compress = !raw.contains(&path);
            writer
                .write_file(&path, compress, bytes)
                .map_err(|e| format!("{path}: {e}"))?;
        }
        Ok(())
    });
    if let Err(e) = written {
        let _ = fs::remove_file(&staged);
        return Err(e);
    }
    fs::rename(&staged, pak).map_err(|e| {
        let _ = fs::remove_file(&staged);
        format!("replace {}: {e}", pak.display())
    })?;
    Ok(found)
}

/// The entries the pak stores uncompressed, by their paths from the game root, so a rebuild can
/// store them so again.
pub fn uncompressed_entries(pak: &Path) -> Result<Vec<String>, String> {
    let reader = open_pak(pak)?;
    let mut out = Vec::new();
    for (path, name) in entries(&reader) {
        let entry = reader.get_file_entry(&name).map_err(|e| e.to_string())?;
        if entry.compression_slot.is_none() {
            out.push(path);
        }
    }
    Ok(out)
}

/// Writes the entries `keep` takes, by their paths from the game root, into `out` at those paths.
/// They are read in the order the file holds them, so the pak is read straight through. Returns
/// the paths written.
pub fn extract(pak: &Path, out: &Path, keep: &dyn Fn(&str) -> bool) -> Result<Vec<String>, String> {
    fs::create_dir_all(out).map_err(|e| format!("create {}: {e}", out.display()))?;
    let reader = open_pak(pak)?;
    let mount = reader.mount_point().to_string();
    let mut file = BufReader::new(fs::File::open(pak).map_err(|e| e.to_string())?);
    let mut written = Vec::new();
    for name in reader.files_by_offset() {
        let stripped = entry_path(&mount, name);
        if !keep(&stripped) {
            continue;
        }
        let dest = out.join(inside(&stripped)?);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        let mut target =
            fs::File::create(&dest).map_err(|e| format!("create {}: {e}", dest.display()))?;
        reader
            .read_file(name, &mut file, &mut target)
            .map_err(|e| format!("{stripped}: {e}"))?;
        written.push(stripped);
    }
    Ok(written)
}

/// Writes one entry, by its path from the game root or as the index names it, to `dest`.
pub fn extract_one(pak: &Path, entry: &str, dest: &Path) -> Result<(), String> {
    let reader = open_pak(pak)?;
    let name =
        index_name(&reader, entry).ok_or_else(|| format!("{entry} is not in {}", pak.display()))?;
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let mut file = BufReader::new(fs::File::open(pak).map_err(|e| e.to_string())?);
    let mut target =
        fs::File::create(dest).map_err(|e| format!("create {}: {e}", dest.display()))?;
    reader
        .read_file(&name, &mut file, &mut target)
        .map_err(|e| format!("{entry}: {e}"))
}

/// The entries `keep` takes, by their paths from the game root, with their bytes.
pub fn read_files(
    pak: &Path,
    keep: &dyn Fn(&str) -> bool,
) -> Result<Vec<(String, Vec<u8>)>, String> {
    let reader = open_pak(pak)?;
    let mount = reader.mount_point().to_string();
    let mut file = BufReader::new(fs::File::open(pak).map_err(|e| e.to_string())?);
    let mut out = Vec::new();
    for name in reader.files_by_offset() {
        let stripped = entry_path(&mount, name);
        if !keep(&stripped) {
            continue;
        }
        let mut bytes = Vec::new();
        reader
            .read_file(name, &mut file, &mut bytes)
            .map_err(|e| format!("{stripped}: {e}"))?;
        out.push((stripped, bytes));
    }
    Ok(out)
}

/// Writes a pak at `out` holding `files`, each its path inside the pak and its bytes, all
/// compressed. With none it is an empty pak.
pub fn write_files(
    out: &Path,
    files: Vec<(String, Vec<u8>)>,
    options: &PakOptions,
) -> Result<(), String> {
    write_with(out, options, |writer| {
        for (path, bytes) in files {
            writer
                .write_file(&path, true, bytes)
                .map_err(|e| format!("{path}: {e}"))?;
        }
        Ok(())
    })
}

/// Writes a pak at `out` from files on disk, each `(path inside the pak, file, compress)`, read
/// one at a time so the pak never has to fit in memory. `compress = false` stores an entry raw,
/// as a source pak may have.
pub fn write_streaming(
    out: &Path,
    entries: Vec<(String, PathBuf, bool)>,
    options: &PakOptions,
) -> Result<(), String> {
    write_with(out, options, |writer| {
        for (path, source, compress) in entries {
            let bytes = fs::read(&source).map_err(|e| format!("read {}: {e}", source.display()))?;
            writer
                .write_file(&path, compress, bytes)
                .map_err(|e| format!("{path}: {e}"))?;
        }
        Ok(())
    })
}

/// The files under `input` with the path a pak gives each, leaving out `out` should it sit there
/// and the [`SYSTEM_FILES`] the folders hold.
fn files_under(input: &Path, out: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    if !input.is_dir() {
        return Err(format!("{} is not a folder", input.display()));
    }
    let own = out.canonicalize().ok();
    let mut files = Vec::new();
    for entry in WalkDir::new(input).into_iter().filter_map(Result::ok) {
        let path = entry.path();
        if !entry.file_type().is_file()
            || is_system_file(&entry.file_name().to_string_lossy())
            || own.is_some() && path.canonicalize().ok().as_ref() == own.as_ref()
        {
            continue;
        }
        let relative = path
            .strip_prefix(input)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        files.push((relative, path.to_path_buf()));
    }
    files.sort();
    Ok(files)
}

/// Packs every file under `input` but the [`SYSTEM_FILES`] into a pak at `out`, by its path below
/// `input`. A folder with nothing in it is refused. Returns the paths that went in.
pub fn pack_dir(input: &Path, out: &Path, options: &PakOptions) -> Result<Vec<String>, String> {
    let files = files_under(input, out)?;
    if files.is_empty() {
        return Err(format!("{} holds no files to pack", input.display()));
    }
    let names = files.iter().map(|(path, _)| path.clone()).collect();
    write_streaming(
        out,
        files
            .into_iter()
            .map(|(path, source)| (path, source, true))
            .collect(),
        options,
    )?;
    Ok(names)
}

/// [`pack_dir`], with an empty folder making an empty pak: what an unpacked pak becomes again.
pub(crate) fn repack_dir(input: &Path, out: &Path, options: &PakOptions) -> Result<(), String> {
    let files = files_under(input, out)?;
    write_streaming(
        out,
        files
            .into_iter()
            .map(|(path, source)| (path, source, true))
            .collect(),
        options,
    )
}

/// The entries of `pak`, by their paths from the game root, as a set to look names up in.
pub fn entry_set(pak: &Path) -> Result<HashSet<String>, String> {
    Ok(list_files(pak)?.into_iter().collect())
}

/// A pak another packer wrote at `mount`, holding `files` by their names under it, for tests.
#[cfg(test)]
#[allow(clippy::expect_used)]
pub(crate) fn mounted_pak(out: &Path, mount: &str, files: &[(&str, &[u8])]) {
    let zlib = PakOptions {
        compression: PackCompression::Zlib,
        oodle_level: None,
    };
    write_mounted(out, mount, &zlib, |writer| {
        for (name, bytes) in files {
            writer
                .write_file(name, true, bytes)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    })
    .expect("write the pak");
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
        let dir = std::env::temp_dir().join(format!("rivals-plain-{tag}-{stamp}"));
        fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    /// Zlib, which needs no Oodle library, so these run anywhere.
    const ZLIB: PakOptions = PakOptions {
        compression: PackCompression::Zlib,
        oodle_level: None,
    };

    /// A pak mounted deeper than the game root, as UnrealPak mounts one at the folder its files
    /// share, names its entries from the game root all the same: listed, read, unpacked one by
    /// one or whole, and written back at the root.
    #[test]
    fn a_pak_mounted_deeper_names_its_entries_from_the_game_root() {
        let dir = scratch("mounted");
        let pak = dir.join("Deep_9999999_P.pak");
        mounted_pak(
            &pak,
            "../../../Marvel/Content/WwiseAudio/",
            &[("English/vo.bnk", b"bank"), ("ui.bnk", b"ui")],
        );
        let english = "Marvel/Content/WwiseAudio/English/vo.bnk";
        let ui = "Marvel/Content/WwiseAudio/ui.bnk";
        let mut listed = list_files(&pak).unwrap();
        listed.sort();
        assert_eq!(listed, [english, ui]);
        assert!(entry_set(&pak).unwrap().contains(english));
        assert_eq!(
            read_one(&pak, english).unwrap().as_deref(),
            Some(&b"bank"[..])
        );
        assert_eq!(read_one(&pak, "Marvel/Content/Other.bnk").unwrap(), None);

        let one = dir.join("one.bnk");
        extract_one(&pak, ui, &one).unwrap();
        assert_eq!(fs::read(&one).unwrap(), b"ui");
        let out = dir.join("out");
        extract(&pak, &out, &|_| true).unwrap();
        assert_eq!(fs::read(out.join(english)).unwrap(), b"bank");

        let again = dir.join("Again_9999999_P.pak");
        write_files(&again, read_files(&pak, &|_| true).unwrap(), &ZLIB).unwrap();
        let mut listed = list_files(&again).unwrap();
        listed.sort();
        assert_eq!(listed, [english, ui]);
        let _ = fs::remove_dir_all(&dir);
    }

    /// An entry whose name climbs above the game root is never written outside the folder.
    #[test]
    fn an_entry_climbing_out_of_the_folder_is_refused() {
        let dir = scratch("climb");
        let pak = dir.join("Climb_9999999_P.pak");
        mounted_pak(&pak, "../../../", &[("../../escape.txt", b"x")]);
        let err = extract(&pak, &dir.join("out"), &|_| true).unwrap_err();
        assert!(err.contains("outside the folder"), "{err}");
        assert!(!dir.join("escape.txt").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    /// The entries the game does not support beside a container are known with their mount prefix
    /// or without and in any case; stripping takes them out and keeps the rest, a pak holding
    /// nothing else becomes empty, and one holding none is left as it was.
    #[test]
    fn the_entries_the_game_does_not_support_are_stripped_and_the_rest_kept() {
        assert!(is_unsupported_entry("chunknames"));
        assert!(is_unsupported_entry("../../../chunknames"));
        assert!(is_unsupported_entry("Patched_Files"));
        assert!(!is_unsupported_entry("Marvel/chunknames"));
        let dir = scratch("unsupported");
        let pak = dir.join("Mod_P.pak");
        let ini = ("Marvel/Config/a.ini".to_string(), b"a".to_vec());
        write_files(
            &pak,
            vec![
                ini.clone(),
                ("chunknames".into(), b"Marvel/Content/A.uasset".to_vec()),
                ("patched_files".into(), b"x".to_vec()),
            ],
            &ZLIB,
        )
        .expect("write");
        let mut found = unsupported_entries(&pak).expect("list");
        found.sort();
        assert_eq!(found, ["chunknames", "patched_files"]);
        let mut stripped = strip_unsupported(&pak, &ZLIB).expect("strip");
        stripped.sort();
        assert_eq!(stripped, found);
        assert_eq!(read_files(&pak, &|_| true).expect("read"), vec![ini]);
        let before = fs::read(&pak).expect("bytes");
        assert!(strip_unsupported(&pak, &ZLIB).expect("again").is_empty());
        assert_eq!(fs::read(&pak).expect("bytes"), before, "nothing to strip");
        let bare = dir.join("Bare_P.pak");
        write_files(&bare, vec![("chunknames".into(), Vec::new())], &ZLIB).expect("write");
        strip_unsupported(&bare, &ZLIB).expect("strip");
        assert!(list_files(&bare).expect("list").is_empty());
        assert!(!dir.join("Bare_P.pak.stripped").exists());
        fs::remove_dir_all(&dir).ok();
    }

    /// The files folders leave are known in any folder and any case, and belong beside no
    /// container. A plain pak loses them and keeps the rest, the listings an older packer left
    /// and its raw entries raw among them, and packing a folder leaves them out.
    #[test]
    fn the_files_folders_leave_are_stripped_and_never_packed() {
        assert!(is_system_file("desktop.ini"));
        assert!(is_system_file("Marvel/Content/WwiseAudio/DESKTOP.INI"));
        assert!(is_system_file("Marvel\\Content\\Thumbs.db"));
        assert!(is_system_file("Marvel/.DS_Store"));
        assert!(!is_system_file("Marvel/Content/desktop.ini.bak"));
        assert!(is_unsupported_entry("../../../Marvel/Content/desktop.ini"));
        assert_eq!(
            unsupported_why("Marvel/Thumbs.db"),
            "a file Windows or macOS leaves in a folder, which is no part of the mod"
        );

        let dir = scratch("system-files");
        let folder = dir.join("in");
        for (rel, body) in [
            ("Marvel/Content/WwiseAudio/Skin.bnk", "bank"),
            ("Marvel/Content/WwiseAudio/Skin.wem", "media"),
            ("Marvel/Content/WwiseAudio/desktop.ini", "[.ShellClassInfo]"),
            ("Marvel/Thumbs.db", "thumbs"),
            ("chunknames", "listing"),
        ] {
            let path = folder.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, body).unwrap();
        }
        let packed = dir.join("Packed_P.pak");
        assert_eq!(
            pack_dir(&folder, &packed, &ZLIB).expect("pack"),
            [
                "Marvel/Content/WwiseAudio/Skin.bnk",
                "Marvel/Content/WwiseAudio/Skin.wem",
                "chunknames"
            ]
        );

        let pak = dir.join("Plain_P.pak");
        let file = |rel: &str| {
            (
                rel.to_string(),
                folder.join(rel),
                rel != "Marvel/Content/WwiseAudio/Skin.wem",
            )
        };
        write_streaming(
            &pak,
            vec![
                file("Marvel/Content/WwiseAudio/Skin.bnk"),
                file("Marvel/Content/WwiseAudio/Skin.wem"),
                file("Marvel/Content/WwiseAudio/desktop.ini"),
                file("Marvel/Thumbs.db"),
                file("chunknames"),
            ],
            &ZLIB,
        )
        .expect("write");
        let mut found = system_files(&pak).expect("list");
        found.sort();
        assert_eq!(
            found,
            ["Marvel/Content/WwiseAudio/desktop.ini", "Marvel/Thumbs.db"]
        );
        let mut stripped = strip_system_files(&pak, &ZLIB).expect("strip");
        stripped.sort();
        assert_eq!(stripped, found);
        let mut kept = list_files(&pak).expect("list");
        kept.sort();
        assert_eq!(
            kept,
            [
                "Marvel/Content/WwiseAudio/Skin.bnk",
                "Marvel/Content/WwiseAudio/Skin.wem",
                "chunknames"
            ]
        );
        assert_eq!(
            uncompressed_entries(&pak).expect("raw"),
            ["Marvel/Content/WwiseAudio/Skin.wem"],
            "an entry stored raw stays raw"
        );
        fs::remove_dir_all(&dir).ok();
    }

    /// Files written to a pak list and extract back byte for byte, under the paths given.
    #[test]
    fn written_files_list_and_extract_back_the_same() {
        let dir = scratch("roundtrip");
        let pak = dir.join("Mod_P.pak");
        let files = vec![
            (
                "Marvel/Config/DefaultEngine.ini".to_string(),
                b"[Core]\nA=1\n".to_vec(),
            ),
            (
                "Marvel/Content/Movies/intro.bk2".to_string(),
                vec![7u8; 70_000],
            ),
        ];
        write_files(&pak, files.clone(), &ZLIB).expect("write");
        let mut listed: Vec<String> = list_files(&pak)
            .unwrap()
            .iter()
            .map(|n| strip_mount_prefix(n))
            .collect();
        listed.sort();
        assert_eq!(
            listed,
            files.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>()
        );

        let out = dir.join("out");
        let mut written = extract(&pak, &out, &|_| true).expect("extract");
        written.sort();
        assert_eq!(written, listed);
        for (name, bytes) in &files {
            assert_eq!(&fs::read(out.join(name)).unwrap(), bytes, "{name}");
        }
        let only = extract(&pak, &dir.join("only"), &|name| name.ends_with(".ini")).unwrap();
        assert_eq!(only, vec!["Marvel/Config/DefaultEngine.ini".to_string()]);
        assert_eq!(
            read_files(&pak, &|name| name.ends_with(".bk2"))
                .unwrap()
                .len(),
            1
        );
        fs::remove_dir_all(&dir).ok();
    }

    /// A folder packs into a pak beside or inside it without taking the pak itself in, and an
    /// empty folder is refused rather than written as an empty pak.
    #[test]
    fn a_folder_packs_without_its_own_output() {
        let dir = scratch("pack");
        fs::create_dir_all(dir.join("Marvel/Config")).unwrap();
        fs::write(dir.join("Marvel/Config/a.ini"), b"a").unwrap();
        let pak = dir.join("Out_P.pak");
        fs::write(&pak, b"stale").unwrap();
        assert_eq!(
            pack_dir(&dir, &pak, &ZLIB).expect("pack"),
            vec!["Marvel/Config/a.ini".to_string()]
        );
        assert_eq!(
            entry_set(&pak).unwrap(),
            HashSet::from(["Marvel/Config/a.ini".to_string()])
        );

        let empty = scratch("empty");
        assert!(pack_dir(&empty, &empty.join("E_P.pak"), &ZLIB).is_err());
        repack_dir(&empty, &empty.join("E_P.pak"), &ZLIB).expect("an empty pak");
        assert!(list_files(&empty.join("E_P.pak")).unwrap().is_empty());
        fs::remove_dir_all(&dir).ok();
        fs::remove_dir_all(&empty).ok();
    }

    /// An entry stored raw is listed as one, so a rebuild can store it raw again.
    #[test]
    fn an_entry_stored_raw_is_listed() {
        let dir = scratch("raw");
        fs::write(dir.join("a.bin"), vec![1u8; 4096]).unwrap();
        fs::write(dir.join("b.bin"), vec![2u8; 4096]).unwrap();
        let pak = dir.join("Raw_P.pak");
        write_streaming(
            &pak,
            vec![
                ("a.bin".into(), dir.join("a.bin"), true),
                ("b.bin".into(), dir.join("b.bin"), false),
            ],
            &ZLIB,
        )
        .expect("write");
        assert_eq!(
            uncompressed_entries(&pak).unwrap(),
            vec!["b.bin".to_string()]
        );
        let one = dir.join("one/b.bin");
        extract_one(&pak, "b.bin", &one).expect("extract one");
        assert_eq!(fs::read(one).unwrap(), vec![2u8; 4096]);
        fs::remove_dir_all(&dir).ok();
    }
}
