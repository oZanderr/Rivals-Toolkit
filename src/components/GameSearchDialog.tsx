import { useEffect, useState } from "react";

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Loader2, TextSearch } from "lucide-react";

import { SearchResults, type SearchHit, type SearchResult } from "@/components/SearchResults";
import {
  AlertDialog,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Progress } from "@/components/ui/progress";
import { Switch } from "@/components/ui/switch";
import { Tip } from "@/components/ui/tooltip";

interface GameSearchResult extends SearchResult {
  /** How many places were found in all; `hits` lists the first of them by path. */
  found: number;
  /** Packages the walk listed, after the path filter. */
  listed: number;
  /** How many of them were parsed: the ones holding functions, or all of them for values. */
  searched: number;
  truncated: boolean;
  cancelled: boolean;
}

interface SearchProgress {
  phase: "listing" | "headers" | "scripts" | "packages";
  current: number;
  total: number;
}

/** The kinds of term a search can be narrowed to, as the backend names them. */
const KINDS = [
  { kind: "call", label: "Calls" },
  { kind: "delegate", label: "Delegates" },
  { kind: "string", label: "Strings" },
  { kind: "name", label: "Names" },
  { kind: "object", label: "Objects" },
  { kind: "read", label: "Reads" },
  { kind: "write", label: "Writes" },
] as const;

type Kind = (typeof KINDS)[number]["kind"];

const PHASES: Record<SearchProgress["phase"], string> = {
  listing: "Listing packages",
  headers: "Reading headers",
  scripts: "Searching scripts",
  packages: "Searching packages",
};

/**
 * The Asset Manager's "Search Game" button and the dialog it opens. The search runs in the
 * backend over every package the game loads, enabled mods included. Its state lives here rather
 * than in the dialog, so a hit opened in the inspector leaves the results, and a search still
 * running, as they were.
 */
export function GameSearch({
  gamePath,
  disabled,
  onOpenHit,
}: {
  gamePath: string;
  disabled?: boolean;
  /** Opens a hit in the inspector, from the container it was found in. */
  onOpenHit: (hit: SearchHit) => void;
}) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [values, setValues] = useState(false);
  const [filter, setFilter] = useState("");
  // No kind picked is every kind.
  const [kinds, setKinds] = useState<Kind[]>([]);
  const [wholeWord, setWholeWord] = useState(false);
  const [mods, setMods] = useState(true);
  const [running, setRunning] = useState(false);
  const [progress, setProgress] = useState<SearchProgress | null>(null);
  const [search, setSearch] = useState<{
    query: string;
    values: boolean;
    result: GameSearchResult;
  } | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | null = null;
    void listen<SearchProgress>("game-search-progress", (event) => setProgress(event.payload)).then(
      (stop) => {
        if (cancelled) stop();
        else unlisten = stop;
      }
    );
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  const run = () => {
    const text = query.trim();
    if (!text || running) return;
    setRunning(true);
    setError(null);
    setProgress(null);
    invoke<GameSearchResult>("search_game", {
      gameRoot: gamePath,
      query: text,
      values,
      filter: filter.trim() || null,
      kinds,
      wholeWord,
      mods,
    })
      .then((result) => setSearch({ query: text, values, result }))
      .catch((e: unknown) => setError(String(e)))
      .finally(() => {
        setRunning(false);
        setProgress(null);
      });
  };

  const note = search?.result.cancelled
    ? "Cancelled: these are the places found before it stopped."
    : search?.result.truncated
      ? `Showing the first ${search.result.hits.length} by path; narrow the search to see the rest.`
      : undefined;

  return (
    <>
      <Tip content="Search every script in the game and your enabled mods">
        <Button variant="outline" size="sm" onClick={() => setOpen(true)} disabled={disabled}>
          {running ? <Loader2 size={15} className="animate-spin" /> : <TextSearch size={15} />}
          Search Game
        </Button>
      </Tip>
      <AlertDialog open={open} onOpenChange={setOpen}>
        <AlertDialogContent className="flex max-h-[85vh] max-w-3xl flex-col">
          <AlertDialogHeader>
            <AlertDialogTitle>Search the game</AlertDialogTitle>
            <AlertDialogDescription>
              Every script in the game and your enabled mods: calls, delegates, strings, names,
              objects, and variables read or written.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <div className="flex items-center gap-2">
            <Input
              autoFocus
              aria-label="Search for"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") run();
              }}
              placeholder="A function, a variable, a string, an asset path…"
              className="h-8 font-mono text-[12px]"
            />
            <Button size="sm" variant="outline" disabled={!query.trim() || running} onClick={run}>
              {running ? "Searching…" : "Search"}
            </Button>
          </div>
          <div className="flex items-center gap-3 text-[12px]">
            <label className="flex shrink-0 items-center gap-2">
              <Switch checked={values} onCheckedChange={setValues} aria-label="Stored values" />
              Also search stored values (reads every package: about a minute)
            </label>
            <Input
              aria-label="Only paths containing"
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
              placeholder="Only paths containing… (optional)"
              className="h-7 font-mono text-[11px]"
            />
          </div>
          <div className="flex flex-wrap items-center gap-x-4 gap-y-2 text-[12px]">
            <div className="flex items-center gap-1" role="group" aria-label="Kinds">
              {KINDS.map(({ kind, label }) => {
                const on = kinds.includes(kind);
                return (
                  <Button
                    key={kind}
                    size="sm"
                    variant={on ? "blue" : "outline"}
                    aria-pressed={on}
                    className="h-6 px-2 text-[11px]"
                    onClick={() =>
                      setKinds((held) =>
                        on ? held.filter((other) => other !== kind) : [...held, kind]
                      )
                    }
                  >
                    {label}
                  </Button>
                );
              })}
            </div>
            <label className="flex items-center gap-2">
              <Switch checked={wholeWord} onCheckedChange={setWholeWord} aria-label="Whole word" />
              Whole word
            </label>
            <label className="flex items-center gap-2">
              <Switch checked={mods} onCheckedChange={setMods} aria-label="Enabled mods" />
              Include enabled mods
            </label>
          </div>
          {running && (
            <div className="flex items-center gap-2 text-[11px] text-muted-foreground">
              <span className="shrink-0">{progress ? PHASES[progress.phase] : "Starting"}</span>
              {progress && progress.total > 0 ? (
                <>
                  <Progress
                    value={(progress.current / progress.total) * 100}
                    className="h-2 w-40 shrink-0"
                  />
                  <span className="shrink-0">
                    {progress.current}/{progress.total}
                  </span>
                </>
              ) : (
                <Loader2 size={12} className="shrink-0 animate-spin" />
              )}
              <Button
                variant="outline"
                size="sm"
                className="ml-auto h-6 px-2 text-[11px]"
                onClick={() => void invoke("cancel_game_search").catch(() => undefined)}
              >
                Cancel
              </Button>
            </div>
          )}
          <div className="-mx-6 flex-1 overflow-y-auto px-6 text-[12px]">
            {error && <p className="text-err">{error}</p>}
            {search && (
              <>
                <p className="mb-2 text-[11px] text-muted-foreground">
                  Searched {search.result.searched} of {search.result.listed} packages
                  {search.values ? "." : ", the ones holding functions."}
                </p>
                <SearchResults
                  query={search.query}
                  result={search.result}
                  found={search.result.found}
                  note={note}
                  onOpen={(hit) => {
                    setOpen(false);
                    onOpenHit(hit);
                  }}
                />
              </>
            )}
          </div>
          <AlertDialogFooter>
            <AlertDialogCancel>Close</AlertDialogCancel>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </>
  );
}
