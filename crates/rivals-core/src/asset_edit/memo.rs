//! The parse a save's last stage made, kept while the save runs, so the stages after it that read
//! the same bytes the same way take it rather than parsing the package again.

use std::cell::RefCell;
use std::sync::Arc;
use std::time::SystemTime;

use rivals_uasset::{AssetBundle, Mappings, ParseOptions, ParsedPackage};

use crate::asset::AssetSource;
use crate::schema_synth::{self, PackageSource};

/// What a parse read, and how: equal keys parse to the same package.
struct Key {
    asset: Vec<u8>,
    exports: Vec<u8>,
    source: Source,
    options: ParseOptions,
    mappings: Option<*const Mappings>,
}

/// A container's size and time.
type Stamp = Option<(u64, Option<SystemTime>)>;

#[derive(PartialEq)]
struct Source {
    game_root: String,
    container: String,
    entry: String,
    kind: AssetSource,
    /// The mod read through, with its container's size and time.
    mod_container: Option<(String, Stamp)>,
}

impl Source {
    fn of(source: &PackageSource<'_>) -> Self {
        Self {
            game_root: source.game_root.to_string(),
            container: source.container.to_string(),
            entry: source.entry.to_string(),
            kind: source.kind,
            mod_container: source.mod_container.as_ref().map(|path| {
                let stamp = std::fs::metadata(path)
                    .ok()
                    .map(|meta| (meta.len(), meta.modified().ok()));
                (path.clone(), stamp)
            }),
        }
    }
}

impl Key {
    fn matches(
        &self,
        bundle: &AssetBundle<'_>,
        source: &Source,
        options: ParseOptions,
        mappings: Option<*const Mappings>,
    ) -> bool {
        self.options == options
            && self.mappings == mappings
            && self.source == *source
            && self.asset == bundle.asset
            && self.exports == bundle.exports
    }
}

/// The parse kept, and what it read.
type Kept = Option<(Key, Arc<ParsedPackage>)>;

thread_local! {
    /// `None` outside a save. One parse is kept: each stage reads what the stage before it wrote.
    static LAST: RefCell<Option<Kept>> = const { RefCell::new(None) };
}

/// Keeps parses for as long as it lives, on this thread, and forgets them when dropped.
pub(crate) struct Scope {
    outer: Option<Kept>,
}

pub(crate) fn scope() -> Scope {
    Scope {
        outer: LAST.with(|last| last.replace(Some(None))),
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        let outer = self.outer.take();
        LAST.with(|last| *last.borrow_mut() = outer);
    }
}

/// [`schema_synth::parse_package_opts`], taking the parse kept from an earlier stage when it read
/// these very bytes the same way, and keeping this one for the next stage.
pub(crate) fn parse(
    bundle: &AssetBundle<'_>,
    mappings: Option<&Mappings>,
    source: &PackageSource<'_>,
    options: ParseOptions,
) -> Result<Arc<ParsedPackage>, String> {
    let keeping = LAST.with(|last| last.borrow().is_some()) && !switched_off();
    if !keeping {
        return schema_synth::parse_package_opts(bundle, mappings, source, options).map(Arc::new);
    }
    let pointer = mappings.map(std::ptr::from_ref);
    let key_source = Source::of(source);
    let kept = LAST.with(|last| {
        last.borrow()
            .as_ref()
            .and_then(Option::as_ref)
            .filter(|(key, _)| key.matches(bundle, &key_source, options, pointer))
            .map(|(_, parsed)| Arc::clone(parsed))
    });
    if let Some(parsed) = kept {
        return Ok(parsed);
    }
    let parsed = Arc::new(schema_synth::parse_package_opts(
        bundle, mappings, source, options,
    )?);
    let key = Key {
        asset: bundle.asset.to_vec(),
        exports: bundle.exports.to_vec(),
        source: key_source,
        options,
        mappings: pointer,
    };
    LAST.with(|last| {
        if let Some(slot) = last.borrow_mut().as_mut() {
            *slot = Some((key, Arc::clone(&parsed)));
        }
    });
    Ok(parsed)
}

#[cfg(test)]
thread_local! {
    static OFF: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn switched_off() -> bool {
    OFF.with(std::cell::Cell::get)
}

#[cfg(not(test))]
fn switched_off() -> bool {
    false
}

/// Runs `f` parsing at every stage, for a test comparing that with a save that keeps its parses.
#[cfg(test)]
pub(crate) fn without<T>(f: impl FnOnce() -> T) -> T {
    let was = OFF.with(|off| off.replace(true));
    let out = f();
    OFF.with(|off| off.set(was));
    out
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn source(entry: &str) -> PackageSource<'_> {
        PackageSource {
            game_root: "",
            container: "",
            entry,
            kind: AssetSource::Loose,
            mod_container: None,
        }
    }

    fn built() -> rivals_uasset::script_fixture::Built {
        rivals_uasset::script_fixture::Package::new("BP_Memo_C", &[]).build()
    }

    /// Inside a save the same bytes read the same way parse once; any other bytes, options,
    /// package or mappings parse again, and nothing is kept outside a save or after it.
    #[test]
    fn only_the_same_bytes_read_the_same_way_take_the_kept_parse() {
        let built = built();
        let bundle = built.bundle();
        let options = ParseOptions::default();
        let first = {
            let _save = scope();
            let first = parse(&bundle, None, &source("A"), options).expect("parses");
            let again = parse(&bundle, None, &source("A"), options).expect("parses");
            assert!(Arc::ptr_eq(&first, &again), "the same read parses once");

            // The class renamed by one letter in the name table.
            let mut changed = built.asset.clone();
            let at = changed
                .windows(9)
                .position(|window| window == b"BP_Memo_C")
                .expect("the class name");
            changed[at + 8] = b'D';
            let other = AssetBundle {
                asset: &changed,
                exports: &built.exports,
            };
            let renamed = parse(&other, None, &source("A"), options).expect("parses");
            assert!(
                !Arc::ptr_eq(&first, &renamed),
                "one byte changed parses again"
            );
            assert!(
                renamed.exports.iter().any(|e| e.object_name == "BP_Memo_D"),
                "the parse is of the changed bytes"
            );

            let declared = ParseOptions {
                declared_slots: true,
                ..options
            };
            let slots = parse(&bundle, None, &source("A"), declared).expect("parses");
            assert!(!Arc::ptr_eq(&first, &slots), "other options parse again");
            let elsewhere = parse(&bundle, None, &source("B"), declared).expect("parses");
            assert!(
                !Arc::ptr_eq(&slots, &elsewhere),
                "another entry parses again"
            );
            let mappings = rivals_uasset::script_fixture::native_parent_mappings();
            let mapped = parse(&bundle, Some(&mappings), &source("B"), declared).expect("parses");
            assert!(
                !Arc::ptr_eq(&elsewhere, &mapped),
                "other mappings parse again"
            );
            first
        };
        let after = parse(&bundle, None, &source("A"), options).expect("parses");
        assert!(
            !Arc::ptr_eq(&first, &after),
            "nothing is kept after the save"
        );
        let outside = parse(&bundle, None, &source("A"), options).expect("parses");
        assert!(
            !Arc::ptr_eq(&after, &outside),
            "nothing is kept outside a save"
        );
    }

    /// A save inside another keeps its own parse and leaves the outer one's as it found it.
    #[test]
    fn a_save_inside_another_leaves_the_outer_parse_kept() {
        let built = built();
        let bundle = built.bundle();
        let options = ParseOptions::default();
        let _outer = scope();
        let first = parse(&bundle, None, &source("A"), options).expect("parses");
        {
            let _inner = scope();
            let inner = parse(&bundle, None, &source("A"), options).expect("parses");
            assert!(!Arc::ptr_eq(&first, &inner), "the inner save starts empty");
        }
        let again = parse(&bundle, None, &source("A"), options).expect("parses");
        assert!(
            Arc::ptr_eq(&first, &again),
            "the outer save's parse is kept"
        );
    }
}
