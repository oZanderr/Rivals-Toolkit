import { useState, useEffect, useRef, useCallback } from "react";

import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import {
  CheckCircle2,
  XCircle,
  RefreshCw,
  Save,
  Search,
  FolderOpen,
  Replace,
  X,
  FileText,
  ChevronDown,
  ChevronUp,
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

  // ── Search/replace ──
  const [searchOpen, setSearchOpen] = useState(false);
  const [searchTerm, setSearchTerm] = useState("");
  const [replaceTerm, setReplaceTerm] = useState("");
  const [matchCount, setMatchCount] = useState(0);
  const [currentMatch, setCurrentMatch] = useState(-1);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const searchInputRef = useRef<HTMLInputElement>(null);
  const replaceInputRef = useRef<HTMLInputElement>(null);

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
    if (isDirty && !confirm("You have unsaved changes. Discard and switch paks?")) return;

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

      setDpContent(dp);
      setSavedDp(dp);
      setEngineContent(eng);
      setSavedEngine(eng);

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
    if (isDirty && !confirm("Discard unsaved changes and reload from disk?")) return;
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

  // ── Search/replace logic ──
  useEffect(() => {
    if (!searchTerm || !currentContent) {
      setMatchCount(0);
      setCurrentMatch(-1);
      return;
    }
    const term = searchTerm.toLowerCase();
    const content = currentContent.toLowerCase();
    let count = 0;
    let idx = content.indexOf(term);
    while (idx !== -1) {
      count++;
      idx = content.indexOf(term, idx + 1);
    }
    setMatchCount(count);
    setCurrentMatch(count > 0 ? 0 : -1);
  }, [searchTerm, currentContent]);

  function findNext() {
    if (matchCount === 0 || !currentContent || !textareaRef.current) return;
    const term = searchTerm.toLowerCase();
    const content = currentContent.toLowerCase();

    // Find the Nth occurrence
    const nextIdx = (currentMatch + 1) % matchCount;
    let pos = -1;
    let found = 0;
    let searchFrom = 0;
    while (found <= nextIdx) {
      pos = content.indexOf(term, searchFrom);
      if (pos === -1) break;
      if (found === nextIdx) break;
      found++;
      searchFrom = pos + 1;
    }

    if (pos !== -1) {
      textareaRef.current.focus();
      textareaRef.current.setSelectionRange(pos, pos + searchTerm.length);
      setCurrentMatch(nextIdx);
    }
  }

  function findPrev() {
    if (matchCount === 0 || !currentContent || !textareaRef.current) return;
    const term = searchTerm.toLowerCase();
    const content = currentContent.toLowerCase();

    const prevIdx = (currentMatch - 1 + matchCount) % matchCount;
    let pos = -1;
    let found = 0;
    let searchFrom = 0;
    while (found <= prevIdx) {
      pos = content.indexOf(term, searchFrom);
      if (pos === -1) break;
      if (found === prevIdx) break;
      found++;
      searchFrom = pos + 1;
    }

    if (pos !== -1) {
      textareaRef.current.focus();
      textareaRef.current.setSelectionRange(pos, pos + searchTerm.length);
      setCurrentMatch(prevIdx);
    }
  }

  function replaceOne() {
    if (matchCount === 0 || !currentContent || !textareaRef.current) return;
    const start = textareaRef.current.selectionStart;
    const end = textareaRef.current.selectionEnd;
    const selected = currentContent.substring(start, end);

    if (selected.toLowerCase() === searchTerm.toLowerCase()) {
      const newContent =
        currentContent.substring(0, start) + replaceTerm + currentContent.substring(end);
      setCurrentContent(newContent);
    }
    // Auto-advance to next match
    setTimeout(findNext, 0);
  }

  function replaceAll() {
    if (!searchTerm || !currentContent) return;
    // Case-insensitive replace all
    const regex = new RegExp(searchTerm.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"), "gi");
    const newContent = currentContent.replace(regex, replaceTerm);
    setCurrentContent(newContent);
    showNotice(`Replaced ${matchCount} occurrence${matchCount !== 1 ? "s" : ""}`, "ok");
  }

  function handleKeyDown(e: React.KeyboardEvent<HTMLTextAreaElement>) {
    // Tab inserts two spaces
    if (e.key === "Tab") {
      e.preventDefault();
      const ta = e.currentTarget;
      const start = ta.selectionStart;
      const end = ta.selectionEnd;
      const val = ta.value;
      const newVal = val.substring(0, start) + "  " + val.substring(end);
      setCurrentContent(newVal);
      requestAnimationFrame(() => {
        ta.selectionStart = ta.selectionEnd = start + 2;
      });
    }
    // Ctrl+F opens search bar, Ctrl+H opens search bar focused on replace
    if (e.key === "f" && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      setSearchOpen(true);
      setTimeout(() => searchInputRef.current?.focus(), 0);
    }
    if (e.key === "h" && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      setSearchOpen(true);
      setTimeout(() => replaceInputRef.current?.focus(), 0);
    }
    // Ctrl+S saves
    if (e.key === "s" && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      save();
    }
  }

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
                onClick={() => {
                  setSearchOpen((prev) => !prev);
                  setTimeout(() => searchInputRef.current?.focus(), 0);
                }}
                title="Search & Replace (Ctrl+F / Ctrl+H)"
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
                    if (e.key === "Enter") findNext();
                    if (e.key === "Escape") setSearchOpen(false);
                  }}
                  placeholder="Search..."
                  className="h-7 flex-1 text-xs"
                />
                <Input
                  ref={replaceInputRef}
                  value={replaceTerm}
                  onChange={(e) => setReplaceTerm(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") replaceOne();
                    if (e.key === "Escape") setSearchOpen(false);
                  }}
                  placeholder="Replace..."
                  className="h-7 flex-1 text-xs"
                />
              </div>
              <span className="shrink-0 text-[11px] text-muted-foreground">
                {matchCount > 0 ? `${currentMatch + 1}/${matchCount}` : "No results"}
              </span>
              <div className="flex items-center gap-0.5">
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={findPrev}
                  disabled={matchCount === 0}
                  title="Previous match"
                >
                  <ChevronUp size={13} />
                </Button>
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={findNext}
                  disabled={matchCount === 0}
                  title="Next match"
                >
                  <ChevronDown size={13} />
                </Button>
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={replaceOne}
                  disabled={matchCount === 0}
                  title="Replace"
                >
                  <Replace size={13} />
                </Button>
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={replaceAll}
                  disabled={matchCount === 0}
                  title="Replace all"
                >
                  Replace All
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

          {/* Textarea editor */}
          <textarea
            ref={textareaRef}
            className="flex-1 min-h-0 w-full resize-none bg-background p-4 font-mono text-[13px] leading-relaxed text-foreground focus:outline-none"
            value={currentContent ?? ""}
            onChange={(e) => setCurrentContent(e.target.value)}
            onKeyDown={handleKeyDown}
            spellCheck={false}
            wrap="off"
          />

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
