import type { SearchHit } from "@/components/SearchResults";

function fromFlag(from?: string): string {
  return from ? ` --from "${from}"` : "";
}

/** The `rivals-cli` command that prints `pkg`, read from `from` when given. */
export function dumpCommand(pkg: string, from?: string): string {
  return `rivals-cli asset dump "${pkg}"${fromFlag(from)}`;
}

/** The `rivals-cli` command that shows where a search hit is: the script a bytecode hit is in, or
 *  the export a value hit is in. */
export function hitCommand(hit: SearchHit, from?: string): string {
  const verb = hit.offset === undefined ? "dump" : "script";
  return `rivals-cli asset ${verb} "${hit.package}"${fromFlag(from)} --export ${hit.export_index}`;
}
