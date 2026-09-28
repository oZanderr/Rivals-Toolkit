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
    dialog = await screen.findByRole("alertdialog", { name: /points at nothing/ });
    await user.click(within(dialog).getByRole("button", { name: "Add anyway" }));
    await waitFor(() => expect(mock.callsTo("save_asset_edits")).toHaveLength(2));
    expect(mock.callsTo("save_asset_edits")[1]).toMatchObject({ allowMissing: true });
  });
});
