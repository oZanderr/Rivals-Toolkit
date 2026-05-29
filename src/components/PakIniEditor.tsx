import { useState, useEffect, useRef, useCallback, useMemo } from "react";

import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { EditorState, StateField, StateEffect, RangeSetBuilder } from "@codemirror/state";
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
import { emitModsChanged, normalizeFolderPath, onModsChanged } from "@/lib/modsEvents";
import { emitPakChanged, onPakChanged } from "@/lib/pakEvents";
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

function buildSearchDecos(view: EditorView): DecorationSet {
  const { search, caseSensitive } = view.state.field(searchConfigField);
  if (!search) return Decoration.none;

  const builder = new RangeSetBuilder<Decoration>();
  const doc = view.state.doc.toString();
  const hay = caseSensitive ? doc : doc.toLowerCase();
  const ndl = caseSensitive ? search : search.toLowerCase();
  const selFrom = view.state.selection.main.from;

  let idx = hay.indexOf(ndl);
  while (idx !== -1) {
    builder.add(idx, idx + ndl.length, idx === selFrom ? currentMatchMark : matchMark);
    idx = hay.indexOf(ndl, idx + 1);
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
      if (
        update.docChanged ||
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
    if (parent) return parent;
  }
  return "../../../Marvel/Config";
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
  const [contents, setContents] = useState<Record<string, string>>({});
  const [savedContents, setSavedContents] = useState<Record<string, string>>({});
  // Entries queued for deletion on next save. Hidden from tabs but kept in
  // `contents` so discard cleanly restores them.
  const [pendingDeletes, setPendingDeletes] = useState<Set<string>>(new Set());
  const [deleteConfirm, setDeleteConfirm] = useState<string | null>(null);
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
  const [currentMatchIndex, setCurrentMatchIndex] = useState(-1);
  const searchInputRef = useRef<HTMLInputElement>(null);

  // ── CodeMirror ──
  const editorContainerRef = useRef<HTMLDivElement>(null);
  const editorViewRef = useRef<EditorView | null>(null);
  // Bumped on disk reload / pak switch to force editor recreation.
  const [pakEpoch, setPakEpoch] = useState(0);
  // Top-of-viewport document position, keyed by `${pak_path}::${entry}`. Stored
  // as a CM document offset (not pixels) so restore goes through CM's measurement
  // cycle and renders the lines instead of leaving a blank viewport.
  const entryScrollRef = useRef<Record<string, number>>({});

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
  const dirtyEntries = useMemo(() => {
    const out: string[] = [];
    for (const [entry, value] of Object.entries(contents)) {
      if (pendingDeletes.has(entry)) continue;
      if (savedContents[entry] !== value) out.push(entry);
    }
    return out;
  }, [contents, savedContents, pendingDeletes]);
  // Tabs ignore pending-delete entries; new entries naturally appear via
  // Object.keys order (insertion-order).
  const displayedEntries = useMemo(
    () => Object.keys(contents).filter((e) => !pendingDeletes.has(e)),
    [contents, pendingDeletes]
  );
  // Save fires if there are edits OR pending deletes of entries that were on disk.
  const hasRealDeletes = useMemo(
    () => [...pendingDeletes].some((e) => savedContents[e] !== undefined),
    [pendingDeletes, savedContents]
  );
  const isDirty = dirtyEntries.length > 0 || hasRealDeletes;

  const currentContent = activeEntry !== null ? (contents[activeEntry] ?? null) : null;

  const setCurrentContent = useCallback(
    (value: string) => {
      if (activeEntry === null) return;
      setContents((prev) => ({ ...prev, [activeEntry]: value }));
    },
    [activeEntry]
  );

  // ── Match positions ──
  const matchPositions = useMemo(() => {
    if (!searchTerm || !currentContent || !searchOpen) return [];
    const hay = caseSensitive ? currentContent : currentContent.toLowerCase();
    const ndl = caseSensitive ? searchTerm : searchTerm.toLowerCase();
    if (ndl.length === 0) return [];
    const positions: number[] = [];
    let idx = hay.indexOf(ndl);
    while (idx !== -1) {
      positions.push(idx);
      idx = hay.indexOf(ndl, idx + ndl.length);
    }
    return positions;
  }, [searchTerm, currentContent, caseSensitive, searchOpen]);

  // ── Pak scanning ──
  const scan = useCallback(
    async (silent = false) => {
      if (!gamePath) return;
      setScanning(true);
      try {
        const results = await invoke<PakIniListing[]>("scan_mod_paks_any_ini", {
          gameRoot: gamePath,
        });
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
          setSavedContents({});
          setPendingDeletes(new Set());
        }
        if (merged.length === 0) {
          if (!silent) showNotice("No paks with INI files found", "info");
        } else if (!silent) {
          showNotice(`Found ${merged.length} pak${merged.length !== 1 ? "s" : ""} with INI`, "ok");
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
      await loadPak(info);
      setNewPakOpen(false);
      setNewPakName("");
      // A new pak file landed in ~mods; refresh other tabs' mod lists. The editor
      // ignores its own source so this freshly-created (INI-less) pak isn't pruned.
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
      await loadPak(info);
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
            await loadPak(info);
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
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

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

  async function loadPak(pak: PakIniListing) {
    const isPakSwitch = selectedPak?.pak_path !== pak.pak_path;
    setSelectedPak(pak);
    setContents({});
    setSavedContents({});
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
      setSavedContents(loaded);
      setPakEpoch((n) => n + 1);

      // Preserve user's active entry across reloads when it still exists; otherwise pick the first.
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

  async function reload() {
    if (!selectedPak) return;
    await loadPak(selectedPak);
    showNotice("Reloaded from disk", "ok");
  }

  function discard() {
    if (!isDirty) return;
    const view = editorViewRef.current;
    const target = activeEntry !== null ? (savedContents[activeEntry] ?? "") : null;
    if (view && target !== null) {
      view.dispatch({
        changes: { from: 0, to: view.state.doc.length, insert: target },
      });
    }
    setContents(savedContents);
    setPendingDeletes(new Set());
    // Active entry may have been a brand-new add or a pending-delete; snap to a
    // valid remaining entry.
    if (activeEntry === null || savedContents[activeEntry] === undefined) {
      const firstSaved = Object.keys(savedContents)[0] ?? null;
      setActiveEntry(firstSaved);
    }
  }

  // Queue an entry for deletion on next save. Brand-new entries (not in
  // savedContents) drop entirely from contents so the popover treats the name
  // as available again.
  function queueDelete(entry: string) {
    const wasOnDisk = savedContents[entry] !== undefined;
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
    setActiveEntry(entry);
  }

  async function save() {
    if (!selectedPak || !isDirty) return;
    setSaving(true);
    try {
      const files: PakIniFileContent[] = dirtyEntries.map((entry) => ({
        entry,
        content: contents[entry].replace(/\r?\n/g, "\r\n"),
      }));
      const deletes = [...pendingDeletes].filter((entry) => savedContents[entry] !== undefined);

      const msg = await invoke<string>("save_pak_ini", {
        pakPath: selectedPak.pak_path,
        files,
        deletes,
      });
      showNotice(msg, "ok");
      emitPakChanged({ pakPath: selectedPak.pak_path, source: "PakIniEditor" });

      // Rebuild saved snapshot from in-memory contents minus deletes; skip the
      // disk round-trip to preserve cursor and active entry.
      const newSaved: Record<string, string> = {};
      for (const [entry, value] of Object.entries(contents)) {
        if (pendingDeletes.has(entry)) continue;
        newSaved[entry] = value;
      }
      setSavedContents(newSaved);
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

  function jumpToMatch(index: number) {
    const view = editorViewRef.current;
    if (!view || matchPositions.length === 0) return;
    const pos = matchPositions[index];
    view.dispatch({ selection: { anchor: pos, head: pos + searchTerm.length } });
    scrollToPos(view, pos);
    setCurrentMatchIndex(index);
  }

  function findNext() {
    if (matchPositions.length === 0) return;
    jumpToMatch((currentMatchIndex + 1) % matchPositions.length);
  }

  function findPrev() {
    if (matchPositions.length === 0) return;
    jumpToMatch((currentMatchIndex - 1 + matchPositions.length) % matchPositions.length);
  }

  function replaceOne() {
    const view = editorViewRef.current;
    if (!view || matchPositions.length === 0 || currentMatchIndex < 0) return;
    const pos = matchPositions[currentMatchIndex];
    view.dispatch({
      changes: { from: pos, to: pos + searchTerm.length, insert: replaceTerm },
    });
  }

  function replaceAllMatches() {
    const view = editorViewRef.current;
    if (!view || matchPositions.length === 0) return;
    const count = matchPositions.length;
    view.dispatch({
      changes: matchPositions.map((pos) => ({
        from: pos,
        to: pos + searchTerm.length,
        insert: replaceTerm,
      })),
    });
    showNotice(`Replaced ${count} occurrence${count !== 1 ? "s" : ""}`, "ok");
  }

  // ── CodeMirror setup ──
  // Ref to access current search state in the editor creation effect
  const searchStateRef = useRef({ open: false, term: "", caseSensitive: false });
  useEffect(() => {
    searchStateRef.current = { open: searchOpen, term: searchTerm, caseSensitive };
  });

  useEffect(() => {
    if (!editorContainerRef.current || currentContent === null) return;

    // Capture identity of the file shown at mount so cleanup saves scroll under
    // the right key even if activeEntry/selectedPak have already changed by then.
    const scrollKey = selectedPak && activeEntry ? `${selectedPak.pak_path}::${activeEntry}` : null;

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
      if (update.docChanged) {
        setCurrentContent(update.state.doc.toString());
      }
    });

    const view = new EditorView({
      state: EditorState.create({
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
      }),
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
          effects: EditorView.scrollIntoView(savedPos, { y: "start" }),
        });
      }
    }

    return () => {
      if (scrollKey !== null) {
        const block = view.lineBlockAtHeight(view.scrollDOM.scrollTop);
        entryScrollRef.current[scrollKey] = block.from;
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

    if (search && matchPositions.length > 0) {
      const pos = matchPositions[0];
      setCurrentMatchIndex(0);
      view.dispatch({
        effects,
        selection: { anchor: pos, head: pos + search.length },
      });
      scrollToPos(view, pos);
    } else {
      setCurrentMatchIndex(-1);
      view.dispatch({ effects });
    }
    // Only re-run when search params change, not when content changes
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [searchOpen, searchTerm, caseSensitive]);

  // ── Ctrl+F when this tab is active ──
  useEffect(() => {
    if (!isActive) return;
    function onKeyDown(e: KeyboardEvent) {
      if (e.key === "f" && (e.ctrlKey || e.metaKey)) {
        e.preventDefault();
        openSearch();
      }
    }
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [isActive]);

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
      // Skip our own create-pak emission; re-scanning would prune the new INI-less pak.
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
    setSavedContents({});
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
            if (pak) loadPak(pak);
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
                  const entryDirty = contents[entry] !== savedContents[entry];
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
                      existingEntries={Object.keys(contents).filter((e) => !pendingDeletes.has(e))}
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
                {matchPositions.length > 0
                  ? currentMatchIndex >= 0
                    ? `${currentMatchIndex + 1}/${matchPositions.length}`
                    : `${matchPositions.length} matches`
                  : searchTerm
                    ? "No results"
                    : ""}
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
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={findPrev}
                    disabled={matchPositions.length === 0}
                  >
                    <ChevronUp size={13} />
                  </Button>
                </Tip>
                <Tip content="Next Match">
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={findNext}
                    disabled={matchPositions.length === 0}
                  >
                    <ChevronDown size={13} />
                  </Button>
                </Tip>
                <Tip content="Replace">
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={replaceOne}
                    disabled={matchPositions.length === 0}
                  >
                    <Replace size={13} />
                  </Button>
                </Tip>
                <Tip content="Replace All">
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={replaceAllMatches}
                    disabled={matchPositions.length === 0}
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
                  {dirtyEntries.length === 1
                    ? ` (${entryBasename(dirtyEntries[0])})`
                    : ` (${dirtyEntries.length} files)`}
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

function previewPakFilename(raw: string): string {
  const trimmed = raw
    .trim()
    .replace(/[<>:"/\\|?*]/g, "")
    .replace(/\.pak$/i, "")
    .replace(/_9999999_P$/i, "");
  if (!trimmed) return "";
  return `${trimmed}_9999999_P.pak`;
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
  if (trimmed.split(/[/\\]/).some((seg) => seg === ".." || seg === ".")) {
    return "Path must not contain .. or .";
  }
  const lower = trimmed.toLowerCase();
  if (existingEntries.some((e) => e.toLowerCase() === lower)) {
    return "An entry with this path already exists";
  }
  return null;
}
