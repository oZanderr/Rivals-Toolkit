import { useState } from "react";

import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { TabsSettings } from "./TabsSettings";

import { DEFAULT_TAB_LAYOUT, type TabLayout } from "@/lib/tabs";
import { render, screen } from "@/test/render";

function Section({ initial = DEFAULT_TAB_LAYOUT }: { initial?: TabLayout }) {
  const [layout, setLayout] = useState(initial);
  return <TabsSettings layout={layout} onChange={setLayout} />;
}

function shownSwitch(label: string) {
  return screen.getByRole("switch", { name: `Show ${label}` });
}

function order() {
  return screen
    .getAllByRole("switch")
    .map((s) => s.getAttribute("aria-label")?.replace("Show ", ""));
}

function resetButton() {
  return screen.getByRole("button", { name: "Reset to default" }) as HTMLButtonElement;
}

describe("TabsSettings", () => {
  it("hides a tab with its switch, and Reset brings the default back", async () => {
    const user = userEvent.setup();
    render(<Section />);
    expect(resetButton().disabled).toBe(true);

    await user.click(shownSwitch("Logs"));
    expect(shownSwitch("Logs").getAttribute("aria-checked")).toBe("false");
    expect(resetButton().disabled).toBe(false);

    await user.click(resetButton());
    expect(shownSwitch("Logs").getAttribute("aria-checked")).toBe("true");
    expect(resetButton().disabled).toBe(true);
  });

  it("keeps Settings shown", () => {
    render(<Section />);
    expect((shownSwitch("Settings") as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByText("Always shown")).toBeTruthy();
  });

  it("lists hidden tabs in their place, and moves a row with Alt+Down", async () => {
    const user = userEvent.setup();
    render(<Section initial={{ ...DEFAULT_TAB_LAYOUT, hidden: ["plugins"] }} />);
    expect(order().slice(0, 3)).toEqual(["Mods", "Plugins", "Sounds"]);

    screen.getByRole("button", { name: "Reorder Mods" }).focus();
    await user.keyboard("{Alt>}{ArrowDown}{/Alt}");
    expect(order().slice(0, 3)).toEqual(["Plugins", "Mods", "Sounds"]);
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Reorder Mods" }));
  });
});
