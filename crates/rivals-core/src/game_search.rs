//! Finds where the game's scripts, and when asked its stored values, name something, reading each
//! package from the copy the game loads, an enabled mod's included.
//!
//! Nothing is kept between searches. A script search reads every package's IoStore header first,
//! which says whether it holds any functions, and converts and parses only the ones that do, on
//! every thread the caller's rayon pool has. A value search has no such shortcut: it reads every
//! package the path filter keeps.

use std::collections::HashSet;
use std::io::Cursor;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rayon::prelude::*;
use retoc::container_header::EIoContainerHeaderVersion;
use retoc::iostore::IoStoreTrait;
use retoc::script_objects::FPackageObjectIndex;
use retoc::version::EngineVersion;
use retoc::zen::FZenPackageHeader;
use retoc::{EIoChunkType, EIoStoreTocVersion, FIoChunkId, FPackageId};
use rivals_uasset::{AssetBundle, Mappings, ParseOptions};
use serde::Serialize;

use crate::asset::{AssetSource, PackageConverter, PathFilter};
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
    /// Reading each package's header to find the ones holding functions.
    Headers,
    /// Parsing the packages that hold functions.
    Scripts,
    /// Parsing every package, for a value search.
    Packages,
}

/// One search of the game.
pub struct GameSearch<'a> {
    pub query: &'a Query,
    /// Only packages whose path contains this, as `asset audit --filter` takes it.
    pub filter: Option<&'a str>,
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
}

/// Packages one converter reads before it is dropped. A converter keeps the header of every
/// package it converts and every one those import, so a long walk with one would hold the whole
/// game's headers; making one costs about forty packages' worth of work.
const PACKAGES_PER_CONVERTER: usize = 512;

const ENGINE_VERSION: EngineVersion = EngineVersion::UE5_3;

/// A package the walk reads, from the layer that wins it.
struct Candidate {
    id: FPackageId,
    path: String,
    layer: usize,
    /// The container it was read from, which is where the inspector opens it.
    container: Arc<str>,
    /// That container's versions, which its header is read by.
    versions: Option<(EIoStoreTocVersion, EIoContainerHeaderVersion)>,
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
    let order = load_order::open(game_root, search.mods, false)?;
    let filter = PathFilter::new(search.filter);
    let candidates = winning_copies(&order, &filter);
    let listed = candidates.len();
    progress(SearchPhase::Listing, listed, listed);

    let values = search.query.values;
    let candidates = if values {
        candidates
    } else {
        holding_functions(&order, candidates, cancel, progress)
    };
    let phase = if values {
        SearchPhase::Packages
    } else {
        SearchPhase::Scripts
    };
    let total = candidates.len();
    let done = AtomicUsize::new(0);
    let cap = search.max_hits.unwrap_or(usize::MAX);
    let frontier = Frontier::new(total.div_ceil(PACKAGES_PER_CONVERTER), cap);
    let outcomes: Vec<Outcome> = candidates
        .par_chunks(PACKAGES_PER_CONVERTER)
        // Each run of packages is a job of its own: neighbours by path cost alike, so a thread
        // handed several at once can be left with all of a folder of maps while the rest wait.
        .with_max_len(1)
        .enumerate()
        .flat_map_iter(|(index, chunk)| {
            let converter = PackageConverter::new(&order);
            // A value search reads values the way the inspector shows them, which can take layouts
            // recovered from the Blueprints that define them: the copies the game loads, read
            // through the same converter, which already holds the headers they import.
            let layouts = values.then(|| LayoutReader::through(&order, &converter));
            let mut found = 0;
            let outcomes: Vec<Outcome> = chunk
                .iter()
                .map(|candidate| {
                    if cancel.load(Ordering::Relaxed) {
                        return Outcome::Skipped;
                    }
                    let mut outcome = search_one(
                        &converter,
                        candidate,
                        &order,
                        mappings,
                        search.query,
                        game_root,
                        layouts.as_ref(),
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
            frontier.finished(index, found);
            outcomes
        })
        .collect();
    let mut result = gather(outcomes, cap);
    result.listed = listed;
    result.cancelled = cancel.load(Ordering::Relaxed);
    Ok(result)
}

/// Each package once, from the first layer holding it, in the order the walk reports them: the
/// mods in the order the game prefers them, then the base game, each by path.
fn winning_copies(order: &LoadOrder, filter: &PathFilter) -> Vec<Candidate> {
    let mut seen = HashSet::new();
    let mut containers: Vec<Arc<str>> = Vec::new();
    let mut out = Vec::new();
    for (layer, held) in order.layers.iter().enumerate() {
        for pkg in held.store.packages_all() {
            if !seen.insert(pkg.id().0) {
                continue;
            }
            let chunk = FIoChunkId::from_package_id(pkg.id(), 0, EIoChunkType::ExportBundleData);
            let Some(path) = held.store.chunk_path(chunk) else {
                continue;
            };
            let path = path.strip_prefix(MOUNT_POINT).unwrap_or(&path).to_string();
            if !filter.matches(&path) {
                continue;
            }
            let held_in = pkg.container();
            let versions = held_in
                .container_file_version()
                .zip(held_in.container_header_version());
            let file = held_in.container_path().to_string_lossy();
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
            out.push(Candidate {
                id: pkg.id(),
                path,
                layer,
                container,
                versions,
            });
        }
    }
    out.sort_by(|a, b| (a.layer, &a.path).cmp(&(b.layer, &b.path)));
    out
}

/// The packages whose header says one of their exports is a function, in the order given. A
/// header that does not read keeps its package, so the parse reports it rather than the search
/// dropping it in silence.
fn holding_functions(
    order: &LoadOrder,
    candidates: Vec<Candidate>,
    cancel: &AtomicBool,
    progress: &(dyn Fn(SearchPhase, usize, usize) + Sync),
) -> Vec<Candidate> {
    let function = FPackageObjectIndex::create_script_import("/Script/CoreUObject.Function");
    let total = candidates.len();
    let done = AtomicUsize::new(0);
    candidates
        .into_par_iter()
        .filter(|candidate| {
            if cancel.load(Ordering::Relaxed) {
                return false;
            }
            let store = order.layers[candidate.layer].store.as_ref();
            let keep = header_of(store, candidate).is_none_or(|header| {
                header
                    .export_map
                    .iter()
                    .any(|export| export.class_index == function)
            });
            let now = done.fetch_add(1, Ordering::Relaxed) + 1;
            if now.is_multiple_of(1024) || now == total {
                progress(SearchPhase::Headers, now, total);
            }
            keep
        })
        .collect()
}

/// A package's IoStore header, read from the store holding it.
fn header_of(store: &dyn IoStoreTrait, candidate: &Candidate) -> Option<FZenPackageHeader> {
    let (toc_version, header_version) = candidate.versions?;
    let chunk = FIoChunkId::from_package_id(candidate.id, 0, EIoChunkType::ExportBundleData);
    let data = store.read(chunk).ok()?;
    FZenPackageHeader::deserialize(
        &mut Cursor::new(&data),
        store.package_store_entry(candidate.id),
        toc_version,
        header_version,
        Some(ENGINE_VERSION.package_file_version()),
    )
    .ok()
}

fn search_one(
    converter: &PackageConverter<'_>,
    candidate: &Candidate,
    order: &LoadOrder,
    mappings: &Mappings,
    query: &Query,
    game_root: &str,
    layouts: Option<&LayoutReader<'_>>,
) -> Outcome {
    let unreadable = |reason: String| Outcome::Unreadable(candidate.path.clone(), reason);
    let bundle = match converter.convert(candidate.id, &candidate.path) {
        Ok(bundle) => bundle,
        Err(reason) => return unreadable(reason),
    };
    let bundle = AssetBundle {
        asset: &bundle.asset_file_buffer,
        exports: &bundle.exports_file_buffer,
    };
    let parsed = match layouts {
        // Values read the way the inspector shows them.
        Some(layouts) => schema_synth::parse_package_through(
            &bundle,
            Some(mappings),
            &PackageSource {
                game_root,
                container: &candidate.container,
                entry: &candidate.path,
                kind: AssetSource::Utoc,
            },
            layouts,
            ParseOptions::default(),
        ),
        // Bytecode needs no layout recovered from another Blueprint, so the plain parse does.
        None => rivals_uasset::parse_package_opts(
            &bundle,
            Some(mappings),
            None,
            ParseOptions {
                skip_twins: true,
                ..Default::default()
            },
        ),
    };
    let parsed = match parsed {
        Ok(parsed) => parsed,
        Err(reason) => return unreadable(reason),
    };
    let mut hits = Vec::new();
    search_package(&candidate.path, &parsed, query, &mut hits);
    let mod_name = &order.layers[candidate.layer].mod_name;
    for hit in &mut hits {
        hit.container = Some(candidate.container.to_string());
        hit.in_mod.clone_from(mod_name);
    }
    Outcome::Hits {
        found: hits.len(),
        hits,
    }
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
            container: None,
            in_mod: None,
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
        assert_eq!((all.searched, all.found), (4, 7));
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

    /// What the header says agrees with what the parse finds: a package is kept exactly when one of
    /// its exports is a function.
    #[test]
    fn the_header_filter_keeps_exactly_the_packages_whose_exports_are_functions() {
        let Some((root, mappings)) = install() else {
            return;
        };
        let order = load_order::open(&root, false, false).expect("containers");
        let filter = PathFilter::new(Some(SETTINGS));
        let listed = winning_copies(&order, &filter);
        assert!(listed.len() > 50, "{}", listed.len());
        let kept: HashSet<u64> = holding_functions(
            &order,
            winning_copies(&order, &filter),
            &never(),
            &|_, _, _| {},
        )
        .iter()
        .map(|candidate| candidate.id.0)
        .collect();
        assert!(!kept.is_empty());
        let converter = PackageConverter::new(&order);
        for candidate in &listed {
            let bundle = converter
                .convert(candidate.id, &candidate.path)
                .expect("converts");
            let parsed = rivals_uasset::parse_package(
                &AssetBundle {
                    asset: &bundle.asset_file_buffer,
                    exports: &bundle.exports_file_buffer,
                },
                Some(&mappings),
            )
            .expect("parses");
            let functions = parsed
                .exports
                .iter()
                .any(|export| export.class_name == "Function");
            assert_eq!(
                kept.contains(&candidate.id.0),
                functions,
                "{}",
                candidate.path
            );
        }
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
                filter: Some(SETTINGS),
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

    /// The first enabled mod's own package is read from that mod, and every hit says so.
    #[test]
    fn an_enabled_mod_s_package_is_searched_from_the_mod_that_ships_it() {
        let Some((root, mappings)) = install() else {
            return;
        };
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
                filter: Some(&path),
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
        let patch = crate::asset::newest_patch(&root).expect("a patch");
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
                container: &patch,
                entry: &candidate.path,
                kind: AssetSource::Utoc,
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
