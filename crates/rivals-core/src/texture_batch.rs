//! Exports and replaces the textures of many packages at once, encoding them in parallel and
//! writing a mod once.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use rayon::prelude::*;
use retoc::iostore::IoStoreTrait;
use retoc::script_objects::FPackageObjectIndex;
use retoc::version::EngineVersion;
use rivals_uasset::{ExportRow, Mappings, PackageEdits};
use serde::Serialize;

use crate::asset::{self, AssetSource, PACKAGES_PER_CONVERTER, PackageConverter, PackageEntry};
use crate::asset_edit::{AssetEditRequest, SaveOptions, SaveTarget};
use crate::image_file::ImageKind;
use crate::pak::iostore_out::{self, BuiltPackage, PackageBytes};
use crate::texture::{ImportReport, PixelFormat, Quality};

const ENGINE: EngineVersion = EngineVersion::UE5_3;

/// What a batch is doing, for a caller showing its progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchPhase {
    /// Opening the containers, listing their packages, and finding the images.
    Listing,
    /// Reading each package's header to find the ones holding textures.
    Headers,
    /// Saving textures as images, or encoding images as textures, a package at a time.
    Textures,
    /// Putting the packages into the mod.
    Writing,
}

/// Where a batch reads its packages.
#[derive(Debug, Clone, Copy)]
pub struct TextureSource<'a> {
    pub game_root: &'a str,
    /// A game container or a mod's `.utoc`; for `whole_game`, any of the game's own.
    pub container: &'a str,
    /// Every package the game ships, each from the container that wins it.
    pub whole_game: bool,
    /// Pieces of package paths, as `asset list --filter` matches them: a package any of them
    /// matches is kept, and none keeps every package.
    pub filters: &'a [String],
}

#[derive(Debug, Serialize)]
pub struct TextureFailure {
    pub entry: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub export: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    pub reason: String,
}

/// How long each part of a batch took, in seconds of wall-clock time.
#[derive(Debug, Default, Serialize)]
pub struct BatchTimings {
    pub listing: f64,
    /// Reading package headers to find the ones holding textures.
    pub headers: f64,
    /// Converting packages and saving or encoding their textures, in parallel.
    pub textures: f64,
    /// Of `textures`, how long the writer sat waiting for a package to write. Near all of it
    /// means encoding bounds the batch; near none, writing does.
    pub writer_waiting: f64,
    /// Carrying the mod's other chunks over and swapping the container in.
    pub writing: f64,
}

#[derive(Debug, Serialize)]
pub struct SavedTexture {
    pub entry: String,
    pub export: String,
    pub path: String,
    pub format: String,
    pub mip: usize,
    pub mips: usize,
    pub width: u32,
    pub height: u32,
    /// Why the image is of another kind than asked for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TexturesOut {
    /// Packages the filters kept.
    pub matched: usize,
    /// Of them, the ones whose header names a texture.
    pub packages: usize,
    pub saved: Vec<SavedTexture>,
    pub failed: Vec<TextureFailure>,
    /// Virtual textures, whose tiles this does not read. The game ships only the engine's
    /// placeholders, so nothing is missed.
    pub skipped: Vec<TextureFailure>,
    pub cancelled: bool,
    pub timings: BatchTimings,
}

#[derive(Debug, Serialize)]
pub struct GivenTexture {
    pub entry: String,
    pub image: String,
    #[serde(flatten)]
    pub report: ImportReport,
}

#[derive(Debug, Serialize)]
pub struct TexturesIn {
    /// Image files found in the folder.
    pub images: usize,
    /// Packages the images were matched to.
    pub packages: usize,
    pub given: Vec<GivenTexture>,
    pub failed: Vec<TextureFailure>,
    /// Packages written that the mod already carried.
    pub replaced: usize,
    /// Whether packages the mod carried were built on rather than started again from the source.
    pub layered: bool,
    /// Brushes in the mod drawing a resized texture at a size they store, which it keeps.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub brushes: Vec<String>,
    /// The mod written, or `None` for a dry run, a cancelled one, or one that wrote nothing.
    pub written: Option<PathBuf>,
    pub dry_run: bool,
    pub cancelled: bool,
    pub timings: BatchTimings,
}

fn seconds(since: Instant) -> f64 {
    since.elapsed().as_secs_f64()
}

/// The packages `source` holds that its filters keep, in path order, read through a store opened
/// around `open_as` when one is given, so its copies win over the source's.
fn listing(
    source: &TextureSource<'_>,
    open_as: Option<&Path>,
) -> Result<(Arc<dyn IoStoreTrait>, Vec<PackageEntry>), String> {
    let (store, packages) = if source.whole_game {
        let (base, packages) = asset::list_game_packages(source.game_root)?;
        let store = match open_as {
            Some(utoc) => {
                crate::pak::containers::open_store_for(
                    &crate::paths::paks_dir(source.game_root),
                    &utoc.to_string_lossy(),
                )?
                .0
            }
            None => base,
        };
        (store, packages)
    } else {
        let utoc = Path::new(
            source
                .container
                .strip_suffix(".disabled")
                .unwrap_or(source.container),
        );
        if !utoc
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("utoc"))
        {
            return Err(format!(
                "{} is not an IoStore container: textures are read in batches from the game or \
                 an IoStore mod, so convert a plain pak first (`mods convert`)",
                source.container
            ));
        }
        let open_as = open_as.map_or_else(
            || source.container.to_string(),
            |utoc| utoc.to_string_lossy().into_owned(),
        );
        asset::list_packages_via(source.game_root, &open_as, source.container)?
    };
    let filter = asset::PathFilter::any(source.filters.iter().map(String::as_str));
    let mut kept: Vec<PackageEntry> = packages
        .into_iter()
        .filter(|(_, path)| filter.matches(path))
        .collect();
    kept.sort_by(|left, right| left.1.cmp(&right.1));
    Ok((store, kept))
}

/// The packages whose header names an export of a texture class, in the order given. One whose
/// header does not read is kept, so converting it says why.
fn holding_textures(
    store: &dyn IoStoreTrait,
    packages: Vec<PackageEntry>,
    cancel: &AtomicBool,
    progress: &(dyn Fn(BatchPhase, usize, usize) + Sync),
) -> Vec<PackageEntry> {
    let classes: Vec<FPackageObjectIndex> = rivals_uasset::TEXTURE_CLASSES
        .iter()
        .map(|class| FPackageObjectIndex::create_script_import(&format!("/Script/Engine.{class}")))
        .collect();
    let versions = (ENGINE.toc_version(), ENGINE.container_header_version());
    let total = packages.len();
    let done = AtomicUsize::new(0);
    packages
        .into_par_iter()
        .filter(|(id, _)| {
            if cancel.load(Ordering::Relaxed) {
                return false;
            }
            let keep = crate::game_search::header_names_class(store, *id, versions, &classes)
                .unwrap_or(true);
            let now = done.fetch_add(1, Ordering::Relaxed) + 1;
            if now.is_multiple_of(256) || now == total {
                progress(BatchPhase::Headers, now, total);
            }
            keep
        })
        .collect()
}

fn package_stem(entry: &str) -> &str {
    entry
        .strip_suffix(".uasset")
        .or_else(|| entry.strip_suffix(".umap"))
        .unwrap_or(entry)
}

/// Where a texture of package `entry` goes under `dir`: at the package's path, with `suffix`
/// after it for a texture the package is not named for, as [`texture_suffix`] gives it.
pub fn image_path(dir: &Path, entry: &str, suffix: Option<&str>, kind: ImageKind) -> PathBuf {
    let stem = package_stem(entry);
    let name = match suffix {
        Some(suffix) => format!("{stem}.{suffix}.{}", kind.extension()),
        None => format!("{stem}.{}", kind.extension()),
    };
    let mut path = dir.to_path_buf();
    path.extend(name.split('/'));
    path
}

/// What texture `row` is called after its package's path in an image's name: nothing for the
/// texture named for its package, its name when no other texture in the package has it, else its
/// path below the package, its outers' names before its own, as a Niagara system's textures are
/// told apart.
fn texture_suffix(rows: &[ExportRow], row: &ExportRow, entry: &str) -> Option<String> {
    let stem = package_stem(entry);
    let package = stem.rsplit('/').next().unwrap_or(stem);
    if row.object_name.eq_ignore_ascii_case(package) {
        return None;
    }
    let twins = rows
        .iter()
        .filter(|other| {
            rivals_uasset::is_texture_class(&other.class_name)
                && other.object_name.eq_ignore_ascii_case(&row.object_name)
        })
        .count();
    if twins <= 1 {
        return Some(row.object_name.clone());
    }
    let below = row
        .path
        .split_once('.')
        .map_or(row.path.as_str(), |(_, below)| below);
    let below = below
        .strip_prefix(package)
        .and_then(|rest| rest.strip_prefix(':'))
        .unwrap_or(below);
    Some(below.replace(':', "."))
}

/// The texture an image names in a package by `suffix`, as [`texture_suffix`] names them, or
/// with none the texture named for the package, else its first.
fn texture_named(rows: &[ExportRow], entry: &str, suffix: Option<&str>) -> Result<u32, String> {
    let textures: Vec<&ExportRow> = rows
        .iter()
        .filter(|row| rivals_uasset::is_texture_class(&row.class_name))
        .collect();
    let named = |row: &&&ExportRow| {
        texture_suffix(rows, row, entry)
            .as_deref()
            .zip(suffix)
            .is_some_and(|(name, wanted)| name.eq_ignore_ascii_case(wanted))
            || suffix.is_some_and(|wanted| row.object_name.eq_ignore_ascii_case(wanted))
    };
    let found = match suffix {
        Some(_) => textures.iter().find(named),
        None => textures
            .iter()
            .find(|row| texture_suffix(rows, row, entry).is_none())
            .or(textures.first()),
    };
    found.map(|row| row.index).ok_or_else(|| match suffix {
        Some(name) => format!("the package holds no texture named {name}"),
        None => "the package holds no texture".to_string(),
    })
}

/// The kind a texture goes out as when `asked` for: OpenEXR for one holding high dynamic range
/// values asked for as an 8-bit kind, PNG for an 8-bit one asked for as OpenEXR.
fn kind_for(asked: ImageKind, hdr: bool) -> (ImageKind, Option<String>) {
    if asked.holds(hdr) {
        (asked, None)
    } else if hdr {
        (
            ImageKind::Exr,
            Some(format!(
                "its values are high dynamic range, which a {} cannot hold, so it went out as \
                 OpenEXR",
                asked.name()
            )),
        )
    } else {
        (
            ImageKind::Png,
            Some("its values are 8-bit, so it went out as a PNG".to_string()),
        )
    }
}

/// Saves every texture of the packages `source` holds that its filters keep under `out`, each as
/// an image of `kind` at the package's path: its mip `mip`, or the largest the package has. The
/// packages are read on every thread the caller's pool has, a package at a time each, so the
/// memory it takes grows with the threads rather than the batch. A texture that cannot go out is
/// listed with why, and the rest go on.
pub fn export_textures(
    source: &TextureSource<'_>,
    mappings: Option<&Mappings>,
    out: &Path,
    kind: ImageKind,
    mip: Option<usize>,
    cancel: &AtomicBool,
    progress: &(dyn Fn(BatchPhase, usize, usize) + Sync),
) -> Result<TexturesOut, String> {
    if !kind.writes() {
        return Err(
            "textures do not go out as JPEG, which would lose detail: choose PNG, TGA, BMP, DDS \
             or OpenEXR"
                .into(),
        );
    }
    let mut timings = BatchTimings::default();
    let started = Instant::now();
    progress(BatchPhase::Listing, 0, 0);
    let (store, packages) = listing(source, None)?;
    let matched = packages.len();
    timings.listing = seconds(started);

    let started = Instant::now();
    let packages = holding_textures(&*store, packages, cancel, progress);
    timings.headers = seconds(started);

    let started = Instant::now();
    let total = packages.len();
    let done = AtomicUsize::new(0);
    let mut saved = Vec::new();
    let mut failed = Vec::new();
    // A converter is shared by every thread, and dropped after each run of packages, so its
    // caches of the headers read stay as small as one run's.
    for run in packages.chunks(PACKAGES_PER_CONVERTER) {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let converter = PackageConverter::new(&*store);
        let outcomes: Vec<(Vec<SavedTexture>, Vec<TextureFailure>)> = run
            .par_iter()
            .with_max_len(1)
            .map(|(id, entry)| {
                if cancel.load(Ordering::Relaxed) {
                    return (Vec::new(), Vec::new());
                }
                let outcome =
                    export_package(source, mappings, &converter, *id, entry, out, kind, mip);
                let now = done.fetch_add(1, Ordering::Relaxed) + 1;
                if now.is_multiple_of(8) || now == total {
                    progress(BatchPhase::Textures, now, total);
                }
                outcome
            })
            .collect();
        for (here, wrong) in outcomes {
            saved.extend(here);
            failed.extend(wrong);
        }
    }
    timings.textures = seconds(started);
    let (skipped, failed) = failed
        .into_iter()
        .partition(|failure: &TextureFailure| failure.reason == rivals_uasset::VIRTUAL_TEXTURE);
    Ok(TexturesOut {
        matched,
        packages: total,
        saved,
        failed,
        skipped,
        cancelled: cancel.load(Ordering::Relaxed),
        timings,
    })
}

/// Saves the textures of one package.
#[allow(clippy::too_many_arguments)]
fn export_package(
    source: &TextureSource<'_>,
    mappings: Option<&Mappings>,
    converter: &PackageConverter<'_>,
    id: retoc::FPackageId,
    entry: &str,
    out: &Path,
    asked: ImageKind,
    mip: Option<usize>,
) -> (Vec<SavedTexture>, Vec<TextureFailure>) {
    let failure = |export: Option<&str>, reason: String| TextureFailure {
        entry: entry.to_string(),
        export: export.map(str::to_string),
        image: None,
        reason,
    };
    let read = converter.convert(id, entry).and_then(|loaded| {
        let rows = rivals_uasset::export_rows(&crate::asset_edit::bundle_of(&loaded))?;
        Ok((loaded, rows))
    });
    let (loaded, rows) = match read {
        Ok(read) => read,
        Err(reason) => return (Vec::new(), vec![failure(None, reason)]),
    };
    let request = AssetEditRequest {
        game_root: source.game_root,
        container: source.container,
        entry,
        kind: AssetSource::Utoc,
        mod_name: "",
        changes: PackageEdits::default(),
    };
    let mut saved = Vec::new();
    let mut failed = Vec::new();
    for row in rows
        .iter()
        .filter(|row| rivals_uasset::is_texture_class(&row.class_name))
    {
        let one = || -> Result<SavedTexture, String> {
            let layout = crate::texture::layout_of(&request, mappings, &loaded, row.index)?;
            let platform = layout.platform()?;
            let (kind, note) = kind_for(asked, PixelFormat::of(&platform.format)?.is_hdr());
            let file = crate::texture::texture_file(&loaded, &row.object_name, &layout, mip, kind)?;
            let suffix = texture_suffix(&rows, row, entry);
            let path = image_path(out, entry, suffix.as_deref(), kind);
            if let Some(folder) = path.parent() {
                std::fs::create_dir_all(folder)
                    .map_err(|e| format!("Could not create {}: {e}", folder.display()))?;
            }
            std::fs::write(&path, &file.bytes)
                .map_err(|e| format!("Could not write {}: {e}", path.display()))?;
            Ok(SavedTexture {
                entry: entry.to_string(),
                export: row.object_name.clone(),
                path: path.to_string_lossy().into_owned(),
                format: platform.format.clone(),
                mip: file.mip,
                mips: file.mips,
                width: file.width,
                height: file.height,
                note,
            })
        };
        match one() {
            Ok(texture) => saved.push(texture),
            Err(reason) => failed.push(failure(Some(&row.object_name), reason)),
        }
    }
    (saved, failed)
}

/// What [`import_textures`] reads.
pub struct TextureImports<'a> {
    pub source: TextureSource<'a>,
    /// The folder of images, each at its texture's package path as [`export_textures`] writes
    /// them, or anywhere under it named as its package is when no other package is.
    pub images: &'a Path,
    pub mod_name: &'a str,
    pub quality: Quality,
    /// Encode, patch and check everything, and write nothing.
    pub dry_run: bool,
}

/// The image files under `dir`, by path.
fn image_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    if !dir.is_dir() {
        return Err(format!("{} is not a folder", dir.display()));
    }
    let mut files: Vec<PathBuf> = walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|item| item.file_type().is_file())
        .map(walkdir::DirEntry::into_path)
        .filter(|path| ImageKind::of_path(path).is_some())
        .collect();
    files.sort();
    Ok(files)
}

/// The packages a listing holds, by path and by name, lowercased.
struct PackageIndex {
    by_path: HashMap<String, usize>,
    by_name: HashMap<String, Vec<usize>>,
}

impl PackageIndex {
    fn of(packages: &[PackageEntry]) -> Self {
        let mut by_path = HashMap::with_capacity(packages.len());
        let mut by_name: HashMap<String, Vec<usize>> = HashMap::with_capacity(packages.len());
        for (at, (_, entry)) in packages.iter().enumerate() {
            let stem = package_stem(entry).to_ascii_lowercase();
            let name = stem.rsplit('/').next().unwrap_or(&stem).to_string();
            by_name.entry(name).or_default().push(at);
            by_path.insert(stem, at);
        }
        Self { by_path, by_name }
    }

    /// The package and texture an image names by `relative`, its path under the folder without
    /// its extension: a package's path, with what [`texture_suffix`] calls a texture after its
    /// first dot, else a package's name alone when no other package has it.
    fn resolve(&self, relative: &str) -> Result<(usize, Option<String>), String> {
        let relative = relative.replace('\\', "/");
        let (folder, file) = relative.rsplit_once('/').unwrap_or(("", &relative));
        let (package, suffix) = file
            .split_once('.')
            .map_or((file, None), |(package, suffix)| (package, Some(suffix)));
        let suffix = suffix.map(str::to_string);
        let path = if folder.is_empty() {
            package.to_ascii_lowercase()
        } else {
            format!("{folder}/{package}").to_ascii_lowercase()
        };
        if let Some(&at) = self.by_path.get(&path) {
            return Ok((at, suffix));
        }
        let name = package.to_ascii_lowercase();
        match self.by_name.get(&name).map(Vec::as_slice).unwrap_or(&[]) {
            [at] => Ok((*at, suffix)),
            [] => Err("no package the source holds is at this path or has this name".into()),
            many => Err(format!(
                "{} packages are named {package}: put the image at its package's path, as an \
                 export writes it",
                many.len()
            )),
        }
    }
}

/// The images given one package, each with the texture it names, if it names one.
type Images = Vec<(Option<String>, PathBuf)>;

/// One package's images, and what came of them.
struct Produced {
    built: Option<BuiltPackage>,
    given: Vec<GivenTexture>,
    failed: Vec<TextureFailure>,
}

/// Gives textures the pixels of the images in a folder, each named for its texture as
/// [`export_textures`] names them, and writes every package they change into the mod in one
/// container rewrite. Packages are read, their images encoded and the packages patched, checked
/// and converted on threads of a pool of their own, while this thread does nothing but write them:
/// at most two per thread wait to be written, so memory grows with the threads, not the batch.
///
/// A package the mod carries is built on, unless `options.replace` starts it again from the
/// source. An image that fails is listed with why, and the rest go in. Cancelled, the mod is left
/// as it was.
pub fn import_textures(
    request: &TextureImports<'_>,
    mappings: Option<&Mappings>,
    options: &SaveOptions,
    cancel: &AtomicBool,
    progress: &(dyn Fn(BatchPhase, usize, usize) + Sync),
) -> Result<TexturesIn, String> {
    let source = &request.source;
    if options.target != SaveTarget::IoStore {
        return Err(
            "A batch writes an IoStore mod, since the game reads packages only from a container"
                .into(),
        );
    }
    let utoc =
        crate::asset_edit::mod_pak_path(source.game_root, request.mod_name)?.with_extension("utoc");
    if !request.dry_run && utoc.with_extension("pak").is_file() && !utoc.is_file() {
        return Err(format!(
            "{} is a plain pak mod. Repack it in place as IoStore first, or choose another name.",
            utoc.with_extension("pak").display()
        ));
    }
    let into_source = crate::asset_edit::same_file(&utoc, source.container);
    let layered = !options.replace || into_source;
    let other_mod =
        !source.whole_game && crate::members::is_mod_container(source.game_root, source.container);
    if layered && !into_source && other_mod && utoc.is_file() {
        return Err(
            "reading one mod's textures into another can build on neither's copies: pass \
             --overwrite to start each package again from the source"
                .into(),
        );
    }
    let mut timings = BatchTimings::default();
    let started = Instant::now();
    progress(BatchPhase::Listing, 0, 0);
    let images = image_files(request.images)?;
    let open_as = (layered && !into_source && utoc.is_file()).then_some(utoc.as_path());
    let (store, packages) = listing(source, open_as)?;
    let index = PackageIndex::of(&packages);
    let mut failed = Vec::new();
    let mut groups: BTreeMap<usize, Images> = BTreeMap::new();
    for image in &images {
        let relative = image
            .strip_prefix(request.images)
            .unwrap_or(image)
            .with_extension("");
        match index.resolve(&relative.to_string_lossy()) {
            Ok((at, export)) => groups.entry(at).or_default().push((export, image.clone())),
            Err(reason) => failed.push(TextureFailure {
                entry: relative.to_string_lossy().replace('\\', "/"),
                export: None,
                image: Some(image.to_string_lossy().into_owned()),
                reason,
            }),
        }
    }
    let groups: Vec<(&PackageEntry, Images)> = groups
        .into_iter()
        .map(|(at, images)| (&packages[at], images))
        .collect();
    timings.listing = seconds(started);

    let mut report = TexturesIn {
        images: images.len(),
        packages: groups.len(),
        given: Vec::new(),
        failed,
        replaced: 0,
        layered,
        brushes: Vec::new(),
        written: None,
        dry_run: request.dry_run,
        cancelled: false,
        timings,
    };
    if groups.is_empty() {
        return Ok(report);
    }

    let threads = rayon::current_num_threads();
    let pool = crate::parallel::own_pool(threads)?;
    let (tx, rx) = std::sync::mpsc::sync_channel::<Produced>(threads * 2);
    // Set when the writer stops taking packages, so the producers stop making them.
    let stopped = AtomicBool::new(false);
    let total = groups.len();
    let done = AtomicUsize::new(0);
    let started = Instant::now();
    let mut waiting = std::time::Duration::ZERO;
    let mut last_taken = None;
    // A read earlier in this session may still hold the mod open, and it is about to be replaced.
    if !request.dry_run {
        crate::pak::containers::drop_cached_store();
    }
    let staged = std::thread::scope(|scope| -> Result<_, String> {
        let groups = &groups;
        let store = &store;
        let stopped = &stopped;
        let done = &done;
        let pool = &pool;
        scope.spawn(move || {
            pool.install(|| {
                for run in groups.chunks(PACKAGES_PER_CONVERTER) {
                    if cancel.load(Ordering::Relaxed) || stopped.load(Ordering::Relaxed) {
                        break;
                    }
                    let converter = PackageConverter::new(&**store);
                    run.par_iter()
                        .with_max_len(1)
                        .for_each(|(package, images)| {
                            if cancel.load(Ordering::Relaxed) || stopped.load(Ordering::Relaxed) {
                                return;
                            }
                            let produced = produce(
                                request, mappings, options, &**store, &converter, package, images,
                            );
                            if tx.send(produced).is_err() {
                                stopped.store(true, Ordering::Relaxed);
                            }
                            let now = done.fetch_add(1, Ordering::Relaxed) + 1;
                            progress(BatchPhase::Textures, now, total);
                        });
                }
            });
            drop(tx);
        });

        let given = &mut report.given;
        let failed = &mut report.failed;
        let waiting = &mut waiting;
        let last_taken = &mut last_taken;
        let mut written = 0;
        // Owned by the closure, so a write that stops early drops it and the producers' sends fail
        // rather than wait.
        let take = move |_: usize| -> Option<BuiltPackage> {
            let asked = Instant::now();
            let produced = rx.recv().ok();
            *waiting += asked.elapsed();
            *last_taken = Some(Instant::now());
            let produced = produced?;
            given.extend(produced.given);
            failed.extend(produced.failed);
            if produced.built.is_some() {
                written += 1;
                progress(BatchPhase::Writing, written, total);
            }
            produced.built
        };
        if request.dry_run {
            let mut take = take;
            for index in 0..total {
                take(index);
            }
            return Ok(None);
        }
        iostore_out::stage_built_into_iostore(&utoc, total, take, &options.iostore)
    });
    report.timings.textures = last_taken.map_or(0.0, |last: Instant| {
        last.duration_since(started).as_secs_f64()
    });
    report.timings.writer_waiting = waiting.as_secs_f64();
    report.cancelled = cancel.load(Ordering::Relaxed);
    report
        .given
        .sort_by(|left, right| (&left.entry, &left.image).cmp(&(&right.entry, &right.image)));
    report
        .failed
        .sort_by(|left, right| (&left.entry, &left.image).cmp(&(&right.entry, &right.image)));
    let Some(staged) = staged? else {
        return Ok(report);
    };
    if report.cancelled {
        // Dropping the staged container leaves the mod as it was.
        return Ok(report);
    }
    report.replaced = staged.report().replaced;
    // The swap renames the mod, so everything read along the way has to be closed first: under a
    // layered run the mod is one of the containers the store was reading.
    drop(store);
    let written = staged.commit()?.utoc;
    report.timings.writing = last_taken.map_or(0.0, |last: Instant| seconds(last));
    let resized: std::collections::BTreeSet<String> = report
        .given
        .iter()
        .filter(|given| given.report.from != given.report.to)
        .filter_map(|given| asset::package_name_of_entry(&given.entry))
        .collect();
    if !resized.is_empty() {
        report.brushes = crate::texture::brushes_at(
            source.game_root,
            &written.with_extension("pak"),
            mappings,
            &resized.into_iter().collect::<Vec<_>>(),
        );
    }
    report.written = Some(written);
    Ok(report)
}

/// One package read, given its images, patched, checked and converted for the writer.
#[allow(clippy::too_many_arguments)]
fn produce(
    request: &TextureImports<'_>,
    mappings: Option<&Mappings>,
    options: &SaveOptions,
    store: &dyn IoStoreTrait,
    converter: &PackageConverter<'_>,
    (id, entry): &PackageEntry,
    images: &[(Option<String>, PathBuf)],
) -> Produced {
    let mut produced = Produced {
        built: None,
        given: Vec::new(),
        failed: Vec::new(),
    };
    let fail = |image: &Path, export: Option<&str>, reason: String| TextureFailure {
        entry: entry.clone(),
        export: export.map(str::to_string),
        image: Some(image.to_string_lossy().into_owned()),
        reason,
    };
    let read = converter.convert(*id, entry).and_then(|loaded| {
        let rows = rivals_uasset::export_rows(&crate::asset_edit::bundle_of(&loaded))?;
        Ok((loaded, rows))
    });
    let (loaded, rows) = match read {
        Ok(read) => read,
        Err(reason) => {
            produced.failed = images
                .iter()
                .map(|(export, image)| fail(image, export.as_deref(), reason.clone()))
                .collect();
            return produced;
        }
    };
    let read_from = AssetEditRequest {
        game_root: request.source.game_root,
        container: request.source.container,
        entry,
        kind: AssetSource::Utoc,
        mod_name: request.mod_name,
        changes: PackageEdits::default(),
    };
    let mut changes = PackageEdits::default();
    for (export, image) in images {
        let one = || -> Result<(u32, ImportReport, Vec<rivals_uasset::NewMip>), String> {
            let index = texture_named(&rows, entry, export.as_deref())?;
            let row = &rows[index as usize];
            if changes
                .textures
                .iter()
                .any(|texture| texture.export == index)
            {
                return Err(format!(
                    "{} is given two images; give each texture one",
                    row.object_name
                ));
            }
            let layout = crate::texture::layout_of(&read_from, mappings, &loaded, index)?;
            let (report, mips) =
                crate::texture::encode_image(&loaded, row, &layout, image, request.quality)?;
            Ok((index, report, mips))
        };
        match one() {
            Ok((index, report, mips)) => {
                changes
                    .expect
                    .exports
                    .insert(index, rows[index as usize].path.clone());
                changes.textures.push(rivals_uasset::TextureEdit {
                    export: index,
                    mips,
                });
                produced.given.push(GivenTexture {
                    entry: entry.clone(),
                    image: image.to_string_lossy().into_owned(),
                    report,
                });
            }
            Err(reason) => produced.failed.push(fail(image, export.as_deref(), reason)),
        }
    }
    if changes.textures.is_empty() {
        return produced;
    }
    let save = AssetEditRequest {
        changes,
        ..read_from
    };
    let built = crate::asset_edit::patch_loaded(save, mappings, loaded, options).and_then(
        |(patched, loaded)| {
            for given in &mut produced.given {
                given.report.warnings.extend(patched.notes.iter().cloned());
            }
            if request.dry_run {
                return Ok(None);
            }
            let bytes = PackageBytes {
                entry: entry.clone(),
                asset: patched.asset,
                exports: patched.exports,
                bulk: patched.bulk.or(loaded.bulk_data_buffer),
                optional_bulk: patched.optional_bulk.or(loaded.optional_bulk_data_buffer),
                memory_mapped_bulk: loaded.memory_mapped_bulk_data_buffer,
                shader_map_hashes: store
                    .package_store_entry(*id)
                    .map(|held| held.shader_map_hashes)
                    .unwrap_or_default(),
            };
            bytes.build().map(Some)
        },
    );
    match built {
        Ok(built) => produced.built = built,
        Err(reason) => {
            // The package does not go in, so none of its images do.
            for given in std::mem::take(&mut produced.given) {
                produced.failed.push(TextureFailure {
                    entry: given.entry,
                    export: Some(given.report.name),
                    image: Some(given.image),
                    reason: reason.clone(),
                });
            }
        }
    }
    produced
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    const BODY: &str =
        "Marvel/Content/Marvel/Characters/1014/1014001/Textures/T_1014001_Body_D.uasset";
    const MAP: &str = "Marvel/Content/Marvel/Maps/Lobby/Lobby.umap";

    fn entry(path: &str) -> PackageEntry {
        (asset::package_id(path), path.to_string())
    }

    /// A texture named for its package goes out at the package's path, and any other at the path
    /// with its own name after it; both come back to the package and the texture they left.
    #[test]
    fn an_image_is_named_for_its_package_and_resolves_back_to_it() {
        let dir = Path::new("out");
        let body = image_path(dir, BODY, None, ImageKind::Dds);
        assert_eq!(
            body,
            dir.join("Marvel/Content/Marvel/Characters/1014/1014001/Textures/T_1014001_Body_D.dds")
        );
        let weightmap = image_path(dir, MAP, Some("Weightmap_120"), ImageKind::Png);
        assert_eq!(
            weightmap,
            dir.join("Marvel/Content/Marvel/Maps/Lobby/Lobby.Weightmap_120.png")
        );

        let packages = vec![
            entry(BODY),
            entry(MAP),
            entry("Marvel/Content/A/T_Twice.uasset"),
            entry("Marvel/Content/B/T_Twice.uasset"),
        ];
        let index = PackageIndex::of(&packages);
        let at = |relative: &str| index.resolve(relative);
        assert_eq!(
            at("Marvel/Content/Marvel/Characters/1014/1014001/Textures/T_1014001_Body_D"),
            Ok((0, None))
        );
        assert_eq!(
            at(r"marvel\content\marvel\maps\lobby\lobby.Weightmap_120"),
            Ok((1, Some("Weightmap_120".into())))
        );
        // A bare name finds the one package that has it, from any folder.
        assert_eq!(at("edited/T_1014001_Body_D"), Ok((0, None)));
        assert_eq!(
            at("Lobby.Weightmap_120"),
            Ok((1, Some("Weightmap_120".into())))
        );
        let twice = at("T_Twice").unwrap_err();
        assert!(twice.contains("2 packages"), "{twice}");
        let none = at("T_Nothing").unwrap_err();
        assert!(none.contains("no package"), "{none}");
        assert!(at("Some.Folder/T_Nothing").is_err());
    }

    /// Textures sharing a name in a package go out under their paths below it, and each comes
    /// back to its own.
    #[test]
    fn textures_sharing_a_name_are_told_apart_by_their_outers() {
        const SYSTEM: &str = "Marvel/Content/VFX/NS_Glow.uasset";
        let row = |index: u32, name: &str, class: &str, path: &str| ExportRow {
            index,
            object_name: name.into(),
            class_name: class.into(),
            path: path.into(),
        };
        let rows = vec![
            row(0, "NS_Glow", "NiagaraSystem", "/Game/VFX/NS_Glow.NS_Glow"),
            row(
                1,
                "Texture2D_0",
                "Texture2D",
                "/Game/VFX/NS_Glow.NS_Glow:Curve_1:Texture2D_0",
            ),
            row(
                2,
                "Texture2D_0",
                "Texture2D",
                "/Game/VFX/NS_Glow.NS_Glow:Curve_2:Texture2D_0",
            ),
            row(3, "T_Once", "Texture2D", "/Game/VFX/NS_Glow.T_Once"),
        ];
        let names: Vec<Option<String>> = rows[1..]
            .iter()
            .map(|row| texture_suffix(&rows, row, SYSTEM))
            .collect();
        assert_eq!(
            names,
            [
                Some("Curve_1.Texture2D_0".to_string()),
                Some("Curve_2.Texture2D_0".to_string()),
                Some("T_Once".to_string())
            ]
        );
        assert_eq!(
            texture_named(&rows, SYSTEM, Some("curve_2.texture2d_0")),
            Ok(2)
        );
        assert_eq!(texture_named(&rows, SYSTEM, Some("T_Once")), Ok(3));
        assert_eq!(texture_named(&rows, SYSTEM, None), Ok(1));
        assert!(texture_named(&rows, SYSTEM, Some("Texture2D_9")).is_err());
        let index = PackageIndex::of(&[entry(SYSTEM)]);
        assert_eq!(
            index.resolve("Marvel/Content/VFX/NS_Glow.Curve_1.Texture2D_0"),
            Ok((0, Some("Curve_1.Texture2D_0".into())))
        );
    }

    /// A texture asked for in a kind its values do not fit goes out in the one they do.
    #[test]
    fn a_kind_a_texture_does_not_fit_gives_way_to_one_it_does() {
        assert_eq!(kind_for(ImageKind::Dds, true), (ImageKind::Dds, None));
        assert_eq!(kind_for(ImageKind::Tga, false), (ImageKind::Tga, None));
        assert_eq!(kind_for(ImageKind::Png, true).0, ImageKind::Exr);
        assert_eq!(kind_for(ImageKind::Exr, false).0, ImageKind::Png);
    }

    /// A mod deleted whatever the test does.
    struct Scratch {
        root: String,
        names: Vec<&'static str>,
        _turn: crate::paths::ModsTurn,
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            crate::pak::containers::drop_cached_store();
            for name in &self.names {
                let Ok(pak) = crate::asset_edit::mod_pak_path(&self.root, name) else {
                    continue;
                };
                for extension in ["pak", "utoc", "ucas"] {
                    let _ = std::fs::remove_file(pak.with_extension(extension));
                }
            }
        }
    }

    fn mip_bytes_in(root: &str, utoc: &Path, entry: &str, schema: &Mappings) -> Vec<Vec<u8>> {
        let container = utoc.to_string_lossy().into_owned();
        let request = AssetEditRequest {
            game_root: root,
            container: &container,
            entry,
            kind: AssetSource::Utoc,
            mod_name: "",
            changes: PackageEdits::default(),
        };
        let loaded = asset::load_bundle(root, &container, entry, AssetSource::Utoc).expect("load");
        let layout = crate::texture::layout_of(&request, Some(schema), &loaded, 0).expect("layout");
        let platform = layout.platform().expect("one format");
        platform
            .mips
            .iter()
            .map(|mip| {
                crate::texture::mip_bytes(&loaded, mip)
                    .expect("bytes")
                    .to_vec()
            })
            .collect()
    }

    /// Asked for PNGs, an engine lookup table of halves goes out as OpenEXR saying why, and a
    /// virtual texture, of which the game ships only the engine's placeholders, is skipped rather
    /// than failed.
    #[test]
    fn a_png_batch_skips_virtual_textures_and_keeps_high_dynamic_range() {
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
        let game = asset::game_container(&root).expect("game container");
        let filters = vec![
            "Engine/Content/EngineMaterials/BaseFlattenDiffuseMap_VT".to_string(),
            "Engine/Content/EngineMaterials/Substrate/Glints2/dict_0003_0048".to_string(),
        ];
        let source = TextureSource {
            game_root: &root,
            container: &game,
            whole_game: true,
            filters: &filters,
        };
        let dir = std::env::temp_dir().join(format!("rivals-texture-skip-{}", std::process::id()));
        let out = export_textures(
            &source,
            Some(&schema),
            &dir,
            ImageKind::Png,
            None,
            &AtomicBool::new(false),
            &|_: BatchPhase, _: usize, _: usize| {},
        )
        .expect("export");
        assert!(out.failed.is_empty(), "{:?}", out.failed);
        assert_eq!(out.skipped.len(), 1, "{:?}", out.skipped);
        assert!(out.skipped[0].entry.contains("BaseFlattenDiffuseMap_VT"));
        assert_eq!(out.saved.len(), 1, "{:?}", out.saved);
        assert!(out.saved[0].path.ends_with(".exr"), "{}", out.saved[0].path);
        assert!(out.saved[0].note.is_some());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The Punisher's body textures go out as DDS files and come back in one rewrite of a mod,
    /// each byte for byte what the file holds and what saving it on its own writes. Cancelled
    /// partway, a batch leaves the mod as it was.
    #[test]
    fn a_batch_writes_what_saving_each_texture_on_its_own_writes() {
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
        let scratch = Scratch {
            root: root.clone(),
            names: vec![
                "ZZTextureBatchProbe_9999999_P",
                "ZZTextureOneProbe_9999999_P",
            ],
            _turn: crate::paths::ModsTurn::take(),
        };
        let game = asset::game_container(&root).expect("game container");
        let filters = vec!["Characters/1014/1014001/Textures/T_1014001_Body_".to_string()];
        let source = TextureSource {
            game_root: &root,
            container: &game,
            whole_game: true,
            filters: &filters,
        };
        let dir = std::env::temp_dir().join(format!("rivals-texture-batch-{}", std::process::id()));
        let never = AtomicBool::new(false);
        let quiet = |_: BatchPhase, _: usize, _: usize| {};

        let out = export_textures(
            &source,
            Some(&schema),
            &dir,
            ImageKind::Dds,
            None,
            &never,
            &quiet,
        )
        .expect("export");
        assert!(out.failed.is_empty(), "{:?}", out.failed);
        assert_eq!(out.saved.len(), 4, "{:?}", out.saved);
        assert!(
            out.saved
                .iter()
                .all(|saved| Path::new(&saved.path).is_file())
        );
        let body = out
            .saved
            .iter()
            .find(|saved| saved.entry == BODY)
            .expect("the body");
        assert_eq!(
            PathBuf::from(&body.path),
            image_path(&dir, BODY, None, ImageKind::Dds)
        );

        let options = SaveOptions::default();
        let imports = TextureImports {
            source,
            images: &dir,
            mod_name: scratch.names[0],
            quality: Quality::Fast,
            dry_run: false,
        };
        let given =
            import_textures(&imports, Some(&schema), &options, &never, &quiet).expect("import");
        assert!(given.failed.is_empty(), "{:?}", given.failed);
        assert_eq!((given.images, given.packages, given.given.len()), (4, 4, 4));
        let written = given.written.clone().expect("written");
        for saved in &out.saved {
            let dds = crate::image_file::decode_dds(&std::fs::read(&saved.path).unwrap()).unwrap();
            let held = mip_bytes_in(&root, &written, &saved.entry, &schema);
            assert_eq!(held, dds.mips, "{}", saved.entry);
        }

        // The body saved on its own, into a mod of its own, is the same package.
        let one = crate::texture::import_image(
            &AssetEditRequest {
                game_root: &root,
                container: &game,
                entry: BODY,
                kind: AssetSource::Utoc,
                mod_name: scratch.names[1],
                changes: PackageEdits::default(),
            },
            Some(&schema),
            None,
            Path::new(&body.path),
            Quality::Fast,
            &options,
            false,
        )
        .expect("one save");
        assert!(one.written.is_some());
        let utoc = |name: &str| {
            crate::asset_edit::mod_pak_path(&root, name)
                .unwrap()
                .with_extension("utoc")
                .to_string_lossy()
                .into_owned()
        };
        let read = |name: &str| {
            asset::load_bundle(&root, &utoc(name), BODY, AssetSource::Utoc).expect("read back")
        };
        let (batch, alone) = (read(scratch.names[0]), read(scratch.names[1]));
        assert!(batch.asset_file_buffer == alone.asset_file_buffer);
        assert!(batch.exports_file_buffer == alone.exports_file_buffer);
        assert!(batch.bulk_data_buffer == alone.bulk_data_buffer);

        // Cancelled once the first package is made, nothing is written.
        crate::pak::containers::drop_cached_store();
        let before = std::fs::read(&written).unwrap();
        let cancel = AtomicBool::new(false);
        let stop = |phase: BatchPhase, _: usize, _: usize| {
            if phase == BatchPhase::Textures {
                cancel.store(true, Ordering::Relaxed);
            }
        };
        let cancelled = import_textures(
            &TextureImports {
                quality: Quality::Fast,
                ..imports
            },
            Some(&schema),
            &SaveOptions {
                replace: true,
                ..SaveOptions::default()
            },
            &cancel,
            &stop,
        )
        .expect("cancelled");
        std::fs::remove_dir_all(&dir).ok();
        assert!(cancelled.cancelled && cancelled.written.is_none());
        assert!(
            std::fs::read(&written).unwrap() == before,
            "the mod is as it was"
        );
    }
}
