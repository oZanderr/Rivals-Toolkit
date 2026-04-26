import { AlertTriangle, FileAudio, RotateCcw, Trash2, UploadCloud } from "lucide-react";

import {
  formatDuration,
  formatGainDb,
  formatSampleRateKHz,
  GAIN_DEFAULT_DB,
  GAIN_MAX_DB,
  GAIN_MIN_DB,
  GAIN_STEP_DB,
  type SlotState,
} from "./slot";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Slider } from "@/components/ui/slider";
import { Tip } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";

export function SoundRow({
  slotKey,
  label,
  icon,
  slot,
  onPick,
  onClear,
  onGainChange,
  disabled,
  showDropOverlay,
  onDragOverRow,
}: {
  slotKey: string;
  label: string;
  icon: React.ReactNode;
  slot: SlotState | null;
  onPick: () => void;
  onClear: () => void;
  onGainChange: (db: number) => void;
  disabled: boolean;
  showDropOverlay: boolean;
  onDragOverRow: () => void;
}) {
  return (
    <div
      data-drop-slot={slotKey}
      className="relative flex h-12 min-h-12 max-h-12 items-center gap-4 overflow-hidden rounded-sm px-3 transition-colors hover:bg-secondary/50"
      onDragOver={(e) => {
        e.preventDefault();
        onDragOverRow();
      }}
    >
      {showDropOverlay && (
        <div className="pointer-events-none absolute inset-0 z-20 flex items-center justify-center gap-2 bg-background/92 backdrop-blur-sm">
          <UploadCloud size={16} className="text-foreground" />
          <span className="text-xs font-semibold text-foreground">Drop audio for {label}</span>
        </div>
      )}

      <div className="flex w-32 shrink-0 items-center gap-2">
        <span className="text-muted-foreground">{icon}</span>
        <span className="text-sm font-semibold">{label}</span>
      </div>

      {slot ? (
        <div className="flex min-w-0 flex-1 items-center gap-2.5">
          <FileAudio
            size={14}
            className={cn(
              "shrink-0",
              slot.error ? "text-red-accent-foreground" : "text-muted-foreground"
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
            <span className="shrink-0 text-[11px] text-red-accent-foreground">{slot.error}</span>
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

      {slot && !slot.error && (
        <div className="flex w-60 shrink-0 items-center gap-2">
          {(() => {
            const peak = slot.validation?.peak_dbfs;
            const postGainPeak =
              peak !== undefined && Number.isFinite(peak) ? peak + slot.gainDb : null;
            const willClip = postGainPeak !== null && postGainPeak > 0;
            return (
              <>
                <Slider
                  min={GAIN_MIN_DB}
                  max={GAIN_MAX_DB}
                  step={GAIN_STEP_DB}
                  value={[slot.gainDb]}
                  onValueChange={(v) => onGainChange(v[0] ?? GAIN_DEFAULT_DB)}
                  disabled={disabled}
                  className="flex-1"
                />
                <span className="w-14 shrink-0 text-right font-mono text-[11px] tabular-nums text-muted-foreground">
                  {formatGainDb(slot.gainDb)}
                </span>
                <Tip
                  content={
                    willClip
                      ? `Will clip: post-gain peak +${postGainPeak.toFixed(1)} dB exceeds 0 dBFS. Lower the gain by at least ${postGainPeak.toFixed(1)} dB to avoid distortion.`
                      : ""
                  }
                  disabled={!willClip}
                >
                  <AlertTriangle
                    size={13}
                    className={cn(
                      "shrink-0 text-amber-400 transition-opacity",
                      willClip ? "opacity-100" : "opacity-0 pointer-events-none"
                    )}
                    aria-hidden={!willClip}
                    aria-label={willClip ? "Will clip on build" : undefined}
                  />
                </Tip>
                <Tip content="Reset to 0 dB">
                  <Button
                    size="icon-xs"
                    variant="ghost"
                    onClick={() => onGainChange(GAIN_DEFAULT_DB)}
                    disabled={disabled || slot.gainDb === GAIN_DEFAULT_DB}
                    className="text-muted-foreground hover:text-foreground"
                  >
                    <RotateCcw size={12} />
                  </Button>
                </Tip>
              </>
            );
          })()}
        </div>
      )}

      <div className="flex shrink-0 items-center gap-2">
        {slot && (
          <Badge
            variant="outline"
            className={cn(
              "rounded-full px-2 py-0.5 text-[10px]",
              slot.error
                ? "border-red-accent-border bg-red-accent text-red-accent-foreground"
                : "border-green-accent-border bg-green-accent text-green-accent-foreground"
            )}
          >
            {slot.error ? "Invalid" : "Ready"}
          </Badge>
        )}
        {slot && (
          <Tip content="Remove">
            <Button
              size="icon-xs"
              variant="ghost"
              onClick={onClear}
              disabled={disabled}
              className="text-muted-foreground hover:text-err"
            >
              <Trash2 size={13} />
            </Button>
          </Tip>
        )}
      </div>
    </div>
  );
}
