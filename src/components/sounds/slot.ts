export interface WavValidation {
  channels: number;
  sample_rate: number;
  bits_per_sample: number;
  duration: number;
  peak_dbfs: number;
}

export interface SlotState {
  path: string;
  name: string;
  validation: WavValidation | null;
  error: string | null;
  gainDb: number;
}

export const GAIN_MIN_DB = -18;
export const GAIN_MAX_DB = 18;
export const GAIN_STEP_DB = 0.5;
export const GAIN_DEFAULT_DB = 0;

export function formatGainDb(db: number): string {
  const sign = db > 0 ? "+" : "";
  return `${sign}${db.toFixed(1)} dB`;
}

export function formatSampleRateKHz(sampleRate: number): string {
  const khz = sampleRate / 1000;
  return Number.isInteger(khz) ? `${khz.toFixed(0)}kHz` : `${khz.toFixed(1)}kHz`;
}

export function formatDuration(secs: number): string {
  const m = Math.floor(secs / 60);
  const s = (secs % 60).toFixed(1);
  return m > 0 ? `${m}m ${s}s` : `${s}s`;
}
