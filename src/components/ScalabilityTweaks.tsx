import { useState, useEffect, useCallback, useRef } from "react";

import { invoke } from "@tauri-apps/api/core";
import { openPath } from "@tauri-apps/plugin-opener";
import {
  Save,
  RefreshCw,
  CheckCircle2,
  XCircle,
  Trash2,
  FolderOpen,
  Search,
  Info,
} from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Slider } from "@/components/ui/slider";
import { Switch } from "@/components/ui/switch";
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
  lines: {
    pattern: string;
    scalability_section?: string | null;
    engine_section?: string | null;
    replace_with?: string | null;
  }[];
  remove_only: boolean;
}

interface ToggleTweak extends TweakBase {
  kind: "Toggle";
  key: string;
  on_value: string;
  off_value?: string;
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
  setFilePath: (p: string) => void;
  fileExists: boolean | null;
  content: string;
  setContent: (c: string) => void;
  reloadSignal: number;
  detectBadge: string | null;
  detecting: boolean;
  onDetect: () => void;
  onBrowse: () => void;
  onSaved: (content: string) => void;
  onReload: () => void;
}

export function ScalabilityTweaks({
  filePath,
  setFilePath,
  fileExists,
  content,
  setContent,
  reloadSignal,
  detectBadge,
  detecting,
  onDetect,
  onBrowse,
  onSaved,
  onReload,
}: Props) {
  const [definitions, setDefinitions] = useState<TweakDefinition[]>([]);
  const [defsLoaded, setDefsLoaded] = useState(false);
  const [enabled, setEnabled] = useState<Record<string, boolean>>({});
  const [values, setValues] = useState<Record<string, string>>({});
  const [savedEnabled, setSavedEnabled] = useState<Record<string, boolean>>({});
  const [savedValues, setSavedValues] = useState<Record<string, string>>({});
  const [status, setStatus] = useState<{ msg: string; type: StatusType } | null>(null);
  const statusTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

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
    } catch (e: unknown) {
      showStatus(String(e), "err");
    }
  }, []);

  useEffect(() => {
    detectTweaks(content);
  }, [content, detectTweaks]);

  // Re-detect when an explicit reload is triggered, even if content is unchanged
  useEffect(() => {
    if (reloadSignal > 0) detectTweaks(content);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- intentional: only fires on reload signal, not on every content change
  }, [reloadSignal]);

  function toggleEnabled(id: string) {
    setEnabled((prev) => ({ ...prev, [id]: !prev[id] }));
  }

  function setValue(id: string, val: string) {
    setValues((prev) => ({ ...prev, [id]: val }));
  }

  async function applyAndSave() {
    try {
      const settings: TweakSetting[] = definitions
        .filter((d) => !d.pak_only)
        .map((def) => ({
          id: def.id,
          enabled: enabled[def.id] ?? false,
          value:
            def.kind === "Slider"
              ? String(values[def.id] != null ? values[def.id] : def.default_value)
              : (values[def.id] ?? null),
        }));
      const modified = await invoke<string>("apply_tweaks", { content, settings });
      setContent(modified);
      await invoke("write_scalability", { path: filePath, content: modified });
      showStatus("Settings applied and saved.", "ok");
      onSaved(modified);
    } catch (e: unknown) {
      showStatus(String(e), "err");
    }
  }

  async function clearShaderCache() {
    try {
      const msg = await invoke<string>("clear_shader_cache");
      showStatus(msg, "ok");
    } catch (e: unknown) {
      showStatus(String(e), "err");
    }
  }

  // Group definitions by category (exclude pak-only tweaks)
  const scalabilityDefs = definitions.filter((d) => !d.pak_only);

  // Compute pending changes vs saved baseline — show actual variables like Pak Config
  interface PendingChange {
    id: string;
    kind: "set" | "remove";
    display: string;
  }
  const pendingChanges: PendingChange[] = scalabilityDefs.flatMap((def) => {
    const changes: PendingChange[] = [];
    const isEnabled = enabled[def.id] ?? false;
    const wasEnabled = savedEnabled[def.id] ?? false;
    const toggleChanged = isEnabled !== wasEnabled;

    if (def.kind === "RemoveLines") {
      if (toggleChanged) {
        def.lines.forEach((line, i) => {
          const isReplace = isEnabled && line.replace_with != null;
          changes.push({
            id: `${def.id}_line${i}`,
            kind: isEnabled && !isReplace ? "remove" : "set",
            display: isReplace ? line.replace_with! : line.pattern,
          });
        });
      }
    } else if (def.kind === "Toggle") {
      if (toggleChanged) {
        changes.push({
          id: def.id,
          kind: !isEnabled && def.off_value === undefined ? "remove" : "set",
          display:
            !isEnabled && def.off_value === undefined
              ? def.key
              : `${def.key}=${isEnabled ? def.on_value : def.off_value}`,
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
    <div className="flex flex-1 min-h-0 flex-col gap-5">
      <div className="flex-1 overflow-y-auto pr-6">
        <div className="flex flex-col gap-5">
          {/* Config file location */}
          <Card className="flex flex-col gap-3 p-4 bg-card">
            <div className="flex items-center justify-between">
              <div className="flex items-center gap-2">
                <span className="text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
                  Config File
                </span>
                {detectBadge && (
                  <span
                    className={cn(
                      "flex items-center gap-1 text-[12px] font-medium",
                      detectBadge === "Not found"
                        ? "text-[var(--color-warn)]"
                        : "text-[var(--color-ok)]"
                    )}
                  >
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
                placeholder="Path to Scalability.ini\u2026"
                title={filePath}
              />
              <Button variant="outline" size="sm" onClick={onBrowse}>
                <FolderOpen size={14} />
                Browse
              </Button>
              <Button variant="blue" size="sm" onClick={onDetect} disabled={detecting}>
                <Search size={14} className={cn(detecting && "animate-pulse")} />
                Redetect
              </Button>
            </div>
          </Card>

          {fileExists === false && (
            <div className="flex items-start gap-2.5 rounded-md border border-border bg-muted/40 px-4 py-3 text-[12px] text-muted-foreground">
              <Info size={14} className="mt-0.5 shrink-0" />
              <span>
                <strong className="font-semibold text-foreground">No Scalability.ini found.</strong>{" "}
                You can still configure tweaks here, the file will be created automatically when you
                save.
              </span>
            </div>
          )}

          <div className="grid gap-5 xl:grid-cols-2 2xl:grid-cols-3">
            {Object.entries(categories).map(([category, tweaks]) => (
              <Card key={category} className="flex flex-col gap-4 bg-card p-4">
                <span className="text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
                  {category}
                </span>
                <div className="flex flex-col gap-3">
                  {tweaks.map((tweak) => {
                    const isOn = enabled[tweak.id] ?? false;
                    const lockedOn = tweak.kind === "RemoveLines" && tweak.remove_only && isOn;
                    return (
                      <TweakRow
                        key={tweak.id}
                        tweak={tweak}
                        isEnabled={isOn}
                        currentValue={values[tweak.id]}
                        disabled={lockedOn}
                        disabledReason={
                          lockedOn ? "Remove-only tweak cannot be reverted" : undefined
                        }
                        onToggle={() => toggleEnabled(tweak.id)}
                        onValueChange={(val) => setValue(tweak.id, val)}
                      />
                    );
                  })}
                </div>
              </Card>
            ))}
          </div>
        </div>
      </div>

      {/* Apply bar — fixed footer, outside the scroll area */}
      <div className="flex flex-col gap-3 border-t border-border pt-3 pb-1">
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
          <Button variant="outline" size="sm" onClick={clearShaderCache}>
            <Trash2 size={14} />
            Clear Shader Cache
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
                    : "text-muted-foreground"
              )}
            >
              {status.type === "ok" && <CheckCircle2 size={13} />}
              {status.type === "err" && <XCircle size={13} />}
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
  disabled?: boolean;
  disabledReason?: string;
  onToggle: () => void;
  onValueChange: (val: string) => void;
}

function TweakRow({
  tweak,
  isEnabled,
  currentValue,
  disabled,
  disabledReason,
  onToggle,
  onValueChange,
}: TweakRowProps) {
  return (
    <div
      className={cn(
        "flex flex-col gap-2 rounded-md border border-border/50 bg-background px-4 py-3",
        disabled && "opacity-50"
      )}
    >
      {/* Top line: label + switch */}
      <div className="flex items-start justify-between gap-4">
        <div className="flex flex-col gap-0.5">
          <Label
            htmlFor={tweak.id}
            className={cn("text-[13px] font-medium", !disabled && "cursor-pointer")}
          >
            {tweak.label}
          </Label>
          <span className="text-[11px] leading-snug text-muted-foreground">
            {tweak.description}
          </span>
          {disabledReason && (
            <span className="text-[11px] leading-snug text-[var(--color-warn)] mt-0.5">
              {disabledReason}
            </span>
          )}
          <div className="mt-1 flex flex-wrap gap-1">
            <TweakCodes tweak={tweak} />
          </div>
        </div>
        <Switch id={tweak.id} checked={isEnabled} onCheckedChange={onToggle} disabled={disabled} />
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
  const codeClass = "rounded bg-muted px-1 py-0.5 font-mono text-[10px] text-muted-foreground";

  switch (tweak.kind) {
    case "RemoveLines":
      if (tweak.remove_only) return null;
      return tweak.lines.map((line, i) => (
        <code key={i} className={codeClass}>
          {line.pattern}
        </code>
      ));
    case "Toggle":
      return (
        <code className={codeClass}>
          {tweak.key}={tweak.on_value}
          {tweak.off_value !== undefined ? `/${tweak.off_value}` : ""}
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
      className={cn("flex items-center gap-3 pt-1", !isEnabled && "opacity-40 pointer-events-none")}
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
