import { useState, useCallback, useEffect, useRef } from "react";

import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import {
  UploadCloud,
  FileAudio,
  CheckCircle2,
  XCircle,
  Trash2,
  Crosshair,
  Shield,
  Skull,
  Package,
  Target,
} from "lucide-react";

import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { Badge } from "@/components/ui/badge";
import { Button, buttonVariants } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";

interface WavValidation {
  channels: number;
  sample_rate: number;
  bits_per_sample: number;
  duration: number;
}

interface SlotState {
  path: string;
  name: string;
  validation: WavValidation | null;
  error: string | null;
}

type SlotKey = "body_hit" | "head_hit" | "body_kill" | "head_kill";

interface SlotConfig {
  key: SlotKey;
  label: string;
  group: "hit" | "kill";
  icon: React.ReactNode;
}

const SLOT_CONFIGS: SlotConfig[] = [
  { key: "body_hit", label: "Bodyshot", group: "hit", icon: <Shield size={15} /> },
  { key: "head_hit", label: "Headshot", group: "hit", icon: <Crosshair size={15} /> },
  { key: "body_kill", label: "Bodyshot Kill", group: "kill", icon: <Skull size={15} /> },
  { key: "head_kill", label: "Headshot Kill", group: "kill", icon: <Target size={15} /> },
];

const SLOT_KEYS: SlotKey[] = SLOT_CONFIGS.map((c) => c.key);

interface Props {
  gamePath: string;
  isActive: boolean;
}

type ResultState = { msg: string; ok: boolean; revealPath?: string } | null;

function formatSampleRateKHz(sampleRate: number): string {
  const khz = sampleRate / 1000;
  return Number.isInteger(khz) ? `${khz.toFixed(0)}kHz` : `${khz.toFixed(1)}kHz`;
}

export function Hitsounds({ gamePath, isActive }: Props) {
  const [slots, setSlots] = useState<Record<SlotKey, SlotState | null>>({
    body_hit: null,
    head_hit: null,
    body_kill: null,
    head_kill: null,
  });
  const [modName, setModName] = useState("");
  const [building, setBuilding] = useState(false);
  const [isDragging, setIsDragging] = useState(false);
  const [hoveredDropSlot, setHoveredDropSlot] = useState<SlotKey | null>(null);
  const [buildResult, setBuildResult] = useState<ResultState>(null);
  const [replaceConfirm, setReplaceConfirm] = useState<{
    modName: string;
    outputDir: string;
    pakPath: string;
  } | null>(null);
  const isActiveRef = useRef(isActive);
  const dropProcessingRef = useRef(false);
  const hoveredDropSlotRef = useRef<SlotKey | null>(null);
  const slotRefs = useRef<Record<SlotKey, HTMLDivElement | null>>({
    body_hit: null,
    head_hit: null,
    body_kill: null,
    head_kill: null,
  });

  isActiveRef.current = isActive;

  function setSlot(key: SlotKey, value: SlotState | null) {
    setSlots((prev) => ({ ...prev, [key]: value }));
  }

  function setHoveredSlot(slot: SlotKey | null) {
    hoveredDropSlotRef.current = slot;
    setHoveredDropSlot((prev) => (prev === slot ? prev : slot));
  }

  function slotFromPosition(position: { x: number; y: number }): SlotKey | null {
    const dpr = window.devicePixelRatio || 1;
    const x = position.x / dpr;
    const y = position.y / dpr;

    const el = document.elementFromPoint(x, y) as HTMLElement | null;
    const byAttr = el
      ?.closest("[data-drop-slot]")
      ?.getAttribute("data-drop-slot") as SlotKey | null;
    if (byAttr && SLOT_KEYS.includes(byAttr)) return byAttr;

    for (const key of SLOT_KEYS) {
      const rect = slotRefs.current[key]?.getBoundingClientRect();
      if (rect && x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom) {
        return key;
      }
    }

    return null;
  }

  function normalizeModName(input: string): string {
    const trimmed = input.trim();
    const withoutExt = trimmed.replace(/\.pak$/i, "");
    const withoutVersionedSuffix = withoutExt.replace(/_9999999_P$/i, "");
    const withoutSuffix = withoutVersionedSuffix.replace(/_P$/i, "");
    return withoutSuffix.trim();
  }

  const validateAndSet = useCallback(async (path: string, key: SlotKey) => {
    const name = path.split(/[\\/]/).pop() ?? path;
    try {
      const validation = await invoke<WavValidation>("validate_wav", { path });
      const isCompatible =
        (validation.channels === 1 || validation.channels === 2) &&
        validation.bits_per_sample === 16;
      setSlot(key, {
        path,
        name,
        validation,
        error: isCompatible
          ? null
          : `Incompatible format (${validation.bits_per_sample}-bit, ${validation.channels}ch)`,
      });
    } catch (e) {
      setSlot(key, { path, name, validation: null, error: String(e) });
    }
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    getCurrentWindow()
      .onDragDropEvent(async (event) => {
        if (event.payload.type === "enter") {
          if (isActiveRef.current) {
            setIsDragging(true);
            setHoveredSlot(slotFromPosition(event.payload.position));
          }
        } else if (event.payload.type === "over") {
          if (isActiveRef.current) {
            setHoveredSlot(slotFromPosition(event.payload.position));
          }
        } else if (event.payload.type === "leave") {
          setIsDragging(false);
          setHoveredSlot(null);
        } else if (event.payload.type === "drop") {
          setIsDragging(false);
          const targetSlot = slotFromPosition(event.payload.position) ?? hoveredDropSlotRef.current;
          setHoveredSlot(null);
          if (!isActiveRef.current || dropProcessingRef.current) return;

          const wavPaths = event.payload.paths.filter((p) => {
            const lower = p.toLowerCase();
            return lower.endsWith(".wav") || lower.endsWith(".ogg");
          });
          if (wavPaths.length === 0) return;
          if (!targetSlot) return;

          dropProcessingRef.current = true;
          try {
            await validateAndSet(wavPaths[0], targetSlot);
          } finally {
            dropProcessingRef.current = false;
          }
        }
      })
      .then((fn) => {
        unlisten = fn;
      });

    return () => {
      unlisten?.();
    };
  }, [validateAndSet]);

  async function pickWav(key: SlotKey) {
    const selected = await open({
      multiple: false,
      filters: [{ name: "Audio Files", extensions: ["wav", "ogg"] }],
    });
    if (selected) {
      await validateAndSet(selected, key);
    }
  }

  async function buildMod() {
    const normalizedModName = normalizeModName(modName);
    const defaultOutputDir = `${gamePath}\\MarvelGame\\Marvel\\Content\\Paks\\~mods`;
    const selectedOutputDir = await open({
      directory: true,
      multiple: false,
      defaultPath: defaultOutputDir,
    });
    if (!selectedOutputDir || typeof selectedOutputDir !== "string") {
      return;
    }

    setBuildResult(null);

    const outputPakPath = `${selectedOutputDir}\\${normalizedModName}_9999999_P.pak`;
    const alreadyExists = await invoke<boolean>("path_exists", { path: outputPakPath });
    if (alreadyExists) {
      setReplaceConfirm({
        modName: normalizedModName,
        outputDir: selectedOutputDir,
        pakPath: outputPakPath,
      });
      return;
    }

    await runBuild(normalizedModName, selectedOutputDir, outputPakPath);
  }

  async function runBuild(normalizedModName: string, outputDir: string, outputPakPath: string) {
    const wavs: Record<string, string> = {};
    for (const key of SLOT_KEYS) {
      const slot = slots[key];
      if (slot && !slot.error) {
        wavs[key] = slot.path;
      }
    }

    setBuilding(true);
    try {
      const result = await invoke<string>("build_hitsound_mod", {
        gameRoot: gamePath,
        wavs,
        modName: normalizedModName,
        outputDir,
      });
      setBuildResult({ msg: result, ok: true, revealPath: outputPakPath });
    } catch (e) {
      setBuildResult({ msg: String(e), ok: false });
    } finally {
      setBuilding(false);
    }
  }

  useEffect(() => {
    if (!buildResult) return;
    const timeoutId = window.setTimeout(() => setBuildResult(null), 4000);
    return () => window.clearTimeout(timeoutId);
  }, [buildResult]);

  function formatDuration(secs: number): string {
    const m = Math.floor(secs / 60);
    const s = (secs % 60).toFixed(1);
    return m > 0 ? `${m}m ${s}s` : `${s}s`;
  }

  const filledSlots = SLOT_KEYS.filter((k) => slots[k] !== null);
  const hasAnyValid = filledSlots.some((k) => !slots[k]?.error);
  const hasErrors = filledSlots.some((k) => slots[k]?.error);

  const canBuild = gamePath && hasAnyValid && !hasErrors && normalizeModName(modName).length > 0;
  const isConfigured = Boolean(gamePath);

  const hitSlots = SLOT_CONFIGS.filter((c) => c.group === "hit");
  const killSlots = SLOT_CONFIGS.filter((c) => c.group === "kill");

  return (
    <div className="flex min-h-0 w-full flex-1 flex-col gap-4 overflow-y-auto">
      {/* Page header */}
      <div className="flex items-start justify-between gap-4">
        <div>
          <h2 className="text-xl font-bold">Hitsounds</h2>
          <p className="mt-1 text-xs text-muted-foreground">
            Build a hitsound mod from WAV or OGG files. 16-bit PCM, mono or stereo, 48kHz
            recommended. To extract WAVs from a mod, select its{" "}
            <span className="font-medium text-foreground">bnk_ui_battle.bnk</span> in Asset Manager.
          </p>
        </div>
        <div className="flex shrink-0 items-center gap-2 pt-0.5">
          <Badge
            variant="outline"
            className={cn(
              "rounded-full px-2.5 py-1",
              isConfigured
                ? "border-[var(--green-accent-border)] bg-[var(--green-accent)] text-[var(--green-accent-foreground)]"
                : "border-border bg-background text-muted-foreground"
            )}
          >
            {isConfigured ? "Game detected" : "Game not detected"}
          </Badge>
          <Badge
            variant="outline"
            className={cn(
              "rounded-full px-2.5 py-1",
              hasErrors
                ? "border-[var(--red-accent-border)] bg-[var(--red-accent)] text-[var(--red-accent-foreground)]"
                : hasAnyValid
                  ? "border-[var(--green-accent-border)] bg-[var(--green-accent)] text-[var(--green-accent-foreground)]"
                  : "border-border bg-background text-muted-foreground"
            )}
          >
            {hasErrors ? "Validation issues" : hasAnyValid ? "Audio ready" : "Awaiting audio"}
          </Badge>
        </div>
      </div>

      {/* Sound slots + build */}
      <Card className="gap-0 overflow-hidden py-0">
        {/* Hit sounds group */}
        <div className="px-5 pt-4 pb-1">
          <span className="text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
            Hit Sounds
          </span>
        </div>
        {hitSlots.map((config, i) => (
          <div key={config.key}>
            <SoundRow
              slotKey={config.key}
              rowRef={(el) => {
                slotRefs.current[config.key] = el;
              }}
              label={config.label}
              icon={config.icon}
              slot={slots[config.key]}
              onPick={() => pickWav(config.key)}
              onClear={() => setSlot(config.key, null)}
              formatDuration={formatDuration}
              disabled={building}
              showDropOverlay={isDragging && hoveredDropSlot === config.key}
              onDragOverRow={() => setHoveredSlot(config.key)}
            />
            {i < hitSlots.length - 1 && <div className="mx-5 h-px bg-border" />}
          </div>
        ))}

        <div className="h-px bg-border" />

        {/* Kill sounds group */}
        <div className="px-5 pt-4 pb-1">
          <span className="text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
            Kill Confirmed Sounds
          </span>
        </div>
        {killSlots.map((config, i) => (
          <div key={config.key}>
            <SoundRow
              slotKey={config.key}
              rowRef={(el) => {
                slotRefs.current[config.key] = el;
              }}
              label={config.label}
              icon={config.icon}
              slot={slots[config.key]}
              onPick={() => pickWav(config.key)}
              onClear={() => setSlot(config.key, null)}
              formatDuration={formatDuration}
              disabled={building}
              showDropOverlay={isDragging && hoveredDropSlot === config.key}
              onDragOverRow={() => setHoveredSlot(config.key)}
            />
            {i < killSlots.length - 1 && <div className="mx-5 h-px bg-border" />}
          </div>
        ))}

        <div className="h-px bg-border" />

        {/* Build section */}
        <div className="flex flex-col gap-3 px-5 py-5">
          <label className="text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
            Mod Name
          </label>
          <div className="flex items-center gap-2.5">
            <Input
              value={modName}
              onChange={(e) => {
                setModName(e.target.value);
                setBuildResult(null);
              }}
              className="h-9 flex-1"
              placeholder="Enter mod name"
              disabled={building}
            />
            <span className="shrink-0 text-xs text-muted-foreground">_9999999_P.pak</span>
          </div>
          <Button
            variant="blue"
            disabled={!canBuild || building}
            onClick={buildMod}
            className="h-10 w-full gap-2"
          >
            {building ? <Package size={14} className="animate-spin" /> : <Package size={14} />}
            {building ? "Building..." : "Build Hitsound Mod"}
          </Button>
          {buildResult && (
            <div
              className={cn(
                "flex items-center gap-1.5 text-xs",
                buildResult.ok
                  ? "text-[var(--green-accent-foreground)]"
                  : "text-[var(--red-accent-foreground)]",
                buildResult.revealPath && "cursor-pointer hover:underline"
              )}
              onClick={
                buildResult.revealPath ? () => revealItemInDir(buildResult.revealPath!) : undefined
              }
              title={buildResult.revealPath ? "Click to reveal in explorer" : undefined}
            >
              {buildResult.ok ? (
                <CheckCircle2 size={13} className="shrink-0" />
              ) : (
                <XCircle size={13} className="shrink-0" />
              )}
              <span className="truncate">{buildResult.msg}</span>
            </div>
          )}
        </div>

        {/* Game not detected warning */}
        {!gamePath && (
          <div className="border-t border-[var(--red-accent-border)] bg-[var(--red-accent)] px-5 py-2.5 text-xs text-[var(--red-accent-foreground)]">
            Game not detected. Visit the Home tab to detect your install first.
          </div>
        )}
      </Card>

      <AlertDialog
        open={!!replaceConfirm}
        onOpenChange={(isOpen) => {
          if (!isOpen) setReplaceConfirm(null);
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Replace Existing Mod</AlertDialogTitle>
            <AlertDialogDescription>
              <span className="font-semibold text-foreground">
                {replaceConfirm?.modName}_9999999_P.pak
              </span>{" "}
              already exists in this folder. Do you want to replace it?
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              className={buttonVariants({ variant: "blue" })}
              onClick={() => {
                if (replaceConfirm) {
                  runBuild(
                    replaceConfirm.modName,
                    replaceConfirm.outputDir,
                    replaceConfirm.pakPath
                  );
                }
                setReplaceConfirm(null);
              }}
            >
              Replace
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  );
}

function SoundRow({
  slotKey,
  rowRef,
  label,
  icon,
  slot,
  onPick,
  onClear,
  formatDuration,
  disabled,
  showDropOverlay,
  onDragOverRow,
}: {
  slotKey: string;
  rowRef: React.RefCallback<HTMLDivElement>;
  label: string;
  icon: React.ReactNode;
  slot: SlotState | null;
  onPick: () => void;
  onClear: () => void;
  formatDuration: (s: number) => string;
  disabled: boolean;
  showDropOverlay: boolean;
  onDragOverRow: () => void;
}) {
  const isReady = Boolean(slot && !slot.error);

  return (
    <div
      ref={rowRef}
      data-drop-slot={slotKey}
      className="relative flex h-16 items-center gap-4 px-5 transition-colors"
      onDragOver={(e) => {
        e.preventDefault();
        onDragOverRow();
      }}
    >
      {/* Drop overlay */}
      {showDropOverlay && (
        <div className="pointer-events-none absolute inset-0 z-20 flex items-center justify-center gap-2 bg-background/92 backdrop-blur-sm">
          <UploadCloud size={16} className="text-foreground" />
          <span className="text-xs font-semibold text-foreground">Drop audio for {label}</span>
        </div>
      )}

      {/* Label column */}
      <div className="flex w-32 shrink-0 items-center gap-2">
        <span className="text-muted-foreground">{icon}</span>
        <span className="text-sm font-semibold">{label}</span>
      </div>

      {/* Content column */}
      {slot ? (
        <div className="flex min-w-0 flex-1 items-center gap-2.5">
          <FileAudio
            size={14}
            className={cn(
              "shrink-0",
              slot.error ? "text-[var(--red-accent-foreground)]" : "text-muted-foreground"
            )}
          />
          <span className="truncate text-sm font-medium">{slot.name}</span>
          {slot.validation && !slot.error && (
            <span className="shrink-0 text-[11px] text-muted-foreground">
              {formatSampleRateKHz(slot.validation.sample_rate)}
              {" · "}
              {slot.validation.bits_per_sample}-bit
              {" · "}
              {formatDuration(slot.validation.duration)}
              {slot.validation.sample_rate !== 48000 && (
                <span className="ml-1.5 rounded-full border border-border bg-background px-1.5 py-0.5">
                  48kHz recommended
                </span>
              )}
            </span>
          )}
          {slot.error && (
            <span className="shrink-0 text-[11px] text-[var(--red-accent-foreground)]">
              {slot.error}
            </span>
          )}
        </div>
      ) : (
        <button
          onClick={onPick}
          disabled={disabled}
          className="flex flex-1 items-center gap-2 text-muted-foreground transition-colors hover:text-foreground disabled:cursor-not-allowed disabled:opacity-50"
        >
          <UploadCloud size={13} className="shrink-0" />
          <span className="text-xs">Drop .wav/.ogg here or click to browse</span>
        </button>
      )}

      {/* Right column */}
      <div className="flex shrink-0 items-center gap-2">
        <Badge
          variant="outline"
          className={cn(
            "rounded-full px-2 py-0.5 text-[10px]",
            slot?.error
              ? "border-[var(--red-accent-border)] bg-[var(--red-accent)] text-[var(--red-accent-foreground)]"
              : isReady
                ? "border-[var(--green-accent-border)] bg-[var(--green-accent)] text-[var(--green-accent-foreground)]"
                : "border-border bg-background text-muted-foreground"
          )}
        >
          {slot?.error ? "Invalid" : isReady ? "Ready" : "Empty"}
        </Badge>
        {slot && (
          <Button
            size="icon-xs"
            variant="ghost"
            onClick={onClear}
            disabled={disabled}
            className="text-muted-foreground hover:text-[var(--color-err)]"
            title="Remove"
          >
            <Trash2 size={13} />
          </Button>
        )}
      </div>
    </div>
  );
}
