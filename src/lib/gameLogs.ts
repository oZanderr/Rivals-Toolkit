export type Outcome =
  | "running"
  | "clean_exit"
  | "crashed"
  | "fatal_error"
  | "ended_abruptly"
  | "empty"
  | "unreadable";
export type Verbosity = "fatal" | "error" | "warning" | "display" | "log" | "verbose";
export type ModuleClass = "game" | "windows" | "gpu_driver" | "third_party" | "unknown";
export type ModState = "enabled" | "disabled" | "missing";
export type ModsSource = "log" | "enabled" | "unavailable";
export type KeySource = "bundled" | "manual";

export interface GameSession {
  name: string;
  log_path: string | null;
  crash_dir: string | null;
  pid: number | null;
  /** Unix seconds. */
  started: number | null;
  ended: number | null;
  size: number;
  build: string | null;
  outcome: Outcome;
  crash_type: string | null;
  crash_message: string | null;
  crash_signature: string | null;
  repeat_count: number;
  error: string | null;
}

export interface Frame {
  module: string;
  offset: number | null;
  symbol: string | null;
  class: ModuleClass;
}

export interface LoadedModule {
  name: string;
  path: string;
  class: ModuleClass;
}

export interface CrashReport {
  dir: string;
  folder: string;
  crash_type: string;
  error_message: string;
  process_id: number | null;
  seconds_since_start: number | null;
  crashed_at: number | null;
  engine_version: string | null;
  build_version: string | null;
  command_line: string | null;
  cpu: string | null;
  gpu: string | null;
  gpu_driver: string | null;
  rhi: string | null;
  os: string | null;
  memory_gb: number | null;
  out_of_memory: boolean;
  crashed_thread: string | null;
  frames: Frame[];
  faulting_frame: number | null;
  culprit_frame: number | null;
  modules: LoadedModule[];
  signature: string;
  log_file: string | null;
}

export interface ErrorGroup {
  verbosity: Verbosity;
  category: string;
  message: string;
  count: number;
  /** A 0-based line index. */
  first_line: number;
  packages: string[];
  /** The game logs this too in recent sessions with no mods mounted. */
  without_mods: boolean;
}

export interface PyFrame {
  file: string;
  line: number;
  function: string;
  third_party: boolean;
}

/** What brought the game down, when the crash says so outright. */
export type Culprit =
  | { kind: "mods"; mods: string[]; packages: string[] }
  | { kind: "third_party"; module: string };

export interface ModHit {
  mod_name: string;
  state: ModState;
  packages: string[];
  /** Indices into `LogAnalysis.groups`. */
  groups: number[];
  errors: number;
  warnings: number;
}

export interface LogAnalysis {
  outcome: Outcome;
  build: string | null;
  engine_version: string | null;
  command_line: string | null;
  fatal: { line: number; text: string } | null;
  fatal_count: number;
  error_count: number;
  warning_count: number;
  groups: ErrorGroup[];
  groups_total: number;
  baseline_sessions: number;
  game_thread_python: PyFrame[];
  mods: { name: string; state: ModState }[];
  mods_source: ModsSource;
  mod_hits: ModHit[];
  fatal_mods: { mod_name: string; state: ModState; packages: string[] }[];
  notes: string[];
}

export interface LogOverview {
  snapshot: number;
  name: string;
  log_path: string | null;
  crash_dir: string | null;
  line_count: number;
  verbosity_counts: { verbosity: Verbosity; count: number }[];
  categories: { name: string; count: number }[];
  analysis: LogAnalysis;
  crash: CrashReport | null;
  culprit: Culprit | null;
  key_source: KeySource | null;
  key_error: string | null;
}

export interface LogLine {
  /** The line's 0-based index in the whole log. */
  n: number;
  verbosity: Verbosity;
  category: string;
  text: string;
  continuation: boolean;
  trailer: boolean;
  repeat: boolean;
}

export interface KeyCheck {
  source: KeySource;
  session: string | null;
  opens: boolean | null;
  error: string | null;
}

export const OUTCOME_LABEL: Record<Outcome, string> = {
  running: "Running",
  clean_exit: "Clean exit",
  crashed: "Crashed",
  fatal_error: "Fatal error",
  ended_abruptly: "Ended abruptly",
  empty: "Empty",
  unreadable: "Unreadable",
};

export const OUTCOME_HINT: Record<Outcome, string> = {
  running: "The game is still writing this log",
  clean_exit: "The game closed normally",
  crashed: "The game crashed and the crash reporter kept a report",
  fatal_error: "The log ends in a fatal error, with no crash report left for it",
  ended_abruptly:
    "The log stops with no close and no error: the game was killed or the PC went down",
  empty: "The game wrote nothing to this log",
  unreadable: "The log key does not open this log",
};

export function outcomeClass(outcome: Outcome): string {
  switch (outcome) {
    case "crashed":
    case "fatal_error":
      return "bg-err/15 text-err";
    case "ended_abruptly":
      return "bg-warn/15 text-warn";
    case "running":
      return "bg-sky-500/15 text-sky-400";
    case "clean_exit":
      return "bg-ok/15 text-ok";
    default:
      return "bg-muted text-muted-foreground";
  }
}

export const VERBOSITIES: Verbosity[] = ["fatal", "error", "warning", "display", "log", "verbose"];

export const VERBOSITY_LABEL: Record<Verbosity, string> = {
  fatal: "Fatal",
  error: "Error",
  warning: "Warning",
  display: "Display",
  log: "Log",
  verbose: "Verbose",
};

export function verbosityTextClass(verbosity: Verbosity): string {
  switch (verbosity) {
    case "fatal":
    case "error":
      return "text-err";
    case "warning":
      return "text-warn";
    case "verbose":
      return "text-muted-foreground";
    default:
      return "text-foreground/90";
  }
}

export const MODULE_CLASS_LABEL: Record<ModuleClass, string> = {
  game: "Game",
  windows: "Windows",
  gpu_driver: "GPU driver",
  third_party: "Third-party",
  unknown: "Unknown",
};

export function moduleClassClass(moduleClass: ModuleClass): string {
  switch (moduleClass) {
    case "third_party":
      return "bg-warn/15 text-warn";
    case "game":
      return "bg-sky-500/15 text-sky-400";
    default:
      return "bg-muted text-muted-foreground";
  }
}

export function frameLabel(frame: Frame): string {
  if (frame.symbol) return `${frame.module}!${frame.symbol}`;
  if (frame.offset !== null) return `${frame.module}+0x${frame.offset.toString(16)}`;
  return frame.module;
}

const FATAL_SOURCE = /^Fatal error: \[File:([^\]]*)\] \[Line: (\d+)\]\s*/;

/** Splits the engine's `Fatal error: [File:...] [Line: N]` prefix off a crash message. */
export function splitFatalSource(message: string): {
  source: { path: string; label: string } | null;
  message: string;
} {
  const m = FATAL_SOURCE.exec(message);
  if (!m) return { source: null, message };
  const file = m[1].split(/[\\/]/).pop() || m[1];
  return { source: { path: m[1], label: `${file}:${m[2]}` }, message: message.slice(m[0].length) };
}

/** Whether a command failed because the log it named was replaced by another one opening. */
export function isStaleSnapshot(error: unknown): boolean {
  return String(error).includes("stale log snapshot");
}

export function formatDuration(seconds: number): string {
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ${seconds % 60}s`;
  return `${Math.floor(minutes / 60)}h ${minutes % 60}m`;
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const kb = bytes / 1024;
  if (kb < 1024) return `${kb.toFixed(kb < 10 ? 1 : 0)} KB`;
  const mb = kb / 1024;
  return `${mb.toFixed(mb < 10 ? 1 : 0)} MB`;
}

export function formatWhen(seconds: number): string {
  return new Date(seconds * 1000).toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });
}
