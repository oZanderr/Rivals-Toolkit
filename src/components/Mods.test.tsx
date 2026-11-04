import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { act, render, screen, waitFor } from "@/test/render";
import { createTauri, installTauri, type TauriMock } from "@/test/tauri";

type DragHandler = (event: {
  payload: { type: string; paths?: string[]; position?: { x: number; y: number } };
}) => unknown;

const { dragHandlers } = vi.hoisted(() => ({ dragHandlers: [] as DragHandler[] }));

vi.mock("@tauri-apps/api/core", async () => {
  const { invokeProxy } = await import("@/test/tauri");
  return { invoke: invokeProxy };
});
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    onDragDropEvent: async (handler: DragHandler) => {
      dragHandlers.push(handler);
      return () => {};
    },
  }),
}));
vi.mock("@tauri-apps/plugin-dialog", async () => {
  const { dialogStub } = await import("@/test/tauri");
  return dialogStub;
});
vi.mock("@tauri-apps/plugin-opener", () => ({ revealItemInDir: async () => {} }));

const { Mods } = await import("./Mods");

const GAME = "C:\\Game";
const MODS = `${GAME}\\MarvelGame\\Marvel\\Content\\Paks\\~mods`;
const LOKI = "Skins/Loki_9999999_P.pak";
const RED = "Skins/Venom/Red_9999999_P.pak";
const BLUE = "Skins/Venom/Blue_9999999_P.pak.disabled";
const TRY = "Testing/Try_9999999_P.pak.disabled";
const CLEAN = "Clean_UI_9999999_P.pak";

function mod(fullName: string) {
  return {
    full_name: fullName,
    display_name: fullName.replace(/\.disabled$/, ""),
    enabled: !fullName.endsWith(".disabled"),
    has_companions: true,
    size_bytes: 1024,
    kind: "IoStore",
    heroes: [],
  };
}

function status(folders: string[] | null = ["Empty", "Skins", "Skins/Venom", "Testing"]) {
  const names = folders ? [LOKI, RED, BLUE, TRY, CLEAN] : [CLEAN, "Other_9999999_P.pak"];
  return {
    mods_folder_exists: true,
    mods_folder_path: MODS,
    sig_bypass_kind: "installed",
    mod_entries: names.map(mod),
    conflicts_resolved: 0,
    folders,
  };
}

let tauri: TauriMock;

function mockBackend(
  { folders, collapsed = [] as string[] } = {} as {
    folders?: string[] | null;
    collapsed?: string[];
  }
) {
  tauri = createTauri()
    .on("get_mods_status", () => status(folders))
    .on("text_edited_mods", () => [])
    .on("get_mod_conflict_check_enabled", () => false)
    .on("list_mod_profiles", () => [])
    .on("list_known_heroes", () => [])
    .on("should_auto_sync_character_data", () => false)
    .on("get_show_hero_icons", () => false)
    .on("get_collapsed_mod_folders", () => collapsed)
    .on("set_collapsed_mod_folders", () => null)
    .on("toggle_mods_enabled", ({ fullNames }) => ({
      successes: (fullNames as string[]).length,
      failures: [],
    }))
    .on("move_mods", ({ fullNames }) => ({
      successes: (fullNames as string[]).length,
      failures: [],
    }))
    .on("install_mods", () => [])
    .on("create_mod_folder", ({ path }) => path)
    .on("delete_mod_folder", () => ({ deleted: 1, failures: [], kept: [] }));
  installTauri(tauri);
}

async function mount() {
  const user = userEvent.setup();
  render(<Mods gamePath={GAME} isActive gameRunning={false} />);
  await screen.findByText("Loki");
  return user;
}

function checkboxes() {
  return screen.getAllByRole("checkbox").map((box) => box.getAttribute("aria-label"));
}

const folderRow = (path: string) =>
  screen.getByRole("button", { name: new RegExp(`^(Collapse|Expand) ${path}$`) }).closest("li");

beforeEach(() => {
  dragHandlers.length = 0;
});

afterEach(() => {
  Reflect.deleteProperty(document, "elementFromPoint");
});

describe("Mods folder tree", () => {
  it("lists folders before mods at each level, each mod under its own name", async () => {
    mockBackend();
    await mount();
    expect(checkboxes()).toEqual([
      "Select all mods",
      "Select every mod in Empty",
      "Select every mod in Skins",
      "Select every mod in Skins/Venom",
      `Select ${RED}`,
      `Select ${BLUE.replace(".disabled", "")}`,
      `Select ${LOKI}`,
      "Select every mod in Testing",
      `Select ${TRY.replace(".disabled", "")}`,
      `Select ${CLEAN}`,
    ]);
    expect(screen.queryByText("Skins/Loki")).toBeNull();
    expect(folderRow("Skins")?.textContent).toContain("2/3 on");
    expect(folderRow("Skins/Venom")?.textContent).toContain("1/2 on");
    expect(folderRow("Empty")?.textContent).toContain("empty");
  });

  it("keeps a folder collapsed as stored, and saves one collapsed by hand", async () => {
    mockBackend({ collapsed: ["Testing"] });
    const user = await mount();
    await waitFor(() => expect(screen.queryByText("Try")).toBeNull());

    await user.click(screen.getByRole("button", { name: "Collapse Skins" }));
    expect(screen.queryByText("Loki")).toBeNull();
    expect(screen.queryByText("Red")).toBeNull();
    expect(tauri.callsTo("set_collapsed_mod_folders")).toEqual([{ folders: ["Skins", "Testing"] }]);
  });

  it("switches off every mod in a folder and its subfolders with its switch", async () => {
    mockBackend();
    const user = await mount();
    await user.click(screen.getByRole("switch", { name: "Switch every mod in Skins off" }));
    await waitFor(() => expect(tauri.callsTo("toggle_mods_enabled")).toHaveLength(1));
    expect(tauri.callsTo("toggle_mods_enabled")[0]).toEqual({
      modsFolder: MODS,
      fullNames: [LOKI, RED],
      enabled: false,
    });
  });

  it("moves a mod into another folder from its menu", async () => {
    mockBackend();
    const user = await mount();
    await user.pointer({ keys: "[MouseRight]", target: screen.getByText("Loki") });
    await screen.findByRole("menu");
    act(() => screen.getByRole("menuitem", { name: "Move to" }).focus());
    await user.keyboard("{ArrowRight}");
    const testing = await screen.findByRole("menuitem", { name: "Testing" });
    expect(screen.getByRole("menuitem", { name: "Skins" }).hasAttribute("data-disabled")).toBe(
      true
    );
    act(() => testing.focus());
    await user.keyboard("{Enter}");
    await waitFor(() => expect(tauri.callsTo("move_mods")).toHaveLength(1));
    expect(tauri.callsTo("move_mods")[0]).toEqual({
      modsFolder: MODS,
      fullNames: [LOKI],
      to: "Testing",
    });
  });

  it("installs a drop into the folder under it, and at the top elsewhere", async () => {
    mockBackend();
    await mount();
    let under: Element | null = folderRow("Skins");
    Object.defineProperty(document, "elementFromPoint", {
      configurable: true,
      value: () => under,
    });
    const drag = async (type: string) => {
      await act(async () => {
        for (const handler of dragHandlers) {
          await handler({
            payload: {
              type,
              paths: ["C:\\Downloads\\New_9999999_P.pak"],
              position: { x: 5, y: 5 },
            },
          });
        }
      });
    };
    await drag("enter");
    await drag("over");
    expect(screen.getByText("Drop to install into Skins")).toBeTruthy();
    await drag("drop");
    await waitFor(() => expect(tauri.callsTo("install_mods")).toHaveLength(1));
    expect(tauri.callsTo("install_mods")[0]).toEqual({
      modsFolder: MODS,
      sourcePaths: ["C:\\Downloads\\New_9999999_P.pak"],
      folder: "Skins",
    });

    under = null;
    await drag("enter");
    await drag("drop");
    await waitFor(() => expect(tauri.callsTo("install_mods")).toHaveLength(2));
    expect(tauri.callsTo("install_mods")[1].folder).toBeNull();
  });

  it("makes a new folder from Move to and moves the mod into it", async () => {
    mockBackend();
    const user = await mount();
    await user.pointer({ keys: "[MouseRight]", target: screen.getByText("Loki") });
    await screen.findByRole("menu");
    act(() => screen.getByRole("menuitem", { name: "Move to" }).focus());
    await user.keyboard("{ArrowRight}");
    act(() => screen.getByRole("menuitem", { name: "New folder…" }).focus());
    await user.keyboard("{Enter}");
    const name = await screen.findByRole("textbox", { name: "Folder name" });
    await waitFor(() => expect(document.activeElement).toBe(name));
    await user.keyboard("Art/Loki{Enter}");
    await waitFor(() => expect(tauri.callsTo("move_mods")).toHaveLength(1));
    expect(tauri.callsTo("create_mod_folder")).toEqual([{ modsFolder: MODS, path: "Art/Loki" }]);
    expect(tauri.callsTo("move_mods")[0]).toEqual({
      modsFolder: MODS,
      fullNames: [LOKI],
      to: "Art/Loki",
    });
  });

  it("asks before deleting a folder that holds mods", async () => {
    mockBackend();
    const user = await mount();
    await user.pointer({ keys: "[MouseRight]", target: folderRow("Testing") as Element });
    await user.click(await screen.findByRole("menuitem", { name: "Delete folder" }));
    expect(await screen.findByText("Delete Testing and its 1 mod?")).toBeTruthy();
    expect(tauri.callsTo("delete_mod_folder")).toHaveLength(0);
    await user.click(screen.getByRole("button", { name: "Delete" }));
    await waitFor(() => expect(tauri.callsTo("delete_mod_folder")).toHaveLength(1));
    expect(tauri.callsTo("delete_mod_folder")[0]).toEqual({
      modsFolder: MODS,
      path: "Testing",
      withMods: true,
    });
  });

  it("selects a shift-clicked range in the order the tree shows the mods", async () => {
    mockBackend();
    const user = await mount();
    await user.click(screen.getByText("Red"));
    await user.keyboard("{Shift>}");
    await user.click(screen.getByText("Loki"));
    await user.keyboard("{/Shift}");
    expect(screen.getByText("3 selected")).toBeTruthy();
    const checked = (name: string) =>
      screen.getByRole("checkbox", { name: `Select ${name}` }).getAttribute("aria-checked");
    expect(checked(RED)).toBe("true");
    expect(checked(BLUE.replace(".disabled", ""))).toBe("true");
    expect(checked(LOKI)).toBe("true");
    expect(checked(TRY.replace(".disabled", ""))).toBe("false");
  });

  it("offers no folder controls while subfolders are not scanned", async () => {
    mockBackend({ folders: null });
    const user = userEvent.setup();
    render(<Mods gamePath={GAME} isActive gameRunning={false} />);
    await screen.findByText("Clean_UI");
    expect(screen.queryByRole("button", { name: "New folder" })).toBeNull();
    await user.pointer({ keys: "[MouseRight]", target: screen.getByText("Clean_UI") });
    await screen.findByRole("menu");
    expect(screen.queryByRole("menuitem", { name: "Move to" })).toBeNull();
  });
});
