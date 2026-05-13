import { useCallback, useEffect, useRef, useState } from "react";

import { invoke } from "@tauri-apps/api/core";
import { openPath } from "@tauri-apps/plugin-opener";
import { AlertTriangle, CheckCircle2, RefreshCw, Save, Undo2, XCircle } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Slider } from "@/components/ui/slider";
import { Switch } from "@/components/ui/switch";
import { Tip } from "@/components/ui/tooltip";
import { useSaveHotkeys } from "@/hooks/useSaveHotkeys";
import { useScrollAtBottom } from "@/hooks/useScrollAtBottom";
import { cn } from "@/lib/utils";

interface ToggleTweak {
  id: string;
  label: string;
  category: string;
  description: string;
  pak_only: boolean;
  kind: "Toggle";
  key: string;
  on_value: string;
  off_value?: string;
  default_enabled: boolean;
}

interface SliderTweak {
  id: string;
  label: string;
  category: string;
  description: string;
  pak_only: boolean;
  kind: "Slider";
  key: string;
  min: number;
  max: number;
  step: number;
  default_value: number;
}

type TweakDefinition = ToggleTweak | SliderTweak;

interface TweakState {
  id: string;
  active: boolean;
  current_value: string | null;
}

interface TweakSetting {
  id: string;
  enabled: boolean;
  value: string | null;
}

type StatusType = "ok" | "err" | "info";

export function GameUserSettingsTweaks() {
  const [filePath, setFilePath] = useState("");
  const [content, setContent] = useState("");
  const [fileExists, setFileExists] = useState<boolean | null>(null);
  const [definitions, setDefinitions] = useState<TweakDefinition[]>([]);
  const [defsLoaded, setDefsLoaded] = useState(false);
  const [enabled, setEnabled] = useState<Record<string, boolean>>({});
  const [values, setValues] = useState<Record<string, string>>({});
  const [savedEnabled, setSavedEnabled] = useState<Record<string, boolean>>({});
  const [savedValues, setSavedValues] = useState<Record<string, string>>({});
  const [status, setStatus] = useState<{ msg: string; type: StatusType } | null>(null);
  const [detecting, setDetecting] = useState(false);
  const statusTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const showStatus = useCallback((msg: string, type: StatusType = "info") => {
    if (statusTimer.current) clearTimeout(statusTimer.current);
    setStatus({ msg, type });
    statusTimer.current = setTimeout(() => setStatus(null), 4000);
  }, []);

  // Load definitions once
  useEffect(() => {
    invoke<TweakDefinition[]>("get_game_user_settings_definitions").then((defs) => {
      setDefinitions(defs);
      setDefsLoaded(true);
    });
  }, []);

  const detectTweaks = useCallback(
    async (text: string) => {
      try {
        const states = await invoke<TweakState[]>("detect_game_user_settings_tweaks", {
          content: text,
        });
        const enabledMap: Record<string, boolean> = {};
        const valuesMap: Record<string, string> = {};
        for (const s of states) {
          enabledMap[s.id] = s.active;
          if (s.current_value != null) valuesMap[s.id] = s.current_value;
        }
        setEnabled(enabledMap);
        setValues(valuesMap);
        setSavedEnabled(enabledMap);
        setSavedValues(valuesMap);
      } catch (e: unknown) {
        showStatus(String(e), "err");
      }
    },
    [showStatus]
  );

  const loadFile = useCallback(
    async (path: string) => {
      try {
        const text = await invoke<string>("read_game_user_settings", { path });
        setContent(text);
        setFileExists(true);
        await detectTweaks(text);
      } catch {
        setContent("");
        setFileExists(false);
      }
    },
    [detectTweaks]
  );

  const detectPath = useCallback(async () => {
    setDetecting(true);
    try {
      const p = await invoke<string>("get_game_user_settings_path");
      setFilePath(p);
      await loadFile(p);
    } catch (e) {
      showStatus(String(e), "err");
    } finally {
      setDetecting(false);
    }
  }, [loadFile, showStatus]);

  useEffect(() => {
    detectPath();
    // eslint-disable-next-line react-hooks/exhaustive-deps -- initial mount only
  }, []);

  const dirty =
    JSON.stringify(enabled) !== JSON.stringify(savedEnabled) ||
    JSON.stringify(values) !== JSON.stringify(savedValues);

  const RADIO_GROUPS: string[][] = [
    ["gus_dlss_fg", "gus_fsr_fg", "gus_xe_fg"],
    ["gus_nvidia_reflex", "gus_amd_anti_lag", "gus_xe_low_latency"],
  ];

  function toggleEnabled(id: string) {
    setEnabled((prev) => {
      const enabling = !(prev[id] ?? false);
      const next = { ...prev, [id]: enabling };
      if (enabling) {
        const group = RADIO_GROUPS.find((g) => g.includes(id));
        if (group) {
          for (const other of group) {
            if (other !== id) next[other] = false;
          }
        }
      }
      return next;
    });
  }

  function setValue(id: string, val: string) {
    setValues((prev) => ({ ...prev, [id]: val }));
  }

  function buildCurrentSettings(): TweakSetting[] {
    return definitions.map((def) => ({
      id: def.id,
      // Sliders are always written; toggles follow user switch state.
      enabled: def.kind === "Slider" ? true : (enabled[def.id] ?? false),
      value:
        def.kind === "Slider"
          ? String(values[def.id] != null ? values[def.id] : def.default_value)
          : (values[def.id] ?? null),
    }));
  }

  async function applyAndSave() {
    if (!filePath) {
      showStatus("Game user settings path not detected.", "err");
      return;
    }
    try {
      const modified = await invoke<string>("apply_game_user_settings_tweaks", {
        content,
        settings: buildCurrentSettings(),
      });
      await invoke("write_game_user_settings", { path: filePath, content: modified });
      setContent(modified);
      setFileExists(true);
      setSavedEnabled(enabled);
      setSavedValues(values);
      showStatus("Saved", "ok");
    } catch (e) {
      showStatus(String(e), "err");
    }
  }

  const discardChanges = useCallback(() => {
    setEnabled(savedEnabled);
    setValues(savedValues);
  }, [savedEnabled, savedValues]);

  useSaveHotkeys({ dirty, onSave: applyAndSave, onDiscard: discardChanges });

  const { atBottom, scrollRef, sentinelRef } = useScrollAtBottom();

  if (!defsLoaded) return null;

  const categories = definitions.reduce<Record<string, TweakDefinition[]>>((acc, def) => {
    (acc[def.category] ??= []).push(def);
    return acc;
  }, {});
  const categoryOrder = [
    "Display",
    "Frame Caps",
    "Latency",
    "Frame Generation",
    "Upscaling",
    "Quality Groups",
  ];
  const orderedCategories = categoryOrder.filter((c) => categories[c]);

  const pendingChanges = definitions.flatMap((def) => {
    const curVal = values[def.id];
    const savedVal = savedValues[def.id];
    if (def.kind === "Slider") {
      if (curVal !== savedVal) {
        return [{ id: def.id, label: def.label, kind: "update" as const }];
      }
      return [];
    }
    const isOn = enabled[def.id] ?? false;
    const wasOn = savedEnabled[def.id] ?? false;
    if (isOn !== wasOn) {
      return [{ id: def.id, label: def.label, kind: isOn ? "add" : ("remove" as const) }];
    }
    return [];
  });

  return (
    <div className="flex flex-1 min-h-0 flex-col">
      <div ref={scrollRef} className="flex-1 min-h-0 overflow-y-auto scrollbar-gutter-stable">
        <div className="flex flex-col gap-5">
          {/* File location card */}
          <div className="flex flex-col overflow-hidden rounded-md border border-border">
            <div className="flex items-center justify-between border-b border-border bg-card px-3 py-2">
              <div className="flex min-w-0 flex-1 items-center gap-2">
                <span className="shrink-0 text-sm font-semibold">Config File</span>
                {status && (
                  <Tip content={status.msg}>
                    <span
                      className={cn(
                        "flex min-w-0 items-center gap-1 text-[12px] font-medium",
                        status.type === "ok"
                          ? "text-ok"
                          : status.type === "err"
                            ? "text-err"
                            : "text-muted-foreground"
                      )}
                    >
                      {status.type === "ok" && (
                        <CheckCircle2 size={13} strokeWidth={2.5} className="shrink-0" />
                      )}
                      {status.type === "err" && (
                        <XCircle size={13} strokeWidth={2.5} className="shrink-0" />
                      )}
                      <span className="truncate">{status.msg}</span>
                    </span>
                  </Tip>
                )}
                {fileExists === false && (
                  <span className="flex items-center gap-1 text-[12px] font-medium text-warn">
                    <AlertTriangle size={13} className="shrink-0" />
                    File not found. Launch the game once to generate it.
                  </span>
                )}
              </div>
              <div className="flex shrink-0 items-center gap-0.5">
                <Tip content="Reload from file">
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    onClick={async () => {
                      if (!filePath) return;
                      await loadFile(filePath);
                      showStatus("Reloaded", "ok");
                    }}
                    disabled={!filePath || detecting}
                  >
                    <RefreshCw size={14} className={cn(detecting && "animate-spin")} />
                  </Button>
                </Tip>
                <Button
                  variant="ghost"
                  size="sm"
                  disabled={!filePath}
                  onClick={() => filePath && openPath(filePath.replace(/[/\\][^/\\]+$/, ""))}
                >
                  Show in Explorer
                </Button>
              </div>
            </div>
            <div className="px-3 py-2">
              <span className="block truncate font-mono text-[12px] text-muted-foreground">
                {filePath || "not detected"}
              </span>
            </div>
          </div>

          {/* Tweak categories */}
          {orderedCategories.map((cat) => (
            <div
              key={cat}
              className="flex flex-col overflow-hidden rounded-md border border-border"
            >
              <div className="flex items-center gap-2 border-b border-border bg-card px-3 py-2">
                <span className="text-sm font-semibold">{cat}</span>
              </div>
              <div className="flex flex-col divide-y divide-border/50">
                {categories[cat].map((def) => (
                  <TweakRow
                    key={def.id}
                    tweak={def}
                    isEnabled={enabled[def.id] ?? false}
                    currentValue={values[def.id]}
                    onToggle={() => toggleEnabled(def.id)}
                    onValueChange={(v) => setValue(def.id, v)}
                  />
                ))}
              </div>
            </div>
          ))}
        </div>
        <div ref={sentinelRef} aria-hidden className="h-px w-full shrink-0" />
      </div>

      {!atBottom && (
        <div
          aria-hidden
          className="pointer-events-none -mt-8 h-8 shrink-0 bg-linear-to-t from-background to-transparent"
        />
      )}

      {dirty && (
        <div className="flex shrink-0 items-center gap-2 pt-2">
          <div className="flex min-w-0 flex-1 flex-wrap items-center gap-1.5">
            <span className="text-[11px] font-semibold uppercase text-muted-foreground">
              Pending ({pendingChanges.length})
            </span>
            {pendingChanges.map((c) => (
              <Badge
                key={c.id}
                variant="outline"
                className={cn(
                  "rounded-sm px-1.5 py-0 text-[11px] font-mono",
                  c.kind === "remove"
                    ? "border-destructive/40 bg-destructive/10 text-destructive"
                    : "border-ok/40 bg-ok/10 text-ok"
                )}
              >
                {c.kind === "remove" ? `- ${c.label}` : c.label}
              </Badge>
            ))}
          </div>
          <Button variant="outline" size="sm" onClick={discardChanges} disabled={!dirty}>
            <Undo2 size={14} />
            Discard
          </Button>
          <Button variant="blue" size="sm" onClick={applyAndSave} disabled={!dirty}>
            <Save size={14} />
            Save
          </Button>
        </div>
      )}
    </div>
  );
}

function TweakRow({
  tweak,
  isEnabled,
  currentValue,
  onToggle,
  onValueChange,
}: {
  tweak: TweakDefinition;
  isEnabled: boolean;
  currentValue: string | undefined;
  onToggle: () => void;
  onValueChange: (val: string) => void;
}) {
  return (
    <div className="flex flex-col gap-2 px-3 py-3">
      <div className="flex items-center justify-between gap-3">
        <div className="flex min-w-0 flex-1 flex-col gap-0.5">
          <Label className="text-[13px] font-medium">{tweak.label}</Label>
          <p className="text-[11px] text-muted-foreground">{tweak.description}</p>
        </div>
        {tweak.kind === "Toggle" && <Switch checked={isEnabled} onCheckedChange={onToggle} />}
      </div>
      {tweak.kind === "Slider" && (
        <div className="flex items-center gap-3">
          <Slider
            min={tweak.min}
            max={tweak.max}
            step={tweak.step}
            value={[Number(currentValue ?? tweak.default_value)]}
            onValueChange={(vals) => onValueChange(String(vals[0]))}
            className="flex-1"
          />
          <Input
            value={currentValue ?? String(tweak.default_value)}
            onChange={(e) => onValueChange(e.target.value)}
            className="h-7 w-24 font-mono text-[12px]"
          />
        </div>
      )}
    </div>
  );
}
