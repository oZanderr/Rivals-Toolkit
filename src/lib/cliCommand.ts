import type { SearchHit } from "@/components/SearchResults";
import { isWindows } from "@/lib/platform";

/** `value` as one argument to the shell the command is pasted into. Linux shells read a `!` in
 *  double quotes as history, and mod names often start with one. */
function quote(value: string): string {
  return isWindows() ? `"${value}"` : `'${value.replaceAll("'", `'\\''`)}'`;
}

function fromFlag(from?: string): string {
  return from ? ` --from ${quote(from)}` : "";
}

/** The `rivals-cli` command that prints `pkg`, read from `from` when given. */
export function dumpCommand(pkg: string, from?: string): string {
  return `rivals-cli asset dump ${quote(pkg)}${fromFlag(from)}`;
}

/** The `rivals-cli` command that shows where a search hit is: the script a bytecode hit is in, or
 *  the export a value hit is in. */
export function hitCommand(hit: SearchHit, from?: string): string {
  const verb = hit.offset === undefined ? "dump" : "script";
  return `rivals-cli asset ${verb} ${quote(hit.package)}${fromFlag(from)} --export ${hit.export_index}`;
}
