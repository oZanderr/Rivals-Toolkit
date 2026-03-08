import { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import {
  FolderOpen,
  RefreshCw,
  CheckCircle2,
  XCircle,
  Wrench,
  ArrowRight,
  Settings,
  Package,
  Trash2,
  Play,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Card } from "@/components/ui/card";
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
  mod_paks: string[];
}

type Tab = "home" | "mod-tools" | "pak-manager" | "settings";

interface Props {
  gamePath: string;
  setGamePath: (p: string) => void;
  setActiveTab: (tab: Tab) => void;
  installInfo: InstallInfo | null | undefined;
  setInstallInfo: (info: InstallInfo | null) => void;
}

export function Home({ gamePath, setGamePath, setActiveTab, installInfo: info, setInstallInfo: setInfo }: Props) {
  const [detecting, setDetecting] = useState(false);
  const [showBadge, setShowBadge] = useState(false);
  const [modsStatus, setModsStatus] = useState<ModsStatus | null>(null);
  const [statusRefreshing, setStatusRefreshing] = useState(false);
  const [activeTweaks, setActiveTweaks] = useState<number | "missing" | null>(null);
  const [clearingCache, setClearingCache] = useState(false);
  const [cacheMsg, setCacheMsg] = useState<string | null>(null);
  const cacheMsgTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const badgeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [launching, setLaunching] = useState(false);
  const [launchMsg, setLaunchMsg] = useState<string | null>(null);
  const launchMsgTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  async function detect() {
    if (badgeTimer.current) clearTimeout(badgeTimer.current);
    setDetecting(true);
    setShowBadge(false);
    try {
      const result = await invoke<InstallInfo | null>("detect_install_path");
      setInfo(result);
      if (result) {
        setGamePath(result.path);
        setShowBadge(true);
        badgeTimer.current = setTimeout(() => setShowBadge(false), 4000);
      }
    } catch (e) {
      console.error(e);
    } finally {
      setDetecting(false);
    }
  }

  async function browse() {
    const selected = await open({ directory: true, multiple: false });
    if (typeof selected === "string") setGamePath(selected);
  }

  async function launchGame() {
    if (!info?.source) return;
    if (launchMsgTimer.current) clearTimeout(launchMsgTimer.current);
    setLaunching(true);
    setLaunchMsg(null);
    try {
      await invoke("launch_game", { installInfo: info });
      setLaunchMsg("Launching…");
      launchMsgTimer.current = setTimeout(() => setLaunchMsg(null), 4000);
    } catch (e) {
      setLaunchMsg(String(e));
      launchMsgTimer.current = setTimeout(() => setLaunchMsg(null), 6000);
      console.error(e);
    } finally {
      setLaunching(false);
    }
  }

  async function clearShaderCache() {
    if (cacheMsgTimer.current) clearTimeout(cacheMsgTimer.current);
    setClearingCache(true);
    setCacheMsg(null);
    try {
      const msg = await invoke<string>("clear_shader_cache");
      setCacheMsg(msg);
      cacheMsgTimer.current = setTimeout(() => setCacheMsg(null), 4000);
    } catch (e) {
      setCacheMsg("Failed to clear shader cache");
      cacheMsgTimer.current = setTimeout(() => setCacheMsg(null), 4000);
      console.error(e);
    } finally {
      setClearingCache(false);
    }
  }

  async function refreshModsStatus(path: string) {
    setStatusRefreshing(true);
    try {
      const s = await invoke<ModsStatus>("get_mods_status", { gameRoot: path });
      setModsStatus(s);
    } catch (e) {
      console.error(e);
    } finally {
      setStatusRefreshing(false);
    }
  }

  async function fetchActiveTweaks() {
    try {
      const path = await invoke<string>("get_scalability_path");
      let content: string;
      try {
        content = await invoke<string>("read_scalability", { path });
      } catch {
        // File doesn't exist yet — normal for a fresh install
        setActiveTweaks("missing");
        return;
      }
      const states = await invoke<{ id: string; active: boolean }[]>("detect_tweaks", { content });
      setActiveTweaks(states.filter((s) => s.active).length);
    } catch {
      setActiveTweaks(null);
    }
  }

  useEffect(() => {
    if (!gamePath) detect();
    fetchActiveTweaks();
  }, []);

  useEffect(() => {
    if (gamePath) refreshModsStatus(gamePath);
    else setModsStatus(null);
  }, [gamePath]);

  const allDone = !!modsStatus && modsStatus.mods_folder_exists && modsStatus.sig_bypass_installed;

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
    <div className="flex w-full flex-col gap-6">
      {/* ── Game path ── */}
      <div className="flex items-center gap-3">
        <h2 className="text-xl font-bold">Installation</h2>
        {showBadge && info && (
          <span className="flex items-center gap-1.5 text-[12px] font-medium text-[var(--color-ok)]">
            <CheckCircle2 size={14} strokeWidth={2.5} />
            Found via {info.source}
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
        <Card className="flex flex-col gap-4 bg-card p-4">
          <div className="flex items-center justify-between">
            <h3 className="text-sm font-semibold">Mod Setup</h3>
            <Button
              variant="ghost"
              size="sm"
              onClick={() => refreshModsStatus(gamePath)}
              disabled={statusRefreshing}
            >
              <RefreshCw size={13} className={cn(statusRefreshing && "animate-spin")} />
              Refresh
            </Button>
          </div>

          {/* Checklist */}
          <ol className="flex flex-col gap-2">
            {setupSteps.map((step, i) => (
              <li key={i} className="flex items-start gap-2.5">
                <span
                  className={cn(
                    "flex size-5 shrink-0 items-center justify-center rounded-full text-[10px] font-bold",
                    step.done
                      ? "bg-[var(--color-ok)]/15 text-[var(--color-ok)]"
                      : "bg-[var(--color-warn)]/15 text-[var(--color-warn)]",
                  )}
                >
                  {step.done ? "✓" : i + 1}
                </span>
                <div className="flex flex-col gap-0.5">
                  <span
                    className={cn(
                      "text-[13px] font-medium",
                      step.done ? "text-foreground" : "text-muted-foreground",
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
          <div className="flex items-center justify-between rounded-md border border-border bg-background px-4 py-3">
            <div className="flex items-center gap-2 text-[13px] text-muted-foreground">
              <Wrench size={14} />
              {allDone
                ? "All set — manage mods and bypass on the Mod Tools page."
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
          stat={modsStatus ? `${modsStatus.mod_paks.length} active mod${modsStatus.mod_paks.length !== 1 ? "s" : ""}` : undefined}
          onClick={() => setActiveTab("mod-tools")}
        />
        <FeatureCard
          icon={<Settings size={16} />}
          title="Quick Settings"
          description="Tweak graphics scalability and PAK-based config settings."
          stat={
            activeTweaks === null ? undefined
            : activeTweaks === "missing" ? "no file yet"
            : `${activeTweaks} tweak${activeTweaks !== 1 ? "s" : ""} active`
          }
          statDim={activeTweaks === "missing"}
          onClick={() => setActiveTab("settings")}
        />
        <FeatureCard
          icon={<Package size={16} />}
          title="PAK Manager"
          description="Inspect game and mod PAK files, extract assets, and repack folders."
          onClick={() => setActiveTab("pak-manager")}
        />
      </div>

      {/* ── Quick Actions ── */}
      <div className="flex flex-col gap-2">
        <span className="text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
          Quick Actions
        </span>
        <div className="flex flex-wrap gap-2">
          {info?.source && (
            <Button
              variant="green"
              size="sm"
              onClick={launchGame}
              disabled={launching}
              title={`Launch via ${info.source}`}
            >
              {launching ? (
                <RefreshCw size={14} className="animate-spin" />
              ) : (
                <Play size={14} />
              )}
              Launch Game
            </Button>
          )}
          <Button
            variant="outline"
            size="sm"
            onClick={clearShaderCache}
            disabled={clearingCache}
            title="Deletes pipeline cache files from %LOCALAPPDATA%\Marvel\Saved"
          >
            {clearingCache ? (
              <RefreshCw size={14} className="animate-spin" />
            ) : (
              <Trash2 size={14} />
            )}
            Clear Shader Cache
          </Button>
          {launchMsg && (
            <span className="flex items-center gap-1.5 text-[12px] font-medium text-[var(--color-ok)]">
              <CheckCircle2 size={14} strokeWidth={2.5} />
              {launchMsg}
            </span>
          )}
          {cacheMsg && (
            <span className="flex items-center gap-1.5 text-[12px] font-medium text-[var(--color-ok)]">
              <CheckCircle2 size={14} strokeWidth={2.5} />
              {cacheMsg}
            </span>
          )}
        </div>
      </div>
    </div>
  );
}

function FeatureCard({
  icon,
  title,
  description,
  stat,
  statDim,
  onClick,
}: {
  icon: React.ReactNode;
  title: string;
  description: string;
  stat?: string;
  statDim?: boolean;
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
        <span
          className={cn(
            "text-[11px] font-medium",
            statDim ? "text-muted-foreground" : "text-[var(--color-ok)]",
          )}
        >
          {stat}
        </span>
      )}
    </Card>
  );
}

