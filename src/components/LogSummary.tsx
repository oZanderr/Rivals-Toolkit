import { useMemo, useState, type ReactNode } from "react";

import { AlertTriangle, ChevronRight, Settings as SettingsIcon } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Tip } from "@/components/ui/tooltip";
import {
  formatDuration,
  frameLabel,
  MODULE_CLASS_LABEL,
  moduleClassClass,
  OUTCOME_HINT,
  OUTCOME_LABEL,
  outcomeClass,
  splitFatalSource,
  VERBOSITY_LABEL,
  verbosityTextClass,
  type CrashReport,
  type Culprit,
  type ErrorGroup,
  type Frame,
  type GameSession,
  type LogAnalysis,
  type LogOverview,
  type ModHit,
  type ModState,
  type PyFrame,
} from "@/lib/gameLogs";
import { cn } from "@/lib/utils";

const SHOWN_FRAMES = 6;
const SHOWN_PACKAGES = 8;
const SHOWN_GROUPS = 200;
const SHOWN_PYTHON_FRAMES = 8;
const SHOWN_MOD_NAMES = 30;

const MOD_STATE: Record<ModState, { label: string; hint: string } | null> = {
  enabled: null,
  disabled: { label: "disabled now", hint: "You switched this mod off after this session" },
  missing: { label: "uninstalled", hint: "This mod is no longer in your mods folder" },
};

interface Props {
  overview: LogOverview;
  session: GameSession | null;
  onJump: (line: number) => void;
  onOpenSettings: () => void;
}

function plural(n: number, word: string): string {
  return `${n.toLocaleString()} ${word}${n === 1 ? "" : "s"}`;
}

function Card({
  title,
  count,
  aside,
  toolbar,
  className,
  children,
}: {
  title: string;
  count?: number;
  aside?: ReactNode;
  toolbar?: ReactNode;
  className?: string;
  children: ReactNode;
}) {
  return (
    <section className={cn("overflow-hidden rounded-md border border-border", className)}>
      <div className="flex min-h-9 items-center gap-2 border-b border-border bg-card px-3 py-1.5">
        <h3 className="text-sm font-semibold">{title}</h3>
        {count !== undefined && (
          <span className="rounded-sm bg-muted px-1.5 text-[11px] tabular-nums text-muted-foreground">
            {count.toLocaleString()}
          </span>
        )}
        {aside}
      </div>
      {toolbar && (
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1.5 border-b border-border/60 px-3 py-1.5">
          {toolbar}
        </div>
      )}
      <div className="bg-background">{children}</div>
    </section>
  );
}

function Fact({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex min-w-0 flex-col gap-0.5">
      <span className="text-[10px] uppercase tracking-wide text-muted-foreground">{label}</span>
      <span className="text-[12px] [overflow-wrap:anywhere]">{children}</span>
    </div>
  );
}

function SubLabel({ children }: { children: ReactNode }) {
  return (
    <span className="text-[10px] uppercase tracking-wide text-muted-foreground">{children}</span>
  );
}

function ModuleBadge({ frame }: { frame: Frame }) {
  return (
    <span className="flex items-center gap-1.5">
      {frame.module}
      <span className={cn("rounded-sm px-1 text-[10px]", moduleClassClass(frame.class))}>
        {MODULE_CLASS_LABEL[frame.class]}
      </span>
    </span>
  );
}

function PythonStack({ frames }: { frames: PyFrame[] }) {
  return (
    <div className="flex flex-col gap-1">
      <SubLabel>Python on the game thread when it died</SubLabel>
      <div className="flex flex-col rounded-sm border border-border/60 py-1">
        {frames.slice(0, SHOWN_PYTHON_FRAMES).map((f, i) => (
          <div
            key={i}
            className={cn(
              "truncate px-2 font-mono text-[11px] leading-[18px]",
              f.third_party && "text-warn"
            )}
          >
            {f.file}:{f.line} in {f.function}
          </div>
        ))}
      </div>
    </div>
  );
}

function GroupRow({ group, onJump }: { group: ErrorGroup; onJump: (line: number) => void }) {
  return (
    <button
      onClick={() => onJump(group.first_line)}
      className="flex w-full items-center gap-2 px-3 py-1 text-left hover:bg-secondary/40"
    >
      <span className="w-12 shrink-0 text-right font-mono text-[11px] tabular-nums text-muted-foreground">
        ×{group.count.toLocaleString()}
      </span>
      <span
        className={cn("w-14 shrink-0 text-[11px] font-medium", verbosityTextClass(group.verbosity))}
      >
        {VERBOSITY_LABEL[group.verbosity]}
      </span>
      <Tip content={group.message} side="top" align="start">
        <span className="min-w-0 flex-1 truncate font-mono text-[11px]">
          {group.category && <span className="text-muted-foreground">{group.category}: </span>}
          {group.message}
        </span>
      </Tip>
      <ChevronRight size={13} className="shrink-0 text-muted-foreground/60" />
    </button>
  );
}

function ModRow({
  name,
  state,
  named,
  hit,
  analysis,
  onJump,
}: {
  name: string;
  state: ModState;
  /** The packages the crash names that this mod ships. */
  named: string[];
  hit: ModHit | undefined;
  analysis: LogAnalysis;
  onJump: (line: number) => void;
}) {
  const packages = [...named, ...(hit?.packages.filter((p) => !named.includes(p)) ?? [])];
  const groups = (hit?.groups ?? []).map((g) => analysis.groups[g]).filter(Boolean);
  const status = MOD_STATE[state];
  return (
    <li className="border-b border-border/50 py-2 last:border-0">
      <div className="flex items-baseline gap-2 px-3">
        <span
          className={cn(
            "min-w-0 text-[13px] font-medium [overflow-wrap:anywhere]",
            named.length > 0 && "text-err"
          )}
        >
          {name}
        </span>
        {status && (
          <Tip content={status.hint}>
            <span className="shrink-0 rounded-sm bg-muted px-1.5 text-[10px] text-muted-foreground">
              {status.label}
            </span>
          </Tip>
        )}
        <span className="flex-1" />
        {hit && (
          <span className="shrink-0 text-[11px] tabular-nums">
            {hit.errors > 0 && <span className="text-err">{plural(hit.errors, "error")}</span>}
            {hit.errors > 0 && hit.warnings > 0 && (
              <span className="text-muted-foreground"> · </span>
            )}
            {hit.warnings > 0 && (
              <span className="text-warn">{plural(hit.warnings, "warning")}</span>
            )}
          </span>
        )}
      </div>
      {packages.length > 0 && (
        <div className="flex flex-wrap gap-1 px-3 pt-1">
          {packages.slice(0, SHOWN_PACKAGES).map((p) => (
            <Tip key={p} content={named.includes(p) ? "The crash names this package" : null}>
              <span
                className={cn(
                  "rounded-sm px-1.5 font-mono text-[10px] [overflow-wrap:anywhere]",
                  named.includes(p) ? "bg-err/10 text-err" : "bg-muted text-muted-foreground"
                )}
              >
                {p}
              </span>
            </Tip>
          ))}
          {packages.length > SHOWN_PACKAGES && (
            <span className="text-[10px] text-muted-foreground">
              +{packages.length - SHOWN_PACKAGES} more
            </span>
          )}
        </div>
      )}
      {groups.length > 0 && (
        <div className="pt-1">
          {groups.map((group, i) => (
            <GroupRow key={i} group={group} onJump={onJump} />
          ))}
        </div>
      )}
    </li>
  );
}

/** The cause when the crash names one, with every mod the session's errors name. */
function ModsCard({
  culprit,
  analysis,
  onJump,
}: {
  culprit: Culprit | null;
  analysis: LogAnalysis;
  onJump: (line: number) => void;
}) {
  const culpritMods = culprit?.kind === "mods" ? culprit.mods : [];
  const others = analysis.mod_hits.filter((h) => !culpritMods.includes(h.mod_name));
  if (!culprit && others.length === 0) return null;

  const hitOf = (name: string) => analysis.mod_hits.find((h) => h.mod_name === name);
  const fatalOf = (name: string) => analysis.fatal_mods.find((m) => m.mod_name === name);

  return (
    <Card
      title={culprit ? "Cause" : "Errors naming your mods"}
      className={cn(
        culprit?.kind === "mods" && "border-err/40",
        culprit?.kind === "third_party" && "border-warn/40"
      )}
    >
      {culprit && (
        <p className="px-3 pt-2.5 pb-1 text-[12px] [overflow-wrap:anywhere]">
          {culprit.kind === "mods" ? (
            <>
              The crash names {culprit.packages.length === 1 ? "a package" : "packages"}{" "}
              {culprit.mods.length === 1 ? "this mod ships" : "these mods ship"}.
            </>
          ) : (
            <>
              It crashed inside <span className="font-medium text-warn">{culprit.module}</span>,
              third-party code loaded into the game rather than a mod.
            </>
          )}
        </p>
      )}
      {culpritMods.length > 0 && (
        <ul>
          {culpritMods.map((name) => {
            const fatal = fatalOf(name);
            const hit = hitOf(name);
            return (
              <ModRow
                key={name}
                name={name}
                state={fatal?.state ?? hit?.state ?? "enabled"}
                named={fatal?.packages ?? []}
                hit={hit}
                analysis={analysis}
                onJump={onJump}
              />
            );
          })}
        </ul>
      )}
      {others.length > 0 && (
        <>
          {culprit && (
            <div className="border-t border-border/50 px-3 pt-2">
              <SubLabel>Also named in errors</SubLabel>
            </div>
          )}
          <ul>
            {others.map((hit) => (
              <ModRow
                key={hit.mod_name}
                name={hit.mod_name}
                state={hit.state}
                named={[]}
                hit={hit}
                analysis={analysis}
                onJump={onJump}
              />
            ))}
          </ul>
        </>
      )}
      {analysis.mods_source === "enabled" && (
        <p className="border-t border-border/50 px-3 py-1.5 text-[11px] text-muted-foreground">
          The log does not name its mods, so the ones switched on now stand in.
        </p>
      )}
    </Card>
  );
}

function CrashCard({
  crash,
  session,
  fatalLine,
  python,
  onJump,
}: {
  crash: CrashReport;
  session: GameSession | null;
  fatalLine: number | null;
  python: PyFrame[];
  onJump: (line: number) => void;
}) {
  const [allFrames, setAllFrames] = useState(false);
  const { source, message } = splitFatalSource(crash.error_message);
  const faulting = crash.faulting_frame != null ? crash.frames[crash.faulting_frame] : null;
  const calledFrom =
    crash.culprit_frame != null && crash.culprit_frame !== crash.faulting_frame
      ? crash.frames[crash.culprit_frame]
      : null;
  const thirdParty = crash.modules.filter((m) => m.class === "third_party");
  const topFrames = Math.max(SHOWN_FRAMES, (crash.culprit_frame ?? crash.faulting_frame ?? 0) + 1);
  const frames = allFrames ? crash.frames : crash.frames.slice(0, topFrames);

  return (
    <Card
      title="Crash"
      aside={
        <>
          <span className="rounded-sm bg-err/15 px-1.5 py-0.5 text-[11px] font-medium text-err">
            {crash.crash_type}
          </span>
          {session && session.repeat_count > 1 && (
            <Tip content="Crash reports with the same message and top frames">
              <span className="text-[11px] text-muted-foreground">
                Seen {session.repeat_count} times
              </span>
            </Tip>
          )}
          <span className="flex-1" />
          {fatalLine !== null && (
            <Button size="xs" variant="ghost" onClick={() => onJump(fatalLine)}>
              Show in log
            </Button>
          )}
        </>
      }
    >
      <div className="flex flex-col gap-3 px-3 py-2.5">
        <div className="flex flex-col gap-1">
          {source && (
            <Tip content={source.path} side="top" align="start">
              <span className="self-start font-mono text-[11px] text-muted-foreground">
                {source.label}
              </span>
            </Tip>
          )}
          <p className="font-mono text-[12px] leading-relaxed [overflow-wrap:anywhere]">
            {message}
          </p>
        </div>
        <div className="flex flex-wrap gap-x-8 gap-y-3">
          <Fact label="Faulting module">
            {faulting ? <ModuleBadge frame={faulting} /> : "Unknown"}
          </Fact>
          {calledFrom && (
            <Fact label="Called from">
              <ModuleBadge frame={calledFrom} />
            </Fact>
          )}
          <Fact label="Thread">{crash.crashed_thread ?? "Unknown"}</Fact>
          <Fact label="GPU">
            <Tip content={[crash.gpu, crash.rhi].filter(Boolean).join(", ") || null}>
              <span>
                {crash.gpu_driver ? `Driver ${crash.gpu_driver}` : (crash.gpu ?? "Unknown")}
              </span>
            </Tip>
          </Fact>
          {crash.out_of_memory && <Fact label="Memory">Out of memory</Fact>}
        </div>
        {crash.frames.length > 0 && (
          <div className="flex flex-col gap-1">
            <SubLabel>Stack</SubLabel>
            <div className="flex flex-col rounded-sm border border-border/60 py-1">
              {frames.map((frame, i) => (
                <div
                  key={i}
                  className={cn(
                    "flex items-center gap-2 px-2 font-mono text-[11px] leading-[18px]",
                    i === crash.faulting_frame && "bg-err/5",
                    frame.class === "third_party" && "text-warn",
                    (frame.class === "windows" ||
                      frame.class === "gpu_driver" ||
                      frame.class === "unknown") &&
                      "text-muted-foreground"
                  )}
                >
                  <span className="w-5 shrink-0 text-right text-muted-foreground/50">{i}</span>
                  <span className="min-w-0 truncate">{frameLabel(frame)}</span>
                </div>
              ))}
              {crash.frames.length > topFrames && (
                <button
                  onClick={() => setAllFrames((v) => !v)}
                  className="px-2 pt-0.5 text-left text-[11px] text-muted-foreground hover:text-foreground"
                >
                  {allFrames ? "Show fewer" : `Show all ${crash.frames.length} frames`}
                </button>
              )}
            </div>
          </div>
        )}
        {thirdParty.length > 0 && (
          <div className="flex flex-col gap-1">
            <SubLabel>Third-party modules loaded</SubLabel>
            <div className="flex flex-wrap gap-1.5">
              {thirdParty.map((m) => (
                <Tip key={m.path} content={m.path}>
                  <span className="rounded-sm bg-warn/10 px-1.5 py-0.5 font-mono text-[11px] text-warn">
                    {m.name}
                  </span>
                </Tip>
              ))}
            </div>
          </div>
        )}
        {python.length > 0 && <PythonStack frames={python} />}
      </div>
    </Card>
  );
}

function ModList({ mods }: { mods: LogAnalysis["mods"] }) {
  return (
    <div className="flex max-w-[360px] flex-col gap-0.5">
      {mods.slice(0, SHOWN_MOD_NAMES).map((m) => (
        <span key={m.name} className="font-mono text-[11px] [overflow-wrap:anywhere]">
          {m.name}
          {MOD_STATE[m.state] && (
            <span className="font-sans opacity-70"> ({MOD_STATE[m.state]?.label})</span>
          )}
        </span>
      ))}
      {mods.length > SHOWN_MOD_NAMES && (
        <span className="text-[11px] opacity-70">+{mods.length - SHOWN_MOD_NAMES} more</span>
      )}
    </div>
  );
}

export function LogSummary({ overview, session, onJump, onOpenSettings }: Props) {
  const { analysis, crash } = overview;
  const fatal = analysis.fatal;
  const readable = overview.line_count > 0;
  const [groupFilter, setGroupFilter] = useState<"all" | "errors" | "warnings">("all");
  const [showSeenWithout, setShowSeenWithout] = useState(false);

  const byVerbosity = useMemo(
    () =>
      analysis.groups.filter((g) =>
        groupFilter === "all"
          ? true
          : groupFilter === "errors"
            ? g.verbosity !== "warning"
            : g.verbosity === "warning"
      ),
    [analysis.groups, groupFilter]
  );
  const withMods = byVerbosity.filter((g) => !g.without_mods);
  const seenWithout = byVerbosity.filter((g) => g.without_mods);
  const python = analysis.game_thread_python;

  const duration =
    session?.started != null && session.ended != null && session.ended >= session.started
      ? session.ended - session.started
      : null;

  return (
    <div className="min-h-0 flex-1 overflow-y-auto scrollbar-gutter-stable p-3">
      <div className="flex flex-col gap-3">
        {overview.key_error && (
          <div className="flex items-center gap-2.5 rounded-md border border-warn/20 bg-warn/5 px-3 py-2">
            <AlertTriangle size={15} className="shrink-0 text-warn" />
            <span className="flex-1 text-[12px] text-warn">{overview.key_error}</span>
            <Button size="xs" variant="outline" onClick={onOpenSettings}>
              <SettingsIcon size={12} />
              Settings
            </Button>
          </div>
        )}

        <div className="flex flex-wrap gap-x-8 gap-y-3 rounded-md border border-border bg-card px-3 py-2.5">
          <Fact label="Outcome">
            <Tip content={OUTCOME_HINT[analysis.outcome]}>
              <span
                className={cn(
                  "rounded-sm px-1.5 py-0.5 text-[11px] font-medium",
                  outcomeClass(analysis.outcome)
                )}
              >
                {OUTCOME_LABEL[analysis.outcome]}
              </span>
            </Tip>
          </Fact>
          <Fact label="Build">{analysis.build ?? crash?.build_version ?? "Unknown"}</Fact>
          <Fact label="Ran for">
            {duration !== null
              ? formatDuration(duration)
              : crash?.seconds_since_start
                ? formatDuration(crash.seconds_since_start)
                : "Unknown"}
          </Fact>
          {analysis.mods_source !== "unavailable" && (
            <Fact label="Mods">
              {analysis.mods.length === 0 ? (
                "None"
              ) : (
                <Tip content={<ModList mods={analysis.mods} />} side="bottom" align="start">
                  <span className="cursor-default underline decoration-muted-foreground/50 decoration-dotted underline-offset-2">
                    {analysis.mods_source === "enabled"
                      ? `${analysis.mods.length} switched on`
                      : `${analysis.mods.length} mounted`}
                  </span>
                </Tip>
              )}
            </Fact>
          )}
          {readable && (
            <Fact label="Log">
              {plural(overview.line_count, "line")}
              <span className="text-muted-foreground"> · </span>
              <span className={analysis.error_count > 0 ? "text-err" : "text-muted-foreground"}>
                {plural(analysis.error_count, "error")}
              </span>
              <span className="text-muted-foreground"> · </span>
              <span className={analysis.warning_count > 0 ? "text-warn" : "text-muted-foreground"}>
                {plural(analysis.warning_count, "warning")}
              </span>
            </Fact>
          )}
        </div>

        <ModsCard culprit={overview.culprit} analysis={analysis} onJump={onJump} />

        {crash && (
          <CrashCard
            key={crash.dir}
            crash={crash}
            session={session}
            fatalLine={fatal?.line ?? null}
            python={python}
            onJump={onJump}
          />
        )}

        {fatal && !crash && (
          <Card
            title="Fatal error"
            aside={
              <>
                <span className="flex-1" />
                <Button size="xs" variant="ghost" onClick={() => onJump(fatal.line)}>
                  Show in log
                </Button>
              </>
            }
          >
            <pre className="max-h-64 overflow-auto whitespace-pre-wrap px-3 py-2 font-mono text-[11px] [overflow-wrap:anywhere]">
              {fatal.text}
            </pre>
            {python.length > 0 && (
              <div className="px-3 pb-2.5">
                <PythonStack frames={python} />
              </div>
            )}
          </Card>
        )}

        {readable && (
          <Card
            title="Errors and warnings"
            count={byVerbosity.length}
            toolbar={
              <>
                <div className="flex gap-0.5 rounded-sm bg-muted p-0.5">
                  {(["all", "errors", "warnings"] as const).map((f) => (
                    <button
                      key={f}
                      onClick={() => setGroupFilter(f)}
                      className={cn(
                        "rounded-sm px-2 py-0.5 text-[11px] capitalize",
                        groupFilter === f
                          ? "bg-background text-foreground shadow-sm"
                          : "text-muted-foreground hover:text-foreground"
                      )}
                    >
                      {f}
                    </button>
                  ))}
                </div>
                <span className="text-[11px] text-muted-foreground">
                  Alike lines grouped
                  {analysis.groups_total > analysis.groups.length &&
                    `, the first ${analysis.groups.length} of ${analysis.groups_total}`}
                </span>
              </>
            }
          >
            {withMods.length > 0 ? (
              <div className="py-1">
                {withMods.slice(0, SHOWN_GROUPS).map((group, i) => (
                  <GroupRow key={i} group={group} onJump={onJump} />
                ))}
              </div>
            ) : (
              <p className="px-3 py-2.5 text-[12px] text-muted-foreground">
                {seenWithout.length > 0
                  ? "Every one of these also shows up in sessions without mods, so none points at a mod."
                  : "Nothing here."}
              </p>
            )}
            {seenWithout.length > 0 && (
              <>
                <Tip
                  content={`Logged in the last ${analysis.baseline_sessions} sessions on this build that mounted no mods`}
                  side="top"
                  align="start"
                >
                  <button
                    onClick={() => setShowSeenWithout((v) => !v)}
                    className="flex w-full items-center gap-1.5 border-t border-border/50 px-3 py-1.5 text-left text-[11px] text-muted-foreground hover:text-foreground"
                  >
                    <ChevronRight
                      size={13}
                      className={cn(
                        "shrink-0 transition-transform",
                        showSeenWithout && "rotate-90"
                      )}
                    />
                    {seenWithout.length.toLocaleString()} also logged without mods
                  </button>
                </Tip>
                {showSeenWithout && (
                  <div className="pb-1 opacity-75">
                    {seenWithout.slice(0, SHOWN_GROUPS).map((group, i) => (
                      <GroupRow key={i} group={group} onJump={onJump} />
                    ))}
                  </div>
                )}
              </>
            )}
          </Card>
        )}

        {analysis.notes.length > 0 && (
          <ul className="flex flex-col gap-1 px-1">
            {analysis.notes.map((note, i) => (
              <li key={i} className="text-[11px] text-muted-foreground">
                {note}
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}
