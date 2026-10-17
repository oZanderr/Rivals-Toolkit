import { describe, expect, it } from "vitest";

import { type ModEntry, saveTargetNames } from "./pakName";

function mod(full_name: string, kind: "Pak" | "IoStore", enabled = true): ModEntry {
  return {
    full_name,
    display_name: full_name.replace(/\.disabled$/, ""),
    enabled,
    has_companions: kind === "IoStore",
    kind,
  };
}

describe("saveTargetNames", () => {
  const mods = [
    mod("!ProjectGalacta_9999999_P.pak", "IoStore"),
    mod("zPlain_9999999_P.pak", "Pak"),
    mod("Off_9999999_P.pak.disabled", "IoStore", false),
    mod("Sub/Nested_9999999_P.pak", "IoStore"),
    mod("Loose.pak", "Pak"),
  ];

  it("offers the IoStore mods to an IoStore save", () => {
    expect(saveTargetNames(mods, "io_store")).toEqual(["!ProjectGalacta"]);
  });

  it("offers only plain paks to a pak save, never an IoStore mod's stub", () => {
    expect(saveTargetNames(mods, "pak")).toEqual(["zPlain"]);
  });
});
