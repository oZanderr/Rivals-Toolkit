import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

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

function mount() {
  render(<AssetManager gamePath={GAME} gameRunning={false} />);
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
    .on("cancel_texture_batch", () => null);
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
