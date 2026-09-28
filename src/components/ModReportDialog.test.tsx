import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { createTauri, installTauri, type TauriMock } from "@/test/tauri";

vi.mock("@tauri-apps/api/core", async () => {
  const { invokeProxy } = await import("@/test/tauri");
  return { invoke: invokeProxy };
});

const { ModReportDialog } = await import("./ModReportDialog");

const GAME = "C:\\Game";
const MOD = "C:\\Game\\MarvelGame\\Marvel\\Content\\Paks\\~mods\\Old_9999999_P.pak";
const FAULTY = "Marvel/Content/Mods/Old/BP_Thing.uasset";

function report(faults: number) {
  return {
    container: "Old_9999999_P",
    patch_priority: 9999999,
    packages: [
      {
        path: FAULTY,
        kind: "Blueprint",
        overrides_game: false,
        ...(faults > 0 ? { header_faults: faults } : {}),
      },
    ],
    runtime_natives: {},
    python_classes: {},
    files: {},
    save_slots: {},
    urls: {},
  };
}

let mock: TauriMock;

beforeEach(() => {
  let repaired = false;
  mock = createTauri()
    .on("get_mod_report", () => report(repaired ? 0 : 2))
    .on("repair_mod_headers", () => {
      repaired = true;
      return [{ entry: FAULTY, faults: 2 }];
    });
  installTauri(mock);
});

describe("ModReportDialog", () => {
  it("flags headers the game reads past and repairs them", async () => {
    render(
      <ModReportDialog
        gamePath={GAME}
        container={MOD}
        onClose={() => undefined}
        onOpenHit={() => undefined}
      />
    );
    const user = userEvent.setup();
    expect(await screen.findByText(/can crash it/)).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Repair" }));
    expect(await screen.findByText("Repaired 2 headers in 1 package.")).toBeTruthy();
    expect(mock.callsTo("repair_mod_headers")).toEqual([{ gameRoot: GAME, container: MOD }]);
    await waitFor(() => expect(screen.queryByText(/can crash it/)).toBeNull());
    expect(mock.callsTo("get_mod_report")).toHaveLength(2);
  });
});
