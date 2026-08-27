import { useState, useEffect, useRef } from "react";

import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open } from "@tauri-apps/plugin-dialog";
import {
  Package,
  Pencil,
  RefreshCw,
  Save,
  Search,
  FolderOpen,
  UploadCloud,
  X,
  CheckCircle2,
  XCircle,
  Info,
  Plus,
  Trash2,
  TriangleAlert,
  Undo2,
} from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Slider as SliderUI } from "@/components/ui/slider";
import { Switch } from "@/components/ui/switch";
import { Tip } from "@/components/ui/tooltip";
import { useSaveHotkeys } from "@/hooks/useSaveHotkeys";
import { useScrollAtBottom } from "@/hooks/useScrollAtBottom";
import { normalizeFolderPath, onModsChanged } from "@/lib/modsEvents";
import { emitPakChanged, onPakChanged } from "@/lib/pakEvents";
import { unreadableScanMessage, type PakScanError } from "@/lib/pakScan";
import { emitTweakProfilesChanged, onTweakProfilesChanged } from "@/lib/tweakProfileEvents";
import { cn } from "@/lib/utils";

// ── Types matching Rust backend ──────────────────────────────────────

interface PakIniInfo {
  pak_name: string;
  pak_path: string;
  has_device_profiles: boolean;
  has_engine_ini: boolean;
  has_base_engine: boolean;
  has_windows_engine: boolean;
  device_profiles_entry: string | null;
  engine_ini_entry: string | null;
  base_engine_entry: string | null;
  windows_engine_entry: string | null;
}

// One of the four tweakable INI files, used to render presence badges on each pak.
// Runtime priority for shared keys: device_profiles > windows_engine > engine > base_engine.
type PakIniTarget = "base_engine" | "engine" | "windows_engine" | "device_profiles";

interface TweakSetting {
  id: string;
  enabled: boolean;
  value: string | null;
}

interface TweakPreset {
  name: string;
  settings: TweakSetting[];
  created_at: number;
  modified_at: number;
}

// Matches tweaks::TweakState on the Rust side
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
  engine_section?: string;
}

interface BatchToggleTweak extends TweakBase {
  kind: "BatchToggle";
  entries: BatchToggleEntry[];
  default_enabled: boolean;
}

type TweakDefinition = RemoveLinesTweak | ToggleTweak | SliderTweak | BatchToggleTweak;

// Per-pak state cache that preserves tweak states and unsaved changes when switching between paks
interface PakCacheEntry {
  tweakStates: TweakState[];
  savedTweakStates: TweakState[];
  pending: TweakSetting[];
}

interface Props {
  gamePath: string;
  isActive?: boolean;
}

const ADVANCED_CATEGORIES = new Set(["Latency"]);

function hasAnyEngine(pak: PakIniInfo): boolean {
  return pak.has_engine_ini || pak.has_base_engine || pak.has_windows_engine;
}

const TARGET_BADGE: Record<PakIniTarget, string> = {
  base_engine: "BaseEngine",
  engine: "Engine",
  windows_engine: "WindowsEngine",
  device_profiles: "DeviceProfiles",
};

export function PakTweaks({ gamePath, isActive }: Props) {
  const [paks, setPaks] = useState<PakIniInfo[]>([]);
  const [selectedPak, setSelectedPak] = useState<PakIniInfo | null>(null);
  const [tweakStates, setTweakStates] = useState<TweakState[]>([]);
  const [savedTweakStates, setSavedTweakStates] = useState<TweakState[]>([]);
  const [pending, setPending] = useState<TweakSetting[]>([]);
  const [scanning, setScanning] = useState(false);
  const [loading, setLoading] = useState(false);
  const [applying, setApplying] = useState(false);
  const [notice, setNotice] = useState<{ msg: string; type: "ok" | "err" | "info" } | null>(null);
  const noticeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const pakCache = useRef<Map<string, PakCacheEntry>>(new Map());
  const scanRef = useRef(scan);
  // Tweak definitions (for rendering controls)
  const [definitions, setDefinitions] = useState<TweakDefinition[]>([]);
  const [presets, setPresets] = useState<TweakPreset[]>([]);
  const [selectedPreset, setSelectedPreset] = useState<string>("");
  const [appliedPresetAt, setAppliedPresetAt] = useState<number | null>(null);
  const [savingAs, setSavingAs] = useState(false);
  const [renamingAs, setRenamingAs] = useState(false);
  const [newPresetName, setNewPresetName] = useState("");

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

  useEffect(() => {
    scanRef.current = scan;
  });
  useEffect(() => {
    if (gamePath) scanRef.current(true);
  }, [gamePath]);

  // Re-scan when ~mods composition changes elsewhere (mod install/delete, repack, recursive toggle).
  useEffect(() => {
    return onModsChanged((event) => {
      if (!gamePath) return;
      const modsFolder = `${gamePath}\\MarvelGame\\Marvel\\Content\\Paks\\~mods`;
      if (normalizeFolderPath(event.modsFolder) !== normalizeFolderPath(modsFolder)) return;
      scanRef.current(true);
    });
  }, [gamePath]);

  // Load tweak definitions once
  useEffect(() => {
    invoke<TweakDefinition[]>("get_tweak_definitions").then(setDefinitions);
  }, []);

  // Drag-and-drop: accept .pak files (same as browse).
  const [isDragging, setIsDragging] = useState(false);
  const isActiveRef = useRef(isActive);
  useEffect(() => {
    isActiveRef.current = isActive;
  }, [isActive]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    getCurrentWindow()
      .onDragDropEvent(async (event) => {
        if (event.payload.type === "enter") {
          if (isActiveRef.current) setIsDragging(true);
        } else if (event.payload.type === "drop") {
          setIsDragging(false);
          if (!isActiveRef.current) return;
          const pakPaths = event.payload.paths.filter((p) => p.toLowerCase().endsWith(".pak"));
          if (pakPaths.length === 0) return;
          try {
            const info = await invoke<PakIniInfo | null>("inspect_pak_path", {
              pakPath: pakPaths[0],
            });
            if (!info) {
              showNotice("No tweakable INI found in that pak", "err");
              return;
            }
            setPaks((prev) =>
              prev.find((p) => p.pak_path === info.pak_path) ? prev : [...prev, info]
            );
            await selectPak(info);
          } catch (e: unknown) {
            showNotice("Failed to read pak", "err");
            console.error(e);
          }
        } else if (event.payload.type === "leave") {
          setIsDragging(false);
        }
      })
      .then((fn) => {
        unlisten = fn;
      });
    return () => unlisten?.();
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // External pak mutation: invalidate cache and reload if current pak is affected.
  useEffect(() => {
    return onPakChanged((e) => {
      if (e.source === "PakTweaks") return;
      pakCache.current.delete(e.pakPath);
      // An added/removed INI can change which paks qualify, so refresh the list.
      scanRef.current(true);
      if (selectedPak?.pak_path !== e.pakPath) return;
      if (pending.length > 0) {
        showNotice("Pak changed elsewhere; reload manually to discard changes", "info", 6000);
        return;
      }
      forceReloadPak(selectedPak);
    });
  }, [selectedPak, pending.length]); // eslint-disable-line react-hooks/exhaustive-deps

  function toggleQuickTweak(id: string) {
    const def = definitions.find((d) => d.id === id);
    if (!def) return;

    const currentState = tweakStates.find((s) => s.id === id);
    const newEnabled = !(currentState?.active ?? false);

    // Optimistically update local state so the UI responds immediately
    setTweakStates((prev) => prev.map((s) => (s.id === id ? { ...s, active: newEnabled } : s)));
    queueSetting(id, newEnabled, currentState?.current_value ?? null);
  }

  const refreshPresets = async () => {
    try {
      const list = await invoke<TweakPreset[]>("list_tweak_profiles");
      setPresets(list);
      setSelectedPreset((prev) => (list.some((p) => p.name === prev) ? prev : ""));
    } catch {
      setPresets([]);
      setSelectedPreset("");
      setAppliedPresetAt(null);
    }
  };

  useEffect(() => {
    refreshPresets();
    return onTweakProfilesChanged(refreshPresets);
  }, []);

  // Clear stale selection when current preset disappears from list
  useEffect(() => {
    if (selectedPreset && !presets.some((p) => p.name === selectedPreset)) {
      setSelectedPreset("");
      setAppliedPresetAt(null);
    }
  }, [presets, selectedPreset]);

  function buildCurrentSettings(): TweakSetting[] {
    return definitions.map((def) => {
      const state = tweakStates.find((s) => s.id === def.id);
      return {
        id: def.id,
        enabled: state?.active ?? false,
        value: state?.current_value ?? null,
      };
    });
  }

  function applyPresetToCurrentPak(preset: TweakPreset) {
    if (!selectedPak) {
      setAppliedPresetAt(null);
      return;
    }

    // Only tweaks the preset names are touched; everything else keeps its current state.
    const presetMap = new Map(preset.settings.map((s) => [s.id, s]));

    setTweakStates((prev) =>
      prev.map((s) => {
        const target = presetMap.get(s.id);
        if (!target) return s;
        return { id: s.id, active: target.enabled, current_value: target.value };
      })
    );

    const next: TweakSetting[] = [...pending];
    for (const def of definitions) {
      const target = presetMap.get(def.id);
      if (!target) continue;
      // A remove-only tweak cannot be restored, and its row is hidden once saved active.
      if (!target.enabled && def.kind === "RemoveLines" && def.remove_only) continue;

      const saved = savedTweakStates.find((s) => s.id === def.id);
      const idx = next.findIndex((e) => e.id === def.id);
      const unchanged =
        target.enabled === (saved?.active ?? false) &&
        target.value === (saved?.current_value ?? null);

      if (unchanged) {
        if (idx >= 0) next.splice(idx, 1);
        continue;
      }
      const entry = { id: def.id, enabled: target.enabled, value: target.value };
      if (idx >= 0) next[idx] = entry;
      else next.push(entry);
    }

    setPending(next);
    setAppliedPresetAt(preset.modified_at);
  }

  async function saveCurrentAsPreset() {
    const trimmed = newPresetName.trim();
    if (!trimmed) return;
    try {
      const profile = await invoke<TweakPreset>("save_tweak_profile", {
        name: trimmed,
        settings: buildCurrentSettings(),
      });
      setNewPresetName("");
      setSavingAs(false);
      setPresets((prev) => [...prev.filter((p) => p.name !== profile.name), profile]);
      setSelectedPreset(profile.name);
      setAppliedPresetAt(profile.modified_at);
      emitTweakProfilesChanged();
      showNotice(`Saved preset "${profile.name}"`, "ok");
    } catch (e) {
      showNotice(String(e), "err");
    }
  }

  async function overwriteSelectedPreset() {
    if (!selectedPreset) return;
    try {
      const profile = await invoke<TweakPreset>("overwrite_tweak_profile", {
        name: selectedPreset,
        settings: buildCurrentSettings(),
      });
      setPresets((prev) => prev.map((p) => (p.name === profile.name ? profile : p)));
      setAppliedPresetAt(profile.modified_at);
      emitTweakProfilesChanged();
      showNotice(`Updated preset "${profile.name}"`, "ok");
    } catch (e) {
      showNotice(String(e), "err");
    }
  }

  // Auto-reapply when the selected preset is modified on another tab.
  useEffect(() => {
    if (!selectedPreset || appliedPresetAt == null || !selectedPak) return;
    const preset = presets.find((p) => p.name === selectedPreset);
    if (!preset || preset.modified_at <= appliedPresetAt) return;
    applyPresetToCurrentPak(preset);
    showNotice(`Preset "${preset.name}" was updated, reapplied`, "info", 5000);
  }, [presets, selectedPreset, appliedPresetAt, selectedPak]); // eslint-disable-line react-hooks/exhaustive-deps

  async function deleteSelectedPreset() {
    if (!selectedPreset) return;
    const name = selectedPreset;
    try {
      await invoke("delete_tweak_profile", { name });
      setSelectedPreset("");
      setAppliedPresetAt(null);
      setPresets((prev) => prev.filter((p) => p.name !== name));
      emitTweakProfilesChanged();
      showNotice(`Deleted preset "${name}"`, "ok");
    } catch (e) {
      showNotice(String(e), "err");
    }
  }

  async function renameSelectedPreset() {
    if (!selectedPreset) return;
    const oldName = selectedPreset;
    const trimmed = newPresetName.trim();
    if (!trimmed || trimmed === oldName) {
      setRenamingAs(false);
      setNewPresetName("");
      return;
    }
    try {
      const profile = await invoke<TweakPreset>("rename_tweak_profile", {
        oldName,
        newName: trimmed,
      });
      setRenamingAs(false);
      setNewPresetName("");
      setPresets((prev) => prev.map((p) => (p.name === oldName ? profile : p)));
      setSelectedPreset(profile.name);
      emitTweakProfilesChanged();
      showNotice(`Renamed preset to "${profile.name}"`, "ok");
    } catch (e) {
      showNotice(String(e), "err");
    }
  }

  function setQuickTweakValue(id: string, val: string) {
    const def = definitions.find((d) => d.id === id);
    if (!def || def.kind !== "Slider") return;
    const active = tweakStates.find((s) => s.id === id)?.active ?? false;
    setTweakStates((prev) => prev.map((s) => (s.id === id ? { ...s, current_value: val } : s)));
    queueSetting(id, active, val);
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
      const scanned = await invoke<{ paks: PakIniInfo[]; unreadable: PakScanError[] }>(
        "scan_mod_paks_for_ini",
        { gameRoot: gamePath }
      );
      const results = scanned.paks;
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
        setPending([]);
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
          setPending([]);
        }
        if (!silent) showNotice(formatModsFoundMessage(merged.length, removedMissing), "ok");
      } else {
        if (!silent) showNotice(formatModsFoundMessage(merged.length, removedMissing), "ok");
      }
      if (scanned.unreadable.length > 0) {
        console.error("Paks that could not be read:", scanned.unreadable);
        if (!silent) showNotice(unreadableScanMessage(scanned.unreadable), "err", 8000);
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
      pakCache.current.set(selectedPak.pak_path, {
        tweakStates,
        savedTweakStates,
        pending,
      });
    }

    const cached = pakCache.current.get(pak.pak_path);
    if (cached) {
      setSelectedPak(pak);
      setTweakStates(cached.tweakStates);
      setSavedTweakStates(cached.savedTweakStates);
      setPending(cached.pending);
      return;
    }

    // Cache miss — fetch from backend, keep showing previous state during load
    setSelectedPak(pak);
    setLoading(true);
    try {
      const states = await invoke<TweakState[]>("detect_pak_tweaks", { pakPath: pak.pak_path });
      setTweakStates(states);
      setSavedTweakStates(states);
      setPending([]);
      pakCache.current.set(pak.pak_path, {
        tweakStates: states,
        savedTweakStates: states,
        pending: [],
      });
    } catch (e: unknown) {
      // Clear on failure so stale state doesn't linger
      setTweakStates([]);
      setSavedTweakStates([]);
      setPending([]);
      if (isPakMissingError(e)) {
        removePak(pak.pak_path);
        showNotice("That pak file is missing now. Removed it from the list.", "info");
      } else {
        showNotice(String(e), "err");
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
      setPending([]);
      pakCache.current.set(pak.pak_path, {
        tweakStates: states,
        savedTweakStates: states,
        pending: [],
      });
    } catch (e: unknown) {
      if (isPakMissingError(e)) {
        removePak(pak.pak_path);
        showNotice("That pak file is missing now. Removed it from the list.", "info");
      } else {
        showNotice(String(e), "err");
      }
      console.error("Reload failed:", e);
    }
  }

  /** Record a tweak's desired state, or drop it once it matches disk. Comparing against
   * savedTweakStates rather than the previous click is what makes toggling back and forth end clean. */
  function queueSetting(id: string, enabled: boolean, value: string | null) {
    const saved = savedTweakStates.find((s) => s.id === id);
    const unchanged =
      enabled === (saved?.active ?? false) && value === (saved?.current_value ?? null);
    setPending((prev) => {
      const existing = prev.findIndex((e) => e.id === id);
      if (unchanged) {
        if (existing < 0) return prev;
        const updated = [...prev];
        updated.splice(existing, 1);
        return updated;
      }
      if (existing >= 0) {
        const updated = [...prev];
        updated[existing] = { id, enabled, value };
        return updated;
      }
      return [...prev, { id, enabled, value }];
    });
  }

  async function applyEdits() {
    if (!selectedPak || pending.length === 0) return;
    setApplying(true);
    try {
      const msg = await invoke<string>("apply_pak_tweak_settings", {
        pakPath: selectedPak.pak_path,
        settings: pending,
      });
      showNotice(msg, "ok");
      emitPakChanged({ pakPath: selectedPak.pak_path, source: "PakTweaks" });
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

  const dirty = pending.length > 0;

  const { atBottom, scrollRef, sentinelRef } = useScrollAtBottom();
  const discardEdits = () => {
    if (!selectedPak) return;
    setSelectedPreset("");
    setAppliedPresetAt(null);
    forceReloadPak(selectedPak);
  };
  useSaveHotkeys({
    dirty,
    saving: applying,
    onSave: applyEdits,
    onDiscard: discardEdits,
  });

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
        setPending([]);
      }
    }
  }

  return (
    <div className="relative flex w-full flex-1 min-h-0 flex-col">
      {isDragging && (
        <div className="pointer-events-none absolute inset-0 z-50 flex flex-col items-center justify-center gap-3 rounded-lg border-2 border-dashed border-ok bg-background/80 backdrop-blur-sm">
          <UploadCloud size={36} className="text-ok" />
          <span className="text-sm font-semibold text-ok">Drop .pak to inspect</span>
        </div>
      )}
      {/* Scrollable content: pak list + tweak cards */}
      <div ref={scrollRef} className="flex-1 min-h-0 overflow-y-auto scrollbar-gutter-stable">
        <div className="flex flex-col gap-5">
          {/* Pak list */}
          <div className="flex flex-col overflow-hidden rounded-md border border-border">
            <div className="flex items-center justify-between gap-2 border-b border-border bg-card px-3 py-2">
              <div className="flex min-w-0 flex-1 items-center gap-2">
                <span className="shrink-0 text-sm font-semibold">Config Mods</span>
                {notice && (
                  <Tip content={notice.msg}>
                    <span
                      className={cn(
                        "flex min-w-0 items-center gap-1 text-[12px] font-medium",
                        notice.type === "ok"
                          ? "text-ok"
                          : notice.type === "err"
                            ? "text-err"
                            : "text-warn"
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
                  </Tip>
                )}
              </div>
              <div className="flex shrink-0 items-center gap-0.5">
                <Tip content="Browse for config mods">
                  <Button variant="ghost" size="icon-sm" onClick={browse}>
                    <FolderOpen size={14} />
                  </Button>
                </Tip>
                <Tip content="Scan for config mods">
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    onClick={() => scan()}
                    disabled={scanning || !gamePath}
                  >
                    <Search size={14} className={cn(scanning && "animate-pulse")} />
                  </Button>
                </Tip>
              </div>
            </div>

            {!gamePath && (
              <div className="px-3 py-2">
                <span className="flex items-center gap-1.5 text-[12px] text-warn">
                  <XCircle size={14} strokeWidth={2.5} />
                  Set game root in Settings first
                </span>
              </div>
            )}

            {gamePath && paks.length === 0 && (
              <div className="px-3 py-2">
                <span className="flex items-start gap-1.5 text-[12px] text-muted-foreground">
                  <Info size={14} className="mt-0.5 shrink-0" />
                  <span>
                    <strong className="font-semibold text-foreground">No config mods found.</strong>{" "}
                    Mods that only contain assets won't appear here.
                  </span>
                </span>
              </div>
            )}

            {/* Pak list — unified for any count */}
            {paks.length > 0 && (
              <ul className="flex flex-col">
                {paks.map((pak) => (
                  <li
                    key={pak.pak_path}
                    className={cn(
                      "flex h-9 min-w-0 items-center transition-colors hover:bg-secondary/50",
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
                        {(
                          [
                            "device_profiles",
                            "windows_engine",
                            "engine",
                            "base_engine",
                          ] as PakIniTarget[]
                        )
                          .filter((t) => {
                            switch (t) {
                              case "device_profiles":
                                return pak.has_device_profiles;
                              case "windows_engine":
                                return pak.has_windows_engine;
                              case "engine":
                                return pak.has_engine_ini;
                              case "base_engine":
                                return pak.has_base_engine;
                            }
                          })
                          .map((t) => (
                            <Badge key={t} variant="outline" className="text-[10px] px-2 py-0.5">
                              {TARGET_BADGE[t]}
                            </Badge>
                          ))}
                      </div>
                    </button>
                    <Tip content="Remove from list">
                      <button
                        onClick={() => removePak(pak.pak_path)}
                        className="mr-2 shrink-0 rounded p-1.5 text-muted-foreground/60 transition-colors hover:bg-destructive/15 hover:text-destructive"
                      >
                        <X size={14} />
                      </button>
                    </Tip>
                  </li>
                ))}
              </ul>
            )}
          </div>

          {/* Toolbar: presets */}
          {selectedPak && tweakStates.length > 0 && (
            <div className="flex flex-wrap items-center gap-2 rounded-md border border-border bg-card/40 px-3 py-2">
              <div className="flex flex-wrap items-center gap-2">
                <Label className="shrink-0 text-[12px] font-medium text-muted-foreground">
                  Preset
                </Label>
                <div className="w-56 shrink-0">
                  {savingAs || renamingAs ? (
                    <input
                      autoFocus
                      value={newPresetName}
                      onChange={(e) => setNewPresetName(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") {
                          if (renamingAs) renameSelectedPreset();
                          else saveCurrentAsPreset();
                        }
                        if (e.key === "Escape") {
                          setSavingAs(false);
                          setRenamingAs(false);
                          setNewPresetName("");
                        }
                      }}
                      placeholder={renamingAs ? "New preset name…" : "Preset name…"}
                      className="h-7 w-full rounded-md border border-border bg-background px-3 text-[12px] outline-none placeholder:text-muted-foreground/50 focus:border-primary"
                    />
                  ) : (
                    <Select
                      value={selectedPreset}
                      onValueChange={(name) => {
                        setSelectedPreset(name);
                        const p = presets.find((x) => x.name === name);
                        if (p) applyPresetToCurrentPak(p);
                      }}
                      disabled={presets.length === 0}
                    >
                      <SelectTrigger
                        size="sm"
                        className="w-full text-left text-[12px] [&>span]:text-left"
                      >
                        <SelectValue
                          placeholder={presets.length === 0 ? "No saved presets" : "Choose preset…"}
                        />
                      </SelectTrigger>
                      <SelectContent>
                        {presets.map((p) => (
                          <SelectItem key={p.name} value={p.name}>
                            {p.name}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  )}
                </div>
                {savingAs || renamingAs ? (
                  <>
                    <Tip content="Save (Enter)">
                      <Button
                        variant="blue"
                        size="icon-sm"
                        onClick={renamingAs ? renameSelectedPreset : saveCurrentAsPreset}
                        disabled={
                          !newPresetName.trim() ||
                          (renamingAs && newPresetName.trim() === selectedPreset)
                        }
                      >
                        <Save size={13} />
                      </Button>
                    </Tip>
                    <Tip content="Cancel (Esc)">
                      <Button
                        variant="ghost"
                        size="icon-sm"
                        onClick={() => {
                          setSavingAs(false);
                          setRenamingAs(false);
                          setNewPresetName("");
                        }}
                      >
                        <X size={13} />
                      </Button>
                    </Tip>
                  </>
                ) : (
                  <>
                    {selectedPreset && (
                      <>
                        <Tip content="Save current tweaks into this preset">
                          <Button variant="ghost" size="icon-sm" onClick={overwriteSelectedPreset}>
                            <Save size={13} />
                          </Button>
                        </Tip>
                        <Tip content="Rename this preset">
                          <Button
                            variant="ghost"
                            size="icon-sm"
                            onClick={() => {
                              setNewPresetName(selectedPreset);
                              setRenamingAs(true);
                            }}
                          >
                            <Pencil size={13} />
                          </Button>
                        </Tip>
                        <Tip content="Delete this preset">
                          <Button
                            variant="ghost"
                            size="icon-sm"
                            className="text-destructive hover:bg-destructive/15 hover:text-destructive"
                            onClick={deleteSelectedPreset}
                          >
                            <Trash2 size={13} />
                          </Button>
                        </Tip>
                        <span className="mx-1 h-4 w-px bg-border/60" />
                      </>
                    )}
                    <Tip content="Save current tweaks as new preset">
                      <Button variant="ghost" size="icon-sm" onClick={() => setSavingAs(true)}>
                        <Plus size={13} />
                      </Button>
                    </Tip>
                  </>
                )}
              </div>
            </div>
          )}

          {/* Selected pak editor — tweak cards */}
          {selectedPak &&
            tweakStates.length > 0 &&
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
                    <div key={category} className="overflow-hidden rounded-md border border-border">
                      <div className="flex items-center justify-between gap-2 border-b border-border bg-card px-3 py-2">
                        <span className="text-sm font-semibold">{category}</span>
                        {ADVANCED_CATEGORIES.has(category) && (
                          <Tip
                            content={
                              <span className="block break-normal">
                                Advanced tweaks. Defaults are tuned for most setups; only change
                                these if you understand what they do.
                              </span>
                            }
                          >
                            <span className="flex shrink-0 items-center gap-1 rounded-full bg-warn/10 px-2 py-0.5 text-[10px] font-medium text-warn">
                              <TriangleAlert size={11} strokeWidth={2.5} />
                              Advanced
                            </span>
                          </Tip>
                        )}
                      </div>
                      <div className="flex flex-col divide-y divide-border/50">
                        {defs.map((tweak) => {
                          const engineOnly =
                            (tweak.kind === "Toggle" && !!tweak.engine_section) ||
                            (tweak.kind === "Slider" && !!tweak.engine_section) ||
                            (tweak.kind === "BatchToggle" &&
                              tweak.entries.some((entry) => !!entry.engine_section)) ||
                            (tweak.kind === "RemoveLines" &&
                              tweak.lines.some((line) => !!line.engine_section));
                          const isEnabled =
                            tweakStates.find((s) => s.id === tweak.id)?.active ?? false;
                          const removeOnly = tweak.kind === "RemoveLines" && tweak.remove_only;
                          const isSavedEnabled =
                            savedTweakStates.find((s) => s.id === tweak.id)?.active ?? false;
                          if (removeOnly && isSavedEnabled) return null;
                          // Engine-section settings can't live in DefaultDeviceProfiles.ini;
                          // they need an engine file present in the pak.
                          const needsEngine = engineOnly && !hasAnyEngine(selectedPak);
                          const disabled = needsEngine;
                          return (
                            <QuickTweakRow
                              key={tweak.id}
                              tweak={tweak}
                              isEnabled={isEnabled}
                              disabledReason={
                                needsEngine ? "Requires an Engine.ini in this pak mod" : undefined
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
                    </div>
                  ))}
                </div>
              );
            })()}
        </div>
        <div ref={sentinelRef} aria-hidden className="h-px w-full shrink-0" />
      </div>

      {!atBottom && (
        <div
          aria-hidden
          className="pointer-events-none -mt-8 h-8 shrink-0 bg-linear-to-t from-background to-transparent"
        />
      )}

      {/* Save bar */}
      {selectedPak && !loading && dirty && (
        <div className="flex shrink-0 items-center gap-2 pt-2">
          <div className="flex min-w-0 flex-1 flex-wrap items-center gap-1.5">
            <span className="text-[11px] font-semibold uppercase text-muted-foreground">
              Pending ({pending.length})
            </span>
            {pending.map((entry) => {
              const label = definitions.find((d) => d.id === entry.id)?.label ?? entry.id;
              return (
                <Badge
                  key={entry.id}
                  variant="outline"
                  className={cn(
                    "rounded-sm px-1.5 py-0 text-[11px]",
                    entry.enabled
                      ? "border-ok/40 bg-ok/10 text-ok"
                      : "border-destructive/40 bg-destructive/10 text-destructive"
                  )}
                >
                  {entry.enabled ? label : `- ${label}`}
                </Badge>
              );
            })}
          </div>
          <Button variant="outline" size="sm" onClick={discardEdits} disabled={loading}>
            <Undo2 size={14} />
            Discard
          </Button>
          <Button variant="blue" size="sm" onClick={applyEdits} disabled={!dirty || applying}>
            {applying ? <RefreshCw size={14} className="animate-spin" /> : <Save size={14} />}
            {applying ? "Repacking…" : "Save"}
          </Button>
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
    <div className={cn("flex flex-col gap-2 px-3 py-3", disabled && "opacity-50")}>
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
            <span className="text-[11px] leading-snug text-warn mt-0.5">{disabledReason}</span>
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
