import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { createTauri, installTauri, type TauriMock } from "@/test/tauri";

vi.mock("@tauri-apps/api/core", async () => {
  const { invokeProxy } = await import("@/test/tauri");
  return { invoke: invokeProxy };
});

const { VmPane } = await import("./VmPane");

const GAME = "C:\\Game";
const ENTRY = "Marvel/Content/Marvel/VFX/NS_Trail.uasset";
const RED = {
  offset: 1388,
  table: "literals",
  within: 44,
  bits: 0x41400000,
  key: "lit+44=0x41400000",
};
const ENEMY = { offset: 648, table: "external", within: 8, name: "User.IsEnemy", key: "ext+8" };

function view(extra: Record<string, unknown> = {}) {
  return {
    object_name: "SpawnScript_1",
    usage: "ParticleSpawnScriptInterpolated",
    interpolated: true,
    param_size: 32,
    bytecode_len: 1077,
    literals: new Array(144).fill(0),
    program: { instructions: [{}, {}, {}] },
    layout: { tables: [["literals", 1344, 144]] },
    lines: [
      { at: 0, text: "enter_stat_scope 8 (lit+0)", constants: [] },
      {
        at: 0xbd,
        text: "lerp             12.0 (lit+44), 10.0 (lit+48), r11 -> r14",
        constants: [[0, RED]],
      },
      {
        at: 0x2d5,
        text: "outputdata_float r14 -> Particles.Color.R [row r23]",
        constants: [],
        writes: "Particles.Color.R",
      },
    ],
    traces: [
      {
        at: 0x2d5,
        writes: "Particles.Color.R",
        expression: "lerp(12.0, 10.0, clamp(User.IsEnemy, 0.0, 1.0))",
        baked: false,
        leaves: [
          {
            kind: "constant",
            at: 0xbd,
            operand: 0,
            constant: RED,
            ty: "float",
            uses: 1,
            feeds: ["Particles.Color.R"],
          },
          {
            kind: "constant",
            at: 0xb3,
            operand: 0,
            constant: ENEMY,
            ty: "float",
            uses: 1,
            feeds: ["Particles.Color.R", "Particles.Color.G"],
          },
        ],
      },
      {
        at: 0x343,
        writes: "Particles.Lifetime",
        expression: "add(0.2, random(0.0))",
        baked: false,
        leaves: [],
      },
    ],
    ...extra,
  };
}

let mock: TauriMock;

beforeEach(() => {
  mock = createTauri().on("export_vm_view", () => view());
  installTauri(mock);
});

function mount() {
  render(<VmPane gamePath={GAME} container="pakchunkVFX-Windows" entry={ENTRY} exportIndex={28} />);
}

describe("VmPane", () => {
  it("lists the program with its header and asks for the export shown", async () => {
    mount();
    expect(
      await screen.findByText(/ParticleSpawnScriptInterpolated, interpolated spawn/)
    ).toBeTruthy();
    expect(screen.getByText("3 instructions")).toBeTruthy();
    expect(screen.getByText("literals 144 bytes at 1344")).toBeTruthy();
    expect(screen.getByText(/outputdata_float r14 -> Particles.Color.R/)).toBeTruthy();
    expect(mock.callsTo("export_vm_view")).toEqual([
      { gameRoot: GAME, container: "pakchunkVFX-Windows", entry: ENTRY, export: 28 },
    ]);
  });

  it("narrows the outputs by name and opens one to what it is made of", async () => {
    mount();
    const user = userEvent.setup();
    await user.type(await screen.findByLabelText("Filter outputs"), "color");
    expect(screen.queryByText("Particles.Lifetime")).toBeNull();
    expect(screen.getByText("= lerp(12.0, 10.0, clamp(User.IsEnemy, 0.0, 1.0))")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: /Particles.Color.R/ }));
    expect(screen.getByText("12.0")).toBeTruthy();
    expect(screen.getByText("User.IsEnemy")).toBeTruthy();
    expect(screen.getByText("feeds 2 outputs")).toBeTruthy();
  });

  it("shows a leaf's instruction and copies the command that changes it", async () => {
    mount();
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: /Particles.Color.R/ }));
    await user.click(screen.getAllByRole("button", { name: "Show 0x00BD" })[0]);
    const line = screen.getByText(/lerp 12\.0 \(lit\+44\)/).parentElement;
    expect(line?.getAttribute("aria-current")).toBe("true");
    await user.click(screen.getByRole("button", { name: "Copy edit-vm for 0x00BD #0" }));
    // user-event stands in for the clipboard, so what was copied reads back from it.
    expect(await navigator.clipboard.readText()).toBe(
      `rivals-cli asset edit-vm "${ENTRY}" --export SpawnScript_1 --at 0x00BD --operand 0 --value 12.0 --into MyMod`
    );
  });

  it("says why its constants cannot be edited, and when there is no program", async () => {
    mock.on("export_vm_view", () => view({ edit_refusal: "its bytecode is stored compressed" }));
    mount();
    expect(
      await screen.findByText(/constants not editable: its bytecode is stored compressed/)
    ).toBeTruthy();
  });

  it("explains a script with no bytecode", async () => {
    mock.on("export_vm_view", () => view({ bytecode_len: 0, usage: "ParticleGPUComputeScript" }));
    mount();
    expect(
      await screen.findByText(/This ParticleGPUComputeScript script carries no VM bytecode/)
    ).toBeTruthy();
  });
});
