import { useState, useEffect, useRef, useCallback } from "react";

import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open } from "@tauri-apps/plugin-dialog";
import {
  AlertTriangle,
  CheckCircle2,
  FileText,
  Gauge,
  Package,
  Trash2,
  UploadCloud,
  XCircle,
} from "lucide-react";

import { GameUserSettingsTweaks } from "./GameUserSettingsTweaks";
import { PakTweaks } from "./PakTweaks";
import { ScalabilityTweaks } from "./ScalabilityTweaks";

import { Button } from "@/components/ui/button";
import { Tip } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";

type SubTab = "scalability" | "pak-config" | "game-settings";

interface Props {
  gamePath: string;
  isActive: boolean;
}

export function ConfigTweaks({ gamePath, isActive }: Props) {
  const [subTab, setSubTab] = useState<SubTab>("scalability");
  const [isDragging, setIsDragging] = useState(false);

  const [filePath, setFilePath] = useState("");
  const [content, setContent] = useState("");
  const [scalabilityContent, setScalabilityContent] = useState("");
  const [fileExists, setFileExists] = useState<boolean | null>(null);
  const [reloadSignal, setReloadSignal] = useState(0);
  const [detecting, setDetecting] = useState(false);
  const [detectBadge, setDetectBadge] = useState<string | null>(null);
  const detectBadgeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const detectPathRef = useRef<(() => Promise<void>) | null>(null);

  const [shaderNotice, setShaderNotice] = useState<{
    msg: string;
    type: "ok" | "err";
  } | null>(null);
  const shaderTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const clearShaderCache = useCallback(async () => {
    if (shaderTimer.current) clearTimeout(shaderTimer.current);
    try {
      const msg = await invoke<string>("clear_shader_cache");
      setShaderNotice({ msg, type: "ok" });
    } catch (e: unknown) {
      setShaderNotice({ msg: String(e), type: "err" });
    }
    shaderTimer.current = setTimeout(() => setShaderNotice(null), 6000);
  }, []);

  const isActiveRef = useRef(isActive);
  const subTabRef = useRef(subTab);
  const fileExistsRef = useRef(fileExists);

  // Drag-and-drop: accept .ini on scalability sub-tab, .pak on pak-config sub-tab.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    getCurrentWindow()
      .onDragDropEvent(async (event) => {
        if (event.payload.type === "enter") {
          if (isActiveRef.current) setIsDragging(true);
        } else if (event.payload.type === "drop") {
          setIsDragging(false);
          if (!isActiveRef.current) return;
          if (subTabRef.current === "scalability") {
            const iniPaths = event.payload.paths.filter((p) => p.toLowerCase().endsWith(".ini"));
            if (iniPaths.length === 0) return;
            const droppedPath = iniPaths[0];
            try {
              const text = await invoke<string>("read_scalability", { path: droppedPath });
              // If no scalability.ini exists yet, install to default path
              if (!fileExistsRef.current) {
                const defaultPath = await invoke<string>("get_scalability_path");
                await invoke("write_scalability", { path: defaultPath, content: text });
                setFilePath(defaultPath);
                setContent(text);
                setScalabilityContent(text);
                setFileExists(true);
                setReloadSignal((s) => s + 1);
              } else {
                setFilePath(droppedPath);
                setContent(text);
                setScalabilityContent(text);
                setFileExists(true);
                setReloadSignal((s) => s + 1);
              }
            } catch {
              // Silently ignore unreadable files
            }
          }
        } else if (event.payload.type === "leave") {
          setIsDragging(false);
        }
      })
      .then((fn) => {
        unlisten = fn;
      });
    return () => unlisten?.();
  }, []);

  async function loadFile(path: string): Promise<boolean> {
    try {
      const text = await invoke<string>("read_scalability", { path });
      setContent(text);
      setScalabilityContent(text);
      setFileExists(true);
      return true;
    } catch {
      setContent("");
      setScalabilityContent("");
      setFileExists(false);
      return false;
    }
  }

  /** Used on mount: detects path and loads file content. */
  async function detectPath() {
    setDetecting(true);
    setDetectBadge(null);
    try {
      const p = await invoke<string>("get_scalability_path");
      setFilePath(p);
      await loadFile(p);
    } catch {
      // no-op on mount
    } finally {
      setDetecting(false);
    }
  }

  function showDetectBadge(msg: string) {
    if (detectBadgeTimer.current) clearTimeout(detectBadgeTimer.current);
    setDetectBadge(msg);
    detectBadgeTimer.current = setTimeout(() => setDetectBadge(null), 4000);
  }

  async function reloadContent() {
    const found = await loadFile(filePath);
    setReloadSignal((s) => s + 1);
    // No success badge when the file is missing; the persistent "Not found" badge stands.
    if (found) showDetectBadge("Reloaded");
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

  useEffect(() => {
    isActiveRef.current = isActive;
    subTabRef.current = subTab;
    fileExistsRef.current = fileExists;
    detectPathRef.current = detectPath;
  });

  useEffect(() => {
    detectPathRef.current?.();
  }, []);

  const SUB_TABS: { id: SubTab; label: string; Icon: React.ElementType }[] = [
    { id: "scalability", label: "Scalability", Icon: FileText },
    { id: "pak-config", label: "Pak Config", Icon: Package },
    { id: "game-settings", label: "Game Settings", Icon: Gauge },
  ];

  return (
    <div className="flex flex-1 min-h-0 w-full flex-col gap-6">
      {/* Header */}
      <div className="flex min-h-8 items-center gap-3">
        <h2 className="text-xl font-bold">Config Tweaks</h2>
        {shaderNotice && (
          <span
            className={cn(
              "flex items-center gap-1.5 text-[12px] font-medium",
              shaderNotice.type === "ok" ? "text-ok" : "text-err"
            )}
          >
            {shaderNotice.type === "ok" ? (
              <CheckCircle2 size={13} strokeWidth={2.5} />
            ) : (
              <XCircle size={13} strokeWidth={2.5} />
            )}
            {shaderNotice.msg}
          </span>
        )}
        {subTab === "pak-config" && (
          <div className="ml-auto">
            <Tip content="Recommended after changing config tweaks">
              <Button variant="outline" size="sm" onClick={clearShaderCache}>
                <Trash2 size={13} />
                Clear Shader Cache
              </Button>
            </Tip>
          </div>
        )}
      </div>

      {/* Warning banner: anti-cheat for tweaks, overwrite caveat for game-settings. */}
      <div className="flex items-center gap-2.5 rounded-md border border-warn/20 bg-warn/5 px-3 py-2">
        <AlertTriangle size={15} className="shrink-0 text-warn" />
        <span className="flex-1 text-[12px] text-warn">
          {subTab === "game-settings"
            ? "Marvel Rivals overwrites this file when it saves settings. Close the game before editing, and changes may reset when the game writes its own preferences."
            : "The game now detects graphics-altering config tweaks. No punishments yet, but use at your own risk."}
        </span>
      </div>

      {/* Sub-tab bar */}
      <div className="flex w-fit gap-1 rounded-md bg-muted p-1">
        {SUB_TABS.map(({ id, label, Icon }) => (
          <button
            key={id}
            onClick={() => setSubTab(id)}
            className={cn(
              "flex items-center gap-1.5 rounded-sm px-3 py-1.5 text-[12px] font-medium transition-colors",
              subTab === id
                ? "bg-background text-foreground shadow-sm"
                : "text-muted-foreground hover:text-foreground"
            )}
          >
            <Icon size={13} />
            {label}
          </button>
        ))}
      </div>

      {/* ── Scalability tab ── */}
      <div
        className={cn(
          "relative flex flex-1 min-h-0 flex-col",
          subTab !== "scalability" && "hidden"
        )}
      >
        {isDragging && subTab === "scalability" && (
          <div className="pointer-events-none absolute inset-0 z-50 flex flex-col items-center justify-center gap-3 rounded-lg border-2 border-dashed border-ok bg-background/80 backdrop-blur-sm">
            <UploadCloud size={36} className="text-ok" />
            <span className="text-sm font-semibold text-ok">
              Drop .ini to load scalability config
            </span>
          </div>
        )}
        <ScalabilityTweaks
          filePath={filePath}
          fileExists={fileExists}
          content={content}
          setContent={setContent}
          reloadSignal={reloadSignal}
          detectBadge={detectBadge}
          detecting={detecting}
          onBrowse={browse}
          onSaved={(newContent) => {
            setFileExists(true);
            setScalabilityContent(newContent);
          }}
          onReload={reloadContent}
          onDeleted={() => {
            setContent("");
            setScalabilityContent("");
            setFileExists(false);
            setReloadSignal((s) => s + 1);
          }}
        />
      </div>

      {/* ── Pak Config tab ── */}
      <div className={cn("flex flex-1 min-h-0 flex-col", subTab !== "pak-config" && "hidden")}>
        <PakTweaks
          gamePath={gamePath}
          scalabilityContent={scalabilityContent}
          isActive={isActive && subTab === "pak-config"}
        />
      </div>

      {/* ── Game Settings tab ── */}
      <div className={cn("flex flex-1 min-h-0 flex-col", subTab !== "game-settings" && "hidden")}>
        <GameUserSettingsTweaks />
      </div>
    </div>
  );
}
