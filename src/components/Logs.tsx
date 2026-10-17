import { useCallback, useEffect, useRef, useState } from "react";

import { invoke } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";
import { openPath, revealItemInDir } from "@tauri-apps/plugin-opener";
import {
  CheckCircle2,
  Copy,
  Download,
  FileText,
  FolderOpen,
  LayoutList,
  Loader2,
  RefreshCw,
  XCircle,
} from "lucide-react";

import { LogSummary } from "./LogSummary";
import { LogViewer } from "./LogViewer";

import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { Tip } from "@/components/ui/tooltip";
import {
  formatBytes,
  formatDuration,
  formatWhen,
  OUTCOME_HINT,
  OUTCOME_LABEL,
  outcomeClass,
  splitFatalSource,
  type GameSession,
  type LogOverview,
} from "@/lib/gameLogs";
import { cn } from "@/lib/utils";

type SubTab = "summary" | "log";
type StatusType = "ok" | "err" | "info";

interface Props {
  gamePath: string;
  isActive: boolean;
  onOpenSettings: () => void;
}

export function Logs({ gamePath, isActive, onOpenSettings }: Props) {
  const [sessions, setSessions] = useState<GameSession[] | null>(null);
  const [listing, setListing] = useState(false);
  const [crashesOnly, setCrashesOnly] = useState(false);
  const [selected, setSelected] = useState<string | null>(null);
  const [overview, setOverview] = useState<LogOverview | null>(null);
  const [opening, setOpening] = useState(false);
  const [openError, setOpenError] = useState<string | null>(null);
  const [subTab, setSubTab] = useState<SubTab>("summary");
  const [jumpTo, setJumpTo] = useState<{ line: number; nonce: number } | null>(null);
  const [notice, setNotice] = useState<{ msg: string; type: StatusType } | null>(null);
  const noticeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const openSeq = useRef(0);
  const selectedRef = useRef<string | null>(null);
  const overviewRef = useRef<LogOverview | null>(null);

  const sessionsRef = useRef<GameSession[] | null>(null);
  const openSessionRef = useRef<(session: GameSession) => Promise<void>>(async () => {});

  useEffect(() => {
    selectedRef.current = selected;
    overviewRef.current = overview;
    sessionsRef.current = sessions;
  }, [selected, overview, sessions]);

  const showNotice = useCallback((msg: string, type: StatusType, duration = 6000) => {
    if (noticeTimer.current) clearTimeout(noticeTimer.current);
    setNotice({ msg, type });
    noticeTimer.current = setTimeout(() => setNotice(null), duration);
  }, []);

  const fetchSessions = useCallback(
    (): Promise<GameSession[] | null> =>
      invoke<GameSession[]>("list_game_sessions").then(
        (list) => {
          setSessions(list);
          return list;
        },
        (e: unknown) => {
          showNotice(String(e), "err");
          return null;
        }
      ),
    [showNotice]
  );

  const refresh = () => {
    setListing(true);
    void fetchSessions().finally(() => setListing(false));
  };

  const openSession = useCallback(
    async (session: GameSession) => {
      const seq = ++openSeq.current;
      setSelected(session.name);
      setOpening(true);
      setOpenError(null);
      try {
        const opened = await invoke<LogOverview>("open_game_log", {
          name: session.name,
          logPath: session.log_path,
          crashDir: session.crash_dir,
          gameRoot: gamePath,
        });
        if (seq !== openSeq.current) return;
        setJumpTo(null);
        setOverview(opened);
      } catch (e) {
        if (seq !== openSeq.current) return;
        setOverview(null);
        setOpenError(String(e));
      } finally {
        if (seq === openSeq.current) setOpening(false);
      }
    },
    [gamePath]
  );

  // Coming back to the tab picks up new sessions, and a log a wrong key left unread is tried
  // again in case the key was just set.
  useEffect(() => {
    if (!isActive) return;
    let cancelled = false;
    void fetchSessions().then((list) => {
      if (cancelled || !list) return;
      const current = selectedRef.current;
      if (current === null) {
        if (list[0]) void openSession(list[0]);
      } else if (overviewRef.current?.key_error) {
        const again = list.find((s) => s.name === current);
        if (again) void openSession(again);
      }
    });
    return () => {
      cancelled = true;
    };
  }, [isActive, fetchSessions, openSession]);

  const session = sessions?.find((s) => s.name === selected) ?? null;
  const shown = (sessions ?? []).filter((s) => !crashesOnly || s.crash_dir);

  useEffect(() => {
    openSessionRef.current = openSession;
  }, [openSession]);

  // Stable, so the viewer's filter does not run again each time the session list refreshes.
  const reopen = useCallback(() => {
    const current = sessionsRef.current?.find((s) => s.name === selectedRef.current);
    if (current) void openSessionRef.current(current);
  }, []);

  const jump = useCallback((line: number) => {
    setSubTab("log");
    setJumpTo((prev) => ({ line, nonce: (prev?.nonce ?? 0) + 1 }));
  }, []);

  const exportLog = async () => {
    if (!overview) return;
    const dest = await save({
      defaultPath: `${overview.name}.txt`,
      filters: [{ name: "Text", extensions: ["txt", "log"] }],
    });
    if (!dest) return;
    try {
      await invoke("export_game_log", { snapshot: overview.snapshot, dest });
      showNotice(`Saved ${dest}`, "ok");
    } catch (e) {
      showNotice(String(e), "err");
    }
  };

  const copyLog = async () => {
    if (!overview) return;
    try {
      const text = await invoke<string>("game_log_text", { snapshot: overview.snapshot });
      await navigator.clipboard.writeText(text);
      showNotice(`Copied the whole log, ${overview.line_count.toLocaleString()} lines`, "ok");
    } catch (e) {
      showNotice(String(e), "err");
    }
  };

  const openLogsFolder = async () => {
    try {
      const dir = await invoke<string | null>("get_game_logs_dir");
      if (dir) await openPath(dir);
    } catch (e) {
      showNotice(String(e), "err");
    }
  };

  return (
    <div className="flex flex-1 min-h-0 w-full flex-col gap-3">
      <div className="flex min-h-8 items-center gap-3">
        <h2 className="shrink-0 text-xl font-bold">Logs</h2>
        {notice && (
          <span
            className={cn(
              "flex min-w-0 items-center gap-1.5 truncate text-[12px] font-medium",
              notice.type === "ok"
                ? "text-ok"
                : notice.type === "err"
                  ? "text-err"
                  : "text-muted-foreground"
            )}
          >
            {notice.type === "ok" ? (
              <CheckCircle2 className="shrink-0" size={14} strokeWidth={2.5} />
            ) : notice.type === "err" ? (
              <XCircle className="shrink-0" size={14} strokeWidth={2.5} />
            ) : null}
            <span className="truncate">{notice.msg}</span>
          </span>
        )}
        <span className="flex-1" />
        <Tip content="Open the game's Logs folder">
          <Button variant="ghost" size="icon-sm" onClick={openLogsFolder}>
            <FolderOpen size={15} />
          </Button>
        </Tip>
        <Tip content="Look for new sessions">
          <Button variant="ghost" size="icon-sm" onClick={refresh} disabled={listing}>
            <RefreshCw size={15} className={cn(listing && "animate-spin")} />
          </Button>
        </Tip>
      </div>

      <div className="flex min-h-0 flex-1 gap-3">
        <section className="flex w-[280px] shrink-0 flex-col overflow-hidden rounded-md border border-border">
          <div className="flex h-10 shrink-0 items-center gap-2 border-b border-border bg-card px-3">
            <h3 className="text-sm font-semibold">Sessions</h3>
            <span className="text-[12px] text-muted-foreground">{shown.length}</span>
            <span className="flex-1" />
            <label className="flex items-center gap-1.5 text-[11px] text-muted-foreground">
              Crashes only
              <Switch checked={crashesOnly} onCheckedChange={setCrashesOnly} />
            </label>
          </div>
          <ul className="min-h-0 flex-1 overflow-y-auto bg-background">
            {sessions === null ? (
              <li className="px-3 py-3 text-[12px] text-muted-foreground">Reading logs…</li>
            ) : shown.length === 0 ? (
              <li className="px-3 py-3 text-[12px] text-muted-foreground">
                {crashesOnly ? "No crash reports." : "No game logs yet."}
              </li>
            ) : (
              shown.map((s) => (
                <li key={s.name}>
                  <button
                    onClick={() => void openSession(s)}
                    className={cn(
                      "flex w-full flex-col gap-0.5 border-b border-border/50 px-3 py-2 text-left hover:bg-secondary/40",
                      selected === s.name && "bg-secondary hover:bg-secondary"
                    )}
                  >
                    <span className="flex items-center gap-2">
                      <Tip content={s.error ?? OUTCOME_HINT[s.outcome]}>
                        <span
                          className={cn(
                            "shrink-0 rounded-sm px-1.5 py-px text-[10px] font-medium",
                            outcomeClass(s.outcome)
                          )}
                        >
                          {OUTCOME_LABEL[s.outcome]}
                        </span>
                      </Tip>
                      <span className="min-w-0 flex-1 truncate text-[12px] font-medium">
                        {s.started !== null ? formatWhen(s.started) : s.name}
                      </span>
                      {s.repeat_count > 1 && (
                        <Tip content={`The same crash is in ${s.repeat_count} reports`}>
                          <span className="shrink-0 text-[10px] text-err">×{s.repeat_count}</span>
                        </Tip>
                      )}
                    </span>
                    <span className="flex gap-2 text-[10px] text-muted-foreground">
                      {s.started !== null && s.ended !== null && s.ended >= s.started && (
                        <span>{formatDuration(s.ended - s.started)}</span>
                      )}
                      <span>{formatBytes(s.size)}</span>
                      {s.build && <span className="truncate">{s.build.split(".").pop()}</span>}
                    </span>
                    {s.crash_message && (
                      <span className="truncate font-mono text-[10px] text-err/80">
                        {splitFatalSource(s.crash_message).message}
                      </span>
                    )}
                  </button>
                </li>
              ))
            )}
          </ul>
        </section>

        <section className="flex min-w-0 flex-1 flex-col overflow-hidden rounded-md border border-border">
          <div className="flex h-10 shrink-0 items-center gap-2 border-b border-border bg-card px-2">
            <div className="flex gap-1 rounded-md bg-muted p-0.5">
              {(
                [
                  { id: "summary", label: "Summary", Icon: LayoutList },
                  { id: "log", label: "Log", Icon: FileText },
                ] as const
              ).map(({ id, label, Icon }) => (
                <button
                  key={id}
                  onClick={() => setSubTab(id)}
                  className={cn(
                    "flex items-center gap-1.5 rounded-sm px-2.5 py-1 text-[12px] font-medium transition-colors",
                    subTab === id
                      ? "bg-background text-foreground shadow-sm"
                      : "text-muted-foreground hover:text-foreground"
                  )}
                >
                  <Icon size={13} />
                  {label}
                </button>
              ))}
            </div>
            <span className="min-w-0 flex-1 truncate font-mono text-[11px] text-muted-foreground">
              {overview?.name ?? selected ?? ""}
            </span>
            {opening && (
              <Loader2 size={14} className="shrink-0 animate-spin text-muted-foreground" />
            )}
            <Tip content="Copy the whole decoded log to the clipboard">
              <Button
                size="xs"
                variant="outline"
                onClick={copyLog}
                disabled={!overview || overview.line_count === 0}
              >
                <Copy size={12} />
                Copy log
              </Button>
            </Tip>
            <Tip content="Save the decoded log as text">
              <Button
                size="xs"
                variant="outline"
                onClick={exportLog}
                disabled={!overview || overview.line_count === 0}
              >
                <Download size={12} />
                Export
              </Button>
            </Tip>
            <Tip content={overview?.crash_dir ? "Open the crash folder" : "Show the log file"}>
              <Button
                variant="ghost"
                size="icon-sm"
                disabled={!overview?.log_path && !overview?.crash_dir}
                onClick={() => {
                  const target = overview?.crash_dir
                    ? openPath(overview.crash_dir)
                    : overview?.log_path
                      ? revealItemInDir(overview.log_path)
                      : Promise.resolve();
                  target.catch((e: unknown) => showNotice(String(e), "err"));
                }}
              >
                <FolderOpen size={15} />
              </Button>
            </Tip>
          </div>

          {openError ? (
            <p className="p-6 text-sm text-err">{openError}</p>
          ) : !overview ? (
            <p className="flex items-center gap-2 p-6 text-sm text-muted-foreground">
              {opening || sessions === null ? (
                <>
                  <Loader2 size={14} className="animate-spin" /> Decoding the log
                </>
              ) : (
                "Pick a session."
              )}
            </p>
          ) : (
            <>
              <div className={cn("flex min-h-0 flex-1 flex-col", subTab !== "summary" && "hidden")}>
                <LogSummary
                  overview={overview}
                  session={session}
                  onJump={jump}
                  onOpenSettings={onOpenSettings}
                />
              </div>
              <div className={cn("flex min-h-0 flex-1 flex-col", subTab !== "log" && "hidden")}>
                <LogViewer
                  key={overview.snapshot}
                  overview={overview}
                  jumpTo={jumpTo}
                  onStale={reopen}
                />
              </div>
            </>
          )}
        </section>
      </div>
    </div>
  );
}
