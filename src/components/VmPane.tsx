import { useEffect, useMemo, useRef, useState } from "react";

import { invoke } from "@tauri-apps/api/core";
import { AlertTriangle, ChevronDown, ChevronRight, Copy, Crosshair } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Tip } from "@/components/ui/tooltip";
import { constantText, editCommand, hex4, type VmLeaf, type VmView } from "@/lib/niagaraVm";
import { cn } from "@/lib/utils";

function Leaf({
  leaf,
  entry,
  script,
  onShow,
}: {
  leaf: VmLeaf;
  entry: string;
  script: string;
  onShow: (at: number) => void;
}) {
  if (leaf.kind === "unwritten") {
    return <li className="text-amber-400">r{leaf.register} is read before anything writes it</li>;
  }
  const show = (
    <Tip content="Show the instruction in the listing">
      <button
        type="button"
        aria-label={`Show ${hex4(leaf.at)}`}
        className="text-muted-foreground hover:text-foreground"
        onClick={() => onShow(leaf.at)}
      >
        <Crosshair size={11} />
      </button>
    </Tip>
  );
  if (leaf.kind !== "constant") {
    const what =
      leaf.kind === "input"
        ? `input ${leaf.name}`
        : leaf.kind === "call"
          ? `call ${leaf.function}`
          : leaf.what;
    return (
      <li className="flex items-center gap-2">
        {show}
        <span className="font-mono text-muted-foreground">{hex4(leaf.at)}</span>
        <span>{what}</span>
      </li>
    );
  }
  return (
    <li className="flex items-center gap-2">
      {show}
      <span className="font-mono text-muted-foreground">
        {hex4(leaf.at)} #{leaf.operand}
      </span>
      <span className="font-mono">{constantText(leaf)}</span>
      <span className="text-muted-foreground">
        {leaf.constant.key}
        {leaf.uses > 1 ? `, ${leaf.uses} uses` : ""}
      </span>
      <Tip content={`Feeds ${leaf.feeds.join(", ")}`}>
        <span className="truncate text-muted-foreground">
          feeds {leaf.feeds.length === 1 ? leaf.feeds[0] : `${leaf.feeds.length} outputs`}
        </span>
      </Tip>
      <Tip content="Copy the command that gives this constant a new value">
        <button
          type="button"
          aria-label={`Copy edit-vm for ${hex4(leaf.at)} #${leaf.operand}`}
          className="ml-auto text-muted-foreground hover:text-foreground"
          onClick={() => void navigator.clipboard.writeText(editCommand(entry, script, leaf))}
        >
          <Copy size={11} />
        </button>
      </Tip>
    </li>
  );
}

/**
 * A Niagara script's VM program, read-only: its listing with every constant and output named,
 * and what each output is computed from. Constants are changed from the CLI or an edit file, and
 * each leaf copies the command that does it.
 */
export function VmPane({
  gamePath,
  container,
  entry,
  exportIndex,
}: {
  gamePath: string;
  container: string;
  entry: string;
  exportIndex: number;
}) {
  const [view, setView] = useState<VmView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(true);
  const [filter, setFilter] = useState("");
  const [open, setOpen] = useState<Set<number>>(new Set());
  const [focus, setFocus] = useState<number | null>(null);
  const lines = useRef(new Map<number, HTMLDivElement>());

  // The pane is keyed by export, so it mounts fresh for each one.
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const result = await invoke<VmView>("export_vm_view", {
          gameRoot: gamePath,
          container,
          entry,
          export: exportIndex,
        });
        if (!cancelled) setView(result);
      } catch (e) {
        if (!cancelled) setError(String(e));
      } finally {
        if (!cancelled) setBusy(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [gamePath, container, entry, exportIndex]);

  useEffect(() => {
    if (focus !== null) lines.current.get(focus)?.scrollIntoView?.({ block: "center" });
  }, [focus]);

  const traces = useMemo(
    () =>
      (view?.traces ?? []).filter((trace) =>
        trace.writes.toLowerCase().includes(filter.trim().toLowerCase())
      ),
    [view, filter]
  );

  if (busy) {
    return <p className="p-6 text-center text-sm text-muted-foreground">Disassembling…</p>;
  }
  if (error || !view) {
    return <p className="p-6 text-center text-sm text-red-400">{error ?? "No program"}</p>;
  }
  if (view.bytecode_len === 0) {
    return (
      <p className="p-6 text-center text-sm text-muted-foreground">
        This {view.usage} script carries no VM bytecode: it runs on the GPU, or nothing runs it.
      </p>
    );
  }
  const base = view.layout?.tables.find(([table]) => table === "literals")?.[1];
  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden">
      <div className="flex flex-wrap items-center gap-3 border-b border-border/60 px-3 py-1.5 text-[11px] text-muted-foreground">
        <span>
          {view.usage}
          {view.interpolated ? ", interpolated spawn" : ""}
        </span>
        <span>
          {view.program.instructions.length} instruction
          {view.program.instructions.length === 1 ? "" : "s"}
        </span>
        <span>{view.bytecode_len} bytes</span>
        <span>
          literals {view.literals.length} bytes{base !== undefined ? ` at ${base}` : ""}
        </span>
        {view.program.stop && (
          <span className="flex items-center gap-1 text-amber-400">
            <AlertTriangle size={10} /> stops at {hex4(view.program.stop.at)}:{" "}
            {view.program.stop.reason}
          </span>
        )}
        {view.edit_refusal && (
          <span className="flex items-center gap-1 text-amber-400">
            <AlertTriangle size={10} /> constants not editable: {view.edit_refusal}
          </span>
        )}
      </div>
      <div className="grid min-h-0 flex-1 grid-cols-1 lg:grid-cols-2">
        <div className="min-h-0 overflow-auto border-r border-border/60 py-1 font-mono text-[11px]">
          {view.lines.map((line) => (
            <div
              key={line.at}
              ref={(node) => {
                if (node) lines.current.set(line.at, node);
                else lines.current.delete(line.at);
              }}
              aria-current={focus === line.at ? "true" : undefined}
              className={cn(
                "flex gap-3 whitespace-pre px-3",
                focus === line.at && "bg-blue-accent/20",
                line.writes && "text-blue-accent-foreground"
              )}
              title={line.constants.map(([k, c]) => `#${k} ${c.name ?? c.key}`).join("\n")}
            >
              <span className="text-muted-foreground">{hex4(line.at)}</span>
              <span>{line.text}</span>
            </div>
          ))}
        </div>
        <div className="flex min-h-0 flex-col">
          <div className="flex items-center gap-2 border-b border-border/60 px-3 py-1.5">
            <span className="text-[11px] font-medium">Outputs</span>
            <Input
              aria-label="Filter outputs"
              value={filter}
              onChange={(event) => setFilter(event.target.value)}
              placeholder="Particles.Color"
              className="h-6 text-[11px]"
            />
          </div>
          <ul className="min-h-0 flex-1 overflow-auto px-3 py-1 text-[11px]">
            {traces.map((trace) => {
              const expanded = open.has(trace.at);
              return (
                <li key={trace.at} className="border-b border-border/40 py-1">
                  <Button
                    variant="ghost"
                    size="sm"
                    className="h-auto w-full justify-start gap-2 px-1 py-0.5 text-left text-[11px]"
                    aria-expanded={expanded}
                    onClick={() =>
                      setOpen((was) => {
                        const next = new Set(was);
                        if (next.has(trace.at)) next.delete(trace.at);
                        else next.add(trace.at);
                        return next;
                      })
                    }
                  >
                    {expanded ? <ChevronDown size={11} /> : <ChevronRight size={11} />}
                    <span className="font-medium">{trace.writes}</span>
                    <span className="text-muted-foreground">
                      {trace.baked ? "baked" : "computed"}
                    </span>
                  </Button>
                  <p
                    className="truncate pl-6 font-mono text-muted-foreground"
                    title={trace.expression}
                  >
                    = {trace.expression}
                  </p>
                  {expanded && (
                    <ul className="space-y-0.5 pl-6 pt-1">
                      {trace.leaves.map((leaf, i) => (
                        <Leaf
                          key={i}
                          leaf={leaf}
                          entry={entry}
                          script={view.object_name}
                          onShow={setFocus}
                        />
                      ))}
                    </ul>
                  )}
                </li>
              );
            })}
          </ul>
        </div>
      </div>
    </div>
  );
}
