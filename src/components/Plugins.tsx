import * as React from "react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open } from "@tauri-apps/plugin-dialog";
import { openPath, revealItemInDir } from "@tauri-apps/plugin-opener";
import {
  AlertTriangle,
  CheckCircle2,
  Cpu,
  FileText,
  FolderOpen,
  Plus,
  Power,
  PowerOff,
  RefreshCw,
  Search,
  Shield,
  Trash2,
  UploadCloud,
  X,
  XCircle,
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
import { Tip } from "@/components/ui/tooltip";
import { emitModsChanged, normalizeFolderPath, onModsChanged } from "@/lib/modsEvents";
import { cn } from "@/lib/utils";

type LoaderKind = "minimal_asi_loader" | "ultimate_asi_loader" | "unknown";
type BypassRole = "loader" | "outdated_loader" | "payload";
type StatusType = "ok" | "err" | "info";

interface Companion {
  file_name: string;
  path: string;
  size: number;
}

interface Loader {
  file_name: string;
  path: string;
  proxy_for: string;
  enabled: boolean;
  size: number;
  modified: number | null;
  kind: LoaderKind;
  description: string | null;
  companions: Companion[];
  bypass: BypassRole | null;
}

interface Plugin {
  name: string;
  file_name: string;
  path: string;
  /** Relative to Win64, `/`-separated: empty beside the game, else such as `plugins`. */
  location: string;
  enabled: boolean;
  size: number;
  modified: number | null;
  companions: Companion[];
  shadowed_by: string | null;
  bypass: BypassRole | null;
}

interface PluginsStatus {
  win64_dir: string;
  plugins_dir: string;
  win64_exists: boolean;
  plugins_dir_exists: boolean;
  loaders: Loader[];
  plugins: Plugin[];
}

interface BulkOpResult {
  successes: number;
  failures: { full_name: string; error: string }[];
}

interface PluginInstall {
  source: string;
  installed: string[];
  kept: string[];
  existing: string[];
  skipped: { file: string; reason: string }[];
  error: string | null;
}

type Entry = Loader | Plugin;

interface Props {
  gamePath: string;
  isActive: boolean;
  gameRunning: boolean;
  pathLoading?: boolean;
  onOpenInEditor?: (path: string) => void;
}

const BLOCKED_TIP = "Close the game to change plugins";

function isIni(name: string): boolean {
  return /\.ini$/i.test(name);
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const kb = bytes / 1024;
  if (kb < 1024) return `${kb.toFixed(kb < 10 ? 1 : 0)} KB`;
  const mb = kb / 1024;
  return `${mb.toFixed(mb < 10 ? 1 : 0)} MB`;
}

function plural(count: number, word: string): string {
  return `${count} ${word}${count !== 1 ? "s" : ""}`;
}

function baseName(path: string): string {
  return path.split(/[\\/]/).pop() ?? path;
}

/** Survives switching on and off, which renames the file. */
function pluginKey(plugin: Plugin): string {
  return `${plugin.location}/${plugin.file_name.replace(/\.disabled$/i, "")}`.toLowerCase();
}

function loaderLabel(kind: LoaderKind): string {
  if (kind === "minimal_asi_loader") return "Minimal ASI Loader";
  if (kind === "ultimate_asi_loader") return "Ultimate ASI Loader";
  return "Unknown proxy DLL";
}

function pathTip(path: string, modified: number | null): React.ReactNode {
  return (
    <span className="flex flex-col">
      <span>{path}</span>
      {modified !== null && (
        <span className="text-muted-foreground">
          Modified {new Date(modified * 1000).toLocaleString()}
        </span>
      )}
    </span>
  );
}

function bypassWarning(entries: Entry[]): string | null {
  if (entries.some((e) => e.bypass === "payload"))
    return "This is the signature bypass. Without it, mods in ~mods stop loading.";
  if (entries.some((e) => e.bypass === "loader" || e.bypass === "outdated_loader"))
    return "This is the loader the signature bypass installed. Without it no plugin loads, the bypass included, so mods in ~mods stop loading.";
  return null;
}

function listNames(names: string[], max = 4): string {
  if (names.length <= max) return names.join(", ");
  return `${names.slice(0, max).join(", ")} and ${names.length - max} more`;
}

export function Plugins({ gamePath, isActive, gameRunning, pathLoading, onOpenInEditor }: Props) {
  const [status, setStatus] = useState<PluginsStatus | null>(null);
  const [notice, setNotice] = useState<{ msg: string; type: StatusType } | null>(null);
  const [busy, setBusy] = useState<Set<string>>(new Set());
  const [bulkBusy, setBulkBusy] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [searchQuery, setSearchQuery] = useState("");
  const [pendingDelete, setPendingDelete] = useState<string | null>(null);
  const [deleteRequest, setDeleteRequest] = useState<Entry[] | null>(null);
  const [withCompanions, setWithCompanions] = useState(false);
  const [confirmOff, setConfirmOff] = useState<Entry[] | null>(null);
  const [replaceRequest, setReplaceRequest] = useState<{
    sources: string[];
    existing: string[];
  } | null>(null);
  const [isDragging, setIsDragging] = useState(false);
  const noticeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const lastClickedIndex = useRef<number | null>(null);
  const statusRef = useRef(status);
  const isActiveRef = useRef(isActive);
  const gameRunningRef = useRef(gameRunning);
  const dropProcessingRef = useRef(false);
  const refreshRef = useRef<() => Promise<void>>(async () => {});
  const installRef = useRef<(sources: string[], replace?: boolean) => Promise<void>>(
    async () => {}
  );

  const modsFolder = gamePath ? `${gamePath}\\MarvelGame\\Marvel\\Content\\Paks\\~mods` : "";

  const showNotice = useCallback((msg: string, type: StatusType, duration = 6000) => {
    if (noticeTimer.current) clearTimeout(noticeTimer.current);
    setNotice({ msg, type });
    noticeTimer.current = setTimeout(() => setNotice(null), duration);
  }, []);

  useEffect(() => {
    statusRef.current = status;
  }, [status]);
  useEffect(() => {
    isActiveRef.current = isActive;
  }, [isActive]);
  useEffect(() => {
    gameRunningRef.current = gameRunning;
  }, [gameRunning]);

  const refresh = useCallback(
    (announce = false): Promise<void> => {
      if (!gamePath) return Promise.resolve();
      return invoke<PluginsStatus>("get_plugins_status", { gameRoot: gamePath }).then(
        (next) => {
          setStatus(next);
          const alive = new Set(next.plugins.map(pluginKey));
          setSelected((prev) => {
            const kept = new Set([...prev].filter((key) => alive.has(key)));
            return kept.size === prev.size ? prev : kept;
          });
          if (announce) showNotice("Plugins refreshed", "ok", 4000);
        },
        (e: unknown) => showNotice(String(e), "err")
      );
    },
    [gamePath, showNotice]
  );

  useEffect(() => {
    refreshRef.current = refresh;
  }, [refresh]);

  // This folder is often changed outside the app, so look again whenever the tab is shown.
  useEffect(() => {
    if (isActive) void refresh();
  }, [isActive, refresh]);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    getCurrentWindow()
      .onFocusChanged(({ payload: focused }) => {
        if (focused && isActiveRef.current) void refreshRef.current();
      })
      .then((fn) => {
        if (disposed) fn();
        else unlisten = fn;
      })
      .catch(() => {});
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    if (!modsFolder) return;
    const folder = normalizeFolderPath(modsFolder);
    return onModsChanged((event) => {
      if (event.source === "Plugins") return;
      if (normalizeFolderPath(event.modsFolder) !== folder) return;
      void refreshRef.current();
    });
  }, [modsFolder]);

  const afterChange = useCallback(
    async (bypassTouched: boolean) => {
      await refresh();
      if (bypassTouched && modsFolder) emitModsChanged({ modsFolder, source: "Plugins" });
    },
    [refresh, modsFolder]
  );

  function markBusy(paths: string[], on: boolean) {
    setBusy((prev) => {
      const next = new Set(prev);
      for (const path of paths) {
        if (on) next.add(path);
        else next.delete(path);
      }
      return next;
    });
  }

  function reportBulk(result: BulkOpResult, verb: string) {
    if (result.failures.length === 0) {
      showNotice(`${verb} ${plural(result.successes, "file")}`, "ok", 4000);
      return;
    }
    const first = result.failures[0];
    const more =
      result.failures.length > 1 ? ` (and ${result.failures.length - 1} more failed)` : "";
    showNotice(`${first.full_name}: ${first.error}${more}`, "err");
  }

  async function runToggle(entries: Entry[], enabled: boolean) {
    if (!gamePath || entries.length === 0) return;
    const paths = entries.map((e) => e.path);
    markBusy(paths, true);
    if (entries.length > 1) setBulkBusy(true);
    try {
      if (entries.length === 1) {
        await invoke("set_plugin_enabled", { gameRoot: gamePath, path: paths[0], enabled });
      } else {
        const result = await invoke<BulkOpResult>("set_plugins_enabled", {
          gameRoot: gamePath,
          paths,
          enabled,
        });
        reportBulk(result, enabled ? "Switched on" : "Switched off");
      }
    } catch (e: unknown) {
      showNotice(String(e), "err");
    } finally {
      markBusy(paths, false);
      setBulkBusy(false);
      await afterChange(entries.some((e) => e.bypass !== null || "kind" in e));
    }
  }

  function requestToggle(entries: Entry[], enabled: boolean) {
    const targets = entries.filter((e) => e.enabled !== enabled);
    if (targets.length === 0) return;
    if (!enabled && targets.some((e) => e.bypass !== null)) {
      setConfirmOff(targets);
      return;
    }
    void runToggle(targets, enabled);
  }

  async function runDelete(paths: string[], companions: boolean, bypassTouched: boolean) {
    if (!gamePath || paths.length === 0) return;
    markBusy(paths, true);
    setBulkBusy(true);
    try {
      const result = await invoke<BulkOpResult>("delete_plugins", {
        gameRoot: gamePath,
        paths,
        withCompanions: companions,
      });
      reportBulk(result, "Deleted");
    } catch (e: unknown) {
      showNotice(String(e), "err");
    } finally {
      markBusy(paths, false);
      setBulkBusy(false);
      setPendingDelete(null);
      await afterChange(bypassTouched);
    }
  }

  /** Straight to the inline Yes/No when there is nothing more to ask, else to the dialog. */
  function requestDelete(entries: Entry[]) {
    if (entries.length === 0) return;
    const asks = entries.some((e) => e.companions.length > 0 || e.bypass !== null);
    if (entries.length === 1 && !asks) {
      setPendingDelete(entries[0].path);
      return;
    }
    setWithCompanions(false);
    setDeleteRequest(entries);
  }

  async function install(sourcePaths: string[], replace = false) {
    if (!gamePath || sourcePaths.length === 0) return;
    showNotice("Installing…", "info", 600_000);
    try {
      const outcomes = await invoke<PluginInstall[]>("install_plugins", {
        gameRoot: gamePath,
        sourcePaths,
        replace,
      });
      const plugins = outcomes.flatMap((o) => o.installed).filter((n) => /\.asi$/i.test(n));
      const kept = outcomes.flatMap((o) => o.kept);
      const skipped = outcomes.flatMap((o) => o.skipped);
      const errors = outcomes
        .filter((o) => o.error)
        .map((o) => `${baseName(o.source)}: ${o.error}`);
      const waiting = outcomes.filter((o) => o.existing.length > 0);

      const parts: string[] = [];
      if (plugins.length > 0) parts.push(`Installed ${plural(plugins.length, "plugin")}`);
      if (kept.length > 0) parts.push(`kept your ${listNames(kept, 3)}`);
      if (skipped.length > 0)
        parts.push(
          `skipped ${plural(skipped.length, "file")} (${skipped[0].file}: ${skipped[0].reason})`
        );
      if (errors.length > 0) {
        const more = errors.length > 1 ? ` (and ${errors.length - 1} more failed)` : "";
        showNotice([...parts, `${errors[0]}${more}`].join("; "), "err");
      } else if (parts.length > 0) {
        showNotice(parts.join(", "), plugins.length > 0 ? "ok" : "info");
      } else {
        setNotice(null);
      }
      if (waiting.length > 0 && !replace) {
        setReplaceRequest({
          sources: waiting.map((o) => o.source),
          existing: waiting.flatMap((o) => o.existing),
        });
      }
      if (plugins.length > 0) await afterChange(true);
    } catch (e: unknown) {
      showNotice(String(e), "err");
    }
  }

  useEffect(() => {
    installRef.current = install;
  });

  async function pickAndInstall() {
    const picked = await open({
      multiple: true,
      filters: [{ name: "ASI plugins and archives", extensions: ["asi", "zip", "7z", "rar"] }],
    }).catch(() => null);
    if (!picked) return;
    await install(Array.isArray(picked) ? picked : [picked]);
  }

  async function installLoader() {
    if (!gamePath) return;
    try {
      const msg = await invoke<string>("install_asi_loader", { gameRoot: gamePath });
      showNotice(msg, "ok");
    } catch (e: unknown) {
      showNotice(String(e), "err");
    }
    await afterChange(true);
  }

  // Drag-and-drop: install the .asi files, folders and archives dropped on the window.
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    getCurrentWindow()
      .onDragDropEvent(async (event) => {
        if (event.payload.type === "enter") {
          if (isActiveRef.current && statusRef.current?.win64_exists && !gameRunningRef.current)
            setIsDragging(true);
        } else if (event.payload.type === "drop") {
          setIsDragging(false);
          if (!isActiveRef.current || dropProcessingRef.current) return;
          if (gameRunningRef.current) {
            showNotice("Close Marvel Rivals before installing plugins.", "err");
            return;
          }
          if (!statusRef.current?.win64_exists || event.payload.paths.length === 0) return;
          dropProcessingRef.current = true;
          try {
            await installRef.current(event.payload.paths);
          } finally {
            dropProcessingRef.current = false;
          }
        } else if (event.payload.type === "leave") {
          setIsDragging(false);
        }
      })
      .then((fn) => {
        if (disposed) fn();
        else unlisten = fn;
      })
      .catch(() => {});
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [showNotice]);

  const plugins = useMemo(() => status?.plugins ?? [], [status]);
  const loaders = status?.loaders ?? [];

  const filtered = useMemo(() => {
    const query = searchQuery.trim().toLowerCase();
    if (!query) return plugins;
    return plugins.filter(
      (p) =>
        p.file_name.toLowerCase().includes(query) ||
        p.companions.some((c) => c.file_name.toLowerCase().includes(query))
    );
  }, [plugins, searchQuery]);

  const selectedEntries = useMemo(
    () => plugins.filter((p) => selected.has(pluginKey(p))),
    [plugins, selected]
  );

  function handleRowClick(index: number, key: string, e: React.MouseEvent) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (e.shiftKey && lastClickedIndex.current !== null) {
        const start = Math.min(lastClickedIndex.current, index);
        const end = Math.max(lastClickedIndex.current, index);
        for (let i = start; i <= end; i++) next.add(pluginKey(filtered[i]));
      } else if (e.ctrlKey || e.metaKey) {
        if (next.has(key)) next.delete(key);
        else next.add(key);
      } else if (next.size === 1 && next.has(key)) {
        next.clear();
      } else {
        next.clear();
        next.add(key);
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
    setSelected(new Set(filtered.map(pluginKey)));
  }

  const dialogOpen = deleteRequest !== null || confirmOff !== null || replaceRequest !== null;

  useEffect(() => {
    if (!isActive) return;
    function isTypingTarget(el: EventTarget | null): boolean {
      if (!(el instanceof HTMLElement)) return false;
      return el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.isContentEditable;
    }
    function onKeyDown(e: KeyboardEvent) {
      if (isTypingTarget(e.target) || dialogOpen) return;
      if ((e.key === "a" || e.key === "A") && (e.ctrlKey || e.metaKey)) {
        if (filtered.length === 0) return;
        e.preventDefault();
        selectAll();
      } else if (e.key === "Escape" && selected.size > 0) {
        e.preventDefault();
        clearSelection();
      } else if (
        (e.key === "Delete" || e.key === "Backspace") &&
        selectedEntries.length > 0 &&
        !bulkBusy &&
        !gameRunning
      ) {
        e.preventDefault();
        setWithCompanions(false);
        setDeleteRequest(selectedEntries);
      }
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isActive, selected, selectedEntries, filtered, bulkBusy, gameRunning, dialogOpen]);

  const enabledPlugins = plugins.filter((p) => p.enabled).length;
  const loadersOn = loaders.filter((l) => l.enabled);
  const recognisedOn = loadersOn.filter((l) => l.kind !== "unknown");
  const unknownOn = loadersOn.filter((l) => l.kind === "unknown");
  const outdated = loaders.find((l) => l.bypass === "outdated_loader");
  const totalCount = plugins.length;
  const selectAllState: boolean | "indeterminate" =
    filtered.length > 0 && selected.size === filtered.length
      ? true
      : selected.size > 0
        ? "indeterminate"
        : false;
  const allOnInSelection = selectedEntries.every((p) => p.enabled);
  const allOffInSelection = selectedEntries.every((p) => !p.enabled);

  const deleteCompanions = deleteRequest?.flatMap((e) => e.companions.map((c) => c.file_name));
  const deleteWarning = deleteRequest ? bypassWarning(deleteRequest) : null;
  const offWarning = confirmOff ? bypassWarning(confirmOff) : null;

  function openInDefaultApp(path: string) {
    openPath(path).catch((err: unknown) => showNotice(String(err), "err"));
  }

  function openCompanion(c: Companion) {
    if (onOpenInEditor && isIni(c.file_name)) onOpenInEditor(c.path);
    else openInDefaultApp(c.path);
  }

  function companionChips(companions: Companion[]) {
    return companions.map((c) => (
      <Tip
        key={c.path}
        content={onOpenInEditor && isIni(c.file_name) ? "Edit in INI Editor" : `Open ${c.path}`}
      >
        <button
          type="button"
          onClick={(e) => {
            e.stopPropagation();
            openCompanion(c);
          }}
          className="flex shrink-0 items-center gap-1 rounded-full bg-secondary/80 px-2 py-0.5 text-[11px] text-foreground hover:bg-secondary"
        >
          <FileText size={11} className="text-muted-foreground" />
          <span className="max-w-36 truncate">{c.file_name}</span>
        </button>
      </Tip>
    ));
  }

  function bypassChip(role: BypassRole | null) {
    if (!role) return null;
    return (
      <Tip
        content={
          role === "outdated_loader"
            ? "An outdated loader build the toolkit shipped with the signature bypass"
            : "Installed by the signature bypass, which mods in ~mods need to load"
        }
      >
        <span
          className={cn(
            "flex shrink-0 items-center gap-1 rounded px-1.5 py-0.5 text-[9px] font-semibold uppercase leading-none",
            role === "outdated_loader"
              ? "bg-warn/15 text-warn"
              : "bg-blue-accent text-blue-accent-foreground"
          )}
        >
          <Shield size={9} />
          {role === "outdated_loader" ? "Outdated bypass" : "Signature bypass"}
        </span>
      </Tip>
    );
  }

  function inlineDelete(path: string, bypassTouched: boolean) {
    return (
      <div className="flex shrink-0 items-center gap-1" onClick={(e) => e.stopPropagation()}>
        <span className="text-[11px] font-medium text-err">Delete?</span>
        <Tip content="Confirm delete">
          <button
            disabled={busy.has(path)}
            onClick={() => runDelete([path], false, bypassTouched)}
            className="rounded px-1.5 text-[11px] font-semibold bg-err text-white hover:opacity-90 transition-opacity leading-5.5"
          >
            Yes
          </button>
        </Tip>
        <Tip content="Cancel">
          <button
            onClick={() => setPendingDelete(null)}
            className="rounded px-1.5 text-[11px] font-semibold border border-border text-muted-foreground hover:bg-secondary transition-colors leading-5.5"
          >
            No
          </button>
        </Tip>
      </div>
    );
  }

  function entryControls(entry: Entry, kindLabel: string) {
    const isBusy = busy.has(entry.path) || bulkBusy;
    if (pendingDelete === entry.path) return inlineDelete(entry.path, entry.bypass !== null);
    return (
      <>
        <Tip content={gameRunning ? BLOCKED_TIP : entry.enabled ? "Switch off" : "Switch on"}>
          <span className="shrink-0">
            <Switch
              checked={entry.enabled}
              disabled={isBusy || gameRunning}
              onClick={(e) => e.stopPropagation()}
              onCheckedChange={(v) => requestToggle([entry], v)}
              aria-label={`Switch ${entry.file_name} ${entry.enabled ? "off" : "on"}`}
            />
          </span>
        </Tip>
        <Tip content={gameRunning ? BLOCKED_TIP : `Delete ${kindLabel}`}>
          <button
            disabled={isBusy || gameRunning}
            onClick={(e) => {
              e.stopPropagation();
              requestDelete([entry]);
            }}
            aria-label={`Delete ${entry.file_name}`}
            className="shrink-0 rounded p-1 text-err/70 transition-colors hover:text-err hover:bg-err/10 disabled:opacity-40 disabled:hover:bg-transparent"
          >
            <Trash2 size={13} />
          </button>
        </Tip>
      </>
    );
  }

  return (
    <div className="relative flex flex-1 min-h-0 w-full flex-col gap-3">
      {isDragging && (
        <div className="pointer-events-none absolute inset-0 z-50 flex flex-col items-center justify-center gap-3 rounded-lg border-2 border-dashed border-ok bg-background/80 backdrop-blur-sm">
          <UploadCloud size={36} className="text-ok" />
          <span className="text-sm font-semibold text-ok">
            Drop .asi plugins, archives or folders to install
          </span>
        </div>
      )}

      <div className="flex min-h-8 items-center gap-3">
        <h2 className="shrink-0 text-xl font-bold">Plugins</h2>
        {notice && (
          <span
            className={cn(
              "flex min-w-0 items-center gap-1.5 truncate text-[12px] font-medium",
              notice.type === "ok"
                ? "text-ok"
                : notice.type === "err"
                  ? "text-err"
                  : "text-muted-foreground"
            )}
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

      {gameRunning && (
        <div className="flex items-center gap-2.5 rounded-md border border-warn/20 bg-warn/5 px-3 py-2">
          <AlertTriangle size={15} className="shrink-0 text-warn" />
          <span className="flex-1 text-[12px] text-warn">
            Marvel Rivals is running! Close the game to change plugins.
          </span>
        </div>
      )}

      {status && !status.win64_exists && (
        <div className="flex items-center gap-2.5 rounded-md border border-warn/20 bg-warn/5 px-3 py-2">
          <AlertTriangle size={15} className="shrink-0 text-warn" />
          <span className="flex-1 truncate text-[12px] text-warn">
            Game binaries folder not found: {status.win64_dir}
          </span>
        </div>
      )}

      {status?.win64_exists && enabledPlugins > 0 && recognisedOn.length === 0 && (
        <div className="flex items-center gap-2.5 rounded-md border border-warn/20 bg-warn/5 px-3 py-2">
          <AlertTriangle size={15} className="shrink-0 text-warn" />
          <span className="flex-1 text-[12px] text-warn">
            {unknownOn.length > 0
              ? `No recognised ASI loader is switched on. ${listNames(unknownOn.map((l) => l.file_name))} may load plugins, but it is not one the toolkit knows.`
              : "No ASI loader is switched on, so none of these plugins load."}
          </span>
          <Button
            variant="green"
            size="xs"
            onClick={installLoader}
            disabled={!gamePath || gameRunning}
          >
            Install Loader
          </Button>
        </div>
      )}

      {recognisedOn.length > 1 && (
        <div className="flex items-center gap-2.5 rounded-md border border-warn/20 bg-warn/5 px-3 py-2">
          <AlertTriangle size={15} className="shrink-0 text-warn" />
          <span className="flex-1 text-[12px] text-warn">
            {listNames(recognisedOn.map((l) => l.file_name))} are all ASI loaders. Each plugin still
            loads once, but one loader is enough.
          </span>
        </div>
      )}

      {outdated && (
        <div className="flex items-center gap-2.5 rounded-md border border-warn/20 bg-warn/5 px-3 py-2">
          <Shield size={15} className="shrink-0 text-warn" />
          <span className="flex-1 text-[12px] text-warn">
            {outdated.file_name} is an outdated loader build the toolkit shipped
          </span>
          <Button
            variant="green"
            size="xs"
            onClick={installLoader}
            disabled={!gamePath || gameRunning}
          >
            Update Loader
          </Button>
        </div>
      )}

      {/* Loaders */}
      <section className="shrink-0 overflow-hidden rounded-md border border-border">
        <div className="flex h-10 items-center gap-2 border-b border-border bg-card px-3">
          <h3 className="text-sm font-semibold">ASI Loader</h3>
          <span className="truncate text-[12px] text-muted-foreground">
            Proxy DLLs beside the game executable
          </span>
          <span className="flex-1" />
          <Tip content="Open the Win64 folder">
            <Button
              variant="ghost"
              size="icon-sm"
              disabled={!status?.win64_exists}
              onClick={() =>
                status &&
                openPath(status.win64_dir).catch((e: unknown) => showNotice(String(e), "err"))
              }
            >
              <FolderOpen size={15} />
            </Button>
          </Tip>
        </div>
        {loaders.length === 0 ? (
          <div className="flex h-12 items-center bg-background px-3 text-[12px] text-muted-foreground">
            {status ? "No loader DLL beside the game." : "Loading…"}
          </div>
        ) : (
          <ul className="bg-background">
            {loaders.map((loader) => (
              <ContextMenu key={loader.path}>
                <ContextMenuTrigger asChild>
                  <li className="flex h-12 items-center gap-3 border-b border-border/50 px-3 last:border-transparent hover:bg-secondary/40">
                    <span
                      className={cn(
                        "flex min-w-0 flex-1 items-center gap-2 text-[13px]",
                        !loader.enabled && "opacity-40"
                      )}
                    >
                      <Cpu size={14} className="shrink-0 text-muted-foreground" />
                      <Tip content={pathTip(loader.path, loader.modified)} side="top" align="start">
                        <span className="shrink-0 font-medium">{loader.file_name}</span>
                      </Tip>
                      <span
                        className={cn(
                          "shrink-0 rounded px-1.5 py-0.5 text-[10px] font-semibold leading-none",
                          loader.kind === "unknown"
                            ? "bg-secondary text-muted-foreground"
                            : "bg-ok/15 text-ok"
                        )}
                      >
                        {loaderLabel(loader.kind)}
                      </span>
                      {bypassChip(loader.bypass)}
                      {loader.description && (
                        <span className="min-w-0 truncate text-[12px] text-muted-foreground">
                          {loader.description}
                        </span>
                      )}
                      {companionChips(loader.companions)}
                      <span className="flex-1" />
                      <span className="shrink-0 w-14 text-right font-mono text-[11px] text-muted-foreground/60 tabular-nums">
                        {formatBytes(loader.size)}
                      </span>
                    </span>
                    {entryControls(loader, "loader")}
                  </li>
                </ContextMenuTrigger>
                <ContextMenuContent>
                  <ContextMenuItem onSelect={() => revealItemInDir(loader.path)}>
                    <FolderOpen />
                    Reveal in Explorer
                  </ContextMenuItem>
                  <ContextMenuSeparator />
                  <ContextMenuItem
                    disabled={gameRunning}
                    onSelect={() => requestToggle([loader], !loader.enabled)}
                  >
                    {loader.enabled ? <PowerOff /> : <Power />}
                    {loader.enabled ? "Switch off" : "Switch on"}
                  </ContextMenuItem>
                  <ContextMenuItem
                    destructive
                    disabled={gameRunning}
                    onSelect={() => requestDelete([loader])}
                  >
                    <Trash2 />
                    Delete
                  </ContextMenuItem>
                </ContextMenuContent>
              </ContextMenu>
            ))}
          </ul>
        )}
      </section>

      {/* Plugins */}
      <div className="flex min-h-0 flex-1 flex-col overflow-hidden rounded-md border border-border">
        <div className="flex h-12 shrink-0 items-center justify-between gap-2 border-b border-border bg-card px-3">
          {selected.size > 0 ? (
            <>
              <div className="flex items-center gap-2">
                <Checkbox
                  checked={selectAllState}
                  onCheckedChange={(v) => (v ? selectAll() : clearSelection())}
                  aria-label="Select all plugins"
                />
                <span className="text-sm font-semibold">{selected.size} selected</span>
              </div>
              <div className="flex items-center gap-1 -mr-1">
                <Tip content={gameRunning ? BLOCKED_TIP : "Switch on selected plugins"}>
                  <Button
                    variant="ghost"
                    size="xs"
                    onClick={() => requestToggle(selectedEntries, true)}
                    disabled={bulkBusy || allOnInSelection || gameRunning}
                    className="text-ok hover:text-ok hover:bg-ok/10"
                  >
                    <Power size={13} />
                    Enable
                  </Button>
                </Tip>
                <Tip content={gameRunning ? BLOCKED_TIP : "Switch off selected plugins"}>
                  <Button
                    variant="ghost"
                    size="xs"
                    onClick={() => requestToggle(selectedEntries, false)}
                    disabled={bulkBusy || allOffInSelection || gameRunning}
                  >
                    <PowerOff size={13} />
                    Disable
                  </Button>
                </Tip>
                <Tip content={gameRunning ? BLOCKED_TIP : "Delete selected plugins"}>
                  <Button
                    variant="ghost"
                    size="xs"
                    onClick={() => {
                      setWithCompanions(false);
                      setDeleteRequest(selectedEntries);
                    }}
                    disabled={bulkBusy || gameRunning}
                    className="text-err hover:text-err hover:bg-err/10"
                  >
                    <Trash2 size={13} />
                    Delete
                  </Button>
                </Tip>
                <Tip content="Clear selection (Esc)">
                  <Button variant="ghost" size="icon-sm" onClick={clearSelection}>
                    <X size={15} />
                  </Button>
                </Tip>
              </div>
            </>
          ) : (
            <>
              <div className="flex min-w-0 items-center gap-2">
                {filtered.length > 0 && (
                  <Checkbox
                    checked={selectAllState}
                    onCheckedChange={(v) => (v ? selectAll() : clearSelection())}
                    aria-label="Select all plugins"
                  />
                )}
                <h3 className="text-sm font-semibold">Plugins</h3>
                {status && (
                  <span className="shrink-0 text-[12px] tabular-nums text-muted-foreground">
                    {searchQuery.trim()
                      ? `(${filtered.length} of ${totalCount} shown)`
                      : `(${enabledPlugins}/${totalCount} active)`}
                  </span>
                )}
                {totalCount > 0 && (
                  <div className="ml-1 flex h-7 w-48 items-center gap-1.5 rounded-sm border border-border bg-background px-2 focus-within:ring-1 focus-within:ring-primary">
                    <Search size={12} className="shrink-0 text-muted-foreground" />
                    <input
                      value={searchQuery}
                      onChange={(e) => setSearchQuery(e.target.value)}
                      placeholder="Search plugins..."
                      className="min-w-0 flex-1 bg-transparent text-[12px] outline-none placeholder:text-muted-foreground/50"
                    />
                    {searchQuery && (
                      <Tip content="Clear search">
                        <button
                          type="button"
                          onClick={() => setSearchQuery("")}
                          className="shrink-0 text-muted-foreground hover:text-foreground"
                        >
                          <X size={12} />
                        </button>
                      </Tip>
                    )}
                  </div>
                )}
              </div>
              <div className="flex items-center gap-1 -mr-1">
                <Tip content={gameRunning ? BLOCKED_TIP : "Install .asi plugins or archives"}>
                  <Button
                    variant="ghost"
                    size="xs"
                    onClick={pickAndInstall}
                    disabled={!status?.win64_exists || gameRunning}
                  >
                    <Plus size={13} />
                    Add
                  </Button>
                </Tip>
                <Tip
                  content={
                    status?.plugins_dir_exists
                      ? "Open the plugins folder"
                      : "No plugins folder yet, installing a plugin creates it"
                  }
                >
                  <span>
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      disabled={!status?.plugins_dir_exists}
                      onClick={() =>
                        status &&
                        openPath(status.plugins_dir).catch((e: unknown) =>
                          showNotice(String(e), "err")
                        )
                      }
                    >
                      <FolderOpen size={15} />
                    </Button>
                  </span>
                </Tip>
                <Tip content="Refresh">
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    onClick={() => refresh(true)}
                    disabled={!gamePath}
                  >
                    <RefreshCw size={15} />
                  </Button>
                </Tip>
              </div>
            </>
          )}
        </div>

        <div className="min-h-0 flex-1 overflow-y-auto bg-background">
          {pathLoading ? null : !status ? (
            <p className="px-3 py-4 text-[12px] text-muted-foreground">
              {gamePath ? "Loading…" : ""}
            </p>
          ) : totalCount === 0 ? (
            <div className="flex h-full flex-col items-center justify-center gap-2 py-10 text-center">
              <UploadCloud size={28} className="text-muted-foreground/60" />
              <p className="text-sm text-muted-foreground">No plugins installed.</p>
              <p className="text-[12px] text-muted-foreground/80">
                Drop .asi files, folders or zip, 7z and rar archives here, or use Add.
              </p>
            </div>
          ) : (
            <ul>
              {filtered.length === 0 && (
                <li className="flex h-12 items-center justify-center text-[12px] text-muted-foreground">
                  No plugins match the search.
                </li>
              )}
              {filtered.map((plugin, index) => {
                const key = pluginKey(plugin);
                const isSelected = selected.has(key);
                const isMulti = isSelected && selected.size > 1;
                return (
                  <ContextMenu key={plugin.path}>
                    <ContextMenuTrigger asChild>
                      <li
                        onClick={(e) => handleRowClick(index, key, e)}
                        onContextMenu={() => {
                          if (!selected.has(key)) {
                            setSelected(new Set([key]));
                            lastClickedIndex.current = index;
                          }
                        }}
                        className={cn(
                          "relative flex h-12 cursor-default items-center gap-3 border-b border-border/50 px-3 last:border-transparent select-none",
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
                              if (v) next.add(key);
                              else next.delete(key);
                              return next;
                            })
                          }
                          aria-label={`Select ${plugin.name}`}
                        />
                        <span
                          className={cn(
                            "flex min-w-0 flex-1 items-center gap-2 text-[13px]",
                            !plugin.enabled && "opacity-40"
                          )}
                        >
                          <Tip
                            content={pathTip(plugin.path, plugin.modified)}
                            side="top"
                            align="start"
                          >
                            <span className="min-w-0 truncate">{plugin.name}</span>
                          </Tip>
                          {plugin.location !== "plugins" && (
                            <Tip
                              content={
                                plugin.location === ""
                                  ? "Beside the game executable"
                                  : `In ${plugin.location}`
                              }
                            >
                              <span className="shrink-0 rounded bg-secondary px-1.5 py-0.5 text-[10px] font-semibold leading-none text-muted-foreground">
                                {plugin.location === "" ? "Win64" : plugin.location}
                              </span>
                            </Tip>
                          )}
                          {bypassChip(plugin.bypass)}
                          {plugin.shadowed_by && (
                            <Tip
                              content={`The copy beside the game loads instead: ${plugin.shadowed_by}`}
                            >
                              <span className="shrink-0 rounded bg-warn/15 px-1.5 py-0.5 text-[9px] font-semibold uppercase leading-none text-warn">
                                Overridden
                              </span>
                            </Tip>
                          )}
                          {companionChips(plugin.companions)}
                          <span className="flex-1" />
                          <span className="shrink-0 w-14 text-right font-mono text-[11px] text-muted-foreground/60 tabular-nums">
                            {formatBytes(plugin.size)}
                          </span>
                        </span>
                        {entryControls(plugin, "plugin")}
                      </li>
                    </ContextMenuTrigger>
                    <ContextMenuContent>
                      {isMulti ? (
                        <>
                          <ContextMenuItem
                            disabled={bulkBusy || allOnInSelection || gameRunning}
                            onSelect={() => requestToggle(selectedEntries, true)}
                          >
                            <Power />
                            Enable {selected.size} selected
                          </ContextMenuItem>
                          <ContextMenuItem
                            disabled={bulkBusy || allOffInSelection || gameRunning}
                            onSelect={() => requestToggle(selectedEntries, false)}
                          >
                            <PowerOff />
                            Disable {selected.size} selected
                          </ContextMenuItem>
                          <ContextMenuSeparator />
                          <ContextMenuItem
                            destructive
                            disabled={bulkBusy || gameRunning}
                            onSelect={() => {
                              setWithCompanions(false);
                              setDeleteRequest(selectedEntries);
                            }}
                          >
                            <Trash2 />
                            Delete {selected.size} selected
                          </ContextMenuItem>
                        </>
                      ) : (
                        <>
                          <ContextMenuItem onSelect={() => revealItemInDir(plugin.path)}>
                            <FolderOpen />
                            Reveal in Explorer
                          </ContextMenuItem>
                          {plugin.companions.map((c) =>
                            onOpenInEditor && isIni(c.file_name) ? (
                              <React.Fragment key={c.path}>
                                <ContextMenuItem onSelect={() => onOpenInEditor(c.path)}>
                                  <FileText />
                                  Edit {c.file_name}
                                </ContextMenuItem>
                                <ContextMenuItem onSelect={() => openInDefaultApp(c.path)}>
                                  <FileText />
                                  Open {c.file_name} in default app
                                </ContextMenuItem>
                              </React.Fragment>
                            ) : (
                              <ContextMenuItem
                                key={c.path}
                                onSelect={() => openInDefaultApp(c.path)}
                              >
                                <FileText />
                                Open {c.file_name}
                              </ContextMenuItem>
                            )
                          )}
                          <ContextMenuSeparator />
                          <ContextMenuItem
                            disabled={gameRunning}
                            onSelect={() => requestToggle([plugin], !plugin.enabled)}
                          >
                            {plugin.enabled ? <PowerOff /> : <Power />}
                            {plugin.enabled ? "Switch off" : "Switch on"}
                          </ContextMenuItem>
                          <ContextMenuItem
                            destructive
                            disabled={gameRunning}
                            onSelect={() => requestDelete([plugin])}
                          >
                            <Trash2 />
                            Delete
                          </ContextMenuItem>
                        </>
                      )}
                    </ContextMenuContent>
                  </ContextMenu>
                );
              })}
            </ul>
          )}
        </div>
      </div>

      <p className="shrink-0 text-[11px] text-muted-foreground">
        Loaders read plugins when the game starts, so changes apply on the next launch.
      </p>

      <AlertDialog open={deleteRequest !== null} onOpenChange={(o) => !o && setDeleteRequest(null)}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              {deleteRequest?.length === 1
                ? `Delete ${deleteRequest[0].file_name}?`
                : `Delete ${plural(deleteRequest?.length ?? 0, "file")}?`}
            </AlertDialogTitle>
            <AlertDialogDescription>
              {deleteRequest && deleteRequest.length > 1
                ? `${listNames(deleteRequest.map((e) => e.file_name))} will be removed. `
                : ""}
              This cannot be undone.
            </AlertDialogDescription>
          </AlertDialogHeader>
          {deleteWarning && (
            <p className="flex items-start gap-2 text-[12px] text-warn">
              <Shield size={14} className="mt-0.5 shrink-0" />
              {deleteWarning}
            </p>
          )}
          {deleteCompanions && deleteCompanions.length > 0 && (
            <label className="flex items-start gap-2 text-[12px]">
              <Checkbox
                checked={withCompanions}
                onCheckedChange={(v) => setWithCompanions(v === true)}
                className="mt-0.5"
              />
              <span>
                Also delete {listNames(deleteCompanions)}
                <span className="block text-muted-foreground">
                  Settings files can hold your configuration or licence keys.
                </span>
              </span>
            </label>
          )}
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              className="bg-err text-white hover:bg-err/90 focus-visible:ring-err/40"
              onClick={() => {
                const entries = deleteRequest ?? [];
                setDeleteRequest(null);
                void runDelete(
                  entries.map((e) => e.path),
                  withCompanions,
                  entries.some((e) => e.bypass !== null || "kind" in e)
                );
              }}
            >
              Delete
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      <AlertDialog open={confirmOff !== null} onOpenChange={(o) => !o && setConfirmOff(null)}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              Switch off{" "}
              {listNames(
                (confirmOff ?? []).map((e) => e.file_name),
                2
              )}
              ?
            </AlertDialogTitle>
            <AlertDialogDescription>{offWarning}</AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={() => {
                const entries = confirmOff ?? [];
                setConfirmOff(null);
                void runToggle(entries, false);
              }}
            >
              Switch Off
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      <AlertDialog
        open={replaceRequest !== null}
        onOpenChange={(o) => !o && setReplaceRequest(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Replace installed plugins?</AlertDialogTitle>
            <AlertDialogDescription>
              {listNames(replaceRequest?.existing ?? [])}{" "}
              {(replaceRequest?.existing.length ?? 0) === 1 ? "is" : "are"} already installed.
              Replacing writes the new .asi over it, switched on, and keeps the settings files
              already there.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={() => {
                const sources = replaceRequest?.sources ?? [];
                setReplaceRequest(null);
                void install(sources, true);
              }}
            >
              Replace
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  );
}
