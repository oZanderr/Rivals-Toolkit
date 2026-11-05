import { describe, expect, it } from "vitest";

import { joinPath, modsFolderOf } from "./paths";

describe("paths", () => {
  it("joins in the root's own separators", () => {
    expect(joinPath("C:\\Game", "a/b", "c")).toBe("C:\\Game\\a\\b\\c");
    expect(joinPath("/home/me/Game", "a/b", "c")).toBe("/home/me/Game/a/b/c");
    expect(joinPath("D:\\", "x")).toBe("D:\\x");
    expect(joinPath("/", "x")).toBe("/x");
  });

  it("finds ~mods below the game on either system", () => {
    expect(modsFolderOf("C:\\Game")).toBe("C:\\Game\\MarvelGame\\Marvel\\Content\\Paks\\~mods");
    expect(modsFolderOf("/games/steamapps/common/MarvelRivals")).toBe(
      "/games/steamapps/common/MarvelRivals/MarvelGame/Marvel/Content/Paks/~mods"
    );
  });
});
