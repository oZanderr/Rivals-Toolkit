import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { createTauri, installTauri, type TauriMock } from "@/test/tauri";

vi.mock("@tauri-apps/api/core", async () => {
  const { invokeProxy } = await import("@/test/tauri");
  return { invoke: invokeProxy };
});
vi.mock("@tauri-apps/api/window", async () => {
  const { windowStub } = await import("@/test/tauri");
  return windowStub;
});
vi.mock("@tauri-apps/plugin-dialog", async () => {
  const { dialogStub } = await import("@/test/tauri");
  return dialogStub;
});

const { PakIniEditor } = await import("./PakIniEditor");

const GAME = "C:\\Game";
const PAK = "C:\\Game\\MarvelGame\\Marvel\\Content\\Paks\\~mods\\config_P.pak";

const ENGINE = "../../../Marvel/Config/DefaultEngine.ini";
const DEVICE = "../../../Marvel/Config/DefaultDeviceProfiles.ini";

const listing = { pak_name: "config_P.pak", pak_path: PAK, ini_entries: [ENGINE, DEVICE] };

interface SavedFile {
  entry: string;
  content: string;
  staged_path?: string;
}

let tauri: TauriMock;
/// What the backend would have assembled out of the pieces a large file is handed over in.
let staged: Map<string, string>;

function mockBackend(contents: Record<string, string>) {
  staged = new Map();
  let nextStaged = 0;
  tauri = createTauri()
    .on("scan_mod_paks_any_ini", () => ({ paks: [listing], unreadable: [] }))
    .on("inspect_pak_path_any_ini", () => listing)
    .on("extract_pak_ini", ({ entry }) => contents[entry as string] ?? "")
    .on("extract_game_default_ini", () => null)
    .on("stage_pak_ini_chunk", ({ path, chunk }) => {
      const key = (path as string | null) ?? `staged-${nextStaged++}`;
      staged.set(key, (staged.get(key) ?? "") + (chunk as string));
      return key;
    })
    .on("save_pak_ini", () => "Saved");
  installTauri(tauri);
}

/// The text a save wrote for one entry, however it was handed over.
function savedText(call: number, entry: string): string {
  const files = tauri.callsTo("save_pak_ini")[call].files as SavedFile[];
  const file = files.find((f) => f.entry === entry);
  if (!file) throw new Error(`${entry} was not in save ${call}`);
  return file.staged_path ? (staged.get(file.staged_path) ?? "") : file.content;
}

beforeEach(() => {
  vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
    cb(0);
    return 0;
  });
});

async function mount() {
  const user = userEvent.setup();
  render(<PakIniEditor gamePath={GAME} isActive gameRunning={false} />);
  // A single pak is selected for you, and that reads the index only.
  await screen.findByText("DefaultEngine.ini");
  return user;
}

const tab = (name: string) => screen.getByText(name).closest("button") ?? screen.getByText(name);

/// Put the caret at the very start of the open document and type, so an assertion can look at the
/// front of the file without depending on where a click landed.
async function typeAtStart(user: ReturnType<typeof userEvent.setup>, text: string) {
  const content = document.querySelector(".cm-content");
  expect(content, "the editor should be mounted").not.toBeNull();
  (content as HTMLElement).focus();
  await user.keyboard("{Control>}{Home}{/Control}");
  await user.keyboard(text);
}

async function save(user: ReturnType<typeof userEvent.setup>, nth: number) {
  await user.click(screen.getByRole("button", { name: /^save$/i }));
  await waitFor(() => expect(tauri.callsTo("save_pak_ini").length).toBe(nth));
}

describe("PakIniEditor", () => {
  it("reads a file when its tab is opened, not when the pak is selected", async () => {
    mockBackend({ [ENGINE]: "a=1\nb=2", [DEVICE]: "c=3" });
    const user = await mount();

    // Both tabs come from the pak's index, so they are there before anything is read.
    expect(screen.getByText("DefaultDeviceProfiles.ini")).toBeTruthy();

    await waitFor(() => expect(tauri.callsTo("extract_pak_ini").length).toBe(1));
    expect(tauri.callsTo("extract_pak_ini")[0].entry).toBe(ENGINE);

    await user.click(tab("DefaultDeviceProfiles.ini"));
    await waitFor(() => expect(tauri.callsTo("extract_pak_ini").length).toBe(2));
    expect(tauri.callsTo("extract_pak_ini")[1].entry).toBe(DEVICE);

    // Going back uses what was already read rather than reading it again.
    await user.click(tab("DefaultEngine.ini"));
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(tauri.callsTo("extract_pak_ini").length).toBe(2);
  });

  // Each file is over MAX_CACHED_STATE_BYTES on its own, so once a save clears the dirty flag that
  // exempted them, the next tab switch evicts the editor state and the tab has to be rebuilt from
  // `contents`. That is the path that used to seed it with the text from before the save.
  it("seeds a reopened tab from what was saved, not from what it was opened with", async () => {
    const filler = (marker: string) => `${marker}=1\n`.repeat(600_000);
    mockBackend({ [ENGINE]: filler("engine"), [DEVICE]: filler("device") });
    const user = await mount();
    await waitFor(() => expect(tauri.callsTo("extract_pak_ini").length).toBe(1));

    await typeAtStart(user, "ZZZ");
    await user.click(tab("DefaultDeviceProfiles.ini"));
    await waitFor(() => expect(tauri.callsTo("extract_pak_ini").length).toBe(2));
    await typeAtStart(user, "YYY");

    await save(user, 1);
    expect(savedText(0, ENGINE).startsWith("ZZZengine=1")).toBe(true);
    expect(savedText(0, DEVICE).startsWith("YYYdevice=1")).toBe(true);

    // Reopen the first tab and add to it. What the second save writes says which text the editor
    // was rebuilt from.
    await user.click(tab("DefaultEngine.ini"));
    await waitFor(() => expect(document.querySelector(".cm-content")).not.toBeNull());
    await typeAtStart(user, "Q");
    await save(user, 2);

    expect(savedText(1, ENGINE).startsWith("QZZZengine=1")).toBe(true);
  }, 120_000);
});
