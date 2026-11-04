import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { createTauri, dialogStub, installTauri, type TauriMock } from "@/test/tauri";

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

const { default: AssetInspector } = await import("./AssetInspector");

const GAME = "C:\\Game";
const CONTAINER = "C:\\Game\\MarvelGame\\Marvel\\Content\\Paks\\pakchunk0-Windows.utoc";
const ENTRY = "Marvel/Content/Test/BP_Test.uasset";
const PACKAGE = "/Game/Test/BP_Test";

type Value = Record<string, unknown>;

function entry(name: string, value: Value, at: number) {
  return { name, value, span: [at, at + 4] };
}

function object(index: number, path: string): Value {
  return { kind: "object", index, path };
}

function exp(index: number, object_name: string, class_name: string, properties: unknown[] = []) {
  return {
    index,
    object_name,
    class_name,
    serial_offset: 100 * index,
    serial_size: 50,
    outer_index: index === 0 ? 0 : 1,
    class_index: -1,
    super_index: 0,
    template_index: 0,
    object_flags: 0,
    generate_public_hash: false,
    path: `${PACKAGE}.BP_Test_C:${object_name}`,
    status: { state: "complete" },
    properties,
  };
}

/** A Blueprint whose construction script builds a root component with a mesh under it. */
function blueprint(extra: Record<string, unknown> = {}) {
  const node = (at: number) => object(at + 1, `${PACKAGE}.BP_Test_C:SCS_Node_${at}`);
  return {
    package_name: PACKAGE,
    cooked: true,
    unversioned_properties: true,
    name_count: 0,
    import_count: 0,
    export_count: 4,
    names: ["OldName"],
    imports: [],
    exports: [
      exp(0, "BP_Test_C", "BlueprintGeneratedClass"),
      exp(1, "SimpleConstructionScript_0", "SimpleConstructionScript", [
        entry("RootNodes", { kind: "array", items: [node(2)] }, 10),
      ]),
      exp(2, "SCS_Node_2", "SCS_Node", [
        entry("ChildNodes", { kind: "array", items: [node(3)] }, 20),
        entry("InternalVariableName", { kind: "name", value: "Root" }, 30),
      ]),
      exp(3, "SCS_Node_3", "SCS_Node", [
        entry("InternalVariableName", { kind: "name", value: "Mesh" }, 40),
      ]),
    ],
    unresolved_structs: [],
    resources: [],
    ...extra,
  };
}

function tauri(pkg: unknown = blueprint()): TauriMock {
  return createTauri()
    .on("inspect_asset", () => pkg)
    .on("get_text_culture", () => "en")
    .on("text_cultures", () => ["en"])
    .on("set_text_culture", () => null)
    .on("get_asset_mod_name", () => "TestMod")
    .on("get_asset_save_target", () => "iostore")
    .on("set_asset_mod_name", () => null)
    .on("set_asset_save_target", () => null)
    .on("get_mods_status", () => ({ mod_entries: [] }))
    .on("mod_copy_of", () => null)
    .on("get_mappings_status", () => ({
      path: "Mappings.usmap",
      loaded: true,
      struct_count: 1,
      enum_count: 1,
      error: null,
    }))
    .on("check_object_path", () => ({ state: "found" }))
    .on("unused_names", () => ["OldName"])
    .on("parent_components", () => ["StaticMesh", "DefaultSceneRoot"])
    .on("plan_export_edits", () => ({
      blockers: [],
      warnings: [],
      public: [],
      repathed: [],
      importers: [],
      index_available: false,
      mentioned_by: [],
    }))
    .on("save_asset_edits", () => ({
      outcome: "written",
      message: "Saved",
      pak: "TestMod_9999999_P.utoc",
      warnings: [],
    }));
}

let mock: TauriMock;

function mount() {
  render(
    <AssetInspector
      gamePath={GAME}
      container={CONTAINER}
      entry={ENTRY}
      gameRunning={false}
      isActive
      onClose={() => undefined}
      onOpenSettings={() => undefined}
    />
  );
}

/** The edit list the last save sent. */
function lastSave(): Record<string, unknown> {
  const calls = mock.callsTo("save_asset_edits");
  expect(calls.length).toBeGreaterThan(0);
  return calls[calls.length - 1].edits as Record<string, unknown>;
}

/** Opens the actions of the export named `name` in the package overview. */
async function actionsOf(name: string) {
  const user = userEvent.setup();
  await screen.findAllByText(name);
  const buttons = await screen.findAllByRole("button", { name: "Export actions" });
  // The overview lists the exports in table order, one actions button each.
  const at = (blueprint().exports as { object_name: string }[]).findIndex(
    (exp) => exp.object_name === name
  );
  await user.click(buttons[at]);
  return user;
}

beforeEach(() => {
  mock = tauri();
  installTauri(mock);
});

describe("AssetInspector dialogs", () => {
  it("duplicates a component with the components under it", async () => {
    mount();
    const user = await actionsOf("SCS_Node_2");
    await user.click(await screen.findByText("Duplicate component…"));
    const dialog = await screen.findByRole("alertdialog");
    await user.click(within(dialog).getByRole("checkbox"));
    await user.click(within(dialog).getByRole("button", { name: "Duplicate" }));
    await waitFor(() =>
      expect(lastSave().add_components).toEqual([{ node: 2, name: "Root2", with_children: true }])
    );
  });

  it("removes a component and hangs its children where it was", async () => {
    mount();
    const user = await actionsOf("SCS_Node_2");
    await user.click(await screen.findByText("Remove component…"));
    const dialog = await screen.findByRole("alertdialog");
    await user.click(within(dialog).getByRole("button", { name: "Remove" }));
    await waitFor(() =>
      expect(lastSave().remove_components).toEqual([{ node: 2, with_children: false }])
    );
  });

  it("copies a component the parent Blueprint adds", async () => {
    mount();
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: /Add from parent/ }));
    const dialog = await screen.findByRole("alertdialog");
    await within(dialog).findByRole("combobox");
    await user.click(within(dialog).getByRole("button", { name: "Add" }));
    await waitFor(() =>
      expect(lastSave().add_components).toEqual([
        { node: 0, name: "StaticMeshCopy", from_parent: "StaticMesh" },
      ])
    );
    expect(mock.callsTo("parent_components")[0]).toMatchObject({ entry: ENTRY });
  });

  it("saves the asset under a new package name", async () => {
    mount();
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: /Save as a new asset/ }));
    const dialog = await screen.findByRole("alertdialog");
    const path = within(dialog).getByLabelText("Package name");
    await user.clear(path);
    await user.type(path, "/Game/Mods/Test/BP_Copy");
    await user.click(within(dialog).getByRole("button", { name: /Save into/ }));
    await waitFor(() =>
      expect(lastSave().save_as).toEqual({
        package: "/Game/Mods/Test/BP_Copy",
        rename_objects: true,
      })
    );
  });

  it("adds an empty object of a class", async () => {
    mount();
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: /Add object/ }));
    const dialog = await screen.findByRole("alertdialog");
    await user.type(within(dialog).getByLabelText("Class"), "/Script/Engine.DataAsset");
    await user.type(within(dialog).getByLabelText("Name"), "Thing");
    await user.click(within(dialog).getByRole("button", { name: "Add" }));
    await waitFor(() =>
      expect(lastSave().add_exports).toEqual([{ class: "/Script/Engine.DataAsset", name: "Thing" }])
    );
  });

  it("adds a field to a Blueprint struct from a name and a type, as a save of its own", async () => {
    mock = tauri(
      blueprint({ export_count: 1, exports: [exp(0, "CardData", "UserDefinedStruct")] })
    );
    installTauri(mock);
    mount();
    const user = userEvent.setup();
    await screen.findAllByText("CardData");
    await user.click((await screen.findAllByRole("button", { name: "Export actions" }))[0]);
    await user.click(await screen.findByText("Add field…"));
    const dialog = await screen.findByRole("alertdialog");
    const add = within(dialog).getByRole("button", { name: "Add field" }) as HTMLButtonElement;
    expect(add.disabled).toBe(true);
    await user.type(within(dialog).getByLabelText("New field name"), "Rarity");
    await user.type(within(dialog).getByLabelText("New field type"), "Int");
    await user.click(add);
    await waitFor(() =>
      expect(lastSave().add_fields).toEqual([{ struct: 0, name: "Rarity", type: "Int" }])
    );
  });

  it("offers a new field on a Blueprint struct only", async () => {
    mount();
    await actionsOf("SCS_Node_2");
    await screen.findByText("Duplicate component…");
    expect(screen.queryByText("Add field…")).toBeNull();
  });

  it("drops the names nothing uses once it has listed them", async () => {
    mount();
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: /Drop unused names/ }));
    const dialog = await screen.findByRole("alertdialog");
    expect(await within(dialog).findByText("OldName")).toBeTruthy();
    await user.click(within(dialog).getByRole("button", { name: /Drop 1 names/ }));
    await waitFor(() => expect(lastSave().compact_names).toBe(true));
  });

  it("moves an export under another once the move is planned", async () => {
    mount();
    const user = await actionsOf("SCS_Node_3");
    await user.click(await screen.findByText("Move export…"));
    const dialog = await screen.findByRole("alertdialog");
    await user.selectOptions(within(dialog).getByRole("combobox"), "");
    await waitFor(() => expect(mock.callsTo("plan_export_edits").length).toBeGreaterThan(0));
    await user.click(within(dialog).getByRole("button", { name: "Move" }));
    await waitFor(() =>
      expect(lastSave().export_edits).toEqual([{ op: "set_outer", export: 3, outer: null }])
    );
  });

  it("asks before saving an import of nothing, and saves it when told to", async () => {
    let tries = 0;
    mock.on("save_asset_edits", () => {
      tries += 1;
      // Tauri rejects with the command's error text itself, not an Error around it.
      if (tries === 1)
        return Promise.reject("Nothing is at the path these edits point at: /Game/X");
      return { outcome: "written", message: "Saved", pak: "TestMod_9999999_P.utoc", warnings: [] };
    });
    mount();
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: /Add object/ }));
    let dialog = await screen.findByRole("alertdialog");
    await user.type(within(dialog).getByLabelText("Class"), "/Game/X.X_C");
    await user.type(within(dialog).getByLabelText("Name"), "Thing");
    await user.click(within(dialog).getByRole("button", { name: "Add" }));
    dialog = await screen.findByRole("alertdialog", { name: /point at nothing/ });
    await user.click(within(dialog).getByRole("button", { name: "Save anyway" }));
    await waitFor(() => expect(mock.callsTo("save_asset_edits")).toHaveLength(2));
    expect(mock.callsTo("save_asset_edits")[1]).toMatchObject({ allowMissing: true });
  });
});

const MENU_TABLE = "/Game/UI/Menu_ST.Menu_ST";

/** One export holding a string table text, which opens straight into its property tree. */
function labelPackage() {
  return {
    package_name: PACKAGE,
    cooked: true,
    unversioned_properties: true,
    name_count: 0,
    import_count: 0,
    export_count: 1,
    names: [],
    imports: [],
    exports: [
      exp(0, "Settings", "SettingsData", [
        {
          name: "Label",
          span: [40, 72],
          value: {
            kind: "text",
            value: `${MENU_TABLE}:Play`,
            display: "Play now",
            parts: [
              { name: "TableId", span: [45, 53], value: { kind: "name", value: MENU_TABLE } },
              { name: "Key", span: [53, 72], value: { kind: "str", value: "Play" } },
            ],
          },
        },
      ]),
    ],
    unresolved_structs: [],
    resources: [],
  };
}

describe("AssetInspector text edits", () => {
  it("retypes a string table text as a whole, starting from its literal", async () => {
    mock = tauri(labelPackage());
    installTauri(mock);
    mount();
    const user = userEvent.setup();
    await user.click(await screen.findByText("Play now"));
    const input = await screen.findByDisplayValue(`LOCTABLE("${MENU_TABLE}", "Play")`);
    await user.clear(input);
    await user.type(input, 'INVTEXT("Hi"){Enter}');
    await user.click(await screen.findByRole("button", { name: /Save as mod/ }));
    await waitFor(() =>
      expect(lastSave().values).toEqual([
        { offset: 40, name: "Label", kind: "text", op: "set", text: 'INVTEXT("Hi")' },
      ])
    );
    // The drift check holds the text to the literal it was retyped from.
    expect(lastSave().expect).toMatchObject({
      values: { "40": `LOCTABLE("${MENU_TABLE}", "Play")` },
    });
  });
});

/** One string table, which opens straight into its entries. */
function tablePackage() {
  return {
    ...labelPackage(),
    exports: [
      {
        ...exp(0, "Menu_ST", "StringTable"),
        string_table: {
          namespace: "Menu_ST",
          entries: [
            { key: "Play", source: "Play", tag: "", metadata: [] },
            { key: "Quit", source: "Quit", tag: "", metadata: [] },
          ],
        },
      },
    ],
  };
}

const TEXT_REPORT = {
  pak: "C:\\Game\\MarvelGame\\Marvel\\Content\\Paks\\~mods\\TestMod_9999999_P.pak",
  files: [
    {
      culture: "en",
      path: "Marvel/Content/Localization/Game/en/Game.locres",
      edits: 1,
      removed: false,
      adopted: 0,
      dropped: 0,
    },
  ],
  written: true,
};

describe("AssetInspector translations", () => {
  it("gives a string table text a wording of its own in the mod, leaving the package alone", async () => {
    mock = tauri(labelPackage())
      .on("text_translation", () => ({
        culture: "en",
        namespace: "Menu_ST",
        key: "Play",
        game: "Play now",
        held: null,
      }))
      .on("save_text_translation", () => TEXT_REPORT);
    installTauri(mock);
    mount();
    const user = userEvent.setup();
    const menu = await menuOf(user, "Play now");
    await user.click(within(menu).getByRole("menuitem", { name: /Edit translation \(en\)/ }));
    const wording = await screen.findByDisplayValue("Play now");
    expect(mock.callsTo("text_translation")[0]).toMatchObject({
      modName: "TestMod",
      text: `${MENU_TABLE}:Play`,
    });
    await user.clear(wording);
    await user.type(wording, "Go!");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(mock.callsTo("save_text_translation")[0]).toMatchObject({
        text: `${MENU_TABLE}:Play`,
        wording: "Go!",
        overwrite: false,
      })
    );
    expect(await screen.findByText(/its own en wording in TestMod_9999999_P\.pak/)).toBeTruthy();
    expect(mock.callsTo("save_asset_edits")).toHaveLength(0);
  });

  it("writes over a translation file holding texts the game dropped only when asked again", async () => {
    let calls = 0;
    mock = tauri(labelPackage())
      .on("text_translation", () => ({
        culture: "en",
        namespace: "Menu_ST",
        key: "Play",
        game: "Play now",
        held: "Old",
      }))
      .on("save_text_translation", () => {
        calls += 1;
        if (calls === 1) {
          throw new Error(
            "TestMod_9999999_P ships it with 2 text(s) the game's en translations no longer have, which writing it again leaves out; pass --overwrite to write it anyway"
          );
        }
        return TEXT_REPORT;
      });
    installTauri(mock);
    mount();
    const user = userEvent.setup();
    const menu = await menuOf(user, "Play now");
    await user.click(within(menu).getByRole("menuitem", { name: /Edit translation/ }));
    const wording = await screen.findByDisplayValue("Old");
    await user.type(wording, "er");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await user.click(await screen.findByRole("button", { name: "Write anyway" }));
    await waitFor(() =>
      expect(mock.callsTo("save_text_translation").map((args) => args.overwrite)).toEqual([
        false,
        true,
      ])
    );
  });

  it("shows each string table entry's translation and opens the dialog from it", async () => {
    // jsdom lays nothing out, and the entries are virtualized: give them a viewport to fill.
    vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockReturnValue(600);
    vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockReturnValue(800);
    mock = tauri(tablePackage())
      .on("table_translations", () => ["Jouer", null])
      .on("text_translation", () => ({
        culture: "en",
        namespace: "Menu_ST",
        key: "Play",
        game: "Jouer",
        held: null,
      }));
    installTauri(mock);
    mount();
    const user = userEvent.setup();
    await user.click(await screen.findByText("Jouer"));
    expect(screen.getByText("shows the source")).toBeTruthy();
    expect(mock.callsTo("table_translations")[0]).toMatchObject({
      namespace: "Menu_ST",
      sources: [
        ["Play", "Play"],
        ["Quit", "Quit"],
      ],
    });
    await screen.findByDisplayValue("Jouer");
    expect(mock.callsTo("text_translation")[0]).toMatchObject({ text: "Menu_ST/Play" });
    vi.restoreAllMocks();
  });
});

/** A skeletal mesh with two material slots, two LODs and three bones. The first LOD lists a hidden
 *  flag for each slot, the second none. */
function meshPackage(hiddenInLod0 = [false, false]) {
  const slot = (at: number, name: string, tags: string[]) => ({
    kind: "struct",
    name: "SkeletalMaterial",
    fields: [
      {
        name: "MaterialInterface",
        span: [at, at + 4],
        value: object(-(at / 100), `/Game/Mesh/MI_${name}.MI_${name}`),
      },
      { name: "MaterialSlotName", span: [at + 4, at + 12], value: { kind: "name", value: name } },
      {
        name: "MaterialTags",
        span: [at + 40, at + 44 + 8 * tags.length],
        value: { kind: "array", items: tags.map((tag) => ({ kind: "name", value: tag })) },
      },
    ],
  });
  const lod = (at: number, flags: boolean[]) => ({
    kind: "struct",
    name: "SkeletalMeshLODInfo",
    fields: [
      {
        name: "DefaultHiddenMaterials",
        span: [at, at + 4 + flags.length],
        value: { kind: "array", items: flags.map((value) => ({ kind: "bool", value })) },
      },
    ],
  });
  const bone = (name: string, parent: number) => ({
    kind: "struct",
    name: "MeshBoneInfo",
    fields: [
      { name: "Name", value: { kind: "name", value: name } },
      { name: "ParentIndex", value: { kind: "int", value: parent } },
    ],
  });
  return {
    ...labelPackage(),
    exports: [
      exp(0, "SK_Test", "SkeletalMesh", [
        {
          name: "LODInfo",
          span: [100, 200],
          value: { kind: "array", items: [lod(110, hiddenInLod0), lod(150, [])] },
        },
        {
          name: "Materials",
          span: [300, 500],
          value: {
            kind: "array",
            items: [slot(300, "Body", ["MaterialTag.A"]), slot(400, "Head", [])],
          },
        },
        {
          name: "RefSkeleton",
          value: {
            kind: "array",
            items: [bone("root", -1), bone("spine_01", 0), bone("head", 1)],
          },
        },
      ]),
    ],
  };
}

describe("AssetInspector mesh slots", () => {
  it("swaps a material, hides a slot in every LOD and adds and drops tags, as one save", async () => {
    mock = tauri(meshPackage());
    installTauri(mock);
    mount();
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "Slots" }));
    expect(await screen.findByText("spine_01")).toBeTruthy();
    expect(screen.getByText("Head")).toBeTruthy();

    await user.click(screen.getByText("/Game/Mesh/MI_Body.MI_Body"));
    const path = await screen.findByDisplayValue("/Game/Mesh/MI_Body.MI_Body");
    await user.clear(path);
    await user.type(path, "/Game/Mesh/MI_Gold.MI_Gold{Enter}");
    await user.click(screen.getByRole("checkbox", { name: "Hide slot 1" }));
    await user.click(screen.getByRole("button", { name: "Remove tag MaterialTag.A" }));
    await user.click(screen.getByRole("button", { name: "Add a tag to slot 1" }));
    await user.type(await screen.findByLabelText("New tag"), "MaterialTag.B{Enter}");
    expect(await screen.findByText("MaterialTag.B")).toBeTruthy();

    await user.click(await screen.findByRole("button", { name: /Save as mod/ }));
    await waitFor(() => expect(lastSave().values).toHaveLength(6));
    const values = lastSave().values as Record<string, unknown>[];
    expect(values).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          offset: 300,
          name: "MaterialInterface",
          op: "set",
          text: "/Game/Mesh/MI_Gold.MI_Gold",
        }),
        expect.objectContaining({
          offset: 110,
          name: "DefaultHiddenMaterials",
          op: "set_element",
          index: 1,
          text: "true",
        }),
        expect.objectContaining({ offset: 340, name: "MaterialTags", op: "remove", index: 0 }),
        expect.objectContaining({ offset: 440, name: "MaterialTags", op: "insert", index: 0 }),
      ])
    );
    // The second LOD lists no flags, so it is given one per slot in the same save.
    expect(
      values.filter((value) => value.offset === 150 && value.op === "insert" && value.index === 0)
    ).toHaveLength(2);
    expect(lastSave().field_sets).toHaveLength(3);
    expect(lastSave().field_sets).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          offset: 440,
          name: "MaterialTags",
          path: ["[0]"],
          text: "MaterialTag.B",
        }),
        expect.objectContaining({
          offset: 150,
          name: "DefaultHiddenMaterials",
          path: ["[0]"],
          text: "false",
        }),
        expect.objectContaining({
          offset: 150,
          name: "DefaultHiddenMaterials",
          path: ["[1]"],
          text: "true",
        }),
      ])
    );
  });

  it("takes one tag change a slot at a time, and leaves nothing pending once a slot is shown again", async () => {
    mock = tauri(meshPackage());
    installTauri(mock);
    mount();
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "Slots" }));
    await user.click(await screen.findByRole("button", { name: "Remove tag MaterialTag.A" }));
    expect(
      (screen.getByRole("button", { name: "Add a tag to slot 0" }) as HTMLButtonElement).disabled
    ).toBe(true);
    await user.click(screen.getByRole("button", { name: "Keep tag MaterialTag.A" }));
    expect(
      (screen.getByRole("button", { name: "Add a tag to slot 0" }) as HTMLButtonElement).disabled
    ).toBe(false);
    await user.click(screen.getByRole("checkbox", { name: "Hide slot 0" }));
    expect(await screen.findByText("2 unsaved changes")).toBeTruthy();
    await user.click(screen.getByRole("checkbox", { name: "Hide slot 0" }));
    await waitFor(() => expect(screen.queryByText(/unsaved change/)).toBeNull());
  });

  it("shows a slot hidden in some LODs as mixed, and hides it in the rest", async () => {
    mock = tauri(meshPackage([true, false]));
    installTauri(mock);
    mount();
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "Slots" }));
    const box = await screen.findByRole("checkbox", { name: "Hide slot 0" });
    expect(box.getAttribute("aria-checked")).toBe("mixed");
    await user.click(box);
    expect(box.getAttribute("aria-checked")).toBe("true");

    await user.click(await screen.findByRole("button", { name: /Save as mod/ }));
    await waitFor(() => expect(lastSave().values).toHaveLength(2));
    expect(lastSave().values).toEqual([
      expect.objectContaining({ offset: 150, op: "insert", index: 0 }),
      expect.objectContaining({ offset: 150, op: "insert", index: 0 }),
    ]);
    expect(lastSave().field_sets).toEqual([
      expect.objectContaining({ offset: 150, path: ["[0]"], text: "true" }),
      expect.objectContaining({ offset: 150, path: ["[1]"], text: "false" }),
    ]);
  });
});

/** One export holding an array of two structs, which opens straight into its property tree. */
function pointsPackage() {
  const point = (x: number, at: number) => ({
    kind: "struct",
    name: "IntPoint",
    fields: [entry("X", { kind: "int", value: x }, at)],
  });
  return {
    ...labelPackage(),
    exports: [
      exp(0, "Settings", "SettingsData", [
        {
          name: "Points",
          span: [40, 52],
          value: { kind: "array", items: [point(1, 44), point(2, 48)] },
        },
      ]),
    ],
  };
}

/** Opens the context menu of the tree row reading `name`. */
async function menuOf(user: ReturnType<typeof userEvent.setup>, name: string) {
  await user.pointer({ keys: "[MouseRight]", target: await screen.findByText(name) });
  return screen.findByRole("menu");
}

/** One export holding a value of each kind typed in a form of its own. */
function referencesPackage() {
  return {
    ...labelPackage(),
    exports: [
      exp(0, "Settings", "SettingsData", [
        entry("Count", { kind: "uint", value: 3 }, 40),
        entry("OnFired", { kind: "delegate", object: "/Game/A.A_C", function: "Handler" }, 50),
        entry("Watched", { kind: "field_path", path: "Count", owner: "/Script/T.Thing" }, 60),
        entry("Lazy", { kind: "lazy_object", guid: "0".repeat(32) }, 70),
      ]),
    ],
  };
}

describe("AssetInspector reference edits", () => {
  it("edits a uint, a delegate, a field path and a lazy object inline", async () => {
    mock = tauri(referencesPackage());
    installTauri(mock);
    mount();
    const user = userEvent.setup();
    const retype = async (shown: string, typed: string) => {
      await user.click(await screen.findByText(shown));
      const input = await screen.findByDisplayValue(shown);
      await user.clear(input);
      await user.type(input, `${typed}{Enter}`);
    };
    await retype("3", "4");
    await retype("/Game/A.A_C::Handler", "None");
    await retype("Count in /Script/T.Thing", "Lazy");
    await retype("0".repeat(32), "1".repeat(32));
    await user.click(await screen.findByRole("button", { name: /Save as mod/ }));
    await waitFor(() =>
      expect(lastSave().values).toEqual(
        expect.arrayContaining([
          { offset: 40, name: "Count", kind: "uint", op: "set", text: "4" },
          { offset: 50, name: "OnFired", kind: "delegate", op: "set", text: "None" },
          { offset: 60, name: "Watched", kind: "field_path", op: "set", text: "Lazy" },
          { offset: 70, name: "Lazy", kind: "lazy_object", op: "set", text: "1".repeat(32) },
        ])
      )
    );
    // Each is held to what it read, in the form it is typed in.
    expect(lastSave().expect).toMatchObject({
      values: {
        "40": "3",
        "50": "/Game/A.A_C::Handler",
        "60": "Count in /Script/T.Thing",
        "70": "0".repeat(32),
      },
    });
  });
});

/** One export holding a map of names to ints and an array of strings. */
function containersPackage() {
  const name = (value: string) => ({ kind: "name", value });
  const int = (value: number) => ({ kind: "int", value });
  return {
    ...labelPackage(),
    exports: [
      exp(0, "Settings", "SettingsData", [
        {
          name: "Scores",
          span: [40, 72],
          value: {
            kind: "map",
            entries: [
              { key: name("A"), value: int(1) },
              { key: name("B"), value: int(2) },
            ],
          },
        },
        {
          name: "Tags",
          span: [80, 100],
          value: {
            kind: "array",
            items: ["a", "b", "c"].map((value) => ({ kind: "str", value })),
          },
        },
      ]),
    ],
  };
}

describe("AssetInspector instanced structs", () => {
  it("gives an instanced struct another type through its type field", async () => {
    mock = tauri({
      ...labelPackage(),
      exports: [
        exp(0, "Settings", "SettingsData", [
          {
            name: "Payload",
            span: [40, 60],
            value: {
              kind: "struct",
              name: "Point",
              fields: [entry("(type)", object(-5, "/Script/Test.Point"), 40)],
            },
          },
        ]),
      ],
    });
    installTauri(mock);
    mount();
    const user = userEvent.setup();
    await user.click(await screen.findByText("/Script/Test.Point"));
    const input = await screen.findByDisplayValue("/Script/Test.Point");
    await user.clear(input);
    await user.type(input, "/Script/CoreUObject.Vector{Enter}");
    await user.click(await screen.findByRole("button", { name: /Save as mod/ }));
    await waitFor(() =>
      expect(lastSave().values).toEqual([
        {
          offset: 40,
          name: "(type)",
          kind: "object",
          op: "set",
          text: "/Script/CoreUObject.Vector",
        },
      ])
    );
  });
});

describe("AssetInspector raw bytes", () => {
  it("replaces a value's bytes starting from what it holds", async () => {
    mock = tauri({
      ...labelPackage(),
      exports: [
        exp(0, "Settings", "SettingsData", [entry("Count", { kind: "int", value: 3 }, 40)]),
      ],
    }).on("value_bytes", () => [3, 0, 0, 0]);
    installTauri(mock);
    mount();
    const user = userEvent.setup();
    const menu = await menuOf(user, "Count");
    await user.click(within(menu).getByRole("menuitem", { name: /Edit bytes/ }));
    const dialog = await screen.findByRole("alertdialog");
    const bytes = await within(dialog).findByDisplayValue("03 00 00 00");
    expect(mock.callsTo("value_bytes")[0]).toMatchObject({ start: 40, end: 44 });
    const replace = within(dialog).getByRole("button", { name: "Replace" }) as HTMLButtonElement;
    expect(replace.disabled).toBe(true);
    await user.clear(bytes);
    await user.type(bytes, "05 00 00 00 00");
    expect(within(dialog).getByText("5 bytes, where it holds 4.")).toBeTruthy();
    await user.click(replace);
    await user.click(await screen.findByRole("button", { name: /Save as mod/ }));
    await waitFor(() =>
      expect(lastSave().values).toEqual([
        { offset: 40, name: "Count", kind: "int", op: "set_raw", hex: "05 00 00 00 00" },
      ])
    );
    // Held to the bytes it replaces, not to what they read as.
    expect(lastSave().expect).toMatchObject({ values: { "40": "03000000" } });
  });
});

describe("AssetInspector keys and moves", () => {
  it("renames a map key from its own row", async () => {
    mock = tauri(containersPackage());
    installTauri(mock);
    mount();
    const user = userEvent.setup();
    await user.click(await screen.findByText("[0] A"));
    await user.click(await screen.findByText("A"));
    const input = await screen.findByDisplayValue("A");
    await user.clear(input);
    await user.type(input, "Z{Enter}");
    await user.click(await screen.findByRole("button", { name: /Save as mod/ }));
    await waitFor(() =>
      expect(lastSave().values).toEqual([
        { offset: 40, name: "Scores", kind: "map", op: "set_key", index: 0, text: "Z" },
      ])
    );
    expect(lastSave().expect).toMatchObject({ values: { "40[0].key": "A" } });
  });

  it("moves elements and saves one reorder", async () => {
    mock = tauri(containersPackage());
    installTauri(mock);
    mount();
    const user = userEvent.setup();
    let menu = await menuOf(user, "[2]");
    await user.click(within(menu).getByRole("menuitem", { name: /Move up/ }));
    menu = await menuOf(user, "moved from [2]");
    await user.click(within(menu).getByRole("menuitem", { name: /Move up/ }));
    expect(await screen.findByText("moved from [1]")).toBeTruthy();
    await user.click(await screen.findByRole("button", { name: /Save as mod/ }));
    await waitFor(() =>
      expect(lastSave().values).toEqual([
        { offset: 80, name: "Tags", kind: "array", op: "reorder", order: [2, 0, 1] },
      ])
    );
    // Held to the elements it moved, each as it is typed.
    expect(lastSave().expect).toMatchObject({ values: { "80": '[["a"],["b"],["c"]]' } });
  });
});

describe("AssetInspector container edits", () => {
  it.each([
    ["Remove this element", "[1]", { op: "remove", index: 1 }],
    ["Duplicate element", "[0]", { op: "insert", index: 0 }],
  ])("%s works on an element that is a struct", async (item, row, op) => {
    mock = tauri(pointsPackage());
    installTauri(mock);
    mount();
    const user = userEvent.setup();
    const menu = await menuOf(user, row);
    await user.click(within(menu).getByRole("menuitem", { name: new RegExp(item) }));
    await user.click(await screen.findByRole("button", { name: /Save as mod/ }));
    await waitFor(() =>
      expect(lastSave().values).toEqual([{ offset: 40, name: "Points", kind: "array", ...op }])
    );
  });

  it("will not add or drop an element while a value inside the container is edited", async () => {
    mock = tauri(pointsPackage());
    installTauri(mock);
    mount();
    const user = userEvent.setup();
    await user.click(await screen.findByText("[0]"));
    await user.click(await screen.findByText("1"));
    const input = await screen.findByDisplayValue("1");
    await user.clear(input);
    await user.type(input, "5{Enter}");
    const menu = await menuOf(user, "[1]");
    expect(
      within(menu)
        .getByRole("menuitem", { name: /Remove this element/ })
        .getAttribute("aria-disabled")
    ).toBe("true");
  });
});

const BRANCH = "Jump @0139 unless LocalVariable(bOk)";
const CALL = "CallMath /Script/Engine.KismetMathLibrary:Greater_IntInt(LocalVariable(Count), 0)";

/** One function with a branch and a call, which opens straight into its script. */
function functionPackage() {
  return {
    package_name: PACKAGE,
    cooked: true,
    unversioned_properties: true,
    name_count: 0,
    import_count: 0,
    export_count: 1,
    names: [],
    imports: [],
    exports: [
      {
        ...exp(0, "IsActiveAbility", "Function"),
        status: { state: "payload", consumed: 8, payload_bytes: 316, kind: "bytecode" },
        script: { buffer_size: 316, storage_size: 456, decoded_size: 316, statements: [] },
      },
    ],
    unresolved_structs: [],
    resources: [],
  };
}

function scriptView() {
  return {
    lines: [
      {
        offset: 0x55,
        text: BRANCH,
        targets: [0x139],
        expressions: [{ at: 0x55, kind: "condition", token: "JumpIfNot", text: BRANCH }],
      },
      {
        offset: 0x104,
        text: `LetBool LocalVariable(Greater) = ${CALL}`,
        expressions: [
          {
            at: 0x10e,
            kind: "call",
            token: "CallMath",
            text: CALL,
            value: "/Script/Engine.KismetMathLibrary:Greater_IntInt",
          },
        ],
      },
    ],
    entries: [],
    signature: null,
    signature_text: null,
    callers: [],
    complete: true,
    stopped: null,
    resize_lock: null,
    buffer_size: 316,
    storage_size: 456,
    statements: 2,
  };
}

describe("AssetInspector without a mappings file", () => {
  /** A tagged asset whose function layout the reader left unexplained. */
  function partialPackage() {
    return {
      ...blueprint(),
      unversioned_properties: false,
      exports: [
        {
          ...exp(0, "CollectPaks", "Function"),
          status: { state: "partial", consumed: 12, expected: 1928 },
        },
      ],
    };
  }

  const noMappings = {
    path: null,
    loaded: false,
    struct_count: 0,
    enum_count: 0,
    error: "no .usmap mappings file is set.",
  };

  it("says unexplained bytes come from the missing mappings file, and offers to set one", async () => {
    mock = tauri(partialPackage()).on("get_mappings_status", () => noMappings);
    installTauri(mock);
    mount();
    expect(await screen.findByText(/No mappings file is loaded/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "Set the mappings file" })).toBeTruthy();
  });

  it("says nothing about mappings when one is loaded", async () => {
    mock = tauri(partialPackage());
    installTauri(mock);
    mount();
    await screen.findAllByText(/bytes unexplained/);
    await waitFor(() => expect(mock.callsTo("get_mappings_status")).toHaveLength(1));
    expect(screen.queryByText(/No mappings file is loaded/)).toBeNull();
  });
});

describe("AssetInspector script edits", () => {
  it("shows the labels code is jumped to, and what an unresolved call probably was", async () => {
    const view = scriptView();
    mock = tauri(functionPackage()).on("export_script_view", () => ({
      ...view,
      lines: [
        view.lines[0],
        {
          ...view.lines[1],
          labels: [{ name: "0104", note: "ReceiveBeginPlay enters here" }],
          note: "probably Greater_IntInt",
        },
      ],
      end_labels: [{ name: "0139" }],
    }));
    installTauri(mock);
    mount();
    expect(await screen.findByText("ReceiveBeginPlay enters here")).toBeTruthy();
    expect(screen.getByText("@0104:")).toBeTruthy();
    expect(screen.getByText("@0139:")).toBeTruthy();
    expect(screen.getByText("; probably Greater_IntInt")).toBeTruthy();
  });

  it("reads a function as pseudo-code and opens a line's statement in the listing", async () => {
    mock = tauri(functionPackage()).on("export_script_view", () => ({
      ...scriptView(),
      flow: {
        lines: [
          { depth: 0, text: "if (LocalVariable(Flag)) {", offsets: [0x55] },
          { depth: 1, text: "goto @0139", offsets: [0x60] },
          { depth: 0, text: "}" },
          { depth: 0, text: "@0104:  // ReceiveBeginPlay enters here" },
          { depth: 0, text: `LetBool LocalVariable(Greater) = ${CALL}`, offsets: [0x104] },
        ],
        gotos: 1,
      },
    }));
    installTauri(mock);
    mount();
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "Pseudo-code" }));
    expect(screen.getByText("1 jump could not be nested and stay as goto.")).toBeTruthy();
    expect(screen.getByText("(LocalVariable(Flag)) {", { exact: false })).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "@0139" }));
    expect(screen.getByRole("button", { name: "Listing" })).toBeTruthy();
    await user.click(screen.getByText(`LetBool LocalVariable(Greater) = ${CALL}`));
    expect(await screen.findByRole("button", { name: "always false" })).toBeTruthy();
  });

  it("fixes a branch's condition by where it starts, holding it to what it read", async () => {
    mock = tauri(functionPackage()).on("export_script_view", () => scriptView());
    installTauri(mock);
    mount();
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "always false" }));
    await user.click(await screen.findByRole("button", { name: /Save as mod/ }));
    await waitFor(() =>
      expect(lastSave().scripts).toEqual([
        { export: 0, statement: 0x55, constant: 0, at: 0x55, value: "false", was: BRANCH },
      ])
    );
  });

  it("offers a Niagara script's VM program, and only for a Niagara script", async () => {
    const niagara = {
      ...functionPackage(),
      exports: [exp(0, "SpawnScript_1", "NiagaraScript")],
    };
    mock = tauri(niagara).on("export_vm_view", () => ({
      object_name: "SpawnScript_1",
      usage: "ParticleSpawnScript",
      interpolated: false,
      param_size: 4,
      bytecode_len: 1,
      literals: [],
      program: { instructions: [{}] },
      lines: [{ at: 0, text: "done", constants: [] }],
      traces: [],
    }));
    installTauri(mock);
    mount();
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "VM" }));
    expect(await screen.findByText("done")).toBeTruthy();
    expect(mock.callsTo("export_vm_view")).toEqual([
      { gameRoot: GAME, container: CONTAINER, entry: ENTRY, export: 0 },
    ]);
  });

  it("offers no VM tab for a Blueprint function", async () => {
    mock = tauri(functionPackage()).on("export_script_view", () => scriptView());
    installTauri(mock);
    mount();
    await screen.findByRole("button", { name: "Script" });
    expect(screen.queryByRole("button", { name: "VM" })).toBeNull();
  });

  it("shows whether a retargeted call fits the function it now names", async () => {
    mock = tauri(functionPackage())
      .on("export_script_view", () => scriptView())
      .on("script_call_preview", () => ({
        callee: "/Script/Engine.KismetMathLibrary:Greater_IntInt",
        was: "(Int, Int) -> Bool",
        now: "(Str) -> Text",
        verdict: "mismatch",
        reason: "argument 0 is a Int, and the new function takes a Str there",
      }));
    installTauri(mock);
    mount();
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: /Greater_IntInt/ }));
    const input = await screen.findByDisplayValue(
      "/Script/Engine.KismetMathLibrary:Greater_IntInt"
    );
    await user.clear(input);
    await user.type(input, "/Script/Engine.KismetTextLibrary:Conv_StringToText{Enter}");
    await user.hover(await screen.findByLabelText("call does not fit"));
    expect(await screen.findAllByText(/\(Str\) -> Text/)).not.toHaveLength(0);
    expect(await screen.findAllByText(/argument 0 is a Int/)).not.toHaveLength(0);
  });

  it("asks before saving a call it could not check, and saves it when told to", async () => {
    let tries = 0;
    mock = tauri(functionPackage())
      .on("export_script_view", () => scriptView())
      .on("script_call_preview", () => ({
        callee: "/Script/Engine.KismetMathLibrary:Greater_IntInt",
        was: "(Int, Int) -> Bool",
        now: null,
        verdict: "unknown",
        reason: "how it is called could not be read",
      }))
      .on("save_asset_edits", () => {
        tries += 1;
        if (tries === 1)
          return Promise.reject(
            "These edits point a script at something that could not be confirmed to fit it: Less"
          );
        return {
          outcome: "written",
          message: "Saved",
          pak: "TestMod_9999999_P.utoc",
          warnings: [],
        };
      });
    installTauri(mock);
    mount();
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: /Greater_IntInt/ }));
    const input = await screen.findByDisplayValue(
      "/Script/Engine.KismetMathLibrary:Greater_IntInt"
    );
    await user.clear(input);
    await user.type(input, "/Script/Engine.KismetMathLibrary:Less_IntInt{Enter}");
    await screen.findByLabelText("call not confirmed");
    expect(mock.callsTo("script_call_preview")[0]).toMatchObject({
      export: 0,
      statement: 0x104,
      at: 0x10e,
      value: "/Script/Engine.KismetMathLibrary:Less_IntInt",
    });
    await user.click(await screen.findByRole("button", { name: /Save as mod/ }));
    const dialog = await screen.findByRole("alertdialog", { name: /could not be checked/ });
    await user.click(within(dialog).getByRole("button", { name: "Save anyway" }));
    await waitFor(() => expect(mock.callsTo("save_asset_edits")).toHaveLength(2));
    expect(mock.callsTo("save_asset_edits")[1]).toMatchObject({ allowUnchecked: true });
    expect(lastSave().scripts).toEqual([
      {
        export: 0,
        statement: 0x104,
        constant: 0,
        at: 0x10e,
        value: "/Script/Engine.KismetMathLibrary:Less_IntInt",
        was: CALL,
      },
    ]);
  });
});

describe("AssetInspector script text", () => {
  const PRINTED = `${BRANCH}\nReturn Nothing\n@0139:\nEndOfScript\n`;
  const assembles = {
    diagnostics: [],
    warnings: [],
    applied: [
      { name: "IsActiveAbility script text", before: "3 statement(s)", after: "4 statement(s)" },
    ],
  };

  function textView() {
    return { ...scriptView(), text: PRINTED };
  }

  /** Puts the caret at the very start of the text and types there. */
  async function typeAtStart(user: ReturnType<typeof userEvent.setup>, text: string) {
    const content = await waitFor(() => {
      const found = document.querySelector("[data-testid=script-text] .cm-content");
      expect(found, "the editor should be mounted").not.toBeNull();
      return found as HTMLElement;
    });
    content.focus();
    await user.keyboard("{Control>}{Home}{/Control}");
    await user.keyboard(text);
  }

  it("writes a function from text and saves it with the text it printed", async () => {
    mock = tauri(functionPackage())
      .on("export_script_view", () => textView())
      .on("assemble_preview", () => assembles);
    installTauri(mock);
    mount();
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: /Edit as text/ }));
    await typeAtStart(user, "Return Nothing{Enter}");
    expect(await screen.findByText(/Assembles\. Save as mod to write it/)).toBeTruthy();
    expect(screen.getByText(/3 statement\(s\) → 4 statement\(s\)/)).toBeTruthy();
    await user.click(await screen.findByRole("button", { name: /Save as mod/ }));
    await waitFor(() =>
      expect(lastSave().script_texts).toEqual([
        { export: 0, text: `Return Nothing\n${PRINTED}`, was: PRINTED },
      ])
    );
  });

  it("adds a function from a name, a signature and its text, as a save of its own", async () => {
    mock = tauri(functionPackage())
      .on("export_script_view", () => textView())
      .on("new_function_preview", () => ({
        diagnostics: [],
        warnings: [],
        applied: [
          {
            name: "BP_Test_C function Glow",
            before: "(none)",
            after: "Glow(Strength: Float) as export 9",
          },
        ],
      }));
    installTauri(mock);
    mount();
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: /New function/ }));
    await user.type(screen.getByLabelText("New function name"), "Glow");
    const signature = screen.getByLabelText("New function signature");
    await user.clear(signature);
    await user.type(signature, "(Strength: Float)");
    await user.click(screen.getByRole("button", { name: /Write its script/ }));
    expect(await screen.findByText(/Assembles\. Add function to write it/)).toBeTruthy();
    expect(mock.callsTo("new_function_preview")[0]).toMatchObject({
      name: "Glow",
      signature: "(Strength: Float)",
    });
    await user.click(screen.getByRole("button", { name: /Add function/ }));
    await waitFor(() =>
      expect(lastSave().script_texts).toEqual([
        {
          new_function: "Glow",
          signature: "(Strength: Float)",
          text: "Return Nothing\nEndOfScript\n",
        },
      ])
    );
  });

  it("adds a variable to the class from a name and a type, as a save of its own", async () => {
    mock = tauri(functionPackage()).on("export_script_view", () => textView());
    installTauri(mock);
    mount();
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: /New variable/ }));
    const add = screen.getByRole("button", { name: /Add variable/ }) as HTMLButtonElement;
    expect(add.disabled).toBe(true);
    await user.type(screen.getByLabelText("New variable name"), "Charges");
    await user.type(screen.getByLabelText("New variable type"), "Int");
    await user.click(add);
    await waitFor(() =>
      expect(lastSave().add_variables).toEqual([{ name: "Charges", type: "Int" }])
    );
    expect(screen.queryByLabelText("New variable name")).toBeNull();
  });

  it("marks the lines the assembler refuses and lists why", async () => {
    mock = tauri(functionPackage())
      .on("export_script_view", () => textView())
      .on("assemble_preview", () => ({
        diagnostics: [{ line: 1, column: 6, message: "expected a label such as @0045" }],
        warnings: [],
        applied: [],
      }));
    installTauri(mock);
    mount();
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: /Edit as text/ }));
    await typeAtStart(user, "x");
    expect(await screen.findByText("line 1:6: expected a label such as @0045")).toBeTruthy();
    await waitFor(() => expect(document.querySelector(".cm-st-error-line")).not.toBeNull());
  });

  it("locks the expressions while the function is written from text, and frees them on discard", async () => {
    mock = tauri(functionPackage())
      .on("export_script_view", () => textView())
      .on("assemble_preview", () => assembles);
    installTauri(mock);
    mount();
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: /Edit as text/ }));
    await typeAtStart(user, "Return Nothing{Enter}");
    await screen.findByText(/Assembles\./);
    await user.click(screen.getByRole("button", { name: /Listing/ }));
    expect(await screen.findByText("written from text, not saved yet")).toBeTruthy();
    expect(
      (screen.getByRole("button", { name: "always false" }) as HTMLButtonElement).disabled
    ).toBe(true);
    await user.click(screen.getByRole("button", { name: /Edit as text/ }));
    await user.click(await screen.findByRole("button", { name: /Discard text/ }));
    await waitFor(() =>
      expect(
        (screen.getByRole("button", { name: "always false" }) as HTMLButtonElement).disabled
      ).toBe(false)
    );
  });

  it("will not write a function from text over edits inside it", async () => {
    mock = tauri(functionPackage()).on("export_script_view", () => textView());
    installTauri(mock);
    mount();
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "always false" }));
    await waitFor(() =>
      expect(
        (screen.getByRole("button", { name: /Edit as text/ }) as HTMLButtonElement).disabled
      ).toBe(true)
    );
  });
});

/** A package holding one texture. */
function texturePackage() {
  return {
    ...blueprint(),
    exports: [exp(0, "T_Test", "Texture2D")],
  };
}

const TEXTURE_INFO = {
  export: 0,
  name: "T_Test",
  layout: {
    class: "Texture2D",
    platforms: [
      {
        format: "PF_DXT1",
        size_x: 256,
        size_y: 256,
        slices: 1,
        cubemap: false,
        first_mip: 0,
        mips: [
          { size_x: 256, size_y: 256, size_z: 1, placement: "uptnl", size: 32768 },
          { size_x: 128, size_y: 128, size_z: 1, placement: "inline", size: 8192 },
        ],
      },
    ],
  },
  absent_mips: [0],
};

function textureTauri(): TauriMock {
  return tauri(texturePackage())
    .on("texture_info", () => TEXTURE_INFO)
    .on("texture_preview", () => new ArrayBuffer(8))
    .on("export_texture", ({ path }) => ({
      path,
      export: 0,
      mip: 1,
      mips: 1,
      format: "PF_DXT1",
      width: 128,
      height: 128,
    }));
}

describe("AssetInspector textures", () => {
  it("opens a texture on its mips, and saves one as the image file chosen", async () => {
    mock = textureTauri();
    installTauri(mock);
    dialogStub.save.mockResolvedValueOnce("C:\\Out\\T_Test.dds");
    mount();
    const user = userEvent.setup();
    expect(await screen.findByText("PF_DXT1, 256x256, 2 mip(s)")).toBeTruthy();
    expect(screen.getByText("not installed")).toBeTruthy();

    await user.click(screen.getByRole("button", { name: "Export…" }));
    await waitFor(() => expect(mock.callsTo("export_texture")).toHaveLength(1));
    expect(mock.callsTo("export_texture")[0]).toEqual({
      gameRoot: GAME,
      container: CONTAINER,
      entry: ENTRY,
      export: 0,
      mip: null,
      path: "C:\\Out\\T_Test.dds",
    });
    expect(await screen.findByText(/Saved mip 1, 128x128/)).toBeTruthy();
  });

  it("queues an image as the texture's new pixels, and the save sends it", async () => {
    mock = textureTauri();
    installTauri(mock);
    dialogStub.open.mockResolvedValueOnce("C:\\In\\T_New.png");
    mount();
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "Replace…" }));
    expect(await screen.findByText("C:\\In\\T_New.png")).toBeTruthy();

    await user.click(await screen.findByRole("button", { name: /Save as mod/ }));
    await waitFor(() =>
      expect(lastSave().textures).toEqual([
        { export: 0, image: "C:\\In\\T_New.png", quality: "default" },
      ])
    );
    expect(lastSave().values).toEqual([]);
  });
});

/** A lone colour curve, two RGBA samples, which opens on the Colour view. */
function curvePackage() {
  const lut = [2, 0.5, 0.25, 1, 1, 1, 1, 0].map((value) => ({ kind: "float", value }));
  return {
    ...blueprint(),
    exports: [
      exp(0, "NiagaraDataInterfaceColorCurve_0", "NiagaraDataInterfaceColorCurve", [
        { name: "ShaderLUT", value: { kind: "array", items: lut }, span: [100, 140] },
      ]),
    ],
  };
}

const CURVE_REPORT = {
  sources: [
    {
      kind: "lut",
      export: 0,
      object_name: "NiagaraDataInterfaceColorCurve_0",
      class_name: "NiagaraDataInterfaceColorCurve",
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
    },
  ],
  gpu_scripts: [],
  renderers: [],
};

const PLANNED = [
  { export: "0", path: "ShaderLUT[0]", op: "set", text: "1", was: "2" },
  { export: "0", path: "ShaderLUT[2]", op: "set", text: "0.5", was: "0.25" },
];

function curveTauri(): TauriMock {
  return tauri(curvePackage())
    .on("niagara_colors", () => CURVE_REPORT)
    .on("recolor_colors", ({ colors }) =>
      (colors as number[][]).map(([r, g, b, a]) => [r * 0.5, g, b * 2, a])
    )
    .on("plan_recolor", () => ({ paths: PLANNED, sources: 1, scripts: 0, notes: [] }));
}

describe("AssetInspector colours", () => {
  it("opens a colour curve on the Colour view and saves a tool's change as element edits", async () => {
    mock = curveTauri();
    installTauri(mock);
    mount();
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "Apply to this curve" }));
    await user.click(await screen.findByRole("button", { name: /Save as mod/ }));
    await waitFor(() =>
      expect(lastSave().values).toEqual([
        { offset: 100, name: "ShaderLUT", kind: "array", op: "set_element", index: 0, text: "1" },
        { offset: 100, name: "ShaderLUT", kind: "array", op: "set_element", index: 2, text: "0.5" },
        { offset: 100, name: "ShaderLUT", kind: "array", op: "set_element", index: 4, text: "0.5" },
        { offset: 100, name: "ShaderLUT", kind: "array", op: "set_element", index: 6, text: "2" },
      ])
    );
  });

  it("recolours the whole effect as the planned path edits in one save", async () => {
    mock = curveTauri();
    installTauri(mock);
    mount();
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "Recolour effect" }));
    await user.click(screen.getByRole("button", { name: "Recolour and save" }));
    await waitFor(() => expect(mock.callsTo("save_asset_edits")).toHaveLength(1));
    expect(mock.callsTo("plan_recolor")[0]).toMatchObject({
      container: CONTAINER,
      entry: ENTRY,
      pick: {},
      tool: { tool: "paint", rgb: [0.6, 0.2, 1] },
    });
    expect(lastSave().paths).toEqual(PLANNED);
    expect(lastSave().values).toEqual([]);
  });
});
