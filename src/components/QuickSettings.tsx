import { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { FileText, Package } from "lucide-react";
import { cn } from "@/lib/utils";
import { ScalabilityTweaks } from "./ScalabilityTweaks";
import { PakTweaks } from "./PakTweaks";

type SubTab = "scalability" | "pak-config";

interface Props {
  gamePath: string;
}

export function QuickSettings({ gamePath }: Props) {
  const [subTab, setSubTab] = useState<SubTab>("scalability");

  const [filePath, setFilePath] = useState("");
  const [content, setContent] = useState("");
  const [fileExists, setFileExists] = useState<boolean | null>(null);
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

  /** Re-read file from disk and remount ScalabilityTweaks to refresh tweak states */
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
      setFileExists(true);
    } catch {
      setContent("");
      setFileExists(false);
    }
  }

  const SUB_TABS: { id: SubTab; label: string; Icon: React.ElementType }[] = [
    { id: "scalability", label: "Scalability", Icon: FileText },
    { id: "pak-config", label: "Pak Config", Icon: Package },
  ];

  return (
    <div className="flex w-full flex-1 min-h-0 flex-col gap-6">
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
      <div className={cn("flex flex-1 min-h-0 flex-col", subTab !== "scalability" && "hidden")}>
          <ScalabilityTweaks
            key={reloadKey}
            filePath={filePath}
            setFilePath={setFilePath}
            fileExists={fileExists}
            content={content}
            setContent={setContent}
            detectBadge={detectBadge}
            detecting={detecting}
            onDetect={detectPath}
            onBrowse={browse}
            onSaved={() => setFileExists(true)}
            onReload={reloadContent}
          />
      </div>

      {/* ── Pak Config tab ── */}
      <div className={cn("flex flex-1 min-h-0 flex-col", subTab !== "pak-config" && "hidden")}>
        <PakTweaks gamePath={gamePath} />
      </div>
    </div>
  );
}
