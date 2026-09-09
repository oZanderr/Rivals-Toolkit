import { useState, useEffect, useRef, useCallback, useMemo } from "react";

import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { EditorState, StateField, StateEffect, RangeSetBuilder, Text } from "@codemirror/state";
import {
  EditorView,
  keymap,
  Decoration,
  ViewPlugin,
  type DecorationSet,
  type ViewUpdate,
} from "@codemirror/view";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open } from "@tauri-apps/plugin-dialog";
import {
  AlertTriangle,
  CheckCircle2,
  XCircle,
  RefreshCw,
  Save,
  Search,
  FilePlus2,
  FolderOpen,
  FileText,
  ListRestart,
  CaseSensitive,
  ChevronUp,
  ChevronDown,
  Plus,
  Replace,
  ReplaceAll,
  Trash2,
  Undo2,
  UploadCloud,
  X,
} from "lucide-react";

import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Tip } from "@/components/ui/tooltip";
import {
  countMatches,
  matchAtOrAfter,
  matchBefore,
  ordinalOf,
  replaceAllLines,
  scanLines,
} from "@/lib/iniSearch";
import { emitModsChanged, normalizeFolderPath, onModsChanged } from "@/lib/modsEvents";
import { emitPakChanged, onPakChanged } from "@/lib/pakEvents";
import { previewPakFilename } from "@/lib/pakName";
import { unreadableScanMessage, type PakScanError } from "@/lib/pakScan";
import { cn } from "@/lib/utils";

// ── Types matching Rust backend ─────────────────────────────────────

interface PakIniListing {
  pak_name: string;
  pak_path: string;
  ini_entries: string[];
}

interface PakIniFileContent {
  entry: string;
  content: string;
}

type NoticeType = "ok" | "err" | "info";

interface Props {
  gamePath: string;
  isActive: boolean;
  gameRunning: boolean;
}

// ── Search highlight CM extension ───────────────────────────────────

const setSearchHighlight = StateEffect.define<{
  search: string;
  caseSensitive: boolean;
}>();

const searchConfigField = StateField.define<{
  search: string;
  caseSensitive: boolean;
}>({
  create: () => ({ search: "", caseSensitive: false }),
  update(value, tr) {
    for (const e of tr.effects) {
      if (e.is(setSearchHighlight)) return e.value;
    }
    return value;
  },
});

const matchMark = Decoration.mark({ class: "cm-search-match" });
const currentMatchMark = Decoration.mark({ class: "cm-search-match-current" });

// Counting every match walks the whole file, so it trails typing rather than running per
// keystroke. Highlights and the jump to the first hit stay immediate.
const COUNT_DEBOUNCE_MS = 200;
// Coalesces the keystrokes that feed the counter into one state change.
const DOC_VERSION_DEBOUNCE_MS = 150;
// Past this many changed lines, Replace All swaps the whole rope instead of listing every
// line, trading granular undo for a bounded transaction.
const REPLACE_ALL_LINE_LIMIT = 20000;
// Undo history is kept for this many recently visited entries, or this many characters of
// document, whichever binds first. Dirty entries are exempt.
const MAX_CACHED_STATES = 6;
const MAX_CACHED_STATE_BYTES = 8_000_000;

// The entry travels with the state so eviction never has to parse it back out of the key,
// which contains a Windows path.
interface CachedEditor {
  state: EditorState;
  entry: string;
}

// The scan inputs travel with the result so "still counting" is derived, not stored.
interface MatchInfo {
  count: number;
  index: number;
  term: string;
  caseSensitive: boolean;
  version: number;
}

const EMPTY_MATCH_INFO: MatchInfo = {
  count: 0,
  index: -1,
  term: "",
  caseSensitive: false,
  version: -1,
};

function buildSearchDecos(view: EditorView): DecorationSet {
  const { search, caseSensitive } = view.state.field(searchConfigField);
  if (!search) return Decoration.none;

  const doc = view.state.doc;
  const selFrom = view.state.selection.main.from;
  const builder = new RangeSetBuilder<Decoration>();

  // Only what is on screen gets decorated. The match counter does its own full pass.
  let lastLine = 0;
  for (const { from, to } of view.visibleRanges) {
    const fromLine = Math.max(doc.lineAt(from).number, lastLine + 1);
    const toLine = doc.lineAt(to).number;
    if (toLine < fromLine) continue;
    scanLines(doc, search, caseSensitive, fromLine, toLine, (pos) => {
      builder.add(pos, pos + search.length, pos === selFrom ? currentMatchMark : matchMark);
    });
    lastLine = toLine;
  }
  return builder.finish();
}

const searchHighlightPlugin = ViewPlugin.fromClass(
  class {
    decorations: DecorationSet;
    constructor(view: EditorView) {
      this.decorations = buildSearchDecos(view);
    }
    update(update: ViewUpdate) {
      // Scrolling has to rescan because decorations only cover the viewport, and a
      // selection change moves the current-match marker. Both are cheap now that a
      // rebuild touches a screenful of lines rather than the whole document.
      if (
        update.docChanged ||
        update.viewportChanged ||
        update.selectionSet ||
        update.transactions.some((t) => t.effects.some((e) => e.is(setSearchHighlight)))
      ) {
        this.decorations = buildSearchDecos(update.view);
      }
    }
  },
  { decorations: (v) => v.decorations }
);

const searchExtension = [searchConfigField, searchHighlightPlugin];

// ── Helpers ─────────────────────────────────────────────────────────

function entryBasename(entry: string): string {
  const parts = entry.split(/[/\\]/);
  return parts[parts.length - 1] || entry;
}

function entryParentDir(entry: string): string {
  const normalized = entry.replace(/\\/g, "/");
  const idx = normalized.lastIndexOf("/");
  return idx >= 0 ? normalized.slice(0, idx) : "";
}

function normalizeLineEndings(s: string): string {
  return s.replace(/\r\n/g, "\n");
}

// In-pak path convention: every entry stored in `contents` is the full path
// repak returns, which always has the UE mount prefix prepended.
const MOUNT_PREFIX = "../../../";

function ensureMountPrefix(p: string): string {
  const trimmed = p.trim().replace(/\\/g, "/");
  return trimmed.startsWith(MOUNT_PREFIX) ? trimmed : MOUNT_PREFIX + trimmed.replace(/^\/+/, "");
}

// The mount prefix is internal; strip it for anything user-facing.
function stripMountPrefixForDisplay(p: string): string {
  const normalized = p.replace(/\\/g, "/");
  return normalized.startsWith(MOUNT_PREFIX) ? normalized.slice(MOUNT_PREFIX.length) : normalized;
}

// Canonical in-pak destination paths for each preset, mirroring where the
// matching files live in pakchunk0. WindowsEngine and BaseEngine live under
// `Engine/Config/`; DefaultEngine and DefaultDeviceProfiles under `Marvel/Config/`.
const PRESET_INI_PATHS: Record<string, string> = {
  "DefaultEngine.ini": "../../../Marvel/Config/DefaultEngine.ini",
  "BaseEngine.ini": "../../../Engine/Config/BaseEngine.ini",
  "WindowsEngine.ini": "../../../Engine/Config/Windows/WindowsEngine.ini",
  "DefaultDeviceProfiles.ini": "../../../Marvel/Config/DefaultDeviceProfiles.ini",
};

const PRESET_INI_FILES = Object.keys(PRESET_INI_PATHS) as readonly string[];

// Parent dir for the inline custom-path placeholder, derived from existing
// entries so the suggestion stays consistent with how the pak is organized.
function inferCustomParentDir(existingEntries: string[]): string {
  for (const entry of existingEntries) {
    const parent = entryParentDir(entry);
    if (parent) return stripMountPrefixForDisplay(parent);
  }
  return "Marvel/Config";
}

// ── Component ───────────────────────────────────────────────────────

export function PakIniEditor({ gamePath, isActive, gameRunning }: Props) {
  // ── Pak selection ──
  const [paks, setPaks] = useState<PakIniListing[]>([]);
  const [selectedPak, setSelectedPak] = useState<PakIniListing | null>(null);
  const [scanning, setScanning] = useState(false);
  const [loading, setLoading] = useState(false);

  // ── Editor content (per INI entry) ──
  const [activeEntry, setActiveEntry] = useState<string | null>(null);
  // Text as loaded from disk. It seeds a fresh editor and lists the tabs; it is not
  // updated per keystroke, because the live CodeMirror document is the source of truth.
  const [contents, setContents] = useState<Record<string, string>>({});
  // Entries edited since the last load or save. A name instead of a second full copy of
  // the file: comparing multi-megabyte strings on every render is what this replaces.
  const [dirtySet, setDirtySet] = useState<ReadonlySet<string>>(new Set());
  // Entries that exist in the pak on disk, so a delete knows whether it has to reach it.
  const onDiskRef = useRef<Set<string>>(new Set());
  // Entries queued for deletion on next save. Hidden from tabs but kept in
  // `contents` so discard cleanly restores them.
  const [pendingDeletes, setPendingDeletes] = useState<Set<string>>(new Set());
  const [deleteConfirm, setDeleteConfirm] = useState<string | null>(null);
  // Pak the user asked to open while edits are unsaved, held until they confirm. The
  // wording is snapshotted rather than derived: acting on the dialog changes both the
  // selected pak and the unsaved count, which would rewrite the text mid close animation.
  // Keeping the snapshot after the dialog closes is what holds the layout steady.
  const [pakSwitchPrompt, setPakSwitchPrompt] = useState<{
    pak: PakIniListing;
    fromName: string;
    changeCount: number;
  } | null>(null);
  const [pakSwitchOpen, setPakSwitchOpen] = useState(false);
  const [addOpen, setAddOpen] = useState(false);
  const [addCustomPath, setAddCustomPath] = useState("");
  const [adding, setAdding] = useState(false);
  const [newPakOpen, setNewPakOpen] = useState(false);
  const [newPakName, setNewPakName] = useState("");
  const [creatingPak, setCreatingPak] = useState(false);
  const [saving, setSaving] = useState(false);

  // ── Search ──
  const [searchOpen, setSearchOpen] = useState(false);
  const [searchTerm, setSearchTerm] = useState("");
  const [replaceTerm, setReplaceTerm] = useState("");
  const [caseSensitive, setCaseSensitive] = useState(false);
  const searchInputRef = useRef<HTMLInputElement>(null);
  // Current search state, read from callbacks that outlive the render that created them
  // (the CodeMirror update listener and the editor creation effect).
  const searchStateRef = useRef({ open: false, term: "", caseSensitive: false });
  useEffect(() => {
    searchStateRef.current = { open: searchOpen, term: searchTerm, caseSensitive };
  });
  // Bumped on every document change so the debounced count knows to rerun. Cheap on
  // purpose: the old listener pushed the whole document into React state instead.
  const [docVersion, setDocVersion] = useState(0);

  // ── CodeMirror ──
  const editorContainerRef = useRef<HTMLDivElement>(null);
  const editorViewRef = useRef<EditorView | null>(null);
  // Bumped on disk reload / pak switch to force editor recreation.
  const [pakEpoch, setPakEpoch] = useState(0);
  // Top-of-viewport document position, keyed by `${pak_path}::${entry}`. Stored
  // as a CM document offset (not pixels) so restore goes through CM's measurement
  // cycle and renders the lines instead of leaving a blank viewport.
  const entryScrollRef = useRef<Record<string, number>>({});
  // Cached per-entry CodeMirror state (undo history + selection) so tab switches
  // preserve history. Keyed by `${pak_path}::${entry}`; restored only on doc match.
  const editorStatesRef = useRef<Map<string, CachedEditor>>(new Map());
  // Mirror of dirtySet for the cache eviction, which runs from closures created before
  // the current render.
  const dirtyRef = useRef<ReadonlySet<string>>(new Set());
  useEffect(() => {
    dirtyRef.current = dirtySet;
  });

  // ── Drag-and-drop ──
  const [isDragging, setIsDragging] = useState(false);
  const isActiveRef = useRef(isActive);
  useEffect(() => {
    isActiveRef.current = isActive;
  }, [isActive]);

  // ── Notices ──
  const [notice, setNotice] = useState<{ msg: string; type: NoticeType } | null>(null);
  const noticeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  function showNotice(msg: string, type: NoticeType, duration = 4000) {
    if (noticeTimer.current) clearTimeout(noticeTimer.current);
    setNotice({ msg, type });
    noticeTimer.current = setTimeout(() => setNotice(null), duration);
  }

  // ── Dirty detection ──
  // An entry is "edit-dirty" if its in-memory content differs from disk; this
  // naturally covers both modifications (saved value differs) and brand-new
  // entries (saved value is undefined). Pending deletes are tracked separately.
  const dirtyEntries = useMemo(
    () => [...dirtySet].filter((entry) => !pendingDeletes.has(entry)),
    [dirtySet, pendingDeletes]
  );
  // Tabs ignore pending-delete entries; new entries naturally appear via
  // Object.keys order (insertion-order).
  const displayedEntries = useMemo(
    () => Object.keys(contents).filter((e) => !pendingDeletes.has(e)),
    [contents, pendingDeletes]
  );
  // Save fires if there are edits OR pending deletes of entries that were on disk.
  const realDeletes = useMemo(
    () => [...pendingDeletes].filter((e) => onDiskRef.current.has(e)),
    [pendingDeletes]
  );
  const hasRealDeletes = realDeletes.length > 0;
  const isDirty = dirtyEntries.length > 0 || hasRealDeletes;
  // A queued delete is an unsaved change too, so the counter has to include it or a
  // delete-only state reads as "Unsaved (0 files)".
  const pendingChanges = useMemo(
    () => [...dirtyEntries, ...realDeletes],
    [dirtyEntries, realDeletes]
  );

  const currentContent = activeEntry !== null ? (contents[activeEntry] ?? null) : null;

  const markDirty = useCallback((entry: string) => {
    setDirtySet((prev) => (prev.has(entry) ? prev : new Set(prev).add(entry)));
  }, []);

  // Cache the outgoing editor state, then drop the least recently used clean ones. Each
  // cached state pins a document and its undo history, so visiting several tabs in a pak
  // of large INIs used to hold all of them for the life of the window.
  //
  // A dirty entry is never evicted: after the React mirror of the document went away its
  // state is the only place those unsaved edits live.
  const cacheEditorState = useCallback((key: string, state: EditorState, entry: string) => {
    const cache = editorStatesRef.current;
    cache.delete(key);
    cache.set(key, { state, entry });

    let bytes = 0;
    for (const cached of cache.values()) bytes += cached.state.doc.length;
    for (const [candidate, cached] of cache) {
      if (cache.size <= MAX_CACHED_STATES && bytes <= MAX_CACHED_STATE_BYTES) break;
      if (candidate === key || dirtyRef.current.has(cached.entry)) continue;
      cache.delete(candidate);
      bytes -= cached.state.doc.length;
    }
  }, []);

  // Only the match counter reads docVersion, so with the find bar closed a keystroke
  // needs no React state change at all, and with it open one render per window rather
  // than one per character. Typing used to re-render this whole component every key.
  const docVersionTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const bumpDocVersion = useCallback(() => {
    const { open, term } = searchStateRef.current;
    if (!open || !term || docVersionTimer.current) return;
    docVersionTimer.current = setTimeout(() => {
      docVersionTimer.current = null;
      setDocVersion((n) => n + 1);
    }, DOC_VERSION_DEBOUNCE_MS);
  }, []);

  useEffect(() => {
    return () => {
      if (docVersionTimer.current) clearTimeout(docVersionTimer.current);
    };
  }, []);

  // ── Match count ──
  // Counted off the live CodeMirror document rather than a React copy of it, and
  // debounced. Holding every match position costs tens of megabytes on a large INI
  // with a common needle, and navigation finds its own matches from the cursor.
  const [matchInfo, setMatchInfo] = useState<MatchInfo>(EMPTY_MATCH_INFO);

  useEffect(() => {
    if (!searchOpen || !searchTerm) {
      setMatchInfo(EMPTY_MATCH_INFO);
      return;
    }
    const timer = setTimeout(() => {
      const view = editorViewRef.current;
      if (!view) return;
      const { count, index } = countMatches(
        view.state.doc,
        searchTerm,
        caseSensitive,
        view.state.selection.main.from
      );
      setMatchInfo({ count, index, term: searchTerm, caseSensitive, version: docVersion });
    }, COUNT_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [searchTerm, caseSensitive, searchOpen, docVersion, activeEntry, pakEpoch]);

  // Derived rather than stored, so the debounce does not need a second state update.
  const countPending =
    searchOpen &&
    !!searchTerm &&
    (matchInfo.term !== searchTerm ||
      matchInfo.caseSensitive !== caseSensitive ||
      matchInfo.version !== docVersion);

  // ── Pak scanning ──
  const scan = useCallback(
    async (silent = false) => {
      if (!gamePath) return;
      setScanning(true);
      try {
        const scanned = await invoke<{ paks: PakIniListing[]; unreadable: PakScanError[] }>(
          "scan_mod_paks_any_ini",
          { gameRoot: gamePath }
        );
        const results = scanned.paks;
        // Re-inspect manually-browsed paks not in the folder scan; drop those that no longer have INI entries.
        const manualOnly = paks.filter((p) => !results.find((r) => r.pak_path === p.pak_path));
        const inspectedManual = await Promise.all(
          manualOnly.map(async (pak) => {
            try {
              return await invoke<PakIniListing | null>("inspect_pak_path_any_ini", {
                pakPath: pak.pak_path,
              });
            } catch {
              return null;
            }
          })
        );
        const retainedManual = inspectedManual.filter((p): p is PakIniListing => p !== null);
        const merged = [...results, ...retainedManual];
        setPaks(merged);
        if (selectedPak && !merged.find((p) => p.pak_path === selectedPak.pak_path)) {
          setSelectedPak(null);
          setActiveEntry(null);
          setContents({});
          setDirtySet(new Set());
          onDiskRef.current = new Set();
          editorStatesRef.current.clear();
          setPendingDeletes(new Set());
        }
        if (merged.length === 0) {
          if (!silent) showNotice("No paks with INI files found", "info");
        } else if (!silent) {
          showNotice(`Found ${merged.length} pak${merged.length !== 1 ? "s" : ""} with INI`, "ok");
        }
        if (scanned.unreadable.length > 0) {
          console.error("Paks that could not be read:", scanned.unreadable);
          if (!silent) showNotice(unreadableScanMessage(scanned.unreadable), "err", 8000);
        }
      } catch (e) {
        console.error("Scan failed:", e);
        if (!silent) showNotice("Scan failed", "err");
      } finally {
        setScanning(false);
      }
    },
    [gamePath, paks, selectedPak]
  );

  async function createNewPak() {
    if (!gamePath || !newPakName.trim()) return;
    setCreatingPak(true);
    try {
      const info = await invoke<PakIniListing>("create_new_mod_pak", {
        gameRoot: gamePath,
        name: newPakName.trim(),
      });
      setPaks((prev) => (prev.find((p) => p.pak_path === info.pak_path) ? prev : [...prev, info]));
      requestLoadPak(info);
      setNewPakOpen(false);
      setNewPakName("");
      // Refresh other tabs' mod lists. The editor skips its own source so this
      // empty (INI-less) pak isn't pruned from its own list.
      emitModsChanged({
        modsFolder: `${gamePath}\\MarvelGame\\Marvel\\Content\\Paks\\~mods`,
        source: "PakIniEditor",
      });
      showNotice(`Created ${info.pak_name}`, "ok");
    } catch (e) {
      showNotice(String(e), "err", 6000);
      console.error(e);
    } finally {
      setCreatingPak(false);
    }
  }

  async function browse() {
    const selected = await open({
      multiple: false,
      filters: [{ name: "Pak files", extensions: ["pak"] }],
    });
    if (typeof selected !== "string") return;
    try {
      const info = await invoke<PakIniListing | null>("inspect_pak_path_any_ini", {
        pakPath: selected,
      });
      if (!info) {
        showNotice("No INI files found in that pak", "err");
        return;
      }
      setPaks((prev) => (prev.find((p) => p.pak_path === info.pak_path) ? prev : [...prev, info]));
      requestLoadPak(info);
    } catch (e) {
      showNotice("Failed to read pak", "err");
      console.error(e);
    }
  }

  // Drag-and-drop: accept .pak files to add to the list (same as browse).
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    getCurrentWindow()
      .onDragDropEvent(async (event) => {
        if (event.payload.type === "enter") {
          if (isActiveRef.current) setIsDragging(true);
        } else if (event.payload.type === "drop") {
          setIsDragging(false);
          if (!isActiveRef.current) return;
          const pakPaths = event.payload.paths.filter((p) => p.toLowerCase().endsWith(".pak"));
          if (pakPaths.length === 0) return;
          try {
            const info = await invoke<PakIniListing | null>("inspect_pak_path_any_ini", {
              pakPath: pakPaths[0],
            });
            if (!info) {
              showNotice("No INI files found in that pak", "err");
              return;
            }
            setPaks((prev) =>
              prev.find((p) => p.pak_path === info.pak_path) ? prev : [...prev, info]
            );
            requestLoadPakRef.current(info);
          } catch (e) {
            showNotice("Failed to read pak", "err");
            console.error(e);
          }
        } else if (event.payload.type === "leave") {
          setIsDragging(false);
        }
      })
      .then((fn) => {
        unlisten = fn;
      });
    return () => unlisten?.();
  }, []);

  // External pak mutation: reload if affecting current pak and clean.
  useEffect(() => {
    if (!selectedPak) return;
    return onPakChanged((e) => {
      if (e.source === "PakIniEditor") return;
      if (e.pakPath !== selectedPak.pak_path) return;
      if (isDirty) {
        showNotice("Pak changed elsewhere; reload manually to discard edits", "info", 6000);
        return;
      }
      loadPak(selectedPak);
    });
  }, [selectedPak, isDirty]); // eslint-disable-line react-hooks/exhaustive-deps

  // Selecting a pak reads its index, not its contents. Extracting every INI up front cost
  // a full copy of each one before you had opened any of them, and on a pak with a large
  // INI that is seconds of stall and tens of megabytes for a file you may never look at.
  async function loadPak(pak: PakIniListing) {
    const isPakSwitch = selectedPak?.pak_path !== pak.pak_path;
    setSelectedPak(pak);
    setContents({});
    setDirtySet(new Set());
    onDiskRef.current = new Set();
    editorStatesRef.current.clear();
    setPendingDeletes(new Set());
    setLoading(true);

    try {
      const loaded: Record<string, string> = {};
      for (const entry of pak.ini_entries) {
        try {
          const raw = await invoke<string>("extract_pak_ini", {
            pakPath: pak.pak_path,
            entry,
          });
          loaded[entry] = normalizeLineEndings(raw);
        } catch (e) {
          console.error(`Failed to read ${entry}:`, e);
        }
      }
      setContents(loaded);
      onDiskRef.current = new Set(Object.keys(loaded));
      setPakEpoch((n) => n + 1);

      // Preserve the active entry across reloads when it still exists; otherwise pick the first.
      const firstEntry = pak.ini_entries.find((e) => loaded[e] !== undefined) ?? null;
      if (isPakSwitch) {
        setActiveEntry(firstEntry);
      } else if (activeEntry === null || loaded[activeEntry] === undefined) {
        setActiveEntry(firstEntry);
      }
    } catch (e) {
      showNotice(String(e), "err");
      console.error(e);
    } finally {
      setLoading(false);
    }
  }

  // Opening another pak drops every unsaved edit, the same as Discard, so it asks first.
  // An external change to the current pak is already refused while dirty; this closes the
  // matching hole for switches the user starts.
  function requestLoadPak(pak: PakIniListing) {
    // Any load wipes the buffers, so the prompt is about being dirty, not about which pak
    // was picked: browsing to the pak already open would discard just as much.
    if (isDirty) {
      setPakSwitchPrompt({
        pak,
        fromName: selectedPak?.pak_name ?? "",
        changeCount: pendingChanges.length,
      });
      setPakSwitchOpen(true);
      return;
    }
    void loadPak(pak);
  }

  const requestLoadPakRef = useRef(requestLoadPak);
  useEffect(() => {
    requestLoadPakRef.current = requestLoadPak;
  });

  async function reload() {
    if (!selectedPak) return;
    await loadPak(selectedPak);
    showNotice("Reloaded from disk", "ok");
  }

  // Throwing the edits away means going back to what the pak holds, so re-read it rather
  // than keeping a second copy of every file around purely to answer this.
  async function discard() {
    if (!isDirty || !selectedPak) return;
    await loadPak(selectedPak);
    showNotice("Discarded unsaved changes", "ok");
  }

  // Queue an entry for deletion on next save. Brand-new entries (not in
  // savedContents) drop entirely from contents so the popover treats the name
  // as available again.
  function queueDelete(entry: string) {
    const wasOnDisk = onDiskRef.current.has(entry);
    if (wasOnDisk) {
      setPendingDeletes((prev) => {
        const next = new Set(prev);
        next.add(entry);
        return next;
      });
    } else {
      setContents((prev) => {
        const next = { ...prev };
        delete next[entry];
        return next;
      });
      setDirtySet((prev) => {
        if (!prev.has(entry)) return prev;
        const next = new Set(prev);
        next.delete(entry);
        return next;
      });
    }
    if (activeEntry === entry) {
      const remaining = displayedEntries.find((e) => e !== entry);
      setActiveEntry(remaining ?? null);
    }
  }

  // Add a new INI entry to the working buffer. `entry` is the full in-pak path;
  // a previously-queued delete for the same path is un-queued instead so the
  // round-trip is a no-op. Seeds new content from pakchunk0's matching default
  // when available so the user starts with the real UE defaults rather than a
  // blank section header.
  async function addEntry(rawEntry: string) {
    const entry = ensureMountPrefix(rawEntry);
    if (pendingDeletes.has(entry)) {
      setPendingDeletes((prev) => {
        const next = new Set(prev);
        next.delete(entry);
        return next;
      });
      setActiveEntry(entry);
      return;
    }
    if (contents[entry] !== undefined) {
      showNotice(`${entryBasename(entry)} already exists in this pak`, "err");
      return;
    }

    let seeded = "";
    if (gamePath) {
      try {
        const fromGame = await invoke<string | null>("extract_game_default_ini", {
          gameRoot: gamePath,
          inPakPath: entry,
        });
        if (fromGame !== null) {
          seeded = normalizeLineEndings(fromGame);
        }
      } catch (e) {
        console.warn("Failed to seed from game default:", e);
        // Fall through with an empty buffer; the user can fill it in.
      }
    }
    setContents((prev) => ({ ...prev, [entry]: seeded }));
    markDirty(entry);
    setActiveEntry(entry);
  }

  // A dirty entry lives in the editor, not in React state, so read it back from there.
  function docFor(entry: string): Text | null {
    if (entry === activeEntry && editorViewRef.current) return editorViewRef.current.state.doc;
    const cached = selectedPak
      ? editorStatesRef.current.get(`${pakEpoch}::${selectedPak.pak_path}::${entry}`)
      : undefined;
    if (cached) return cached.state.doc;
    const seed = contents[entry];
    return seed === undefined ? null : Text.of(seed.split("\n"));
  }

  async function save() {
    if (!selectedPak || !isDirty) return;
    setSaving(true);
    try {
      // sliceString emits the CRLF form directly, instead of a toString plus a regex
      // pass over the whole file.
      const files: PakIniFileContent[] = [];
      for (const entry of dirtyEntries) {
        const doc = docFor(entry);
        if (doc) files.push({ entry, content: doc.sliceString(0, doc.length, "\r\n") });
      }
      const deletes = [...pendingDeletes].filter((entry) => onDiskRef.current.has(entry));

      const msg = await invoke<string>("save_pak_ini", {
        pakPath: selectedPak.pak_path,
        files,
        deletes,
      });
      showNotice(msg, "ok");
      emitPakChanged({ pakPath: selectedPak.pak_path, source: "PakIniEditor" });

      // Adding or deleting an entry changes what the pak holds, and the listing captured
      // at selection time no longer describes it. Reload and discard both re-read from
      // that listing, so a stale one loses an entry that was just saved.
      if (files.some((f) => !onDiskRef.current.has(f.entry)) || deletes.length > 0) {
        try {
          const fresh = await invoke<PakIniListing | null>("inspect_pak_path_any_ini", {
            pakPath: selectedPak.pak_path,
          });
          if (fresh) {
            setSelectedPak(fresh);
            onDiskRef.current = new Set(fresh.ini_entries);
            setPaks((prev) => prev.map((p) => (p.pak_path === fresh.pak_path ? fresh : p)));
          }
        } catch (e) {
          console.error("Failed to refresh pak listing after save:", e);
        }
      }

      // Everything that was dirty is now on disk. The editor still holds the text, so
      // there is no disk round-trip here and the cursor and undo history survive.
      for (const entry of dirtyEntries) onDiskRef.current.add(entry);
      for (const entry of pendingDeletes) onDiskRef.current.delete(entry);
      setDirtySet(new Set());
      // Drop deleted (and discarded-add) entries from contents.
      if (pendingDeletes.size > 0) {
        setContents((prev) => {
          const next: Record<string, string> = {};
          for (const [entry, value] of Object.entries(prev)) {
            if (pendingDeletes.has(entry)) continue;
            next[entry] = value;
          }
          return next;
        });
      }
      setPendingDeletes(new Set());
      // If the active entry was deleted, switch to first remaining.
      if (activeEntry !== null && pendingDeletes.has(activeEntry)) {
        const remaining = Object.keys(contents).find(
          (e) => e !== activeEntry && !pendingDeletes.has(e)
        );
        setActiveEntry(remaining ?? null);
      }
    } catch (e) {
      showNotice(String(e), "err", 8000);
      console.error(e);
    } finally {
      setSaving(false);
    }
  }

  // ── Search functions ──
  const saveRef = useRef(save);
  useEffect(() => {
    saveRef.current = save;
  });

  function openSearch() {
    setSearchOpen(true);
    setTimeout(() => searchInputRef.current?.focus(), 0);
  }

  const openSearchRef = useRef(openSearch);
  useEffect(() => {
    openSearchRef.current = openSearch;
  });

  function scrollToPos(view: EditorView, pos: number) {
    requestAnimationFrame(() => {
      const coords = view.coordsAtPos(pos);
      if (!coords) return;
      const scroller = view.scrollDOM;
      const rect = scroller.getBoundingClientRect();
      scroller.scrollTop += coords.top - rect.top - rect.height / 2;
    });
  }

  function jumpToMatch(pos: number, index: number) {
    const view = editorViewRef.current;
    if (!view) return;
    view.dispatch({ selection: { anchor: pos, head: pos + searchTerm.length } });
    scrollToPos(view, pos);
    setMatchInfo((prev) => ({ ...prev, index }));
  }

  // Whether the selection is exactly the search term, which is what makes it safe to step
  // the ordinal instead of recounting it.
  function selectionIsMatch(view: EditorView): boolean {
    const sel = view.state.selection.main;
    if (searchTerm.length === 0 || sel.to - sel.from !== searchTerm.length) return false;
    const selected = view.state.doc.sliceString(sel.from, sel.to);
    return caseSensitive
      ? selected === searchTerm
      : selected.toLowerCase() === searchTerm.toLowerCase();
  }

  // Stepping from a known ordinal is free. When it is unknown, because the editor was
  // rebuilt by a reload or the cursor was moved by hand, recover it exactly rather than
  // assuming: incrementing regardless is what drifted the counter ahead of the highlight.
  function indexAfterJump(doc: Text, target: number, step: 1 | -1, stepable: boolean): number {
    if (stepable && matchInfo.index >= 0 && matchInfo.count > 0) {
      return (matchInfo.index + step + matchInfo.count) % matchInfo.count;
    }
    return ordinalOf(doc, searchTerm, caseSensitive, target);
  }

  function findNext() {
    const view = editorViewRef.current;
    if (!view || !searchTerm) return;
    const doc = view.state.doc;
    const onMatch = selectionIsMatch(view);
    // Only advance past the cursor when it is already sitting on a match, or the first
    // match of a freshly opened file gets skipped.
    const from = view.state.selection.main.from + (onMatch ? 1 : 0);

    const after = matchAtOrAfter(doc, searchTerm, caseSensitive, from);
    if (after !== null) {
      jumpToMatch(after, indexAfterJump(doc, after, 1, onMatch));
      return;
    }
    const first = matchAtOrAfter(doc, searchTerm, caseSensitive, 0);
    if (first !== null) jumpToMatch(first, 0);
  }

  function findPrev() {
    const view = editorViewRef.current;
    if (!view || !searchTerm) return;
    const doc = view.state.doc;
    const onMatch = selectionIsMatch(view);

    const before = matchBefore(doc, searchTerm, caseSensitive, view.state.selection.main.from);
    if (before !== null) {
      jumpToMatch(before, indexAfterJump(doc, before, -1, onMatch));
      return;
    }
    const last = matchBefore(doc, searchTerm, caseSensitive, doc.length);
    if (last !== null) jumpToMatch(last, matchInfo.count > 0 ? matchInfo.count - 1 : -1);
  }

  function replaceOne() {
    const view = editorViewRef.current;
    if (!view || !searchTerm) return;
    // Nothing selected yet, so the first press just takes you to a match to replace.
    if (!selectionIsMatch(view)) {
      findNext();
      return;
    }

    const from = view.state.selection.main.from;
    const resumeAt = from + replaceTerm.length;
    view.dispatch({
      changes: { from, to: from + searchTerm.length, insert: replaceTerm },
      selection: { anchor: resumeAt },
    });

    // Land on the next occurrence so repeated presses walk the file, instead of leaving
    // the cursor in the replacement with no ordinal to show.
    const doc = view.state.doc;
    const next =
      matchAtOrAfter(doc, searchTerm, caseSensitive, resumeAt) ??
      matchAtOrAfter(doc, searchTerm, caseSensitive, 0);
    if (next === null) {
      setMatchInfo((prev) => ({ ...prev, count: 0, index: -1 }));
      return;
    }

    // Dropping the match we were on shifts every later ordinal down by one, so the next
    // one inherits the position we were at. A replacement that contains the search term
    // creates matches instead of only removing one, so count it properly in that case.
    const needle = caseSensitive ? searchTerm : searchTerm.toLowerCase();
    const inserted = caseSensitive ? replaceTerm : replaceTerm.toLowerCase();
    const selfMatching = inserted.includes(needle);
    const count = selfMatching ? matchInfo.count : Math.max(matchInfo.count - 1, 0);
    const index = selfMatching
      ? ordinalOf(doc, searchTerm, caseSensitive, next)
      : matchInfo.index >= 0 && matchInfo.index < count
        ? matchInfo.index
        : 0;

    view.dispatch({ selection: { anchor: next, head: next + searchTerm.length } });
    scrollToPos(view, next);
    setMatchInfo((prev) => ({ ...prev, count, index }));
  }

  // One change spec per match blows up the ChangeSet and the undo history: hundreds of
  // thousands of small objects allocated in one burst is what took the renderer down.
  // Rewriting whole lines keeps the spec count to the lines that actually changed, and
  // past that a single rope swap keeps it to one.
  function replaceAllMatches() {
    const view = editorViewRef.current;
    if (!view || !searchTerm) return;
    const doc = view.state.doc;

    const { changes, lines, replaced } = replaceAllLines(
      doc,
      searchTerm,
      replaceTerm,
      caseSensitive
    );
    if (replaced === 0) return;

    const anchor = Math.min(view.state.selection.main.from, doc.length);
    view.dispatch({
      changes:
        changes.length <= REPLACE_ALL_LINE_LIMIT
          ? changes
          : { from: 0, to: doc.length, insert: Text.of(lines) },
      selection: { anchor },
      scrollIntoView: false,
      userEvent: "input.replace.all",
    });
    showNotice(`Replaced ${replaced} occurrence${replaced !== 1 ? "s" : ""}`, "ok");
  }

  // ── CodeMirror setup ──

  useEffect(() => {
    if (!editorContainerRef.current || currentContent === null) return;

    // Capture identity of the file shown at mount so cleanup saves scroll under
    // the right key even if activeEntry/selectedPak have already changed by then.
    const scrollKey = selectedPak && activeEntry ? `${selectedPak.pak_path}::${activeEntry}` : null;
    // Cached states are keyed by load epoch as well. This effect's cleanup runs after the
    // next load has already cleared the cache, so an epoch-free key would let the outgoing
    // (pre-reload) document be written back and then restored over the fresh one.
    const stateKey = scrollKey === null ? null : `${pakEpoch}::${scrollKey}`;

    // Destroy previous editor if switching files
    if (editorViewRef.current) {
      editorViewRef.current.destroy();
      editorViewRef.current = null;
    }

    const cmTheme = EditorView.theme({
      "&": {
        height: "100%",
        fontSize: "13px",
        backgroundColor: "var(--color-background)",
      },
      ".cm-content": {
        fontFamily: "ui-monospace, SFMono-Regular, 'SF Mono', Menlo, Consolas, monospace",
        caretColor: "var(--color-foreground)",
        color: "var(--color-foreground)",
        // Pinned to an integer pixel value so every row renders at the same height;
        // a unitless multiplier (e.g. 1.625) yields 21.125px which the browser
        // rounds inconsistently between rows.
        lineHeight: "21px",
        padding: "16px 0",
      },
      ".cm-line": {
        padding: "0 16px",
      },
      "&.cm-focused .cm-cursor": {
        borderLeftColor: "var(--color-foreground)",
      },
      "&.cm-focused .cm-selectionBackground, .cm-selectionBackground": {
        backgroundColor: "hsl(215 60% 40% / 0.4)",
      },
      ".cm-gutters": {
        display: "none",
      },
      ".cm-scroller": {
        overflow: "auto",
      },
      "&.cm-focused": {
        outline: "none",
      },
      ".cm-search-match": {
        backgroundColor: "hsl(210 80% 60% / 0.35)",
      },
      ".cm-search-match-current": {
        backgroundColor: "hsl(210 80% 60% / 0.7)",
      },
    });

    const cmKeymap = keymap.of([
      {
        key: "Mod-s",
        run: () => {
          saveRef.current();
          return true;
        },
      },
      {
        key: "Mod-f",
        run: () => {
          openSearchRef.current();
          return true;
        },
      },
    ]);

    const updateListener = EditorView.updateListener.of((update) => {
      if (!update.docChanged) return;
      bumpDocVersion();
      if (activeEntry !== null) markDirty(activeEntry);
    });

    // Reuse the cached state and its undo history. A hit can only come from this load
    // epoch, so it is current by construction and needs no doc comparison to prove it.
    for (const key of [...editorStatesRef.current.keys()]) {
      if (!key.startsWith(`${pakEpoch}::`)) editorStatesRef.current.delete(key);
    }
    const hit = stateKey === null ? undefined : editorStatesRef.current.get(stateKey);
    // Re-inserting marks the entry most recently used for the eviction policy.
    if (hit && stateKey !== null) {
      editorStatesRef.current.delete(stateKey);
      editorStatesRef.current.set(stateKey, hit);
    }
    const cached = hit?.state;
    const state =
      cached ??
      EditorState.create({
        doc: currentContent,
        extensions: [
          cmTheme,
          cmKeymap,
          keymap.of([...defaultKeymap, ...historyKeymap]),
          history(),
          searchExtension,
          updateListener,
          EditorView.lineWrapping,
        ],
      });

    const view = new EditorView({
      state,
      parent: editorContainerRef.current,
    });

    editorViewRef.current = view;

    // Re-sync search highlights if search is open when editor is recreated
    const s = searchStateRef.current;
    if (s.open && s.term) {
      view.dispatch({
        effects: setSearchHighlight.of({ search: s.term, caseSensitive: s.caseSensitive }),
      });
    }

    // Restore scroll position from prior visit of this entry. scrollIntoView
    // tells CM to render around that document position so the viewport isn't
    // left blank, and works even with search open (which is what we want when
    // iterating matches and tab-hopping).
    if (scrollKey !== null) {
      const savedPos = entryScrollRef.current[scrollKey];
      if (savedPos !== undefined && savedPos > 0) {
        view.dispatch({
          // The cursor moves with the viewport so find-next resumes from what is on
          // screen rather than jumping back to the top of the file.
          selection: cached ? undefined : { anchor: savedPos },
          effects: EditorView.scrollIntoView(savedPos, { y: "start" }),
        });
      }
    }

    return () => {
      if (scrollKey !== null && stateKey !== null) {
        const block = view.lineBlockAtHeight(view.scrollDOM.scrollTop);
        entryScrollRef.current[scrollKey] = block.from;
        cacheEditorState(stateKey, view.state, activeEntry ?? "");
      }
      view.destroy();
      editorViewRef.current = null;
    };
    // Only recreate when switching entries or loading new content from disk (pakEpoch bump)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeEntry, pakEpoch]);

  // ── Sync search config + auto-jump (single atomic dispatch) ──
  useEffect(() => {
    const view = editorViewRef.current;
    if (!view) return;

    const search = searchOpen ? searchTerm : "";
    const effects = setSearchHighlight.of({ search, caseSensitive });

    // Finding the first hit stops at that hit, so this stays immediate while the full
    // count catches up behind its debounce.
    const first = search ? matchAtOrAfter(view.state.doc, search, caseSensitive, 0) : null;
    if (first !== null) {
      setMatchInfo((prev) => ({ ...prev, index: 0 }));
      view.dispatch({
        effects,
        selection: { anchor: first, head: first + search.length },
      });
      scrollToPos(view, first);
    } else {
      setMatchInfo((prev) => ({ ...prev, index: -1 }));
      view.dispatch({ effects });
    }
    // Only re-run when search params change, not when content changes.
  }, [searchOpen, searchTerm, caseSensitive]);

  // ── Ctrl+F / Esc when this tab is active ──
  useEffect(() => {
    if (!isActive) return;
    function onKeyDown(e: KeyboardEvent) {
      if (e.key === "f" && (e.ctrlKey || e.metaKey)) {
        e.preventDefault();
        openSearch();
      } else if (e.key === "Escape" && searchOpen) {
        // Close from anywhere (e.g. the editor), not just the search inputs.
        setSearchOpen(false);
      }
    }
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [isActive, searchOpen]);

  // Auto-scan on first activation with a game path
  const hasScanned = useRef(false);
  useEffect(() => {
    if (isActive && gamePath && !hasScanned.current) {
      hasScanned.current = true;
      scan(true);
    }
  }, [isActive, gamePath, scan]);

  // Re-scan when ~mods composition changes elsewhere; prunes deleted paks and adds new INI-bearing paks.
  const scanRef = useRef(scan);
  useEffect(() => {
    scanRef.current = scan;
  });
  useEffect(() => {
    return onModsChanged((event) => {
      if (!gamePath) return;
      // Skip our own emission; a re-scan would prune the new INI-less pak.
      if (event.source === "PakIniEditor") return;
      const modsFolder = `${gamePath}\\MarvelGame\\Marvel\\Content\\Paks\\~mods`;
      if (normalizeFolderPath(event.modsFolder) !== normalizeFolderPath(modsFolder)) return;
      scanRef.current(true);
    });
  }, [gamePath]);

  // Auto-select when exactly one pak is found
  useEffect(() => {
    if (paks.length === 1 && !selectedPak && !loading) {
      loadPak(paks[0]);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps -- fire-once after scan populates paks
  }, [paks, selectedPak, loading]);

  // Reset scan flag when game path changes
  useEffect(() => {
    hasScanned.current = false;
    setPaks([]);
    setSelectedPak(null);
    setActiveEntry(null);
    setContents({});
    setDirtySet(new Set());
    onDiskRef.current = new Set();
    editorStatesRef.current.clear();
    setPendingDeletes(new Set());
  }, [gamePath]);

  return (
    <div className="relative flex flex-1 min-h-0 w-full flex-col gap-4">
      {isDragging && (
        <div className="pointer-events-none absolute inset-0 z-50 flex flex-col items-center justify-center gap-3 rounded-lg border-2 border-dashed border-ok bg-background/80 backdrop-blur-sm">
          <UploadCloud size={36} className="text-ok" />
          <span className="text-sm font-semibold text-ok">Drop .pak to inspect</span>
        </div>
      )}
      {/* ── Header ── */}
      <div className="flex min-h-8 items-center gap-3">
        <h2 className="text-xl font-bold">Pak INI Editor</h2>
        {notice && (
          <span
            className={cn(
              "flex min-w-0 items-center gap-1 text-[12px] font-medium",
              notice.type === "ok" && "text-ok",
              notice.type === "err" && "text-err",
              notice.type === "info" && "text-muted-foreground"
            )}
          >
            {notice.type === "ok" && (
              <CheckCircle2 size={13} strokeWidth={2.5} className="shrink-0" />
            )}
            {notice.type === "err" && <XCircle size={13} strokeWidth={2.5} className="shrink-0" />}
            <span className="truncate">{notice.msg}</span>
          </span>
        )}
      </div>

      {/* ── Pak selection ── */}
      <div className="flex items-center gap-1">
        <Select
          value={selectedPak?.pak_path ?? ""}
          onValueChange={(value) => {
            const pak = paks.find((p) => p.pak_path === value);
            if (pak) requestLoadPak(pak);
          }}
          disabled={paks.length === 0}
        >
          <SelectTrigger size="sm" className="min-w-0 flex-1 text-left font-mono text-xs">
            <SelectValue
              className="block min-w-0 max-w-full truncate text-left"
              placeholder={
                paks.length === 0 ? "No paks with INI — scan or browse" : "Select a pak..."
              }
            />
          </SelectTrigger>
          <SelectContent position="popper" className="w-(--radix-select-trigger-width)">
            {paks.map((p) => (
              <SelectItem key={p.pak_path} value={p.pak_path} className="font-mono text-xs">
                {p.pak_name}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Tip content="Browse for pak file">
          <Button variant="ghost" size="icon-sm" onClick={browse}>
            <FolderOpen size={14} />
          </Button>
        </Tip>
        <Popover
          open={newPakOpen}
          onOpenChange={(open) => {
            if (creatingPak) return;
            setNewPakOpen(open);
            if (!open) setNewPakName("");
          }}
        >
          <Tip content="Create a new empty pak">
            <PopoverTrigger asChild>
              <Button variant="ghost" size="icon-sm" disabled={!gamePath}>
                <FilePlus2 size={14} />
              </Button>
            </PopoverTrigger>
          </Tip>
          <PopoverContent align="end" className="w-72 p-3">
            <NewPakPopover
              name={newPakName}
              setName={setNewPakName}
              creating={creatingPak}
              onCreate={createNewPak}
            />
          </PopoverContent>
        </Popover>
        <Tip content="Scan for paks with INI files">
          <Button
            variant="ghost"
            size="icon-sm"
            onClick={() => scan()}
            disabled={scanning || !gamePath}
          >
            {scanning ? (
              <RefreshCw size={14} className="animate-spin" />
            ) : (
              <ListRestart size={14} />
            )}
          </Button>
        </Tip>
      </div>

      {/* ── Editor area ── */}
      {selectedPak && !loading && (
        <div className="flex flex-1 min-h-0 flex-col rounded-md border border-border overflow-hidden">
          {/* File tabs + toolbar */}
          <div className="flex items-center justify-between border-b border-border px-3 py-1.5">
            <div className="flex min-w-0 flex-1 items-center gap-2 overflow-x-auto">
              <div className="flex items-center gap-1 rounded-md bg-muted p-1">
                {displayedEntries.map((entry) => {
                  const entryDirty = dirtySet.has(entry);
                  const isActive = activeEntry === entry;
                  return (
                    <div
                      key={entry}
                      className={cn(
                        "group flex items-center rounded-sm transition-colors",
                        isActive ? "bg-background shadow-sm" : "hover:bg-background/40"
                      )}
                    >
                      <button
                        onClick={() => setActiveEntry(entry)}
                        title={entry}
                        className={cn(
                          "flex items-center gap-1.5 pl-3 py-1 text-[12px] font-medium transition-colors",
                          displayedEntries.length === 1 ? "pr-3" : "pr-2",
                          isActive
                            ? "text-foreground"
                            : "text-muted-foreground hover:text-foreground"
                        )}
                      >
                        <FileText size={12} />
                        {entryBasename(entry)}
                        {entryDirty && <span className="size-1.5 rounded-full bg-warn" />}
                      </button>
                      {displayedEntries.length > 1 && (
                        <button
                          onClick={() => setDeleteConfirm(entry)}
                          title={`Delete ${entryBasename(entry)}`}
                          className={cn(
                            "mr-1 rounded-sm p-0.5 transition-opacity hover:bg-destructive/15 hover:text-destructive",
                            isActive ? "opacity-60" : "opacity-0 group-hover:opacity-60"
                          )}
                        >
                          <X size={11} />
                        </button>
                      )}
                    </div>
                  );
                })}
                <Popover
                  open={addOpen}
                  onOpenChange={(open) => {
                    if (!adding) setAddOpen(open);
                  }}
                >
                  <PopoverTrigger asChild>
                    <button
                      title="Add INI file"
                      className="ml-0.5 rounded-sm p-1 text-muted-foreground transition-colors hover:bg-background/40 hover:text-foreground"
                    >
                      <Plus size={13} />
                    </button>
                  </PopoverTrigger>
                  <PopoverContent align="start" className="w-80 p-3">
                    <AddIniPopover
                      existingEntries={displayedEntries}
                      adding={adding}
                      onAdd={async (entry) => {
                        setAdding(true);
                        try {
                          await addEntry(entry);
                          setAddOpen(false);
                          setAddCustomPath("");
                        } finally {
                          setAdding(false);
                        }
                      }}
                      customPath={addCustomPath}
                      setCustomPath={setAddCustomPath}
                    />
                  </PopoverContent>
                </Popover>
              </div>
            </div>

            <div className="flex shrink-0 items-center gap-1">
              <Tip content="Reload from disk">
                <Button variant="ghost" size="sm" onClick={reload} disabled={loading || saving}>
                  <RefreshCw size={13} />
                </Button>
              </Tip>
              <Tip content="Search & Replace (Ctrl+F)">
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={() => (searchOpen ? setSearchOpen(false) : openSearch())}
                  className={cn(searchOpen && "bg-secondary")}
                >
                  <Search size={13} />
                </Button>
              </Tip>
            </div>
          </div>

          {/* Search/replace bar */}
          {searchOpen && (
            <div className="flex items-center gap-2 border-b border-border bg-muted/50 px-3 py-1.5">
              <div className="flex flex-1 items-center gap-2">
                <Input
                  ref={searchInputRef}
                  value={searchTerm}
                  onChange={(e) => setSearchTerm(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" && e.shiftKey) {
                      e.preventDefault();
                      findPrev();
                    } else if (e.key === "Enter") {
                      e.preventDefault();
                      findNext();
                    }
                    if (e.key === "Escape") setSearchOpen(false);
                  }}
                  placeholder="Search..."
                  className="h-7 flex-1 text-xs"
                />
                <Input
                  value={replaceTerm}
                  onChange={(e) => setReplaceTerm(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      e.preventDefault();
                      replaceOne();
                    }
                    if (e.key === "Escape") setSearchOpen(false);
                  }}
                  placeholder="Replace..."
                  className="h-7 flex-1 text-xs"
                />
              </div>
              <span className="shrink-0 text-[11px] text-muted-foreground">
                {!searchTerm
                  ? ""
                  : matchInfo.count > 0
                    ? matchInfo.index >= 0
                      ? `${matchInfo.index + 1}/${matchInfo.count}`
                      : `${matchInfo.count} matches`
                    : countPending
                      ? "..."
                      : "No results"}
              </span>
              <div className="flex items-center gap-0.5">
                <Tip content="Match Case">
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={() => setCaseSensitive((p) => !p)}
                    className={cn(caseSensitive && "bg-secondary text-foreground")}
                  >
                    <CaseSensitive size={14} />
                  </Button>
                </Tip>
                <Tip content="Previous Match">
                  <Button variant="ghost" size="sm" onClick={findPrev} disabled={!searchTerm}>
                    <ChevronUp size={13} />
                  </Button>
                </Tip>
                <Tip content="Next Match">
                  <Button variant="ghost" size="sm" onClick={findNext} disabled={!searchTerm}>
                    <ChevronDown size={13} />
                  </Button>
                </Tip>
                <Tip content="Replace">
                  <Button variant="ghost" size="sm" onClick={replaceOne} disabled={!searchTerm}>
                    <Replace size={13} />
                  </Button>
                </Tip>
                <Tip content="Replace All">
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={replaceAllMatches}
                    disabled={!searchTerm}
                  >
                    <ReplaceAll size={13} />
                  </Button>
                </Tip>
                <Tip content="Close">
                  <Button variant="ghost" size="sm" onClick={() => setSearchOpen(false)}>
                    <X size={13} />
                  </Button>
                </Tip>
              </div>
            </div>
          )}

          {/* CodeMirror editor (mounted only when an entry is active) */}
          {currentContent !== null && activeEntry !== null ? (
            <div ref={editorContainerRef} className="flex-1 min-h-0 w-full overflow-hidden" />
          ) : (
            <div className="flex flex-1 min-h-0 flex-col items-center justify-center gap-2 px-4 text-center">
              <FileText size={22} className="text-muted-foreground/50" />
              <span className="text-[12px] text-muted-foreground">
                No INI file open. Use the + tab to add one.
              </span>
            </div>
          )}

          {/* Save bar — only visible when there are pending edits */}
          {isDirty && (
            <div className="flex items-center justify-end gap-2 border-t border-border px-3 py-1.5">
              {gameRunning ? (
                <span className="mr-auto flex items-center gap-1.5 text-[11px] font-medium text-warn">
                  <AlertTriangle size={13} className="shrink-0" />
                  Close the game to save changes
                </span>
              ) : (
                <span className="mr-auto flex items-center gap-1.5 text-[11px] font-medium text-warn">
                  <span className="relative flex h-2 w-2">
                    <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-warn opacity-60" />
                    <span className="relative inline-flex h-2 w-2 rounded-full bg-warn" />
                  </span>
                  Unsaved
                  {pendingChanges.length === 1
                    ? ` (${entryBasename(pendingChanges[0])})`
                    : ` (${pendingChanges.length} files)`}
                </span>
              )}
              <Button variant="ghost" size="sm" onClick={discard} disabled={saving}>
                <Undo2 size={13} />
                Discard
              </Button>
              <Button
                variant="blue"
                size="sm"
                onClick={save}
                disabled={!isDirty || saving || gameRunning}
              >
                {saving ? <RefreshCw size={13} className="animate-spin" /> : <Save size={13} />}
                {saving ? "Repacking..." : "Save"}
              </Button>
            </div>
          )}
        </div>
      )}

      {/* Loading state */}
      {loading && (
        <div className="flex flex-1 min-h-0 items-center justify-center rounded-md border border-border">
          <RefreshCw size={20} className="animate-spin text-muted-foreground" />
        </div>
      )}

      {/* Empty state */}
      {!selectedPak && !loading && (
        <div className="flex flex-1 min-h-0 items-center justify-center rounded-md border border-border">
          <span className="text-[13px] text-muted-foreground">
            Select a pak with INI files to start editing
          </span>
        </div>
      )}

      {/* Delete-tab confirmation */}
      <AlertDialog open={pakSwitchOpen} onOpenChange={setPakSwitchOpen}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Discard unsaved changes?</AlertDialogTitle>
            <AlertDialogDescription>
              {pakSwitchPrompt !== null && (
                <>
                  Opening{" "}
                  <span className="font-mono text-foreground">{pakSwitchPrompt.pak.pak_name}</span>{" "}
                  discards {pakSwitchPrompt.changeCount} unsaved change
                  {pakSwitchPrompt.changeCount === 1 ? "" : "s"} in{" "}
                  <span className="font-mono text-foreground">{pakSwitchPrompt.fromName}</span>
                </>
              )}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={() => {
                setPakSwitchOpen(false);
                if (pakSwitchPrompt) void loadPak(pakSwitchPrompt.pak);
              }}
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
            >
              <Undo2 size={13} />
              Discard and open
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      <AlertDialog
        open={deleteConfirm !== null}
        onOpenChange={(open) => !open && setDeleteConfirm(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete INI file?</AlertDialogTitle>
            <AlertDialogDescription>
              {deleteConfirm !== null && (
                <>
                  <span className="font-mono text-foreground">{entryBasename(deleteConfirm)}</span>{" "}
                  will be removed from this pak on save. Discard the change to undo before saving.
                </>
              )}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={() => {
                if (deleteConfirm !== null) queueDelete(deleteConfirm);
                setDeleteConfirm(null);
              }}
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
            >
              <Trash2 size={13} />
              Delete
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  );
}

// ── Add INI popover ─────────────────────────────────────────────────

function AddIniPopover({
  existingEntries,
  adding,
  onAdd,
  customPath,
  setCustomPath,
}: {
  existingEntries: string[];
  adding: boolean;
  onAdd: (entry: string) => void;
  customPath: string;
  setCustomPath: (s: string) => void;
}) {
  const customParentDir = inferCustomParentDir(existingEntries);
  // Compare presets by basename: mod paks routinely put e.g. BaseEngine.ini at
  // a non-canonical path (Marvel/Config/ instead of Engine/Config/), and adding
  // another copy at the canonical path would create a duplicate at runtime.
  const existingBasenames = new Set(existingEntries.map((e) => entryBasename(e).toLowerCase()));
  const customError = validateCustomPath(customPath, existingEntries);
  const trimmed = customPath.trim();
  return (
    <div className="flex flex-col gap-3">
      <div>
        <div className="mb-1.5 flex items-center gap-2 text-[11px] font-semibold uppercase tracking-wide text-muted-foreground">
          <span>Presets</span>
          {adding && <RefreshCw size={11} className="animate-spin" />}
        </div>
        <div className="flex flex-wrap gap-1">
          {PRESET_INI_FILES.map((name) => {
            const targetPath = PRESET_INI_PATHS[name];
            const already = existingBasenames.has(name.toLowerCase());
            const disabled = already || adding;
            return (
              <button
                key={name}
                disabled={disabled}
                onClick={() => onAdd(targetPath)}
                title={already ? `${name} is already in this pak` : `Add ${targetPath}`}
                className={cn(
                  "rounded border border-border px-2 py-1 text-[11px] font-medium transition-colors",
                  disabled
                    ? "cursor-not-allowed text-muted-foreground/40"
                    : "hover:border-foreground/40 hover:bg-muted"
                )}
              >
                {name}
              </button>
            );
          })}
        </div>
        <div className="mt-1.5 text-[10px] text-muted-foreground">
          Seeds new content from the game's pakchunk0 default when available.
        </div>
      </div>

      <div>
        <div className="mb-1.5 text-[11px] font-semibold uppercase tracking-wide text-muted-foreground">
          Custom path
        </div>
        <Input
          value={customPath}
          onChange={(e) => setCustomPath(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !customError && trimmed && !adding) onAdd(trimmed);
          }}
          placeholder={`${customParentDir}/MyOverride.ini`}
          disabled={adding}
          className="h-7 text-[11px] font-mono"
        />
        {customError && trimmed && (
          <div className="mt-1 text-[10px] text-destructive">{customError}</div>
        )}
        <div className="mt-2 flex justify-end">
          <Button
            variant="blue"
            size="sm"
            onClick={() => onAdd(trimmed)}
            disabled={!trimmed || customError !== null || adding}
          >
            {adding ? <RefreshCw size={12} className="animate-spin" /> : <Plus size={12} />}
            Add
          </Button>
        </div>
      </div>
    </div>
  );
}

function NewPakPopover({
  name,
  setName,
  creating,
  onCreate,
}: {
  name: string;
  setName: (s: string) => void;
  creating: boolean;
  onCreate: () => void;
}) {
  const preview = previewPakFilename(name);
  const canCreate = preview.length > 0 && !creating;
  return (
    <div className="flex flex-col gap-2">
      <div className="text-[11px] font-semibold uppercase tracking-wide text-muted-foreground">
        New pak name
      </div>
      <Input
        autoFocus
        value={name}
        onChange={(e) => setName(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && canCreate) onCreate();
        }}
        placeholder="MyConfigMod"
        disabled={creating}
        className="h-7 font-mono text-[11px]"
      />
      {preview && (
        <div className="truncate text-[10px] text-muted-foreground">
          Saves as <span className="font-mono text-foreground/80">{preview}</span> in{" "}
          <span className="font-mono">~mods</span>
        </div>
      )}
      <div className="mt-1 flex justify-end">
        <Button variant="blue" size="sm" onClick={onCreate} disabled={!canCreate}>
          {creating ? <RefreshCw size={12} className="animate-spin" /> : <FilePlus2 size={12} />}
          Create
        </Button>
      </div>
    </div>
  );
}

function validateCustomPath(raw: string, existingEntries: string[]): string | null {
  const trimmed = raw.trim();
  if (!trimmed) return null;
  if (!/\.ini$/i.test(trimmed)) return "Path must end with .ini";
  // Accept a clean or mount-prefixed path; the leading `../../../` is the canonical
  // prefix, so check for traversal segments on the remainder only.
  const full = ensureMountPrefix(trimmed);
  const relative = stripMountPrefixForDisplay(full);
  if (relative.split("/").some((seg) => seg === ".." || seg === ".")) {
    return "Path must not contain .. or .";
  }
  const lower = full.toLowerCase();
  if (existingEntries.some((e) => ensureMountPrefix(e).toLowerCase() === lower)) {
    return "An entry with this path already exists";
  }
  return null;
}
