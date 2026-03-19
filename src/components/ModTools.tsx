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
  ShieldCheck,
  ShieldOff,
  ShieldX,
  CheckCircle2,
  XCircle,
  Trash2,
  UploadCloud,
} from "lucide-react";

import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";

interface ModEntry {
  full_name: string;
  display_name: string;
  enabled: boolean;
  has_companions: boolean;
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

export function ModTools({ gamePath, isActive }: Props) {
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

  // Drag-and-drop: accept .pak files dropped anywhere on the window.
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
          if (pakPaths.length === 0) return;
          dropProcessingRef.current = true;
          try {
            let installed = 0;
            let replacedDisabled = 0;
            let replacedEnabled = 0;
            const errors: string[] = [];
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
    if (!gamePath) return showNotice("Set game root on the Home tab first.", "err");
    try {
      const msg = await invoke<string>("install_signature_bypass", { gameRoot: gamePath });
      showNotice(msg, "ok", 4000);
      await refresh(true);
    } catch (e: unknown) {
      showNotice(String(e), "err");
    }
  }

  async function removeBypass() {
    if (!gamePath) return showNotice("Set game root on the Home tab first.", "err");
    try {
      const msg = await invoke<string>("remove_signature_bypass", { gameRoot: gamePath });
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
    <div className="relative flex flex-1 min-h-0 w-full flex-col gap-6">
      {isDragging && (
        <div className="pointer-events-none absolute inset-0 z-50 flex flex-col items-center justify-center gap-3 rounded-lg border-2 border-dashed border-[var(--color-ok)] bg-background/80 backdrop-blur-sm">
          <UploadCloud size={36} className="text-[var(--color-ok)]" />
          <span className="text-sm font-semibold text-[var(--color-ok)]">Drop .pak to install</span>
        </div>
      )}
      {/* Header */}
      <div className="flex min-h-8 items-center gap-3">
        <h2 className="shrink-0 text-xl font-bold">Mod Tools</h2>
        {notice && (
          <span
            className={cn(
              "flex min-w-0 items-center gap-1.5 truncate text-[12px] font-medium",
              notice.type === "ok"
                ? "text-[var(--color-ok)]"
                : notice.type === "err"
                  ? "text-[var(--color-err)]"
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
          <span className="flex shrink-0 items-center gap-1.5 text-[12px] font-medium text-[var(--color-warn)]">
            <XCircle size={14} strokeWidth={2.5} />
            Set game root on Home tab first
          </span>
        )}
        <Button
          variant="ghost"
          size="sm"
          onClick={() => refresh()}
          disabled={!gamePath}
          className="ml-auto shrink-0"
        >
          <RefreshCw size={14} />
          Refresh
        </Button>
      </div>

      {/* Status cards */}
      <div className="grid gap-3 sm:grid-cols-3">
        <StatusCard
          label="~mods Folder"
          ok={modsStatus?.mods_folder_exists ?? false}
          loading={!modsStatus}
          okText="Exists"
          failText="Missing"
          okIcon={<FolderOpen size={14} />}
          failIcon={<FolderOpen size={14} />}
        />
        <StatusCard
          label="Signature Bypass"
          ok={modsStatus?.sig_bypass_installed ?? false}
          loading={!modsStatus}
          okText="dsound.dll present"
          failText="Not installed"
          okIcon={<ShieldCheck size={14} />}
          failIcon={<ShieldX size={14} />}
        />
        <Card className="flex flex-col gap-1 p-4 bg-card">
          <span className="text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
            Active Mods
          </span>
          <span className="text-2xl font-bold">{modsStatus ? enabledCount : "—"}</span>
          <span className="text-[11px] text-muted-foreground">
            {modsStatus ? `of ${totalCount} installed` : "pak files in ~mods"}
          </span>
        </Card>
      </div>

      {/* Actions */}
      <Card className="flex flex-col gap-4 p-4 bg-card">
        <div>
          <h3 className="text-sm font-semibold">Signature Bypass</h3>
          <p className="mt-1 text-[12px] text-muted-foreground">
            Installs <Code>dsound.dll</Code> (ASI loader) and the bypass plugin into{" "}
            <Code>MarvelGame\Marvel\Binaries\Win64</Code>, and creates the <Code>~mods</Code> folder
            so the game loads unsigned pak files.
          </p>
        </div>
        <div className="flex flex-wrap gap-2">
          <Button variant="green" size="sm" onClick={installBypass} disabled={!gamePath}>
            <Shield size={14} />
            Install Bypass
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={removeBypass}
            disabled={!gamePath || !modsStatus?.sig_bypass_installed}
          >
            <ShieldOff size={14} />
            Remove Bypass
          </Button>
        </div>
      </Card>

      {/* Mod list */}
      {modsStatus && totalCount > 0 && (
        <Card className="flex min-h-0 flex-1 flex-col gap-4 p-4 bg-card">
          <div className="flex items-center justify-between gap-2">
            <div className="flex items-center gap-2">
              <h3 className="text-sm font-semibold">Installed Mods</h3>
              {totalCount > 0 && (
                <span className="rounded-full bg-secondary px-2 py-0.5 text-[11px] font-medium text-foreground">
                  {totalCount}
                </span>
              )}
            </div>
            <div className="flex items-center gap-2">
              <Button
                variant="ghost"
                size="sm"
                onClick={openFolder}
                disabled={!gamePath}
                title="Open ~mods folder in Explorer"
              >
                <FolderOpen size={14} />
                Open Folder
              </Button>
              <Button
                variant="blue"
                size="sm"
                onClick={exportZip}
                disabled={enabledCount === 0}
                title={
                  enabledCount === 0 ? "No enabled mods to export" : "Export enabled mods as zip"
                }
              >
                <Archive size={14} />
                Export Zip
              </Button>
            </div>
          </div>

          {totalCount === 0 ? (
            <p className="text-[12px] text-muted-foreground">
              No mods found in the ~mods folder. Copy <Code>.pak</Code> files there to get started.
            </p>
          ) : (
            <div className="min-h-0 flex-1 overflow-y-auto rounded-md border border-border bg-background [scrollbar-gutter:stable]">
              <ul>
                {modsStatus.mod_entries.map((entry) => {
                  const busy = busyMods.has(entry.full_name);
                  return (
                    <li
                      key={entry.full_name}
                      className="flex items-center gap-3 border-b border-border/50 px-3 py-2 last:border-none hover:bg-secondary/50"
                    >
                      <span
                        className={cn(
                          "flex-1 truncate font-mono text-[12px]",
                          !entry.enabled && "text-muted-foreground opacity-50"
                        )}
                      >
                        {entry.display_name}
                      </span>
                      {entry.has_companions && (
                        <span className="shrink-0 text-[10px] text-muted-foreground/50">
                          +ucas/utoc
                        </span>
                      )}
                      {pendingDelete === entry.full_name ? (
                        <div className="flex shrink-0 items-center gap-1">
                          <span className="text-[11px] font-medium text-[var(--color-err)]">
                            Delete?
                          </span>
                          <button
                            title="Confirm delete"
                            disabled={busy}
                            onClick={() => deleteMod(entry)}
                            className="rounded px-1.5 py-0.5 text-[11px] font-semibold bg-[var(--color-err)] text-white hover:opacity-90 transition-opacity"
                          >
                            Yes
                          </button>
                          <button
                            title="Cancel"
                            onClick={() => setPendingDelete(null)}
                            className="rounded px-1.5 py-0.5 text-[11px] font-semibold border border-border text-muted-foreground hover:bg-secondary transition-colors"
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
                            className="shrink-0 rounded p-1 text-muted-foreground/30 transition-colors hover:text-[var(--color-err)] hover:bg-[var(--color-err)]/10"
                          >
                            <Trash2 size={13} />
                          </button>
                        </>
                      )}
                    </li>
                  );
                })}
              </ul>
            </div>
          )}
        </Card>
      )}

      {/* How-to */}
      {(!modsStatus || totalCount === 0) && (
        <Card className="flex flex-col gap-3 p-4 bg-card">
          <h3 className="text-sm font-semibold">How to Install a Mod</h3>
          <ol className="flex flex-col gap-3 text-[12px] text-muted-foreground">
            {[
              <>
                Click <strong className="text-foreground">Install Bypass</strong> once. This places{" "}
                <Code>dsound.dll</Code> + <Code>plugins/bypass.asi</Code> in Binaries and creates
                the <Code>~mods</Code> folder.
              </>,
              <>
                Copy your mod <Code>.pak</Code> into the <Code>~mods</Code> folder. Rename it so it
                ends with <Code>_9999999_P.pak</Code> for correct load priority.
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
        </Card>
      )}
    </div>
  );
}

function StatusCard({
  label,
  ok,
  loading,
  okText,
  failText,
  okIcon,
  failIcon,
}: {
  label: string;
  ok: boolean;
  loading: boolean;
  okText: string;
  failText: string;
  okIcon: React.ReactNode;
  failIcon: React.ReactNode;
}) {
  return (
    <Card className="flex flex-col gap-1 p-4 bg-card">
      <span className="text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
        {label}
      </span>
      {loading ? (
        <span className="text-sm text-muted-foreground">—</span>
      ) : (
        <span
          className={cn(
            "flex items-center gap-1.5 text-sm font-medium",
            ok ? "text-[var(--color-ok)]" : "text-[var(--color-warn)]"
          )}
        >
          {ok ? okIcon : failIcon}
          {ok ? okText : failText}
        </span>
      )}
    </Card>
  );
}

function Code({ children }: { children: React.ReactNode }) {
  return (
    <code className="rounded bg-muted px-1 py-0.5 font-mono text-[11px] text-foreground">
      {children}
    </code>
  );
}
