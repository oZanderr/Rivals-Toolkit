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
import { open } from "@tauri-apps/plugin-dialog";
import {
  CheckCircle2,
  XCircle,
  RefreshCw,
  Save,
  Search,
  FolderOpen,
  FileText,
  CaseSensitive,
  ChevronUp,
  ChevronDown,
  Replace,
  ReplaceAll,
  X,
} from "lucide-react";

import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { cn } from "@/lib/utils";

// ── Types matching Rust backend ─────────────────────────────────────

interface PakIniInfo {
  pak_name: string;
  pak_path: string;
  has_device_profiles: boolean;
  has_engine_ini: boolean;
  device_profiles_entry: string | null;
  engine_ini_entry: string | null;
}

interface PakIniFileContent {
  entry: string;
  content: string;
}

type IniFile = "device_profiles" | "engine";
type NoticeType = "ok" | "err" | "info";

interface Props {
  gamePath: string;
  isActive: boolean;
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

// ── Component ───────────────────────────────────────────────────────

export function PakIniEditor({ gamePath, isActive }: Props) {
  // ── Pak selection ──
  const [paks, setPaks] = useState<PakIniInfo[]>([]);
  const [selectedPak, setSelectedPak] = useState<PakIniInfo | null>(null);
  const [scanning, setScanning] = useState(false);
  const [loading, setLoading] = useState(false);

  // ── Editor content ──
  const [activeFile, setActiveFile] = useState<IniFile>("device_profiles");
  const [dpContent, setDpContent] = useState<string | null>(null);
  const [engineContent, setEngineContent] = useState<string | null>(null);
  const [savedDp, setSavedDp] = useState<string | null>(null);
  const [savedEngine, setSavedEngine] = useState<string | null>(null);
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

  // ── Notices ──
  const [notice, setNotice] = useState<{ msg: string; type: NoticeType } | null>(null);
  const noticeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  function showNotice(msg: string, type: NoticeType, duration = 4000) {
    if (noticeTimer.current) clearTimeout(noticeTimer.current);
    setNotice({ msg, type });
    noticeTimer.current = setTimeout(() => setNotice(null), duration);
  }

  // ── Dirty detection ──
  const dpDirty = dpContent !== null && dpContent !== savedDp;
  const engineDirty = engineContent !== null && engineContent !== savedEngine;
  const isDirty = dpDirty || engineDirty;

  const currentContent = activeFile === "device_profiles" ? dpContent : engineContent;
  const setCurrentContent = activeFile === "device_profiles" ? setDpContent : setEngineContent;

  // ── Match positions ──
  const matchPositions = useMemo(() => {
    if (!searchTerm || !currentContent || !searchOpen) return [];
    const hay = caseSensitive ? currentContent : currentContent.toLowerCase();
    const ndl = caseSensitive ? searchTerm : searchTerm.toLowerCase();
    const positions: number[] = [];
    let idx = hay.indexOf(ndl);
    while (idx !== -1) {
      positions.push(idx);
      idx = hay.indexOf(ndl, idx + 1);
    }
    return positions;
  }, [searchTerm, currentContent, caseSensitive, searchOpen]);

  // ── Pak scanning ──
  const scan = useCallback(
    async (silent = false) => {
      if (!gamePath) return;
      setScanning(true);
      try {
        const results = await invoke<PakIniInfo[]>("scan_mod_paks_for_ini", {
          gameRoot: gamePath,
        });
        setPaks(results);
        if (results.length === 0) {
          if (!silent) showNotice("No config mods found", "info");
        } else if (!silent) {
          showNotice(`Found ${results.length} config mod${results.length !== 1 ? "s" : ""}`, "ok");
        }
      } catch (e) {
        console.error("Scan failed:", e);
        if (!silent) showNotice("Scan failed", "err");
      } finally {
        setScanning(false);
      }
    },
    [gamePath]
  );

  async function browse() {
    const selected = await open({
      multiple: false,
      filters: [{ name: "Pak files", extensions: ["pak"] }],
    });
    if (typeof selected !== "string") return;
    try {
      const info = await invoke<PakIniInfo | null>("inspect_pak_path", { pakPath: selected });
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

  async function loadPak(pak: PakIniInfo) {
    setSelectedPak(pak);
    setDpContent(null);
    setEngineContent(null);
    setSavedDp(null);
    setSavedEngine(null);
    setLoading(true);

    try {
      let dp: string | null = null;
      let eng: string | null = null;

      if (pak.has_device_profiles && pak.device_profiles_entry) {
        dp = await invoke<string>("extract_pak_ini", {
          pakPath: pak.pak_path,
          entry: pak.device_profiles_entry,
        });
      }
      if (pak.has_engine_ini && pak.engine_ini_entry) {
        eng = await invoke<string>("extract_pak_ini", {
          pakPath: pak.pak_path,
          entry: pak.engine_ini_entry,
        });
      }

      // Normalize to \n so dirty detection matches CM's internal line endings
      const normDp = dp?.replace(/\r\n/g, "\n") ?? null;
      const normEng = eng?.replace(/\r\n/g, "\n") ?? null;
      setDpContent(normDp);
      setSavedDp(normDp);
      setEngineContent(normEng);
      setSavedEngine(normEng);

      // Auto-select the first available file
      if (dp !== null) setActiveFile("device_profiles");
      else if (eng !== null) setActiveFile("engine");
    } catch (e) {
      showNotice("Failed to extract INI files", "err");
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

  async function save() {
    if (!selectedPak || !isDirty) return;
    setSaving(true);
    try {
      const files: PakIniFileContent[] = [];

      if (dpDirty && dpContent !== null && selectedPak.device_profiles_entry) {
        files.push({
          entry: selectedPak.device_profiles_entry,
          content: dpContent.replace(/\r?\n/g, "\r\n"),
        });
      }
      if (engineDirty && engineContent !== null && selectedPak.engine_ini_entry) {
        files.push({
          entry: selectedPak.engine_ini_entry,
          content: engineContent.replace(/\r?\n/g, "\r\n"),
        });
      }

      const msg = await invoke<string>("save_pak_ini", {
        pakPath: selectedPak.pak_path,
        files,
      });
      showNotice(msg, "ok");

      // Reload from repacked pak to verify round-trip
      await loadPak(selectedPak);
    } catch (e) {
      showNotice(String(e), "err", 8000);
      console.error(e);
    } finally {
      setSaving(false);
    }
  }

  // ── Search functions ──
  const saveRef = useRef(save);
  saveRef.current = save;

  function openSearch() {
    setSearchOpen(true);
    setTimeout(() => searchInputRef.current?.focus(), 0);
  }

  const openSearchRef = useRef(openSearch);
  openSearchRef.current = openSearch;

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
  searchStateRef.current = { open: searchOpen, term: searchTerm, caseSensitive };

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
    // Only recreate when switching files or loading new content from disk
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeFile, savedDp, savedEngine]);

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

  // Auto-select when exactly one config mod is found
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
    setDpContent(null);
    setEngineContent(null);
    setSavedDp(null);
    setSavedEngine(null);
  }, [gamePath]);

  const hasBothFiles = selectedPak?.has_device_profiles && selectedPak?.has_engine_ini;

  return (
    <div className="flex flex-1 min-h-0 w-full flex-col gap-4">
      {/* ── Header ── */}
      <div className="flex min-h-8 items-center gap-3">
        <h2 className="text-xl font-bold">Pak INI Editor</h2>
        {notice && (
          <span
            className={cn(
              "flex items-center gap-1.5 text-[12px] font-medium",
              notice.type === "ok" && "text-[var(--color-ok)]",
              notice.type === "err" && "text-[var(--color-err)]",
              notice.type === "info" && "text-muted-foreground"
            )}
          >
            {notice.type === "ok" ? (
              <CheckCircle2 size={13} strokeWidth={2.5} />
            ) : notice.type === "err" ? (
              <XCircle size={13} strokeWidth={2.5} />
            ) : null}
            {notice.msg}
          </span>
        )}
      </div>

      {/* ── Pak selection ── */}
      <div className="flex items-center gap-2">
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
                paks.length === 0 ? "No config mods — scan or browse" : "Select a config mod..."
              }
            />
          </SelectTrigger>
          <SelectContent position="popper" className="w-[var(--radix-select-trigger-width)]">
            {paks.map((p) => (
              <SelectItem
                key={p.pak_path}
                value={p.pak_path}
                className="font-mono text-xs"
                title={p.pak_name}
              >
                {p.pak_name}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Button variant="outline" size="sm" onClick={browse} className="shrink-0">
          <FolderOpen size={14} />
          Browse
        </Button>
        <Button
          onClick={() => scan()}
          disabled={scanning || !gamePath}
          variant="blue"
          size="sm"
          className="shrink-0"
        >
          <RefreshCw size={14} className={cn(scanning && "animate-spin")} />
          Scan
        </Button>
      </div>

      {/* ── Editor area ── */}
      {selectedPak && !loading && currentContent !== null && (
        <Card className="flex flex-1 min-h-0 flex-col bg-card p-0 overflow-hidden">
          {/* File tabs + toolbar */}
          <div className="flex items-center justify-between border-b border-border px-3 py-1.5">
            <div className="flex items-center gap-2">
              {hasBothFiles ? (
                <div className="flex gap-1 rounded-md bg-muted p-1">
                  {selectedPak.has_device_profiles && (
                    <button
                      onClick={() => setActiveFile("device_profiles")}
                      className={cn(
                        "flex items-center gap-1.5 rounded-sm px-3 py-1 text-[12px] font-medium transition-colors",
                        activeFile === "device_profiles"
                          ? "bg-background text-foreground shadow-sm"
                          : "text-muted-foreground hover:text-foreground"
                      )}
                    >
                      <FileText size={12} />
                      DefaultDeviceProfiles.ini
                      {dpDirty && <span className="size-1.5 rounded-full bg-[var(--color-warn)]" />}
                    </button>
                  )}
                  {selectedPak.has_engine_ini && (
                    <button
                      onClick={() => setActiveFile("engine")}
                      className={cn(
                        "flex items-center gap-1.5 rounded-sm px-3 py-1 text-[12px] font-medium transition-colors",
                        activeFile === "engine"
                          ? "bg-background text-foreground shadow-sm"
                          : "text-muted-foreground hover:text-foreground"
                      )}
                    >
                      <FileText size={12} />
                      DefaultEngine.ini
                      {engineDirty && (
                        <span className="size-1.5 rounded-full bg-[var(--color-warn)]" />
                      )}
                    </button>
                  )}
                </div>
              ) : (
                <span className="flex items-center gap-1.5 text-[12px] font-medium text-muted-foreground">
                  <FileText size={12} />
                  {selectedPak.has_device_profiles
                    ? "DefaultDeviceProfiles.ini"
                    : "DefaultEngine.ini"}
                  {isDirty && <span className="size-1.5 rounded-full bg-[var(--color-warn)]" />}
                </span>
              )}
            </div>

            <div className="flex items-center gap-1">
              <Button
                variant="ghost"
                size="sm"
                onClick={() => (searchOpen ? setSearchOpen(false) : openSearch())}
                title="Search & Replace (Ctrl+F)"
                className={cn(searchOpen && "bg-secondary")}
              >
                <Search size={13} />
              </Button>
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
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={() => setCaseSensitive((p) => !p)}
                  title="Match Case"
                  className={cn(caseSensitive && "bg-secondary text-foreground")}
                >
                  <CaseSensitive size={14} />
                </Button>
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={findPrev}
                  disabled={matchPositions.length === 0}
                  title="Previous Match"
                >
                  <ChevronUp size={13} />
                </Button>
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={findNext}
                  disabled={matchPositions.length === 0}
                  title="Next Match"
                >
                  <ChevronDown size={13} />
                </Button>
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={replaceOne}
                  disabled={matchPositions.length === 0}
                  title="Replace"
                >
                  <Replace size={13} />
                </Button>
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={replaceAllMatches}
                  disabled={matchPositions.length === 0}
                  title="Replace All"
                >
                  <ReplaceAll size={13} />
                </Button>
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={() => setSearchOpen(false)}
                  title="Close"
                >
                  <X size={13} />
                </Button>
              </div>
            </div>
          )}

          {/* CodeMirror editor */}
          <div ref={editorContainerRef} className="flex-1 min-h-0 w-full overflow-hidden" />

          {/* Footer bar */}
          <div className="flex items-center justify-between border-t border-border px-3 py-1.5">
            <div className="flex items-center gap-2">
              {isDirty && (
                <span className="flex items-center gap-1.5 text-[11px] font-medium text-[var(--color-warn)]">
                  Unsaved changes
                  {dpDirty && engineDirty
                    ? " (both files)"
                    : dpDirty
                      ? " (DeviceProfiles)"
                      : " (Engine)"}
                </span>
              )}
            </div>
            <div className="flex items-center gap-2">
              <Button variant="ghost" size="sm" onClick={reload} disabled={saving}>
                <RefreshCw size={13} />
                Reload
              </Button>
              <Button variant="green" size="sm" onClick={save} disabled={!isDirty || saving}>
                {saving ? <RefreshCw size={13} className="animate-spin" /> : <Save size={13} />}
                {saving ? "Repacking..." : "Save & Repack"}
              </Button>
            </div>
          </div>
        </Card>
      )}

      {/* Loading state */}
      {loading && (
        <Card className="flex flex-1 min-h-0 items-center justify-center bg-card">
          <RefreshCw size={20} className="animate-spin text-muted-foreground" />
        </Card>
      )}

      {/* Empty state */}
      {!selectedPak && !loading && (
        <Card className="flex flex-1 min-h-0 items-center justify-center bg-card">
          <span className="text-[13px] text-muted-foreground">
            Select a config mod pak to start editing
          </span>
        </Card>
      )}
    </div>
  );
}
