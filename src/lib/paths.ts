/** `parts` joined onto `root` in the root's own separators, a `/` in a part included. */
export function joinPath(root: string, ...parts: string[]): string {
  const sep = root.includes("\\") ? "\\" : "/";
  const base = root.endsWith(sep) ? root.slice(0, -1) : root;
  return [base, ...parts.map((part) => part.replaceAll("/", sep))].join(sep);
}

/** The `~mods` folder of the game installed at `gamePath`. */
export function modsFolderOf(gamePath: string): string {
  return joinPath(gamePath, "MarvelGame", "Marvel", "Content", "Paks", "~mods");
}
