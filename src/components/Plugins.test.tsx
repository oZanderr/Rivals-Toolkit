import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { createTauri, installTauri, type TauriMock } from "@/test/tauri";

type DragHandler = (event: { payload: { type: string; paths?: string[] } }) => unknown;

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
    onFocusChanged: async () => () => {},
  }),
}));
vi.mock("@tauri-apps/plugin-dialog", async () => {
  const { dialogStub } = await import("@/test/tauri");
  return dialogStub;
});
vi.mock("@tauri-apps/plugin-opener", () => ({
  openPath: async () => {},
  revealItemInDir: async () => {},
}));

const { Plugins } = await import("./Plugins");

const GAME = "C:\\Game";
const WIN64 = `${GAME}\\MarvelGame\\Marvel\\Binaries\\Win64`;
const PLUGINS = `${WIN64}\\plugins`;

function plugin(fileName: string, extra: Record<string, unknown> = {}) {
  return {
    name: fileName.replace(/\.asi(\.disabled)?$/i, ""),
    file_name: fileName,
    path: `${PLUGINS}\\${fileName}`,
    location: "plugins",
    enabled: !fileName.endsWith(".disabled"),
    size: 9728,
    modified: null,
    companions: [],
    shadowed_by: null,
    bypass: null,
    ...extra,
  };
}

const STATUS = {
  win64_dir: WIN64,
  plugins_dir: PLUGINS,
  win64_exists: true,
  plugins_dir_exists: true,
  loaders: [
    {
      file_name: "dsound.dll",
      path: `${WIN64}\\dsound.dll`,
      proxy_for: "dsound.dll",
      enabled: true,
      size: 216576,
      modified: null,
      kind: "minimal_asi_loader",
      description: null,
      companions: [],
      bypass: "loader",
    },
  ],
  plugins: [
    plugin("MarvelRivalsUTOCSignatureBypass.asi", { bypass: "payload" }),
    plugin("PigeonPass.asi", {
      companions: [{ file_name: "PigeonPass.ini", path: `${PLUGINS}\\PigeonPass.ini`, size: 3270 }],
    }),
    plugin("RivalsDbg.asi.disabled"),
    plugin("UTOCSigga.asi"),
  ],
};

let tauri: TauriMock;

beforeEach(() => {
  dragHandlers.length = 0;
  tauri = createTauri()
    .on("get_plugins_status", () => STATUS)
    .on("set_plugin_enabled", () => null);
  installTauri(tauri);
});

async function mount(isActive = true) {
  const user = userEvent.setup();
  const view = render(<Plugins gamePath={GAME} isActive={isActive} gameRunning={false} />);
  return { user, view };
}

describe("Plugins", () => {
  it("lists the loader and the plugins with their settings files", async () => {
    await mount();

    expect(await screen.findByText("PigeonPass")).toBeTruthy();
    expect(screen.getByText("Minimal ASI Loader")).toBeTruthy();
    expect(screen.getByText("PigeonPass.ini")).toBeTruthy();
    expect(screen.getAllByText("Signature bypass")).toHaveLength(2);
    expect(screen.getByText("(3/4 active)")).toBeTruthy();
  });

  it("switches a plugin off by its path", async () => {
    const { user } = await mount();
    await user.click(await screen.findByRole("switch", { name: "Switch UTOCSigga.asi off" }));

    await waitFor(() => expect(tauri.callsTo("set_plugin_enabled")).toHaveLength(1));
    expect(tauri.callsTo("set_plugin_enabled")[0]).toEqual({
      gameRoot: GAME,
      path: `${PLUGINS}\\UTOCSigga.asi`,
      enabled: false,
    });
  });

  it("asks before switching off the signature bypass", async () => {
    const { user } = await mount();
    await user.click(
      await screen.findByRole("switch", {
        name: "Switch MarvelRivalsUTOCSignatureBypass.asi off",
      })
    );

    expect(await screen.findByText(/mods in ~mods stop loading/)).toBeTruthy();
    expect(tauri.callsTo("set_plugin_enabled")).toHaveLength(0);
    await user.click(screen.getByRole("button", { name: "Switch Off" }));
    await waitFor(() => expect(tauri.callsTo("set_plugin_enabled")).toHaveLength(1));
  });

  it("ignores drops while another tab is showing", async () => {
    await mount(false);
    await waitFor(() => expect(dragHandlers).toHaveLength(1));

    await dragHandlers[0]({ payload: { type: "drop", paths: ["D:\\Downloads\\New.asi"] } });

    expect(tauri.callsTo("install_plugins")).toHaveLength(0);
  });

  it("asks to replace a plugin already installed, then installs with replace", async () => {
    tauri.on("install_plugins", (args) =>
      args.replace
        ? [
            {
              source: "D:\\Downloads\\PigeonPass.asi",
              installed: ["PigeonPass.asi"],
              kept: ["PigeonPass.ini"],
              existing: [],
              skipped: [],
              error: null,
            },
          ]
        : [
            {
              source: "D:\\Downloads\\PigeonPass.asi",
              installed: [],
              kept: [],
              existing: ["PigeonPass.asi"],
              skipped: [],
              error: null,
            },
          ]
    );
    const { user } = await mount();
    await screen.findByText("PigeonPass");
    await waitFor(() => expect(dragHandlers).toHaveLength(1));

    await dragHandlers[0]({
      payload: { type: "drop", paths: ["D:\\Downloads\\PigeonPass.asi"] },
    });
    await user.click(await screen.findByRole("button", { name: "Replace" }));

    await waitFor(() => expect(tauri.callsTo("install_plugins")).toHaveLength(2));
    expect(tauri.callsTo("install_plugins")[1]).toEqual({
      gameRoot: GAME,
      sourcePaths: ["D:\\Downloads\\PigeonPass.asi"],
      replace: true,
    });
    expect(await screen.findByText(/kept your PigeonPass.ini/)).toBeTruthy();
  });

  it("hands a plugin's .ini to the INI Editor", async () => {
    const user = userEvent.setup();
    const onOpenInEditor = vi.fn();
    render(
      <Plugins gamePath={GAME} isActive gameRunning={false} onOpenInEditor={onOpenInEditor} />
    );

    await user.click(await screen.findByText("PigeonPass.ini"));

    expect(onOpenInEditor).toHaveBeenCalledWith(`${PLUGINS}\\PigeonPass.ini`);
  });
});
