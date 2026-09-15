import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { createTauri, deferred, installTauri, type TauriMock } from "@/test/tauri";

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

const { PakTweaks } = await import("./PakTweaks");

const GAME = "C:\\Game";
const SMALL = "C:\\Game\\MarvelGame\\Marvel\\Content\\Paks\\~mods\\small_P.pak";
const BIG = "C:\\Game\\MarvelGame\\Marvel\\Content\\Paks\\~mods\\big_P.pak";

function pak(name: string, path: string) {
  return {
    pak_name: name,
    pak_path: path,
    has_device_profiles: true,
    has_base_device_profiles: true,
    has_engine_ini: true,
    has_base_engine: true,
    has_windows_engine: true,
    device_profiles_entries: ["Marvel/Config/DefaultDeviceProfiles.ini"],
    base_device_profiles_entries: ["Engine/Config/BaseDeviceProfiles.ini"],
    engine_ini_entries: ["Marvel/Config/DefaultEngine.ini"],
    base_engine_entries: ["Engine/Config/BaseEngine.ini"],
    windows_engine_entries: ["Engine/Config/Windows/WindowsEngine.ini"],
  };
}

// Two toggles is enough to tell "the preset wrote what it names" from "the preset wrote what
// differed", which is the distinction the third test turns on.
const DEFINITIONS = [
  {
    id: "fix_dark_maps",
    label: "Fix Dark Maps",
    category: "Lighting & Color",
    description: "",
    pak_only: true,
    kind: "Toggle",
    key: "r.LightFadeDistance",
    on_value: "1",
    default_enabled: false,
  },
  {
    id: "force_default_material",
    label: "Force Default Material",
    category: "Experimental",
    description: "",
    pak_only: true,
    kind: "Toggle",
    key: "r.debug.ForceDefaultMtl",
    on_value: "1",
    default_enabled: false,
  },
];

const QOL = {
  name: "QOL",
  created_at: 1,
  modified_at: 1,
  settings: [
    // The value a preset carries is whatever the pak read as when it was saved, so a pak already
    // in that state matches the preset exactly. That is the case the preset has to write anyway.
    { id: "fix_dark_maps", enabled: true, value: "1" },
    { id: "force_default_material", enabled: true, value: null },
  ],
};

const ALL_OFF = DEFINITIONS.map((d) => ({ id: d.id, active: false, current_value: null }));

let tauri: TauriMock;

/// Everything a mount needs, with both paks reading as having no tweaks on.
function baseMock() {
  return createTauri()
    .on("get_tweak_definitions", () => DEFINITIONS)
    .on("list_tweak_profiles", () => [QOL])
    .on("scan_mod_paks_for_ini", () => ({
      paks: [pak("small_P.pak", SMALL), pak("big_P.pak", BIG)],
      unreadable: [],
    }))
    .on("detect_pak_tweaks", () => ALL_OFF);
}

beforeEach(() => {
  tauri = baseMock();
  installTauri(tauri);
});

async function mount() {
  const user = userEvent.setup();
  render(<PakTweaks gamePath={GAME} isActive />);
  await screen.findByText("small_P.pak");
  return user;
}

const pakRow = (name: string) => screen.getByText(name).closest("button") ?? screen.getByText(name);

async function choosePreset(user: ReturnType<typeof userEvent.setup>, name: string) {
  await user.click(screen.getByRole("combobox"));
  await user.click(await screen.findByRole("option", { name }));
}

/// The save bar only renders while there are queued changes, so its absence is the assertion for
/// "nothing pending" and its badges name what is queued.
function pendingLabels(): string[] {
  const heading = screen.queryByText(/^Pending \(/);
  if (!heading) return [];
  const bar = heading.parentElement;
  return bar === null
    ? []
    : within(bar)
        .getAllByText(/./)
        .map((el) => el.textContent ?? "")
        .filter((text) => !text.startsWith("Pending ("));
}

describe("PakTweaks presets", () => {
  it("does not carry the chosen preset to another pak", async () => {
    const user = await mount();

    await user.click(pakRow("small_P.pak"));
    await waitFor(() => expect(tauri.callsTo("detect_pak_tweaks").length).toBe(1));
    await choosePreset(user, "QOL");
    expect(pendingLabels().length).toBeGreaterThan(0);

    await user.click(pakRow("big_P.pak"));
    await waitFor(() => expect(tauri.callsTo("detect_pak_tweaks").length).toBe(2));

    // The dropdown named a preset this pak had never had, and re-picking it was not a change for
    // the Select to report, so nothing could apply it.
    expect(screen.getByRole("combobox").textContent ?? "").toMatch(/choose preset/i);

    await choosePreset(user, "QOL");
    expect(pendingLabels().length).toBeGreaterThan(0);

    // Going back restores the pak's own choice rather than clearing it.
    await user.click(pakRow("small_P.pak"));
    await waitFor(() => expect(screen.getByRole("combobox").textContent ?? "").toContain("QOL"));
  });

  it("queues a tweak the pak already reports as on", async () => {
    tauri.on("detect_pak_tweaks", () => [
      { id: "fix_dark_maps", active: true, current_value: "1" },
      { id: "force_default_material", active: false, current_value: null },
    ]);
    const user = await mount();

    await user.click(pakRow("small_P.pak"));
    await waitFor(() => expect(tauri.callsTo("detect_pak_tweaks").length).toBe(1));
    await choosePreset(user, "QOL");

    // Detection reads the merged view across config layers, so a key written into one file alone
    // reports the whole pak as set. Both tweaks have to be queued or the files that lack the key
    // keep lacking it.
    const labels = pendingLabels();
    expect(labels).toContain("Fix Dark Maps");
    expect(labels).toContain("Force Default Material");
  });

  it("discarding one pak leaves another pak's queued changes alone", async () => {
    const slow = deferred<typeof ALL_OFF>();
    tauri.on("detect_pak_tweaks", ({ pakPath }) =>
      pakPath === BIG && tauri.callsTo("detect_pak_tweaks").length > 2 ? slow.promise : ALL_OFF
    );
    const user = await mount();

    await user.click(pakRow("small_P.pak"));
    await waitFor(() => expect(tauri.callsTo("detect_pak_tweaks").length).toBe(1));
    await choosePreset(user, "QOL");
    const queuedOnSmall = pendingLabels();
    expect(queuedOnSmall.length).toBeGreaterThan(0);

    await user.click(pakRow("big_P.pak"));
    await waitFor(() => expect(tauri.callsTo("detect_pak_tweaks").length).toBe(2));
    await choosePreset(user, "QOL");
    expect(pendingLabels().length).toBeGreaterThan(0);

    // Discard re-reads the pak, which on a config mod of this size takes over a second.
    await user.click(screen.getByRole("button", { name: /discard/i }));
    await waitFor(() => expect(tauri.callsTo("detect_pak_tweaks").length).toBe(3));

    // The button looks like it did nothing while that runs, so the other pak gets clicked.
    await user.click(pakRow("small_P.pak"));
    slow.resolve(ALL_OFF);

    // The discard belongs to the pak it was started for, not to whichever one is on screen when
    // it lands.
    await waitFor(() => expect(pendingLabels()).toEqual(queuedOnSmall));
  });
});
