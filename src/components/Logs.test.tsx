import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { createTauri, installTauri, type TauriMock } from "@/test/tauri";

vi.mock("@tauri-apps/api/core", async () => {
  const { invokeProxy } = await import("@/test/tauri");
  return { invoke: invokeProxy };
});
vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: async () => null,
  save: async () => null,
}));
vi.mock("@tauri-apps/plugin-opener", () => ({
  openPath: async () => {},
  revealItemInDir: async () => {},
}));

const { Logs } = await import("./Logs");

const CRASHED = {
  name: "2026.10.15_15.41.59-31760",
  log_path: "C:\\Logs\\2026.10.15_15.41.59-31760.log",
  crash_dir: "C:\\Crashes\\UECC-Windows-A_0000",
  pid: 31760,
  started: 1792100000,
  ended: 1792100010,
  size: 45000,
  build: "1.1.3939753.3939753",
  outcome: "crashed",
  crash_type: "Crash",
  crash_message: "Unhandled Exception: EXCEPTION_ACCESS_VIOLATION reading address 0x28",
  crash_signature: "sig",
  repeat_count: 6,
  error: null,
};

const CLEAN = {
  ...CRASHED,
  name: "2026.10.15_14.00.00-100",
  log_path: "C:\\Logs\\2026.10.15_14.00.00-100.log",
  crash_dir: null,
  pid: 100,
  started: 1792090000,
  ended: 1792093600,
  outcome: "clean_exit",
  crash_type: null,
  crash_message: null,
  crash_signature: null,
  repeat_count: 0,
};

const OVERVIEW = {
  snapshot: 1,
  name: CRASHED.name,
  log_path: CRASHED.log_path,
  crash_dir: CRASHED.crash_dir,
  line_count: 3,
  verbosity_counts: [
    { verbosity: "error", count: 1 },
    { verbosity: "display", count: 2 },
  ],
  categories: [
    { name: "LogInit", count: 2 },
    { name: "", count: 1 },
  ],
  analysis: {
    outcome: "crashed",
    build: "1.1.3939753.3939753",
    engine_version: null,
    command_line: null,
    fatal: null,
    fatal_count: 0,
    error_count: 1,
    warning_count: 0,
    groups: [
      {
        verbosity: "error",
        category: "",
        message: "CreateExport: /Game/Marvel/UI/Shared - Could not find class object",
        count: 2,
        first_line: 2,
        packages: ["/Game/Marvel/UI/Shared"],
        without_mods: false,
      },
      {
        verbosity: "error",
        category: "LogOnline",
        message: "EOS: FOnlineSubsystemEOS::PlatformCreate() GetSelectedArtifactSettings failed",
        count: 1,
        first_line: 1,
        packages: [],
        without_mods: true,
      },
    ],
    groups_total: 2,
    baseline_sessions: 12,
    game_thread_python: [
      {
        file: "Scripts/subclassing/ui/PyUIManager.py",
        line: 99,
        function: "OnGameInstanceStart",
        third_party: false,
      },
    ],
    mods: [{ name: "Skin_9999999_P.pak", state: "enabled" }],
    mods_source: "log",
    mod_hits: [
      {
        mod_name: "Skin_9999999_P.pak",
        state: "enabled",
        packages: ["/Game/Marvel/UI/Shared"],
        groups: [0],
        errors: 2,
        warnings: 0,
      },
    ],
    fatal_mods: [
      {
        mod_name: "Skin_9999999_P.pak",
        state: "enabled",
        packages: ["/Game/Marvel/UI/Shared"],
      },
    ],
    notes: [],
  },
  crash: {
    dir: CRASHED.crash_dir,
    folder: "UECC-Windows-A_0000",
    crash_type: "Crash",
    error_message: CRASHED.crash_message,
    process_id: 31760,
    seconds_since_start: 0,
    crashed_at: null,
    engine_version: null,
    build_version: "3939753",
    command_line: null,
    cpu: null,
    gpu: "GPU",
    gpu_driver: "616.64",
    rhi: "D3D12",
    os: null,
    memory_gb: 32,
    out_of_memory: false,
    crashed_thread: "GameThread",
    frames: [
      { module: "Tool", offset: 0x16dc31, symbol: null, class: "third_party" },
      { module: "Marvel-Win64-Shipping", offset: 0x245e66f, symbol: null, class: "game" },
    ],
    faulting_frame: 0,
    culprit_frame: 0,
    modules: [{ name: "Tool.dll", path: "D:\\Tools\\Tool.dll", class: "third_party" }],
    signature: "sig",
    log_file: null,
  },
  culprit: {
    kind: "mods",
    mods: ["Skin_9999999_P.pak"],
    packages: ["/Game/Marvel/UI/Shared"],
  },
  key_source: "bundled",
  key_error: null,
};

function line(n: number, verbosity: string, text: string) {
  return {
    n,
    verbosity,
    category: "",
    text,
    continuation: false,
    trailer: false,
    repeat: false,
  };
}

const LINES = [
  line(0, "display", "LogInit: Display: Build: 1.1.3939753.3939753"),
  line(1, "display", "LogInit: Display: Engine Version: 5.3.2"),
  line(2, "error", "Error: CreateExport: /Game/Marvel/UI/Shared - Could not find class object"),
];

let tauri: TauriMock;

beforeEach(() => {
  // jsdom lays nothing out, and the log list is virtualized: give it a viewport to fill.
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockReturnValue(600);
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockReturnValue(800);
  tauri = createTauri()
    .on("list_game_sessions", () => [CRASHED, CLEAN])
    .on("open_game_log", () => OVERVIEW)
    .on("filter_game_log", () => LINES.length)
    .on("game_log_rows", ({ start, count }) =>
      LINES.slice(Number(start), Number(start) + Number(count))
    )
    .on("game_log_row_of", ({ line: at }) => at)
    .on("game_log_text", () => LINES.map((l) => l.text).join("\n"));
  installTauri(tauri);
});

describe("Logs", () => {
  it("opens the newest session and summarises its crash and the mod behind it", async () => {
    render(<Logs gamePath="C:\Game" isActive onOpenSettings={() => {}} />);

    expect(await screen.findByText("Cause")).toBeTruthy();
    expect(tauri.callsTo("open_game_log")[0]).toMatchObject({
      name: CRASHED.name,
      logPath: CRASHED.log_path,
      crashDir: CRASHED.crash_dir,
      gameRoot: "C:\\Game",
    });
    expect(screen.getByText("Tool.dll")).toBeTruthy();
    expect(screen.getByText("Seen 6 times")).toBeTruthy();
    expect(screen.getByText("2 errors")).toBeTruthy();
  });

  it("pages the log's lines in from the backend", async () => {
    const user = userEvent.setup();
    render(<Logs gamePath="C:\Game" isActive onOpenSettings={() => {}} />);
    await screen.findByText("Cause");

    await user.click(screen.getByRole("button", { name: "Log" }));

    expect(await screen.findByText(LINES[0].text)).toBeTruthy();
    expect(tauri.callsTo("game_log_rows")[0]).toMatchObject({ snapshot: 1, start: 0 });
  });

  it("opens the log at an error group's first line", async () => {
    const user = userEvent.setup();
    render(<Logs gamePath="C:\Game" isActive onOpenSettings={() => {}} />);
    await screen.findByText("Cause");

    const [group] = screen.getAllByText(/Could not find class object/);
    await user.click(group);

    await waitFor(() =>
      expect(tauri.callsTo("game_log_row_of")).toEqual([{ snapshot: 1, line: 2 }])
    );
  });

  it("names the mod shipping the package the crash names, and the Python it died in", async () => {
    render(<Logs gamePath="C:\Game" isActive onOpenSettings={() => {}} />);

    expect(await screen.findByText("Cause")).toBeTruthy();
    expect(screen.getByText("The crash names a package this mod ships.")).toBeTruthy();
    expect(screen.getByText("Skin_9999999_P.pak")).toBeTruthy();
    expect(
      screen.getByText("Scripts/subclassing/ui/PyUIManager.py:99 in OnGameInstanceStart")
    ).toBeTruthy();
  });

  it("lists the mods errors name without calling one the cause when the crash names none", async () => {
    tauri.on("open_game_log", () => ({ ...OVERVIEW, culprit: null }));
    render(<Logs gamePath="C:\Game" isActive onOpenSettings={() => {}} />);

    expect(await screen.findByText("Errors naming your mods")).toBeTruthy();
    expect(screen.queryByText("Cause")).toBeNull();
    expect(screen.getByText("2 errors")).toBeTruthy();
  });

  it("splits the source location off a fatal error's message", async () => {
    tauri.on("open_game_log", () => ({
      ...OVERVIEW,
      crash: {
        ...OVERVIEW.crash,
        error_message:
          "Fatal error: [File:G:\\ws\\Engine\\AsyncLoading2.cpp] [Line: 1734] ObjectSerializationError: /Game/A",
      },
    }));
    render(<Logs gamePath="C:\Game" isActive onOpenSettings={() => {}} />);

    expect(await screen.findByText("AsyncLoading2.cpp:1734")).toBeTruthy();
    expect(screen.getByText("ObjectSerializationError: /Game/A")).toBeTruthy();
  });

  it("folds away the errors the game also logs without mods until asked", async () => {
    const user = userEvent.setup();
    render(<Logs gamePath="C:\Game" isActive onOpenSettings={() => {}} />);
    await screen.findByText("Errors and warnings");
    expect(screen.queryByText(/GetSelectedArtifactSettings failed/)).toBeNull();

    await user.click(screen.getByRole("button", { name: "1 also logged without mods" }));

    expect(screen.getByText(/GetSelectedArtifactSettings failed/)).toBeTruthy();
  });

  it("says so when every error also shows up without mods", async () => {
    tauri.on("open_game_log", () => ({
      ...OVERVIEW,
      analysis: {
        ...OVERVIEW.analysis,
        groups: OVERVIEW.analysis.groups.map((g) => ({ ...g, without_mods: true })),
      },
    }));
    render(<Logs gamePath="C:\Game" isActive onOpenSettings={() => {}} />);

    expect(
      await screen.findByText(
        "Every one of these also shows up in sessions without mods, so none points at a mod."
      )
    ).toBeTruthy();
    expect(screen.getByRole("button", { name: "2 also logged without mods" })).toBeTruthy();
  });

  it("marks a mod switched off since the session", async () => {
    tauri.on("open_game_log", () => ({
      ...OVERVIEW,
      analysis: {
        ...OVERVIEW.analysis,
        fatal_mods: [{ ...OVERVIEW.analysis.fatal_mods[0], state: "disabled" }],
      },
    }));
    render(<Logs gamePath="C:\Game" isActive onOpenSettings={() => {}} />);

    expect(await screen.findByText("disabled now")).toBeTruthy();
  });

  it("copies the whole log", async () => {
    const user = userEvent.setup();
    render(<Logs gamePath="C:\Game" isActive onOpenSettings={() => {}} />);
    await screen.findByText("Errors and warnings");

    await user.click(screen.getByRole("button", { name: /Copy log/ }));

    await waitFor(async () =>
      expect(await navigator.clipboard.readText()).toBe(LINES.map((l) => l.text).join("\n"))
    );
  });

  it("leaves out the mods card when neither the crash nor an error names a mod", async () => {
    tauri.on("open_game_log", () => ({
      ...OVERVIEW,
      culprit: null,
      analysis: { ...OVERVIEW.analysis, mod_hits: [], fatal_mods: [] },
    }));
    render(<Logs gamePath="C:\Game" isActive onOpenSettings={() => {}} />);

    expect(await screen.findByText("Errors and warnings")).toBeTruthy();
    expect(screen.queryByText("Cause")).toBeNull();
    expect(screen.queryByText("Errors naming your mods")).toBeNull();
  });

  it("lists only crashed sessions when asked", async () => {
    const user = userEvent.setup();
    render(<Logs gamePath="C:\Game" isActive onOpenSettings={() => {}} />);
    await screen.findByText("Cause");
    expect(screen.getByText("Clean exit")).toBeTruthy();

    await user.click(screen.getByRole("switch", { name: /Crashes only/ }));

    expect(screen.queryByText("Clean exit")).toBeNull();
  });
});
