/** Drops the `<Project>/Content/` prefix every cooked path starts with. */
export function shortPath(path: string): string {
  const at = path.indexOf("/Content/");
  return at >= 0 ? path.slice(at + "/Content/".length) : path;
}
