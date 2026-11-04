//! Finds where the game's scripts, and when asked its stored values, name something, reading each
//! package from the copy the game loads, an enabled mod's included.
//!
//! A script search converts and parses only the packages holding functions, which the import index
//! records from each package's header once, and again only for a container that changed. A value
//! search has no such shortcut: it reads every package the path filter keeps. Either reads on
//! every thread the caller's rayon pool has.

use std::collections::HashSet;
use std::io::Cursor;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rayon::prelude::*;
use retoc::container_header::EIoContainerHeaderVersion;
use retoc::iostore::IoStoreTrait;
use retoc::script_objects::FPackageObjectIndex;
use retoc::version::EngineVersion;
use retoc::zen::FZenPackageHeader;
use retoc::{EIoChunkType, EIoStoreTocVersion, FIoChunkId, FPackageId};
use rivals_uasset::{AssetBundle, ExportStatus, Mappings, ParseOptions};
use serde::Serialize;

use crate::asset::{AssetSource, PACKAGES_PER_CONVERTER, PackageConverter, PathFilter};

/// Most packages a script search reads through one converter every thread shares: about what the
/// runs in flight at once would hold between them with one each, so sharing costs no more.
const SHARED_CONVERTER_LIMIT: usize = 8192;
use crate::mod_search::{Query, SearchHit, search_package};
use crate::pak::containers::MOUNT_POINT;
use crate::pak::load_order::{self, LoadOrder};
use crate::schema_synth::{self, LayoutReader, PackageSource};

/// What a search is doing, for a caller showing its progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchPhase {
    /// Opening the containers and listing their packages.
    Listing,
    /// Bringing the import index up to date, which says which packages hold functions: how many
    /// packages it reads again out of how many.
    Headers,
    /// Parsing the packages that hold functions.
    Scripts,
    /// Parsing every package, for a value search.
    Packages,
}

/// One search of the game.
pub struct GameSearch<'a> {
    pub query: &'a Query,
    /// Only packages whose path contains one of these, as `asset audit --filter` takes each; none
    /// keeps every package.
    pub filter: &'a [String],
    /// Read the enabled mods' copies where they win, as the game does.
    pub mods: bool,
    /// List at most this many places: the first by path. Every place is still counted.
    pub max_hits: Option<usize>,
}

#[derive(Debug, Default, Serialize)]
pub struct GameSearchResult {
    /// The places found, in walk order: the mods the game prefers first, then the base game, each
    /// by path. At most the cap, the first ones when there are more.
    pub hits: Vec<SearchHit>,
    /// How many places were found in all, listed or not.
    pub found: usize,
    /// How many packages hold them.
    pub packages: usize,
    /// Packages that could not be read, with why, so an empty result is not taken as proof.
    pub unreadable: Vec<(String, String)>,
    /// How many packages the walk listed, after the path filter.
    pub listed: usize,
    /// How many of them were parsed: the ones holding functions, or every one for a value search.
    pub searched: usize,
    /// More places were found than the cap lists.
    pub truncated: bool,
    /// The search was cancelled, and the hits are the ones found before it stopped.
    pub cancelled: bool,
    #[serde(skip)]
    pub timings: SearchTimings,
}

/// How long each part of a search took, in seconds: the steps before the walk by the clock, and
/// the walk's parts summed over every thread that ran them.
#[derive(Debug, Default, Clone, Copy)]
pub struct SearchTimings {
    pub open: f64,
    pub listing: f64,
    /// Finding the packages that hold functions, for a script search.
    pub functions: f64,
    pub walk: f64,
    pub convert: f64,
    /// Parsing, `layouts` included.
    pub parse: f64,
    /// Recovering the Blueprint layouts a value search reads through.
    pub layouts: f64,
    pub matching: f64,
    pub threads: usize,
    pub converters: usize,
}

/// The walk's parts, summed over the threads running them, in nanoseconds.
#[derive(Default)]
struct Clock {
    convert: AtomicU64,
    parse: AtomicU64,
    layouts: AtomicU64,
    matching: AtomicU64,
    converters: AtomicUsize,
}

impl Clock {
    fn add(counter: &AtomicU64, spent: Duration) {
        counter.fetch_add(
            u64::try_from(spent.as_nanos()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }

    fn seconds(counter: &AtomicU64) -> f64 {
        Duration::from_nanos(counter.load(Ordering::Relaxed)).as_secs_f64()
    }
}

const ENGINE_VERSION: EngineVersion = EngineVersion::UE5_3;

/// A package the walk reads, from the layer that wins it.
struct Candidate {
    id: FPackageId,
    path: String,
    layer: usize,
    /// The container it was read from.
    container: Arc<str>,
}

enum Outcome {
    /// A package read: the places it holds, and how many, which is more than it lists when they
    /// fall past the cap.
    Hits {
        hits: Vec<SearchHit>,
        found: usize,
    },
    Unreadable(String, String),
    /// Not read, because the search was cancelled first.
    Skipped,
}

/// Which chunks of the walk still keep the places they find. Chunks finish in any order, but once
/// every chunk up to one has finished and together they hold the cap's worth of places, nothing
/// later in the walk can be among the first by path, so later chunks only count theirs. That keeps
/// the listing the same from run to run without holding every place a broad search finds.
struct Frontier {
    cap: usize,
    /// Places each finished chunk found, by chunk.
    found: Mutex<Vec<Option<usize>>>,
    /// Chunks from this one on keep nothing.
    keep_below: AtomicUsize,
}

impl Frontier {
    fn new(chunks: usize, cap: usize) -> Self {
        Self {
            cap,
            found: Mutex::new(vec![None; chunks]),
            keep_below: AtomicUsize::new(usize::MAX),
        }
    }

    fn keeps(&self, chunk: usize) -> bool {
        chunk < self.keep_below.load(Ordering::Relaxed)
    }

    fn finished(&self, chunk: usize, found: usize) {
        let Ok(mut held) = self.found.lock() else {
            return;
        };
        if let Some(slot) = held.get_mut(chunk) {
            *slot = Some(found);
        }
        let mut sum = 0usize;
        for (at, count) in held.iter().enumerate() {
            let Some(count) = count else { break };
            sum += count;
            if sum >= self.cap {
                self.keep_below.fetch_min(at + 1, Ordering::Relaxed);
                break;
            }
        }
    }
}

/// Every place the game's scripts, or with `query.values` its stored values, name what `query`
/// looks for. Runs on the caller's current rayon pool. `progress` is told the phase and how far
/// into it the walk is; `cancel` stops it, returning what was found so far.
pub fn game_search(
    game_root: &str,
    mappings: &Mappings,
    search: &GameSearch<'_>,
    cancel: &AtomicBool,
    progress: &(dyn Fn(SearchPhase, usize, usize) + Sync),
) -> Result<GameSearchResult, String> {
    progress(SearchPhase::Listing, 0, 0);
    let started = Instant::now();
    let order = load_order::open(game_root, search.mods, false)?;
    let open = started.elapsed().as_secs_f64();
    let started = Instant::now();
    let filter = PathFilter::any(search.filter.iter().map(String::as_str));
    let candidates = winning_copies(&order, &filter);
    let listed = candidates.len();
    let listing = started.elapsed().as_secs_f64();
    progress(SearchPhase::Listing, listed, listed);

    let values = search.query.values;
    let started = Instant::now();
    let candidates = if values {
        candidates
    } else {
        progress(SearchPhase::Headers, 0, 0);
        let index = crate::import_index::current_with(game_root, &|done, total| {
            progress(SearchPhase::Headers, done, total);
        })
        .map_err(|e| {
            format!("finding the packages that hold functions needs the import index: {e}")
        })?;
        candidates
            .into_iter()
            .filter(|candidate| {
                index.may_hold_functions(
                    order.layers[candidate.layer].mod_name.as_deref(),
                    candidate.id.0,
                )
            })
            .collect()
    };
    let functions = started.elapsed().as_secs_f64();
    let phase = if values {
        SearchPhase::Packages
    } else {
        SearchPhase::Scripts
    };
    let total = candidates.len();
    let done = AtomicUsize::new(0);
    let cap = search.max_hits.unwrap_or(usize::MAX);
    let clock = Clock::default();
    // A script search is short enough to share one converter, so its runs only spread the work:
    // a few per thread, small enough that no thread is left with a folder of heavy packages. A
    // longer walk gives each run a converter of its own, so what each keeps stays bounded, every
    // one of them sharing the template's script object table.
    let threads = rayon::current_num_threads();
    let template = PackageConverter::new(&order);
    template.prepare();
    let shared = !values && total <= SHARED_CONVERTER_LIMIT;
    let run = if shared {
        clock.converters.fetch_add(1, Ordering::Relaxed);
        total.div_ceil(threads * 8).clamp(1, 32)
    } else {
        total
            .div_ceil(threads * 4)
            .clamp(64, PACKAGES_PER_CONVERTER)
    };
    let frontier = Frontier::new(total.div_ceil(run), cap);
    let started = Instant::now();
    let outcomes: Vec<Outcome> = candidates
        .par_chunks(run)
        // Each run of packages is a job of its own: neighbours by path cost alike, so a thread
        // handed several at once can be left with all of a folder of maps while the rest wait.
        .with_max_len(1)
        .enumerate()
        .flat_map_iter(|(index, chunk)| {
            let own;
            let converter = if shared {
                &template
            } else {
                clock.converters.fetch_add(1, Ordering::Relaxed);
                own = template.sibling();
                &own
            };
            // A value search reads values the way `asset dump` shows them, which can take layouts
            // recovered from the Blueprints that define them: the copies the game loads, read
            // through the same converter, which already holds the headers they import.
            let layouts = values.then(|| LayoutReader::through(&order, converter));
            let mut found = 0;
            let outcomes: Vec<Outcome> = chunk
                .iter()
                .map(|candidate| {
                    if cancel.load(Ordering::Relaxed) {
                        return Outcome::Skipped;
                    }
                    let mut outcome = search_one(
                        converter,
                        shared.then_some(&order),
                        candidate,
                        &order,
                        mappings,
                        search.query,
                        game_root,
                        layouts.as_ref(),
                        Some(&clock),
                    );
                    if let Outcome::Hits { hits, found: here } = &mut outcome {
                        found += *here;
                        if !frontier.keeps(index) {
                            hits.clear();
                        }
                    }
                    let now = done.fetch_add(1, Ordering::Relaxed) + 1;
                    if now.is_multiple_of(64) || now == total {
                        progress(phase, now, total);
                    }
                    outcome
                })
                .collect();
            if let Some(layouts) = &layouts {
                Clock::add(&clock.layouts, layouts.spent());
            }
            frontier.finished(index, found);
            outcomes
        })
        .collect();
    let walk = started.elapsed().as_secs_f64();
    let mut result = gather(outcomes, cap);
    result.listed = listed;
    result.cancelled = cancel.load(Ordering::Relaxed);
    result.timings = SearchTimings {
        open,
        listing,
        functions,
        walk,
        convert: Clock::seconds(&clock.convert),
        parse: Clock::seconds(&clock.parse),
        layouts: Clock::seconds(&clock.layouts),
        matching: Clock::seconds(&clock.matching),
        threads: rayon::current_num_threads(),
        converters: clock.converters.load(Ordering::Relaxed),
    };
    Ok(result)
}

/// Each package once, from the first layer holding it, in the order the walk reports them: the
/// mods in the order the game prefers them, then the base game, each by path.
fn winning_copies(order: &LoadOrder, filter: &PathFilter) -> Vec<Candidate> {
    // A package counts from the first layer listing it, whether or not that layer gives it a path.
    let single = order.layers.len() == 1;
    let mut seen = HashSet::new();
    let mut listed = Vec::new();
    for (layer, held) in order.layers.iter().enumerate() {
        for pkg in held.store.packages_all() {
            if single || seen.insert(pkg.id().0) {
                listed.push((layer, pkg));
            }
        }
    }
    // Finding a chunk's path searches the layer's containers, so it is the part worth spreading.
    let mut kept: Vec<_> = listed
        .into_par_iter()
        .filter_map(|(layer, pkg)| {
            let chunk = FIoChunkId::from_package_id(pkg.id(), 0, EIoChunkType::ExportBundleData);
            let path = order.layers[layer].store.chunk_path(chunk)?;
            let path = match path.strip_prefix(MOUNT_POINT) {
                Some(rest) => rest.to_string(),
                None => path,
            };
            filter.matches(&path).then_some((layer, pkg, path))
        })
        .collect();
    kept.par_sort_by(|a, b| (a.0, &a.2).cmp(&(b.0, &b.2)));
    let mut containers: Vec<Arc<str>> = Vec::new();
    kept.into_iter()
        .map(|(layer, pkg, path)| {
            let file = pkg.container().container_path().to_string_lossy();
            let container = match containers
                .iter()
                .find(|held| held.as_ref() == file.as_ref())
            {
                Some(held) => Arc::clone(held),
                None => {
                    let fresh: Arc<str> = Arc::from(file.as_ref());
                    containers.push(Arc::clone(&fresh));
                    fresh
                }
            };
            Candidate {
                id: pkg.id(),
                path,
                layer,
                container,
            }
        })
        .collect()
}

/// Whether package `id`'s IoStore header, read from `store` by its container's `versions`, gives
/// one of its exports one of `classes`, a native class as a script import names it. `None` when
/// the header does not read, which a caller takes as a reason to read the package whole.
pub(crate) fn header_names_class(
    store: &dyn IoStoreTrait,
    id: FPackageId,
    versions: (EIoStoreTocVersion, EIoContainerHeaderVersion),
    classes: &[FPackageObjectIndex],
) -> Option<bool> {
    let held = export_classes(store, id, versions)?;
    Some(held.iter().any(|class| classes.contains(class)))
}

/// The class of each of package `id`'s exports, as its IoStore header gives them, read from
/// `store` by its container's `versions`. `None` when the header does not read.
pub(crate) fn export_classes(
    store: &dyn IoStoreTrait,
    id: FPackageId,
    (toc_version, header_version): (EIoStoreTocVersion, EIoContainerHeaderVersion),
) -> Option<Vec<FPackageObjectIndex>> {
    let chunk = FIoChunkId::from_package_id(id, 0, EIoChunkType::ExportBundleData);
    let data = store.read(chunk).ok()?;
    let header = FZenPackageHeader::deserialize(
        &mut Cursor::new(&data),
        store.package_store_entry(id),
        toc_version,
        header_version,
        Some(ENGINE_VERSION.package_file_version()),
    )
    .ok()?;
    Some(
        header
            .export_map
            .iter()
            .map(|export| export.class_index)
            .collect(),
    )
}

/// `retry` is the store a converter other threads share reads, for a package that fails to convert
/// through it: the failure is converted again on a fresh converter, so what it says never depends on
/// which thread met a broken import first.
#[allow(clippy::too_many_arguments)]
fn search_one(
    converter: &PackageConverter<'_>,
    retry: Option<&LoadOrder>,
    candidate: &Candidate,
    order: &LoadOrder,
    mappings: &Mappings,
    query: &Query,
    game_root: &str,
    layouts: Option<&LayoutReader<'_>>,
    clock: Option<&Clock>,
) -> Outcome {
    let parsed = match parse_candidate(
        converter, retry, candidate, mappings, game_root, layouts, clock,
    ) {
        Ok(parsed) => parsed,
        Err(reason) => return Outcome::Unreadable(candidate.path.clone(), reason),
    };
    let started = Instant::now();
    let mut hits = Vec::new();
    search_package(&candidate.path, &parsed, query, &mut hits);
    let mod_name = &order.layers[candidate.layer].mod_name;
    let own = crate::ability_ids::place_of(&candidate.path);
    for hit in &mut hits {
        hit.container = Some(candidate.container.to_string());
        hit.in_mod.clone_from(mod_name);
        // A package outside any hero's folders may still name a hero's Python class.
        let place = match own.hero {
            Some(_) => own,
            None if hit.term.starts_with("/Script/UnrealEnginePython/") => {
                crate::ability_ids::place_of(&hit.term)
            }
            None => own,
        };
        hit.place = place.hero.is_some().then_some(place);
    }
    if let Some(clock) = clock {
        Clock::add(&clock.matching, started.elapsed());
    }
    Outcome::Hits {
        found: hits.len(),
        hits,
    }
}

/// One package read the way a search reads it: through `layouts` the way `asset dump` shows its
/// values, or with the plain parse bytecode needs.
fn parse_candidate(
    converter: &PackageConverter<'_>,
    retry: Option<&LoadOrder>,
    candidate: &Candidate,
    mappings: &Mappings,
    game_root: &str,
    layouts: Option<&LayoutReader<'_>>,
    clock: Option<&Clock>,
) -> Result<rivals_uasset::ParsedPackage, String> {
    let started = Instant::now();
    let bundle = converter
        .convert(candidate.id, &candidate.path)
        .or_else(|failed| match retry {
            Some(order) => PackageConverter::new(order).convert(candidate.id, &candidate.path),
            None => Err(failed),
        });
    if let Some(clock) = clock {
        Clock::add(&clock.convert, started.elapsed());
    }
    let bundle = bundle?;
    let bundle = AssetBundle {
        asset: &bundle.asset_file_buffer,
        exports: &bundle.exports_file_buffer,
    };
    let started = Instant::now();
    let parsed = match layouts {
        // Values read the way `asset dump` shows them.
        Some(layouts) => schema_synth::parse_package_through(
            &bundle,
            Some(mappings),
            &PackageSource {
                game_root,
                container: &candidate.container,
                entry: &candidate.path,
                kind: AssetSource::Utoc,
                mod_container: None,
            },
            layouts,
            ParseOptions::default(),
        ),
        // Bytecode needs no layout recovered from another Blueprint, nor the exports that hold
        // none. An export read that fails could have taken a repair a whole read makes, which can
        // reach the exports holding bytecode, so that package is read whole.
        None => script_parse(&bundle, mappings, true).and_then(|parsed| {
            match parsed
                .exports
                .iter()
                .any(|export| matches!(export.status, ExportStatus::Failed { .. }))
            {
                true => script_parse(&bundle, mappings, false),
                false => Ok(parsed),
            }
        }),
    };
    if let Some(clock) = clock {
        Clock::add(&clock.parse, started.elapsed());
    }
    parsed
}

/// The parse a script search reads bytecode from, of the exports holding it alone or of all.
fn script_parse(
    bundle: &AssetBundle<'_>,
    mappings: &Mappings,
    scripts_only: bool,
) -> Result<rivals_uasset::ParsedPackage, String> {
    rivals_uasset::parse_package_opts(
        bundle,
        Some(mappings),
        None,
        ParseOptions {
            skip_twins: true,
            scripts_only,
            ..Default::default()
        },
    )
}

/// A package the game loads, read whole.
pub(crate) struct ReadPackage {
    pub path: String,
    /// The container it was read from.
    pub container: String,
    /// The enabled mod whose copy the game loads.
    pub in_mod: Option<String>,
    pub parsed: Result<rivals_uasset::ParsedPackage, String>,
}

/// Every package the game loads whose path contains one of `filters`, as `asset audit --filter`
/// takes one, each read whole from the copy the game loads, an enabled mod's when `mods` is set.
/// For a few packages at once; a walk over the whole game is `game_search`'s.
pub(crate) fn read_packages(
    game_root: &str,
    mappings: &Mappings,
    mods: bool,
    filters: &[&str],
) -> Result<Vec<ReadPackage>, String> {
    let order = load_order::open(game_root, mods, false)?;
    let mut candidates: Vec<Candidate> = Vec::new();
    for filter in filters {
        for candidate in winning_copies(&order, &PathFilter::new(Some(filter))) {
            if !candidates.iter().any(|held| held.path == candidate.path) {
                candidates.push(candidate);
            }
        }
    }
    let converter = PackageConverter::new(&order);
    let layouts = LayoutReader::through(&order, &converter);
    Ok(candidates
        .iter()
        .map(|candidate| ReadPackage {
            path: candidate.path.clone(),
            container: candidate.container.to_string(),
            in_mod: order.layers[candidate.layer].mod_name.clone(),
            parsed: parse_candidate(
                &converter,
                None,
                candidate,
                mappings,
                game_root,
                Some(&layouts),
                None,
            ),
        })
        .collect())
}

/// The first `cap` places in walk order, how many there were in all, and the packages that
/// would not read.
fn gather(outcomes: Vec<Outcome>, cap: usize) -> GameSearchResult {
    let mut result = GameSearchResult::default();
    for outcome in outcomes {
        match outcome {
            Outcome::Hits { hits, found } => {
                result.searched += 1;
                result.found += found;
                result.packages += usize::from(found > 0);
                result.hits.extend(hits);
            }
            Outcome::Unreadable(path, reason) => {
                result.searched += 1;
                result.unreadable.push((path, reason));
            }
            Outcome::Skipped => {}
        }
    }
    result.truncated = result.found > cap;
    result.hits.truncate(cap);
    result
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::mod_search::HitKind;

    fn hit(package: &str) -> SearchHit {
        SearchHit {
            package: package.into(),
            export: "F".into(),
            export_index: 0,
            offset: Some(0),
            kind: HitKind::Call,
            term: "T".into(),
            line: "T()".into(),
            call: None,
            container: None,
            in_mod: None,
            row: None,
            field: None,
            id: None,
            place: None,
        }
    }

    fn listed(hits: Vec<SearchHit>) -> Outcome {
        Outcome::Hits {
            found: hits.len(),
            hits,
        }
    }

    /// The hits come back in walk order, a cap lists the first of them, and every place found is
    /// counted, listed or not.
    #[test]
    fn a_hit_cap_lists_the_first_by_path_and_counts_the_rest() {
        let outcomes = || {
            vec![
                listed(vec![hit("A"), hit("A")]),
                Outcome::Unreadable("B".into(), "broken".into()),
                Outcome::Skipped,
                listed(vec![hit("C")]),
                // A chunk past the frontier counts its places without keeping them.
                Outcome::Hits {
                    hits: Vec::new(),
                    found: 4,
                },
            ]
        };
        let all = gather(outcomes(), usize::MAX);
        let packages: Vec<&str> = all.hits.iter().map(|h| h.package.as_str()).collect();
        assert_eq!(packages, ["A", "A", "C"]);
        assert_eq!((all.searched, all.found, all.packages), (4, 7, 3));
        assert_eq!(all.unreadable, [("B".to_string(), "broken".to_string())]);

        let capped = gather(outcomes(), 2);
        let packages: Vec<&str> = capped.hits.iter().map(|h| h.package.as_str()).collect();
        assert_eq!(packages, ["A", "A"]);
        assert_eq!(capped.found, 7);
        assert!(capped.truncated);
        assert!(!gather(vec![listed(vec![hit("A")])], 1).truncated);
    }

    /// Chunks finish in any order. Only once the chunks from the first one on hold the cap's worth
    /// do later chunks stop keeping their places, so what is listed is the same however the
    /// threads ran.
    #[test]
    fn the_frontier_moves_only_past_a_finished_run_from_the_start() {
        let frontier = Frontier::new(4, 3);
        frontier.finished(2, 10);
        assert!(
            frontier.keeps(3),
            "chunk 0 and 1 may still hold the first places"
        );
        frontier.finished(0, 2);
        assert!(frontier.keeps(3));
        frontier.finished(1, 1);
        assert!(frontier.keeps(1));
        assert!(!frontier.keeps(2), "chunks 0 and 1 already hold three");
        assert!(!frontier.keeps(3));
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod game_data_tests {
    use super::*;
    use crate::mod_search::HitKind;

    /// The Settings screens' widgets: a few hundred packages, a good share of them Blueprints.
    const SETTINGS: &str = "Marvel/Content/Marvel/UI/Blueprints/Setting";

    fn install() -> Option<(String, Arc<Mappings>)> {
        let root = std::env::var("RIVALS_GAME_ROOT").ok()?;
        let usmap = std::env::var("RIVALS_USMAP").ok()?;
        let mappings = crate::mappings::load(std::path::Path::new(&usmap)).expect("mappings");
        Some((root, mappings))
    }

    fn never() -> AtomicBool {
        AtomicBool::new(false)
    }

    /// A script search reads each script export, its bytecode, signature and layout, exactly as a
    /// read of the whole package gives it, over a spread of the packages holding functions, or all
    /// of them with `RIVALS_EVERY_HOLDER` set.
    #[test]
    fn a_script_only_read_gives_every_script_export_as_the_whole_read_does() {
        let Some((root, mappings)) = install() else {
            return;
        };
        let order = load_order::open(&root, false, false).expect("containers");
        let index = crate::import_index::current(&root).expect("import index");
        let holders: Vec<Candidate> = winning_copies(&order, &PathFilter::new(None))
            .into_iter()
            .filter(|candidate| index.may_hold_functions(None, candidate.id.0))
            .collect();
        drop(index);
        let stride = if std::env::var("RIVALS_EVERY_HOLDER").is_ok() {
            1
        } else {
            13
        };
        let converter = PackageConverter::new(&order);
        let shown = |parsed: &rivals_uasset::ParsedPackage| -> Vec<String> {
            parsed
                .exports
                .iter()
                .filter(|export| export.script.is_some() || export.struct_definition.is_some())
                .map(|export| {
                    format!(
                        "{} {} {:?} {:?} {:?} {:?}",
                        export.object_name,
                        export.outer_index,
                        export.script,
                        export.signature,
                        export.struct_definition,
                        export.layout
                    )
                })
                .collect()
        };
        let compared: Vec<bool> = holders
            .par_iter()
            .step_by(stride)
            .filter_map(|candidate| {
                let bundle = converter.convert(candidate.id, &candidate.path).ok()?;
                let bundle = AssetBundle {
                    asset: &bundle.asset_file_buffer,
                    exports: &bundle.exports_file_buffer,
                };
                let whole = script_parse(&bundle, &mappings, false).ok()?;
                let scripts = script_parse(&bundle, &mappings, true).expect("a script-only read");
                let reread = scripts
                    .exports
                    .iter()
                    .any(|export| matches!(export.status, ExportStatus::Failed { .. }));
                if !reread {
                    assert_eq!(shown(&scripts), shown(&whole), "{}", candidate.path);
                }
                Some(reread)
            })
            .collect();
        let reread = compared.iter().filter(|reread| **reread).count();
        assert!(compared.len() > 300, "{}", compared.len());
        eprintln!(
            "{} holder(s) compared, {reread} read whole for a failing export",
            compared.len()
        );
    }

    /// A search narrowed to a folder and to calls finds them only there, each at a statement of a
    /// package read from a container that exists.
    #[test]
    fn a_filtered_script_search_finds_calls_only_where_the_filter_keeps() {
        let Some((root, mappings)) = install() else {
            return;
        };
        let query = Query::new("ExecuteUbergraph", vec![HitKind::Call], false).unwrap();
        let result = game_search(
            &root,
            &mappings,
            &GameSearch {
                query: &query,
                filter: &[SETTINGS.to_string()],
                mods: false,
                max_hits: None,
            },
            &never(),
            &|_, _, _| {},
        )
        .expect("searched");
        assert!(!result.hits.is_empty());
        assert!(result.searched < result.listed, "only the scripted ones");
        for hit in &result.hits {
            assert!(hit.package.contains(SETTINGS), "{}", hit.package);
            assert_eq!(hit.kind, HitKind::Call);
            assert!(hit.offset.is_some());
            assert!(hit.in_mod.is_none());
            let container = hit.container.as_deref().expect("a container");
            assert!(std::path::Path::new(container).is_file(), "{container}");
        }
    }

    /// A search through `filter` with `query`, the base game alone.
    fn searched(root: &str, mappings: &Mappings, query: &Query, filter: &str) -> GameSearchResult {
        game_search(
            root,
            mappings,
            &GameSearch {
                query,
                filter: &[filter.to_string()],
                mods: false,
                max_hits: None,
            },
            &never(),
            &|_, _, _| {},
        )
        .expect("searched")
    }

    /// The idle rule's timer is found by its column's name: each match mode's row with the row's
    /// id and what it holds, the 10-minute rule's mode among them.
    #[test]
    fn a_table_column_is_found_by_its_name_with_each_row_s_value() {
        let Some((root, mappings)) = install() else {
            return;
        };
        let query = Query::new("InactiveFightJudgeTime", Vec::new(), true)
            .unwrap()
            .fields(true);
        let result = searched(&root, &mappings, &query, "MarvelGameMatchModeTable");
        assert!(!result.hits.is_empty());
        for hit in &result.hits {
            assert_eq!(hit.kind, HitKind::Field, "{}", hit.line);
            assert!(hit.row.is_some(), "{}", hit.line);
            assert!(
                hit.id
                    .as_deref()
                    .is_some_and(|id| id.starts_with("GamePlayModeId ")),
                "{:?}",
                hit.id
            );
        }
        assert!(
            result.hits.iter().any(|hit| hit.line.ends_with("= 600")),
            "{:?}",
            result.hits.iter().map(|hit| &hit.line).collect::<Vec<_>>()
        );
    }

    /// The turret's summon id finds the packages that name it, and with self-references left
    /// out, not the summon's own components pointing at each other by their full paths.
    #[test]
    fn a_package_naming_itself_is_left_out_when_asked() {
        let Some((root, mappings)) = install() else {
            return;
        };
        let own = |hits: &[SearchHit]| {
            hits.iter()
                .filter(|hit| hit.package.contains("Summoned_10146101_BP"))
                .count()
        };
        let all = Query::new("10146101", Vec::new(), true).unwrap();
        let every = searched(&root, &mappings, &all, "AbilitySystem/1014");
        assert!(own(&every.hits) > 0, "the summon names itself");
        let skipping = all.clone().skip_self(true);
        let others = searched(&root, &mappings, &skipping, "AbilitySystem/1014");
        assert_eq!(own(&others.hits), 0);
        assert!(others.found < every.found);
        for hit in &others.hits {
            assert!(
                hit.place.is_some_and(|place| place.hero == Some(1014)),
                "{}",
                hit.package
            );
        }
    }

    /// The first enabled mod's own package is read from that mod, and every hit says so.
    #[test]
    fn an_enabled_mod_s_package_is_searched_from_the_mod_that_ships_it() {
        let Some((root, mappings)) = install() else {
            return;
        };
        let _turn = crate::paths::ModsTurn::take();
        let order = load_order::open(&root, true, false).expect("containers");
        let Some(layer) = order.layers.iter().find(|layer| layer.mod_name.is_some()) else {
            return;
        };
        let name = layer.mod_name.clone().unwrap();
        let path = winning_copies(&order, &PathFilter::new(None))
            .into_iter()
            .find(|candidate| order.layers[candidate.layer].mod_name.as_ref() == Some(&name))
            .expect("a package the mod wins")
            .path;
        let query = Query::new("/", Vec::new(), true).unwrap();
        let result = game_search(
            &root,
            &mappings,
            &GameSearch {
                query: &query,
                filter: std::slice::from_ref(&path),
                mods: true,
                max_hits: None,
            },
            &never(),
            &|_, _, _| {},
        )
        .expect("searched");
        assert!(result.unreadable.is_empty(), "{:?}", result.unreadable);
        for hit in &result.hits {
            assert_eq!(
                hit.in_mod.as_deref(),
                Some(name.as_str()),
                "{}",
                hit.package
            );
            assert!(
                hit.container
                    .as_deref()
                    .is_some_and(|container| container.contains(&name)),
                "{:?}",
                hit.container
            );
        }
    }

    /// A mod's package imports the game's, so it converts only with the game layered under it.
    #[test]
    fn every_package_of_a_mod_converts_through_the_load_order() {
        let Some((root, _)) = install() else {
            return;
        };
        let _turn = crate::paths::ModsTurn::take();
        let order = load_order::open(&root, true, false).expect("containers");
        let converter = PackageConverter::new(&order);
        for candidate in winning_copies(&order, &PathFilter::new(None))
            .iter()
            .filter(|candidate| order.layers[candidate.layer].mod_name.is_some())
            .take(300)
        {
            converter
                .convert(candidate.id, &candidate.path)
                .unwrap_or_else(|e| panic!("{}: {e}", candidate.path));
        }
    }

    /// Layouts a value search recovers through its own converter read every package just as
    /// layouts recovered from the container do, over the Settings widgets, some of which need one.
    #[test]
    fn layouts_read_through_the_search_s_converter_read_as_the_container_s_do() {
        let Some((root, mappings)) = install() else {
            return;
        };
        let order = load_order::open(&root, false, false).expect("containers");
        let candidates = winning_copies(&order, &PathFilter::new(Some(SETTINGS)));
        assert!(!candidates.is_empty());
        let container = crate::asset::game_container(&root).expect("a game container");
        let converter = PackageConverter::new(&order);
        let layouts = LayoutReader::through(&order, &converter);
        let json = |parsed: Result<rivals_uasset::ParsedPackage, String>| {
            parsed.map(|parsed| serde_json::to_string(&parsed).expect("json"))
        };
        let mut recovered = 0;
        for candidate in &candidates {
            let bundle = converter
                .convert(candidate.id, &candidate.path)
                .expect("converts");
            let bundle = AssetBundle {
                asset: &bundle.asset_file_buffer,
                exports: &bundle.exports_file_buffer,
            };
            let source = PackageSource {
                game_root: &root,
                container: &container,
                entry: &candidate.path,
                kind: AssetSource::Utoc,
                mod_container: None,
            };
            let options = ParseOptions::default();
            let through = json(schema_synth::parse_package_through(
                &bundle,
                Some(&mappings),
                &source,
                &layouts,
                options,
            ));
            let from_container = json(schema_synth::parse_package_opts(
                &bundle,
                Some(&mappings),
                &source,
                options,
            ));
            assert!(through == from_container, "{}", candidate.path);
            let unrecovered = json(rivals_uasset::parse_package_opts(
                &bundle,
                Some(&mappings),
                None,
                options,
            ));
            recovered += usize::from(unrecovered != through);
        }
        assert!(recovered > 0, "nothing under {SETTINGS} needed a layout");
    }

    /// Every field record of every class and function in the Settings widgets writes back to
    /// exactly the bytes the cook wrote, which is what a new one is written in the likeness of.
    #[test]
    fn every_field_record_writes_back_as_the_cook_wrote_it() {
        let Some((root, mappings)) = install() else {
            return;
        };
        let order = load_order::open(&root, false, false).expect("containers");
        let converter = PackageConverter::new(&order);
        let mut records = 0;
        for candidate in winning_copies(&order, &PathFilter::new(Some(SETTINGS))) {
            let bundle = converter
                .convert(candidate.id, &candidate.path)
                .expect("converts");
            let assets = AssetBundle {
                asset: &bundle.asset_file_buffer,
                exports: &bundle.exports_file_buffer,
            };
            let parsed = rivals_uasset::parse_package_opts(
                &assets,
                Some(&mappings),
                None,
                ParseOptions {
                    skip_twins: true,
                    ..Default::default()
                },
            )
            .expect("parses");
            let header = rivals_uasset::read_header(&assets).expect("header");
            let bytes = [
                bundle.asset_file_buffer.as_slice(),
                bundle.exports_file_buffer.as_slice(),
            ]
            .concat();
            let mut names = header.name_map.clone();
            for export in &parsed.exports {
                for (record, (start, end)) in export.layout.iter().flat_map(|l| &l.records) {
                    assert!(
                        rivals_uasset::encode_field_record(record, &mut names)
                            == bytes[*start as usize..*end as usize],
                        "{} {}: {}",
                        candidate.path,
                        export.object_name,
                        record.name
                    );
                    records += 1;
                }
            }
            assert_eq!(
                names.num_names(),
                header.name_map.num_names(),
                "{}",
                candidate.path
            );
        }
        assert!(records > 1000, "only {records} records");
    }
}
