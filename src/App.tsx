import { useState, useEffect, useCallback, useMemo, useRef, type ReactNode } from "react";

import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import {
  Package,
  Settings as SettingsIcon,
  SlidersHorizontal,
  Puzzle,
  Play,
  FileCode2,
  Volume2,
} from "lucide-react";

import { Separator } from "@/components/ui/separator";
import { useUpdateCheck, type UpdateInfo } from "@/hooks/useUpdateCheck";
import { cn } from "@/lib/utils";

import { AssetManager } from "./components/AssetManager";
import { ConfigTweaks } from "./components/ConfigTweaks";
import { Hitsounds } from "./components/Hitsounds";
import { Mods } from "./components/Mods";
import { PakIniEditor } from "./components/PakIniEditor";
import { Settings } from "./components/Settings";
import { Titlebar } from "./components/Titlebar";
import { UpdateAvailableDialog } from "./components/UpdateAvailableDialog";

type Tab = "mod-tools" | "pak-manager" | "ini-editor" | "config-tweaks" | "hitsounds" | "settings";

interface InstallInfo {
  path: string;
  source: string;
  launch_url: string;
}

const TABS: { id: Tab; label: string; icon: ReactNode }[] = [
  { id: "mod-tools", label: "Mods", icon: <Puzzle size={15} /> },
  { id: "hitsounds", label: "Hitsounds", icon: <Volume2 size={15} /> },
  { id: "config-tweaks", label: "Config Tweaks", icon: <SlidersHorizontal size={15} /> },
  { id: "ini-editor", label: "Pak INI Editor", icon: <FileCode2 size={15} /> },
  { id: "pak-manager", label: "Asset Manager", icon: <Package size={15} /> },
  { id: "settings", label: "Settings", icon: <SettingsIcon size={15} /> },
];

function App() {
  const [activeTab, setActiveTab] = useState<Tab>("mod-tools");
  const [gamePath, setGamePath] = useState("");
  const [installInfo, setInstallInfo] = useState<InstallInfo | null | undefined>(undefined);
  const [detecting, setDetecting] = useState(false);
  const [showDetectBadge, setShowDetectBadge] = useState(false);
  const [mountedTabs, setMountedTabs] = useState<Set<Tab>>(() => new Set(["mod-tools"]));
  const [version, setVersion] = useState("");
  const autoUpdateInfo = useUpdateCheck();
  const [manualUpdateInfo, setManualUpdateInfo] = useState<UpdateInfo | null>(null);
  const [updateDialogOpen, setUpdateDialogOpen] = useState(false);
  const activeUpdateInfo = useMemo(
    () => manualUpdateInfo ?? autoUpdateInfo,
    [manualUpdateInfo, autoUpdateInfo]
  );

  useEffect(() => {
    if (!autoUpdateInfo?.update_available) return;
    if (installInfo === undefined) return;
    setUpdateDialogOpen(true);
  }, [autoUpdateInfo, installInfo]);

  const handleManualUpdateFound = useCallback((info: UpdateInfo) => {
    setManualUpdateInfo(info);
    setUpdateDialogOpen(true);
  }, []);

  const handleUpdateDialogOpenChange = useCallback((open: boolean) => {
    setUpdateDialogOpen(open);
    if (!open) setManualUpdateInfo(null);
  }, []);
  const didInit = useRef(false);
  const lastSavedPath = useRef<string | null>(null);
  const detectBadgeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const detect = useCallback(async () => {
    setDetecting(true);
    try {
      const result = await invoke<InstallInfo | null>("detect_install_path");
      setInstallInfo(result);
      if (result) {
        setGamePath(result.path);
        lastSavedPath.current = result.path;
        invoke("set_game_path", {
          path: result.path,
          installInfo: result,
        }).catch(console.error);
        if (detectBadgeTimer.current) clearTimeout(detectBadgeTimer.current);
        setShowDetectBadge(true);
        detectBadgeTimer.current = setTimeout(() => setShowDetectBadge(false), 4000);
      }
    } catch (e) {
      console.error(e);
      setInstallInfo(null);
    } finally {
      setDetecting(false);
    }
  }, []);

  useEffect(() => {
    if (didInit.current) return;
    didInit.current = true;
    (async () => {
      let saved: string | null = null;
      try {
        saved = await invoke<string | null>("get_game_path");
      } catch (e) {
        console.error(e);
      }
      lastSavedPath.current = saved;
      if (saved) {
        setGamePath(saved);
        try {
          const info = await invoke<InstallInfo | null>("get_saved_install_info");
          setInstallInfo(info);
        } catch (e) {
          console.error(e);
        }
      } else {
        detect();
      }
    })();
  }, [detect]);

  useEffect(() => {
    const next = gamePath || null;
    if (lastSavedPath.current === next) return;
    const t = setTimeout(() => {
      invoke("set_game_path", { path: next, installInfo: null })
        .then(() => {
          lastSavedPath.current = next;
        })
        .catch(console.error);
    }, 400);
    return () => clearTimeout(t);
  }, [gamePath]);

  useEffect(() => {
    setMountedTabs((prev) => {
      if (prev.has(activeTab)) return prev;
      return new Set(prev).add(activeTab);
    });
  }, [activeTab]);

  useEffect(() => {
    getVersion()
      .then(setVersion)
      .catch(() => {});
  }, []);

  // Suppress webview defaults: native Ctrl+F and right-click context menu
  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if (e.key === "f" && (e.ctrlKey || e.metaKey)) e.preventDefault();
    }
    function onContextMenu(e: MouseEvent) {
      e.preventDefault();
    }
    document.addEventListener("keydown", onKeyDown);
    document.addEventListener("contextmenu", onContextMenu);
    return () => {
      document.removeEventListener("keydown", onKeyDown);
      document.removeEventListener("contextmenu", onContextMenu);
    };
  }, []);

  return (
    <div className="flex h-screen flex-col overflow-hidden bg-background text-foreground">
      <Titlebar />
      {activeUpdateInfo?.update_available && (
        <UpdateAvailableDialog
          updateInfo={activeUpdateInfo}
          open={updateDialogOpen}
          onOpenChange={handleUpdateDialogOpenChange}
        />
      )}
      <div className="flex min-h-0 flex-1 overflow-hidden">
        {/* Sidebar */}
        <nav className="flex w-48 min-w-48 flex-col overflow-x-hidden overflow-y-auto border-r border-border bg-card">
          <div className="px-2 pt-2 pb-2">
            <button
              onClick={() => installInfo && invoke("launch_game", { installInfo })}
              disabled={!installInfo}
              className={cn(
                "flex w-full items-center gap-2.5 rounded-sm px-2.5 py-2 text-sm font-medium transition-colors",
                installInfo
                  ? "text-green-accent-foreground hover:bg-green-accent hover:text-green-accent-foreground"
                  : "cursor-not-allowed text-muted-foreground/40"
              )}
              title={installInfo ? `Launch via ${installInfo.source}` : "Game not detected"}
            >
              <Play size={15} />
              Launch Game
            </button>
          </div>
          <Separator className="mx-2 w-auto" />
          <ul className="flex flex-1 flex-col gap-0.5 px-2 pt-2">
            {TABS.map((t) => (
              <li key={t.id}>
                <button
                  onClick={() => setActiveTab(t.id)}
                  className={cn(
                    "flex w-full items-center gap-2.5 rounded-sm px-2.5 py-2 text-sm text-muted-foreground transition-colors hover:bg-secondary hover:text-foreground",
                    activeTab === t.id && "bg-secondary font-semibold text-foreground"
                  )}
                >
                  {t.icon}
                  {t.label}
                </button>
              </li>
            ))}
          </ul>

          {version && (
            <>
              <Separator className="mb-3" />
              <div className="px-4 pb-4 text-center">
                <span className="text-[10px] text-muted-foreground/50">v{version}</span>
              </div>
            </>
          )}
        </nav>

        {/* Content — lazy-mount & keep-mounted to preserve state across tab switches */}
        <main className="flex min-h-0 flex-1 flex-col overflow-hidden bg-background">
          {mountedTabs.has("mod-tools") && (
            <div
              className={cn(
                "flex flex-1 min-h-0 flex-col overflow-hidden p-5",
                activeTab !== "mod-tools" && "hidden"
              )}
            >
              <Mods gamePath={gamePath} isActive={activeTab === "mod-tools"} />
            </div>
          )}
          {mountedTabs.has("pak-manager") && (
            <div
              className={cn(
                "flex flex-1 min-h-0 flex-col overflow-hidden p-5",
                activeTab !== "pak-manager" && "hidden"
              )}
            >
              <AssetManager gamePath={gamePath} />
            </div>
          )}
          {mountedTabs.has("ini-editor") && (
            <div
              className={cn(
                "flex flex-1 min-h-0 flex-col overflow-hidden p-5",
                activeTab !== "ini-editor" && "hidden"
              )}
            >
              <PakIniEditor gamePath={gamePath} isActive={activeTab === "ini-editor"} />
            </div>
          )}
          {mountedTabs.has("config-tweaks") && (
            <div
              className={cn(
                "flex flex-1 min-h-0 flex-col overflow-hidden p-5",
                activeTab !== "config-tweaks" && "hidden"
              )}
            >
              <ConfigTweaks gamePath={gamePath} />
            </div>
          )}
          {mountedTabs.has("hitsounds") && (
            <div
              className={cn(
                "flex flex-1 min-h-0 flex-col overflow-hidden p-5",
                activeTab !== "hitsounds" && "hidden"
              )}
            >
              <Hitsounds gamePath={gamePath} isActive={activeTab === "hitsounds"} />
            </div>
          )}
          {mountedTabs.has("settings") && (
            <div
              className={cn(
                "flex flex-1 min-h-0 flex-col overflow-hidden p-5",
                activeTab !== "settings" && "hidden"
              )}
            >
              <Settings
                gamePath={gamePath}
                setGamePath={setGamePath}
                installInfo={installInfo}
                detect={detect}
                detecting={detecting}
                showDetectBadge={showDetectBadge}
                onManualUpdateFound={handleManualUpdateFound}
              />
            </div>
          )}
        </main>
      </div>
    </div>
  );
}

export default App;
