import { describe, expect, it } from "vitest";

import {
  buildModTree,
  folderPaths,
  modsIn,
  movedName,
  renamedUnder,
  treeRows,
  withFolderRenamed,
  withFolderShown,
  withoutFolder,
  type TreeRow,
} from "./modTree";

const mod = (full_name: string, enabled = true) => ({ full_name, enabled });

const MODS = [
  mod("Top.pak"),
  mod("Skins/b.pak"),
  mod("Skins/Venom/Red.pak"),
  mod("Skins/Venom/Blue.pak.disabled", false),
  mod("Testing/Try.pak.disabled", false),
];
const FOLDERS = ["Empty", "Folder 10", "Folder 2", "Skins", "Skins/Venom", "Testing"];

function shown(rows: TreeRow<ReturnType<typeof mod>>[]) {
  return rows.map((row) =>
    row.kind === "folder"
      ? `${"  ".repeat(row.depth)}[${row.folder.name}] ${row.mods.length}`
      : `${"  ".repeat(row.depth)}${row.mod.full_name}`
  );
}

describe("buildModTree", () => {
  it("puts folders first, by name ignoring case and with numbers in order, then mods as given", () => {
    const tree = buildModTree(MODS, FOLDERS);
    expect(folderPaths(tree)).toEqual([
      "Empty",
      "Folder 2",
      "Folder 10",
      "Skins",
      "Skins/Venom",
      "Testing",
    ]);
    expect(tree.mods.map((m) => m.full_name)).toEqual(["Top.pak"]);
  });

  it("makes the folders a mod is in when the listing leaves them out", () => {
    const tree = buildModTree([mod("A/B/C.pak")], []);
    expect(folderPaths(tree)).toEqual(["A", "A/B"]);
    expect(modsIn(tree.folders[0]).map((m) => m.full_name)).toEqual(["A/B/C.pak"]);
  });
});

describe("treeRows", () => {
  const tree = buildModTree(MODS, ["Skins", "Skins/Venom", "Testing", "Empty"]);

  it("nests rows, each folder covering the mods in its subfolders too", () => {
    expect(shown(treeRows(tree, new Set()))).toEqual([
      "[Empty] 0",
      "[Skins] 3",
      "  [Venom] 2",
      "    Skins/Venom/Red.pak",
      "    Skins/Venom/Blue.pak.disabled",
      "  Skins/b.pak",
      "[Testing] 1",
      "  Testing/Try.pak.disabled",
      "Top.pak",
    ]);
  });

  it("hides what a collapsed folder holds, its open subfolders too", () => {
    const rows = shown(treeRows(tree, new Set(["Skins", "Testing"])));
    expect(rows).toEqual(["[Empty] 0", "[Skins] 3", "[Testing] 1", "Top.pak"]);
  });

  it("shows only the folders holding a match while filtering, every one open", () => {
    const rows = treeRows(tree, new Set(["Skins"]), (m) => m.full_name.includes("Red"));
    expect(shown(rows)).toEqual(["[Skins] 1", "  [Venom] 1", "    Skins/Venom/Red.pak"]);
    expect(rows.every((row) => row.kind !== "folder" || row.expanded)).toBe(true);
  });
});

describe("following a move or a rename", () => {
  it("names a moved mod in its new folder", () => {
    expect(movedName("Skins/Venom/Red.pak", "")).toBe("Red.pak");
    expect(movedName("Top.pak.disabled", "Testing")).toBe("Testing/Top.pak.disabled");
  });

  it("renames inside the folder renamed and nowhere else", () => {
    expect(renamedUnder("Skins", "Skins", "Art")).toBe("Art");
    expect(renamedUnder("Skins/Venom/Red.pak", "Skins", "Art")).toBe("Art/Venom/Red.pak");
    expect(renamedUnder("Skinsy/Red.pak", "Skins", "Art")).toBeNull();
    const collapsed = new Set(["Skins", "Skins/Venom", "Skinsy"]);
    expect([...withFolderRenamed(collapsed, "Skins", "Art")]).toEqual([
      "Art",
      "Art/Venom",
      "Skinsy",
    ]);
  });

  it("forgets a deleted folder and the folders in it", () => {
    const collapsed = new Set(["Skins", "Skins/Venom", "Skinsy"]);
    expect([...withoutFolder(collapsed, "Skins")]).toEqual(["Skinsy"]);
  });

  it("opens a folder and every folder above it", () => {
    const collapsed = new Set(["Skins", "Skins/Venom", "Skins/Venom/Red", "Testing"]);
    expect([...withFolderShown(collapsed, "Skins/Venom")]).toEqual(["Skins/Venom/Red", "Testing"]);
  });
});
