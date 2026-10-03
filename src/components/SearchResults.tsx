import { shortPath } from "@/lib/contentPath";

/** A place a search found, in a script or a stored value. */
export interface SearchHit {
  package: string;
  export: string;
  export_index: number;
  offset?: number;
  kind: "string" | "call" | "variable" | "object" | "name" | "delegate" | "value";
  term: string;
  line: string;
  /** The container a game-wide search read the package from, which is where it opens. */
  container?: string;
  /** The enabled mod whose copy of the package the game loads. */
  in_mod?: string;
}

export interface SearchResult {
  hits: SearchHit[];
  unreadable: [string, string][];
}

/** The places a search found, grouped by package; each opens where it is. */
export function SearchResults({
  query,
  result,
  onOpen,
  note,
}: {
  query: string;
  result: SearchResult;
  onOpen: (hit: SearchHit) => void;
  /** Says how the result is short of the whole answer, when it is. */
  note?: string;
}) {
  const byPackage = new Map<string, SearchHit[]>();
  for (const hit of result.hits)
    byPackage.set(hit.package, [...(byPackage.get(hit.package) ?? []), hit]);
  return (
    <section className="flex flex-col gap-3">
      <p className="text-muted-foreground">
        {result.hits.length} place{result.hits.length === 1 ? "" : "s"} name “{query}”
      </p>
      {note && <p className="text-warn">{note}</p>}
      {[...byPackage].map(([pkg, hits]) => (
        <div key={pkg}>
          <p className="flex items-baseline gap-2 font-mono text-[11px] text-muted-foreground">
            <span className="truncate">{shortPath(pkg)}</span>
            {hits[0].in_mod && (
              <span className="shrink-0 rounded-sm bg-amber-500/15 px-1 font-sans text-[10px] text-amber-400">
                in {hits[0].in_mod}
              </span>
            )}
          </p>
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
