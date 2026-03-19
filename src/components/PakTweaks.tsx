import { useState, useEffect, useRef } from "react";

import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import {
  Package,
  RefreshCw,
  Save,
  Search,
  FolderOpen,
  X,
  CheckCircle2,
  XCircle,
  Trash2,
  Info,
  TriangleAlert,
} from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Label } from "@/components/ui/label";
import { Slider as SliderUI } from "@/components/ui/slider";
import { Switch } from "@/components/ui/switch";
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

interface PakTweakEdit {
  key: string;
  value: string | null;
  engine_section?: string;
}

// Matches scalability::TweakState on the Rust side
interface TweakState {
  id: string;
  active: boolean;
  current_value: string | null;
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
  engine_section?: string;
}

interface SliderTweak extends TweakBase {
  kind: "Slider";
  key: string;
  min: number;
  max: number;
  step: number;
  default_value: number;
  write_default_on_disable?: boolean;
  engine_section?: string;
}

interface BatchToggleEntry {
  key: string;
  on_value: string;
  off_value?: string;
  scalability_section?: string;
  engine_section?: string;
}

interface BatchToggleTweak extends TweakBase {
  kind: "BatchToggle";
  entries: BatchToggleEntry[];
  default_enabled: boolean;
}

type TweakDefinition = RemoveLinesTweak | ToggleTweak | SliderTweak | BatchToggleTweak;

// Per-pak state cache that preserves tweak states and unsaved edits when switching between paks
interface PakCacheEntry {
  tweakStates: TweakState[];
  savedTweakStates: TweakState[];
  edits: PakTweakEdit[];
}

interface Props {
  gamePath: string;
}

export function PakTweaks({ gamePath }: Props) {
  const [paks, setPaks] = useState<PakIniInfo[]>([]);
  const [selectedPak, setSelectedPak] = useState<PakIniInfo | null>(null);
  const [tweakStates, setTweakStates] = useState<TweakState[]>([]);
  const [savedTweakStates, setSavedTweakStates] = useState<TweakState[]>([]);
  const [edits, setEdits] = useState<PakTweakEdit[]>([]);
  const [scanning, setScanning] = useState(false);
  const [loading, setLoading] = useState(false);
  const [applying, setApplying] = useState(false);
  const [notice, setNotice] = useState<{ msg: string; type: "ok" | "err" | "info" } | null>(null);
  const noticeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const pakCache = useRef<Map<string, PakCacheEntry>>(new Map());
  const scanRef = useRef(scan);
  // Tweak definitions (for rendering controls)
  const [definitions, setDefinitions] = useState<TweakDefinition[]>([]);

  const isPakMissingError = (err: unknown): boolean => {
    const text = String(err).toLowerCase();
    return (
      text.includes("pak file not found") ||
      text.includes("no such file") ||
      text.includes("cannot find the file")
    );
  };

  const formatModsFoundMessage = (count: number, removedMissing: number): string => {
    const modsPart = `Found ${count} mod${count !== 1 ? "s" : ""}`;
    if (removedMissing <= 0) return modsPart;
    const removedPart = `removed ${removedMissing} missing manual entr${removedMissing === 1 ? "y" : "ies"}`;
    return `${modsPart} (${removedPart})`;
  };

  const showNotice = (msg: string, type: "ok" | "err" | "info", duration = 4000) => {
    if (noticeTimer.current) clearTimeout(noticeTimer.current);
    setNotice({ msg, type });
    noticeTimer.current = setTimeout(() => setNotice(null), duration);
  };

  scanRef.current = scan;
  useEffect(() => {
    if (gamePath) scanRef.current(true);
  }, [gamePath]);

  // Load tweak definitions once
  useEffect(() => {
    invoke<TweakDefinition[]>("get_tweak_definitions").then(setDefinitions);
  }, []);

  function toggleQuickTweak(id: string) {
    const def = definitions.find((d) => d.id === id);
    if (!def) return;

    const currentState = tweakStates.find((s) => s.id === id);
    const savedState = savedTweakStates.find((s) => s.id === id);
    const newEnabled = !(currentState?.active ?? false);

    // Optimistically update local state so the UI responds immediately
    setTweakStates((prev) => prev.map((s) => (s.id === id ? { ...s, active: newEnabled } : s)));

    switch (def.kind) {
      case "RemoveLines":
        for (const line of def.lines) {
          const eqIdx = line.pattern.indexOf("=");
          const key = eqIdx >= 0 ? line.pattern.substring(0, eqIdx) : line.pattern;
          const patternVal = eqIdx >= 0 ? line.pattern.substring(eqIdx + 1) : "0";
          // When replace_with is set, enabled state writes that value instead of removing
          let replaceVal: string | null = null;
          if (line.replace_with != null) {
            const rwEqIdx = line.replace_with.indexOf("=");
            replaceVal =
              rwEqIdx >= 0 ? line.replace_with.substring(rwEqIdx + 1) : line.replace_with;
          }
          const isSavedActive = savedState?.active ?? false;
          const originalVal = isSavedActive ? replaceVal : patternVal;
          const newVal = newEnabled ? replaceVal : patternVal;
          queueEdit(key, newVal, originalVal, line.engine_section ?? undefined);
        }
        break;
      case "Toggle": {
        const originalVal = (savedState?.active ?? false) ? def.on_value : (def.off_value ?? null);
        queueEdit(
          def.key,
          newEnabled ? def.on_value : (def.off_value ?? null),
          originalVal,
          def.engine_section
        );
        break;
      }
      case "Slider": {
        const sliderDef = def as SliderTweak;
        const currentVal = currentState?.current_value ?? String(sliderDef.default_value);
        const offVal = sliderDef.write_default_on_disable ? String(sliderDef.default_value) : null;
        const originalVal =
          (savedState?.active ?? false)
            ? (savedState?.current_value ?? String(sliderDef.default_value))
            : offVal;
        queueEdit(def.key, newEnabled ? currentVal : offVal, originalVal, def.engine_section);
        break;
      }
      case "BatchToggle": {
        for (const entry of def.entries) {
          const originalVal =
            (savedState?.active ?? false) ? entry.on_value : (entry.off_value ?? null);
          queueEdit(
            entry.key,
            newEnabled ? entry.on_value : (entry.off_value ?? null),
            originalVal,
            entry.engine_section
          );
        }
        break;
      }
    }
  }

  function setQuickTweakValue(id: string, val: string) {
    const def = definitions.find((d) => d.id === id);
    if (!def || def.kind !== "Slider") return;
    const sliderDef = def as SliderTweak;
    const savedState = savedTweakStates.find((s) => s.id === id);
    const offVal = sliderDef.write_default_on_disable ? String(sliderDef.default_value) : null;
    const originalVal =
      (savedState?.active ?? false)
        ? (savedState?.current_value ?? String(sliderDef.default_value))
        : offVal;
    setTweakStates((prev) => prev.map((s) => (s.id === id ? { ...s, current_value: val } : s)));
    queueEdit(sliderDef.key, val, originalVal, sliderDef.engine_section);
  }

  async function browse() {
    const selected = await open({
      multiple: false,
      filters: [{ name: "Pak files", extensions: ["pak"] }],
    });
    if (typeof selected !== "string") return;
    try {
      const info = await invoke<PakIniInfo | null>("inspect_pak_path", { pakPath: selected });
      if (!info) {
        showNotice("No tweakable INI found in that pak", "err");
        return;
      }
      // Add to list if not already present, then select it
      setPaks((prev) => (prev.find((p) => p.pak_path === info.pak_path) ? prev : [...prev, info]));
      await selectPak(info);
    } catch (e: unknown) {
      showNotice("Failed to read pak", "err");
      console.error(e);
    }
  }

  /** Scan the mods folder for pak files — only updates the list, preserves current selection and edits */
  async function scan(silent = false) {
    if (!gamePath) return;
    setScanning(true);
    try {
      const results = await invoke<PakIniInfo[]>("scan_mod_paks_for_ini", {
        gameRoot: gamePath,
      });
      // Keep manually-browsed paks that still exist and still contain tweakable INI entries.
      const manualOnly = paks.filter((p) => !results.find((r) => r.pak_path === p.pak_path));
      const inspectedManual = await Promise.all(
        manualOnly.map(async (pak) => {
          try {
            return await invoke<PakIniInfo | null>("inspect_pak_path", { pakPath: pak.pak_path });
          } catch {
            return null;
          }
        })
      );
      const retainedManual = inspectedManual.filter((pak): pak is PakIniInfo => pak !== null);
      const removedMissing = manualOnly.length - retainedManual.length;

      // Merge: keep valid manually-browsed paks that aren't in the folder scan.
      const merged = [...results, ...retainedManual];
      setPaks(merged);
      if (merged.length === 0) {
        setSelectedPak(null);
        setTweakStates([]);
        setSavedTweakStates([]);
        setEdits([]);
        if (!silent) showNotice("No config mods found", "info");
      } else if (!selectedPak) {
        // Nothing selected yet — auto-select if only one
        if (merged.length === 1) {
          await selectPak(merged[0]);
        }
        if (!silent) showNotice(formatModsFoundMessage(merged.length, removedMissing), "ok");
      } else if (!merged.find((p) => p.pak_path === selectedPak.pak_path)) {
        // Previously selected pak is gone — auto-select if only one remains, otherwise deselect
        if (merged.length === 1) {
          await selectPak(merged[0]);
        } else {
          setSelectedPak(null);
          setTweakStates([]);
          setSavedTweakStates([]);
          setEdits([]);
        }
        if (!silent) showNotice(formatModsFoundMessage(merged.length, removedMissing), "ok");
      } else {
        if (!silent) showNotice(formatModsFoundMessage(merged.length, removedMissing), "ok");
      }
    } catch (e: unknown) {
      console.error("Scan failed:", e);
    } finally {
      setScanning(false);
    }
  }

  async function selectPak(pak: PakIniInfo) {
    // Save current state before switching so we can restore it if user comes back
    if (selectedPak && selectedPak.pak_path !== pak.pak_path) {
      pakCache.current.set(selectedPak.pak_path, { tweakStates, savedTweakStates, edits });
    }

    const cached = pakCache.current.get(pak.pak_path);
    if (cached) {
      setSelectedPak(pak);
      setTweakStates(cached.tweakStates);
      setSavedTweakStates(cached.savedTweakStates);
      setEdits(cached.edits);
      return;
    }

    // Cache miss — fetch from backend
    setSelectedPak(pak);
    setTweakStates([]);
    setSavedTweakStates([]);
    setEdits([]);
    setLoading(true);
    try {
      const states = await invoke<TweakState[]>("detect_pak_tweaks", { pakPath: pak.pak_path });
      setTweakStates(states);
      setSavedTweakStates(states);
      setEdits([]);
      pakCache.current.set(pak.pak_path, {
        tweakStates: states,
        savedTweakStates: states,
        edits: [],
      });
    } catch (e: unknown) {
      if (isPakMissingError(e)) {
        removePak(pak.pak_path);
        showNotice("That pak file is missing now. Removed it from the list.", "info");
      }
      console.error("Load failed:", e);
    } finally {
      setLoading(false);
    }
  }

  /** Force a fresh reload from disk, bypassing and updating the cache */
  async function forceReloadPak(pak: PakIniInfo) {
    pakCache.current.delete(pak.pak_path);
    try {
      const states = await invoke<TweakState[]>("detect_pak_tweaks", { pakPath: pak.pak_path });
      setTweakStates(states);
      setSavedTweakStates(states);
      setEdits([]);
      pakCache.current.set(pak.pak_path, {
        tweakStates: states,
        savedTweakStates: states,
        edits: [],
      });
    } catch (e: unknown) {
      if (isPakMissingError(e)) {
        removePak(pak.pak_path);
        showNotice("That pak file is missing now. Removed it from the list.", "info");
      }
      console.error("Reload failed:", e);
    }
  }

  function queueEdit(
    key: string,
    value: string | null,
    originalValue: string | null | undefined,
    engineSection?: string
  ) {
    setEdits((prev) => {
      const existing = prev.findIndex((e) => e.key.toLowerCase() === key.toLowerCase());
      // If the new value restores to original, cancel out this edit
      if (originalValue !== undefined && value === originalValue) {
        if (existing >= 0) {
          const updated = [...prev];
          updated.splice(existing, 1);
          return updated;
        }
        return prev;
      }
      if (existing >= 0) {
        const updated = [...prev];
        updated[existing] = { key, value, engine_section: engineSection };
        return updated;
      }
      return [...prev, { key, value, engine_section: engineSection }];
    });
  }

  async function applyEdits() {
    if (!selectedPak || edits.length === 0) return;
    setApplying(true);
    try {
      const msg = await invoke<string>("apply_pak_tweak_edits", {
        pakPath: selectedPak.pak_path,
        edits,
      });
      showNotice(msg, "ok");
      await forceReloadPak(selectedPak);
    } catch (e: unknown) {
      if (isPakMissingError(e)) {
        removePak(selectedPak.pak_path);
        showNotice("That pak file is missing now. Removed it from the list.", "info");
      } else {
        showNotice(String(e), "err");
      }
      console.error("Apply failed:", e);
    } finally {
      setApplying(false);
    }
  }

  async function clearShaderCache() {
    try {
      const msg = await invoke<string>("clear_shader_cache");
      showNotice(msg, "ok");
    } catch (e: unknown) {
      showNotice("Failed to clear shader cache", "err");
      console.error("Clear shader cache failed:", e);
    }
  }

  const dirty = edits.length > 0;

  function removePak(pakPath: string) {
    pakCache.current.delete(pakPath);
    const wasSelected = selectedPak?.pak_path === pakPath;
    const remaining = paks.filter((p) => p.pak_path !== pakPath);
    setPaks(remaining);
    if (wasSelected) {
      if (remaining.length === 1) {
        selectPak(remaining[0]);
      } else {
        setSelectedPak(null);
        setTweakStates([]);
        setSavedTweakStates([]);
        setEdits([]);
      }
    }
  }

  return (
    <div className="flex w-full flex-1 min-h-0 flex-col">
      {/* Scrollable content: pak list + tweak cards */}
      <div className="flex-1 overflow-y-auto pr-6">
        <div className="flex flex-col gap-5">
          {/* Pak list */}
          <Card className="flex flex-col gap-3 bg-card p-4">
            <div className="flex items-center justify-between gap-2">
              <div className="flex min-w-0 flex-1 items-center gap-2">
                <span className="shrink-0 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
                  Config Mods
                </span>
                {notice && (
                  <span
                    title={notice.msg}
                    className={cn(
                      "flex min-w-0 items-center gap-1 text-[12px] font-medium",
                      notice.type === "ok"
                        ? "text-[var(--color-ok)]"
                        : notice.type === "err"
                          ? "text-[var(--color-err)]"
                          : "text-[var(--color-warn)]"
                    )}
                  >
                    {notice.type === "ok" && (
                      <CheckCircle2 size={13} strokeWidth={2.5} className="shrink-0" />
                    )}
                    {notice.type === "err" && (
                      <XCircle size={13} strokeWidth={2.5} className="shrink-0" />
                    )}
                    {notice.type === "info" && (
                      <TriangleAlert size={13} strokeWidth={2.5} className="shrink-0" />
                    )}
                    <span className="truncate">{notice.msg}</span>
                  </span>
                )}
              </div>
              <div className="flex shrink-0 items-center gap-2">
                <Button variant="outline" size="sm" onClick={browse}>
                  <FolderOpen size={14} />
                  Browse
                </Button>
                <Button
                  variant="blue"
                  size="sm"
                  onClick={() => scan()}
                  disabled={scanning || !gamePath}
                >
                  <Search size={14} className={cn(scanning && "animate-pulse")} />
                  Scan
                </Button>
              </div>
            </div>

            {!gamePath && (
              <span className="flex items-center gap-1.5 text-[12px] text-[var(--color-warn)]">
                <XCircle size={14} strokeWidth={2.5} />
                Set game root on Home tab first
              </span>
            )}

            {gamePath && paks.length === 0 && (
              <span className="flex items-start gap-1.5 text-[12px] text-muted-foreground">
                <Info size={14} className="mt-0.5 shrink-0" />
                <span>
                  <strong className="font-semibold text-foreground">No config mods found.</strong>{" "}
                  Mods that only contain assets won't appear here.
                </span>
              </span>
            )}

            {/* Single pak — show inline, no list needed */}
            {paks.length === 1 && selectedPak && (
              <div className="flex min-w-0 items-center gap-2">
                <Package size={13} className="shrink-0 text-muted-foreground" />
                <span className="min-w-0 flex-1 truncate font-mono text-[12px]">
                  {selectedPak.pak_name}
                </span>
                <div className="flex shrink-0 items-center gap-1">
                  {selectedPak.has_device_profiles && (
                    <Badge variant="outline" className="text-[10px] px-2 py-0.5">
                      DeviceProfiles
                    </Badge>
                  )}
                  {selectedPak.has_engine_ini && (
                    <Badge variant="outline" className="text-[10px] px-2 py-0.5">
                      Engine
                    </Badge>
                  )}
                  <button
                    onClick={() => removePak(selectedPak.pak_path)}
                    className="ml-1 rounded p-1.5 text-muted-foreground/60 transition-colors hover:bg-destructive/15 hover:text-destructive"
                    title="Remove from list"
                  >
                    <X size={14} />
                  </button>
                </div>
              </div>
            )}

            {/* Multiple paks — show selectable list */}
            {paks.length > 1 && (
              <ul className="flex flex-col divide-y divide-border/50 rounded-md border border-border bg-background">
                {paks.map((pak) => (
                  <li
                    key={pak.pak_path}
                    className={cn(
                      "flex min-w-0 items-center transition-colors hover:bg-secondary",
                      selectedPak?.pak_path === pak.pak_path && "bg-secondary"
                    )}
                  >
                    <button
                      onClick={() => selectPak(pak)}
                      className={cn(
                        "flex min-w-0 flex-1 items-center gap-2 px-3 py-2 text-left",
                        selectedPak?.pak_path === pak.pak_path && "font-medium"
                      )}
                    >
                      <Package size={13} className="shrink-0 text-muted-foreground" />
                      <span className="min-w-0 flex-1 truncate font-mono text-[12px]">
                        {pak.pak_name}
                      </span>
                      <div className="flex shrink-0 gap-1">
                        {pak.has_device_profiles && (
                          <Badge variant="outline" className="text-[10px] px-2 py-0.5">
                            DeviceProfiles
                          </Badge>
                        )}
                        {pak.has_engine_ini && (
                          <Badge variant="outline" className="text-[10px] px-2 py-0.5">
                            Engine
                          </Badge>
                        )}
                      </div>
                    </button>
                    <button
                      onClick={() => removePak(pak.pak_path)}
                      className="mr-2 shrink-0 rounded p-1.5 text-muted-foreground/60 transition-colors hover:bg-destructive/15 hover:text-destructive"
                      title="Remove from list"
                    >
                      <X size={14} />
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </Card>

          {/* Selected pak editor — tweak cards */}
          {selectedPak &&
            !loading &&
            (() => {
              const categories = definitions.reduce<Record<string, TweakDefinition[]>>(
                (acc, def) => {
                  (acc[def.category] ??= []).push(def);
                  return acc;
                },
                {}
              );

              return (
                <div className="grid gap-5 xl:grid-cols-2 2xl:grid-cols-3">
                  {Object.entries(categories).map(([category, defs]) => (
                    <Card key={category} className="flex flex-col gap-3 bg-card p-4">
                      <span className="text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
                        {category}
                      </span>
                      <div className="flex flex-col gap-2">
                        {defs.map((tweak) => {
                          const engineOnly =
                            (tweak.kind === "Toggle" && !!tweak.engine_section) ||
                            (tweak.kind === "Slider" && !!tweak.engine_section) ||
                            (tweak.kind === "BatchToggle" &&
                              tweak.entries.some((entry) => !!entry.engine_section));
                          const isEnabled =
                            tweakStates.find((s) => s.id === tweak.id)?.active ?? false;
                          const removeOnly = tweak.kind === "RemoveLines" && tweak.remove_only;
                          const isSavedEnabled =
                            savedTweakStates.find((s) => s.id === tweak.id)?.active ?? false;
                          if (removeOnly && isSavedEnabled) return null;
                          const needsEngine = engineOnly && !selectedPak.has_engine_ini;
                          const disabled = needsEngine;
                          return (
                            <QuickTweakRow
                              key={tweak.id}
                              tweak={tweak}
                              isEnabled={isEnabled}
                              disabledReason={
                                needsEngine
                                  ? "Requires DefaultEngine.ini in this pak mod"
                                  : undefined
                              }
                              currentValue={
                                tweakStates.find((s) => s.id === tweak.id)?.current_value ??
                                undefined
                              }
                              disabled={disabled}
                              onToggle={() => toggleQuickTweak(tweak.id)}
                              onValueChange={(val) => setQuickTweakValue(tweak.id, val)}
                            />
                          );
                        })}
                      </div>
                    </Card>
                  ))}
                </div>
              );
            })()}
        </div>
      </div>

      {/* Apply bar — fixed footer, outside the scroll area */}
      {selectedPak && !loading && (
        <div className="flex flex-col gap-3 border-t border-border pt-3 pb-1 mt-5">
          {/* Pending edits summary */}
          {selectedPak && !loading && edits.length > 0 && (
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
          <div className="flex min-w-0 items-center gap-3">
            {selectedPak && !loading && (
              <>
                <Button
                  variant="green"
                  size="sm"
                  onClick={applyEdits}
                  disabled={!dirty || applying || edits.length === 0}
                >
                  {applying ? <RefreshCw size={14} className="animate-spin" /> : <Save size={14} />}
                  {applying ? "Repacking…" : dirty ? "Apply & Repack" : "Up to Date"}
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={clearShaderCache}
                  title="Delete pipeline cache files from %LOCALAPPDATA%\Marvel\Saved"
                >
                  <Trash2 size={14} />
                  Clear Shader Cache
                </Button>
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={() => selectedPak && forceReloadPak(selectedPak)}
                  disabled={loading}
                >
                  <RefreshCw size={14} />
                  Reload
                </Button>
              </>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

// ── Quick Tweak Row ──────────────────────────────────────────────────

function QuickTweakRow({
  tweak,
  isEnabled,
  currentValue,
  disabled,
  disabledReason,
  onToggle,
  onValueChange,
}: {
  tweak: TweakDefinition;
  isEnabled: boolean;
  currentValue: string | undefined;
  disabledReason?: string;
  disabled?: boolean;
  onToggle: () => void;
  onValueChange: (val: string) => void;
}) {
  return (
    <div
      className={cn(
        "flex flex-col gap-2 rounded-md border border-border/50 bg-background px-4 py-3",
        disabled && "opacity-50"
      )}
    >
      <div className="flex items-start justify-between gap-4">
        <div className="flex flex-col gap-0.5">
          <Label
            htmlFor={`pak-${tweak.id}`}
            className={cn("text-[13px] font-medium", !disabled && "cursor-pointer")}
          >
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
          {disabledReason && (
            <span className="text-[11px] leading-snug text-[var(--color-warn)] mt-0.5">
              {disabledReason}
            </span>
          )}
          <div className="mt-1 flex flex-wrap gap-1">
            <QuickTweakCodes tweak={tweak} />
          </div>
        </div>
        <Switch
          id={`pak-${tweak.id}`}
          checked={isEnabled}
          onCheckedChange={onToggle}
          disabled={disabled}
        />
      </div>

      {tweak.kind === "Slider" && (
        <QuickSliderControl
          tweak={tweak}
          isEnabled={isEnabled && !disabled}
          currentValue={currentValue}
          onValueChange={onValueChange}
        />
      )}
    </div>
  );
}

function QuickTweakCodes({ tweak }: { tweak: TweakDefinition }) {
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
    case "BatchToggle":
      return (
        <>
          {tweak.entries.map((entry) => (
            <code key={entry.key} className={codeClass}>
              {entry.key}={entry.on_value}
              {entry.off_value !== undefined ? `/${entry.off_value}` : ""}
            </code>
          ))}
        </>
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
      className={cn("flex items-center gap-3 pt-1", !isEnabled && "opacity-40 pointer-events-none")}
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
