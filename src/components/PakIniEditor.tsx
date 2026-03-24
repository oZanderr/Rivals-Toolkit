import { useState, useEffect, useRef, useCallback } from "react";

import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import {
  search,
  openSearchPanel,
  closeSearchPanel,
  SearchQuery,
  setSearchQuery,
  findNext as cmFindNext,
  findPrevious as cmFindPrev,
  replaceNext as cmReplaceNext,
  replaceAll as cmReplaceAll,
  getSearchQuery,
} from "@codemirror/search";
import { EditorState } from "@codemirror/state";
import { EditorView, keymap, type Panel } from "@codemirror/view";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { CheckCircle2, XCircle, RefreshCw, Save, Search, FolderOpen, FileText } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
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

  // ── CodeMirror setup ──
  const saveRef = useRef(save);
  saveRef.current = save;

  function createSearchPanel(view: EditorView): Panel {
    const el = (tag: string, cls?: string) => {
      const e = document.createElement(tag);
      if (cls) e.className = cls;
      return e;
    };

    const svgIcon = (paths: string) => {
      const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
      svg.setAttribute("width", "14");
      svg.setAttribute("height", "14");
      svg.setAttribute("viewBox", "0 0 24 24");
      svg.setAttribute("fill", "none");
      svg.setAttribute("stroke", "currentColor");
      svg.setAttribute("stroke-width", "2");
      svg.setAttribute("stroke-linecap", "round");
      svg.setAttribute("stroke-linejoin", "round");
      svg.innerHTML = paths;
      return svg;
    };

    // ── State ──
    let caseSensitive = false;

    // ── DOM ──
    const dom = el("div", "cm-search-panel");

    const searchInput = document.createElement("input") as HTMLInputElement;
    searchInput.type = "text";
    searchInput.placeholder = "Search...";
    searchInput.setAttribute("main-field", "true");

    const replaceInput = document.createElement("input") as HTMLInputElement;
    replaceInput.type = "text";
    replaceInput.placeholder = "Replace...";

    const countSpan = el("span", "cm-search-count");

    const caseBtn = el("button", "cm-search-icon-btn") as HTMLButtonElement;
    caseBtn.append(
      svgIcon(
        '<path d="m2 16 4.039-9.69a.5.5 0 0 1 .923 0L11 16"/>' +
          '<path d="M22 9v7"/>' +
          '<path d="M3.304 13h6.392"/>' +
          '<circle cx="18.5" cy="12.5" r="3.5"/>'
      )
    );
    caseBtn.title = "Match Case";

    const prevBtn = el("button", "cm-search-icon-btn") as HTMLButtonElement;
    prevBtn.append(svgIcon('<path d="m18 15-6-6-6 6"/>'));
    prevBtn.title = "Previous Match";

    const nextBtn = el("button", "cm-search-icon-btn") as HTMLButtonElement;
    nextBtn.append(svgIcon('<path d="m6 9 6 6 6-6"/>'));
    nextBtn.title = "Next Match";

    const replaceBtn = el("button", "cm-search-icon-btn") as HTMLButtonElement;
    replaceBtn.append(
      svgIcon(
        '<path d="M14 4a1 1 0 0 1 1-1"/>' +
          '<path d="M15 10a1 1 0 0 1-1-1"/>' +
          '<path d="M21 4a1 1 0 0 0-1-1"/>' +
          '<path d="M21 9a1 1 0 0 1-1 1"/>' +
          '<path d="m3 7 3 3 3-3"/>' +
          '<path d="M6 10V5a2 2 0 0 1 2-2h2"/>' +
          '<rect x="3" y="14" width="7" height="7" rx="1"/>'
      )
    );
    replaceBtn.title = "Replace";

    const replaceAllBtn = el("button", "cm-search-icon-btn") as HTMLButtonElement;
    replaceAllBtn.append(
      svgIcon(
        '<path d="M14 14a1 1 0 0 1 1 1v5a1 1 0 0 1-1 1"/>' +
          '<path d="M14 4a1 1 0 0 1 1-1"/>' +
          '<path d="M15 10a1 1 0 0 1-1-1"/>' +
          '<path d="M19 14a1 1 0 0 1 1 1v5a1 1 0 0 1-1 1"/>' +
          '<path d="M21 4a1 1 0 0 0-1-1"/>' +
          '<path d="M21 9a1 1 0 0 1-1 1"/>' +
          '<path d="m3 7 3 3 3-3"/>' +
          '<path d="M6 10V5a2 2 0 0 1 2-2h2"/>' +
          '<rect x="3" y="14" width="7" height="7" rx="1"/>'
      )
    );
    replaceAllBtn.title = "Replace All";

    const closeBtn = el("button", "cm-search-close") as HTMLButtonElement;
    closeBtn.append(svgIcon('<path d="M18 6 6 18"/><path d="m6 6 12 12"/>'));
    closeBtn.title = "Close";

    const inputsWrap = el("div", "cm-search-inputs");
    inputsWrap.append(searchInput, replaceInput);
    dom.append(
      inputsWrap,
      countSpan,
      caseBtn,
      prevBtn,
      nextBtn,
      replaceBtn,
      replaceAllBtn,
      closeBtn
    );

    // ── Helpers ──
    function commit() {
      const query = new SearchQuery({
        search: searchInput.value,
        caseSensitive,
        replace: replaceInput.value,
      });
      view.dispatch({ effects: setSearchQuery.of(query) });
    }

    function updateCount() {
      const term = searchInput.value;
      if (!term) {
        countSpan.textContent = "";
        return;
      }

      const doc = view.state.doc.toString();
      const hay = caseSensitive ? doc : doc.toLowerCase();
      const ndl = caseSensitive ? term : term.toLowerCase();
      const selFrom = view.state.selection.main.from;

      let total = 0;
      let current = 0;
      let idx = hay.indexOf(ndl);
      while (idx !== -1) {
        total++;
        if (idx === selFrom) current = total;
        idx = hay.indexOf(ndl, idx + 1);
      }

      countSpan.textContent = total > 0 ? `${current || "?"}/${total}` : "No results";
    }

    // ── Events ──
    searchInput.addEventListener("input", () => {
      commit();
      // Jump to the first match without stealing focus from the input
      const term = searchInput.value;
      if (term) {
        const doc = view.state.doc.toString();
        const hay = caseSensitive ? doc : doc.toLowerCase();
        const ndl = caseSensitive ? term : term.toLowerCase();
        const idx = hay.indexOf(ndl);
        if (idx !== -1) {
          view.dispatch({
            selection: { anchor: idx, head: idx + ndl.length },
            scrollIntoView: true,
          });
        }
      }
      updateCount();
    });
    searchInput.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && e.shiftKey) {
        e.preventDefault();
        cmFindPrev(view);
        updateCount();
      } else if (e.key === "Enter") {
        e.preventDefault();
        cmFindNext(view);
        updateCount();
      } else if (e.key === "Escape") closeSearchPanel(view);
    });

    replaceInput.addEventListener("input", () => commit());
    replaceInput.addEventListener("keydown", (e) => {
      if (e.key === "Enter") {
        e.preventDefault();
        cmReplaceNext(view);
        updateCount();
      }
      if (e.key === "Escape") closeSearchPanel(view);
    });

    caseBtn.addEventListener("click", () => {
      caseSensitive = !caseSensitive;
      caseBtn.classList.toggle("cm-search-toggle-active", caseSensitive);
      commit();
      updateCount();
    });

    prevBtn.addEventListener("click", () => {
      cmFindPrev(view);
      updateCount();
    });
    nextBtn.addEventListener("click", () => {
      cmFindNext(view);
      updateCount();
    });
    replaceBtn.addEventListener("click", () => {
      cmReplaceNext(view);
      updateCount();
    });
    replaceAllBtn.addEventListener("click", () => {
      cmReplaceAll(view);
      updateCount();
    });
    closeBtn.addEventListener("click", () => closeSearchPanel(view));

    return {
      dom,
      top: true,
      mount() {
        // Sync from any existing query state (e.g. re-opening panel)
        const q = getSearchQuery(view.state);
        searchInput.value = q.search;
        replaceInput.value = q.replace;
        caseSensitive = q.caseSensitive;
        caseBtn.classList.toggle("cm-search-toggle-active", caseSensitive);
        updateCount();
        searchInput.focus();
        searchInput.select();
      },
      update(update) {
        if (update.docChanged || update.selectionSet) updateCount();
      },
    };
  }

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
      ".cm-searchMatch": {
        backgroundColor: "hsl(210 80% 60% / 0.35)",
      },
      ".cm-searchMatch-selected": {
        backgroundColor: "hsl(210 80% 60% / 0.7)",
      },
      // ── Search panel ──
      ".cm-panels": {
        backgroundColor: "var(--color-muted)",
        color: "var(--color-foreground)",
      },
      ".cm-panels-top": {
        borderBottom: "1px solid var(--color-border)",
      },
      ".cm-search-panel": {
        display: "flex",
        flexDirection: "row",
        alignItems: "center",
        gap: "6px",
        padding: "6px 12px",
        fontSize: "12px",
      },
      ".cm-search-inputs": {
        display: "flex",
        flex: "1",
        minWidth: "0",
        gap: "6px",
      },
      ".cm-search-panel input[type=text]": {
        backgroundColor: "var(--color-background)",
        color: "var(--color-foreground)",
        border: "1px solid var(--color-border)",
        borderRadius: "6px",
        fontSize: "12px",
        padding: "4px 8px",
        height: "28px",
        flex: "1",
        minWidth: "0",
        outline: "none",
        fontFamily: "inherit",
        boxSizing: "border-box",
      },
      ".cm-search-panel input[type=text]:focus": {
        borderColor: "var(--color-ring)",
        boxShadow: "0 0 0 1px var(--color-ring)",
      },
      ".cm-search-panel .cm-search-count": {
        fontSize: "11px",
        color: "var(--color-muted-foreground)",
        whiteSpace: "nowrap",
      },
      ".cm-search-panel button": {
        backgroundImage: "none",
        backgroundColor: "var(--color-secondary)",
        color: "var(--color-foreground)",
        border: "none",
        borderRadius: "6px",
        fontSize: "11px",
        padding: "4px 10px",
        height: "28px",
        cursor: "pointer",
        fontFamily: "inherit",
        transition: "background-color 120ms, color 120ms",
        whiteSpace: "nowrap",
      },
      ".cm-search-panel button:hover": {
        backgroundColor: "var(--color-accent)",
      },
      ".cm-search-panel button.cm-search-icon-btn": {
        width: "28px",
        padding: "0",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        flexShrink: "0",
      },
      ".cm-search-panel button svg, .cm-search-panel button span": {
        pointerEvents: "none",
      },
      ".cm-search-panel button.cm-search-toggle-active": {
        backgroundColor: "var(--color-accent)",
        color: "var(--color-foreground)",
      },
      ".cm-search-panel button.cm-search-close": {
        backgroundColor: "transparent",
        border: "none",
        color: "var(--color-muted-foreground)",
        fontSize: "16px",
        width: "22px",
        height: "22px",
        borderRadius: "4px",
        padding: "0",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        marginLeft: "auto",
      },
      ".cm-search-panel button.cm-search-close:hover": {
        color: "var(--color-foreground)",
        backgroundColor: "var(--color-secondary)",
      },
    });

    const saveKeymap = keymap.of([
      {
        key: "Mod-s",
        run: () => {
          saveRef.current();
          return true;
        },
      },
    ]);

    const updateListener = EditorView.updateListener.of((update) => {
      if (update.docChanged) {
        const newDoc = update.state.doc.toString();
        setCurrentContent(newDoc);
      }
    });

    const view = new EditorView({
      state: EditorState.create({
        doc: currentContent,
        extensions: [
          cmTheme,
          saveKeymap,
          keymap.of([...defaultKeymap, ...historyKeymap]),
          history(),
          search({
            top: true,
            createPanel: createSearchPanel,
          }),
          updateListener,
          EditorView.lineWrapping,
        ],
      }),
      parent: editorContainerRef.current,
    });

    editorViewRef.current = view;

    return () => {
      view.destroy();
      editorViewRef.current = null;
    };
    // Only recreate when switching files or loading new content from disk
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeFile, savedDp, savedEngine]);

  // ── Open CM search panel on Ctrl+F when this tab is active ──
  useEffect(() => {
    if (!isActive) return;
    function onKeyDown(e: KeyboardEvent) {
      if (e.key === "f" && (e.ctrlKey || e.metaKey) && editorViewRef.current) {
        openSearchPanel(editorViewRef.current);
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
    // eslint-disable-next-line react-hooks/exhaustive-deps -- fire-once after scan populates paks; loadPak identity is irrelevant here
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
                onClick={() => editorViewRef.current && openSearchPanel(editorViewRef.current)}
                title="Search & Replace (Ctrl+F)"
              >
                <Search size={13} />
              </Button>
            </div>
          </div>

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
