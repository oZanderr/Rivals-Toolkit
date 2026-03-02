import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { FileText, FolderOpen, RefreshCw, Save, SlidersHorizontal } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";
import { ScalabilitySettings } from "./ScalabilitySettings";

type StatusType = "ok" | "err" | "info";
type SubTab = "quick" | "raw";

export function ScalabilityEditor() {
  const [filePath, setFilePath] = useState("");
  const [content, setContent] = useState("");
  const [saved, setSaved] = useState(true);
  const [subTab, setSubTab] = useState<SubTab>("quick");
  const [status, setStatus] = useState<{ msg: string; type: StatusType } | null>(null);
  const [reloadKey, setReloadKey] = useState(0);

  const showStatus = (msg: string, type: StatusType = "info") =>
    setStatus({ msg, type });

  useEffect(() => {
    autoLoad();
  }, []);

  async function autoLoad() {
    try {
      const p = await invoke<string>("get_scalability_path");
      setFilePath(p);
      await loadFile(p);
    } catch (e: any) {
      showStatus(String(e), "err");
    }
  }

  async function loadFile(path: string) {
    try {
      const text = await invoke<string>("read_scalability", { path });
      setContent(text);
      setSaved(true);
      showStatus(`Loaded: ${path}`, "ok");
    } catch {
      showStatus("File not found — a new file will be created on save.", "info");
      setContent("");
      setSaved(false);
    }
  }

  async function save() {
    try {
      await invoke("write_scalability", { path: filePath, content });
      setSaved(true);
      showStatus("Saved successfully.", "ok");
    } catch (e: any) {
      showStatus(String(e), "err");
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

  return (
    <div className="flex w-full flex-col gap-6">
      {/* Header */}
      <div>
        <h2 className="text-xl font-bold">Scalability Editor</h2>
        <p className="mt-1 text-[12px] text-muted-foreground">
          Edit Marvel Rivals' graphics scalability settings stored in AppData.
          Changes take effect on next game launch.
        </p>
      </div>

      {/* Path row */}
      <Card className="flex flex-col gap-3 p-4 bg-card">
        <h3 className="text-sm font-semibold">Config File</h3>
        <div className="flex items-center gap-2">
          <Input
            className="h-8 font-mono text-[12px]"
            value={filePath}
            onChange={(e) => { setFilePath(e.target.value); setSaved(false); }}
            placeholder="Path to Scalability.ini…"
          />
          <Button variant="outline" size="sm" onClick={browse}>
            <FolderOpen size={14} />
            Browse
          </Button>
          <Button variant="blue" size="sm" onClick={() => loadFile(filePath)}>
            <RefreshCw size={14} />
            Reload
          </Button>
        </div>
      </Card>

      {/* Sub-tab bar */}
      <div className="flex gap-1 rounded-md bg-muted p-1">
        <button
          onClick={() => setSubTab("quick")}
          className={cn(
            "flex items-center gap-1.5 rounded-sm px-3 py-1.5 text-[12px] font-medium transition-colors",
            subTab === "quick"
              ? "bg-background text-foreground shadow-sm"
              : "text-muted-foreground hover:text-foreground",
          )}
        >
          <SlidersHorizontal size={13} />
          Quick Settings
        </button>
        <button
          onClick={() => setSubTab("raw")}
          className={cn(
            "flex items-center gap-1.5 rounded-sm px-3 py-1.5 text-[12px] font-medium transition-colors",
            subTab === "raw"
              ? "bg-background text-foreground shadow-sm"
              : "text-muted-foreground hover:text-foreground",
          )}
        >
          <FileText size={13} />
          Raw Editor
        </button>
      </div>

      {/* Quick Settings view */}
      {subTab === "quick" && (
        <ScalabilitySettings
          key={reloadKey}
          filePath={filePath}
          content={content}
          setContent={setContent}
          onSaved={() => setSaved(true)}
          onReload={async () => { await loadFile(filePath); setReloadKey((k) => k + 1); }}
        />
      )}

      {/* Raw Editor view */}
      {subTab === "raw" && (
        <>
          <Card className="flex flex-col gap-3 p-4 bg-card">
            <div className="flex items-center justify-between">
              <h3 className="flex items-center gap-1.5 text-sm font-semibold">
                <FileText size={14} />
                Scalability.ini
              </h3>
              {!saved && (
                <span className="text-[11px] text-[var(--color-warn)]">Unsaved changes</span>
              )}
            </div>
            <textarea
              className="min-h-[16rem] h-[calc(100vh-420px)] w-full resize-y rounded-md border border-border bg-background px-3 py-2 font-mono text-[12px] leading-relaxed text-foreground outline-none focus:ring-1 focus:ring-ring"
              value={content}
              onChange={(e) => { setContent(e.target.value); setSaved(false); }}
              spellCheck={false}
            />
            <div className="flex justify-end">
              <Button variant="green" size="sm" onClick={save} disabled={saved}>
                <Save size={14} />
                {saved ? "Saved" : "Save Changes"}
              </Button>
            </div>
          </Card>
        </>
      )}

      {/* Status */}
      {status && (
        <p
          className={cn(
            "text-[12px]",
            status.type === "ok"
              ? "text-[var(--color-ok)]"
              : status.type === "err"
                ? "text-[var(--color-err)]"
                : "text-muted-foreground",
          )}
        >
          {status.msg}
        </p>
      )}
    </div>
  );
}
