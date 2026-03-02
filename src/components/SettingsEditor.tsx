import { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { openPath } from "@tauri-apps/plugin-opener";
import { CheckCircle2, FileText, FolderOpen, Package, Search } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";
import { ScalabilitySettings } from "./ScalabilitySettings";
import { PakTweaks } from "./PakTweaks";

type SubTab = "scalability" | "pak-config";

interface Props {
  gamePath: string;
}

export function SettingsEditor({ gamePath }: Props) {
  const [subTab, setSubTab] = useState<SubTab>("scalability");

  const [filePath, setFilePath] = useState("");
  const [content, setContent] = useState("");
  const [reloadKey, setReloadKey] = useState(0);
  const [detecting, setDetecting] = useState(false);
  const [detectBadge, setDetectBadge] = useState<string | null>(null);
  const detectBadgeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    detectPath();
  }, []);

  /** Re-detect the Scalability.ini path only — does not reset tweak states */
  async function detectPath() {
    setDetecting(true);
    setDetectBadge(null);
    try {
      const p = await invoke<string>("get_scalability_path");
      const pathChanged = p !== filePath;
      setFilePath(p);
      if (pathChanged) {
        await loadFile(p);
        showDetectBadge("Path updated");
      } else {
        showDetectBadge("Path unchanged");
      }
    } catch {
      showDetectBadge("Not found");
    } finally {
      setDetecting(false);
    }
  }

  function showDetectBadge(msg: string) {
    if (detectBadgeTimer.current) clearTimeout(detectBadgeTimer.current);
    setDetectBadge(msg);
    detectBadgeTimer.current = setTimeout(() => setDetectBadge(null), 4000);
  }

  /** Re-read file from disk and remount ScalabilitySettings to refresh tweak states */
  async function reloadContent() {
    await loadFile(filePath);
    setReloadKey((k) => k + 1);
  }

  async function browse() {
    const selected = await open({
      multiple: false,
      filters: [{ name: "INI files", extensions: ["ini"] }],
    });
    if (typeof selected === "string") {
      setFilePath(selected);
      await loadFile(selected);
    }
  }

  async function loadFile(path: string) {
    try {
      const text = await invoke<string>("read_scalability", { path });
      setContent(text);
    } catch {
      setContent("");
    }
  }

  const SUB_TABS: { id: SubTab; label: string; Icon: React.ElementType }[] = [
    { id: "scalability", label: "Scalability", Icon: FileText },
    { id: "pak-config", label: "Pak Config", Icon: Package },
  ];

  return (
    <div className="flex w-full flex-col gap-6">
      {/* Header */}
      <div>
        <h2 className="text-xl font-bold">Quick Settings</h2>
      </div>

      {/* Sub-tab bar */}
      <div className="flex gap-1 rounded-md bg-muted p-1">
        {SUB_TABS.map(({ id, label, Icon }) => (
          <button
            key={id}
            onClick={() => setSubTab(id)}
            className={cn(
              "flex items-center gap-1.5 rounded-sm px-3 py-1.5 text-[12px] font-medium transition-colors",
              subTab === id
                ? "bg-background text-foreground shadow-sm"
                : "text-muted-foreground hover:text-foreground",
            )}
          >
            <Icon size={13} />
            {label}
          </button>
        ))}
      </div>

      {/* ── Scalability tab ── */}
      <div className={cn(subTab !== "scalability" && "hidden")}>
          {/* Config file location */}
          <div className="flex flex-col gap-6">
          <Card className="flex flex-col gap-3 p-4 bg-card">
            <div className="flex items-center justify-between">
              <div className="flex items-center gap-2">
                <span className="text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">Config File</span>
                {detectBadge && (
                  <span className={cn(
                    "flex items-center gap-1 text-[12px] font-medium",
                    detectBadge === "Not found" ? "text-[var(--color-warn)]" : "text-[var(--color-ok)]",
                  )}>
                    <CheckCircle2 size={13} strokeWidth={2.5} />
                    {detectBadge}
                  </span>
                )}
              </div>
              <Button
                variant="ghost"
                size="sm"
                disabled={!filePath}
                onClick={() => filePath && openPath(filePath.replace(/[/\\][^/\\]+$/, ""))}
              >
                <FolderOpen size={14} />
                Show in Explorer
              </Button>
            </div>
            <div className="flex items-center gap-2">
              <Input
                className="h-8 font-mono text-[12px]"
                value={filePath}
                onChange={(e) => setFilePath(e.target.value)}
                placeholder="Path to Scalability.ini…"
              />
              <Button variant="outline" size="sm" onClick={browse}>
                <FolderOpen size={14} />
                Browse
              </Button>
              <Button variant="blue" size="sm" onClick={detectPath} disabled={detecting}>
                <Search size={14} className={cn(detecting && "animate-pulse")} />
                Re-detect
              </Button>
            </div>
          </Card>

          {/* Quick settings */}
          <ScalabilitySettings
            key={reloadKey}
            filePath={filePath}
            content={content}
            setContent={setContent}
            onSaved={() => {}}
            onReload={reloadContent}
          />
          </div>
      </div>

      {/* ── Pak Config tab ── */}
      <div className={cn(subTab !== "pak-config" && "hidden")}>
        <PakTweaks gamePath={gamePath} />
      </div>
    </div>
  );
}
