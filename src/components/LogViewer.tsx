import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { useVirtualizer } from "@tanstack/react-virtual";
import { invoke } from "@tauri-apps/api/core";
import { ArrowDown, ArrowUp, Search, X } from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuTrigger,
} from "@/components/ui/context-menu";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Tip } from "@/components/ui/tooltip";
import {
  isStaleSnapshot,
  VERBOSITIES,
  VERBOSITY_LABEL,
  verbosityTextClass,
  type LogLine,
  type LogOverview,
  type Verbosity,
} from "@/lib/gameLogs";
import { cn } from "@/lib/utils";

const ROW_HEIGHT = 18;
const PAGE = 500;
/** Pages kept in memory; a 600k-line log would otherwise end up whole in the webview. */
const MAX_PAGES = 40;
const ALL = "__all__";
const NONE = "__none__";
const SEEK_VERBOSITIES: Verbosity[] = ["fatal", "error"];

interface Props {
  overview: LogOverview;
  /** A line to show, from the summary; the nonce repeats a jump to the same line. */
  jumpTo: { line: number; nonce: number } | null;
  onStale: () => void;
}

export function LogViewer({ overview, jumpTo, onStale }: Props) {
  const snapshot = overview.snapshot;
  const [verbosities, setVerbosities] = useState<Verbosity[]>([]);
  const [category, setCategory] = useState(ALL);
  const [search, setSearch] = useState("");
  const [text, setText] = useState("");
  const [total, setTotal] = useState(overview.line_count);
  const [epoch, setEpoch] = useState(0);
  const [, setLoadedPages] = useState(0);
  const [selected, setSelected] = useState<number | null>(null);
  const [marked, setMarked] = useState<number | null>(null);
  const [scrollTarget, setScrollTarget] = useState<{ row: number; nonce: number } | null>(null);
  const [filterError, setFilterError] = useState<string | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const pages = useRef(new Map<number, LogLine[]>());
  const pending = useRef(new Set<number>());
  const epochRef = useRef(0);
  const pendingJump = useRef<number | null>(null);
  const contextRow = useRef<number | null>(null);
  const nonce = useRef(0);

  useEffect(() => {
    const t = setTimeout(() => setText(search), 250);
    return () => clearTimeout(t);
  }, [search]);

  const counts = useMemo(
    () => new Map(overview.verbosity_counts.map((c) => [c.verbosity, c.count])),
    [overview.verbosity_counts]
  );
  const filtered = verbosities.length > 0 || category !== ALL || text !== "";

  const scrollToRow = useCallback((row: number) => {
    nonce.current += 1;
    setSelected(row);
    setScrollTarget({ row, nonce: nonce.current });
  }, []);

  const showLine = useCallback(
    (line: number) => {
      invoke<number | null>("game_log_row_of", { snapshot, line })
        .then((row) => {
          if (row === null) return;
          setMarked(line);
          scrollToRow(row);
        })
        .catch((e: unknown) => {
          if (isStaleSnapshot(e)) onStale();
        });
    },
    [snapshot, scrollToRow, onStale]
  );

  useEffect(() => {
    let cancelled = false;
    const filter = {
      verbosities,
      categories: category === ALL ? [] : [category === NONE ? "" : category],
      text,
      case_sensitive: false,
    };
    invoke<number>("filter_game_log", { snapshot, filter })
      .then((count) => {
        if (cancelled) return;
        epochRef.current += 1;
        pages.current = new Map();
        pending.current = new Set();
        setTotal(count);
        setEpoch(epochRef.current);
        setFilterError(null);
        setSelected(null);
        if (scrollRef.current) scrollRef.current.scrollTop = 0;
        if (pendingJump.current !== null) {
          const line = pendingJump.current;
          pendingJump.current = null;
          showLine(line);
        }
      })
      .catch((e: unknown) => {
        if (cancelled) return;
        if (isStaleSnapshot(e)) onStale();
        else setFilterError(String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [snapshot, verbosities, category, text, onStale, showLine]);

  // A jump from the summary clears the filters first, so the line it names is there to show.
  useEffect(() => {
    if (!jumpTo) return;
    if (verbosities.length > 0 || category !== ALL || text !== "" || search !== "") {
      pendingJump.current = jumpTo.line;
      setVerbosities([]);
      setCategory(ALL);
      setSearch("");
      setText("");
    } else {
      showLine(jumpTo.line);
    }
    // Only a new jump should run this, not the filter changes it makes.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [jumpTo]);

  // eslint-disable-next-line react-hooks/incompatible-library
  const virtualizer = useVirtualizer({
    count: total,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_HEIGHT,
    overscan: 40,
  });

  useEffect(() => {
    if (scrollTarget && scrollTarget.row < total) {
      virtualizer.scrollToIndex(scrollTarget.row, { align: "center" });
    }
  }, [scrollTarget, total, virtualizer]);

  const items = virtualizer.getVirtualItems();
  const firstIndex = items.length > 0 ? items[0].index : 0;
  const lastIndex = items.length > 0 ? items[items.length - 1].index : -1;

  useEffect(() => {
    if (lastIndex < 0) return;
    const at = epochRef.current;
    const firstPage = Math.floor(firstIndex / PAGE);
    const lastPage = Math.floor(lastIndex / PAGE);
    for (let page = firstPage; page <= lastPage; page += 1) {
      if (pages.current.has(page) || pending.current.has(page)) continue;
      pending.current.add(page);
      invoke<LogLine[]>("game_log_rows", { snapshot, start: page * PAGE, count: PAGE })
        .then((lines) => {
          if (epochRef.current !== at) return;
          pending.current.delete(page);
          pages.current.set(page, lines);
          if (pages.current.size > MAX_PAGES) {
            const farthest = [...pages.current.keys()].sort(
              (a, b) => Math.abs(b - firstPage) - Math.abs(a - firstPage)
            );
            for (const evict of farthest.slice(0, pages.current.size - MAX_PAGES)) {
              pages.current.delete(evict);
            }
          }
          setLoadedPages((n) => n + 1);
        })
        .catch((e: unknown) => {
          pending.current.delete(page);
          if (isStaleSnapshot(e)) onStale();
        });
    }
  }, [firstIndex, lastIndex, epoch, snapshot, onStale]);

  const lineAt = (row: number): LogLine | undefined =>
    pages.current.get(Math.floor(row / PAGE))?.[row % PAGE];

  const seek = async (forward: boolean) => {
    const from = selected ?? (forward ? firstIndex - 1 : firstIndex);
    try {
      const row = await invoke<number | null>("game_log_seek", {
        snapshot,
        from,
        forward,
        verbosities: SEEK_VERBOSITIES,
      });
      if (row !== null) scrollToRow(row);
    } catch (e) {
      if (isStaleSnapshot(e)) onStale();
    }
  };

  const copyRow = (row: number | null) => {
    const line = row === null ? undefined : lineAt(row);
    if (line) void navigator.clipboard.writeText(line.text);
  };

  const toggleVerbosity = (verbosity: Verbosity) =>
    setVerbosities((prev) =>
      prev.includes(verbosity) ? prev.filter((v) => v !== verbosity) : [...prev, verbosity]
    );

  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col">
      <div className="flex shrink-0 flex-wrap items-center gap-2 border-b border-border px-3 py-2">
        <div className="flex items-center gap-1">
          {VERBOSITIES.map((verbosity) => {
            const on = verbosities.includes(verbosity);
            return (
              <button
                key={verbosity}
                onClick={() => toggleVerbosity(verbosity)}
                className={cn(
                  "flex h-7 items-center gap-1.5 rounded-sm border px-2 text-[11px] font-medium transition-colors",
                  on
                    ? "border-border bg-secondary text-foreground"
                    : "border-transparent text-muted-foreground hover:text-foreground"
                )}
              >
                <span className={verbosityTextClass(verbosity)}>{VERBOSITY_LABEL[verbosity]}</span>
                <span className="tabular-nums text-muted-foreground">
                  {(counts.get(verbosity) ?? 0).toLocaleString()}
                </span>
              </button>
            );
          })}
        </div>
        <Select value={category} onValueChange={setCategory}>
          <SelectTrigger size="sm" className="h-7 max-w-56 text-[12px]">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value={ALL}>All categories</SelectItem>
            {overview.categories.map((c) => (
              <SelectItem key={c.name || NONE} value={c.name || NONE}>
                {c.name || "No category"}{" "}
                <span className="text-muted-foreground">{c.count.toLocaleString()}</span>
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <div className="relative min-w-40 flex-1">
          <Search
            size={13}
            className="pointer-events-none absolute left-2 top-1/2 -translate-y-1/2 text-muted-foreground"
          />
          <Input
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            placeholder="Search the log"
            className="h-7 pl-7 pr-7 text-[12px]"
          />
          {search && (
            <button
              onClick={() => setSearch("")}
              className="absolute right-2 top-1/2 -translate-y-1/2 text-muted-foreground hover:text-foreground"
            >
              <X size={13} />
            </button>
          )}
        </div>
        <span className="shrink-0 text-[11px] tabular-nums text-muted-foreground">
          {filtered
            ? `${total.toLocaleString()} of ${overview.line_count.toLocaleString()} lines`
            : `${overview.line_count.toLocaleString()} lines`}
        </span>
        <Tip content="Previous error">
          <Button variant="ghost" size="icon-sm" className="size-7" onClick={() => seek(false)}>
            <ArrowUp size={14} />
          </Button>
        </Tip>
        <Tip content="Next error">
          <Button variant="ghost" size="icon-sm" className="size-7" onClick={() => seek(true)}>
            <ArrowDown size={14} />
          </Button>
        </Tip>
      </div>
      {filterError && <p className="px-3 py-2 text-[12px] text-err">{filterError}</p>}
      {total === 0 ? (
        <p className="p-6 text-sm text-muted-foreground">
          {overview.line_count === 0 ? "This log has no text." : "No line matches the filter."}
        </p>
      ) : (
        <ContextMenu>
          <ContextMenuTrigger asChild>
            <div
              ref={scrollRef}
              tabIndex={0}
              onKeyDown={(e) => {
                if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "c") copyRow(selected);
              }}
              className="min-h-0 min-w-0 flex-1 overflow-auto py-1 outline-none"
            >
              <div className="relative w-full" style={{ height: virtualizer.getTotalSize() }}>
                {items.map((item) => {
                  const line = lineAt(item.index);
                  return (
                    <div
                      key={item.key}
                      onClick={() => setSelected(item.index)}
                      onContextMenu={() => {
                        contextRow.current = item.index;
                      }}
                      className={cn(
                        "absolute left-0 top-0 flex w-max min-w-full whitespace-pre font-mono text-[11px] leading-[18px]",
                        line?.verbosity === "fatal" && "bg-err/10",
                        line && line.n === marked && "bg-sky-500/15",
                        selected === item.index && "bg-secondary",
                        line?.repeat && "opacity-50"
                      )}
                      style={{ height: item.size, transform: `translateY(${item.start}px)` }}
                    >
                      <span className="sticky left-0 w-16 shrink-0 select-none bg-background pr-3 text-right tabular-nums text-muted-foreground/50">
                        {line ? line.n + 1 : ""}
                      </span>
                      <span
                        className={cn(
                          "pr-4",
                          line ? verbosityTextClass(line.verbosity) : "text-muted-foreground/40",
                          line?.trailer && "italic"
                        )}
                      >
                        {line ? line.text : "…"}
                      </span>
                    </div>
                  );
                })}
              </div>
            </div>
          </ContextMenuTrigger>
          <ContextMenuContent>
            <ContextMenuItem onSelect={() => copyRow(contextRow.current)}>
              Copy line
            </ContextMenuItem>
          </ContextMenuContent>
        </ContextMenu>
      )}
    </div>
  );
}
