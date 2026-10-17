import type { Text } from "@codemirror/state";

// Search primitives for the Pak INI editor, kept off the component so the offset
// arithmetic can be exercised on its own.
//
// A config INI can be tens of megabytes, so nothing here flattens the document with
// `toString()`. Scanning a line at a time keeps the working set to a single line, and it
// stays exact because an INI assignment never spans a newline and the needle comes from a
// single-line input.

/// Report the start offset of every occurrence of `needle` in lines `fromLine..toLine`
/// (1-based, inclusive).
export function scanLines(
  doc: Text,
  needle: string,
  caseSensitive: boolean,
  fromLine: number,
  toLine: number,
  onMatch: (from: number) => void
): void {
  const ndl = caseSensitive ? needle : needle.toLowerCase();
  if (!ndl || fromLine > toLine || fromLine < 1) return;

  let pos = doc.line(fromLine).from;
  for (const iter = doc.iterLines(fromLine, Math.min(toLine, doc.lines) + 1); !iter.next().done; ) {
    const line = iter.value;
    const hay = caseSensitive ? line : line.toLowerCase();
    let idx = hay.indexOf(ndl);
    while (idx !== -1) {
      onMatch(pos + idx);
      idx = hay.indexOf(ndl, idx + ndl.length);
    }
    pos += line.length + 1;
  }
}

export interface MatchCount {
  count: number;
  index: number;
}

/// Total occurrences, plus the ordinal of the match starting exactly at `anchor`, or -1
/// when the cursor is not sitting on a match.
///
/// Counting beats collecting positions: a common needle in a large INI has hundreds of
/// thousands of hits, and all the UI needs is a counter. The ordinal has to be exact
/// rather than "the next one from here", or the counter claims a position the highlight
/// does not agree with.
export function countMatches(
  doc: Text,
  needle: string,
  caseSensitive: boolean,
  anchor: number
): MatchCount {
  let count = 0;
  let index = -1;
  scanLines(doc, needle, caseSensitive, 1, doc.lines, (pos) => {
    if (pos === anchor && index === -1) index = count;
    count += 1;
  });
  return { count, index };
}

/// Ordinal of the match starting exactly at `target`, or -1 if none does.
///
/// Only counts up to the target's line, so jumping near the top of a large file stays
/// cheap. Used to recover the ordinal after the cursor lands somewhere the running count
/// did not predict, for instance when the editor is rebuilt by a reload.
export function ordinalOf(
  doc: Text,
  needle: string,
  caseSensitive: boolean,
  target: number
): number {
  const line = doc.lineAt(Math.max(0, Math.min(target, doc.length))).number;
  let count = 0;
  let index = -1;
  scanLines(doc, needle, caseSensitive, 1, line, (pos) => {
    if (pos < target) count += 1;
    else if (pos === target && index === -1) index = count;
  });
  return index;
}

/// First match at or after `from`, or null. Stops at the first hit rather than walking
/// the whole document.
///
/// Every line is enumerated from its start with the same non-overlapping stride the
/// counter uses. Resuming mid-line instead would report an overlapping occurrence that
/// the counter never counted, so `next` could land somewhere `1/90` does not describe.
export function matchAtOrAfter(
  doc: Text,
  needle: string,
  caseSensitive: boolean,
  from: number
): number | null {
  const ndl = caseSensitive ? needle : needle.toLowerCase();
  if (!ndl) return null;
  const clamped = Math.max(0, Math.min(from, doc.length));
  const startLine = doc.lineAt(clamped).number;
  let pos = doc.line(startLine).from;
  for (const iter = doc.iterLines(startLine); !iter.next().done; ) {
    const line = iter.value;
    const hay = caseSensitive ? line : line.toLowerCase();
    let at = hay.indexOf(ndl);
    while (at !== -1) {
      if (pos + at >= clamped) return pos + at;
      at = hay.indexOf(ndl, at + ndl.length);
    }
    pos += line.length + 1;
  }
  return null;
}

/// Last match starting strictly before `before`, or null. Walks backwards a line at a
/// time, bounded by the gap to the previous match, enumerating each line forward so it
/// agrees with the counter.
export function matchBefore(
  doc: Text,
  needle: string,
  caseSensitive: boolean,
  before: number
): number | null {
  const ndl = caseSensitive ? needle : needle.toLowerCase();
  if (!ndl) return null;
  const clamped = Math.max(0, Math.min(before, doc.length));
  for (let n = doc.lineAt(clamped).number; n >= 1; n--) {
    const line = doc.line(n);
    const hay = caseSensitive ? line.text : line.text.toLowerCase();
    let found: number | null = null;
    let at = hay.indexOf(ndl);
    while (at !== -1 && line.from + at < clamped) {
      found = line.from + at;
      at = hay.indexOf(ndl, at + ndl.length);
    }
    if (found !== null) return found;
  }
  return null;
}

export interface LineChange {
  from: number;
  to: number;
  insert: string;
}

/// Every rewrite a Replace All would make, as one change list against `doc`, plus how many
/// occurrences it covers.
///
/// Whole lines rather than one change per match. A spec per occurrence is hundreds of thousands of
/// objects on a large config, where the lines that actually change are a fraction of that, and a
/// per-line change set leaves the untouched majority of the rope shared rather than rebuilt. The
/// previous version built an array of every line, changed or not, then past a threshold swapped the
/// whole rope, so a replace on a config of a million lines cost several full copies of it.
///
/// One list rather than batches applied as they are produced. Batching bounded what was alive
/// during the rewrite, but each dispatch is its own history event: measured on a real 143 MB
/// config, a Replace All landed as 117 of them, so undo put back a hundredth of the file per press.
export function replaceAllChanges(
  doc: Text,
  needle: string,
  replacement: string,
  caseSensitive: boolean
): { changes: LineChange[]; replaced: number } {
  const ndl = caseSensitive ? needle : needle.toLowerCase();
  const changes: LineChange[] = [];
  if (!ndl) return { changes, replaced: 0 };

  let replaced = 0;
  let pos = 0;
  for (const iter = doc.iterLines(); !iter.next().done; ) {
    const line = iter.value;
    const hay = caseSensitive ? line : line.toLowerCase();
    let at = hay.indexOf(ndl);
    if (at !== -1) {
      let out = "";
      let cut = 0;
      while (at !== -1) {
        out += line.slice(cut, at) + replacement;
        cut = at + ndl.length;
        replaced += 1;
        at = hay.indexOf(ndl, cut);
      }
      out += line.slice(cut);
      changes.push({ from: pos, to: pos + line.length, insert: out });
    }
    pos += line.length + 1;
  }
  return { changes, replaced };
}
