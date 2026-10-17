/** Which version of an ability a package belongs to: the game's own, or a mode's or an event's. */
export type AbilityVariant =
  | { kind: "base" }
  | { kind: "mode"; id: number }
  | { kind: "event"; id: number };

/** The hero and ability a package belongs to, as its path or the Python class it names says. */
export interface AbilityPlace {
  hero?: number;
  ability?: number;
  variant: AbilityVariant;
}

/** A mode's or an event's version as a badge reads it; nothing for the game's own. */
export function variantLabel(variant: AbilityVariant): string | null {
  switch (variant.kind) {
    case "mode":
      return `mode ${variant.id}`;
    case "event":
      return `event ${variant.id}`;
    default:
      return null;
  }
}
