import * as React from "react";
import { useState, useEffect, useRef, useCallback, useMemo } from "react";

import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { save } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import {
  AlertTriangle,
  Archive,
  FolderOpen,
  RefreshCw,
  Shield,
  CheckCircle2,
  XCircle,
  Trash2,
  UploadCloud,
  Power,
  PowerOff,
  Copy,
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
import { Checkbox } from "@/components/ui/checkbox";
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuSeparator,
  ContextMenuTrigger,
} from "@/components/ui/context-menu";
import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";

interface ModEntry {
  full_name: string;
  display_name: string;
  enabled: boolean;
  has_companions: boolean;
  size_bytes: number;
  kind: "Pak" | "IoStore";
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const kb = bytes / 1024;
  if (kb < 1024) return `${kb.toFixed(kb < 10 ? 1 : 0)} KB`;
  const mb = kb / 1024;
  if (mb < 1024) return `${mb.toFixed(mb < 10 ? 1 : 0)} MB`;
  const gb = mb / 1024;
  return `${gb.toFixed(gb < 10 ? 2 : 1)} GB`;
}

interface ModsStatus {
  mods_folder_exists: boolean;
  mods_folder_path: string;
  sig_bypass_installed: boolean;
  mod_entries: ModEntry[];
  conflicts_resolved: number;
}

interface BulkOpResult {
  successes: number;
  failures: { full_name: string; error: string }[];
}

interface Props {
  gamePath: string;
  isActive: boolean;
  gameRunning: boolean;
  pathLoading?: boolean;
}

type StatusType = "ok" | "err" | "info";

export function Mods({ gamePath, isActive, gameRunning, pathLoading }: Props) {
  const [modsStatus, setModsStatus] = useState<ModsStatus | null>(null);
  const [notice, setNotice] = useState<{
    msg: string;
    type: StatusType;
    revealPath?: string;
  } | null>(null);
  const [busyMods, setBusyMods] = useState<Set<string>>(new Set());
  const [isExporting, setIsExporting] = useState(false);
  const [pendingDelete, setPendingDelete] = useState<string | null>(null);
  const [isDragging, setIsDragging] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [bulkBusy, setBulkBusy] = useState(false);
  const [bulkDeleteOpen, setBulkDeleteOpen] = useState(false);
  const lastClickedIndex = useRef<number | null>(null);
  const listRef = useRef<HTMLUListElement>(null);
  const noticeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const modsStatusRef = useRef(modsStatus);
  const isActiveRef = useRef(isActive);
  const gameRunningRef = useRef(gameRunning);
  gameRunningRef.current = gameRunning;
  const refreshRef = useRef<typeof refresh>(null!);
  const dropProcessingRef = useRef(false);
  const outerRef = useRef<HTMLDivElement>(null);

  const showNotice = useCallback(
    (msg: string, type: StatusType, duration: number = 6000, opts?: { revealPath?: string }) => {
      if (noticeTimer.current) clearTimeout(noticeTimer.current);
      setNotice({ msg, type, revealPath: opts?.revealPath });
      noticeTimer.current = setTimeout(() => setNotice(null), duration);
    },
    []
  );

  modsStatusRef.current = modsStatus;
  isActiveRef.current = isActive;

  const refresh = useCallback(
    async (silent = false) => {
      if (!gamePath) return;
      try {
        const s = await invoke<ModsStatus>("get_mods_status", { gameRoot: gamePath });
        setModsStatus(s);
        if (!silent) showNotice("Status refreshed", "ok", 4000);
        else if (s.conflicts_resolved > 0)
          showNotice(
            `Removed ${s.conflicts_resolved} outdated disabled mod${s.conflicts_resolved !== 1 ? "s" : ""} (replaced by enabled version)`,
            "info"
          );
      } catch (e: unknown) {
        showNotice(String(e), "err");
      }
    },
    [gamePath, showNotice]
  );
  refreshRef.current = refresh;

  useEffect(() => {
    if (gamePath) refresh(true);
  }, [gamePath, refresh]);

  // Drop selected entries that no longer exist after a refresh.
  useEffect(() => {
    if (!modsStatus) return;
    setSelected((prev) => {
      if (prev.size === 0) return prev;
      const alive = new Set(modsStatus.mod_entries.map((e) => e.full_name));
      const next = new Set<string>();
      for (const name of prev) if (alive.has(name)) next.add(name);
      return next.size === prev.size ? prev : next;
    });
  }, [modsStatus]);

  // Drag-and-drop: accept .pak and .zip files dropped anywhere on the window.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    getCurrentWindow()
      .onDragDropEvent(async (event) => {
        if (event.payload.type === "enter") {
          if (
            isActiveRef.current &&
            modsStatusRef.current?.mods_folder_exists &&
            !gameRunningRef.current
          )
            setIsDragging(true);
        } else if (event.payload.type === "drop") {
          setIsDragging(false);
          if (!isActiveRef.current || dropProcessingRef.current) return;
          if (gameRunningRef.current) {
            showNotice("Close Marvel Rivals before installing mods.", "err");
            return;
          }
          const folder = modsStatusRef.current?.mods_folder_path;
          if (!folder) return;
          const pakPaths = event.payload.paths.filter((p) => p.endsWith(".pak"));
          const archivePaths = event.payload.paths.filter((p) => {
            const lower = p.toLowerCase();
            return lower.endsWith(".zip") || lower.endsWith(".7z");
          });
          if (pakPaths.length === 0 && archivePaths.length === 0) return;
          dropProcessingRef.current = true;
          try {
            let installed = 0;
            let replacedDisabled = 0;
            let replacedEnabled = 0;
            const errors: string[] = [];

            // Install loose .pak files
            for (const p of pakPaths) {
              try {
                const result = await invoke<{
                  file_name: string;
                  replaced_disabled: boolean;
                  replaced_enabled: boolean;
                }>("install_mod", { modsFolder: folder, sourcePath: p });
                installed++;
                if (result.replaced_disabled) replacedDisabled++;
                if (result.replaced_enabled) replacedEnabled++;
              } catch (e: unknown) {
                errors.push(String(e));
              }
            }

            // Install mods from .zip / .7z archives
            for (const z of archivePaths) {
              try {
                const results = await invoke<
                  { file_name: string; replaced_disabled: boolean; replaced_enabled: boolean }[]
                >("install_from_archive", { modsFolder: folder, archivePath: z });
                for (const result of results) {
                  installed++;
                  if (result.replaced_disabled) replacedDisabled++;
                  if (result.replaced_enabled) replacedEnabled++;
                }
              } catch (e: unknown) {
                errors.push(String(e));
              }
            }

            if (errors.length > 0) showNotice(errors[0], "err");
            else if (installed > 0) {
              await refreshRef.current(true);
              const n = (c: number, s: string) => `${c} ${s}${c !== 1 ? "s" : ""}`;
              const parts: string[] = [];
              if (replacedEnabled > 0) parts.push(`updated ${n(replacedEnabled, "existing mod")}`);
              if (replacedDisabled > 0)
                parts.push(`replaced ${n(replacedDisabled, "disabled version")}`);
              const suffix = parts.length > 0 ? ` (${parts.join(", ")})` : "";
              showNotice(`Installed ${n(installed, "mod")}${suffix}`, "ok");
            }
          } finally {
            dropProcessingRef.current = false;
          }
        } else if (event.payload.type === "leave") {
          setIsDragging(false);
        }
      })
      .then((fn) => {
        unlisten = fn;
      });
    return () => unlisten?.();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  async function installBypass() {
    if (!gamePath) return showNotice("Set game root in Settings first.", "err");
    try {
      const msg = await invoke<string>("install_signature_bypass", { gameRoot: gamePath });
      showNotice(msg, "ok", 4000);
      await refresh(true);
    } catch (e: unknown) {
      showNotice(String(e), "err");
    }
  }

  async function openFolder() {
    if (!gamePath) return;
    try {
      await invoke("open_mods_folder", { gameRoot: gamePath });
    } catch (e: unknown) {
      showNotice(String(e), "err");
    }
  }

  async function deleteMod(entry: ModEntry) {
    if (!modsStatus) return;
    if (pendingDelete !== entry.full_name) {
      setPendingDelete(entry.full_name);
      return;
    }
    setPendingDelete(null);
    setBusyMods((prev) => new Set(prev).add(entry.full_name));
    try {
      await invoke("delete_mod", {
        modsFolder: modsStatus.mods_folder_path,
        fullName: entry.full_name,
      });
      await refresh(true);
    } catch (e: unknown) {
      showNotice(String(e), "err");
    } finally {
      setBusyMods((prev) => {
        const next = new Set(prev);
        next.delete(entry.full_name);
        return next;
      });
    }
  }

  async function toggleMod(entry: ModEntry) {
    if (!modsStatus) return;
    setBusyMods((prev) => new Set(prev).add(entry.full_name));
    try {
      await invoke("toggle_mod_enabled", {
        modsFolder: modsStatus.mods_folder_path,
        fullName: entry.full_name,
        enabled: !entry.enabled,
      });
      await refresh(true);
    } catch (e: unknown) {
      showNotice(String(e), "err");
    } finally {
      setBusyMods((prev) => {
        const next = new Set(prev);
        next.delete(entry.full_name);
        return next;
      });
    }
  }

  function handleRowClick(index: number, fullName: string, e: React.MouseEvent) {
    const entries = modsStatus?.mod_entries ?? [];
    setSelected((prev) => {
      const next = new Set(prev);
      if (e.shiftKey && lastClickedIndex.current !== null) {
        const start = Math.min(lastClickedIndex.current, index);
        const end = Math.max(lastClickedIndex.current, index);
        for (let i = start; i <= end; i++) next.add(entries[i].full_name);
      } else if (e.ctrlKey || e.metaKey) {
        if (next.has(fullName)) next.delete(fullName);
        else next.add(fullName);
      } else {
        if (next.size === 1 && next.has(fullName)) {
          next.clear();
        } else {
          next.clear();
          next.add(fullName);
        }
      }
      return next;
    });
    lastClickedIndex.current = index;
  }

  function clearSelection() {
    setSelected(new Set());
    lastClickedIndex.current = null;
  }

  function selectAll() {
    if (!modsStatus) return;
    setSelected(new Set(modsStatus.mod_entries.map((e) => e.full_name)));
  }

  async function bulkToggle(enabled: boolean) {
    if (!modsStatus || selected.size === 0 || bulkBusy) return;
    // Skip mods already in the target state
    const targets = modsStatus.mod_entries
      .filter((e) => selected.has(e.full_name) && e.enabled !== enabled)
      .map((e) => e.full_name);
    if (targets.length === 0) return;
    setBulkBusy(true);
    setBusyMods((prev) => {
      const next = new Set(prev);
      for (const n of targets) next.add(n);
      return next;
    });
    try {
      const res = await invoke<BulkOpResult>("toggle_mods_enabled", {
        modsFolder: modsStatus.mods_folder_path,
        fullNames: targets,
        enabled,
      });
      await refresh(true);
      const verb = enabled ? "Enabled" : "Disabled";
      if (res.failures.length === 0) {
        showNotice(`${verb} ${res.successes} mod${res.successes !== 1 ? "s" : ""}`, "ok");
      } else {
        showNotice(
          `${verb} ${res.successes}, ${res.failures.length} failed: ${res.failures[0].error}`,
          "err"
        );
      }
    } catch (e: unknown) {
      showNotice(String(e), "err");
    } finally {
      setBulkBusy(false);
      setBusyMods((prev) => {
        const next = new Set(prev);
        for (const n of targets) next.delete(n);
        return next;
      });
    }
  }

  async function bulkDelete() {
    if (!modsStatus || selected.size === 0 || bulkBusy) return;
    const targets = Array.from(selected);
    setBulkDeleteOpen(false);
    setBulkBusy(true);
    setBusyMods((prev) => {
      const next = new Set(prev);
      for (const n of targets) next.add(n);
      return next;
    });
    try {
      const res = await invoke<BulkOpResult>("delete_mods", {
        modsFolder: modsStatus.mods_folder_path,
        fullNames: targets,
      });
      clearSelection();
      await refresh(true);
      if (res.failures.length === 0) {
        showNotice(`Deleted ${res.successes} mod${res.successes !== 1 ? "s" : ""}`, "ok");
      } else {
        showNotice(
          `Deleted ${res.successes}, ${res.failures.length} failed: ${res.failures[0].error}`,
          "err"
        );
      }
    } catch (e: unknown) {
      showNotice(String(e), "err");
    } finally {
      setBulkBusy(false);
      setBusyMods((prev) => {
        const next = new Set(prev);
        for (const n of targets) next.delete(n);
        return next;
      });
    }
  }

  async function disableAllOthers(keep: ModEntry) {
    if (!modsStatus) return;
    const targets = modsStatus.mod_entries
      .filter((e) => e.enabled && e.full_name !== keep.full_name)
      .map((e) => e.full_name);
    if (targets.length === 0) return;
    try {
      const res = await invoke<BulkOpResult>("toggle_mods_enabled", {
        modsFolder: modsStatus.mods_folder_path,
        fullNames: targets,
        enabled: false,
      });
      await refresh(true);
      if (res.failures.length === 0) {
        showNotice(`Disabled ${res.successes} other mod${res.successes !== 1 ? "s" : ""}`, "ok");
      } else {
        showNotice(`${res.failures.length} failed: ${res.failures[0].error}`, "err");
      }
    } catch (e: unknown) {
      showNotice(String(e), "err");
    }
  }

  async function revealMod(entry: ModEntry) {
    if (!modsStatus) return;
    try {
      const sep = modsStatus.mods_folder_path.includes("\\") ? "\\" : "/";
      await revealItemInDir(`${modsStatus.mods_folder_path}${sep}${entry.full_name}`);
    } catch (e: unknown) {
      showNotice(String(e), "err");
    }
  }

  async function copyPath(entry: ModEntry) {
    if (!modsStatus) return;
    const sep = modsStatus.mods_folder_path.includes("\\") ? "\\" : "/";
    const fullPath = `${modsStatus.mods_folder_path}${sep}${entry.full_name}`;
    try {
      await navigator.clipboard.writeText(fullPath);
      showNotice("Copied path", "ok", 2000);
    } catch (e: unknown) {
      showNotice(String(e), "err");
    }
  }

  // Keyboard shortcuts: Delete, Ctrl/Cmd+A, Escape. Only when tab is active and
  // focus is inside the list (or nothing is focused in an input/textarea).
  useEffect(() => {
    if (!isActive) return;
    function isTypingTarget(el: EventTarget | null): boolean {
      if (!(el instanceof HTMLElement)) return false;
      const tag = el.tagName;
      return tag === "INPUT" || tag === "TEXTAREA" || el.isContentEditable;
    }
    function onKeyDown(e: KeyboardEvent) {
      if (isTypingTarget(e.target)) return;
      if (bulkDeleteOpen) return;
      if ((e.key === "a" || e.key === "A") && (e.ctrlKey || e.metaKey)) {
        if (!modsStatusRef.current || modsStatusRef.current.mod_entries.length === 0) return;
        e.preventDefault();
        selectAll();
        return;
      }
      if (e.key === "Escape" && selected.size > 0) {
        e.preventDefault();
        clearSelection();
        return;
      }
      if ((e.key === "Delete" || e.key === "Backspace") && selected.size > 0 && !bulkBusy) {
        e.preventDefault();
        setBulkDeleteOpen(true);
      }
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isActive, selected, bulkBusy, bulkDeleteOpen]);

  async function exportZip() {
    if (!modsStatus || isExporting) return;
    const destPath = await save({
      defaultPath: "mods.zip",
      filters: [
        { name: "Zip Archive", extensions: ["zip"] },
        { name: "7z Archive", extensions: ["7z"] },
      ],
    });
    if (!destPath) return;
    setIsExporting(true);
    try {
      const msg = await invoke<string>("export_mods_archive", {
        modsFolder: modsStatus.mods_folder_path,
        destPath,
      });
      const fileName = destPath.split(/[\\/]/).pop() ?? destPath;
      showNotice(`${msg}: ${fileName}`, "ok", 8000, { revealPath: destPath });
    } catch (e: unknown) {
      showNotice(String(e), "err");
    } finally {
      setIsExporting(false);
    }
  }

  const enabledCount = modsStatus?.mod_entries.filter((m) => m.enabled).length ?? 0;
  const totalCount = modsStatus?.mod_entries.length ?? 0;

  const selectedEntries = useMemo(() => {
    if (!modsStatus || selected.size === 0) return [] as ModEntry[];
    return modsStatus.mod_entries.filter((e) => selected.has(e.full_name));
  }, [modsStatus, selected]);
  const allEnabledInSelection =
    selectedEntries.length > 0 && selectedEntries.every((e) => e.enabled);
  const allDisabledInSelection =
    selectedEntries.length > 0 && selectedEntries.every((e) => !e.enabled);
  const selectAllState: boolean | "indeterminate" =
    totalCount > 0 && selected.size === totalCount
      ? true
      : selected.size > 0
        ? "indeterminate"
        : false;

  return (
    <div ref={outerRef} className="relative flex flex-1 min-h-0 w-full flex-col gap-3">
      {isDragging && (
        <div className="pointer-events-none absolute inset-0 z-50 flex flex-col items-center justify-center gap-3 rounded-lg border-2 border-dashed border-ok bg-background/80 backdrop-blur-sm">
          <UploadCloud size={36} className="text-ok" />
          <span className="text-sm font-semibold text-ok">Drop .pak, .zip or .7z to install</span>
        </div>
      )}
      {/* Header */}
      <div className="flex min-h-8 items-center gap-3">
        <h2 className="shrink-0 text-xl font-bold">Mods</h2>
        {notice && (
          <span
            className={cn(
              "flex min-w-0 items-center gap-1.5 truncate text-[12px] font-medium",
              notice.type === "ok"
                ? "text-ok"
                : notice.type === "err"
                  ? "text-err"
                  : "text-muted-foreground",
              notice.revealPath && "cursor-pointer hover:underline"
            )}
            onClick={notice.revealPath ? () => revealItemInDir(notice.revealPath!) : undefined}
            title={notice.revealPath ? "Click to reveal in explorer" : undefined}
          >
            {notice.type === "ok" ? (
              <CheckCircle2 className="shrink-0" size={14} strokeWidth={2.5} />
            ) : notice.type === "err" ? (
              <XCircle className="shrink-0" size={14} strokeWidth={2.5} />
            ) : null}
            <span className="truncate">{notice.msg}</span>
          </span>
        )}
        {!gamePath && !pathLoading && (
          <span className="flex shrink-0 items-center gap-1.5 text-[12px] font-medium text-warn">
            <XCircle size={14} strokeWidth={2.5} />
            Set game root in Settings first
          </span>
        )}
      </div>

      {/* Game-running banner */}
      {gameRunning && (
        <div className="flex items-center gap-2.5 rounded-md border border-warn/20 bg-warn/5 px-3 py-2">
          <AlertTriangle size={15} className="shrink-0 text-warn" />
          <span className="flex-1 text-[12px] text-warn">
            Marvel Rivals is running — mod operations are disabled. Close the game to modify mods.
          </span>
        </div>
      )}

      {/* Bypass banner — only if not installed */}
      {modsStatus && !modsStatus.sig_bypass_installed && (
        <div className="flex items-center gap-2.5 rounded-md border border-warn/20 bg-warn/5 px-3 py-2">
          <Shield size={15} className="shrink-0 text-warn" />
          <span className="flex-1 text-[12px] text-warn">
            Signature bypass required for mods to load
          </span>
          <Button variant="green" size="xs" onClick={installBypass} disabled={!gamePath}>
            Install Bypass
          </Button>
        </div>
      )}

      {/* Unified mod list panel */}
      <div className="flex min-h-0 flex-1 flex-col overflow-hidden rounded-md border border-border">
        {/* Toolbar */}
        <div className="flex h-12 items-center justify-between gap-2 border-b border-border bg-card px-3">
          {selected.size > 0 ? (
            <>
              <div className="flex items-center gap-2">
                <Checkbox
                  checked={selectAllState}
                  onCheckedChange={(v) => (v ? selectAll() : clearSelection())}
                  aria-label="Select all mods"
                />
                <span className="text-sm font-semibold">{selected.size} selected</span>
              </div>
              <div className="flex items-center gap-1 -mr-1">
                <Button
                  variant="ghost"
                  size="xs"
                  onClick={() => bulkToggle(true)}
                  disabled={bulkBusy || allEnabledInSelection || gameRunning}
                  className="text-ok hover:text-ok hover:bg-ok/10"
                  title={gameRunning ? "Close the game to modify mods" : "Enable selected mods"}
                >
                  <Power size={13} />
                  Enable
                </Button>
                <Button
                  variant="ghost"
                  size="xs"
                  onClick={() => bulkToggle(false)}
                  disabled={bulkBusy || allDisabledInSelection || gameRunning}
                  title={gameRunning ? "Close the game to modify mods" : "Disable selected mods"}
                >
                  <PowerOff size={13} />
                  Disable
                </Button>
                <Button
                  variant="ghost"
                  size="xs"
                  onClick={() => setBulkDeleteOpen(true)}
                  disabled={bulkBusy || gameRunning}
                  className="text-err hover:text-err hover:bg-err/10"
                  title={gameRunning ? "Close the game to modify mods" : "Delete selected mods"}
                >
                  <Trash2 size={13} />
                  Delete
                </Button>
                <Button
                  variant="ghost"
                  size="icon-sm"
                  onClick={clearSelection}
                  title="Clear selection (Esc)"
                >
                  <X size={15} />
                </Button>
              </div>
            </>
          ) : (
            <>
              <div className="flex items-center gap-2">
                {totalCount > 0 && (
                  <Checkbox
                    checked={selectAllState}
                    onCheckedChange={(v) => (v ? selectAll() : clearSelection())}
                    aria-label="Select all mods"
                  />
                )}
                <h3 className="text-sm font-semibold">Installed Mods</h3>
                {modsStatus && (
                  <span className="text-[12px] text-muted-foreground">
                    ({enabledCount}/{totalCount} active)
                  </span>
                )}
              </div>
              <div className="flex items-center -mr-1">
                <Button
                  variant="ghost"
                  size="icon-sm"
                  onClick={openFolder}
                  disabled={!gamePath}
                  title="Open ~mods folder"
                >
                  <FolderOpen size={15} />
                </Button>
                <Button
                  variant="ghost"
                  size="icon-sm"
                  onClick={exportZip}
                  disabled={enabledCount === 0 || isExporting}
                  title={
                    enabledCount === 0
                      ? "No enabled mods to export"
                      : isExporting
                        ? "Exporting…"
                        : "Export enabled mods as .zip or .7z"
                  }
                >
                  <Archive size={15} />
                </Button>
                <Button
                  variant="ghost"
                  size="icon-sm"
                  onClick={() => refresh()}
                  disabled={!gamePath}
                  title="Refresh"
                >
                  <RefreshCw size={15} />
                </Button>
              </div>
            </>
          )}
        </div>

        {/* Scrollable list */}
        <div className="min-h-0 flex-1 overflow-y-auto bg-background">
          {pathLoading ? null : !modsStatus || totalCount === 0 ? (
            <div className="flex h-full flex-col items-center justify-center py-12">
              <div className="flex w-full max-w-md flex-col gap-4">
                <p className="text-sm text-muted-foreground">
                  {!modsStatus
                    ? "Loading…"
                    : "No mods installed yet. Follow the steps below to get started."}
                </p>
                {modsStatus && totalCount === 0 && (
                  <ol className="flex flex-col gap-3 text-[12px] text-muted-foreground">
                    {[
                      <>
                        Install the <strong className="text-foreground">Signature Bypass</strong> so
                        the game loads modded pak files.
                      </>,
                      <>
                        Drag and drop <Code>.pak</Code> or <Code>.zip</Code> mod files onto this
                        window to install them.
                      </>,
                      <>Launch Marvel Rivals, your mods will be active.</>,
                    ].map((step, i) => (
                      <li key={i} className="flex gap-2.5">
                        <span className="flex size-5 shrink-0 items-center justify-center rounded-full bg-secondary text-[10px] font-bold text-foreground">
                          {i + 1}
                        </span>
                        <span className="pt-0.5 leading-relaxed">{step}</span>
                      </li>
                    ))}
                  </ol>
                )}
              </div>
            </div>
          ) : (
            <ul ref={listRef}>
              {modsStatus.mod_entries.map((entry, index) => {
                const busy = busyMods.has(entry.full_name);
                const isSelected = selected.has(entry.full_name);
                return (
                  <ContextMenu key={entry.full_name}>
                    <ContextMenuTrigger asChild>
                      <li
                        onClick={(e) => handleRowClick(index, entry.full_name, e)}
                        onContextMenu={() => {
                          if (!selected.has(entry.full_name)) {
                            setSelected(new Set([entry.full_name]));
                            lastClickedIndex.current = index;
                          }
                        }}
                        className={cn(
                          "relative flex h-12 cursor-default items-center gap-3 border-b border-border/50 px-3 last:border-none select-none",
                          isSelected ? "bg-primary/10 hover:bg-primary/15" : "hover:bg-secondary/40"
                        )}
                      >
                        {isSelected && (
                          <span className="absolute inset-y-0 left-0 w-0.5 bg-primary" />
                        )}
                        <Checkbox
                          checked={isSelected}
                          onClick={(e) => e.stopPropagation()}
                          onCheckedChange={(v) =>
                            setSelected((prev) => {
                              const next = new Set(prev);
                              if (v) next.add(entry.full_name);
                              else next.delete(entry.full_name);
                              return next;
                            })
                          }
                          aria-label={`Select ${entry.display_name}`}
                        />
                        <span
                          className={cn(
                            "flex min-w-0 flex-1 items-center gap-2 text-[13px]",
                            !entry.enabled && "opacity-40"
                          )}
                        >
                          <span className="min-w-0 flex-1 truncate">
                            <ModName displayName={entry.display_name} />
                          </span>
                          <span className="shrink-0 font-mono text-[11px] text-muted-foreground/60 tabular-nums">
                            {formatBytes(entry.size_bytes)}
                          </span>
                        </span>
                        {pendingDelete === entry.full_name ? (
                          <div
                            className="flex shrink-0 items-center gap-1"
                            onClick={(e) => e.stopPropagation()}
                          >
                            <span className="text-[11px] font-medium text-err">Delete?</span>
                            <button
                              title="Confirm delete"
                              disabled={busy}
                              onClick={() => deleteMod(entry)}
                              className="rounded px-1.5 text-[11px] font-semibold bg-err text-white hover:opacity-90 transition-opacity leading-5.5"
                            >
                              Yes
                            </button>
                            <button
                              title="Cancel"
                              onClick={() => setPendingDelete(null)}
                              className="rounded px-1.5 text-[11px] font-semibold border border-border text-muted-foreground hover:bg-secondary transition-colors leading-5.5"
                            >
                              No
                            </button>
                          </div>
                        ) : (
                          <>
                            <Switch
                              checked={entry.enabled}
                              disabled={busy || gameRunning}
                              onClick={(e) => e.stopPropagation()}
                              onCheckedChange={() => toggleMod(entry)}
                              className="shrink-0"
                            />
                            <button
                              title={gameRunning ? "Close the game to modify mods" : "Delete mod"}
                              disabled={busy || gameRunning}
                              onClick={(e) => {
                                e.stopPropagation();
                                deleteMod(entry);
                              }}
                              className="shrink-0 rounded p-1 text-err/70 transition-colors hover:text-err hover:bg-err/10 disabled:opacity-40 disabled:hover:bg-transparent"
                            >
                              <Trash2 size={13} />
                            </button>
                          </>
                        )}
                      </li>
                    </ContextMenuTrigger>
                    <ContextMenuContent>
                      <ContextMenuItem
                        disabled={busy || bulkBusy || gameRunning}
                        onSelect={() => toggleMod(entry)}
                      >
                        {entry.enabled ? <PowerOff /> : <Power />}
                        {entry.enabled ? "Disable" : "Enable"}
                      </ContextMenuItem>
                      <ContextMenuItem
                        disabled={busy || bulkBusy || gameRunning}
                        onSelect={() => disableAllOthers(entry)}
                      >
                        <PowerOff />
                        Disable all others
                      </ContextMenuItem>
                      <ContextMenuSeparator />
                      <ContextMenuItem onSelect={() => revealMod(entry)}>
                        <FolderOpen />
                        Show in folder
                      </ContextMenuItem>
                      <ContextMenuItem onSelect={() => copyPath(entry)}>
                        <Copy />
                        Copy path
                      </ContextMenuItem>
                      <ContextMenuSeparator />
                      <ContextMenuItem
                        destructive
                        disabled={busy || bulkBusy || gameRunning}
                        onSelect={() => {
                          if (selected.size > 1 && selected.has(entry.full_name)) {
                            setBulkDeleteOpen(true);
                          } else {
                            setPendingDelete(entry.full_name);
                          }
                        }}
                      >
                        <Trash2 />
                        Delete
                      </ContextMenuItem>
                    </ContextMenuContent>
                  </ContextMenu>
                );
              })}
            </ul>
          )}
        </div>
      </div>

      <AlertDialog open={bulkDeleteOpen} onOpenChange={setBulkDeleteOpen}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              Delete {selected.size} mod{selected.size !== 1 ? "s" : ""}?
            </AlertDialogTitle>
            <AlertDialogDescription>
              This permanently removes the selected .pak files and any .ucas/.utoc companions. This
              cannot be undone.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              className={cn("bg-err text-white hover:bg-err/90 focus-visible:ring-err/40")}
              onClick={bulkDelete}
            >
              Delete {selected.size}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  );
}

function ModName({ displayName }: { displayName: string }) {
  // Strip ".pak" extension and optional "_NNNNN_P" UE pak suffix for cleaner display.
  const match = displayName.match(/^(.+?)(_\d+_P)?\.pak$/);
  if (!match) return <span className="font-mono">{displayName}</span>;
  const [, base, suffix] = match;
  return (
    <>
      <span className="font-semibold">{base}</span>
      {suffix && <span className="font-mono text-muted-foreground/40">{suffix}</span>}
    </>
  );
}

function Code({ children }: { children: React.ReactNode }) {
  return (
    <code className="rounded bg-muted px-1 py-0.5 font-mono text-[11px] text-foreground">
      {children}
    </code>
  );
}
