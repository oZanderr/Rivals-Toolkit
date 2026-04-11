import { useState, useEffect, type ReactNode } from "react";

import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { House, Package, Settings, Wrench, Play, FileCode2, Volume2 } from "lucide-react";

import { Separator } from "@/components/ui/separator";
import { cn } from "@/lib/utils";

import { AssetManager } from "./components/AssetManager";
import { ConfigTweaks } from "./components/ConfigTweaks";
import { Hitsounds } from "./components/Hitsounds";
import { Home } from "./components/Home";
import { ModTools } from "./components/ModTools";
import { PakIniEditor } from "./components/PakIniEditor";
import { Titlebar } from "./components/Titlebar";

type Tab = "home" | "mod-tools" | "pak-manager" | "ini-editor" | "settings" | "hitsounds";

interface InstallInfo {
  path: string;
  source: string;
  launch_url: string;
}

const TABS: { id: Tab; label: string; icon: ReactNode }[] = [
  { id: "home", label: "Home", icon: <House size={15} /> },
  { id: "mod-tools", label: "Mod Tools", icon: <Wrench size={15} /> },
  { id: "hitsounds", label: "Hitsounds", icon: <Volume2 size={15} /> },
  { id: "settings", label: "Config Tweaks", icon: <Settings size={15} /> },
  { id: "ini-editor", label: "Pak INI Editor", icon: <FileCode2 size={15} /> },
  { id: "pak-manager", label: "Asset Manager", icon: <Package size={15} /> },
];

function App() {
  const [activeTab, setActiveTab] = useState<Tab>("home");
  const [gamePath, setGamePath] = useState("");
  const [installInfo, setInstallInfo] = useState<InstallInfo | null | undefined>(undefined);
  const [mountedTabs, setMountedTabs] = useState<Set<Tab>>(() => new Set(["home"]));
  const [version, setVersion] = useState("");

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- functional updater is safe; accumulates visited tabs with no external side effects
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

  // Suppress webview's native Ctrl+F on all tabs
  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if (e.key === "f" && (e.ctrlKey || e.metaKey)) e.preventDefault();
    }
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, []);

  return (
    <div className="flex h-screen flex-col overflow-hidden bg-background text-foreground">
      <Titlebar />
      <div className="flex min-h-0 flex-1 overflow-hidden">
        {/* Sidebar */}
        <nav className="flex w-[210px] min-w-[210px] flex-col overflow-x-hidden overflow-y-auto border-r border-border bg-card">
          <div className="px-2 pt-2 pb-2">
            <button
              onClick={() => installInfo && invoke("launch_game", { installInfo })}
              disabled={!installInfo}
              className={cn(
                "flex w-full items-center gap-2.5 rounded-sm px-2.5 py-2 text-[13px] font-medium transition-colors",
                installInfo
                  ? "text-[var(--green-accent-foreground)] hover:bg-[var(--green-accent)] hover:text-[var(--green-accent-foreground)]"
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
                    "flex w-full items-center gap-2.5 rounded-sm px-2.5 py-2 text-[13px] text-muted-foreground transition-colors hover:bg-secondary hover:text-foreground",
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
              <div className="px-4 pb-4">
                <span className="block text-center text-[10px] text-muted-foreground/50">
                  v{version}
                </span>
              </div>
            </>
          )}
        </nav>

        {/* Content — lazy-mount & keep-mounted to preserve state across tab switches */}
        <main className="flex min-h-0 flex-1 flex-col overflow-hidden bg-background">
          {mountedTabs.has("home") && (
            <div
              className={cn(
                "flex flex-1 min-h-0 flex-col overflow-hidden p-5",
                activeTab !== "home" && "hidden"
              )}
            >
              <Home
                gamePath={gamePath}
                setGamePath={setGamePath}
                setActiveTab={setActiveTab}
                installInfo={installInfo}
                setInstallInfo={setInstallInfo}
                isActive={activeTab === "home"}
              />
            </div>
          )}
          {mountedTabs.has("mod-tools") && (
            <div
              className={cn(
                "flex flex-1 min-h-0 flex-col overflow-hidden p-5",
                activeTab !== "mod-tools" && "hidden"
              )}
            >
              <ModTools gamePath={gamePath} isActive={activeTab === "mod-tools"} />
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
          {mountedTabs.has("settings") && (
            <div
              className={cn(
                "flex flex-1 min-h-0 flex-col overflow-hidden p-5",
                activeTab !== "settings" && "hidden"
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
        </main>
      </div>
    </div>
  );
}

export default App;
