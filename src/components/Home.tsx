import { useState, useEffect, useRef, useCallback, type ReactNode } from "react";

import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { openPath } from "@tauri-apps/plugin-opener";
import {
  FolderOpen,
  RefreshCw,
  CheckCircle2,
  XCircle,
  Wrench,
  Settings,
  Package,
  ArrowRight,
} from "lucide-react";

import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";

interface InstallInfo {
  path: string;
  source: string;
  launch_url: string;
}

interface ModsStatus {
  mods_folder_exists: boolean;
  mods_folder_path: string;
  sig_bypass_installed: boolean;
  mod_entries: { enabled: boolean }[];
}

type Tab = "home" | "mod-tools" | "pak-manager" | "settings";

interface Props {
  gamePath: string;
  setGamePath: (p: string) => void;
  setActiveTab: (tab: Tab) => void;
  installInfo: InstallInfo | null | undefined;
  setInstallInfo: (info: InstallInfo | null) => void;
  isActive: boolean;
}

export function Home({
  gamePath,
  setGamePath,
  setActiveTab,
  installInfo: info,
  setInstallInfo: setInfo,
  isActive,
}: Props) {
  const [folderNotice, setFolderNotice] = useState<{ msg: string; type: "ok" | "err" } | null>(
    null
  );
  const folderNoticeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [detecting, setDetecting] = useState(false);
  const [showBadge, setShowBadge] = useState(false);
  const [detectError, setDetectError] = useState<string | null>(null);
  const detectErrTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [modsStatus, setModsStatus] = useState<ModsStatus | null>(null);
  const [statusRefreshing, setStatusRefreshing] = useState(false);
  const [showRefreshBadge, setShowRefreshBadge] = useState(false);
  const refreshBadgeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const badgeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const detect = useCallback(async () => {
    if (badgeTimer.current) clearTimeout(badgeTimer.current);
    if (detectErrTimer.current) clearTimeout(detectErrTimer.current);
    setDetecting(true);
    setShowBadge(false);
    setDetectError(null);
    try {
      const result = await invoke<InstallInfo | null>("detect_install_path");
      setInfo(result);
      if (result) {
        setGamePath(result.path);
        setShowBadge(true);
        badgeTimer.current = setTimeout(() => setShowBadge(false), 4000);
      }
    } catch (e) {
      setDetectError("Detection failed");
      detectErrTimer.current = setTimeout(() => setDetectError(null), 6000);
      console.error(e);
    } finally {
      setDetecting(false);
    }
  }, [setInfo, setGamePath]);

  async function browse() {
    const selected = await open({ directory: true, multiple: false });
    if (typeof selected === "string") setGamePath(selected);
  }

  const refreshModsStatus = useCallback(async (path: string, showBadge = false) => {
    setStatusRefreshing(true);
    try {
      const s = await invoke<ModsStatus>("get_mods_status", { gameRoot: path });
      setModsStatus(s);
      if (showBadge) {
        if (refreshBadgeTimer.current) clearTimeout(refreshBadgeTimer.current);
        setShowRefreshBadge(true);
        refreshBadgeTimer.current = setTimeout(() => setShowRefreshBadge(false), 4000);
      }
    } catch (e) {
      console.error(e);
    } finally {
      setStatusRefreshing(false);
    }
  }, []);

  function showFolderNotice(msg: string, type: "ok" | "err") {
    if (folderNoticeTimer.current) clearTimeout(folderNoticeTimer.current);
    setFolderNotice({ msg, type });
    folderNoticeTimer.current = setTimeout(() => setFolderNotice(null), 4000);
  }

  async function openShortcut(target: string, label: string) {
    if (!target) return;
    try {
      await openPath(target);
      showFolderNotice(`${label} opened`, "ok");
    } catch (e) {
      showFolderNotice(`Failed to open ${label.toLowerCase()}`, "err");
      console.error(e);
    }
  }

  async function openScalabilityFolder() {
    try {
      const scalabilityPath = await invoke<string>("get_scalability_path");
      const folderPath = scalabilityPath.replace(/[/\\][^/\\]+$/, "");
      await openPath(folderPath);
      showFolderNotice("Scalability folder opened", "ok");
    } catch (e) {
      showFolderNotice("Failed to open scalability folder", "err");
      console.error(e);
    }
  }

  useEffect(() => {
    if (!gamePath) detect();
    // eslint-disable-next-line react-hooks/exhaustive-deps -- intentional mount-only: auto-detect only on first load
  }, []);

  useEffect(() => {
    if (isActive && gamePath) refreshModsStatus(gamePath);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- gamePath intentionally omitted: separate effect handles gamePath changes
  }, [isActive, refreshModsStatus]);

  useEffect(() => {
    if (gamePath) refreshModsStatus(gamePath);
    else setModsStatus(null);
  }, [gamePath, refreshModsStatus]);

  const allDone = !!modsStatus && modsStatus.mods_folder_exists && modsStatus.sig_bypass_installed;
  const enabledModsCount = modsStatus?.mod_entries.filter((m) => m.enabled).length ?? 0;
  const paksPath = gamePath ? `${gamePath}\\MarvelGame\\Marvel\\Content\\Paks` : "";
  const modsPath = modsStatus?.mods_folder_path ?? (paksPath ? `${paksPath}\\~mods` : "");
  const binariesPath = gamePath ? `${gamePath}\\MarvelGame\\Marvel\\Binaries\\Win64` : "";

  const setupSteps: { label: string; done: boolean; description: string }[] = modsStatus
    ? [
        {
          label: "Game path set",
          done: !!gamePath,
          description: "Game root is configured above.",
        },
        {
          label: "Mods folder exists",
          done: modsStatus.mods_folder_exists,
          description: "~mods folder inside the Paks directory.",
        },
        {
          label: "Signature bypass installed",
          done: modsStatus.sig_bypass_installed,
          description: "dsound.dll and bypass plugin in Binaries\\Win64.",
        },
      ]
    : [];

  return (
    <div className="flex flex-1 min-h-0 w-full flex-col gap-4 overflow-y-auto">
      {/* ── Game path ── */}
      <div className="flex min-h-8 items-center gap-3">
        <h2 className="text-xl font-bold">Installation</h2>
        {showBadge && info && (
          <span className="flex items-center gap-1.5 text-[12px] font-medium text-[var(--color-ok)]">
            <CheckCircle2 size={14} strokeWidth={2.5} />
            Found via {info.source}
          </span>
        )}
        {!showBadge && detectError && (
          <span className="flex items-center gap-1.5 text-[12px] font-medium text-[var(--color-err)]">
            <XCircle size={14} strokeWidth={2.5} />
            {detectError}
          </span>
        )}
        {info === null && (
          <span className="flex items-center gap-1.5 text-[12px] font-medium text-[var(--color-warn)]">
            <XCircle size={14} strokeWidth={2.5} />
            Not detected
          </span>
        )}
      </div>

      <div className="flex flex-col gap-1.5">
        <label className="text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
          Game Root
        </label>
        <div className="flex gap-2">
          <Input
            value={gamePath}
            onChange={(e) => setGamePath(e.target.value)}
            placeholder={`e.g. C:\\Program Files (x86)\\Steam\\steamapps\\common\\MarvelRivals`}
            title={gamePath}
            className="flex-1 font-mono text-xs"
          />
          <Button variant="outline" onClick={browse} className="shrink-0">
            <FolderOpen size={15} />
            Browse
          </Button>
          <Button onClick={detect} disabled={detecting} variant="blue" className="shrink-0">
            <RefreshCw size={15} className={cn(detecting && "animate-spin")} />
            Redetect
          </Button>
        </div>
      </div>

      {/* ── Mod setup status ── */}
      {gamePath && (
        <Card className="flex flex-col gap-3 bg-card p-3">
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-2">
              <h3 className="text-sm font-semibold">Mod Setup</h3>
              {showRefreshBadge && (
                <span className="flex items-center gap-1 text-[12px] font-medium text-[var(--color-ok)]">
                  <CheckCircle2 size={13} strokeWidth={2.5} />
                  Status refreshed
                </span>
              )}
            </div>
            <Button
              variant="ghost"
              size="sm"
              onClick={() => refreshModsStatus(gamePath, true)}
              disabled={statusRefreshing}
            >
              <RefreshCw size={13} className={cn(statusRefreshing && "animate-spin")} />
              Refresh
            </Button>
          </div>

          {/* Checklist */}
          <ol className="flex flex-col gap-1.5">
            {setupSteps.map((step, i) => (
              <li key={i} className="flex items-start gap-2.5">
                <span
                  className={cn(
                    "flex size-5 shrink-0 items-center justify-center rounded-full text-[10px] font-bold",
                    step.done
                      ? "bg-[var(--color-ok)]/15 text-[var(--color-ok)]"
                      : "bg-[var(--color-warn)]/15 text-[var(--color-warn)]"
                  )}
                >
                  {step.done ? "✓" : i + 1}
                </span>
                <div className="flex flex-col gap-0.5">
                  <span
                    className={cn(
                      "text-[13px] font-medium",
                      step.done ? "text-foreground" : "text-muted-foreground"
                    )}
                  >
                    {step.label}
                  </span>
                  <span className="text-[11px] text-muted-foreground">{step.description}</span>
                </div>
              </li>
            ))}
          </ol>

          {/* Navigate to Mod Tools */}
          <div className="flex items-center justify-between rounded-md border border-border bg-background px-3 py-2">
            <div className="flex items-center gap-2 text-[13px] text-muted-foreground">
              <Wrench size={14} />
              {allDone
                ? "All done! Manage mods and signature bypass on the Mod Tools page."
                : "Complete setup and manage mods on the Mod Tools page."}
            </div>
            <Button variant="outline" size="sm" onClick={() => setActiveTab("mod-tools")}>
              Mod Tools
              <ArrowRight size={13} />
            </Button>
          </div>
        </Card>
      )}

      {/* ── Feature nav cards ── */}
      <div className="grid gap-3 sm:grid-cols-3">
        <FeatureCard
          icon={<Wrench size={16} />}
          title="Mod Tools"
          description="Install the signature bypass and manage your mods folder."
          stat={
            enabledModsCount > 0
              ? `${enabledModsCount} active mod${enabledModsCount !== 1 ? "s" : ""}`
              : undefined
          }
          onClick={() => setActiveTab("mod-tools")}
        />
        <FeatureCard
          icon={<Settings size={16} />}
          title="Quick Settings"
          description="Tweak graphics scalability and pak-based config settings."
          onClick={() => setActiveTab("settings")}
        />
        <FeatureCard
          icon={<Package size={16} />}
          title="Pak Manager"
          description="Inspect game and mod pak files, extract assets, and repack folders."
          onClick={() => setActiveTab("pak-manager")}
        />
      </div>

      {/* ── Quick folders ── */}
      <Card className="flex flex-col gap-3 bg-card p-3">
        <div className="flex items-center justify-between gap-2">
          <h3 className="text-sm font-semibold">Quick Folders</h3>
          {folderNotice && (
            <span
              className={cn(
                "flex items-center gap-1 text-[12px] font-medium",
                folderNotice.type === "ok" ? "text-[var(--color-ok)]" : "text-[var(--color-err)]"
              )}
            >
              {folderNotice.type === "ok" ? (
                <CheckCircle2 size={13} strokeWidth={2.5} />
              ) : (
                <XCircle size={13} strokeWidth={2.5} />
              )}
              {folderNotice.msg}
            </span>
          )}
        </div>

        <div className="grid gap-2 sm:grid-cols-2 xl:grid-cols-3">
          <FolderShortcutCard
            title="Game Folder"
            description="Your selected Marvel Rivals root directory."
            onClick={() => openShortcut(gamePath, "Game folder")}
            disabled={!gamePath}
          />
          <FolderShortcutCard
            title="Binaries Folder"
            description="MarvelGame/Marvel/Binaries/Win64"
            onClick={() => openShortcut(binariesPath, "Binaries folder")}
            disabled={!binariesPath}
          />
          <FolderShortcutCard
            title="Mods Folder"
            description="MarvelGame/Marvel/Content/Paks/~mods"
            onClick={() => openShortcut(modsPath, "Mods folder")}
            disabled={!modsPath}
          />
          <FolderShortcutCard
            title="Scalability Folder"
            description="%LOCALAPPDATA%/Marvel/Saved/Config/Windows"
            onClick={openScalabilityFolder}
          />
        </div>
      </Card>
    </div>
  );
}

function FolderShortcutCard({
  title,
  description,
  onClick,
  disabled,
}: {
  title: string;
  description: string;
  onClick: () => void;
  disabled?: boolean;
}) {
  return (
    <button
      onClick={onClick}
      disabled={disabled}
      className={cn(
        "flex items-center justify-between rounded-md border border-border bg-background px-3 py-2 text-left transition-colors",
        disabled ? "cursor-not-allowed opacity-50" : "hover:bg-secondary/60"
      )}
      title={disabled ? "Set game path first" : `Open ${title}`}
    >
      <span className="flex min-w-0 flex-col">
        <span className="text-[13px] font-medium">{title}</span>
        <span className="truncate text-[11px] text-muted-foreground">{description}</span>
      </span>
      <FolderOpen size={14} className="shrink-0 text-muted-foreground" />
    </button>
  );
}

function FeatureCard({
  icon,
  title,
  description,
  stat,
  onClick,
}: {
  icon: ReactNode;
  title: string;
  description: string;
  stat?: string;
  onClick: () => void;
}) {
  return (
    <Card
      className="flex cursor-pointer flex-col gap-3 bg-card p-4 transition-colors hover:bg-secondary/50"
      onClick={onClick}
    >
      <div className="flex items-center justify-between">
        <span className="flex items-center gap-2 text-sm font-semibold">
          <span className="text-muted-foreground">{icon}</span>
          {title}
        </span>
        <ArrowRight size={13} className="text-muted-foreground" />
      </div>
      <p className="text-[11px] leading-relaxed text-muted-foreground">{description}</p>
      {stat !== undefined && (
        <span className="text-[11px] font-medium text-[var(--color-ok)]">{stat}</span>
      )}
    </Card>
  );
}
