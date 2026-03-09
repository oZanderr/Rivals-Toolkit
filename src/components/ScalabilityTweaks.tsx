import { useState, useEffect, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Card } from "@/components/ui/card";
import { Switch } from "@/components/ui/switch";
import { Label } from "@/components/ui/label";
import { Slider } from "@/components/ui/slider";
import { Button } from "@/components/ui/button";
import { Save, RefreshCw, CheckCircle2, Trash2 } from "lucide-react";
import { Badge } from "@/components/ui/badge";
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
  onReload: () => void;
}

export function ScalabilityTweaks({ filePath, content, setContent, onSaved, onReload }: Props) {
  const [definitions, setDefinitions] = useState<TweakDefinition[]>([]);
  const [defsLoaded, setDefsLoaded] = useState(false);
  const [enabled, setEnabled] = useState<Record<string, boolean>>({});
  const [values, setValues] = useState<Record<string, string>>({});
  const [savedEnabled, setSavedEnabled] = useState<Record<string, boolean>>({});
  const [savedValues, setSavedValues] = useState<Record<string, string>>({});
  const [status, setStatus] = useState<{ msg: string; type: StatusType } | null>(null);
  const statusTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [clearingCache, setClearingCache] = useState(false);

  const showStatus = (msg: string, type: StatusType = "info") => {
    if (statusTimer.current) clearTimeout(statusTimer.current);
    setStatus({ msg, type });
    statusTimer.current = setTimeout(() => setStatus(null), 4000);
  };

  // Load tweak definitions once
  useEffect(() => {
    invoke<TweakDefinition[]>("get_tweak_definitions").then((defs) => {
      setDefinitions(defs);
      setDefsLoaded(true);
    });
  }, []);

  // Detect active tweaks whenever content changes
  const detectTweaks = useCallback(async (text: string) => {
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
      setSavedEnabled(enabledMap);
      setSavedValues(valuesMap);
    } catch (e: any) {
      showStatus(String(e), "err");
    }
  }, []);

  useEffect(() => {
    detectTweaks(content);
  }, [content, detectTweaks]);

  function toggleEnabled(id: string) {
    setEnabled((prev) => ({ ...prev, [id]: !prev[id] }));
  }

  function setValue(id: string, val: string) {
    setValues((prev) => ({ ...prev, [id]: val }));
  }

  async function applyAndSave() {
    try {
      const settings: TweakSetting[] = definitions.filter((d) => !d.pak_only).map((def) => ({
        id: def.id,
        enabled: enabled[def.id] ?? false,
        value: def.kind === "Slider"
          ? String(values[def.id] != null ? values[def.id] : def.default_value)
          : (values[def.id] ?? null),
      }));
      const modified = await invoke<string>("apply_tweaks", { content, settings });
      setContent(modified);
      await invoke("write_scalability", { path: filePath, content: modified });
      showStatus("Settings applied and saved.", "ok");
      onSaved();
    } catch (e: any) {
      showStatus(String(e), "err");
    }
  }

  async function clearShaderCache() {
    setClearingCache(true);
    try {
      const msg = await invoke<string>("clear_shader_cache");
      showStatus(msg, "ok");
    } catch (e: any) {
      showStatus(String(e), "err");
    } finally {
      setClearingCache(false);
    }
  }

  // Group definitions by category (exclude pak-only tweaks)
  const scalabilityDefs = definitions.filter((d) => !d.pak_only);

  // Compute pending changes vs saved baseline — show actual variables like Pak Config
  interface PendingChange { id: string; kind: "set" | "remove"; display: string; }
  const pendingChanges: PendingChange[] = scalabilityDefs.flatMap((def) => {
    const changes: PendingChange[] = [];
    const isEnabled = enabled[def.id] ?? false;
    const wasEnabled = savedEnabled[def.id] ?? false;
    const toggleChanged = isEnabled !== wasEnabled;

    if (def.kind === "RemoveLines") {
      if (toggleChanged) {
        // Each line pattern is being added or removed
        def.lines.forEach((line, i) => {
          changes.push({
            id: `${def.id}_line${i}`,
            kind: isEnabled ? "remove" : "set",
            display: line.pattern,
          });
        });
      }
    } else if (def.kind === "Toggle") {
      if (toggleChanged) {
        changes.push({
          id: def.id,
          kind: "set",
          display: `${def.key}=${isEnabled ? def.on_value : def.off_value}`,
        });
      }
    } else if (def.kind === "Slider") {
      const cur = values[def.id];
      const prev = savedValues[def.id];
      const valueChanged = cur !== undefined && cur !== prev;
      if (toggleChanged && !isEnabled) {
        // Tweak turned off — line is removed
        changes.push({ id: def.id, kind: "remove", display: def.key });
      } else if (isEnabled && (toggleChanged || valueChanged)) {
        // Turned on or value changed while on
        const displayVal = cur ?? String(def.default_value);
        changes.push({ id: def.id, kind: "set", display: `${def.key}=${displayVal}` });
      }
    }

    return changes;
  });
  const dirty = pendingChanges.length > 0;
  const categories = scalabilityDefs.reduce<Record<string, TweakDefinition[]>>((acc, def) => {
    (acc[def.category] ??= []).push(def);
    return acc;
  }, {});

  if (!defsLoaded) return null;

  return (
    <div className="flex flex-col gap-5">
      <div className="grid gap-5 xl:grid-cols-2 2xl:grid-cols-3">
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
      </div>

      {/* Pending changes + apply bar */}
      <div className="flex flex-col gap-4">
        {pendingChanges.length > 0 && (
          <div className="flex flex-col gap-1.5">
            <span className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
              Pending Changes ({pendingChanges.length})
            </span>
            <div className="flex flex-wrap gap-1">
              {pendingChanges.map((c) => (
                <Badge
                  key={c.id}
                  variant={c.kind === "remove" ? "destructive" : "secondary"}
                  className="text-[10px] font-mono px-1.5 py-0"
                >
                  {c.kind === "remove" ? `- ${c.display}` : c.display}
                </Badge>
              ))}
            </div>
          </div>
        )}
        <div className="flex items-center gap-3">
        <Button variant="green" size="sm" onClick={applyAndSave} disabled={!dirty}>
          <Save size={14} />
          {dirty ? "Apply & Save" : "Up to Date"}
        </Button>
        <Button variant="outline" size="sm" onClick={clearShaderCache} disabled={clearingCache}>
          <Trash2 size={14} />
          {clearingCache ? "Clearing…" : "Clear Shader Cache"}
        </Button>
        <Button variant="ghost" size="sm" onClick={onReload}>
          <RefreshCw size={14} />
          Reload
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
