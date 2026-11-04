/** What the tree needs of a mod. */
export interface TreeMod {
  /** The pak's path below `~mods`, `/` between folders. */
  full_name: string;
  enabled: boolean;
}

export interface FolderNode<M extends TreeMod> {
  /** Below `~mods`, `/` between its parts, and "" for `~mods` itself. */
  path: string;
  name: string;
  folders: FolderNode<M>[];
  mods: M[];
}

export type TreeRow<M extends TreeMod> =
  | {
      kind: "folder";
      folder: FolderNode<M>;
      depth: number;
      expanded: boolean;
      /** The mods it covers, those inside its subfolders too: only the matching ones while a
       *  filter is on. */
      mods: M[];
    }
  | { kind: "mod"; mod: M; depth: number; parent: string };

export function parentFolder(path: string): string {
  const at = path.lastIndexOf("/");
  return at < 0 ? "" : path.slice(0, at);
}

export function leafName(path: string): string {
  return path.slice(path.lastIndexOf("/") + 1);
}

export function joinFolder(parent: string, name: string): string {
  return parent ? `${parent}/${name}` : name;
}

/** How far in, in pixels, a row at `depth` sits, capped so a deep tree keeps room for names. */
export function indent(depth: number): number {
  return Math.min(depth, 8) * 16;
}

/** Whether `path` is the folder `folder` or inside it. */
export function isInFolder(path: string, folder: string): boolean {
  return folder === "" || path === folder || path.startsWith(`${folder}/`);
}

const byName = (a: { name: string }, b: { name: string }) =>
  a.name.localeCompare(b.name, undefined, { sensitivity: "base", numeric: true });

/** The folder tree of `~mods`: every folder listed and every one a mod is in, each holding its
 *  subfolders by name, then its mods in the order given. */
export function buildModTree<M extends TreeMod>(
  mods: readonly M[],
  folders: readonly string[]
): FolderNode<M> {
  const root: FolderNode<M> = { path: "", name: "", folders: [], mods: [] };
  const byPath = new Map<string, FolderNode<M>>([["", root]]);
  const folderAt = (path: string): FolderNode<M> => {
    const known = byPath.get(path);
    if (known) return known;
    const node: FolderNode<M> = { path, name: leafName(path), folders: [], mods: [] };
    byPath.set(path, node);
    folderAt(parentFolder(path)).folders.push(node);
    return node;
  };
  for (const path of folders) folderAt(path);
  for (const mod of mods) folderAt(parentFolder(mod.full_name)).mods.push(mod);
  const sort = (node: FolderNode<M>) => {
    node.folders.sort(byName);
    node.folders.forEach(sort);
  };
  sort(root);
  return root;
}

/** The mods inside `folder` and its subfolders, those `matches` keeps when given. */
export function modsIn<M extends TreeMod>(
  folder: FolderNode<M>,
  matches?: (mod: M) => boolean
): M[] {
  const own = matches ? folder.mods.filter(matches) : folder.mods;
  return [...own, ...folder.folders.flatMap((sub) => modsIn(sub, matches))];
}

/** Every folder below `root`, in the order the tree shows them. */
export function folderPaths<M extends TreeMod>(root: FolderNode<M>): string[] {
  return root.folders.flatMap((folder) => [folder.path, ...folderPaths(folder)]);
}

/** The rows the list shows, folders before mods at each level. A collapsed folder hides what is
 *  inside it. While `matches` filters, only folders holding a match show, every one open. */
export function treeRows<M extends TreeMod>(
  root: FolderNode<M>,
  collapsed: ReadonlySet<string>,
  matches?: (mod: M) => boolean
): TreeRow<M>[] {
  const rows: TreeRow<M>[] = [];
  const visit = (node: FolderNode<M>, depth: number) => {
    for (const folder of node.folders) {
      const mods = modsIn(folder, matches);
      if (matches && mods.length === 0) continue;
      const expanded = matches !== undefined || !collapsed.has(folder.path);
      rows.push({ kind: "folder", folder, depth, expanded, mods });
      if (expanded) visit(folder, depth + 1);
    }
    for (const mod of node.mods) {
      if (!matches || matches(mod)) rows.push({ kind: "mod", mod, depth, parent: node.path });
    }
  };
  visit(root, 0);
  return rows;
}

/** The name the mod `fullName` has once moved into the folder `to`. */
export function movedName(fullName: string, to: string): string {
  return joinFolder(to, leafName(fullName));
}

/** `path` with the folder `from` in it renamed `to`, or null when it is not in `from`. */
export function renamedUnder(path: string, from: string, to: string): string | null {
  if (path === from) return to;
  return path.startsWith(`${from}/`) ? to + path.slice(from.length) : null;
}

/** `paths` with the folder `from` renamed `to` in each it is in. */
export function withFolderRenamed(
  paths: ReadonlySet<string>,
  from: string,
  to: string
): Set<string> {
  return new Set([...paths].map((path) => renamedUnder(path, from, to) ?? path));
}

/** `paths` without the folder `folder` and every folder inside it. */
export function withoutFolder(paths: ReadonlySet<string>, folder: string): Set<string> {
  return new Set([...paths].filter((path) => !isInFolder(path, folder)));
}

/** `collapsed` with `folder` and every folder above it open, so it shows. */
export function withFolderShown(collapsed: ReadonlySet<string>, folder: string): Set<string> {
  return new Set(
    [...collapsed].filter((path) => folder !== path && !folder.startsWith(`${path}/`))
  );
}
