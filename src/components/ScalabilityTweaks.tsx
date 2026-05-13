import { useState, useEffect, useCallback, useRef } from "react";

import { invoke } from "@tauri-apps/api/core";
import { openPath } from "@tauri-apps/plugin-opener";
import {
  Save,
  CheckCircle2,
  XCircle,
  FolderOpen,
  Pencil,
  Plus,
  RefreshCw,
  Search,
  Info,
  Trash2,
  Undo2,
  X,
} from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Slider } from "@/components/ui/slider";
import { Switch } from "@/components/ui/switch";
import { Tip } from "@/components/ui/tooltip";
import { useSaveHotkeys } from "@/hooks/useSaveHotkeys";
import { useScrollAtBottom } from "@/hooks/useScrollAtBottom";
import { emitTweakProfilesChanged, onTweakProfilesChanged } from "@/lib/tweakProfileEvents";
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

interface TweakPreset {
  name: string;
  settings: TweakSetting[];
  created_at: number;
  modified_at: number;
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
  const [presets, setPresets] = useState<TweakPreset[]>([]);
  const [selectedPreset, setSelectedPreset] = useState<string>("");
  const [appliedPresetAt, setAppliedPresetAt] = useState<number | null>(null);
  const [savingAs, setSavingAs] = useState(false);
  const [renamingAs, setRenamingAs] = useState(false);
  const [newPresetName, setNewPresetName] = useState("");

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

  const refreshPresets = useCallback(async () => {
    try {
      const list = await invoke<TweakPreset[]>("list_tweak_profiles");
      setPresets(list);
      setSelectedPreset((prev) => (list.some((p) => p.name === prev) ? prev : ""));
    } catch {
      setPresets([]);
      setSelectedPreset("");
      setAppliedPresetAt(null);
    }
  }, []);

  useEffect(() => {
    refreshPresets();
    return onTweakProfilesChanged(refreshPresets);
  }, [refreshPresets]);

  // Clear stale selection when current preset disappears from list
  useEffect(() => {
    if (selectedPreset && !presets.some((p) => p.name === selectedPreset)) {
      setSelectedPreset("");
      setAppliedPresetAt(null);
    }
  }, [presets, selectedPreset]);

  function buildCurrentScalabilitySettings(): TweakSetting[] {
    return definitions
      .filter((d) => !d.pak_only)
      .map((def) => ({
        id: def.id,
        enabled: enabled[def.id] ?? false,
        value:
          def.kind === "Slider"
            ? String(values[def.id] != null ? values[def.id] : def.default_value)
            : (values[def.id] ?? null),
      }));
  }

  function applyPresetToDraft(preset: TweakPreset) {
    const scalabilityIds = new Set(definitions.filter((d) => !d.pak_only).map((d) => d.id));
    const newEnabled = { ...enabled };
    const newValues = { ...values };
    for (const s of preset.settings) {
      if (!scalabilityIds.has(s.id)) continue;
      newEnabled[s.id] = s.enabled;
      if (s.value != null) newValues[s.id] = s.value;
    }
    setEnabled(newEnabled);
    setValues(newValues);
    setSelectedPreset(preset.name);
    setAppliedPresetAt(preset.modified_at);
  }

  async function saveCurrentAsPreset() {
    const trimmed = newPresetName.trim();
    if (!trimmed) return;
    try {
      const profile = await invoke<TweakPreset>("save_tweak_profile", {
        name: trimmed,
        settings: buildCurrentScalabilitySettings(),
      });
      setNewPresetName("");
      setSavingAs(false);
      setPresets((prev) => [...prev.filter((p) => p.name !== profile.name), profile]);
      setSelectedPreset(profile.name);
      setAppliedPresetAt(profile.modified_at);
      emitTweakProfilesChanged();
      showStatus(`Saved preset "${profile.name}"`, "ok");
    } catch (e) {
      showStatus(String(e), "err");
    }
  }

  async function overwriteSelectedPreset() {
    if (!selectedPreset) return;
    const existing = presets.find((p) => p.name === selectedPreset);
    if (!existing) return;
    // Preserve pak-only entries from the existing preset, replace scalability entries.
    const scalabilityIds = new Set(definitions.filter((d) => !d.pak_only).map((d) => d.id));
    const preserved = existing.settings.filter((s) => !scalabilityIds.has(s.id));
    const merged = [...preserved, ...buildCurrentScalabilitySettings()];
    try {
      const profile = await invoke<TweakPreset>("overwrite_tweak_profile", {
        name: selectedPreset,
        settings: merged,
      });
      setPresets((prev) => prev.map((p) => (p.name === profile.name ? profile : p)));
      setAppliedPresetAt(profile.modified_at);
      emitTweakProfilesChanged();
      showStatus(`Updated preset "${profile.name}"`, "ok");
    } catch (e) {
      showStatus(String(e), "err");
    }
  }

  // Auto-reapply when the selected preset is modified on another tab.
  useEffect(() => {
    if (!selectedPreset || appliedPresetAt == null) return;
    const preset = presets.find((p) => p.name === selectedPreset);
    if (!preset || preset.modified_at <= appliedPresetAt) return;
    applyPresetToDraft(preset);
    showStatus(`Preset "${preset.name}" was updated, reapplied`, "info");
  }, [presets, selectedPreset, appliedPresetAt]); // eslint-disable-line react-hooks/exhaustive-deps

  async function deleteSelectedPreset() {
    if (!selectedPreset) return;
    const name = selectedPreset;
    try {
      await invoke("delete_tweak_profile", { name });
      setSelectedPreset("");
      setAppliedPresetAt(null);
      setPresets((prev) => prev.filter((p) => p.name !== name));
      emitTweakProfilesChanged();
      showStatus(`Deleted preset "${name}"`, "ok");
    } catch (e) {
      showStatus(String(e), "err");
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
      showStatus(`Renamed preset to "${profile.name}"`, "ok");
    } catch (e) {
      showStatus(String(e), "err");
    }
  }

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
      const changeCount = pendingChanges.length;
      const modified = await invoke<string>("apply_tweaks", { content, settings });
      setContent(modified);
      await invoke("write_scalability", { path: filePath, content: modified });
      const fileName = filePath.split(/[\\/]/).pop() || "Scalability.ini";
      const label = changeCount === 1 ? "change" : "changes";
      showStatus(`Applied ${changeCount} ${label} to ${fileName}`, "ok");
      onSaved(modified);
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

  const { atBottom, scrollRef, sentinelRef } = useScrollAtBottom();
  const discardChanges = useCallback(() => {
    setEnabled(savedEnabled);
    setValues(savedValues);
    setSelectedPreset("");
    setAppliedPresetAt(null);
  }, [savedEnabled, savedValues]);
  useSaveHotkeys({ dirty, onSave: applyAndSave, onDiscard: discardChanges });

  if (!defsLoaded) return null;

  return (
    <div className="flex flex-1 min-h-0 flex-col">
      <div ref={scrollRef} className="flex-1 min-h-0 overflow-y-auto [scrollbar-gutter:stable]">
        <div className="flex flex-col gap-5">
          {/* Config file location */}
          <div className="flex flex-col overflow-hidden rounded-md border border-border">
            <div className="flex items-center justify-between border-b border-border bg-card px-3 py-2">
              <div className="flex min-w-0 flex-1 items-center gap-2">
                <span className="shrink-0 text-sm font-semibold">Config File</span>
                {detectBadge && !status && (
                  <span
                    className={cn(
                      "flex items-center gap-1 text-[12px] font-medium",
                      detectBadge === "Not found" ? "text-warn" : "text-ok"
                    )}
                  >
                    <CheckCircle2 size={13} strokeWidth={2.5} />
                    {detectBadge}
                  </span>
                )}
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
              </div>
              <Button
                variant="ghost"
                size="sm"
                disabled={!filePath}
                onClick={() => filePath && openPath(filePath.replace(/[/\\][^/\\]+$/, ""))}
              >
                Show in Explorer
              </Button>
            </div>
            <div className="relative">
              <Tip content={filePath} disabled={!filePath}>
                <Input
                  className="pr-16 rounded-none border-0 shadow-none font-mono text-[12px] focus-visible:ring-0 focus-visible:border-0"
                  value={filePath}
                  onChange={(e) => setFilePath(e.target.value)}
                  placeholder="Path to Scalability.ini\u2026"
                />
              </Tip>
              <div className="absolute right-1 top-1/2 flex -translate-y-1/2 items-center gap-0.5">
                <Tip content="Browse for config file">
                  <Button variant="ghost" size="icon-sm" onClick={onBrowse}>
                    <FolderOpen size={14} />
                  </Button>
                </Tip>
                <Tip content="Reload from file">
                  <Button variant="ghost" size="icon-sm" onClick={onReload} disabled={!filePath}>
                    <RefreshCw size={14} />
                  </Button>
                </Tip>
                <Tip content="Auto-detect path">
                  <Button variant="ghost" size="icon-sm" onClick={onDetect} disabled={detecting}>
                    <Search size={14} className={cn(detecting && "animate-pulse")} />
                  </Button>
                </Tip>
              </div>
            </div>
          </div>

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

          {/* Preset bar */}
          <div className="flex flex-wrap items-center gap-2 rounded-md border border-border bg-card/40 px-3 py-2">
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
                    const p = presets.find((x) => x.name === name);
                    if (p) applyPresetToDraft(p);
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

          <div className="grid gap-5 xl:grid-cols-2 2xl:grid-cols-3">
            {Object.entries(categories).map(([category, tweaks]) => (
              <div key={category} className="overflow-hidden rounded-md border border-border">
                <div className="border-b border-border bg-card px-3 py-2">
                  <span className="text-sm font-semibold">{category}</span>
                </div>
                <div className="flex flex-col divide-y divide-border/50">
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
              </div>
            ))}
          </div>
        </div>
        <div ref={sentinelRef} aria-hidden className="h-px w-full shrink-0" />
      </div>

      {!atBottom && (
        <div
          aria-hidden
          className="pointer-events-none -mt-8 h-8 shrink-0 bg-gradient-to-t from-background to-transparent"
        />
      )}

      {/* Save bar */}
      {dirty && (
        <div className="flex shrink-0 items-center gap-2 pt-2">
          <div className="flex min-w-0 flex-1 flex-wrap items-center gap-1.5">
            <span className="text-[11px] font-semibold uppercase text-muted-foreground">
              Pending ({pendingChanges.length})
            </span>
            {pendingChanges.map((change) => (
              <Badge
                key={change.id}
                variant="outline"
                className={cn(
                  "rounded-sm px-1.5 py-0 text-[11px] font-mono",
                  change.kind === "remove"
                    ? "border-destructive/40 bg-destructive/10 text-destructive"
                    : "border-ok/40 bg-ok/10 text-ok"
                )}
              >
                {change.kind === "remove" ? `- ${change.display}` : change.display}
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
    <div className={cn("flex flex-col gap-2 px-3 py-3", disabled && "opacity-50")}>
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
            <span className="text-[11px] leading-snug text-warn mt-0.5">{disabledReason}</span>
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
