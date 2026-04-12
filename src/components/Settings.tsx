import { useEffect, useRef, useState } from "react";

import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { CheckCircle2, FolderOpen, RefreshCw, Save, Undo2, XCircle } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import type { UpdateInfo } from "@/hooks/useUpdateCheck";
import { cn } from "@/lib/utils";

interface InstallInfo {
  path: string;
  source: string;
  launch_url: string;
}

interface Props {
  gamePath: string;
  setGamePath: (p: string) => void;
  installInfo: InstallInfo | null | undefined;
  detect: () => void;
  detecting: boolean;
  showDetectBadge: boolean;
  onManualUpdateFound: (info: UpdateInfo) => void;
}

export function Settings({
  gamePath,
  setGamePath,
  installInfo,
  detect,
  detecting,
  showDetectBadge,
  onManualUpdateFound,
}: Props) {
  const [draftGamePath, setDraftGamePath] = useState(gamePath);

  const [draftSkipLauncher, setDraftSkipLauncher] = useState<boolean | null>(null);
  const [savedSkipLauncher, setSavedSkipLauncher] = useState<boolean | null>(null);
  const [skipLauncherError, setSkipLauncherError] = useState<string | null>(null);

  const [draftAutoCheck, setDraftAutoCheck] = useState<boolean | null>(null);
  const [savedAutoCheck, setSavedAutoCheck] = useState<boolean | null>(null);

  const [saving, setSaving] = useState(false);
  const [pathError, setPathError] = useState<string | null>(null);
  const [savedBadge, setSavedBadge] = useState(false);
  const savedBadgeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const [updateChecking, setUpdateChecking] = useState(false);
  const [updateError, setUpdateError] = useState<string | null>(null);
  const [updateBadge, setUpdateBadge] = useState<{
    msg: string;
    type: "ok" | "info";
  } | null>(null);
  const updateBadgeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Sync draft when parent gamePath changes externally (e.g. detect, initial load)
  useEffect(() => {
    setDraftGamePath(gamePath);
    setPathError(null);
  }, [gamePath]);

  // Load skip-launcher whenever the draft path changes
  useEffect(() => {
    if (!draftGamePath) {
      setDraftSkipLauncher(null);
      setSavedSkipLauncher(null);
      setSkipLauncherError(null);
      return;
    }
    let cancelled = false;
    invoke<boolean>("get_skip_launcher", { gameRoot: draftGamePath })
      .then((v) => {
        if (cancelled) return;
        setDraftSkipLauncher(v);
        setSavedSkipLauncher(v);
        setSkipLauncherError(null);
      })
      .catch((e) => {
        if (cancelled) return;
        setDraftSkipLauncher(null);
        setSavedSkipLauncher(null);
        setSkipLauncherError(String(e));
        console.error(e);
      });
    return () => {
      cancelled = true;
    };
  }, [draftGamePath]);

  useEffect(() => {
    invoke<boolean>("get_auto_check_updates")
      .then((v) => {
        setDraftAutoCheck(v);
        setSavedAutoCheck(v);
      })
      .catch((e) => {
        console.error(e);
        setDraftAutoCheck(true);
        setSavedAutoCheck(true);
      });
  }, []);

  async function browse() {
    const selected = await open({ directory: true, multiple: false });
    if (typeof selected === "string") {
      setDraftGamePath(selected);
      setPathError(null);
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
      if (info.update_available) {
        onManualUpdateFound(info);
      } else {
        setUpdateBadge({ msg: "Up to date", type: "ok" });
        updateBadgeTimer.current = setTimeout(() => setUpdateBadge(null), 8000);
      }
    } catch (e) {
      setUpdateError(String(e));
      console.error(e);
    } finally {
      setUpdateChecking(false);
    }
  }

  const pathDirty = draftGamePath !== gamePath;
  const skipDirty =
    draftSkipLauncher !== null &&
    savedSkipLauncher !== null &&
    draftSkipLauncher !== savedSkipLauncher;
  const autoCheckDirty =
    draftAutoCheck !== null && savedAutoCheck !== null && draftAutoCheck !== savedAutoCheck;
  const dirty = pathDirty || skipDirty || autoCheckDirty;

  async function save() {
    setSaving(true);
    setPathError(null);
    try {
      if (pathDirty && draftGamePath) {
        const valid = await invoke<boolean>("validate_game_path", { path: draftGamePath });
        if (!valid) {
          setPathError("No Marvel Rivals install found at this path.");
          setSaving(false);
          return;
        }
      }
      if (pathDirty) {
        setGamePath(draftGamePath);
      }
      if (skipDirty && draftGamePath && draftSkipLauncher !== null) {
        try {
          await invoke("set_skip_launcher", {
            gameRoot: draftGamePath,
            skip: draftSkipLauncher,
          });
          setSavedSkipLauncher(draftSkipLauncher);
          setSkipLauncherError(null);
        } catch (e) {
          setSkipLauncherError(String(e));
          console.error(e);
        }
      }
      if (autoCheckDirty && draftAutoCheck !== null) {
        try {
          await invoke("set_auto_check_updates", { enabled: draftAutoCheck });
          setSavedAutoCheck(draftAutoCheck);
        } catch (e) {
          console.error(e);
        }
      }
      if (savedBadgeTimer.current) clearTimeout(savedBadgeTimer.current);
      setSavedBadge(true);
      savedBadgeTimer.current = setTimeout(() => setSavedBadge(false), 2500);
    } finally {
      setSaving(false);
    }
  }

  function discard() {
    setDraftGamePath(gamePath);
    setDraftSkipLauncher(savedSkipLauncher);
    setDraftAutoCheck(savedAutoCheck);
    setPathError(null);
  }

  return (
    <div className="flex flex-1 min-h-0 w-full flex-col">
      <div className="flex flex-1 min-h-0 flex-col gap-4 overflow-y-auto">
        {/* ── Header ── */}
        <div className="flex min-h-8 items-center gap-3">
          <h2 className="text-xl font-bold">Settings</h2>
        </div>

        {/* ── Game Root ── */}
        <Card className="flex flex-col gap-3 bg-card p-3">
          <div className="flex items-center gap-2">
            <h3 className="text-sm font-semibold">Game Root</h3>
            {pathError && (
              <span className="flex items-center gap-1.5 text-[11px] font-medium text-err">
                <XCircle size={13} strokeWidth={2.5} />
                {pathError}
              </span>
            )}
            {!pathError && showDetectBadge && installInfo && (
              <span className="flex items-center gap-1.5 text-[11px] font-medium text-ok">
                <CheckCircle2 size={13} strokeWidth={2.5} />
                Found via {installInfo.source}
              </span>
            )}
            {!pathError && !showDetectBadge && installInfo === null && (
              <span className="flex items-center gap-1.5 text-[11px] font-medium text-warn">
                <XCircle size={13} strokeWidth={2.5} />
                Not detected
              </span>
            )}
          </div>
          <div className="flex flex-col gap-1.5">
            <label className="text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
              Install Path
            </label>
            <div className="flex gap-2">
              <Input
                value={draftGamePath}
                onChange={(e) => {
                  setDraftGamePath(e.target.value);
                  setPathError(null);
                }}
                placeholder={`e.g. C:\\Program Files (x86)\\Steam\\steamapps\\common\\MarvelRivals`}
                title={draftGamePath}
                className="flex-1 font-mono text-xs"
              />
              <Button variant="outline" onClick={browse} className="shrink-0">
                <FolderOpen size={15} />
                Browse
              </Button>
              <Button
                onClick={() => detect()}
                disabled={detecting}
                variant="blue"
                className="shrink-0"
              >
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
                  skipLauncherError && "text-err"
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
              checked={draftSkipLauncher ?? false}
              onCheckedChange={(v) => setDraftSkipLauncher(v)}
              disabled={!draftGamePath || draftSkipLauncher === null}
            />
          </div>
          {!draftGamePath && (
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
                  updateBadge.type === "info" ? "text-blue-400" : "text-ok"
                )}
              >
                <CheckCircle2 size={13} strokeWidth={2.5} />
                {updateBadge.msg}
              </span>
            )}
            {updateError && (
              <span className="flex items-center gap-1 text-[12px] font-medium text-err">
                <XCircle size={13} strokeWidth={2.5} />
                {updateError}
              </span>
            )}
          </div>

          <div className="flex items-center justify-between rounded-md border border-border bg-background px-3 py-2">
            <div className="flex flex-col gap-0.5">
              <Label
                htmlFor="auto-check-updates"
                className="cursor-pointer text-[13px] font-medium"
              >
                Check for updates on startup
              </Label>
              <span className="text-[11px] text-muted-foreground">
                Automatically check GitHub for new releases when the app launches.
              </span>
            </div>
            <Switch
              id="auto-check-updates"
              checked={draftAutoCheck ?? false}
              onCheckedChange={(v) => setDraftAutoCheck(v)}
              disabled={draftAutoCheck === null}
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

      {/* ── Save bar ── */}
      <div className="mt-3 flex shrink-0 items-center justify-end gap-2 border-t border-border pt-3">
        {savedBadge && !dirty && (
          <span className="mr-auto flex items-center gap-1.5 text-[12px] font-medium text-ok">
            <CheckCircle2 size={13} strokeWidth={2.5} />
            Saved
          </span>
        )}
        {dirty && (
          <span className="mr-auto flex items-center gap-1.5 text-[12px] font-medium text-warn">
            <span className="relative flex h-2 w-2">
              <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-warn opacity-60" />
              <span className="relative inline-flex h-2 w-2 rounded-full bg-warn" />
            </span>
            Unsaved changes
          </span>
        )}
        <Button variant="outline" onClick={discard} disabled={!dirty || saving}>
          <Undo2 size={14} />
          Discard
        </Button>
        <Button variant="blue" onClick={save} disabled={!dirty || saving}>
          <Save size={14} />
          Save
        </Button>
      </div>
    </div>
  );
}
