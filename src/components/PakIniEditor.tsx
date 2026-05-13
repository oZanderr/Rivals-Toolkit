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
  FolderOpen,
  FileText,
  ListRestart,
  CaseSensitive,
  ChevronUp,
  ChevronDown,
  Replace,
  ReplaceAll,
  Undo2,
  UploadCloud,
  X,
} from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Tip } from "@/components/ui/tooltip";
import { normalizeFolderPath, onModsChanged } from "@/lib/modsEvents";
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

function normalizeLineEndings(s: string): string {
  return s.replace(/\r\n/g, "\n");
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
  const dirtyEntries = useMemo(() => {
    const out: string[] = [];
    for (const [entry, value] of Object.entries(contents)) {
      if (savedContents[entry] !== value) out.push(entry);
    }
    return out;
  }, [contents, savedContents]);
  const isDirty = dirtyEntries.length > 0;

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
  }

  async function save() {
    if (!selectedPak || !isDirty) return;
    setSaving(true);
    try {
      const files: PakIniFileContent[] = dirtyEntries.map((entry) => ({
        entry,
        content: contents[entry].replace(/\r?\n/g, "\r\n"),
      }));

      const msg = await invoke<string>("save_pak_ini", {
        pakPath: selectedPak.pak_path,
        files,
      });
      showNotice(msg, "ok");
      emitPakChanged({ pakPath: selectedPak.pak_path, source: "PakIniEditor" });

      // Sync saved snapshot to current buffer; skip disk round-trip to preserve cursor and active entry.
      setSavedContents(contents);
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
        lineHeight: "1.625",
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

    return () => {
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
  }, [gamePath]);

  const showTabs = !!selectedPak && selectedPak.ini_entries.length > 1;

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
      {selectedPak && !loading && currentContent !== null && activeEntry !== null && (
        <div className="flex flex-1 min-h-0 flex-col rounded-md border border-border overflow-hidden">
          {/* File tabs + toolbar */}
          <div className="flex items-center justify-between border-b border-border px-3 py-1.5">
            <div className="flex min-w-0 flex-1 items-center gap-2 overflow-x-auto">
              {showTabs ? (
                <div className="flex gap-1 rounded-md bg-muted p-1">
                  {selectedPak.ini_entries.map((entry) => {
                    const entryDirty = contents[entry] !== savedContents[entry];
                    return (
                      <button
                        key={entry}
                        onClick={() => setActiveEntry(entry)}
                        title={entry}
                        className={cn(
                          "flex items-center gap-1.5 rounded-sm px-3 py-1 text-[12px] font-medium transition-colors",
                          activeEntry === entry
                            ? "bg-background text-foreground shadow-sm"
                            : "text-muted-foreground hover:text-foreground"
                        )}
                      >
                        <FileText size={12} />
                        {entryBasename(entry)}
                        {entryDirty && <span className="size-1.5 rounded-full bg-warn" />}
                      </button>
                    );
                  })}
                </div>
              ) : (
                <span
                  className="flex items-center gap-1.5 text-[12px] font-medium text-foreground"
                  title={activeEntry}
                >
                  <FileText size={12} />
                  {entryBasename(activeEntry)}
                  {isDirty && <span className="size-1.5 rounded-full bg-warn" />}
                </span>
              )}
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

          {/* CodeMirror editor */}
          <div ref={editorContainerRef} className="flex-1 min-h-0 w-full overflow-hidden" />

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
    </div>
  );
}
