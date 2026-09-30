import { describe, expect, it } from "vitest";

import { textLiteral } from "./textLiteral";

import type { PropertyEntry, PropertyValue } from "@/components/AssetInspector";

const TABLE = "/Game/UI/Menu_ST.Menu_ST";

function part(name: string, value: PropertyValue): PropertyEntry {
  return { name, value };
}

const tableText: PropertyValue = {
  kind: "text",
  value: `${TABLE}:Play`,
  parts: [
    part("TableId", { kind: "name", value: TABLE }),
    part("Key", { kind: "str", value: "Play" }),
  ],
};

/// The same golden strings as rivals-uasset's text_literal tests: what the app prefills is what
/// the encoder parses back, so the two must spell every form alike.
describe("textLiteral", () => {
  it("spells a string table text as LOCTABLE", () => {
    expect(textLiteral(tableText)).toBe(`LOCTABLE("${TABLE}", "Play")`);
  });

  it("wraps a transformed text's source in its case change", () => {
    const upper: PropertyValue = {
      kind: "text",
      value: `${TABLE}:Play`,
      parts: [part("SourceText", tableText), part("TransformType", { kind: "byte", value: 1 })],
    };
    expect(textLiteral(upper)).toBe(`LOCGEN_TOUPPER(LOCTABLE("${TABLE}", "Play"))`);
    const lower: PropertyValue = {
      ...upper,
      parts: [
        part("SourceText", { kind: "text", value: "Loud" }),
        part("TransformType", { kind: "byte", value: 0 }),
      ],
    };
    expect(textLiteral(lower)).toBe('LOCGEN_TOLOWER(INVTEXT("Loud"))');
  });

  it("spells a localized text as NSLOCTEXT and a plain one as INVTEXT", () => {
    expect(textLiteral({ kind: "text", value: "Play now", namespace: "Menu", key: "Play" })).toBe(
      'NSLOCTEXT("Menu", "Play", "Play now")'
    );
    expect(textLiteral({ kind: "text", value: "Fixed" })).toBe('INVTEXT("Fixed")');
  });

  it("escapes what UE escapes", () => {
    expect(textLiteral({ kind: "text", value: 'quote " slash \\ line\nend' })).toBe(
      'INVTEXT("quote \\" slash \\\\ line\\nend")'
    );
  });

  it("gives nothing for a value that is not a text", () => {
    expect(textLiteral({ kind: "str", value: "x" })).toBeNull();
  });
});
