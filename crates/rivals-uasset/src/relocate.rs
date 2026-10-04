//! Moves everything that points into a script when edits change the length of parts of it.
//!
//! A change replaces one run of a script's bytes with others that may take more or less room, both
//! on disk and once loaded. Every code offset is a loaded offset, so the loaded lengths decide
//! where each jump has to land afterwards: [`OffsetMap`] says, for each old offset, where the same
//! code starts once the changes are made. The script's own jumps, switch arms, context skips and
//! latent resume points are rewritten through it, then its size words, then whatever other
//! functions hold into it: the entry each event stub passes an event graph, and a latent resume
//! point another function's code holds.
//!
//! A script written anew from text maps by its labels instead: each label the text kept, named
//! after an offset the script had, says where that code starts now, and nothing else survives.

use std::collections::BTreeMap;

use crate::kismet::{self, Fixup, FixupKind};
use crate::package::ParsedPackage;
use crate::write::Splice;

/// One run of a script's bytes replaced: `[at, end_at)` in the file, `[offset, end_offset)` once
/// loaded, by `bytes` that take `loaded_len` once loaded.
#[derive(Debug, Clone)]
pub(crate) struct Change {
    pub at: u64,
    pub end_at: u64,
    pub offset: u32,
    pub end_offset: u32,
    pub bytes: Vec<u8>,
    pub loaded_len: u32,
    /// What the change is, for a refusal that has to name it.
    pub label: String,
}

impl Change {
    fn loaded_delta(&self) -> i64 {
        i64::from(self.loaded_len) - i64::from(self.end_offset - self.offset)
    }

    fn file_delta(&self) -> i64 {
        self.bytes.len() as i64 - (self.end_at - self.at) as i64
    }
}

/// Where each old loaded offset lands once a script's changes are made.
#[derive(Debug, Clone, Default)]
pub(crate) struct OffsetMap {
    /// `(offset, end_offset, loaded delta)` for each change, in order.
    moves: Vec<(u32, u32, i64)>,
    /// For a script written anew from text: each old offset the text kept a label for, to where
    /// that label is now. An offset without one is gone.
    labels: Option<BTreeMap<u32, u32>>,
}

impl OffsetMap {
    pub fn new(changes: &[Change]) -> Self {
        Self::from_moves(
            changes
                .iter()
                .map(|change| (change.offset, change.end_offset, change.loaded_delta()))
                .collect(),
        )
    }

    pub fn from_moves(mut moves: Vec<(u32, u32, i64)>) -> Self {
        moves.sort_by_key(|(offset, _, _)| *offset);
        Self {
            moves,
            labels: None,
        }
    }

    /// The map a script written anew from text makes: old offset to new, by the labels it kept.
    pub fn from_labels(labels: BTreeMap<u32, u32>) -> Self {
        Self {
            moves: Vec::new(),
            labels: Some(labels),
        }
    }

    /// Whether this maps by a text's labels, which can drop an offset without moving anything.
    pub fn labelled(&self) -> bool {
        self.labels.is_some()
    }

    /// Whether nothing moves at all.
    pub fn is_identity(&self) -> bool {
        match &self.labels {
            Some(labels) => labels.iter().all(|(old, new)| old == new),
            None => self.moves.iter().all(|(_, _, delta)| *delta == 0),
        }
    }

    /// Where code that started at `target` starts now. A target inside a replaced run names code
    /// that is gone, so it has nowhere to land; the start of a run lands on its replacement.
    pub fn map(&self, target: u32) -> Result<u32, String> {
        if let Some(labels) = &self.labels {
            return labels
                .get(&target)
                .copied()
                .ok_or_else(|| format!("the text keeps no label @{target:04X} for it"));
        }
        let mut shift = 0i64;
        for (offset, end_offset, delta) in &self.moves {
            if *offset < target && target < *end_offset {
                return Err(format!(
                    "0x{target:04X} is inside the code replaced at 0x{offset:04X}"
                ));
            }
            if *end_offset <= target {
                shift += delta;
            }
        }
        u32::try_from(i64::from(target) + shift)
            .map_err(|_| format!("0x{target:04X} would move outside the script"))
    }
}

/// What relocating one script wrote, for the report.
#[derive(Debug, Clone, Default)]
pub(crate) struct Relocated {
    /// Each splice with what it writes, so two that collide can be named.
    pub splices: Vec<(Splice, String)>,
    pub jumps: usize,
    pub entries: usize,
    pub linkages: usize,
    /// Each entry and resume point another function holds that moved: what it is, and the offset
    /// it held before and holds now.
    pub moved: Vec<(String, u32, u32)>,
    /// `((loaded, stored) before, after)`, when the size changed.
    pub sizes: Option<((u32, u32), (u32, u32))>,
    /// Changes another function has to take for this one: an event stub whose literal cannot hold
    /// the offset its event enters at now, by the stub's export.
    pub induced: Vec<(u32, Change)>,
}

/// What an event stub's literal takes when the offset its event enters at moves: the new value in
/// place where it fits, or an `IntConst` in place of the literal, which makes the stub longer.
pub(crate) enum HolderChange {
    InPlace(Splice),
    Widened(Change),
}

pub(crate) fn holder_change(
    entry: &kismet::Entry,
    new: u32,
    label: &str,
) -> Result<HolderChange, String> {
    let span = &entry.span;
    match span.token {
        0x1D => Ok(HolderChange::InPlace(word(span.at + 1, new))),
        0x2C if new <= 0xFF => Ok(HolderChange::InPlace(Splice {
            start: span.at + 1,
            end: span.at + 2,
            bytes: vec![new as u8],
        })),
        0x2C | 0x25 | 0x26 => {
            let value = i32::try_from(new)
                .map_err(|_| format!("{label} 0x{new:X}, past what an entry can hold"))?;
            let mut bytes = vec![0x1D];
            bytes.extend_from_slice(&value.to_le_bytes());
            Ok(HolderChange::Widened(Change {
                at: span.at,
                end_at: span.end_at,
                offset: span.offset,
                end_offset: span.end_offset,
                bytes,
                loaded_len: 5,
                label: label.to_string(),
            }))
        }
        other => Err(format!(
            "{label} is held by a {} that cannot hold 0x{new:04X}",
            kismet::token_name(other).unwrap_or("literal")
        )),
    }
}

/// The splices that make `changes` in the script of `export` and move everything pointing into it.
pub(crate) fn relocate(
    parsed: &ParsedPackage,
    export: u32,
    changes: Vec<Change>,
) -> Result<Relocated, String> {
    let map = OffsetMap::new(&changes);
    relocate_through(parsed, export, changes, map)
}

/// [`relocate`], with where each old offset lands given rather than worked out from the changes:
/// what a script written anew from text needs, whose labels say it.
pub(crate) fn relocate_through(
    parsed: &ParsedPackage,
    export: u32,
    mut changes: Vec<Change>,
    map: OffsetMap,
) -> Result<Relocated, String> {
    let found = parsed
        .exports
        .iter()
        .find(|e| e.index == export)
        .ok_or_else(|| format!("no export {export}"))?;
    let script = found
        .script
        .as_ref()
        .ok_or_else(|| format!("{} carries no bytecode", found.object_name))?;
    changes.sort_by_key(|change| change.at);
    for pair in changes.windows(2) {
        if pair[0].end_at > pair[1].at {
            return Err(format!(
                "two edits in {} change the same bytes",
                found.object_name
            ));
        }
    }
    let file_delta: i64 = changes.iter().map(Change::file_delta).sum();
    let loaded_delta: i64 = changes.iter().map(Change::loaded_delta).sum();
    let resized = !map.is_identity() || file_delta != 0;
    if resized && let Some(lock) = script.resize_lock() {
        return Err(format!(
            "{} has to keep its size, so this edit cannot change the length of anything in it: {lock}",
            found.object_name
        ));
    }
    let mut out = Relocated::default();
    let replaced = |at: u64| {
        changes
            .iter()
            .any(|change| change.at <= at && at < change.end_at)
    };
    for change in &changes {
        out.splices.push((
            Splice {
                start: change.at,
                end: change.end_at,
                bytes: change.bytes.clone(),
            },
            change.label.clone(),
        ));
    }
    // A text can drop a label something enters at without moving anything, so its labels are
    // checked whether or not the script changes size.
    if !resized && !map.labelled() {
        return Ok(out);
    }
    for fixup in script.fixups.iter().filter(|fixup| !replaced(fixup.at)) {
        if let Some((old, new)) = remapped(fixup, &map, found.object_name.as_str())?
            && old != new
        {
            out.splices.push((
                word(fixup.at, new),
                format!("an offset in {}", found.object_name),
            ));
            out.jumps += 1;
        }
    }
    if resized {
        let loaded = shifted(script.buffer_size, loaded_delta, &found.object_name)?;
        let stored = shifted(script.storage_size, file_delta, &found.object_name)?;
        let mut words = loaded.to_le_bytes().to_vec();
        words.extend_from_slice(&stored.to_le_bytes());
        out.splices.push((
            Splice {
                start: script.sizes_at,
                end: script.sizes_at + 8,
                bytes: words,
            },
            format!("{}'s size words", found.object_name),
        ));
        out.sizes = Some(((script.buffer_size, script.storage_size), (loaded, stored)));
    }

    let functions = kismet::functions_of(&parsed.exports);
    if let Some(target) = functions.iter().find(|f| f.export == export) {
        let inbound = kismet::inbound(&functions, target);
        for entry in &inbound.entries {
            let holder = name_of(parsed, entry.export);
            let new = map.map(entry.offset).map_err(|reason| {
                format!(
                    "{holder} enters {} at 0x{:04X}, and this edit leaves it nowhere to land: {reason}",
                    found.object_name, entry.offset
                )
            })?;
            if new == entry.offset {
                continue;
            }
            let what = format!("the offset {holder} enters {} at", found.object_name);
            match holder_change(entry, new, &what)? {
                HolderChange::InPlace(splice) => out.splices.push((splice, what.clone())),
                HolderChange::Widened(change) => out.induced.push((entry.export, change)),
            }
            out.moved.push((what, entry.offset, new));
            out.entries += 1;
        }
        for (holder, fixup) in &inbound.linkages {
            let FixupKind::Absolute { target, .. } = &fixup.kind else {
                continue;
            };
            let holder = name_of(parsed, *holder);
            let new = map.map(*target).map_err(|reason| {
                format!(
                    "a latent action in {holder} resumes {} at 0x{target:04X}, and this edit leaves it nowhere to land: {reason}",
                    found.object_name
                )
            })?;
            if new != *target {
                let what = format!("a latent action in {holder} resuming {}", found.object_name);
                out.splices.push((word(fixup.at, new), what.clone()));
                out.moved.push((what, *target, new));
                out.linkages += 1;
            }
        }
    }
    Ok(out)
}

/// The value a fixup holds now and the one it takes: an offset into this script follows the map,
/// and a length of code follows its two ends. An offset into another function's code is left to
/// that function's own edits.
fn remapped(fixup: &Fixup, map: &OffsetMap, owner: &str) -> Result<Option<(u32, u32)>, String> {
    let context = |reason: String| {
        format!(
            "{owner}: a {:?} at file {:#X}: {reason}",
            fixup.source, fixup.at
        )
    };
    match &fixup.kind {
        FixupKind::Absolute { target, resumes } => {
            if resumes.as_deref().is_some_and(|name| name != owner) {
                return Ok(None);
            }
            Ok(Some((*target, map.map(*target).map_err(context)?)))
        }
        FixupKind::Relative { from, to } => {
            let new = map.map(*to).map_err(context)? - map.map(*from).map_err(context)?;
            Ok(Some((to - from, new)))
        }
    }
}

fn name_of(parsed: &ParsedPackage, export: u32) -> String {
    parsed
        .exports
        .iter()
        .find(|e| e.index == export)
        .map_or_else(|| format!("export {export}"), |e| e.object_name.clone())
}

fn word(at: u64, value: u32) -> Splice {
    Splice {
        start: at,
        end: at + 4,
        bytes: value.to_le_bytes().to_vec(),
    }
}

fn shifted(size: u32, delta: i64, owner: &str) -> Result<u32, String> {
    u32::try_from(i64::from(size) + delta)
        .map_err(|_| format!("{owner}'s script would take a size no script can hold"))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn change(offset: u32, end_offset: u32, loaded_len: u32) -> Change {
        Change {
            at: u64::from(offset),
            end_at: u64::from(end_offset),
            offset,
            end_offset,
            bytes: vec![0; loaded_len as usize],
            loaded_len,
            label: String::new(),
        }
    }

    /// Code before a change stays put, code after it moves by what the change added, the start of
    /// a change lands on its replacement, and code inside one is gone.
    #[test]
    fn an_offset_moves_by_what_the_changes_before_it_added() {
        let map = OffsetMap::new(&[change(10, 14, 9), change(30, 40, 2)]);
        assert_eq!(map.map(0), Ok(0));
        assert_eq!(map.map(10), Ok(10));
        assert_eq!(map.map(14), Ok(19));
        assert_eq!(map.map(30), Ok(35));
        assert_eq!(map.map(40), Ok(37));
        assert_eq!(map.map(50), Ok(47));
        assert!(map.map(12).is_err());
        assert!(map.map(35).is_err());
        assert!(!map.is_identity());
        assert!(OffsetMap::new(&[change(10, 14, 4)]).is_identity());
    }

    /// A text's labels say where each offset they kept lands, and an offset they did not keep has
    /// nowhere to land, even where nothing moved.
    #[test]
    fn a_label_map_moves_only_the_offsets_its_text_kept() {
        let map = OffsetMap::from_labels(BTreeMap::from([(0x0A, 0x0A), (0x92, 0xA9)]));
        assert!(map.labelled());
        assert_eq!(map.map(0x0A), Ok(0x0A));
        assert_eq!(map.map(0x92), Ok(0xA9));
        let missing = map.map(0x18).expect_err("dropped");
        assert!(missing.contains("no label @0018"), "{missing}");
        assert!(!map.is_identity());
        assert!(OffsetMap::from_labels(BTreeMap::from([(4, 4)])).is_identity());
    }
}
