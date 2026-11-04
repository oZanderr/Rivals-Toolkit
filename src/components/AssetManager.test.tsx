import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { act, render, screen, waitFor } from "@/test/render";
import {
  createTauri,
  deferred,
  dialogStub,
  emitEvent,
  installTauri,
  type TauriMock,
} from "@/test/tauri";

vi.mock("@tauri-apps/api/core", async () => {
  const { invokeProxy } = await import("@/test/tauri");
  return { invoke: invokeProxy };
});
vi.mock("@tauri-apps/api/event", async () => {
  const { eventStub } = await import("@/test/tauri");
  return eventStub;
});
vi.mock("@tauri-apps/api/window", async () => {
  const { windowStub } = await import("@/test/tauri");
  return windowStub;
});
vi.mock("@tauri-apps/plugin-dialog", async () => {
  const { dialogStub } = await import("@/test/tauri");
  return dialogStub;
});
vi.mock("@tauri-apps/plugin-opener", () => ({ revealItemInDir: vi.fn() }));

const { AssetManager } = await import("./AssetManager");

const GAME = "C:\\Game";
const PAKS = `${GAME}\\MarvelGame\\Marvel\\Content\\Paks`;
const GAME_PAK = `${PAKS}\\pakchunk0-Windows.pak`;
const MOD_PAK = `${PAKS}\\~mods\\Skin_9999999_P.pak`;

function pak(path: string) {
  return {
    path,
    has_utoc: true,
    has_ucas: true,
    obfuscated: false,
    optional_pak: null,
    optional_has_utoc: false,
    optional_has_ucas: false,
  };
}

let mock: TauriMock;

function mount(gameRunning = false) {
  render(<AssetManager gamePath={GAME} gameRunning={gameRunning} />);
}

/** Opens the context menu of the pak listed as `name`. */
async function menuOf(user: ReturnType<typeof userEvent.setup>, name: string) {
  await user.pointer({ keys: "[MouseRight]", target: await screen.findByText(name) });
  return screen.findByRole("menu");
}

beforeEach(() => {
  mock = createTauri()
    .on("list_pak_files_info", () => [pak(GAME_PAK), pak(MOD_PAK)])
    .on("list_known_heroes", () => [])
    .on("cancel_texture_batch", () => null)
    .on("get_should_block_for_game", () => false);
  installTauri(mock);
  dialogStub.open.mockReset();
});

describe("AssetManager texture batches", () => {
  it("exports a container's textures with their progress, and cancels on request", async () => {
    const running = deferred<unknown>();
    mock.on("export_textures", () => running.promise);
    dialogStub.open.mockResolvedValueOnce("C:\\Out");
    mount();
    const user = userEvent.setup();
    await menuOf(user, "pakchunk0-Windows.pak");
    // A submenu opened by the pointer closes as it leaves the trigger where nothing has a size, so
    // it is opened and chosen from by the keyboard.
    act(() => screen.getByRole("menuitem", { name: "Export Textures" }).focus());
    await user.keyboard("{ArrowRight}");
    await screen.findByRole("menuitem", { name: "As DDS, Blocks and Mips as Stored…" });
    await user.keyboard("{ArrowDown}{ArrowDown}{Enter}");

    await waitFor(() => expect(mock.callsTo("export_textures")).toHaveLength(1));
    expect(mock.callsTo("export_textures")[0]).toEqual({
      gameRoot: GAME,
      container: `${PAKS}\\pakchunk0-Windows.utoc`,
      filters: [],
      out: "C:\\Out",
      kind: "dds",
      mip: null,
    });
    act(() => emitEvent("texture-batch-progress", { phase: "textures", current: 3, total: 10 }));
    expect(await screen.findByText("3/10")).toBeTruthy();

    await user.click(screen.getByRole("button", { name: "Cancel" }));
    expect(mock.callsTo("cancel_texture_batch")).toHaveLength(1);
    await act(async () =>
      running.resolve({
        packages: 10,
        saved: [{ path: "C:\\Out\\A.png" }, { path: "C:\\Out\\B.png" }, { path: "C:\\Out\\C.png" }],
        failed: [],
        skipped: [],
        cancelled: true,
      })
    );
    expect(
      await screen.findByText("Cancelled after saving 3 texture(s) of 10 package(s) to C:\\Out")
    ).toBeTruthy();
  });

  it("replaces a mod's textures from a folder into the mod itself, listing what failed", async () => {
    mock.on("import_textures", () => ({
      images: 3,
      packages: 2,
      given: [{}, {}],
      failed: [{ entry: "notes", image: "C:\\In\\notes.png", reason: "no package is named so" }],
      written: `${PAKS}\\~mods\\Skin_9999999_P.utoc`,
      cancelled: false,
    }));
    dialogStub.open.mockResolvedValueOnce("C:\\In");
    mount();
    const user = userEvent.setup();
    await menuOf(user, "~mods/Skin_9999999_P.pak");
    await user.click(screen.getByRole("menuitem", { name: "Replace Textures from Folder…" }));

    await waitFor(() => expect(mock.callsTo("import_textures")).toHaveLength(1));
    expect(mock.callsTo("import_textures")[0]).toEqual(
      expect.objectContaining({
        container: `${PAKS}\\~mods\\Skin_9999999_P.utoc`,
        images: "C:\\In",
        modName: `${PAKS}\\~mods\\Skin_9999999_P.utoc`,
        replace: false,
      })
    );
    expect(
      await screen.findByText(
        "Gave 2 texture(s) in 2 package(s) new pixels in Skin_9999999_P.utoc; 1 failed (hover for why)"
      )
    ).toBeTruthy();
  });

  it("asks which mod a game container's textures go into", async () => {
    mock
      .on("get_asset_mod_name", () => "AssetEdits")
      .on("import_textures", () => ({
        images: 1,
        packages: 1,
        given: [{}],
        failed: [],
        written: `${PAKS}\\~mods\\Mine_9999999_P.utoc`,
        cancelled: false,
      }));
    dialogStub.open.mockResolvedValueOnce("C:\\In");
    mount();
    const user = userEvent.setup();
    await menuOf(user, "pakchunk0-Windows.pak");
    await user.click(screen.getByRole("menuitem", { name: "Replace Textures from Folder…" }));
    const name = await screen.findByLabelText("Mod");
    expect((name as HTMLInputElement).value).toBe("AssetEdits");
    await user.clear(name);
    await user.type(name, "Mine");
    await user.click(screen.getByRole("button", { name: "Replace" }));

    await waitFor(() => expect(mock.callsTo("import_textures")).toHaveLength(1));
    expect(mock.callsTo("import_textures")[0]).toEqual(
      expect.objectContaining({ container: `${PAKS}\\pakchunk0-Windows.utoc`, modName: "Mine" })
    );
  });
});

const SECOND_MOD = `${PAKS}\\~mods\\Other_9999999_P.pak`;

function repacked() {
  return { container_name: "x", assets_in: 1, assets_out: 1, carried_pak_entries: 0 };
}

/** Starts a Repack All over both mods with the dialog's defaults. */
async function repackAll(user: ReturnType<typeof userEvent.setup>) {
  await screen.findByText("~mods/Skin_9999999_P.pak");
  await user.click(screen.getByRole("button", { name: /Repack All/ }));
  await user.click(await screen.findByRole("button", { name: "Repack" }));
}

describe("AssetManager Repack All", () => {
  beforeEach(() => {
    mock.on("list_pak_files_info", () => [pak(GAME_PAK), pak(MOD_PAK), pak(SECOND_MOD)]);
  });

  it("does not start while the game is running or still closing", async () => {
    mock.on("repack_mod_in_place", () => repacked());
    mount(true);
    const user = userEvent.setup();
    await screen.findByText("~mods/Skin_9999999_P.pak");
    await user.click(screen.getByRole("button", { name: /Repack All/ }));
    expect(await screen.findByText(/Marvel Rivals is running, or still closing/)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Repack" })).toBeNull();
    expect(mock.callsTo("repack_mod_in_place")).toHaveLength(0);
  });

  it("stops when the game starts partway, rather than failing every mod left", async () => {
    let running = false;
    mock
      .on("get_should_block_for_game", () => running)
      .on("repack_mod_in_place", () => {
        running = true;
        return repacked();
      });
    mount();
    const user = userEvent.setup();
    await repackAll(user);
    expect(
      await screen.findByText(
        "Stopped after repacking 1 of 2: Marvel Rivals is running. Repack the rest once it has closed."
      )
    ).toBeTruthy();
    expect(mock.callsTo("repack_mod_in_place")).toHaveLength(1);
  });

  it("stops when the game starts while a mod repacks, leaving that mod as it was", async () => {
    let running = false;
    mock
      .on("get_should_block_for_game", () => running)
      .on("cancel_repack_iostore", () => null)
      .on("repack_mod_in_place", () => {
        // The backend checks once more before it swaps the rebuilt mod in.
        running = true;
        throw "Marvel Rivals is running! Close the game before modifying mods.";
      });
    mount();
    const user = userEvent.setup();
    await repackAll(user);
    expect(
      await screen.findByText(
        "Stopped after repacking 0 of 2: Marvel Rivals started while repacking Skin_9999999_P.pak, which was left as it was. Repack the rest once it has closed."
      )
    ).toBeTruthy();
    expect(mock.callsTo("repack_mod_in_place")).toHaveLength(1);
  });

  it("cancels the whole batch, not just the mod being repacked", async () => {
    const first = deferred<unknown>();
    mock
      .on("repack_mod_in_place", () => first.promise)
      .on("cancel_legacy_extraction", () => null)
      .on("cancel_repack_iostore", () => null);
    mount();
    const user = userEvent.setup();
    await repackAll(user);
    await waitFor(() => expect(mock.callsTo("repack_mod_in_place")).toHaveLength(1));
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    await act(async () => first.reject("Repack cancelled"));
    expect(await screen.findByText(/^Cancelled after repacking 0 of 2/)).toBeTruthy();
    expect(mock.callsTo("repack_mod_in_place")).toHaveLength(1);
  });
});
