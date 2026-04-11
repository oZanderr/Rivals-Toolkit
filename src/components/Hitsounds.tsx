import { useState, useCallback, useEffect, useRef } from "react";

import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { confirm, open } from "@tauri-apps/plugin-dialog";
import {
  UploadCloud,
  FileAudio,
  CheckCircle2,
  XCircle,
  Trash2,
  Crosshair,
  Shield,
  Package,
} from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
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

interface Props {
  gamePath: string;
  isActive: boolean;
}

function formatSampleRateKHz(sampleRate: number): string {
  const khz = sampleRate / 1000;
  return Number.isInteger(khz) ? `${khz.toFixed(0)}kHz` : `${khz.toFixed(1)}kHz`;
}

export function Hitsounds({ gamePath, isActive }: Props) {
  const [headSlot, setHeadSlot] = useState<SlotState | null>(null);
  const [bodySlot, setBodySlot] = useState<SlotState | null>(null);
  const [modName, setModName] = useState("");
  const [building, setBuilding] = useState(false);
  const [isDragging, setIsDragging] = useState(false);
  const [hoveredDropSlot, setHoveredDropSlot] = useState<"head" | "body" | null>(null);
  const [buildResult, setBuildResult] = useState<{ msg: string; ok: boolean } | null>(null);
  const isActiveRef = useRef(isActive);
  const dropProcessingRef = useRef(false);
  const hoveredDropSlotRef = useRef<"head" | "body" | null>(null);
  const headRowRef = useRef<HTMLDivElement | null>(null);
  const bodyRowRef = useRef<HTMLDivElement | null>(null);

  isActiveRef.current = isActive;

  function setHoveredSlot(slot: "head" | "body" | null) {
    hoveredDropSlotRef.current = slot;
    setHoveredDropSlot((prev) => (prev === slot ? prev : slot));
  }

  function slotFromPosition(position: { x: number; y: number }): "head" | "body" | null {
    const dpr = window.devicePixelRatio || 1;
    const x = position.x / dpr;
    const y = position.y / dpr;

    const el = document.elementFromPoint(x, y) as HTMLElement | null;
    const byAttr = el?.closest("[data-drop-slot]")?.getAttribute("data-drop-slot");
    if (byAttr === "head" || byAttr === "body") return byAttr;

    const headRect = headRowRef.current?.getBoundingClientRect();
    if (
      headRect &&
      x >= headRect.left &&
      x <= headRect.right &&
      y >= headRect.top &&
      y <= headRect.bottom
    ) {
      return "head";
    }

    const bodyRect = bodyRowRef.current?.getBoundingClientRect();
    if (
      bodyRect &&
      x >= bodyRect.left &&
      x <= bodyRect.right &&
      y >= bodyRect.top &&
      y <= bodyRect.bottom
    ) {
      return "body";
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

  const validateAndSet = useCallback(
    async (path: string, setter: React.Dispatch<React.SetStateAction<SlotState | null>>) => {
      const name = path.split(/[\\/]/).pop() ?? path;
      try {
        const validation = await invoke<WavValidation>("validate_wav", { path });
        const isCompatible =
          (validation.channels === 1 || validation.channels === 2) &&
          validation.bits_per_sample === 16;
        setter({
          path,
          name,
          validation,
          error: isCompatible
            ? null
            : `Requires 16-bit mono or stereo (got ${validation.bits_per_sample}-bit, ${validation.channels}ch)`,
        });
      } catch (e) {
        setter({ path, name, validation: null, error: String(e) });
      }
    },
    []
  );

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

          const wavPaths = event.payload.paths.filter((p) => p.toLowerCase().endsWith(".wav"));
          if (wavPaths.length === 0) return;
          if (!targetSlot) return;

          dropProcessingRef.current = true;
          try {
            if (targetSlot === "head") {
              await validateAndSet(wavPaths[0], setHeadSlot);
            } else {
              await validateAndSet(wavPaths[0], setBodySlot);
            }
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

  async function pickWav(setter: React.Dispatch<React.SetStateAction<SlotState | null>>) {
    const selected = await open({
      multiple: false,
      filters: [{ name: "WAV Audio", extensions: ["wav"] }],
    });
    if (selected) {
      await validateAndSet(selected, setter);
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
      const shouldReplace = await confirm(
        `A mod named ${normalizedModName}_9999999_P.pak already exists in this folder. Replace it?`,
        {
          title: "oinkers-toolkit",
          kind: "warning",
        }
      );
      if (!shouldReplace) {
        return;
      }
    }

    setBuilding(true);
    try {
      const result = await invoke<string>("build_hitsound_mod", {
        gameRoot: gamePath,
        headWav: headSlot?.path ?? null,
        bodyWav: bodySlot?.path ?? null,
        modName: normalizedModName,
        outputDir: selectedOutputDir,
      });
      setBuildResult({ msg: result, ok: true });
    } catch (e) {
      setBuildResult({ msg: String(e), ok: false });
    } finally {
      setBuilding(false);
    }
  }

  useEffect(() => {
    if (!buildResult) return;

    const timeoutId = window.setTimeout(() => {
      setBuildResult(null);
    }, 4000);

    return () => {
      window.clearTimeout(timeoutId);
    };
  }, [buildResult]);

  function formatDuration(secs: number): string {
    const m = Math.floor(secs / 60);
    const s = (secs % 60).toFixed(1);
    return m > 0 ? `${m}m ${s}s` : `${s}s`;
  }

  const canBuild =
    gamePath &&
    (headSlot || bodySlot) &&
    !headSlot?.error &&
    !bodySlot?.error &&
    normalizeModName(modName).length > 0;

  const isConfigured = Boolean(gamePath);
  const hasValidSound = Boolean((headSlot && !headSlot.error) || (bodySlot && !bodySlot.error));
  const hasErrors = Boolean(headSlot?.error || bodySlot?.error);

  return (
    <div className="flex min-h-0 w-full flex-1 flex-col gap-4 overflow-y-auto">
      {/* Page header */}
      <div className="flex items-start justify-between gap-4">
        <div>
          <h2 className="text-xl font-bold">Hitsounds</h2>
          <p className="mt-1 text-xs text-muted-foreground">
            Build a hitsound mod from WAV files. 16-bit PCM (mono or stereo) at 48kHz recommended,
            but 44.1kHz should also be fine. Mono files are automatically converted to stereo.
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
                : hasValidSound
                  ? "border-[var(--green-accent-border)] bg-[var(--green-accent)] text-[var(--green-accent-foreground)]"
                  : "border-border bg-background text-muted-foreground"
            )}
          >
            {hasErrors ? "Validation issues" : hasValidSound ? "Audio ready" : "Awaiting audio"}
          </Badge>
        </div>
      </div>

      {/* Single unified panel */}
      <Card className="gap-0 overflow-hidden py-0">
        {/* Bodyshot row */}
        <SoundRow
          slotKey="body"
          rowRef={bodyRowRef}
          label="Bodyshot"
          icon={<Shield size={15} />}
          slot={bodySlot}
          onPick={() => pickWav(setBodySlot)}
          onClear={() => setBodySlot(null)}
          formatDuration={formatDuration}
          disabled={building}
          showDropOverlay={isDragging && hoveredDropSlot === "body"}
          onDragOverRow={() => setHoveredSlot("body")}
        />

        <div className="mx-5 h-px bg-border" />

        {/* Headshot row */}
        <SoundRow
          slotKey="head"
          rowRef={headRowRef}
          label="Headshot"
          icon={<Crosshair size={15} />}
          slot={headSlot}
          onPick={() => pickWav(setHeadSlot)}
          onClear={() => setHeadSlot(null)}
          formatDuration={formatDuration}
          disabled={building}
          showDropOverlay={isDragging && hoveredDropSlot === "head"}
          onDragOverRow={() => setHoveredSlot("head")}
        />

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
                  : "text-[var(--red-accent-foreground)]"
              )}
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
  slotKey: "head" | "body";
  rowRef: React.RefObject<HTMLDivElement | null>;
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
      className="relative flex h-20 items-center gap-4 px-5 transition-colors"
      onDragOver={(e) => {
        e.preventDefault();
        onDragOverRow();
      }}
    >
      {/* Drop overlay */}
      {showDropOverlay && (
        <div className="pointer-events-none absolute inset-0 z-20 flex items-center justify-center gap-2 bg-background/92 backdrop-blur-sm">
          <UploadCloud size={16} className="text-foreground" />
          <span className="text-xs font-semibold text-foreground">Drop .wav for {label}</span>
        </div>
      )}

      {/* Label column */}
      <div className="flex w-24 shrink-0 items-center gap-2">
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
          <span className="text-xs">Drop .wav here or click to browse</span>
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
