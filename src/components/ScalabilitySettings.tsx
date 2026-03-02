import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Card } from "@/components/ui/card";
import { Switch } from "@/components/ui/switch";
import { Label } from "@/components/ui/label";
import { Slider } from "@/components/ui/slider";
import { Button } from "@/components/ui/button";
import { Save, RefreshCw, CheckCircle2 } from "lucide-react";
import { cn } from "@/lib/utils";

// ── Types matching Rust backend (serde tag="kind" + flatten) ─────────

interface TweakBase {
  id: string;
  label: string;
  category: string;
  description: string;
  pak_only: boolean;
}

interface RemoveLinesTweak extends TweakBase {
  kind: "RemoveLines";
  lines: { pattern: string; section: string }[];
}

interface ToggleTweak extends TweakBase {
  kind: "Toggle";
  key: string;
  on_value: string;
  off_value: string;
  default_enabled: boolean;
}

interface SliderTweak extends TweakBase {
  kind: "Slider";
  key: string;
  min: number;
  max: number;
  step: number;
  default_value: number;
}

type TweakDefinition = RemoveLinesTweak | ToggleTweak | SliderTweak;

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

interface Props {
  filePath: string;
  content: string;
  setContent: (c: string) => void;
  onSaved: () => void;
}

export function ScalabilitySettings({ filePath, content, setContent, onSaved }: Props) {
  const [definitions, setDefinitions] = useState<TweakDefinition[]>([]);
  const [enabled, setEnabled] = useState<Record<string, boolean>>({});
  const [values, setValues] = useState<Record<string, string>>({});
  const [dirty, setDirty] = useState(false);
  const [status, setStatus] = useState<{ msg: string; type: StatusType } | null>(null);

  const showStatus = (msg: string, type: StatusType = "info") =>
    setStatus({ msg, type });

  // Load tweak definitions once
  useEffect(() => {
    invoke<TweakDefinition[]>("get_tweak_definitions").then(setDefinitions);
  }, []);

  // Detect active tweaks whenever content changes
  const detectTweaks = useCallback(async (text: string) => {
    if (!text) return;
    try {
      const states = await invoke<TweakState[]>("detect_tweaks", { content: text });
      const enabledMap: Record<string, boolean> = {};
      const valuesMap: Record<string, string> = {};
      for (const s of states) {
        enabledMap[s.id] = s.active;
        if (s.current_value != null) valuesMap[s.id] = s.current_value;
      }
      setEnabled(enabledMap);
      setValues(valuesMap);
      setDirty(false);
    } catch (e: any) {
      showStatus(String(e), "err");
    }
  }, []);

  useEffect(() => {
    detectTweaks(content);
  }, [content, detectTweaks]);

  function toggleEnabled(id: string) {
    setEnabled((prev) => ({ ...prev, [id]: !prev[id] }));
    setDirty(true);
  }

  function setValue(id: string, val: string) {
    setValues((prev) => ({ ...prev, [id]: val }));
    setDirty(true);
  }

  async function applyAndSave() {
    try {
      const settings: TweakSetting[] = definitions.filter((d) => !d.pak_only).map((def) => ({
        id: def.id,
        enabled: enabled[def.id] ?? false,
        value: values[def.id] ?? null,
      }));
      const modified = await invoke<string>("apply_tweaks", { content, settings });
      setContent(modified);
      await invoke("write_scalability", { path: filePath, content: modified });
      setDirty(false);
      showStatus("Settings applied and saved.", "ok");
      onSaved();
    } catch (e: any) {
      showStatus(String(e), "err");
    }
  }

  // Group definitions by category (exclude pak-only tweaks)
  const scalabilityDefs = definitions.filter((d) => !d.pak_only);
  const categories = scalabilityDefs.reduce<Record<string, TweakDefinition[]>>((acc, def) => {
    (acc[def.category] ??= []).push(def);
    return acc;
  }, {});

  return (
    <div className="flex flex-col gap-5">
      {Object.entries(categories).map(([category, tweaks]) => (
        <Card key={category} className="flex flex-col gap-4 bg-card p-4">
          <span className="text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
            {category}
          </span>
          <div className="flex flex-col gap-3">
            {tweaks.map((tweak) => (
              <TweakRow
                key={tweak.id}
                tweak={tweak}
                isEnabled={enabled[tweak.id] ?? false}
                currentValue={values[tweak.id]}
                onToggle={() => toggleEnabled(tweak.id)}
                onValueChange={(val) => setValue(tweak.id, val)}
              />
            ))}
          </div>
        </Card>
      ))}

      {/* Apply bar */}
      <div className="flex items-center gap-3">
        <Button variant="green" size="sm" onClick={applyAndSave} disabled={!dirty}>
          <Save size={14} />
          {dirty ? "Apply & Save" : "Up to Date"}
        </Button>
        <Button variant="ghost" size="sm" onClick={() => detectTweaks(content)}>
          <RefreshCw size={14} />
          Reset
        </Button>
        {status && (
          <span
            className={cn(
              "flex items-center gap-1 text-[12px]",
              status.type === "ok"
                ? "text-[var(--color-ok)]"
                : status.type === "err"
                  ? "text-[var(--color-err)]"
                  : "text-muted-foreground",
            )}
          >
            {status.type === "ok" && <CheckCircle2 size={13} />}
            {status.msg}
          </span>
        )}
      </div>
    </div>
  );
}

// ── Individual tweak row ──────────────────────────────────────────────

interface TweakRowProps {
  tweak: TweakDefinition;
  isEnabled: boolean;
  currentValue: string | undefined;
  onToggle: () => void;
  onValueChange: (val: string) => void;
}

function TweakRow({ tweak, isEnabled, currentValue, onToggle, onValueChange }: TweakRowProps) {
  return (
    <div className="flex flex-col gap-2 rounded-md border border-border/50 bg-background px-4 py-3">
      {/* Top line: label + switch */}
      <div className="flex items-start justify-between gap-4">
        <div className="flex flex-col gap-0.5">
          <Label htmlFor={tweak.id} className="text-[13px] font-medium cursor-pointer">
            {tweak.label}
          </Label>
          <span className="text-[11px] leading-snug text-muted-foreground">
            {tweak.description}
          </span>
          <div className="mt-1 flex flex-wrap gap-1">
            <TweakCodes tweak={tweak} />
          </div>
        </div>
        <Switch id={tweak.id} checked={isEnabled} onCheckedChange={onToggle} />
      </div>

      {/* Slider control (only for Slider kind) */}
      {tweak.kind === "Slider" && (
        <SliderControl
          tweak={tweak}
          isEnabled={isEnabled}
          currentValue={currentValue}
          onValueChange={onValueChange}
        />
      )}
    </div>
  );
}

// ── CVar code pills ──────────────────────────────────────────────────

function TweakCodes({ tweak }: { tweak: TweakDefinition }) {
  const codeClass =
    "rounded bg-muted px-1 py-0.5 font-mono text-[10px] text-muted-foreground";

  switch (tweak.kind) {
    case "RemoveLines":
      return tweak.lines.map((line, i) => (
        <code key={i} className={codeClass}>
          {line.pattern}
        </code>
      ));
    case "Toggle":
      return (
        <code className={codeClass}>
          {tweak.key}={tweak.on_value}/{tweak.off_value}
        </code>
      );
    case "Slider":
      return (
        <code className={codeClass}>
          {tweak.key} ({tweak.min}–{tweak.max})
        </code>
      );
  }
}

// ── Slider sub-control ───────────────────────────────────────────────

function SliderControl({
  tweak,
  isEnabled,
  currentValue,
  onValueChange,
}: {
  tweak: SliderTweak;
  isEnabled: boolean;
  currentValue: string | undefined;
  onValueChange: (val: string) => void;
}) {
  const numVal = currentValue != null ? parseFloat(currentValue) : tweak.default_value;
  const displayVal = isNaN(numVal) ? tweak.default_value : numVal;

  // Infer decimal places from step
  const stepStr = String(tweak.step);
  const decimals = stepStr.includes(".") ? stepStr.split(".")[1].length : 0;

  return (
    <div
      className={cn(
        "flex items-center gap-3 pt-1",
        !isEnabled && "opacity-40 pointer-events-none",
      )}
    >
      <Slider
        min={tweak.min}
        max={tweak.max}
        step={tweak.step}
        value={[displayVal]}
        onValueChange={([v]) => onValueChange(v.toFixed(decimals))}
        className="flex-1"
      />
      <span className="w-12 text-right font-mono text-[12px] text-muted-foreground tabular-nums">
        {displayVal.toFixed(decimals)}
      </span>
    </div>
  );
}
