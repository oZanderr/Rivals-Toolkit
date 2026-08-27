import { useState, useCallback, useRef } from "react";

import { invoke } from "@tauri-apps/api/core";
import { AlertTriangle, CheckCircle2, Gauge, Package, Trash2, XCircle } from "lucide-react";

import { GameUserSettingsTweaks } from "./GameUserSettingsTweaks";
import { PakTweaks } from "./PakTweaks";

import { Button } from "@/components/ui/button";
import { Tip } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";

type SubTab = "pak-config" | "game-settings";

interface Props {
  gamePath: string;
  isActive: boolean;
}

export function ConfigTweaks({ gamePath, isActive }: Props) {
  const [subTab, setSubTab] = useState<SubTab>("pak-config");

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

  const SUB_TABS: { id: SubTab; label: string; Icon: React.ElementType }[] = [
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

      {/* Banner: a ban risk for pak tweaks, a much milder overwrite caveat for game-settings, so
          the two do not share a severity. */}
      <div
        className={cn(
          "flex items-center gap-2.5 rounded-md border px-3 py-2",
          subTab === "game-settings" ? "border-warn/20 bg-warn/5" : "border-err/25 bg-err/5"
        )}
      >
        <AlertTriangle
          size={15}
          className={cn("shrink-0", subTab === "game-settings" ? "text-warn" : "text-err")}
        />
        <span
          className={cn(
            "flex-1 text-[12px]",
            subTab === "game-settings" ? "text-warn" : "text-err"
          )}
        >
          {subTab === "game-settings"
            ? "Marvel Rivals overwrites this file when it saves settings. Close the game before editing, and changes may reset when the game writes its own preferences."
            : "The game detects graphics-altering config tweaks, and people have been banned for using them. Use at your own risk."}
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

      {/* ── Pak Config tab ── */}
      <div className={cn("flex flex-1 min-h-0 flex-col", subTab !== "pak-config" && "hidden")}>
        <PakTweaks gamePath={gamePath} isActive={isActive && subTab === "pak-config"} />
      </div>

      {/* ── Game Settings tab ── */}
      <div className={cn("flex flex-1 min-h-0 flex-col", subTab !== "game-settings" && "hidden")}>
        <GameUserSettingsTweaks />
      </div>
    </div>
  );
}
