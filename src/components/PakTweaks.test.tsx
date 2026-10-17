import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
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

function pak(name: string, path: string, engine = true) {
  return {
    pak_name: name,
    pak_path: path,
    has_device_profiles: true,
    has_base_device_profiles: true,
    has_engine_ini: engine,
    has_base_engine: engine,
    has_windows_engine: engine,
    device_profiles_entries: ["Marvel/Config/DefaultDeviceProfiles.ini"],
    base_device_profiles_entries: ["Engine/Config/BaseDeviceProfiles.ini"],
    engine_ini_entries: engine ? ["Marvel/Config/DefaultEngine.ini"] : [],
    base_engine_entries: engine ? ["Engine/Config/BaseEngine.ini"] : [],
    windows_engine_entries: engine ? ["Engine/Config/Windows/WindowsEngine.ini"] : [],
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

/// A definition the preset can name that needs an Engine.ini, and one that does not.
const ENGINE_ONLY = {
  id: "latency_sync_interval",
  label: "VSync Sync Interval",
  category: "Latency",
  description: "",
  pak_only: true,
  kind: "Toggle",
  key: "rhi.SyncInterval",
  on_value: "0",
  engine_section: "/Script/Engine.RendererSettings",
  default_enabled: false,
};

describe("PakTweaks preset edge cases", () => {
  it("ignores a preset entry this build has no tweak for", async () => {
    tauri.on("list_tweak_profiles", () => [
      {
        ...QOL,
        settings: [{ id: "a_tweak_from_the_future", enabled: true, value: null }, ...QOL.settings],
      },
    ]);
    const user = await mount();
    await user.click(pakRow("small_P.pak"));
    await waitFor(() => expect(tauri.callsTo("detect_pak_tweaks").length).toBe(1));
    await choosePreset(user, "QOL");

    // The unknown id must not reach the backend, or the whole apply is rejected.
    const labels = pendingLabels();
    expect(labels).toContain("Fix Dark Maps");
    expect(labels.join(" ")).not.toContain("future");
  });

  it("drops tweaks that need an Engine.ini the pak does not ship", async () => {
    tauri
      .on("get_tweak_definitions", () => [...DEFINITIONS, ENGINE_ONLY])
      .on("list_tweak_profiles", () => [
        {
          ...QOL,
          settings: [...QOL.settings, { id: ENGINE_ONLY.id, enabled: true, value: null }],
        },
      ])
      .on("scan_mod_paks_for_ini", () => ({
        paks: [pak("small_P.pak", SMALL, false)],
        unreadable: [],
      }))
      .on("detect_pak_tweaks", () => [
        ...ALL_OFF,
        { id: ENGINE_ONLY.id, active: false, current_value: null },
      ]);
    const user = await mount();
    await user.click(pakRow("small_P.pak"));
    await waitFor(() => expect(tauri.callsTo("detect_pak_tweaks").length).toBe(1));
    await choosePreset(user, "QOL");

    const labels = pendingLabels();
    expect(labels).toContain("Fix Dark Maps");
    expect(labels).not.toContain(ENGINE_ONLY.label);
  });

  it("does not queue a tweak twice when the same preset is picked again", async () => {
    tauri.on("list_tweak_profiles", () => [QOL, { ...QOL, name: "Other", modified_at: 2 }]);
    const user = await mount();
    await user.click(pakRow("small_P.pak"));
    await waitFor(() => expect(tauri.callsTo("detect_pak_tweaks").length).toBe(1));

    await choosePreset(user, "QOL");
    const first = pendingLabels().length;
    expect(first).toBeGreaterThan(0);

    // Radix will not re-fire the same value, so switching away and back is how it repeats.
    await choosePreset(user, "Other");
    await choosePreset(user, "QOL");
    expect(pendingLabels().length).toBe(first);
  });

  it("sends the backend exactly what the save bar shows", async () => {
    tauri.on("apply_pak_tweak_settings", () => "Applied");
    const user = await mount();
    await user.click(pakRow("small_P.pak"));
    await waitFor(() => expect(tauri.callsTo("detect_pak_tweaks").length).toBe(1));
    await choosePreset(user, "QOL");
    const queued = pendingLabels().length;

    await user.click(screen.getByRole("button", { name: /^save$/i }));
    await waitFor(() => expect(tauri.callsTo("apply_pak_tweak_settings").length).toBe(1));
    const sent = tauri.callsTo("apply_pak_tweak_settings")[0].settings as {
      id: string;
      enabled: boolean;
    }[];
    expect(sent.length).toBe(queued);
    // No entry may repeat: the backend rejects a preset that names a tweak twice.
    expect(new Set(sent.map((s) => s.id)).size).toBe(sent.length);
  });

  it("clears the queue after a successful apply", async () => {
    const applied = [
      { id: "fix_dark_maps", active: true, current_value: "1" },
      { id: "force_default_material", active: true, current_value: null },
    ];
    tauri.on("apply_pak_tweak_settings", () => "Applied");
    const user = await mount();
    await user.click(pakRow("small_P.pak"));
    await waitFor(() => expect(tauri.callsTo("detect_pak_tweaks").length).toBe(1));
    await choosePreset(user, "QOL");

    tauri.on("detect_pak_tweaks", () => applied);
    await user.click(screen.getByRole("button", { name: /^save$/i }));
    await waitFor(() => expect(tauri.callsTo("apply_pak_tweak_settings").length).toBe(1));
    await waitFor(() => expect(pendingLabels()).toEqual([]));
  });

  it("keeps a hand-made edit when a preset is chosen on top of it", async () => {
    const user = await mount();
    await user.click(pakRow("small_P.pak"));
    await waitFor(() => expect(tauri.callsTo("detect_pak_tweaks").length).toBe(1));

    // Flip one tweak by hand first.
    const rows = screen.getAllByRole("switch");
    await user.click(rows[0]);
    expect(pendingLabels().length).toBe(1);

    await choosePreset(user, "QOL");
    // The preset names both tweaks, so its values win, but nothing is lost or duplicated.
    const labels = pendingLabels();
    expect(new Set(labels).size).toBe(labels.length);
    expect(labels.length).toBeGreaterThanOrEqual(1);
  });
});

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

  // Reported from the field: apply a preset, close the app, reopen, and the preset can be applied
  // again as though it had never been applied. Nothing persists which preset a pak last had, and
  // choosing one queues every tweak it names, so the save bar fills up again.
  it("offers the same preset again after a restart", async () => {
    const applied = [
      { id: "fix_dark_maps", active: true, current_value: null },
      { id: "force_default_material", active: true, current_value: null },
    ];
    const user = await mount();
    await user.click(pakRow("small_P.pak"));
    await waitFor(() => expect(tauri.callsTo("detect_pak_tweaks").length).toBe(1));
    await choosePreset(user, "QOL");
    expect(pendingLabels().length).toBeGreaterThan(0);

    // Close and reopen: a fresh mount with the pak now matching the preset on disk.
    cleanup();
    tauri = baseMock().on("detect_pak_tweaks", () => applied);
    installTauri(tauri);
    const again = await mount();
    await again.click(pakRow("small_P.pak"));
    await waitFor(() => expect(tauri.callsTo("detect_pak_tweaks").length).toBe(1));

    expect(screen.getByRole("combobox").textContent ?? "").toMatch(/choose preset/i);
    await choosePreset(again, "QOL");
    expect(pendingLabels().length).toBeGreaterThan(0);
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
