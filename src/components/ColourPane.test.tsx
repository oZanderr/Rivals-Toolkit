import { useState, type ReactNode } from "react";

import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { PropertyEntry } from "@/components/AssetInspector";
import {
  draftKey,
  EditSessionContext,
  type Draft,
  type DraftRecord,
  type EditSession,
  type EditTarget,
} from "@/hooks/useAssetEdits";
import {
  sourceKey,
  type ColorReport,
  type ColorSource,
  type RendererColors,
  type Rgba,
} from "@/lib/niagaraColors";
import { createTauri, installTauri, type TauriMock } from "@/test/tauri";

vi.mock("@tauri-apps/api/core", async () => {
  const { invokeProxy } = await import("@/test/tauri");
  return { invoke: invokeProxy };
});

const { ColourPane } = await import("./ColourPane");

const GAME = "C:\\Game";
const CONTAINER = "C:\\Game\\pakchunk0-Windows.utoc";
const ENTRY = "Marvel/Content/Marvel/VFX/NS_Glow.uasset";

const floats = (values: number[]) => ({
  kind: "array" as const,
  items: values.map((value) => ({ kind: "float" as const, value })),
});

const LUT: ColorSource = {
  kind: "lut",
  export: 0,
  object_name: "NiagaraDataInterfaceColorCurve_0",
  class_name: "NiagaraDataInterfaceColorCurve",
  emitter: "NS_Spark",
  owner: "GPUComputeScript",
  peak: 2,
  grey: false,
  enemy: false,
  samples: [
    [2, 0.5, 0.25, 1],
    [1, 1, 1, 0],
  ],
  min_time: 0,
  max_time: 1,
  use_lut: true,
  keys: 0,
  exposed_texture: "/Game/VFX/NS_Glow.NS_Glow:Curve:Texture2D_0",
};
const LUT_PROPS: PropertyEntry[] = [
  { name: "ShaderLUT", span: [100, 140], value: floats([2, 0.5, 0.25, 1, 1, 1, 1, 0]) },
];

const ARRAY: ColorSource = {
  kind: "array",
  export: 1,
  object_name: "NiagaraDataInterfaceArrayColor_0",
  class_name: "NiagaraDataInterfaceArrayColor",
  emitter: "NS_Spark",
  peak: 1,
  grey: false,
  enemy: false,
  field: "ColorData",
  colors: [[1, 0, 0, 1]],
};
const channel = (name: string, value: number, at: number): PropertyEntry => ({
  name,
  span: [at, at + 4],
  value: { kind: "float", value },
});
const ARRAY_PROPS: PropertyEntry[] = [
  {
    name: "ColorData",
    span: [200, 220],
    value: {
      kind: "array",
      items: [
        {
          kind: "struct",
          name: "LinearColor",
          fields: [
            channel("R", 1, 204),
            channel("G", 0, 208),
            channel("B", 0, 212),
            channel("A", 1, 216),
          ],
        },
      ],
    },
  },
];

function bytesOf(color: Rgba): number[] {
  const view = new DataView(new ArrayBuffer(16));
  color.forEach((value, at) => view.setFloat32(at * 4, value, true));
  return Array.from(new Uint8Array(view.buffer));
}
const PARAM: ColorSource = {
  kind: "parameter",
  export: 2,
  object_name: "NS_Glow",
  class_name: "NiagaraSystem",
  peak: 50,
  grey: false,
  enemy: true,
  name: "L10n.Muzzle_light_IsEnemyColor",
  color: [50, 5, 5, 1],
  copies: [{ store: "LocalizationParametersDefault", offsets: [4] }],
};
const PARAM_PROPS: PropertyEntry[] = [
  {
    name: "LocalizationParametersDefault",
    span: [300, 400],
    value: {
      kind: "struct",
      name: "NiagaraParameterStore",
      fields: [
        {
          name: "ParameterData",
          span: [320, 344],
          value: {
            kind: "array",
            items: [0, 0, 0, 0, ...bytesOf([50, 5, 5, 1])].map((value) => ({
              kind: "byte" as const,
              value,
            })),
          },
        },
      ],
    },
  },
];

const SCRIPT: ColorSource = {
  kind: "script",
  export: 3,
  object_name: "SpawnScript",
  class_name: "NiagaraScript",
  emitter: "NS_Spark",
  grey: false,
  enemy: false,
  usage: "ParticleSpawnScript",
  attribute: "Particles.Color",
  drawn: true,
  channels: [
    {
      channel: "R",
      value: 0.75,
      at: 0x2d5,
      writes: "Particles.Color.R",
      expression: "0.75",
      baked: true,
      leaves: [
        {
          kind: "constant",
          at: 0x2d5,
          operand: 2,
          constant: {
            offset: 1400,
            table: "literals",
            within: 48,
            bits: 0x3f400000,
            key: "lit+48=0x3F400000",
          },
          ty: "float",
          uses: 1,
          feeds: ["Particles.Color.R"],
        },
      ],
    },
    {
      channel: "G",
      at: 0x2e0,
      writes: "Particles.Color.G",
      expression: "mul(Particles.Age, 0.5)",
      baked: false,
      leaves: [],
    },
  ],
};

const BEAM = "/Game/VFX/MI_Beam.MI_Beam";
const RENDERER: RendererColors = {
  export: 4,
  object_name: "NiagaraRibbonRendererProperties_2",
  class_name: "NiagaraRibbonRendererProperties",
  emitter: "NS_Spark",
  materials: [BEAM],
  colors: [
    { name: "Enemy1Color01", color: [1, 0.1, 0.1, 1], material: BEAM, from: BEAM, enemy: true },
    {
      name: "MuzzleColor_3_1",
      color: [1, 0.2, 0, 1],
      material: BEAM,
      from: BEAM,
      enemy: false,
      bound: "Emitter.EnemyColor0",
    },
  ],
};

const REPORT: ColorReport = {
  sources: [LUT, ARRAY, PARAM, SCRIPT],
  gpu_scripts: ["GPUComputeScript"],
  renderers: [RENDERER],
};

/** An edit session holding drafts in state, so the pane re-reads them as the inspector's would. */
function Session({ children, seen }: { children: ReactNode; seen: DraftRecord[] }) {
  const [drafts, setDrafts] = useState<Record<string, DraftRecord>>({});
  const session: EditSession = {
    drafts,
    locked: null,
    setDraft(target: EditTarget, draft: Draft, within: string[]) {
      const record = { target, draft, within };
      seen.push(record);
      setDrafts((held) => ({ ...held, [draftKey(target)]: record }));
    },
    dropDraft(key: string) {
      setDrafts((held) => {
        const next = { ...held };
        delete next[key];
        return next;
      });
    },
  };
  return <EditSessionContext.Provider value={session}>{children}</EditSessionContext.Provider>;
}

let tauri: TauriMock;
beforeEach(() => {
  tauri = createTauri()
    .on("niagara_colors", () => REPORT)
    .on("recolor_colors", ({ colors }) =>
      (colors as Rgba[]).map(([r, g, b, a]) => [r * 0.5, g, b * 2, a])
    );
  installTauri(tauri);
});

function show(
  activeExport: number,
  properties: PropertyEntry[],
  extra: Partial<Parameters<typeof ColourPane>[0]> = {}
) {
  const seen: DraftRecord[] = [];
  const props = {
    gamePath: GAME,
    container: CONTAINER,
    entry: ENTRY,
    activeExport,
    properties,
    chosen: null,
    onChoose: vi.fn(),
    onOpenVm: vi.fn(),
    recolorBlocked: null,
    onRecolorAll: vi.fn(),
    ...extra,
  };
  render(
    <Session seen={seen}>
      <ColourPane {...props} />
    </Session>
  );
  return { seen, props };
}

describe("ColourPane", () => {
  it("lists every source under its emitter and says what the GPU scripts are", async () => {
    const { props } = show(0, LUT_PROPS);
    expect(
      await screen.findByText("GPUComputeScript: NiagaraDataInterfaceColorCurve_0")
    ).toBeTruthy();
    expect(screen.getByText("NS_Spark")).toBeTruthy();
    expect(screen.getByText("System")).toBeTruthy();
    expect(screen.getByText(/1 GPU script, whose own colours are shader code/)).toBeTruthy();
    expect(screen.getByText(/Also kept as a texture/)).toBeTruthy();
    await userEvent.click(screen.getByText("L10n.Muzzle_light_IsEnemyColor"));
    expect(props.onChoose).toHaveBeenCalledWith(PARAM.export, sourceKey(PARAM));
  });

  it("tints a curve as one draft of its whole LUT at the same length", async () => {
    const { seen } = show(0, LUT_PROPS);
    await userEvent.click(await screen.findByRole("button", { name: "tint" }));
    await userEvent.click(screen.getByRole("button", { name: "Apply to this curve" }));
    await waitFor(() => expect(seen.length).toBe(1));
    const [{ draft, target }] = seen;
    expect(target.name).toBe("ShaderLUT");
    expect(tauri.callsTo("recolor_colors")[0].colors).toEqual(LUT.samples);
    if (draft.op !== "set_items") throw new Error(`expected set_items, got ${draft.op}`);
    expect(draft.held).toEqual(["2", "0.5", "0.25", "1", "1", "1", "1", "0"]);
    // Halved red and doubled blue change; green and alpha keep the text they read as.
    expect(draft.items).toEqual(["1", "0.5", "0.5", "1", "0.5", "1", "2", "0"]);
  });

  it("edits an array's channel as a set on that field", async () => {
    const { seen } = show(1, ARRAY_PROPS, { chosen: "1:array" });
    const red = await screen.findByLabelText("Element 0 R");
    fireEvent.change(red, { target: { value: "0.25" } });
    fireEvent.blur(red);
    expect(seen).toHaveLength(1);
    expect(seen[0].target.offset).toBe(204);
    expect(seen[0].draft).toEqual({ op: "set", text: "0.25" });
  });

  it("writes a parameter's new colour into its store's bytes", async () => {
    const { seen } = show(2, PARAM_PROPS);
    await userEvent.click(await screen.findByRole("button", { name: "Apply to this parameter" }));
    await waitFor(() => expect(seen.length).toBe(1));
    const { draft, target, within } = seen[0];
    expect(target.name).toBe("ParameterData");
    expect(within).toHaveLength(1);
    if (draft.op !== "set_items") throw new Error(`expected set_items, got ${draft.op}`);
    expect(draft.items.slice(0, 4)).toEqual(["0", "0", "0", "0"]);
    expect(draft.items.slice(4).map(Number)).toEqual(bytesOf([25, 5, 10, 1]));
  });

  it("shows a script's channels read only, with the command for a baked one", async () => {
    const { props } = show(3, []);
    const user = userEvent.setup();
    expect(await screen.findByText("0.75")).toBeTruthy();
    expect(screen.getByText("mul(Particles.Age, 0.5)")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Copy edit-vm for R" }));
    // user-event stands in for the clipboard, so what was copied reads back from it.
    expect(await navigator.clipboard.readText()).toBe(
      `rivals-cli asset edit-vm "${ENTRY}" --export SpawnScript --at 0x02D5 --operand 2 --value 0.75 --into MyMod`
    );
    await user.click(screen.getByRole("button", { name: /Open in the VM view/ }));
    expect(props.onOpenVm).toHaveBeenCalledWith(3);
  });

  it("recolours the whole effect, or only its enemy colours, through one save", async () => {
    const { props } = show(0, LUT_PROPS);
    await userEvent.click(await screen.findByRole("button", { name: "Recolour effect" }));
    await userEvent.click(screen.getByRole("button", { name: "Recolour and save" }));
    expect(props.onRecolorAll).toHaveBeenLastCalledWith({}, { tool: "paint", rgb: [0.6, 0.2, 1] });
    await userEvent.click(screen.getByRole("checkbox", { name: /enemy colour/ }));
    await userEvent.click(screen.getByRole("button", { name: "Recolour and save" }));
    expect(props.onRecolorAll).toHaveBeenLastCalledWith(
      { enemy: true },
      { tool: "paint", rgb: [0.6, 0.2, 1] }
    );
  });

  it("lists a renderer's material colours read only, and recolours them only when asked", async () => {
    const { props } = show(4, []);
    expect(await screen.findByText("Enemy1Color01")).toBeTruthy();
    expect(screen.getByText("from Emitter.EnemyColor0")).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: /^MI_Beam/ }));
    expect(props.onChoose).toHaveBeenCalledWith(4, "4:materials");
    await userEvent.click(screen.getByRole("button", { name: "Recolour effect" }));
    // The bound colour is a script's to set, so only the other counts.
    await userEvent.click(screen.getByRole("checkbox", { name: "With the 1 material colour" }));
    await userEvent.click(screen.getByRole("button", { name: "Recolour and save" }));
    expect(props.onRecolorAll).toHaveBeenLastCalledWith(
      { materials: true },
      { tool: "paint", rgb: [0.6, 0.2, 1] }
    );
  });

  it("holds the effect-wide recolour while edits are pending", async () => {
    show(0, LUT_PROPS, { recolorBlocked: "Save or discard your edits first." });
    await userEvent.click(await screen.findByRole("button", { name: "Recolour effect" }));
    expect(
      (screen.getByRole("button", { name: "Recolour and save" }) as HTMLButtonElement).disabled
    ).toBe(true);
  });
});
