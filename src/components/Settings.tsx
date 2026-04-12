import { useCallback, useEffect, useRef, useState } from "react";

import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { openPath } from "@tauri-apps/plugin-opener";
import { CheckCircle2, ExternalLink, FolderOpen, RefreshCw, XCircle } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";

interface InstallInfo {
  path: string;
  source: string;
  launch_url: string;
}

interface UpdateInfo {
  update_available: boolean;
  latest_version: string;
  current_version: string;
  release_url: string;
  release_notes: string | null;
}

interface Props {
  gamePath: string;
  setGamePath: (p: string) => void;
  installInfo: InstallInfo | null | undefined;
  detect: () => void;
  detecting: boolean;
  showDetectBadge: boolean;
}

export function Settings({
  gamePath,
  setGamePath,
  installInfo,
  detect,
  detecting,
  showDetectBadge,
}: Props) {
  const [skipLauncher, setSkipLauncher] = useState<boolean | null>(null);
  const [skipLauncherError, setSkipLauncherError] = useState<string | null>(null);

  const [autoCheckUpdates, setAutoCheckUpdates] = useState<boolean | null>(null);
  const [updateChecking, setUpdateChecking] = useState(false);
  const [updateError, setUpdateError] = useState<string | null>(null);
  const [updateBadge, setUpdateBadge] = useState<{
    msg: string;
    type: "ok" | "info";
    releaseUrl?: string;
  } | null>(null);
  const updateBadgeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  async function browse() {
    const selected = await open({ directory: true, multiple: false });
    if (typeof selected === "string") setGamePath(selected);
  }

  const loadLaunchRecord = useCallback(async (path: string) => {
    try {
      const skip = await invoke<boolean>("get_skip_launcher", { gameRoot: path });
      setSkipLauncher(skip);
      setSkipLauncherError(null);
    } catch (e) {
      setSkipLauncher(null);
      setSkipLauncherError(String(e));
      console.error(e);
    }
  }, []);

  async function handleSkipLauncherToggle(checked: boolean) {
    if (!gamePath) return;
    try {
      await invoke("set_skip_launcher", { gameRoot: gamePath, skip: checked });
      setSkipLauncher(checked);
      setSkipLauncherError(null);
    } catch (e) {
      setSkipLauncherError(String(e));
      console.error(e);
    }
  }

  async function handleAutoCheckToggle(checked: boolean) {
    try {
      await invoke("set_auto_check_updates", { enabled: checked });
      setAutoCheckUpdates(checked);
    } catch (e) {
      console.error(e);
    }
  }

  async function checkUpdateNow() {
    setUpdateChecking(true);
    setUpdateError(null);
    if (updateBadgeTimer.current) clearTimeout(updateBadgeTimer.current);
    setUpdateBadge(null);
    try {
      const current = await getVersion();
      const info = await invoke<UpdateInfo>("check_for_update", {
        currentVersion: current,
        force: true,
      });
      setUpdateBadge(
        info.update_available
          ? {
              msg: `Update available: v${info.latest_version}`,
              type: "info",
              releaseUrl: info.release_url,
            }
          : { msg: "Up to date", type: "ok" }
      );
      updateBadgeTimer.current = setTimeout(() => setUpdateBadge(null), 8000);
    } catch (e) {
      setUpdateError(String(e));
      console.error(e);
    } finally {
      setUpdateChecking(false);
    }
  }

  useEffect(() => {
    if (gamePath) {
      loadLaunchRecord(gamePath);
    } else {
      setSkipLauncher(null);
    }
  }, [gamePath, loadLaunchRecord]);

  useEffect(() => {
    invoke<boolean>("get_auto_check_updates")
      .then(setAutoCheckUpdates)
      .catch((e) => {
        console.error(e);
        setAutoCheckUpdates(true);
      });
  }, []);

  return (
    <div className="flex flex-1 min-h-0 w-full flex-col gap-4 overflow-y-auto">
      {/* ── Header ── */}
      <div className="flex min-h-8 items-center gap-3">
        <h2 className="text-xl font-bold">Settings</h2>
        {showDetectBadge && installInfo && (
          <span className="flex items-center gap-1.5 text-[12px] font-medium text-[var(--color-ok)]">
            <CheckCircle2 size={14} strokeWidth={2.5} />
            Found via {installInfo.source}
          </span>
        )}
        {!showDetectBadge && installInfo === null && (
          <span className="flex items-center gap-1.5 text-[12px] font-medium text-[var(--color-warn)]">
            <XCircle size={14} strokeWidth={2.5} />
            Not detected
          </span>
        )}
      </div>

      {/* ── Game Root ── */}
      <Card className="flex flex-col gap-3 bg-card p-3">
        <h3 className="text-sm font-semibold">Game Root</h3>
        <div className="flex flex-col gap-1.5">
          <label className="text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
            Install Path
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
      </Card>

      {/* ── Launch Options ── */}
      <Card className="flex flex-col gap-3 bg-card p-3">
        <h3 className="text-sm font-semibold">Launch Options</h3>
        <div
          className="flex items-center justify-between rounded-md border border-border bg-background px-3 py-2"
          title={skipLauncherError ?? undefined}
        >
          <div className="flex flex-col gap-0.5">
            <Label
              htmlFor="skip-launcher"
              className={cn(
                "cursor-pointer text-[13px] font-medium",
                skipLauncherError && "text-[var(--color-err)]"
              )}
            >
              Skip Launcher
            </Label>
            <span className="text-[11px] text-muted-foreground">
              Skip the launcher window and go straight into the game.
            </span>
          </div>
          <Switch
            id="skip-launcher"
            checked={skipLauncher ?? false}
            onCheckedChange={handleSkipLauncherToggle}
            disabled={!gamePath || skipLauncher === null}
          />
        </div>
        {!gamePath && (
          <span className="text-[11px] text-muted-foreground">Set a game path first.</span>
        )}
      </Card>

      {/* ── Updates ── */}
      <Card className="flex flex-col gap-3 bg-card p-3">
        <div className="flex items-center justify-between">
          <h3 className="text-sm font-semibold">Updates</h3>
          {updateBadge && (
            <span
              className={cn(
                "flex items-center gap-1.5 text-[12px] font-medium",
                updateBadge.type === "info" ? "text-blue-400" : "text-[var(--color-ok)]"
              )}
            >
              <CheckCircle2 size={13} strokeWidth={2.5} />
              {updateBadge.msg}
              {updateBadge.releaseUrl && (
                <button
                  onClick={() => openPath(updateBadge.releaseUrl ?? "").catch(console.error)}
                  className="ml-1 inline-flex items-center gap-0.5 underline underline-offset-2"
                >
                  View
                  <ExternalLink size={11} />
                </button>
              )}
            </span>
          )}
          {updateError && (
            <span className="flex items-center gap-1 text-[12px] font-medium text-[var(--color-err)]">
              <XCircle size={13} strokeWidth={2.5} />
              {updateError}
            </span>
          )}
        </div>

        <div className="flex items-center justify-between rounded-md border border-border bg-background px-3 py-2">
          <div className="flex flex-col gap-0.5">
            <Label htmlFor="auto-check-updates" className="cursor-pointer text-[13px] font-medium">
              Check for updates on startup
            </Label>
            <span className="text-[11px] text-muted-foreground">
              Automatically check GitHub for new releases when the app launches.
            </span>
          </div>
          <Switch
            id="auto-check-updates"
            checked={autoCheckUpdates ?? false}
            onCheckedChange={handleAutoCheckToggle}
            disabled={autoCheckUpdates === null}
          />
        </div>

        <div className="flex items-center justify-between rounded-md border border-border bg-background px-3 py-2">
          <div className="flex flex-col gap-0.5">
            <span className="text-[13px] font-medium">Check for updates now</span>
            <span className="text-[11px] text-muted-foreground">
              Manually check GitHub for the latest release.
            </span>
          </div>
          <Button variant="blue" size="sm" onClick={checkUpdateNow} disabled={updateChecking}>
            <RefreshCw size={13} className={cn(updateChecking && "animate-spin")} />
            Check now
          </Button>
        </div>
      </Card>
    </div>
  );
}
