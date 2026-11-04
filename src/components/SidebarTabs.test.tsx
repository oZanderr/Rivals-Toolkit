import { useState } from "react";

import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { SidebarTabs } from "./SidebarTabs";

import { DEFAULT_TAB_LAYOUT, tabInfo, visibleTabs, type Tab, type TabLayout } from "@/lib/tabs";
import { render, screen } from "@/test/render";

const onSelect = vi.fn<(tab: Tab) => void>();
const onCustomize = vi.fn<() => void>();

function Sidebar({ initial = DEFAULT_TAB_LAYOUT }: { initial?: TabLayout }) {
  const [layout, setLayout] = useState(initial);
  const [active, setActive] = useState<Tab>("mod-tools");
  return (
    <SidebarTabs
      layout={layout}
      activeTab={active}
      onSelect={(tab) => {
        setActive(tab);
        onSelect(tab);
      }}
      onLayoutChange={setLayout}
      onCustomize={onCustomize}
    />
  );
}

function labels() {
  return screen.getAllByRole("button").map((b) => b.textContent);
}

/** Opens the context menu of the tab labelled `name`. */
async function menuOf(user: ReturnType<typeof userEvent.setup>, name: string) {
  await user.pointer({ keys: "[MouseRight]", target: screen.getByRole("button", { name }) });
  return screen.findByRole("menu");
}

beforeEach(() => {
  onSelect.mockReset();
  onCustomize.mockReset();
  // Rows 40 px apart and 36 tall, so a drag has something to be measured against.
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (
    this: HTMLElement
  ) {
    const row = this.closest("li");
    const at = row?.parentElement ? Array.from(row.parentElement.children).indexOf(row) : 0;
    return new DOMRect(0, at * 40, 160, 36);
  });
});

describe("SidebarTabs", () => {
  it("lists the shown tabs in order and marks the open one", () => {
    render(<Sidebar initial={{ ...DEFAULT_TAB_LAYOUT, hidden: ["plugins"] }} />);
    expect(labels()).toEqual([
      "Mods",
      "Sounds",
      "Config Tweaks",
      "INI Editor",
      "Asset Manager",
      "Logs",
      "Settings",
    ]);
    expect(screen.getByRole("button", { name: "Mods" }).getAttribute("aria-current")).toBe("page");
  });

  it("opens a tab on click", async () => {
    const user = userEvent.setup();
    render(<Sidebar />);
    await user.click(screen.getByRole("button", { name: "Logs" }));
    expect(onSelect).toHaveBeenCalledWith("logs");
    expect(screen.getByRole("button", { name: "Logs" }).getAttribute("aria-current")).toBe("page");
  });

  it("moves a tab from its menu, and cannot move the first one up", async () => {
    const user = userEvent.setup();
    render(<Sidebar />);
    await menuOf(user, "Mods");
    expect(screen.getByRole("menuitem", { name: "Move up" }).hasAttribute("data-disabled")).toBe(
      true
    );
    await user.click(screen.getByRole("menuitem", { name: "Move down" }));
    expect(labels().slice(0, 3)).toEqual(["Plugins", "Mods", "Sounds"]);
  });

  it("hides a tab from its menu, but offers no hiding for Settings", async () => {
    const user = userEvent.setup();
    render(<Sidebar />);
    await menuOf(user, "Sounds");
    await user.click(screen.getByRole("menuitem", { name: "Hide tab" }));
    expect(labels()).not.toContain("Sounds");

    await menuOf(user, "Settings");
    expect(screen.queryByRole("menuitem", { name: "Hide tab" })).toBeNull();
    await user.click(screen.getByRole("menuitem", { name: "Customize tabs…" }));
    expect(onCustomize).toHaveBeenCalledOnce();
  });

  it("reorders a tab dragged past others, without opening it", async () => {
    const user = userEvent.setup();
    render(<Sidebar />);
    await user.pointer([
      {
        keys: "[MouseLeft>]",
        target: screen.getByRole("button", { name: "Mods" }),
        coords: { clientY: 10 },
      },
      { coords: { clientY: 95 } },
      { keys: "[/MouseLeft]" },
    ]);
    expect(labels().slice(0, 3)).toEqual(["Plugins", "Sounds", "Mods"]);
    expect(onSelect).not.toHaveBeenCalled();
  });

  it("takes a press that barely moves as a click", async () => {
    const user = userEvent.setup();
    render(<Sidebar />);
    await user.pointer([
      {
        keys: "[MouseLeft>]",
        target: screen.getByRole("button", { name: "Sounds" }),
        coords: { clientY: 90 },
      },
      { coords: { clientY: 92 } },
      { keys: "[/MouseLeft]" },
    ]);
    expect(onSelect).toHaveBeenCalledWith("sounds");
    expect(labels()).toEqual(visibleTabs(DEFAULT_TAB_LAYOUT).map((id) => tabInfo(id).label));
  });

  it("moves nothing when Escape cancels a drag", async () => {
    const user = userEvent.setup();
    render(<Sidebar />);
    await user.pointer([
      {
        keys: "[MouseLeft>]",
        target: screen.getByRole("button", { name: "Mods" }),
        coords: { clientY: 10 },
      },
      { coords: { clientY: 95 } },
    ]);
    await user.keyboard("{Escape}");
    await user.pointer({ keys: "[/MouseLeft]" });
    expect(labels()[0]).toBe("Mods");
    expect(onSelect).not.toHaveBeenCalled();
  });
});
