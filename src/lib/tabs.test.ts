import { describe, expect, it } from "vitest";

import {
  DEFAULT_TAB_LAYOUT,
  isDefaultTabLayout,
  normalizeTabLayout,
  reorderTabs,
  setTabHidden,
  visibleTabs,
  type TabLayout,
} from "./tabs";

const withPluginsHidden: TabLayout = { ...DEFAULT_TAB_LAYOUT, hidden: ["plugins"] };

describe("normalizeTabLayout", () => {
  it("gives the default for nothing stored, or nothing readable, with Config Tweaks hidden", () => {
    expect(normalizeTabLayout({})).toEqual(DEFAULT_TAB_LAYOUT);
    expect(normalizeTabLayout({ order: 5, hidden: "logs" })).toEqual(DEFAULT_TAB_LAYOUT);
    expect(DEFAULT_TAB_LAYOUT.hidden).toEqual(["config-tweaks"]);
  });

  it("keeps a stored layout that hides nothing showing everything", () => {
    expect(normalizeTabLayout({ order: [], hidden: [] }).hidden).toEqual([]);
  });

  it("drops ids it does not know and repeats", () => {
    const { order } = normalizeTabLayout({
      order: ["logs", "gone", "logs", 7, ...DEFAULT_TAB_LAYOUT.order],
    });
    expect(order).toEqual(["logs", ...DEFAULT_TAB_LAYOUT.order.filter((id) => id !== "logs")]);
  });

  it("puts a tab missing from the order back after the one it follows by default", () => {
    // Logs follows Asset Manager by default, and Mods follows nothing, so it leads.
    const { order } = normalizeTabLayout({
      order: ["settings", "pak-manager", "plugins", "sounds", "config-tweaks", "ini-editor"],
    });
    expect(order).toEqual([
      "mod-tools",
      "settings",
      "pak-manager",
      "logs",
      "plugins",
      "sounds",
      "config-tweaks",
      "ini-editor",
    ]);
  });

  it("never hides Settings, and keeps the hidden ids it knows in the tabs' order", () => {
    const { hidden } = normalizeTabLayout({ hidden: ["settings", "logs", "nope", "sounds"] });
    expect(hidden).toEqual(["sounds", "logs"]);
  });
});

describe("reorderTabs", () => {
  it("moves among the shown tabs and leaves a hidden one in its place", () => {
    const shown = visibleTabs(withPluginsHidden);
    expect(shown).not.toContain("plugins");
    const { order, hidden } = reorderTabs(withPluginsHidden, shown, 0, 2);
    expect(order).toEqual([
      "sounds",
      "plugins",
      "config-tweaks",
      "mod-tools",
      "ini-editor",
      "pak-manager",
      "logs",
      "settings",
    ]);
    expect(hidden).toEqual(["plugins"]);
  });

  it("ignores a move onto itself or out of range", () => {
    const shown = visibleTabs(DEFAULT_TAB_LAYOUT);
    expect(reorderTabs(DEFAULT_TAB_LAYOUT, shown, 3, 3)).toBe(DEFAULT_TAB_LAYOUT);
    expect(reorderTabs(DEFAULT_TAB_LAYOUT, shown, 0, shown.length)).toBe(DEFAULT_TAB_LAYOUT);
  });
});

describe("setTabHidden", () => {
  it("hides and shows a tab, keeping the hidden list in the tabs' order", () => {
    const shown: TabLayout = { ...DEFAULT_TAB_LAYOUT, hidden: [] };
    const both = setTabHidden(setTabHidden(shown, "logs", true), "sounds", true);
    expect(both.hidden).toEqual(["sounds", "logs"]);
    expect(setTabHidden(both, "logs", false).hidden).toEqual(["sounds"]);
  });

  it("leaves Settings shown", () => {
    expect(setTabHidden(DEFAULT_TAB_LAYOUT, "settings", true)).toBe(DEFAULT_TAB_LAYOUT);
  });
});

describe("isDefaultTabLayout", () => {
  it("is the default order hiding what the default hides", () => {
    expect(isDefaultTabLayout(DEFAULT_TAB_LAYOUT)).toBe(true);
    expect(isDefaultTabLayout(withPluginsHidden)).toBe(false);
    expect(isDefaultTabLayout({ ...DEFAULT_TAB_LAYOUT, hidden: [] })).toBe(false);
    const shown = visibleTabs(DEFAULT_TAB_LAYOUT);
    expect(isDefaultTabLayout(reorderTabs(DEFAULT_TAB_LAYOUT, shown, 0, 1))).toBe(false);
  });
});
