import { useState, useEffect, useCallback, useMemo, useRef } from "react";

import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { Play } from "lucide-react";

import { AssetManager } from "./components/AssetManager";
import { ConfigTweaks } from "./components/ConfigTweaks";
import { IniEditor } from "./components/IniEditor";
import { Logs } from "./components/Logs";
import { Mods } from "./components/Mods";
import { Plugins } from "./components/Plugins";
import { Settings, type SettingsSection } from "./components/Settings";
import { SidebarTabs } from "./components/SidebarTabs";
import { Sounds } from "./components/Sounds";
import { Titlebar } from "./components/Titlebar";
import { UpdateAvailableDialog } from "./components/UpdateAvailableDialog";

import { Separator } from "@/components/ui/separator";
import { TIP_DELAY, Tip, TooltipProvider } from "@/components/ui/tooltip";
import { useGameRunning } from "@/hooks/useGameRunning";
import { useTabLayout } from "@/hooks/useTabLayout";
import { useUpdateCheck, type UpdateInfo } from "@/hooks/useUpdateCheck";
import { useWebviewDefaults } from "@/hooks/useWebviewDefaults";
import { ALWAYS_SHOWN, visibleTabs, type Tab } from "@/lib/tabs";
import { cn } from "@/lib/utils";

interface InstallInfo {
  path: string;
  source: string;
  launch_url: string;
}

function App() {
  const { layout, setLayout } = useTabLayout();
  const [activeTab, setActiveTab] = useState<Tab | null>(null);
  const [gamePath, setGamePath] = useState("");
  const [installInfo, setInstallInfo] = useState<InstallInfo | null | undefined>(undefined);
  const [detecting, setDetecting] = useState(false);
  const [showDetectBadge, setShowDetectBadge] = useState(false);
  const [mountedTabs, setMountedTabs] = useState<Set<Tab>>(() => new Set());
  const [version, setVersion] = useState("");
  const autoUpdateInfo = useUpdateCheck();
  const {
    isRunning: gameRunning,
    shouldBlock: gameBlocking,
    ready: gameStatusReady,
    markLaunched,
  } = useGameRunning(true);
  const [manualUpdateInfo, setManualUpdateInfo] = useState<UpdateInfo | null>(null);
  const [updateDialogOpen, setUpdateDialogOpen] = useState(false);
  const activeUpdateInfo = useMemo(
    () => manualUpdateInfo ?? autoUpdateInfo,
    [manualUpdateInfo, autoUpdateInfo]
  );

  // The automatic check offers each update it finds once, as soon as the install is known.
  const [offeredUpdate, setOfferedUpdate] = useState<UpdateInfo | null>(null);
  if (
    autoUpdateInfo?.update_available &&
    installInfo !== undefined &&
    offeredUpdate !== autoUpdateInfo
  ) {
    setOfferedUpdate(autoUpdateInfo);
    setUpdateDialogOpen(true);
  }

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

  const [pendingAssetPak, setPendingAssetPak] = useState<string | null>(null);

  const navigateToAssetManager = useCallback((pakPath: string) => {
    setPendingAssetPak(pakPath);
    setActiveTab("pak-manager");
  }, []);

  const [pendingIniFile, setPendingIniFile] = useState<string | null>(null);

  const navigateToIniEditor = useCallback((path: string) => {
    setPendingIniFile(path);
    setActiveTab("ini-editor");
  }, []);

  const [settingsSection, setSettingsSection] = useState<SettingsSection | null>(null);

  const customizeTabs = useCallback(() => {
    setSettingsSection("tabs");
    setActiveTab("settings");
  }, []);

  // The first shown tab opens at startup, and takes over when the open one is hidden.
  const shown = layout ? visibleTabs(layout) : null;
  if (shown && (activeTab === null || !shown.includes(activeTab))) {
    setActiveTab(shown[0] ?? ALWAYS_SHOWN);
  }

  // A tab stays mounted once shown, so switching back keeps its state. Hiding it keeps it too.
  if (activeTab && !mountedTabs.has(activeTab)) {
    setMountedTabs((prev) => new Set(prev).add(activeTab));
  }

  useEffect(() => {
    getVersion()
      .then(setVersion)
      .catch(() => {});
  }, []);

  useWebviewDefaults();

  return (
    <TooltipProvider delayDuration={TIP_DELAY} skipDelayDuration={0}>
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
          <nav className="flex w-48 min-w-48 flex-col overflow-x-hidden overflow-y-auto border-r border-border bg-sidebar">
            <SidebarTabs
              layout={layout}
              activeTab={activeTab}
              onSelect={setActiveTab}
              onLayoutChange={setLayout}
              onCustomize={customizeTabs}
            />

            <div className="px-2 pb-2">
              <Tip
                content={
                  !gameStatusReady
                    ? "Checking game status…"
                    : gameRunning
                      ? "Marvel Rivals is already running"
                      : installInfo
                        ? `Launch via ${installInfo.source}`
                        : "Game not detected"
                }
              >
                <button
                  onClick={() => {
                    if (!installInfo) return;
                    markLaunched();
                    invoke("launch_game", { installInfo }).catch(console.error);
                  }}
                  disabled={!installInfo || !gameStatusReady || gameRunning}
                  className={cn(
                    "flex w-full items-center gap-2.5 rounded-sm px-2.5 py-2 text-sm font-medium transition-colors",
                    installInfo && gameStatusReady && !gameRunning
                      ? "text-green-accent-foreground hover:bg-green-accent hover:text-green-accent-foreground"
                      : "cursor-not-allowed text-muted-foreground/40"
                  )}
                >
                  <Play size={15} />
                  Launch Game
                </button>
              </Tip>
            </div>

            <Separator className="mb-3" />
            <div className="px-4 pb-4 text-center">
              <span className="text-[10px] text-muted-foreground/70">
                {version ? `v${version}` : "\u00A0"}
              </span>
            </div>
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
                <Mods
                  gamePath={gamePath}
                  isActive={activeTab === "mod-tools"}
                  gameRunning={gameBlocking}
                  pathLoading={installInfo === undefined}
                  onViewInAssetManager={
                    shown?.includes("pak-manager") ? navigateToAssetManager : undefined
                  }
                />
              </div>
            )}
            {mountedTabs.has("plugins") && (
              <div
                className={cn(
                  "flex flex-1 min-h-0 flex-col overflow-hidden p-5",
                  activeTab !== "plugins" && "hidden"
                )}
              >
                <Plugins
                  key={gamePath}
                  gamePath={gamePath}
                  isActive={activeTab === "plugins"}
                  gameRunning={gameBlocking}
                  pathLoading={installInfo === undefined}
                  onOpenInEditor={shown?.includes("ini-editor") ? navigateToIniEditor : undefined}
                />
              </div>
            )}
            {mountedTabs.has("pak-manager") && (
              <div
                className={cn(
                  "flex flex-1 min-h-0 flex-col overflow-hidden p-5",
                  activeTab !== "pak-manager" && "hidden"
                )}
              >
                <AssetManager
                  gamePath={gamePath}
                  gameRunning={gameBlocking}
                  pendingPak={pendingAssetPak}
                  onPendingPakConsumed={() => setPendingAssetPak(null)}
                />
              </div>
            )}
            {mountedTabs.has("ini-editor") && (
              <div
                className={cn(
                  "flex flex-1 min-h-0 flex-col overflow-hidden p-5",
                  activeTab !== "ini-editor" && "hidden"
                )}
              >
                {/* Keyed by the game, so another install starts it afresh: no paks, entries
                    or edits carried over from the last one. */}
                <IniEditor
                  key={gamePath}
                  gamePath={gamePath}
                  isActive={activeTab === "ini-editor"}
                  gameRunning={gameBlocking}
                  pendingFile={pendingIniFile}
                  onPendingFileConsumed={() => setPendingIniFile(null)}
                />
              </div>
            )}
            {mountedTabs.has("config-tweaks") && (
              <div
                className={cn(
                  "flex flex-1 min-h-0 flex-col overflow-hidden p-5",
                  activeTab !== "config-tweaks" && "hidden"
                )}
              >
                <ConfigTweaks gamePath={gamePath} isActive={activeTab === "config-tweaks"} />
              </div>
            )}
            {mountedTabs.has("sounds") && (
              <div
                className={cn(
                  "flex flex-1 min-h-0 flex-col overflow-hidden p-5",
                  activeTab !== "sounds" && "hidden"
                )}
              >
                <Sounds gamePath={gamePath} isActive={activeTab === "sounds"} />
              </div>
            )}
            {mountedTabs.has("logs") && (
              <div
                className={cn(
                  "flex flex-1 min-h-0 flex-col overflow-hidden p-5",
                  activeTab !== "logs" && "hidden"
                )}
              >
                <Logs
                  gamePath={gamePath}
                  isActive={activeTab === "logs"}
                  onOpenSettings={() => setActiveTab("settings")}
                />
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
                  tabLayout={layout}
                  onTabLayoutChange={setLayout}
                  pendingSection={settingsSection}
                  onPendingSectionConsumed={() => setSettingsSection(null)}
                />
              </div>
            )}
          </main>
        </div>
      </div>
    </TooltipProvider>
  );
}

export default App;
