import { ChangeSet, Text } from "@codemirror/state";
import { describe, expect, it } from "vitest";

import {
  countMatches,
  matchAtOrAfter,
  matchBefore,
  ordinalOf,
  replaceAllChanges,
} from "./iniSearch";

/// Drives a replace the way the editor does: build the change list against the document, then
/// apply it in one go. Anything wrong with the offsets shows up here as wrong text.
function runReplace(lines: string[], needle: string, replacement: string, caseSensitive = false) {
  const doc = Text.of(lines);
  const { changes, replaced } = replaceAllChanges(doc, needle, replacement, caseSensitive);
  const set = ChangeSet.of(changes, doc.length);
  return { text: set.apply(doc).toString(), replaced, changes, newLength: set.newLength };
}

/// What the replace has to agree with, written the obvious way.
function naive(lines: string[], needle: string, replacement: string, caseSensitive = false) {
  const text = lines.join("\n");
  if (caseSensitive) return text.split(needle).join(replacement);
  let out = "";
  const hay = text.toLowerCase();
  const ndl = needle.toLowerCase();
  let cut = 0;
  let at = hay.indexOf(ndl);
  while (at !== -1) {
    out += text.slice(cut, at) + replacement;
    cut = at + ndl.length;
    at = hay.indexOf(ndl, cut);
  }
  return out + text.slice(cut);
}

describe("replaceAllChanges", () => {
  it("rewrites every match and reports the count", () => {
    const { text, replaced } = runReplace(["a=1", "b=2", "a=3"], "a=", "z=");
    expect(replaced).toBe(2);
    expect(text).toBe("z=1\nb=2\nz=3");
  });

  it("emits one change per changed line, not one per match", () => {
    const { changes, replaced } = runReplace(["a,a,a", "b", "a"], "a", "x");
    expect(replaced).toBe(4);
    expect(changes.length).toBe(2);
  });

  it("leaves untouched lines out of the change list", () => {
    const lines = Array.from({ length: 100 }, (_, i) => (i % 10 === 0 ? "hit=1" : "miss=1"));
    const { changes, replaced } = runReplace(lines, "hit", "HIT");
    expect(replaced).toBe(10);
    expect(changes.length).toBe(10);
  });

  it("handles a match on the first and on the last line", () => {
    const { text, replaced } = runReplace(["hit", "miss", "hit"], "hit", "HIT");
    expect(replaced).toBe(2);
    expect(text).toBe("HIT\nmiss\nHIT");
  });

  it("does not re-match a replacement that contains the needle", () => {
    const { text, replaced } = runReplace(["x=1", "x=1"], "x", "xx");
    expect(replaced).toBe(2);
    expect(text).toBe("xx=1\nxx=1");
  });

  it("replaces several times on one line", () => {
    const { text, replaced } = runReplace(["a,a,a"], "a", "b");
    expect(replaced).toBe(3);
    expect(text).toBe("b,b,b");
  });

  it("leaves a document with no match alone", () => {
    const { text, replaced, changes } = runReplace(["a", "b"], "zzz", "!");
    expect(replaced).toBe(0);
    expect(changes).toEqual([]);
    expect(text).toBe("a\nb");
  });

  it("handles the empty and single-line cases", () => {
    expect(runReplace([""], "a", "b").replaced).toBe(0);
    expect(runReplace([""], "", "b").replaced).toBe(0);
    expect(runReplace(["only"], "only", "").text).toBe("");
  });

  it("matches without regard to case but keeps the text it did not touch", () => {
    const { text, replaced } = runReplace(["R.Fog=1", "r.FOG=2", "keep.R.Fog"], "r.fog", "r.Mist");
    expect(replaced).toBe(3);
    expect(text).toBe("r.Mist=1\nr.Mist=2\nkeep.r.Mist");
  });

  it("respects case when asked to", () => {
    const { text, replaced } = runReplace(["R.Fog=1", "r.Fog=2"], "r.Fog", "x", true);
    expect(replaced).toBe(1);
    expect(text).toBe("R.Fog=1\nx=2");
  });

  // Offsets are against the untouched document, so a shrinking replace is the case that would
  // expose an offset carried over from an edit that had already been applied.
  it("shrinking the document does not strand later offsets", () => {
    const lines = Array.from({ length: 50 }, (_, i) => `padding${i}=aaaaaaaaaa`);
    const { text } = runReplace(lines, "aaaaaaaaaa", "b");
    expect(text).toBe(lines.map((line) => line.replace("aaaaaaaaaa", "b")).join("\n"));
  });

  it("reports the length the document will have, for placing the cursor", () => {
    const { newLength, text } = runReplace(["aaa", "aaa"], "aaa", "b");
    expect(newLength).toBe(text.length);
  });

  it("agrees with a whole-document replace, growing and shrinking", () => {
    const lines = Array.from(
      { length: 50_000 },
      (_, i) => `+CVars=r.Setting${i % 97}.Quality=${i % 3}`
    );
    for (const needle of ["=0", "r.Setting7.", "zzz"]) {
      for (const replacement of ["<>", "", "=LONGER-REPLACEMENT-0"]) {
        const { text } = runReplace(lines, needle, replacement);
        expect(text, `needle ${needle} -> ${replacement}`).toBe(naive(lines, needle, replacement));
      }
    }
  });
});

describe("the search primitives agree with each other", () => {
  const lines = ["aaa", "zz", "aa", "banana", "aa"];
  const doc = Text.of(lines);

  it("counts what stepping through finds", () => {
    const { count } = countMatches(doc, "a", false, -1);
    const seen: number[] = [];
    let at = matchAtOrAfter(doc, "a", false, 0);
    while (at !== null) {
      seen.push(at);
      at = matchAtOrAfter(doc, "a", false, at + 1);
    }
    expect(seen.length).toBe(count);
  });

  it("gives each match the ordinal the counter would", () => {
    let at = matchAtOrAfter(doc, "aa", false, 0);
    let expected = 0;
    while (at !== null) {
      expect(ordinalOf(doc, "aa", false, at)).toBe(expected);
      expected += 1;
      at = matchAtOrAfter(doc, "aa", false, at + 1);
    }
    expect(expected).toBe(countMatches(doc, "aa", false, -1).count);
  });

  it("walks backwards over the same positions", () => {
    const forward: number[] = [];
    let at = matchAtOrAfter(doc, "a", false, 0);
    while (at !== null) {
      forward.push(at);
      at = matchAtOrAfter(doc, "a", false, at + 1);
    }
    const backward: number[] = [];
    let before = matchBefore(doc, "a", false, doc.length);
    while (before !== null) {
      backward.push(before);
      before = matchBefore(doc, "a", false, before);
    }
    expect(backward.reverse()).toEqual(forward);
  });
});
