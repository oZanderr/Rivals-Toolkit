import { useState, useEffect, useRef, useCallback } from "react";

import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open } from "@tauri-apps/plugin-dialog";
import { CheckCircle2, FileText, Package, Trash2, UploadCloud, XCircle } from "lucide-react";

import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

import { PakTweaks } from "./PakTweaks";
import { ScalabilityTweaks } from "./ScalabilityTweaks";

type SubTab = "scalability" | "pak-config";

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
  const detectPathRef = useRef(detectPath);

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
  isActiveRef.current = isActive;
  const subTabRef = useRef(subTab);
  subTabRef.current = subTab;
  const fileExistsRef = useRef(fileExists);
  fileExistsRef.current = fileExists;

  detectPathRef.current = detectPath;
  useEffect(() => {
    detectPathRef.current();
  }, []);

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

  /** Redetect the Scalability.ini path only — does not reset tweak states */
  async function detectPath() {
    setDetecting(true);
    setDetectBadge(null);
    try {
      const p = await invoke<string>("get_scalability_path");
      const hadPath = filePath !== "";
      const pathChanged = p !== filePath;
      setFilePath(p);
      await loadFile(p);
      if (pathChanged) {
        if (hadPath) showDetectBadge("Path updated");
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

  async function reloadContent() {
    await loadFile(filePath);
    setReloadSignal((s) => s + 1);
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
      setScalabilityContent(text);
      setFileExists(true);
    } catch {
      setContent("");
      setScalabilityContent("");
      setFileExists(false);
    }
  }

  const SUB_TABS: { id: SubTab; label: string; Icon: React.ElementType }[] = [
    { id: "scalability", label: "Scalability", Icon: FileText },
    { id: "pak-config", label: "Pak Config", Icon: Package },
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
        <div className="ml-auto">
          <Button
            variant="outline"
            size="sm"
            onClick={clearShaderCache}
            title="Recommended after changing config tweaks"
          >
            <Trash2 size={13} />
            Clear Shader Cache
          </Button>
        </div>
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
          setFilePath={setFilePath}
          fileExists={fileExists}
          content={content}
          setContent={setContent}
          reloadSignal={reloadSignal}
          detectBadge={detectBadge}
          detecting={detecting}
          onDetect={detectPath}
          onBrowse={browse}
          onSaved={(newContent) => {
            setFileExists(true);
            setScalabilityContent(newContent);
          }}
          onReload={reloadContent}
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
    </div>
  );
}
