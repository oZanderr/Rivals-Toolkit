import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { createTauri, deferred, emitEvent, installTauri, type TauriMock } from "@/test/tauri";

vi.mock("@tauri-apps/api/core", async () => {
  const { invokeProxy } = await import("@/test/tauri");
  return { invoke: invokeProxy };
});
vi.mock("@tauri-apps/api/event", async () => {
  const { eventStub } = await import("@/test/tauri");
  return eventStub;
});

const { GameSearch } = await import("./GameSearchDialog");

const GAME = "C:\\Game";
const BASE_HIT = {
  package: "Marvel/Content/Marvel/UI/Blueprints/Setting/WBP_Shortcut_Corona.uasset",
  export: "ExecuteUbergraph_WBP_Shortcut_Corona",
  export_index: 10,
  offset: 0x7a,
  kind: "call",
  term: "/Script/Engine.MaterialInstanceDynamic:SetScalarParameterValue",
  line: "Material->SetScalarParameterValue('Selected', 0.0f)",
  container: "C:\\Game\\MarvelGame\\Marvel\\Content\\Paks\\pakchunk3-Windows.utoc",
};
const MOD_HIT = {
  ...BASE_HIT,
  package: "Marvel/Content/Marvel/ProjectGalacta/UI/WBP_Galacta.uasset",
  export: "ExecuteUbergraph_WBP_Galacta",
  export_index: 2,
  line: "Glow->SetScalarParameterValue('Strength', 1.0f)",
  container: "C:\\Game\\MarvelGame\\Marvel\\Content\\Paks\\~mods\\!ProjectGalacta_9999999_P.utoc",
  in_mod: "!ProjectGalacta_9999999_P",
};

function result(extra: Record<string, unknown> = {}) {
  return {
    hits: [BASE_HIT, MOD_HIT],
    unreadable: [],
    listed: 554734,
    searched: 5497,
    truncated: false,
    cancelled: false,
    ...extra,
  };
}

let mock: TauriMock;
const opened = vi.fn();

function mount() {
  render(<GameSearch gamePath={GAME} onOpenHit={opened} />);
}

/** Opens the dialog and searches for `text`. */
async function searchFor(user: ReturnType<typeof userEvent.setup>, text: string) {
  await user.click(screen.getByRole("button", { name: /Search Game/ }));
  await user.type(await screen.findByLabelText("Search for"), `${text}{Enter}`);
}

beforeEach(() => {
  opened.mockReset();
  mock = createTauri().on("search_game", () => result());
  installTauri(mock);
});

describe("GameSearch", () => {
  it("lists hits by package, with the mod that ships them", async () => {
    mount();
    const user = userEvent.setup();
    await searchFor(user, "SetScalarParameterValue");
    expect(await screen.findByText("in !ProjectGalacta_9999999_P")).toBeTruthy();
    expect(
      screen.getByText("Marvel/UI/Blueprints/Setting/WBP_Shortcut_Corona.uasset")
    ).toBeTruthy();
    expect(
      screen.getByText("Searched 5497 of 554734 packages, the ones holding functions.")
    ).toBeTruthy();
    expect(mock.callsTo("search_game")).toEqual([
      { gameRoot: GAME, query: "SetScalarParameterValue", values: false, filter: null },
    ]);
  });

  it("sends stored values and the path filter only when asked", async () => {
    mount();
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: /Search Game/ }));
    await user.click(await screen.findByRole("switch", { name: "Stored values" }));
    await user.type(screen.getByLabelText("Only paths containing"), "Data/DataTable");
    await user.type(screen.getByLabelText("Search for"), "Hulk{Enter}");
    await waitFor(() =>
      expect(mock.callsTo("search_game")).toEqual([
        { gameRoot: GAME, query: "Hulk", values: true, filter: "Data/DataTable" },
      ])
    );
  });

  it("moves the bar with progress events, and cancel reaches the backend", async () => {
    const pending = deferred<unknown>();
    mock.on("search_game", () => pending.promise).on("cancel_game_search", () => null);
    mount();
    const user = userEvent.setup();
    await searchFor(user, "SetScalarParameterValue");
    act(() => emitEvent("game-search-progress", { phase: "scripts", current: 100, total: 5497 }));
    expect(await screen.findByText("Searching scripts")).toBeTruthy();
    expect(screen.getByText("100/5497")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    expect(mock.callsTo("cancel_game_search")).toHaveLength(1);
    await act(async () => pending.resolve(result({ hits: [BASE_HIT], cancelled: true })));
    expect(
      await screen.findByText("Cancelled: these are the places found before it stopped.")
    ).toBeTruthy();
  });

  it("hands over the container a hit was found in", async () => {
    mount();
    const user = userEvent.setup();
    await searchFor(user, "SetScalarParameterValue");
    await user.click(await screen.findByText(MOD_HIT.line));
    expect(opened).toHaveBeenCalledWith(MOD_HIT);
  });
});
