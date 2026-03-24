import { useState, useEffect, type ReactNode } from "react";

import { invoke } from "@tauri-apps/api/core";
import { openPath } from "@tauri-apps/plugin-opener";
import { House, Package, Settings, Wrench, Play, ExternalLink, FileCode2 } from "lucide-react";

import { Separator } from "@/components/ui/separator";
import { cn } from "@/lib/utils";

import { Home } from "./components/Home";
import { ModTools } from "./components/ModTools";
import { PakIniEditor } from "./components/PakIniEditor";
import { PakManager } from "./components/PakManager";
import { QuickSettings } from "./components/QuickSettings";
import { Titlebar } from "./components/Titlebar";

type Tab = "home" | "mod-tools" | "pak-manager" | "ini-editor" | "settings";

const DISCORD_URL = "https://discord.com/invite/F2FYFfVqjs";

interface InstallInfo {
  path: string;
  source: string;
  launch_url: string;
}

const TABS: { id: Tab; label: string; icon: ReactNode }[] = [
  { id: "home", label: "Home", icon: <House size={15} /> },
  { id: "mod-tools", label: "Mod Tools", icon: <Wrench size={15} /> },
  { id: "settings", label: "Quick Settings", icon: <Settings size={15} /> },
  { id: "ini-editor", label: "Pak INI Editor", icon: <FileCode2 size={15} /> },
  { id: "pak-manager", label: "Pak Manager", icon: <Package size={15} /> },
];

function App() {
  const [activeTab, setActiveTab] = useState<Tab>("home");
  const [gamePath, setGamePath] = useState("");
  const [installInfo, setInstallInfo] = useState<InstallInfo | null | undefined>(undefined);
  const [mountedTabs, setMountedTabs] = useState<Set<Tab>>(() => new Set(["home"]));

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- functional updater is safe; accumulates visited tabs with no external side effects
    setMountedTabs((prev) => {
      if (prev.has(activeTab)) return prev;
      return new Set(prev).add(activeTab);
    });
  }, [activeTab]);

  // Suppress webview's native Ctrl+F on all tabs
  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if (e.key === "f" && (e.ctrlKey || e.metaKey)) e.preventDefault();
    }
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, []);

  async function openDiscord() {
    try {
      await openPath(DISCORD_URL);
    } catch (e) {
      console.error("Failed to open Discord link:", e);
    }
  }

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

          <>
            <Separator className="mb-3" />
            <div className="px-4 pb-4">
              <div className="rounded-sm border border-border/70 bg-background/60 px-2.5 py-2">
                <span className="block text-[10px] uppercase tracking-widest text-muted-foreground">
                  About
                </span>
                <span className="block text-[11px] text-muted-foreground">
                  Join the discord for support.
                </span>
                <button
                  onClick={openDiscord}
                  className="mt-1.5 inline-flex items-center gap-1 text-[11px] font-medium text-blue-accent-foreground hover:opacity-90"
                  title="Open Discord server"
                >
                  <ExternalLink size={11} />
                  Oinkers Discord Server
                </button>
              </div>
            </div>
          </>
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
              <PakManager gamePath={gamePath} />
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
              <QuickSettings gamePath={gamePath} />
            </div>
          )}
        </main>
      </div>
    </div>
  );
}

export default App;
