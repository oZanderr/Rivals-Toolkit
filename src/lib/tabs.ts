import {
  FileCode2,
  Package,
  Plug,
  Puzzle,
  ScrollText,
  Settings as SettingsIcon,
  SlidersHorizontal,
  Volume2,
  type LucideIcon,
} from "lucide-react";

export type Tab =
  | "mod-tools"
  | "plugins"
  | "pak-manager"
  | "ini-editor"
  | "config-tweaks"
  | "sounds"
  | "logs"
  | "settings";

export interface TabInfo {
  id: Tab;
  label: string;
  icon: LucideIcon;
}

/** Every tab, in the order the sidebar shows them until the user arranges them. */
export const TABS: readonly TabInfo[] = [
  { id: "mod-tools", label: "Mods", icon: Puzzle },
  { id: "plugins", label: "Plugins", icon: Plug },
  { id: "sounds", label: "Sounds", icon: Volume2 },
  { id: "config-tweaks", label: "Config Tweaks", icon: SlidersHorizontal },
  { id: "ini-editor", label: "INI Editor", icon: FileCode2 },
  { id: "pak-manager", label: "Asset Manager", icon: Package },
  { id: "logs", label: "Logs", icon: ScrollText },
  { id: "settings", label: "Settings", icon: SettingsIcon },
];

/** Holds the game path, and is where hidden tabs are shown again, so it never hides. */
export const ALWAYS_SHOWN: Tab = "settings";

export interface TabLayout {
  /** Every tab, in the user's order. */
  order: Tab[];
  hidden: Tab[];
}

/** The layout until the user arranges the tabs: Config Tweaks starts hidden. */
export const DEFAULT_TAB_LAYOUT: TabLayout = {
  order: TABS.map((t) => t.id),
  hidden: ["config-tweaks"],
};

const BY_ID = new Map(TABS.map((t) => [t.id, t]));

export function tabInfo(id: Tab): TabInfo {
  return BY_ID.get(id) ?? TABS[0];
}

function isTab(value: unknown): value is Tab {
  return typeof value === "string" && BY_ID.has(value as Tab);
}

/**
 * A stored layout as one this build can show: ids it does not know and repeats are dropped, a tab
 * missing from the order goes back after the tab it follows by default, and Settings never hides.
 * One naming no hidden tabs, as none stored, hides what the default does.
 */
export function normalizeTabLayout(stored: { order?: unknown; hidden?: unknown }): TabLayout {
  const order: Tab[] = [];
  for (const id of Array.isArray(stored.order) ? stored.order : []) {
    if (isTab(id) && !order.includes(id)) order.push(id);
  }
  const defaults = DEFAULT_TAB_LAYOUT.order;
  defaults.forEach((id, at) => {
    if (order.includes(id)) return;
    const before = defaults.slice(0, at).findLast((prev) => order.includes(prev));
    order.splice(before === undefined ? 0 : order.indexOf(before) + 1, 0, id);
  });
  const storedHidden: readonly unknown[] = Array.isArray(stored.hidden)
    ? stored.hidden
    : DEFAULT_TAB_LAYOUT.hidden;
  const hidden = order.filter((id) => id !== ALWAYS_SHOWN && storedHidden.includes(id));
  return { order, hidden };
}

export function visibleTabs(layout: TabLayout): Tab[] {
  return layout.order.filter((id) => !layout.hidden.includes(id));
}

/**
 * `layout` with the tab at `from` among `shown` moved to `to` among them. The shown tabs go back into
 * the places they held in the order, so a hidden tab keeps its place.
 */
export function reorderTabs(layout: TabLayout, shown: Tab[], from: number, to: number): TabLayout {
  if (from === to || !shown[from] || !shown[to]) return layout;
  const moved = [...shown];
  const [tab] = moved.splice(from, 1);
  moved.splice(to, 0, tab);
  let next = 0;
  const order = layout.order.map((id) => (shown.includes(id) ? moved[next++] : id));
  return { ...layout, order };
}

export function setTabHidden(layout: TabLayout, id: Tab, hidden: boolean): TabLayout {
  if (id === ALWAYS_SHOWN || layout.hidden.includes(id) === hidden) return layout;
  return {
    ...layout,
    hidden: hidden
      ? layout.order.filter((t) => t === id || layout.hidden.includes(t))
      : layout.hidden.filter((t) => t !== id),
  };
}

export function isDefaultTabLayout(layout: TabLayout): boolean {
  const same = (a: readonly Tab[], b: readonly Tab[]) =>
    a.length === b.length && a.every((id, at) => id === b[at]);
  return (
    same(layout.order, DEFAULT_TAB_LAYOUT.order) && same(layout.hidden, DEFAULT_TAB_LAYOUT.hidden)
  );
}
