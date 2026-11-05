import { describe, expect, it, vi } from "vitest";

import { dumpCommand, hitCommand } from "./cliCommand";

import type { SearchHit } from "@/components/SearchResults";

const PKG = "Marvel/Content/Marvel/UI/WBP_Thing.uasset";

const hit = (extra: Partial<SearchHit>): SearchHit => ({
  package: PKG,
  export: "WBP_Thing_C",
  export_index: 3,
  kind: "call",
  term: "Set",
  line: "Set()",
  ...extra,
});

describe("cliCommand", () => {
  it("prints a package, from a source when one is given", () => {
    expect(dumpCommand(PKG)).toBe(`rivals-cli asset dump "${PKG}"`);
    expect(dumpCommand(PKG, "C:\\Mods\\My Mod_9999999_P.pak")).toBe(
      `rivals-cli asset dump "${PKG}" --from "C:\\Mods\\My Mod_9999999_P.pak"`
    );
  });

  it("shows a bytecode hit's script and a value hit's export", () => {
    expect(hitCommand(hit({ offset: 0x40 }), "MyMod_9999999_P")).toBe(
      `rivals-cli asset script "${PKG}" --from "MyMod_9999999_P" --export 3`
    );
    expect(hitCommand(hit({ kind: "value", row: "Hulk" }))).toBe(
      `rivals-cli asset dump "${PKG}" --export 3`
    );
  });

  it("single-quotes arguments on Linux, where a ! in double quotes is history", () => {
    vi.spyOn(navigator, "userAgent", "get").mockReturnValue("Mozilla/5.0 (X11; Linux x86_64)");
    expect(dumpCommand(PKG, "!Galacta's Mod_9999999_P")).toBe(
      `rivals-cli asset dump '${PKG}' --from '!Galacta'\\''s Mod_9999999_P'`
    );
  });
});
