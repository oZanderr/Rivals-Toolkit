import { render, screen, waitFor, within } from "@testing-library/react";
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
