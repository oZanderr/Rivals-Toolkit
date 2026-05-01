import { useEffect, useRef, useState } from "react";

import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { open, save as saveDialog } from "@tauri-apps/plugin-dialog";
import {
  CheckCircle2,
  CloudDownload,
  Download,
  FolderOpen,
  Pencil,
  RefreshCw,
  Save,
  Search,
  ShieldOff,
  Trash2,
  Undo2,
  Upload,
  X,
  XCircle,
} from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { Tip } from "@/components/ui/tooltip";
import type { UpdateInfo } from "@/hooks/useUpdateCheck";
import { emitModsChanged } from "@/lib/modsEvents";
import { setShowHeroIcons } from "@/lib/showHeroIcons";
import { emitTweakProfilesChanged, onTweakProfilesChanged } from "@/lib/tweakProfileEvents";
import { cn } from "@/lib/utils";

interface TweakSetting {
  id: string;
  enabled: boolean;
  value: string | null;
}

interface TweakProfile {
  name: string;
  settings: TweakSetting[];
  created_at: number;
  modified_at: number;
}

interface InstallInfo {
  path: string;
  source: string;
  launch_url: string;
}

interface CharacterDataInfo {
  character_count: number;
  generated_at: string | null;
  origin: string;
  source: string | null;
  user_file_mtime: number | null;
  user_file_present: boolean;
}

interface SyncResult {
  character_count: number;
  generated_at: string | null;
  fetched_at: number;
  bytes: number;
  source_url: string;
}

function formatRelativeTime(unixSecs: number): string {
  const diff = Date.now() / 1000 - unixSecs;
  if (diff < 60) return "just now";
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
  return `${Math.floor(diff / 86400)}d ago`;
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

  const [draftRecursive, setDraftRecursive] = useState<boolean | null>(null);
  const [savedRecursive, setSavedRecursive] = useState<boolean | null>(null);
  const [draftAutoSyncHeroes, setDraftAutoSyncHeroes] = useState<boolean | null>(null);
  const [savedAutoSyncHeroes, setSavedAutoSyncHeroes] = useState<boolean | null>(null);
  const [draftShowHeroIcons, setDraftShowHeroIcons] = useState<boolean | null>(null);
  const [savedShowHeroIcons, setSavedShowHeroIcons] = useState<boolean | null>(null);

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

  const [bypassNotice, setBypassNotice] = useState<{
    msg: string;
    type: "ok" | "err";
  } | null>(null);
  const bypassNoticeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const [tweakProfiles, setTweakProfiles] = useState<TweakProfile[]>([]);
  const [profileNotice, setProfileNotice] = useState<{
    msg: string;
    type: "ok" | "err";
  } | null>(null);
  const profileNoticeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [renamingProfile, setRenamingProfile] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState("");

  const [characterDataInfo, setCharacterDataInfo] = useState<CharacterDataInfo | null>(null);
  const [syncingHeroes, setSyncingHeroes] = useState(false);
  const [syncHeroNotice, setSyncHeroNotice] = useState<{
    msg: string;
    type: "ok" | "err";
  } | null>(null);
  const syncHeroNoticeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

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

  useEffect(() => {
    invoke<boolean>("get_recursive_mod_scan")
      .then((v) => {
        setDraftRecursive(v);
        setSavedRecursive(v);
      })
      .catch((e) => {
        console.error(e);
        setDraftRecursive(true);
        setSavedRecursive(true);
      });
  }, []);

  useEffect(() => {
    invoke<boolean>("get_auto_sync_character_data")
      .then((v) => {
        setDraftAutoSyncHeroes(v);
        setSavedAutoSyncHeroes(v);
      })
      .catch((e) => {
        console.error(e);
        setDraftAutoSyncHeroes(true);
        setSavedAutoSyncHeroes(true);
      });
  }, []);

  useEffect(() => {
    invoke<boolean>("get_show_hero_icons")
      .then((v) => {
        setDraftShowHeroIcons(v);
        setSavedShowHeroIcons(v);
      })
      .catch((e) => {
        console.error(e);
        setDraftShowHeroIcons(true);
        setSavedShowHeroIcons(true);
      });
  }, []);

  useEffect(() => {
    invoke<CharacterDataInfo>("get_character_data_info")
      .then(setCharacterDataInfo)
      .catch(() => setCharacterDataInfo(null));
  }, []);

  const refreshTweakProfiles = async () => {
    try {
      const list = await invoke<TweakProfile[]>("list_tweak_profiles");
      setTweakProfiles(list);
    } catch {
      setTweakProfiles([]);
    }
  };

  useEffect(() => {
    refreshTweakProfiles();
    return onTweakProfilesChanged(refreshTweakProfiles);
  }, []);

  function showProfileNotice(msg: string, type: "ok" | "err") {
    if (profileNoticeTimer.current) clearTimeout(profileNoticeTimer.current);
    setProfileNotice({ msg, type });
    profileNoticeTimer.current = setTimeout(() => setProfileNotice(null), 6000);
  }

  async function exportTweakProfile(name: string) {
    try {
      const safeName = name.replace(/[<>:"/\\|?*]/g, "_").trim() || "preset";
      const target = await saveDialog({
        title: `Export preset "${name}"`,
        defaultPath: `${safeName}.preset.json`,
        filters: [{ name: "Config preset", extensions: ["preset.json", "json"] }],
      });
      if (!target) return;
      await invoke("export_tweak_profile_to_file", { name, path: target });
      showProfileNotice(`Exported "${name}"`, "ok");
    } catch (e) {
      showProfileNotice(String(e), "err");
    }
  }

  async function importTweakProfile() {
    try {
      const picked = await open({
        title: "Import config preset",
        multiple: false,
        filters: [{ name: "Config preset", extensions: ["preset.json", "json"] }],
      });
      if (typeof picked !== "string") return;

      let nameOverride: string | undefined;
      let originalName: string | null = null;
      for (let attempt = 0; attempt < 100; attempt++) {
        try {
          const profile = await invoke<TweakProfile>("import_tweak_profile_from_file", {
            path: picked,
            nameOverride,
          });
          await refreshTweakProfiles();
          emitTweakProfilesChanged();
          if (originalName && profile.name !== originalName) {
            showProfileNotice(
              `Imported as "${profile.name}" (renamed from "${originalName}")`,
              "ok"
            );
          } else {
            showProfileNotice(`Imported "${profile.name}"`, "ok");
          }
          return;
        } catch (e) {
          const msg = String(e);
          const match = msg.match(/Profile "(.+?)" already exists/);
          if (!match) throw e;
          if (originalName === null) originalName = match[1];
          nameOverride = `${originalName} (${attempt + 2})`;
        }
      }
      throw new Error("Could not find an available name for this preset");
    } catch (e) {
      showProfileNotice(String(e), "err");
    }
  }

  async function deleteTweakProfile(name: string) {
    try {
      await invoke("delete_tweak_profile", { name });
      await refreshTweakProfiles();
      emitTweakProfilesChanged();
      showProfileNotice(`Deleted "${name}"`, "ok");
    } catch (e) {
      showProfileNotice(String(e), "err");
    }
  }

  async function commitRename(oldName: string) {
    const trimmed = renameDraft.trim();
    if (!trimmed || trimmed === oldName) {
      setRenamingProfile(null);
      setRenameDraft("");
      return;
    }
    try {
      const profile = await invoke<TweakProfile>("rename_tweak_profile", {
        oldName,
        newName: trimmed,
      });
      setRenamingProfile(null);
      setRenameDraft("");
      await refreshTweakProfiles();
      emitTweakProfilesChanged();
      showProfileNotice(`Renamed to "${profile.name}"`, "ok");
    } catch (e) {
      showProfileNotice(String(e), "err");
    }
  }

  async function syncHeroesNow() {
    if (syncingHeroes) return;
    if (syncHeroNoticeTimer.current) clearTimeout(syncHeroNoticeTimer.current);
    setSyncHeroNotice(null);
    setSyncingHeroes(true);
    try {
      const result = await invoke<SyncResult>("sync_character_data");
      const info = await invoke<CharacterDataInfo>("get_character_data_info").catch(() => null);
      if (info) setCharacterDataInfo(info);
      setSyncHeroNotice({
        msg: `Synced ${result.character_count} characters`,
        type: "ok",
      });
    } catch (e: unknown) {
      setSyncHeroNotice({ msg: String(e), type: "err" });
    } finally {
      setSyncingHeroes(false);
      syncHeroNoticeTimer.current = setTimeout(() => setSyncHeroNotice(null), 6000);
    }
  }

  async function removeBypass() {
    if (bypassNoticeTimer.current) clearTimeout(bypassNoticeTimer.current);
    try {
      const msg = await invoke<string>("remove_signature_bypass", {
        gameRoot: draftGamePath,
      });
      setBypassNotice({ msg, type: "ok" });
    } catch (e: unknown) {
      setBypassNotice({ msg: String(e), type: "err" });
    }
    bypassNoticeTimer.current = setTimeout(() => setBypassNotice(null), 6000);
  }

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
  const recursiveDirty =
    draftRecursive !== null && savedRecursive !== null && draftRecursive !== savedRecursive;
  const autoSyncHeroesDirty =
    draftAutoSyncHeroes !== null &&
    savedAutoSyncHeroes !== null &&
    draftAutoSyncHeroes !== savedAutoSyncHeroes;
  const showHeroIconsDirty =
    draftShowHeroIcons !== null &&
    savedShowHeroIcons !== null &&
    draftShowHeroIcons !== savedShowHeroIcons;
  const dirty =
    pathDirty ||
    skipDirty ||
    autoCheckDirty ||
    recursiveDirty ||
    autoSyncHeroesDirty ||
    showHeroIconsDirty;

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
      if (recursiveDirty && draftRecursive !== null) {
        try {
          await invoke("set_recursive_mod_scan", { enabled: draftRecursive });
          setSavedRecursive(draftRecursive);
          if (draftGamePath) {
            emitModsChanged({
              modsFolder: `${draftGamePath}\\MarvelGame\\Marvel\\Content\\Paks\\~mods`,
              source: "Settings",
            });
          }
        } catch (e) {
          console.error(e);
        }
      }
      if (autoSyncHeroesDirty && draftAutoSyncHeroes !== null) {
        try {
          await invoke("set_auto_sync_character_data", { enabled: draftAutoSyncHeroes });
          setSavedAutoSyncHeroes(draftAutoSyncHeroes);
        } catch (e) {
          console.error(e);
        }
      }
      if (showHeroIconsDirty && draftShowHeroIcons !== null) {
        try {
          await setShowHeroIcons(draftShowHeroIcons);
          setSavedShowHeroIcons(draftShowHeroIcons);
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
    setDraftRecursive(savedRecursive);
    setDraftAutoSyncHeroes(savedAutoSyncHeroes);
    setDraftShowHeroIcons(savedShowHeroIcons);
    setPathError(null);
  }

  return (
    <div className="flex flex-1 min-h-0 w-full flex-col">
      <div className="flex-1 min-h-0 overflow-y-auto [scrollbar-gutter:stable]">
        <div className="flex flex-col gap-4">
          {/* ── Header ── */}
          <div className="flex min-h-8 items-center gap-3">
            <h2 className="text-xl font-bold">Settings</h2>
          </div>

          {/* ── Game Root ── */}
          <div className="flex flex-col overflow-hidden rounded-md border border-border">
            <div className="border-b border-border bg-card px-3 py-2">
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
            </div>
            <div className="relative">
              <Tip content={draftGamePath} disabled={!draftGamePath}>
                <Input
                  value={draftGamePath}
                  onChange={(e) => {
                    setDraftGamePath(e.target.value);
                    setPathError(null);
                  }}
                  placeholder={`e.g. C:\\Program Files (x86)\\Steam\\steamapps\\common\\MarvelRivals`}
                  className="h-8 pr-20 rounded-none border-0 shadow-none font-mono text-[12px] focus-visible:ring-0 focus-visible:border-0"
                />
              </Tip>
              <div className="absolute right-1 top-1/2 flex -translate-y-1/2 items-center gap-0.5">
                <Tip content="Browse for game folder">
                  <Button variant="ghost" size="icon-sm" onClick={browse}>
                    <FolderOpen size={14} />
                  </Button>
                </Tip>
                <Tip content="Auto-detect game install">
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    onClick={() => detect()}
                    disabled={detecting}
                  >
                    <Search size={14} className={cn(detecting && "animate-pulse")} />
                  </Button>
                </Tip>
              </div>
            </div>
          </div>

          {/* ── Launch Options ── */}
          <div className="flex flex-col overflow-hidden rounded-md border border-border">
            <div className="border-b border-border bg-card px-3 py-2">
              <h3 className="text-sm font-semibold">Launch Options</h3>
            </div>
            <Tip content={skipLauncherError} disabled={!skipLauncherError}>
              <label className="flex items-center gap-3 rounded-sm px-3 py-3 hover:bg-secondary/50">
                <div className="flex flex-1 flex-col gap-0.5">
                  <span className={cn("text-[13px] font-medium", skipLauncherError && "text-err")}>
                    Skip Launcher
                  </span>
                  <span className="text-[11px] text-muted-foreground">
                    Skip the launcher window and go straight into the game.
                  </span>
                </div>
                <Switch
                  checked={draftSkipLauncher ?? false}
                  onCheckedChange={setDraftSkipLauncher}
                  disabled={!draftGamePath || draftSkipLauncher === null}
                />
              </label>
            </Tip>
            {!draftGamePath && (
              <div className="px-3 py-2">
                <span className="text-[11px] text-muted-foreground">Set a game path first.</span>
              </div>
            )}
          </div>

          {/* ── Mods ── */}
          <div className="flex flex-col overflow-hidden rounded-md border border-border">
            <div className="border-b border-border bg-card px-3 py-2">
              <h3 className="text-sm font-semibold">Mods</h3>
            </div>
            <label className="flex items-center gap-3 rounded-sm px-3 py-3 hover:bg-secondary/50">
              <div className="flex flex-1 flex-col gap-0.5">
                <span className="text-[13px] font-medium">Scan ~mods subfolders</span>
                <span className="text-[11px] text-muted-foreground">
                  Include mods nested in subfolders. This matches the game's native load behavior.
                </span>
              </div>
              <Switch
                checked={draftRecursive ?? false}
                onCheckedChange={setDraftRecursive}
                disabled={draftRecursive === null}
              />
            </label>
            <label className="flex items-center gap-3 rounded-sm px-3 py-3 hover:bg-secondary/50">
              <div className="flex flex-1 flex-col gap-0.5">
                <span className="text-[13px] font-medium">Show hero icons</span>
                <span className="text-[11px] text-muted-foreground">
                  Show hero portraits next to mods and asset entries.
                </span>
              </div>
              <Switch
                checked={draftShowHeroIcons ?? false}
                onCheckedChange={setDraftShowHeroIcons}
                disabled={draftShowHeroIcons === null}
              />
            </label>
            <div className="flex items-center gap-3 rounded-sm px-3 py-3 hover:bg-secondary/50">
              <div className="flex flex-1 flex-col gap-0.5">
                <span className="text-[13px] font-medium">Auto-sync hero data</span>
                <span className="text-[11px] text-muted-foreground">
                  Fetch the latest character/skin list from GitHub (once per day).
                  {characterDataInfo &&
                    characterDataInfo.user_file_present &&
                    characterDataInfo.user_file_mtime && (
                      <> Last synced {formatRelativeTime(characterDataInfo.user_file_mtime)}.</>
                    )}
                </span>
                {syncHeroNotice && (
                  <span
                    className={cn(
                      "mt-0.5 flex items-center gap-1.5 text-[11px] font-medium",
                      syncHeroNotice.type === "ok" ? "text-ok" : "text-err"
                    )}
                  >
                    {syncHeroNotice.type === "ok" ? (
                      <CheckCircle2 size={13} strokeWidth={2.5} />
                    ) : (
                      <XCircle size={13} strokeWidth={2.5} />
                    )}
                    {syncHeroNotice.msg}
                  </span>
                )}
              </div>
              <Tip content="Download the latest hero data from remote">
                <Button
                  variant="outline"
                  size="sm"
                  onClick={syncHeroesNow}
                  disabled={syncingHeroes}
                >
                  <CloudDownload size={13} className={cn(syncingHeroes && "animate-pulse")} />
                  Sync now
                </Button>
              </Tip>
              <Switch
                checked={draftAutoSyncHeroes ?? false}
                onCheckedChange={setDraftAutoSyncHeroes}
                disabled={draftAutoSyncHeroes === null}
              />
            </div>
          </div>

          {/* ── Config Presets ── */}
          <div className="flex flex-col overflow-hidden rounded-md border border-border">
            <div className="flex items-center gap-2 border-b border-border bg-card px-3 py-2">
              <h3 className="shrink-0 text-sm font-semibold">Config Presets</h3>
              {profileNotice && (
                <Tip content={profileNotice.msg}>
                  <span
                    className={cn(
                      "flex min-w-0 items-center gap-1 text-[12px] font-medium",
                      profileNotice.type === "ok" ? "text-ok" : "text-err"
                    )}
                  >
                    {profileNotice.type === "ok" ? (
                      <CheckCircle2 size={13} strokeWidth={2.5} className="shrink-0" />
                    ) : (
                      <XCircle size={13} strokeWidth={2.5} className="shrink-0" />
                    )}
                    <span className="truncate">{profileNotice.msg}</span>
                  </span>
                </Tip>
              )}
              <Button
                variant="outline"
                size="sm"
                onClick={importTweakProfile}
                className="ml-auto shrink-0"
              >
                <Upload size={13} />
                Import
              </Button>
            </div>
            {tweakProfiles.length === 0 ? (
              <p className="px-3 py-4 text-center text-[12px] text-muted-foreground">
                No presets yet. Save tweaks as a preset from the Pak Config or Config Tweaks tab.
              </p>
            ) : (
              <ul className="divide-y divide-border/50">
                {[...tweakProfiles]
                  .sort((a, b) => b.modified_at - a.modified_at)
                  .map((p) => {
                    const isRenaming = renamingProfile === p.name;
                    return (
                      <li
                        key={p.name}
                        className="flex items-center gap-1 px-3 py-2 hover:bg-secondary/40"
                      >
                        {isRenaming ? (
                          <input
                            autoFocus
                            value={renameDraft}
                            onChange={(e) => setRenameDraft(e.target.value)}
                            onKeyDown={(e) => {
                              if (e.key === "Enter") commitRename(p.name);
                              if (e.key === "Escape") {
                                setRenamingProfile(null);
                                setRenameDraft("");
                              }
                            }}
                            placeholder="New preset name…"
                            className="h-7 min-w-0 flex-1 rounded-md border border-border bg-background px-3 text-[12px] outline-none placeholder:text-muted-foreground/50 focus:border-primary"
                          />
                        ) : (
                          <span className="min-w-0 flex-1 truncate text-[13px]">{p.name}</span>
                        )}
                        {isRenaming ? (
                          <>
                            <Tip content="Save (Enter)">
                              <Button
                                variant="blue"
                                size="icon-sm"
                                onClick={() => commitRename(p.name)}
                                disabled={!renameDraft.trim() || renameDraft.trim() === p.name}
                              >
                                <Save size={13} />
                              </Button>
                            </Tip>
                            <Tip content="Cancel (Esc)">
                              <Button
                                variant="ghost"
                                size="icon-sm"
                                onClick={() => {
                                  setRenamingProfile(null);
                                  setRenameDraft("");
                                }}
                              >
                                <X size={13} />
                              </Button>
                            </Tip>
                          </>
                        ) : (
                          <>
                            <Tip content="Rename preset">
                              <Button
                                variant="ghost"
                                size="icon-sm"
                                onClick={() => {
                                  setRenamingProfile(p.name);
                                  setRenameDraft(p.name);
                                }}
                              >
                                <Pencil size={13} />
                              </Button>
                            </Tip>
                            <Tip content="Export preset to JSON file">
                              <Button
                                variant="ghost"
                                size="icon-sm"
                                onClick={() => exportTweakProfile(p.name)}
                              >
                                <Download size={13} />
                              </Button>
                            </Tip>
                            <Tip content="Delete preset">
                              <Button
                                variant="ghost"
                                size="icon-sm"
                                className="text-destructive hover:bg-destructive/15 hover:text-destructive"
                                onClick={() => deleteTweakProfile(p.name)}
                              >
                                <Trash2 size={13} />
                              </Button>
                            </Tip>
                          </>
                        )}
                      </li>
                    );
                  })}
              </ul>
            )}
          </div>

          {/* ── Signature Bypass ── */}
          <div className="flex flex-col overflow-hidden rounded-md border border-border">
            <div className="border-b border-border bg-card px-3 py-2">
              <h3 className="text-sm font-semibold">Signature Bypass</h3>
            </div>
            <div className="flex items-center gap-3 rounded-sm px-3 py-3 hover:bg-secondary/50">
              <div className="flex flex-1 flex-col gap-0.5">
                <span className="text-[13px] font-medium">Remove bypass files</span>
                <span className="text-[11px] text-muted-foreground">
                  Removes dsound.dll and the bypass plugin from the game directory.
                </span>
                {bypassNotice && (
                  <span
                    className={cn(
                      "mt-0.5 flex items-center gap-1.5 text-[11px] font-medium",
                      bypassNotice.type === "ok" ? "text-ok" : "text-err"
                    )}
                  >
                    {bypassNotice.type === "ok" ? (
                      <CheckCircle2 size={13} strokeWidth={2.5} />
                    ) : (
                      <XCircle size={13} strokeWidth={2.5} />
                    )}
                    {bypassNotice.msg}
                  </span>
                )}
              </div>
              <Button variant="red" size="sm" onClick={removeBypass} disabled={!draftGamePath}>
                <ShieldOff size={13} />
                Remove
              </Button>
            </div>
          </div>

          {/* ── Updates ── */}
          <div className="flex flex-col overflow-hidden rounded-md border border-border">
            <div className="border-b border-border bg-card px-3 py-2">
              <h3 className="text-sm font-semibold">Updates</h3>
            </div>

            <div className="flex items-center gap-3 rounded-sm px-3 py-3 hover:bg-secondary/50">
              <div className="flex flex-1 flex-col gap-0.5">
                <span className="text-[13px] font-medium">Check for updates on startup</span>
                <span className="text-[11px] text-muted-foreground">
                  Automatically check GitHub for new releases when the app launches.
                </span>
                {updateBadge && (
                  <span
                    className={cn(
                      "mt-0.5 flex items-center gap-1.5 text-[11px] font-medium",
                      updateBadge.type === "info" ? "text-blue-400" : "text-ok"
                    )}
                  >
                    <CheckCircle2 size={13} strokeWidth={2.5} />
                    {updateBadge.msg}
                  </span>
                )}
                {updateError && (
                  <span className="mt-0.5 flex items-center gap-1 text-[11px] font-medium text-err">
                    <XCircle size={13} strokeWidth={2.5} />
                    {updateError}
                  </span>
                )}
              </div>
              <Tip content="Check for a new version of Rivals Toolkit right now">
                <Button
                  variant="outline"
                  size="sm"
                  onClick={checkUpdateNow}
                  disabled={updateChecking}
                >
                  <RefreshCw size={13} className={cn(updateChecking && "animate-spin")} />
                  Check now
                </Button>
              </Tip>
              <Switch
                checked={draftAutoCheck ?? false}
                onCheckedChange={setDraftAutoCheck}
                disabled={draftAutoCheck === null}
              />
            </div>
          </div>
        </div>
      </div>

      {/* ── Save bar (always rendered to avoid layout shift) ── */}
      <div
        className={cn(
          "flex shrink-0 items-center justify-end gap-2 border-t border-border pt-2 transition-opacity duration-150",
          dirty || savedBadge ? "opacity-100" : "pointer-events-none opacity-0"
        )}
      >
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
