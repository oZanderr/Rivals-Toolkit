import { variantLabel, type AbilityPlace } from "@/lib/abilityPlace";
import { shortPath } from "@/lib/contentPath";

/** A place a search found, in a script or a stored value. */
export interface SearchHit {
  package: string;
  export: string;
  export_index: number;
  offset?: number;
  kind:
    | "string"
    | "call"
    | "read"
    | "write"
    | "object"
    | "name"
    | "delegate"
    | "bind"
    | "broadcast"
    | "value"
    | "field";
  term: string;
  line: string;
  /** The call a call hit is found by, with its arguments. */
  call?: string;
  /** The container a game-wide search read the package from, which is where it opens. */
  container?: string;
  /** The enabled mod whose copy of the package the game loads. */
  in_mod?: string;
  /** The DataTable row or StringTable entry a stored value is in. */
  row?: string;
  /** A stored value's path from its row or export. */
  field?: string;
  /** The row's identifying field, as `GamePlayModeId 600`. */
  id?: string;
  place?: AbilityPlace;
}

export interface SearchResult {
  hits: SearchHit[];
  unreadable: [string, string][];
}

/** How a result is laid out: by package alone, or under each hero or each ability first. */
export type GroupBy = "package" | "hero" | "ability";

interface Group {
  key: string;
  title: string;
  variant: string | null;
  /** Hero, ability, then the game's own version before a mode's and a mode's before an event's. */
  order: number[];
}

const VARIANT_RANK = { base: 0, mode: 1, event: 2 } as const;

/** A group's heading and where it goes; hits outside any hero go last, under Other. */
function groupOf(hit: SearchHit, by: GroupBy): Group {
  const place = hit.place;
  if (!place?.hero) return { key: "other", title: "Other", variant: null, order: [Infinity] };
  if (by === "hero" || !place.ability) {
    return { key: `${place.hero}`, title: `${place.hero}`, variant: null, order: [place.hero] };
  }
  const variant = variantLabel(place.variant);
  const id = place.variant.kind === "base" ? 0 : place.variant.id;
  return {
    key: `${place.ability}|${variant ?? ""}`,
    title: `${place.hero} / ${place.ability}`,
    variant,
    order: [place.hero, place.ability, VARIANT_RANK[place.variant.kind], id],
  };
}

function byOrder(a: Group, b: Group): number {
  for (let i = 0; i < Math.max(a.order.length, b.order.length); i++) {
    const [x, y] = [a.order[i] ?? -1, b.order[i] ?? -1];
    if (x !== y) return x < y ? -1 : 1;
  }
  return 0;
}

function HitRow({ hit, onOpen }: { hit: SearchHit; onOpen: (hit: SearchHit) => void }) {
  const shown = hit.call ?? hit.line;
  return (
    <button
      className="flex w-full items-baseline gap-2 rounded-sm pl-3 text-left font-mono text-[11px] hover:bg-muted/50"
      onClick={() => onOpen(hit)}
    >
      <span className="w-16 shrink-0 font-sans text-[10px] uppercase text-muted-foreground">
        {hit.kind}
      </span>
      <span className="shrink-0 text-blue-accent-foreground">
        {hit.export}
        {hit.offset !== undefined && ` 0x${hit.offset.toString(16).toUpperCase().padStart(4, "0")}`}
      </span>
      {hit.id && (
        <span className="shrink-0 rounded-sm bg-muted px-1 font-sans text-[10px] text-muted-foreground">
          {hit.id}
        </span>
      )}
      <span className="truncate" title={hit.line}>
        {shown}
      </span>
    </button>
  );
}

function PackageHits({
  pkg,
  hits,
  onOpen,
}: {
  pkg: string;
  hits: SearchHit[];
  onOpen: (hit: SearchHit) => void;
}) {
  return (
    <div>
      <p className="flex items-baseline gap-2 font-mono text-[11px] text-muted-foreground">
        <span className="truncate">{shortPath(pkg)}</span>
        {hits[0].in_mod && (
          <span className="shrink-0 rounded-sm bg-amber-500/15 px-1 font-sans text-[10px] text-amber-400">
            in {hits[0].in_mod}
          </span>
        )}
      </p>
      {hits.map((hit, i) => (
        <HitRow key={i} hit={hit} onOpen={onOpen} />
      ))}
    </div>
  );
}

function byPackage(hits: SearchHit[]): Map<string, SearchHit[]> {
  const out = new Map<string, SearchHit[]>();
  for (const hit of hits) out.set(hit.package, [...(out.get(hit.package) ?? []), hit]);
  return out;
}

/** The places a search found, grouped by package, under each hero or ability when asked; each
 *  opens where it is. */
export function SearchResults({
  query,
  result,
  onOpen,
  note,
  found = result.hits.length,
  packages,
  groupBy = "package",
}: {
  query: string;
  result: SearchResult;
  onOpen: (hit: SearchHit) => void;
  /** How many places were found in all, when that is more than are listed. */
  found?: number;
  /** How many packages hold them, listed or not. */
  packages?: number;
  /** Says how the result is short of the whole answer, when it is. */
  note?: string;
  groupBy?: GroupBy;
}) {
  const where = packages === undefined ? "" : ` in ${packages} package${packages === 1 ? "" : "s"}`;
  const groups = new Map<string, Group & { hits: SearchHit[] }>();
  if (groupBy !== "package") {
    for (const hit of result.hits) {
      const group = groupOf(hit, groupBy);
      const held = groups.get(group.key) ?? { ...group, hits: [] };
      held.hits.push(hit);
      groups.set(group.key, held);
    }
  }
  return (
    <section className="flex flex-col gap-3">
      <p className="text-muted-foreground">
        {found} place{found === 1 ? "" : "s"}
        {where} name “{query}”
      </p>
      {note && <p className="text-warn">{note}</p>}
      {groupBy === "package"
        ? [...byPackage(result.hits)].map(([pkg, hits]) => (
            <PackageHits key={pkg} pkg={pkg} hits={hits} onOpen={onOpen} />
          ))
        : [...groups.values()].sort(byOrder).map((group) => (
            <div key={group.key} className="flex flex-col gap-1">
              <p className="flex items-baseline gap-2 text-[12px] font-semibold">
                <span>{group.title}</span>
                {group.variant && (
                  <span className="rounded-sm bg-sky-500/15 px-1 text-[10px] font-normal text-sky-400">
                    {group.variant}
                  </span>
                )}
              </p>
              {[...byPackage(group.hits)].map(([pkg, hits]) => (
                <PackageHits key={pkg} pkg={pkg} hits={hits} onOpen={onOpen} />
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
