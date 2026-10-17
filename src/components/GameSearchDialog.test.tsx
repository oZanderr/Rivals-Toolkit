import { act, render, screen, waitFor, within } from "@testing-library/react";
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
    found: 2,
    packages: 2,
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
      {
        gameRoot: GAME,
        query: "SetScalarParameterValue",
        values: false,
        filter: null,
        kinds: [],
        wholeWord: false,
        mods: true,
        skipSelf: false,
      },
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
        {
          gameRoot: GAME,
          query: "Hulk",
          values: true,
          filter: "Data/DataTable",
          kinds: [],
          wholeWord: false,
          mods: true,
          skipSelf: false,
        },
      ])
    );
  });

  it("narrows a search to kinds of term, to whole words and to the base game", async () => {
    mount();
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: /Search Game/ }));
    await user.click(await screen.findByRole("button", { name: "Calls" }));
    await user.click(screen.getByRole("button", { name: "Binds" }));
    await user.click(screen.getByRole("button", { name: "Calls" }));
    await user.click(screen.getByRole("button", { name: "Writes" }));
    await user.click(screen.getByRole("switch", { name: "Whole word" }));
    await user.click(screen.getByRole("switch", { name: "Enabled mods" }));
    await user.type(screen.getByLabelText("Search for"), "Delay{Enter}");
    await waitFor(() =>
      expect(mock.callsTo("search_game")).toEqual([
        {
          gameRoot: GAME,
          query: "Delay",
          values: false,
          filter: null,
          kinds: ["bind", "write"],
          wholeWord: true,
          mods: false,
          skipSelf: false,
        },
      ])
    );
  });

  it("leaves out self-references when asked", async () => {
    mount();
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: /Search Game/ }));
    await user.click(await screen.findByRole("switch", { name: "Hide self-references" }));
    await user.type(screen.getByLabelText("Search for"), "10146101{Enter}");
    await waitFor(() =>
      expect(mock.callsTo("search_game")).toEqual([
        expect.objectContaining({ query: "10146101", skipSelf: true }),
      ])
    );
  });

  it("shows a call with its arguments, and a stored value with its row's id", async () => {
    const call = { ...BASE_HIT, call: "Material->SetScalarParameterValue('Selected', 0.0f)" };
    const value = {
      package: "Marvel/Content/Marvel/Data/DataTable/MarvelGameMatchModeTable.uasset",
      export: "MarvelGameMatchModeTable",
      export_index: 0,
      kind: "field",
      term: "InactiveFightJudgeTime",
      line: "600.InactiveFightJudgeTime = 600",
      row: "600",
      field: "InactiveFightJudgeTime",
      id: "GamePlayModeId 600",
    };
    mock.on("search_game", () => result({ hits: [call, value] }));
    mount();
    const user = userEvent.setup();
    await searchFor(user, "Set");
    expect(await screen.findByText(call.call)).toBeTruthy();
    expect(screen.getByText("GamePlayModeId 600")).toBeTruthy();
    expect(screen.getByText(value.line)).toBeTruthy();
  });

  it("groups hits under each ability, its own version before a mode's", async () => {
    const at = (path: string, variant: Record<string, unknown>) => ({
      ...BASE_HIT,
      package: path,
      line: path,
      place: { hero: 1014, ability: 101461, variant },
    });
    mock.on("search_game", () =>
      result({
        hits: [
          at("Marvel/Content/Marvel/AbilitySystem/Level/GameMode/2206/1014/101461/A.uasset", {
            kind: "mode",
            id: 2206,
          }),
          at("Marvel/Content/Marvel/AbilitySystem/1014/101461/B.uasset", { kind: "base" }),
          { ...BASE_HIT, line: "elsewhere" },
        ],
      })
    );
    mount();
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: /Search Game/ }));
    await user.click(
      within(await screen.findByRole("group", { name: "Group by" })).getByRole("button", {
        name: "Ability",
      })
    );
    await user.type(screen.getByLabelText("Search for"), "Turret{Enter}");
    const headings = await screen.findAllByText("1014 / 101461");
    expect(headings).toHaveLength(2);
    expect(screen.getByText("mode 2206")).toBeTruthy();
    const order = [
      "Marvel/AbilitySystem/1014/101461/B.uasset",
      "Marvel/AbilitySystem/Level/GameMode/2206/1014/101461/A.uasset",
      "Other",
    ].map((text) => screen.getByText(text));
    for (let i = 1; i < order.length; i++) {
      expect(
        order[i - 1].compareDocumentPosition(order[i]) & Node.DOCUMENT_POSITION_FOLLOWING
      ).toBeTruthy();
    }
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

  it("says how many places a capped search found beyond the ones it lists", async () => {
    mock.on("search_game", () => result({ found: 8312, packages: 1899, truncated: true }));
    mount();
    const user = userEvent.setup();
    await searchFor(user, "Set");
    expect(await screen.findByText("8312 places in 1899 packages name “Set”")).toBeTruthy();
    expect(
      screen.getByText("Showing the first 2 by path; narrow the search to see the rest.")
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
