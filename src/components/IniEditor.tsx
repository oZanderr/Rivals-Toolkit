import { useState, useEffect, useRef, useCallback, useMemo } from "react";

import {
  ChangeSet,
  EditorState,
  StateField,
  StateEffect,
  RangeSetBuilder,
  Text,
} from "@codemirror/state";
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
  SelectGroup,
  SelectItem,
  SelectLabel,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Tip } from "@/components/ui/tooltip";
import { editingKeys, editorTheme } from "@/lib/codeEditor";
import {
  countMatches,
  matchAtOrAfter,
  matchBefore,
  ordinalOf,
  replaceAllChanges,
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
  staged_path?: string;
}

interface IniText {
  content: string;
  encoding: string;
  modified: number | null;
}

/// Where the open tabs come from: the INIs inside a pak, or loose files on disk.
interface IniSource {
  kind: "pak" | "files";
  /// The pak's path, a loose file's path, or `PLUGINS_ID`.
  id: string;
  label: string;
  /// In-pak paths for a pak, absolute paths for loose files.
  entries: string[];
}

type NoticeType = "ok" | "err" | "info";

interface Props {
  gamePath: string;
  isActive: boolean;
  gameRunning: boolean;
  /// A loose .ini another tab asked to open.
  pendingFile?: string | null;
  onPendingFileConsumed?: () => void;
}

const PLUGINS_ID = "::plugins";

function pakSource(pak: PakIniListing): IniSource {
  return { kind: "pak", id: pak.pak_path, label: pak.pak_name, entries: pak.ini_entries };
}

function pluginsSource(files: string[]): IniSource {
  return { kind: "files", id: PLUGINS_ID, label: "Plugins", entries: files };
}

function fileSource(path: string): IniSource {
  return { kind: "files", id: path, label: entryBasename(path), entries: [path] };
}

function samePath(a: string, b: string): boolean {
  return normalizeFolderPath(a) === normalizeFolderPath(b);
}

function sameList(a: string[], b: string[]): boolean {
  return a.length === b.length && a.every((value, i) => samePath(value, b[i]));
}

/// The line ending a file uses, from its first line, so a save writes it back the way it came.
function eolOf(raw: string): string {
  const at = raw.indexOf("\n");
  if (at === -1) return "\r\n";
  return at > 0 && raw[at - 1] === "\r" ? "\r\n" : "\n";
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

// A save crosses to the backend as one JSON string, and a string has a hard ceiling of about
// half a gigabyte. This editor opens config files that individually run to a hundred megabytes
// and more, so saving several at once used to exceed it and fail with `RangeError: Invalid
// string length`. Anything past this goes over in pieces instead, which also means the whole
// file is never held as a single string on this side.
const STAGE_ABOVE = 4 << 20;
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

/// Split a file into the lines a document is built from, dropping carriage returns as it goes.
///
/// Normalising with a regex first copies the whole file: measured at 131 MB and 150 ms on this
/// editor's largest config, for a copy thrown away as soon as the document existed.
function toDoc(raw: string): Text {
  const lines = raw.split("\n");
  for (let i = 0; i < lines.length; i++) {
    if (lines[i].endsWith("\r")) lines[i] = lines[i].slice(0, -1);
  }
  return Text.of(lines);
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

export function IniEditor({
  gamePath,
  isActive,
  gameRunning,
  pendingFile,
  onPendingFileConsumed,
}: Props) {
  // ── Source selection ──
  const [paks, setPaks] = useState<PakIniListing[]>([]);
  const [pluginInis, setPluginInis] = useState<string[]>([]);
  const [openedFiles, setOpenedFiles] = useState<string[]>([]);
  const [selectedSource, setSelectedSource] = useState<IniSource | null>(null);
  const [scanning, setScanning] = useState(false);
  // Which entry is being read, if any. There is no whole-pak load to wait for any more: selecting
  // a pak reads its index only, and the tab strip is drawn from that while a file arrives behind it.
  const [entryLoading, setEntryLoading] = useState<string | null>(null);
  const [entryError, setEntryError] = useState<{ entry: string; message: string } | null>(null);
  // A load generation. A read that lands after a reload or a pak switch belongs to text that was
  // already discarded, and writing it back would resurrect it under the new pak's name.
  const loadTokenRef = useRef({});
  const inFlightRef = useRef<Set<string>>(new Set());

  // ── Editor content (per INI entry) ──
  const [activeEntry, setActiveEntry] = useState<string | null>(null);
  // Text as loaded from disk. It seeds a fresh editor and lists the tabs; it is not
  // updated per keystroke, because the live CodeMirror document is the source of truth.
  const [contents, setContents] = useState<Record<string, string | Text>>({});
  // Entries edited since the last load or save. A name instead of a second full copy of
  // the file: comparing multi-megabyte strings on every render is what this replaces.
  const [dirtySet, setDirtySet] = useState<ReadonlySet<string>>(new Set());
  // Entries that exist in the pak on disk, so a delete knows whether it has to reach it.
  const onDiskRef = useRef<Set<string>>(new Set());
  // Each file's own line ending, so a save writes it back the way it came.
  const eolRef = useRef<Map<string, string>>(new Map());
  // When each loose file was last read or written, so a save refuses one changed since.
  const modifiedRef = useRef<Map<string, number | null>>(new Map());
  // Entries queued for deletion on next save. Hidden from tabs but kept in
  // `contents` so discard cleanly restores them.
  const [pendingDeletes, setPendingDeletes] = useState<Set<string>>(new Set());
  const [deleteConfirm, setDeleteConfirm] = useState<string | null>(null);
  // Source the user asked to open while edits are unsaved, held until they confirm. The
  // wording is snapshotted rather than derived: acting on the dialog changes both the
  // selected source and the unsaved count, which would rewrite the text mid close animation.
  // Keeping the snapshot after the dialog closes is what holds the layout steady.
  const [sourceSwitchPrompt, setSourceSwitchPrompt] = useState<{
    source: IniSource;
    entry?: string;
    fromName: string;
    changeCount: number;
  } | null>(null);
  const [sourceSwitchOpen, setSourceSwitchOpen] = useState(false);
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
  const [sourceEpoch, setSourceEpoch] = useState(0);
  // Top-of-viewport document position, keyed by `${source id}::${entry}`. Stored
  // as a CM document offset (not pixels) so restore goes through CM's measurement
  // cycle and renders the lines instead of leaving a blank viewport.
  const entryScrollRef = useRef<Record<string, number>>({});
  // Cached per-entry CodeMirror state (undo history + selection) so tab switches
  // preserve history. Keyed by `${epoch}::${source id}::${entry}`; restored only on doc match.
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
  // Tabs come from the pak's own index, so every one of them is there before any is read.
  // Entries added this session are not in that index yet and follow it, in the insertion order
  // `contents` keeps. Pending deletes are hidden.
  const displayedEntries = useMemo(() => {
    const onDisk = selectedSource?.entries ?? [];
    const added = Object.keys(contents).filter((e) => !onDisk.includes(e));
    return [...onDisk, ...added].filter((e) => !pendingDeletes.has(e));
  }, [selectedSource, contents, pendingDeletes]);
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
  const [counted, setMatchInfo] = useState<MatchInfo>(EMPTY_MATCH_INFO);
  // A closed or empty search shows no matches, whatever was counted last.
  const matchInfo = searchOpen && searchTerm ? counted : EMPTY_MATCH_INFO;

  useEffect(() => {
    if (!searchOpen || !searchTerm) return;
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
  }, [searchTerm, caseSensitive, searchOpen, docVersion, activeEntry, sourceEpoch]);

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
      void refreshPluginInisRef.current();
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
        if (
          selectedSource?.kind === "pak" &&
          !merged.find((p) => p.pak_path === selectedSource.id)
        ) {
          setSelectedSource(null);
          setActiveEntry(null);
          setContents({});
          setDirtySet(new Set());
          onDiskRef.current = new Set();
          editorStatesRef.current.clear();
          setPendingDeletes(new Set());
          loadTokenRef.current = {};
          inFlightRef.current.clear();
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
    [gamePath, paks, selectedSource]
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
      requestLoadSource(pakSource(info));
      setNewPakOpen(false);
      setNewPakName("");
      // Refresh other tabs' mod lists. The editor skips its own source so this
      // empty (INI-less) pak isn't pruned from its own list.
      emitModsChanged({
        modsFolder: `${gamePath}\\MarvelGame\\Marvel\\Content\\Paks\\~mods`,
        source: "IniEditor",
      });
      showNotice(`Created ${info.pak_name}`, "ok");
    } catch (e) {
      showNotice(String(e), "err", 6000);
      console.error(e);
    } finally {
      setCreatingPak(false);
    }
  }

  // A loose .ini opens in the Plugins source when it is one of its files, else as a source of its
  // own. `plugins` is passed in when the caller has just listed it.
  function openLooseFile(path: string, plugins: string[] = pluginInis) {
    const inPlugins = plugins.find((p) => samePath(p, path));
    if (inPlugins) {
      requestLoadSource(pluginsSource(plugins), inPlugins);
      return;
    }
    const known = openedFiles.find((p) => samePath(p, path));
    if (!known) setOpenedFiles((prev) => [...prev, path]);
    requestLoadSource(fileSource(known ?? path));
  }

  async function openAny(path: string) {
    if (/\.ini$/i.test(path)) {
      openLooseFile(path);
      return;
    }
    try {
      const info = await invoke<PakIniListing | null>("inspect_pak_path_any_ini", {
        pakPath: path,
      });
      if (!info) {
        showNotice("No INI files found in that pak", "err");
        return;
      }
      setPaks((prev) => (prev.find((p) => p.pak_path === info.pak_path) ? prev : [...prev, info]));
      requestLoadSource(pakSource(info));
    } catch (e) {
      showNotice("Failed to read pak", "err");
      console.error(e);
    }
  }

  const openAnyRef = useRef(openAny);
  const openLooseFileRef = useRef(openLooseFile);
  useEffect(() => {
    openAnyRef.current = openAny;
    openLooseFileRef.current = openLooseFile;
  });

  async function browse() {
    const selected = await open({
      multiple: false,
      filters: [{ name: "Pak or INI", extensions: ["pak", "ini"] }],
    });
    if (typeof selected === "string") await openAny(selected);
  }

  // Drag-and-drop: a .pak or .ini opens the same as browsing to it.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    getCurrentWindow()
      .onDragDropEvent(async (event) => {
        if (event.payload.type === "enter") {
          if (isActiveRef.current) setIsDragging(true);
        } else if (event.payload.type === "drop") {
          setIsDragging(false);
          if (!isActiveRef.current) return;
          const openable = event.payload.paths.find((p) => /\.(pak|ini)$/i.test(p));
          if (openable) await openAnyRef.current(openable);
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
    if (selectedSource?.kind !== "pak") return;
    return onPakChanged((e) => {
      if (e.source === "IniEditor") return;
      if (e.pakPath !== selectedSource.id) return;
      if (isDirty) {
        showNotice("Pak changed elsewhere; reload manually to discard edits", "info", 6000);
        return;
      }
      loadSource(selectedSource);
    });
  }, [selectedSource, isDirty]); // eslint-disable-line react-hooks/exhaustive-deps

  // Selecting a pak reads its index, not its contents. A config mod can hold hundreds of
  // megabytes of INI across its files, so each one is read when its tab is first opened rather
  // than up front for files you may never look at.
  function loadSource(source: IniSource, entry?: string) {
    const isSourceSwitch = selectedSource?.id !== source.id;
    setSelectedSource(source);
    setContents({});
    setDirtySet(new Set());
    editorStatesRef.current.clear();
    setPendingDeletes(new Set());
    setEntryLoading(null);
    setEntryError(null);
    loadTokenRef.current = {};
    inFlightRef.current.clear();
    onDiskRef.current = new Set(source.entries);
    eolRef.current = new Map();
    modifiedRef.current = new Map();
    setSourceEpoch((n) => n + 1);

    // Preserve the active entry across reloads when it still exists; otherwise pick the first.
    if (entry !== undefined) {
      setActiveEntry(entry);
    } else if (isSourceSwitch || activeEntry === null || !source.entries.includes(activeEntry)) {
      setActiveEntry(source.entries[0] ?? null);
    }
  }

  // Opening another source drops every unsaved edit, the same as Discard, so it asks first.
  // An external change to the current pak is already refused while dirty; this closes the
  // matching hole for switches the user starts.
  function requestLoadSource(source: IniSource, entry?: string) {
    // Any load wipes the buffers, so the prompt is about being dirty, not about which source
    // was picked: browsing to the one already open would discard just as much.
    if (isDirty) {
      setSourceSwitchPrompt({
        source,
        entry,
        fromName: selectedSource?.label ?? "",
        changeCount: pendingChanges.length,
      });
      setSourceSwitchOpen(true);
      return;
    }
    loadSource(source, entry);
  }

  // Lists the plugins folder's configs. An open, unedited Plugins source follows the new list;
  // one with edits keeps its tabs until it is saved or reloaded.
  async function refreshPluginInis(): Promise<string[]> {
    if (!gamePath) return [];
    let found: string[];
    try {
      found = await invoke<string[]>("list_plugin_inis", { gameRoot: gamePath });
    } catch (e) {
      console.error("Could not list plugin INIs:", e);
      return pluginInis;
    }
    setPluginInis((prev) => (sameList(prev, found) ? prev : found));
    if (selectedSource?.id === PLUGINS_ID && !isDirty && !sameList(selectedSource.entries, found)) {
      if (found.length > 0) {
        loadSource(pluginsSource(found));
      } else {
        setSelectedSource(null);
        setActiveEntry(null);
      }
    }
    return found;
  }

  const refreshPluginInisRef = useRef(refreshPluginInis);
  useEffect(() => {
    refreshPluginInisRef.current = refreshPluginInis;
  });

  // The Plugins tab adds and removes these files, so look again whenever this tab is shown.
  useEffect(() => {
    if (isActive) void refreshPluginInisRef.current();
  }, [isActive, gamePath]);

  useEffect(() => {
    if (!pendingFile) return;
    onPendingFileConsumed?.();
    void refreshPluginInisRef
      .current()
      .then((plugins) => openLooseFileRef.current(pendingFile, plugins));
  }, [pendingFile]); // eslint-disable-line react-hooks/exhaustive-deps

  function reload() {
    if (!selectedSource) return;
    loadSource(selectedSource);
    showNotice("Reloaded from disk", "ok");
  }

  // Throwing the edits away means going back to what the pak holds, so re-read it rather
  // than keeping a second copy of every file around purely to answer this.
  function discard() {
    if (!isDirty || !selectedSource) return;
    loadSource(selectedSource);
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
    if (onDiskRef.current.has(entry) || contents[entry] !== undefined) {
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

  // Reads the active entry the first time it is opened. This is the only place a file's text is
  // read; everything else about one comes from its source's listing.
  useEffect(() => {
    const source = selectedSource;
    const entry = activeEntry;
    if (!source || entry === null) return;
    if (contents[entry] !== undefined || !onDiskRef.current.has(entry)) return;
    if (inFlightRef.current.has(entry)) return;

    const token = loadTokenRef.current;
    inFlightRef.current.add(entry);
    setEntryLoading(entry);
    setEntryError(null);
    void (async () => {
      try {
        let raw: string;
        let modified: number | null = null;
        if (source.kind === "pak") {
          raw = await invoke<string>("extract_pak_ini", { pakPath: source.id, entry });
        } else {
          const read = await invoke<IniText>("read_ini_file", { path: entry });
          raw = read.content;
          modified = read.modified;
        }
        if (loadTokenRef.current !== token) return;
        eolRef.current.set(entry, eolOf(raw));
        if (source.kind === "files") modifiedRef.current.set(entry, modified);
        const text = toDoc(raw);
        setContents((prev) => (prev[entry] !== undefined ? prev : { ...prev, [entry]: text }));
      } catch (e) {
        console.error(`Failed to read ${entry}:`, e);
        if (loadTokenRef.current === token) setEntryError({ entry, message: String(e) });
      } finally {
        if (loadTokenRef.current === token) {
          inFlightRef.current.delete(entry);
          setEntryLoading((cur) => (cur === entry ? null : cur));
        }
      }
    })();
  }, [activeEntry, selectedSource, contents]);

  // A dirty entry lives in the editor, not in React state, so read it back from there.
  function docFor(entry: string): Text | null {
    if (entry === activeEntry && editorViewRef.current) return editorViewRef.current.state.doc;
    const cached = selectedSource
      ? editorStatesRef.current.get(`${sourceEpoch}::${selectedSource.id}::${entry}`)
      : undefined;
    if (cached) return cached.state.doc;
    const seed = contents[entry];
    if (seed === undefined) return null;
    return typeof seed === "string" ? Text.of(seed.split("\n")) : seed;
  }

  function eolFor(entry: string): string {
    return eolRef.current.get(entry) ?? "\r\n";
  }

  // Hands a document over in pieces and answers where the backend assembled it. Lines are
  // joined exactly as `sliceString(.., eol)` would, so a staged file and an inline one are
  // the same bytes.
  async function stageDoc(doc: Text, eol: string): Promise<string> {
    let path: string | null = null;
    let buffer = "";
    let first = true;
    const flush = async () => {
      if (!buffer) return;
      path = await invoke<string>("stage_pak_ini_chunk", { path, chunk: buffer });
      buffer = "";
    };
    for (const iter = doc.iterLines(); !iter.next().done; ) {
      buffer += first ? iter.value : eol + iter.value;
      first = false;
      if (buffer.length >= STAGE_ABOVE) await flush();
    }
    await flush();
    if (path === null) path = await invoke<string>("stage_pak_ini_chunk", { path, chunk: "" });
    return path;
  }

  // Loose files are written one by one. One that fails stops the rest, and only those
  // written so far stop being dirty.
  async function saveFiles() {
    const written = new Map<string, Text>();
    try {
      for (const entry of dirtyEntries) {
        const doc = docFor(entry);
        if (!doc) continue;
        const eol = eolFor(entry);
        const large = doc.length >= STAGE_ABOVE;
        const modified = await invoke<number>("write_ini_file", {
          path: entry,
          content: large ? "" : doc.sliceString(0, doc.length, eol),
          stagedPath: large ? await stageDoc(doc, eol) : null,
          expectedModified: modifiedRef.current.get(entry) ?? null,
        });
        modifiedRef.current.set(entry, modified);
        written.set(entry, doc);
      }
      showNotice(
        written.size === 1
          ? `Saved ${entryBasename([...written.keys()][0])}`
          : `Saved ${written.size} files`,
        "ok"
      );
    } catch (e) {
      showNotice(String(e), "err", 8000);
      console.error(e);
    } finally {
      setContents((prev) => ({ ...prev, ...Object.fromEntries(written) }));
      setDirtySet((prev) => {
        const next = new Set(prev);
        for (const entry of written.keys()) next.delete(entry);
        return next;
      });
    }
  }

  async function save() {
    if (!selectedSource || !isDirty || saving) return;
    if (selectedSource.kind === "pak" && gameRunning) {
      showNotice("Close the game to save changes", "err");
      return;
    }
    setSaving(true);
    if (selectedSource.kind === "files") {
      await saveFiles();
      setSaving(false);
      return;
    }
    try {
      // sliceString emits the file's own line ending directly, instead of a toString plus a
      // regex pass over the whole file.
      const files: PakIniFileContent[] = [];
      const saved = new Map<string, Text>();
      for (const entry of dirtyEntries) {
        const doc = docFor(entry);
        if (!doc) continue;
        saved.set(entry, doc);
        const eol = eolFor(entry);
        if (doc.length >= STAGE_ABOVE) {
          files.push({ entry, content: "", staged_path: await stageDoc(doc, eol) });
        } else {
          files.push({ entry, content: doc.sliceString(0, doc.length, eol) });
        }
      }
      const deletes = [...pendingDeletes].filter((entry) => onDiskRef.current.has(entry));

      const msg = await invoke<string>("save_pak_ini", {
        pakPath: selectedSource.id,
        files,
        deletes,
      });
      showNotice(msg, "ok");
      emitPakChanged({ pakPath: selectedSource.id, source: "IniEditor" });

      // Adding or deleting an entry changes what the pak holds, and the listing captured
      // at selection time no longer describes it. Reload and discard both re-read from
      // that listing, so a stale one loses an entry that was just saved.
      if (files.some((f) => !onDiskRef.current.has(f.entry)) || deletes.length > 0) {
        try {
          const fresh = await invoke<PakIniListing | null>("inspect_pak_path_any_ini", {
            pakPath: selectedSource.id,
          });
          if (fresh) {
            setSelectedSource(pakSource(fresh));
            onDiskRef.current = new Set(fresh.ini_entries);
            setPaks((prev) => prev.map((p) => (p.pak_path === fresh.pak_path ? fresh : p)));
          }
        } catch (e) {
          console.error("Failed to refresh pak listing after save:", e);
        }
      }

      // Everything that was dirty is now on disk. The editor still holds the text, so
      // there is no disk round-trip here and the cursor and undo history survive.
      //
      // Point `contents` at the documents that were written. It seeds a fresh editor, and a
      // cached state is only exempt from eviction while its entry is dirty, so leaving the
      // pre-save text here showed the save undone as soon as one aged out. Those documents are
      // already in memory, so this copies nothing.
      setContents((prev) => ({ ...prev, ...Object.fromEntries(saved) }));
      for (const entry of dirtyEntries) onDiskRef.current.add(entry);
      for (const entry of pendingDeletes) onDiskRef.current.delete(entry);
      setDirtySet(new Set());
      // Drop deleted (and discarded-add) entries from contents.
      if (pendingDeletes.size > 0) {
        setContents((prev) => {
          const next: Record<string, string | Text> = {};
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
        setActiveEntry(displayedEntries.find((e) => e !== activeEntry) ?? null);
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

  // Scrolling has to go through the editor rather than the scroller. `coordsAtPos` only answers
  // for positions the editor has actually rendered, and it renders a window around the viewport,
  // so on a file of a million lines every match outside that window returned null and the view
  // silently stayed put while the counter moved.
  function scrollEffect(pos: number) {
    return EditorView.scrollIntoView(pos, { y: "center" });
  }

  function jumpToMatch(pos: number, index: number) {
    const view = editorViewRef.current;
    if (!view) return;
    view.dispatch({
      selection: { anchor: pos, head: pos + searchTerm.length },
      effects: scrollEffect(pos),
    });
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

    view.dispatch({
      selection: { anchor: next, head: next + searchTerm.length },
      effects: scrollEffect(next),
    });
    setMatchInfo((prev) => ({ ...prev, count, index }));
  }

  // A common key matches hundreds of thousands of lines in a large config. One change per changed
  // line keeps the rope sharing everything it does not touch, and one dispatch keeps the whole
  // rewrite to a single press of undo.
  function replaceAllMatches() {
    const view = editorViewRef.current;
    if (!view || !searchTerm) return;

    const doc = view.state.doc;
    const { changes, replaced } = replaceAllChanges(doc, searchTerm, replaceTerm, caseSensitive);
    if (replaced === 0) return;

    // The cursor has to be placed against the document the replace leaves behind: a replacement
    // shorter than the term moves the end of the file, and an anchor measured before that throws.
    const set = ChangeSet.of(changes, doc.length);
    view.dispatch({
      changes: set,
      selection: { anchor: Math.min(view.state.selection.main.from, set.newLength) },
      scrollIntoView: false,
      userEvent: "input.replace.all",
    });
    showNotice(`Replaced ${replaced} occurrence${replaced !== 1 ? "s" : ""}`, "ok");
  }

  // ── CodeMirror setup ──

  useEffect(() => {
    if (!editorContainerRef.current || currentContent === null) return;

    // Capture identity of the file shown at mount so cleanup saves scroll under
    // the right key even if activeEntry/selectedSource have already changed by then.
    const scrollKey = selectedSource && activeEntry ? `${selectedSource.id}::${activeEntry}` : null;
    // Cached states are keyed by load epoch as well. This effect's cleanup runs after the
    // next load has already cleared the cache, so an epoch-free key would let the outgoing
    // (pre-reload) document be written back and then restored over the fresh one.
    const stateKey = scrollKey === null ? null : `${sourceEpoch}::${scrollKey}`;

    // Destroy previous editor if switching files
    if (editorViewRef.current) {
      editorViewRef.current.destroy();
      editorViewRef.current = null;
    }

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
      if (!key.startsWith(`${sourceEpoch}::`)) editorStatesRef.current.delete(key);
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
          editorTheme(),
          cmKeymap,
          editingKeys(),
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
    // The same record for the life of the editor, held here so the cleanup writes where this
    // effect read.
    const scrollPositions = entryScrollRef.current;
    if (scrollKey !== null) {
      const savedPos = scrollPositions[scrollKey];
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
        scrollPositions[scrollKey] = block.from;
        cacheEditorState(stateKey, view.state, activeEntry ?? "");
      }
      view.destroy();
      editorViewRef.current = null;
    };
    // Recreate when switching entries, when a load brings new content (sourceEpoch bump), and when
    // the active entry's text arrives: that read lands after this effect has already run, so
    // without the last one the editor would never mount. `contents` is not written per keystroke,
    // so this does not churn while typing, and a remount restores unsaved work from the cache.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeEntry, sourceEpoch, currentContent]);

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
        effects: [effects, scrollEffect(first)],
        selection: { anchor: first, head: first + search.length },
      });
    } else {
      setMatchInfo((prev) => ({ ...prev, index: -1 }));
      view.dispatch({ effects });
    }
    // Only re-run when search params change, not when content changes.
  }, [searchOpen, searchTerm, caseSensitive]);

  // ── Ctrl+F / Ctrl+S / Esc when this tab is active ──
  useEffect(() => {
    if (!isActive) return;
    function onKeyDown(e: KeyboardEvent) {
      if (e.key === "f" && (e.ctrlKey || e.metaKey)) {
        e.preventDefault();
        openSearch();
      } else if (e.key.toLowerCase() === "s" && (e.ctrlKey || e.metaKey)) {
        // The editor's own binding has already saved when it had focus.
        if (e.defaultPrevented) return;
        e.preventDefault();
        void saveRef.current();
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
      if (event.source === "IniEditor") return;
      const modsFolder = `${gamePath}\\MarvelGame\\Marvel\\Content\\Paks\\~mods`;
      if (normalizeFolderPath(event.modsFolder) !== normalizeFolderPath(modsFolder)) return;
      scanRef.current(true);
    });
  }, [gamePath]);

  const pluginsEntry = pluginInis.length > 0 ? pluginsSource(pluginInis) : null;
  const pakSources = useMemo(() => paks.map(pakSource), [paks]);
  const fileSources = useMemo(() => openedFiles.map(fileSource), [openedFiles]);
  const allSources = [...(pluginsEntry ? [pluginsEntry] : []), ...pakSources, ...fileSources];
  const isPak = selectedSource?.kind === "pak";

  // Auto-select when exactly one pak is found
  useEffect(() => {
    if (paks.length === 1 && !selectedSource) {
      loadSource(pakSource(paks[0]));
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps -- fire-once after scan populates paks
  }, [paks, selectedSource]);

  return (
    <div className="relative flex flex-1 min-h-0 w-full flex-col gap-4">
      {isDragging && (
        <div className="pointer-events-none absolute inset-0 z-50 flex flex-col items-center justify-center gap-3 rounded-lg border-2 border-dashed border-ok bg-background/80 backdrop-blur-sm">
          <UploadCloud size={36} className="text-ok" />
          <span className="text-sm font-semibold text-ok">Drop a .pak or .ini to open</span>
        </div>
      )}
      {/* ── Header ── */}
      <div className="flex min-h-8 items-center gap-3">
        <h2 className="text-xl font-bold">INI Editor</h2>
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

      {/* ── Source selection ── */}
      <div className="flex items-center gap-1">
        <Select
          value={selectedSource?.id ?? ""}
          onValueChange={(value) => {
            const source = allSources.find((s) => s.id === value);
            if (source) requestLoadSource(source);
          }}
          disabled={allSources.length === 0}
        >
          <SelectTrigger size="sm" className="min-w-0 flex-1 text-left font-mono text-xs">
            <SelectValue
              className="block min-w-0 max-w-full truncate text-left"
              placeholder={
                allSources.length === 0
                  ? "Nothing to open yet: scan, browse, or drop a .pak or .ini"
                  : "Select a pak or INI file..."
              }
            />
          </SelectTrigger>
          <SelectContent position="popper" className="w-(--radix-select-trigger-width)">
            {pluginsEntry && (
              <SelectGroup>
                <SelectLabel className="text-[11px]">Plugins</SelectLabel>
                <SelectItem value={PLUGINS_ID} className="font-mono text-xs">
                  Plugin configs
                </SelectItem>
              </SelectGroup>
            )}
            {pakSources.length > 0 && (
              <SelectGroup>
                <SelectLabel className="text-[11px]">Mods</SelectLabel>
                {pakSources.map((s) => (
                  <SelectItem key={s.id} value={s.id} className="font-mono text-xs">
                    {s.label}
                  </SelectItem>
                ))}
              </SelectGroup>
            )}
            {fileSources.length > 0 && (
              <SelectGroup>
                <SelectLabel className="text-[11px]">Opened files</SelectLabel>
                {fileSources.map((s) => (
                  <SelectItem key={s.id} value={s.id} className="font-mono text-xs">
                    {s.label}
                  </SelectItem>
                ))}
              </SelectGroup>
            )}
          </SelectContent>
        </Select>
        <Tip content="Open a pak or INI file">
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
      {selectedSource && (
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
                          isPak && displayedEntries.length > 1 ? "pr-2" : "pr-3",
                          isActive
                            ? "text-foreground"
                            : "text-muted-foreground hover:text-foreground"
                        )}
                      >
                        <FileText size={12} />
                        {entryBasename(entry)}
                        {entryDirty && <span className="size-1.5 rounded-full bg-warn" />}
                      </button>
                      {isPak && displayedEntries.length > 1 && (
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
                {isPak && (
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
                )}
              </div>
            </div>

            <div className="flex shrink-0 items-center gap-1">
              <Tip content="Reload from disk">
                <Button variant="ghost" size="sm" onClick={reload} disabled={saving}>
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
          ) : entryLoading !== null && entryLoading === activeEntry ? (
            <div className="flex flex-1 min-h-0 items-center justify-center">
              <RefreshCw size={20} className="animate-spin text-muted-foreground" />
            </div>
          ) : entryError !== null && entryError.entry === activeEntry ? (
            <div className="flex flex-1 min-h-0 flex-col items-center justify-center gap-2 px-4 text-center">
              <AlertTriangle size={22} className="text-warn" />
              <span className="text-[12px] text-muted-foreground">
                Could not read {entryBasename(entryError.entry)}: {entryError.message}
              </span>
            </div>
          ) : (
            <div className="flex flex-1 min-h-0 flex-col items-center justify-center gap-2 px-4 text-center">
              <FileText size={22} className="text-muted-foreground/50" />
              <span className="text-[12px] text-muted-foreground">
                {isPak ? "No INI file open. Use the + tab to add one." : "No INI file open."}
              </span>
            </div>
          )}

          {/* Save bar — only visible when there are pending edits */}
          {isDirty && (
            <div className="flex items-center justify-end gap-2 border-t border-border px-3 py-1.5">
              {gameRunning && isPak ? (
                <span className="mr-auto flex items-center gap-1.5 text-[11px] font-medium text-warn">
                  <AlertTriangle size={13} className="shrink-0" />
                  Close the game to save changes
                </span>
              ) : gameRunning ? (
                <span className="mr-auto flex items-center gap-1.5 text-[11px] text-muted-foreground">
                  <AlertTriangle size={13} className="shrink-0 text-warn" />
                  The game is running: a plugin may rewrite this file when it closes.
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
                disabled={!isDirty || saving || (gameRunning && isPak)}
              >
                {saving ? <RefreshCw size={13} className="animate-spin" /> : <Save size={13} />}
                {saving ? (isPak ? "Repacking..." : "Saving...") : "Save"}
              </Button>
            </div>
          )}
        </div>
      )}

      {/* Empty state */}
      {!selectedSource && (
        <div className="flex flex-1 min-h-0 items-center justify-center rounded-md border border-border">
          <span className="text-[13px] text-muted-foreground">
            Select a pak or INI file to start editing
          </span>
        </div>
      )}

      {/* Delete-tab confirmation */}
      <AlertDialog open={sourceSwitchOpen} onOpenChange={setSourceSwitchOpen}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Discard unsaved changes?</AlertDialogTitle>
            <AlertDialogDescription>
              {sourceSwitchPrompt !== null && (
                <>
                  Opening{" "}
                  <span className="font-mono text-foreground">
                    {sourceSwitchPrompt.source.label}
                  </span>{" "}
                  discards {sourceSwitchPrompt.changeCount} unsaved change
                  {sourceSwitchPrompt.changeCount === 1 ? "" : "s"} in{" "}
                  <span className="font-mono text-foreground">{sourceSwitchPrompt.fromName}</span>
                </>
              )}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={() => {
                setSourceSwitchOpen(false);
                if (sourceSwitchPrompt)
                  loadSource(sourceSwitchPrompt.source, sourceSwitchPrompt.entry);
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
