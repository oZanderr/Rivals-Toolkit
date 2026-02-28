import { useState, useEffect, useRef, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Package,
  RefreshCw,
  Save,
  CheckCircle2,
  XCircle,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Switch } from "@/components/ui/switch";
import { Label } from "@/components/ui/label";
import { Slider as SliderUI } from "@/components/ui/slider";
import { cn } from "@/lib/utils";

// ── Types matching Rust backend ──────────────────────────────────────

interface PakIniInfo {
  pak_name: string;
  pak_path: string;
  has_device_profiles: boolean;
  has_engine_ini: boolean;
  device_profiles_entry: string | null;
  engine_ini_entry: string | null;
}

interface PakTweakState {
  key: string;
  value: string;
  source: string;
}

interface PakTweakEdit {
  key: string;
  value: string | null;
}

// ── Tweak definition types (matching Rust backend) ───────────────────

interface TweakBase {
  id: string;
  label: string;
  category: string;
  description: string;
  pak_only: boolean;
}

interface RemoveLinesTweak extends TweakBase {
  kind: "RemoveLines";
  lines: string[];
}

interface ToggleTweak extends TweakBase {
  kind: "Toggle";
  key: string;
  on_value: string;
  off_value: string;
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

interface Props {
  gamePath: string;
}

export function PakTweaks({ gamePath }: Props) {
  const [paks, setPaks] = useState<PakIniInfo[]>([]);
  const [selectedPak, setSelectedPak] = useState<PakIniInfo | null>(null);
  const [tweaks, setTweaks] = useState<PakTweakState[]>([]);
  const [edits, setEdits] = useState<PakTweakEdit[]>([]);
  const [scanning, setScanning] = useState(false);
  const [loading, setLoading] = useState(false);
  const [applying, setApplying] = useState(false);
  const [dirty, setDirty] = useState(false);
  const [showBadge, setShowBadge] = useState(false);
  const [badgeMsg, setBadgeMsg] = useState("");
  const badgeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Quick settings state
  const [definitions, setDefinitions] = useState<TweakDefinition[]>([]);
  const [tweakEnabled, setTweakEnabled] = useState<Record<string, boolean>>({});
  const [tweakValues, setTweakValues] = useState<Record<string, string>>({});

  const flashBadge = (msg: string) => {
    if (badgeTimer.current) clearTimeout(badgeTimer.current);
    setBadgeMsg(msg);
    setShowBadge(true);
    badgeTimer.current = setTimeout(() => setShowBadge(false), 4000);
  };

  useEffect(() => {
    if (gamePath) scan();
  }, [gamePath]);

  // Load tweak definitions once
  useEffect(() => {
    invoke<TweakDefinition[]>("get_tweak_definitions").then(setDefinitions);
  }, []);

  // Detect quick tweak states when raw CVars change
  const detectQuickStates = useCallback(() => {
    if (definitions.length === 0) return;
    const cvarMap = new Map<string, string>();
    for (const t of tweaks) {
      cvarMap.set(t.key.toLowerCase(), t.value);
    }

    const enabledMap: Record<string, boolean> = {};
    const valuesMap: Record<string, string> = {};

    for (const def of definitions) {
      switch (def.kind) {
        case "RemoveLines": {
          const anyFound = def.lines.some((line) => {
            const eqIdx = line.indexOf("=");
            if (eqIdx < 0) return cvarMap.has(line.toLowerCase());
            const key = line.substring(0, eqIdx).toLowerCase();
            const val = line.substring(eqIdx + 1);
            const current = cvarMap.get(key);
            return current !== undefined && current === val;
          });
          enabledMap[def.id] = !anyFound;
          break;
        }
        case "Toggle": {
          const current = cvarMap.get(def.key.toLowerCase());
          // If key not found, the engine default is active → assume enabled
          enabledMap[def.id] = current !== undefined ? current === def.on_value : true;
          if (current !== undefined) valuesMap[def.id] = current;
          break;
        }
        case "Slider": {
          const current = cvarMap.get(def.key.toLowerCase());
          enabledMap[def.id] = current !== undefined;
          if (current !== undefined) valuesMap[def.id] = current;
          break;
        }
      }
    }

    setTweakEnabled(enabledMap);
    setTweakValues(valuesMap);
  }, [tweaks, definitions]);

  useEffect(() => {
    detectQuickStates();
  }, [detectQuickStates]);

  function toggleQuickTweak(id: string) {
    const def = definitions.find((d) => d.id === id);
    if (!def) return;

    const newEnabled = !tweakEnabled[id];
    setTweakEnabled((prev) => ({ ...prev, [id]: newEnabled }));

    switch (def.kind) {
      case "RemoveLines":
        for (const line of def.lines) {
          const eqIdx = line.indexOf("=");
          const key = eqIdx >= 0 ? line.substring(0, eqIdx) : line;
          const val = eqIdx >= 0 ? line.substring(eqIdx + 1) : "0";
          if (newEnabled) {
            queueEdit(key, null); // Fix ON → remove key
          } else {
            queueEdit(key, val); // Fix OFF → add key=value
          }
        }
        break;
      case "Toggle":
        queueEdit(def.key, newEnabled ? def.on_value : def.off_value);
        break;
      case "Slider":
        if (newEnabled) {
          const val = tweakValues[id] ?? String(def.default_value);
          queueEdit(def.key, val);
        } else {
          queueEdit(def.key, null);
        }
        break;
    }
  }

  function setQuickTweakValue(id: string, val: string) {
    const def = definitions.find((d) => d.id === id);
    if (!def || def.kind !== "Slider") return;
    setTweakValues((prev) => ({ ...prev, [id]: val }));
    queueEdit((def as SliderTweak).key, val);
  }

  async function scan() {
    if (!gamePath) return;
    setScanning(true);
    try {
      const results = await invoke<PakIniInfo[]>("scan_mod_paks_for_ini", {
        gameRoot: gamePath,
      });
      setPaks(results);
      if (results.length === 0) {
        setSelectedPak(null);
        setTweaks([]);
        setEdits([]);
        setDirty(false);
      } else if (results.length === 1) {
        // Auto-select the only available pak
        await selectPak(results[0]);
      } else {
        // If previously selected pak is gone, deselect
        if (selectedPak && !results.find((p) => p.pak_path === selectedPak.pak_path)) {
          setSelectedPak(null);
          setTweaks([]);
          setEdits([]);
          setDirty(false);
        }
      }
    } catch (e: any) {
      console.error("Scan failed:", e);
    } finally {
      setScanning(false);
    }
  }

  async function selectPak(pak: PakIniInfo) {
    setSelectedPak(pak);
    setLoading(true);
    try {
      const states = await invoke<PakTweakState[]>("read_pak_tweak_values", {
        pakPath: pak.pak_path,
      });
      setTweaks(states);
      setEdits([]);
      setDirty(false);
    } catch (e: any) {
      console.error("Load failed:", e);
      setTweaks([]);
    } finally {
      setLoading(false);
    }
  }

  function queueEdit(key: string, value: string | null) {
    setEdits((prev) => {
      const existing = prev.findIndex((e) => e.key.toLowerCase() === key.toLowerCase());
      if (existing >= 0) {
        const updated = [...prev];
        updated[existing] = { key, value };
        return updated;
      }
      return [...prev, { key, value }];
    });
    setDirty(true);
  }

  async function applyEdits() {
    if (!selectedPak || edits.length === 0) return;
    setApplying(true);
    try {
      const msg = await invoke<string>("apply_pak_tweak_edits", {
        pakPath: selectedPak.pak_path,
        edits,
      });
      flashBadge(msg);
      setDirty(false);
      // Reload to reflect changes
      await selectPak(selectedPak);
    } catch (e: any) {
      console.error("Apply failed:", e);
    } finally {
      setApplying(false);
    }
  }

  return (
    <div className="flex w-full max-w-4xl flex-col gap-5">
      {/* Pak list */}
      <Card className="flex flex-col gap-3 bg-card p-4">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2">
            <span className="text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">Config Mods</span>
            {showBadge && (
              <span className="flex items-center gap-1.5 text-[12px] font-medium text-[var(--color-ok)]">
                <CheckCircle2 size={14} strokeWidth={2.5} />
                {badgeMsg}
              </span>
            )}
          </div>
          <Button variant="ghost" size="sm" onClick={scan} disabled={scanning || !gamePath}>
            <RefreshCw size={14} className={cn(scanning && "animate-spin")} />
            Scan
          </Button>
        </div>

        {!gamePath && (
          <span className="flex items-center gap-1.5 text-[12px] text-[var(--color-warn)]">
            <XCircle size={14} strokeWidth={2.5} />
            Set game root on Home tab first
          </span>
        )}

        {gamePath && paks.length === 0 && !scanning && (
          <span className="text-[12px] text-muted-foreground">
            No mod paks with INI config files found. Mods that only contain assets won't appear
            here.
          </span>
        )}

        {/* Single pak — show inline, no list needed */}
        {paks.length === 1 && selectedPak && (
          <div className="flex items-center gap-2">
            <Package size={13} className="shrink-0 text-muted-foreground" />
            <span className="flex-1 truncate font-mono text-[12px]">{selectedPak.pak_name}</span>
            <div className="flex gap-1">
              {selectedPak.has_device_profiles && (
                <Badge variant="secondary" className="text-[9px] px-1.5 py-0">DeviceProfiles</Badge>
              )}
              {selectedPak.has_engine_ini && (
                <Badge variant="outline" className="text-[9px] px-1.5 py-0">Engine</Badge>
              )}
            </div>
          </div>
        )}

        {/* Multiple paks — show selectable list */}
        {paks.length > 1 && (
          <ul className="flex flex-col divide-y divide-border/50 rounded-md border border-border bg-background">
            {paks.map((pak) => (
              <li key={pak.pak_path}>
                <button
                  onClick={() => selectPak(pak)}
                  className={cn(
                    "flex w-full items-center gap-2 px-3 py-2 text-left transition-colors hover:bg-secondary",
                    selectedPak?.pak_path === pak.pak_path && "bg-secondary font-medium",
                  )}
                >
                  <Package size={13} className="shrink-0 text-muted-foreground" />
                  <span className="flex-1 truncate font-mono text-[12px]">{pak.pak_name}</span>
                  <div className="flex gap-1">
                    {pak.has_device_profiles && (
                      <Badge variant="secondary" className="text-[9px] px-1.5 py-0">DeviceProfiles</Badge>
                    )}
                    {pak.has_engine_ini && (
                      <Badge variant="outline" className="text-[9px] px-1.5 py-0">Engine</Badge>
                    )}
                  </div>
                </button>
              </li>
            ))}
          </ul>
        )}
      </Card>

      {/* Selected pak editor */}
      {selectedPak && (
        <>
        {/* Settings grouped by category */}
        {!loading && (() => {
          const categories = definitions.reduce<Record<string, TweakDefinition[]>>((acc, def) => {
            (acc[def.category] ??= []).push(def);
            return acc;
          }, {});

          return (
            <>
              {Object.entries(categories).map(([category, defs]) => (
                <Card key={category} className="flex flex-col gap-3 bg-card p-4">
                  <span className="text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
                    {category}
                  </span>
                  <div className="flex flex-col gap-2">
                    {defs.map((tweak) => (
                      <QuickTweakRow
                        key={tweak.id}
                        tweak={tweak}
                        isEnabled={tweakEnabled[tweak.id] ?? false}
                        currentValue={tweakValues[tweak.id]}
                        onToggle={() => toggleQuickTweak(tweak.id)}
                        onValueChange={(val) => setQuickTweakValue(tweak.id, val)}
                      />
                    ))}
                  </div>
                </Card>
              ))}
            </>
          );
        })()}

        {/* Pending Changes & Apply */}
        <div className="flex flex-col gap-4">
              {/* Pending edits summary */}
              {edits.length > 0 && (
                <div className="flex flex-col gap-1.5">
                  <span className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
                    Pending Changes ({edits.length})
                  </span>
                  <div className="flex flex-wrap gap-1">
                    {edits.map((e) => (
                      <Badge
                        key={e.key}
                        variant={e.value === null ? "destructive" : "secondary"}
                        className="text-[10px] font-mono px-1.5 py-0"
                      >
                        {e.value === null ? `- ${e.key}` : `${e.key}=${e.value}`}
                      </Badge>
                    ))}
                  </div>
                </div>
              )}

              {/* Apply */}
              <div className="flex items-center gap-3">
                <Button
                  variant="green"
                  size="sm"
                  onClick={applyEdits}
                  disabled={!dirty || applying || edits.length === 0}
                >
                  {applying ? (
                    <RefreshCw size={14} className="animate-spin" />
                  ) : (
                    <Save size={14} />
                  )}
                  {applying ? "Repacking…" : dirty ? "Apply & Repack" : "Up to Date"}
                </Button>
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={() => selectedPak && selectPak(selectedPak)}
                  disabled={loading}
                >
                  <RefreshCw size={14} />
                  Reload
                </Button>
              </div>
        </div>
        </>
      )}

    </div>
  );
}

// ── Quick Tweak Row ──────────────────────────────────────────────────

function QuickTweakRow({
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
    <div className="flex flex-col gap-2 rounded-md border border-border/50 bg-background px-4 py-3">
      <div className="flex items-start justify-between gap-4">
        <div className="flex flex-col gap-0.5">
          <Label htmlFor={`pak-${tweak.id}`} className="text-[13px] font-medium cursor-pointer">
            {tweak.label}
            {tweak.pak_only && (
              <Badge variant="outline" className="ml-2 text-[9px] px-1.5 py-0 align-middle">
                Pak only
              </Badge>
            )}
          </Label>
          <span className="text-[11px] leading-snug text-muted-foreground">
            {tweak.description}
          </span>
          <div className="mt-1 flex flex-wrap gap-1">
            <QuickTweakCodes tweak={tweak} />
          </div>
        </div>
        <Switch id={`pak-${tweak.id}`} checked={isEnabled} onCheckedChange={onToggle} />
      </div>

      {tweak.kind === "Slider" && (
        <QuickSliderControl
          tweak={tweak}
          isEnabled={isEnabled}
          currentValue={currentValue}
          onValueChange={onValueChange}
        />
      )}
    </div>
  );
}

function QuickTweakCodes({ tweak }: { tweak: TweakDefinition }) {
  const codeClass =
    "rounded bg-muted px-1 py-0.5 font-mono text-[10px] text-muted-foreground";

  switch (tweak.kind) {
    case "RemoveLines":
      return tweak.lines.map((line, i) => (
        <code key={i} className={codeClass}>
          {line}
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

function QuickSliderControl({
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

  const stepStr = String(tweak.step);
  const decimals = stepStr.includes(".") ? stepStr.split(".")[1].length : 0;

  return (
    <div
      className={cn(
        "flex items-center gap-3 pt-1",
        !isEnabled && "opacity-40 pointer-events-none",
      )}
    >
      <SliderUI
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
