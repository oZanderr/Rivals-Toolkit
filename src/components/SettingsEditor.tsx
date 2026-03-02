import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { openPath } from "@tauri-apps/plugin-opener";
import { FileText, FolderOpen, Package, RotateCcw } from "lucide-react";
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

  useEffect(() => {
    autoLoad();
  }, []);

  async function autoLoad() {
    try {
      const p = await invoke<string>("get_scalability_path");
      setFilePath(p);
      await loadFile(p);
    } catch {
      // Path detection failed — user can browse manually
    }
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
    <div className="flex w-full max-w-4xl flex-col gap-6">
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
              <span className="text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">Config File</span>
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
              <Button variant="blue" size="sm" onClick={autoLoad}>
                <RotateCcw size={14} />
                Re-detect
              </Button>
            </div>
          </Card>

          {/* Quick settings */}
          <ScalabilitySettings
            filePath={filePath}
            content={content}
            setContent={setContent}
            onSaved={() => {}}
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
