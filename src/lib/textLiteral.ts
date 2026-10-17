import type { PropertyValue } from "@/components/AssetInspector";

/**
 * A text as UE writes one out: `LOCTABLE("Table", "Key")`, `NSLOCTEXT("Namespace", "Key",
 * "Source")`, `INVTEXT("Text")`, or `LOCGEN_TOUPPER(...)` / `LOCGEN_TOLOWER(...)` around one. Mirrors
 * `text_literal::format(of_value(..))` in rivals-uasset, which parses it back when it is saved.
 */
export function textLiteral(value: PropertyValue): string | null {
  if (value.kind !== "text") return null;
  const part = (name: string) => value.parts?.find((held) => held.name === name)?.value;
  const table = part("TableId");
  const key = part("Key");
  if (table?.kind === "name" && key?.kind === "str") {
    return `LOCTABLE(${quoted(table.value)}, ${quoted(key.value)})`;
  }
  const source = part("SourceText");
  const transform = part("TransformType");
  if (source && transform?.kind === "byte") {
    const inner = textLiteral(source);
    if (inner === null) return null;
    return `${transform.value === 1 ? "LOCGEN_TOUPPER" : "LOCGEN_TOLOWER"}(${inner})`;
  }
  // A pattern, a number, a moment or a generator: no literal spells one.
  if (value.parts?.length) return null;
  if (value.namespace !== undefined) {
    return `NSLOCTEXT(${quoted(value.namespace)}, ${quoted(value.key ?? "")}, ${quoted(value.value ?? "")})`;
  }
  return `INVTEXT(${quoted(value.value ?? "")})`;
}

/** A quoted string with the escapes UE writes. */
function quoted(text: string): string {
  const escaped = text
    .replace(/\\/g, "\\\\")
    .replace(/"/g, '\\"')
    .replace(/\n/g, "\\n")
    .replace(/\r/g, "\\r")
    .replace(/\t/g, "\\t");
  return `"${escaped}"`;
}
