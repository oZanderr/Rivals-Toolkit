import * as React from "react";
import { useState, useEffect, useRef, useCallback } from "react";

import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { save } from "@tauri-apps/plugin-dialog";
import {
  Archive,
  FolderOpen,
  RefreshCw,
  Shield,
  CheckCircle2,
  XCircle,
  Trash2,
  UploadCloud,
} from "lucide-react";

import { Button } from "@/components/ui/button";
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

interface Props {
  gamePath: string;
  isActive: boolean;
}

type StatusType = "ok" | "err" | "info";

export function Mods({ gamePath, isActive }: Props) {
  const [modsStatus, setModsStatus] = useState<ModsStatus | null>(null);
  const [notice, setNotice] = useState<{ msg: string; type: StatusType } | null>(null);
  const [busyMods, setBusyMods] = useState<Set<string>>(new Set());
  const [pendingDelete, setPendingDelete] = useState<string | null>(null);
  const [isDragging, setIsDragging] = useState(false);
  const noticeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const modsStatusRef = useRef(modsStatus);
  const isActiveRef = useRef(isActive);
  const refreshRef = useRef<typeof refresh>(null!);
  const dropProcessingRef = useRef(false);
  const outerRef = useRef<HTMLDivElement>(null);

  const showNotice = useCallback((msg: string, type: StatusType, duration = 6000) => {
    if (noticeTimer.current) clearTimeout(noticeTimer.current);
    setNotice({ msg, type });
    noticeTimer.current = setTimeout(() => setNotice(null), duration);
  }, []);

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

  // Drag-and-drop: accept .pak and .zip files dropped anywhere on the window.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    getCurrentWindow()
      .onDragDropEvent(async (event) => {
        if (event.payload.type === "enter") {
          if (isActiveRef.current && modsStatusRef.current?.mods_folder_exists) setIsDragging(true);
        } else if (event.payload.type === "drop") {
          setIsDragging(false);
          if (!isActiveRef.current || dropProcessingRef.current) return;
          const folder = modsStatusRef.current?.mods_folder_path;
          if (!folder) return;
          const pakPaths = event.payload.paths.filter((p) => p.endsWith(".pak"));
          const zipPaths = event.payload.paths.filter((p) => p.toLowerCase().endsWith(".zip"));
          if (pakPaths.length === 0 && zipPaths.length === 0) return;
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

            // Install mods from .zip archives
            for (const z of zipPaths) {
              try {
                const results = await invoke<
                  { file_name: string; replaced_disabled: boolean; replaced_enabled: boolean }[]
                >("install_from_zip", { modsFolder: folder, zipPath: z });
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

  async function exportZip() {
    if (!modsStatus) return;
    try {
      const destPath = await save({
        defaultPath: "mods.zip",
        filters: [{ name: "Zip Archive", extensions: ["zip"] }],
      });
      if (!destPath) return;
      const msg = await invoke<string>("export_mods_zip", {
        modsFolder: modsStatus.mods_folder_path,
        destPath,
      });
      showNotice(msg, "ok", 4000);
    } catch (e: unknown) {
      showNotice(String(e), "err");
    }
  }

  const enabledCount = modsStatus?.mod_entries.filter((m) => m.enabled).length ?? 0;
  const totalCount = modsStatus?.mod_entries.length ?? 0;

  return (
    <div ref={outerRef} className="relative flex flex-1 min-h-0 w-full flex-col gap-3">
      {isDragging && (
        <div className="pointer-events-none absolute inset-0 z-50 flex flex-col items-center justify-center gap-3 rounded-lg border-2 border-dashed border-ok bg-background/80 backdrop-blur-sm">
          <UploadCloud size={36} className="text-ok" />
          <span className="text-sm font-semibold text-ok">Drop .pak or .zip to install</span>
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
        {!gamePath && (
          <span className="flex shrink-0 items-center gap-1.5 text-[12px] font-medium text-warn">
            <XCircle size={14} strokeWidth={2.5} />
            Set game root in Settings first
          </span>
        )}
      </div>

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
        <div className="flex items-center justify-between gap-2 border-b border-border bg-card px-3 py-2">
          <div className="flex items-center gap-2">
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
              disabled={enabledCount === 0}
              title={
                enabledCount === 0 ? "No enabled mods to export" : "Export enabled mods as zip"
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
        </div>

        {/* Scrollable list */}
        <div className="min-h-0 flex-1 overflow-y-auto bg-background">
          {!modsStatus || totalCount === 0 ? (
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
            <ul>
              {modsStatus.mod_entries.map((entry) => {
                const busy = busyMods.has(entry.full_name);
                return (
                  <li
                    key={entry.full_name}
                    className="flex h-12 items-center gap-3 border-b border-border/50 px-3 last:border-none hover:bg-secondary/40"
                  >
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
                      <div className="flex shrink-0 items-center gap-1">
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
                          disabled={busy}
                          onCheckedChange={() => toggleMod(entry)}
                          className="shrink-0"
                        />
                        <button
                          title="Delete mod"
                          disabled={busy}
                          onClick={() => deleteMod(entry)}
                          className="shrink-0 rounded p-1 text-muted-foreground/30 transition-colors hover:text-err hover:bg-err/10"
                        >
                          <Trash2 size={13} />
                        </button>
                      </>
                    )}
                  </li>
                );
              })}
            </ul>
          )}
        </div>
      </div>
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
