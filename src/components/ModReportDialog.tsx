import { useEffect, useState } from "react";

import { invoke } from "@tauri-apps/api/core";

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
import { cn } from "@/lib/utils";

export interface SearchHit {
  package: string;
  export: string;
  export_index: number;
  offset?: number;
  kind: "string" | "call" | "variable" | "object" | "value";
  term: string;
  line: string;
}

interface SearchResult {
  hits: SearchHit[];
  unreadable: [string, string][];
}

interface PackageReport {
  path: string;
  kind: string;
  overrides_game: boolean;
  error?: string;
}

type ByPackage = Record<string, string[]>;

interface ModReport {
  container: string;
  patch_priority: number | null;
  packages: PackageReport[];
  runtime_natives: ByPackage;
  python_classes: ByPackage;
  files: ByPackage;
  save_slots: ByPackage;
  urls: ByPackage;
}

interface Props {
  gamePath: string;
  /** The selected container's `.pak`; its `.utoc` is what gets read. */
  container: string;
  onClose: () => void;
  /** Opens a search hit in the inspector, at its statement when it has one. */
  onOpenHit: (hit: SearchHit) => void;
}

/** The patch number nearly every mod ships with, which says nothing about this one. */
const USUAL_PRIORITY = 9999999;

const SECTIONS: { key: keyof ModReport; title: string; hint: string }[] = [
  {
    key: "runtime_natives",
    title: "Runtime natives",
    hint: "Functions the game registers only at runtime, such as its pak hot-patch utility.",
  },
  {
    key: "python_classes",
    title: "Python classes",
    hint: "Game classes backed by its embedded Python, which a game update can rename.",
  },
  {
    key: "files",
    title: "Files",
    hint: "Loose files a script names, read or written next to the game.",
  },
  { key: "save_slots", title: "Save slots", hint: "SaveGame slots the mod keeps state in." },
  { key: "urls", title: "Links", hint: "Web addresses a script opens." },
];

/** Mount with `key={container}` so each mod starts from an empty report. */
export function ModReportDialog({ gamePath, container, onClose, onOpenHit }: Props) {
  const modName = container.split(/[\\/]/).pop() ?? container;
  const [report, setReport] = useState<ModReport | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    invoke<ModReport>("get_mod_report", { gameRoot: gamePath, container })
      .then((r) => {
        if (!cancelled) setReport(r);
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [gamePath, container]);

  const [query, setQuery] = useState("");
  const [search, setSearch] = useState<{ query: string; result: SearchResult } | null>(null);
  const [searching, setSearching] = useState(false);
  const [searchError, setSearchError] = useState<string | null>(null);
  const runSearch = () => {
    const text = query.trim();
    if (!text || searching) return;
    setSearching(true);
    setSearchError(null);
    invoke<SearchResult>("search_mod", { gameRoot: gamePath, container, query: text })
      .then((result) => setSearch({ query: text, result }))
      .catch((e: unknown) => setSearchError(String(e)))
      .finally(() => setSearching(false));
  };

  const overrides = report?.packages.filter((p) => p.overrides_game) ?? [];
  const added = report?.packages.filter((p) => !p.overrides_game) ?? [];

  return (
    <AlertDialog open onOpenChange={(open) => !open && onClose()}>
      <AlertDialogContent className="max-w-2xl max-h-[80vh] flex flex-col">
        <AlertDialogHeader>
          <AlertDialogTitle>Mod contents</AlertDialogTitle>
          <AlertDialogDescription className="truncate">
            {modName}
            {report?.patch_priority != null &&
              report.patch_priority !== USUAL_PRIORITY &&
              ` · patch priority ${report.patch_priority}`}
          </AlertDialogDescription>
        </AlertDialogHeader>
        <div className="flex items-center gap-2">
          <Input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") runSearch();
            }}
            placeholder="Search scripts and values: a file name, a function, a variable…"
            className="h-8 font-mono text-[12px]"
          />
          <Button
            size="sm"
            variant="outline"
            disabled={!query.trim() || searching}
            onClick={runSearch}
          >
            {searching ? "Searching…" : "Search"}
          </Button>
          {search && (
            <Button size="sm" variant="ghost" onClick={() => setSearch(null)}>
              Clear
            </Button>
          )}
        </div>
        <div className="flex-1 overflow-y-auto -mx-6 px-6 text-[12px]">
          {searchError && <p className="text-err">{searchError}</p>}
          {search && (
            <SearchResults query={search.query} result={search.result} onOpen={onOpenHit} />
          )}
          {error && <p className="text-err">{error}</p>}
          {!error && !report && <p className="text-muted-foreground">Reading the mod…</p>}
          {report && !search && (
            <div className="flex flex-col gap-4">
              <PackageList
                title={`Replaces game assets (${overrides.length})`}
                packages={overrides}
              />
              <PackageList title={`Adds (${added.length})`} packages={added} />
              {SECTIONS.map(({ key, title, hint }) => {
                const section = report[key] as ByPackage;
                const entries = Object.entries(section);
                if (entries.length === 0) return null;
                return (
                  <section key={key}>
                    <h3 className="font-semibold">{title}</h3>
                    <p className="mb-1 text-[11px] text-muted-foreground">{hint}</p>
                    {entries.map(([pkg, values]) => (
                      <div key={pkg} className="mb-1.5">
                        <p className="truncate font-mono text-[11px] text-muted-foreground">
                          {shortPath(pkg)}
                        </p>
                        {values.map((v) => (
                          <p key={v} className="truncate pl-3 font-mono text-[11px]">
                            {v}
                          </p>
                        ))}
                      </div>
                    ))}
                  </section>
                );
              })}
            </div>
          )}
        </div>
        <AlertDialogFooter>
          <AlertDialogCancel>Close</AlertDialogCancel>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}

function SearchResults({
  query,
  result,
  onOpen,
}: {
  query: string;
  result: SearchResult;
  onOpen: (hit: SearchHit) => void;
}) {
  const byPackage = new Map<string, SearchHit[]>();
  for (const hit of result.hits)
    byPackage.set(hit.package, [...(byPackage.get(hit.package) ?? []), hit]);
  return (
    <section className="flex flex-col gap-3">
      <p className="text-muted-foreground">
        {result.hits.length} place{result.hits.length === 1 ? "" : "s"} name “{query}”
      </p>
      {[...byPackage].map(([pkg, hits]) => (
        <div key={pkg}>
          <p className="truncate font-mono text-[11px] text-muted-foreground">{shortPath(pkg)}</p>
          {hits.map((hit, i) => (
            <button
              key={i}
              className="flex w-full items-baseline gap-2 rounded-sm pl-3 text-left font-mono text-[11px] hover:bg-muted/50"
              onClick={() => onOpen(hit)}
            >
              <span className="w-14 shrink-0 font-sans text-[10px] uppercase text-muted-foreground">
                {hit.kind}
              </span>
              <span className="shrink-0 text-blue-accent-foreground">
                {hit.export}
                {hit.offset !== undefined &&
                  ` 0x${hit.offset.toString(16).toUpperCase().padStart(4, "0")}`}
              </span>
              <span className="truncate" title={hit.line}>
                {hit.line}
              </span>
            </button>
          ))}
        </div>
      ))}
      {result.unreadable.length > 0 && (
        <p className="text-warn">
          {result.unreadable.length} package{result.unreadable.length === 1 ? "" : "s"} could not be
          read, so they were not searched: {result.unreadable.map(([p]) => shortPath(p)).join(", ")}
        </p>
      )}
    </section>
  );
}

function PackageList({ title, packages }: { title: string; packages: PackageReport[] }) {
  if (packages.length === 0) return null;
  return (
    <section>
      <h3 className="mb-1 font-semibold">{title}</h3>
      {packages.map((p) => (
        <div key={p.path} className="flex items-center gap-2">
          <span
            className={cn(
              "w-32 shrink-0 truncate text-[11px]",
              p.error ? "text-warn" : "text-muted-foreground"
            )}
            title={p.error}
          >
            {p.kind}
          </span>
          <span className="truncate font-mono text-[11px]" title={p.path}>
            {shortPath(p.path)}
          </span>
        </div>
      ))}
    </section>
  );
}

/** Drops the `<Project>/Content/` prefix every cooked path starts with. */
function shortPath(path: string): string {
  const at = path.indexOf("/Content/");
  return at >= 0 ? path.slice(at + "/Content/".length) : path;
}
