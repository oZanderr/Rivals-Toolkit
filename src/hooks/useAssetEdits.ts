import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";

import { invoke } from "@tauri-apps/api/core";

import type { PropertyEntry, PropertyValue } from "@/components/AssetInspector";
import { emitModsChanged } from "@/lib/modsEvents";

/**
 * A queued change. Sending a value back to its default is not the same as emptying it, and a
 * container is changed by adding or dropping an element rather than by retyping the whole thing.
 * The row drafts belong to a target naming a table row: an add or a copy is keyed by the new
 * row's name, a removal or a rename by the row it acts on.
 */
export type Draft =
  | { op: "set"; text: string }
  | { op: "clear" }
  | { op: "insert"; index: number; key?: string }
  | { op: "remove"; index: number }
  | { op: "set_element"; index: number; text: string }
  | { op: "store" }
  | { op: "unset" }
  | { op: "key_add"; time: number; value: number }
  | { op: "key_duplicate"; index: number; time: number }
  | { op: "key_move"; index: number; time: number }
  | { op: "key_remove"; index: number }
  | { op: "payload_replace"; file: string }
  | { op: "bulk_replace"; file: string }
  | { op: "script_set"; text: string }
  | { op: "row_add"; at: number }
  | { op: "row_duplicate"; source: string; at: number }
  | { op: "row_remove" }
  | { op: "row_rename"; to: string }
  | { op: "string_set_key"; to: string }
  | { op: "string_set_source"; to: string }
  | { op: "string_set_tag"; to: string }
  | { op: "string_meta_set"; id: string; to: string }
  | { op: "string_meta_remove"; id: string }
  | { op: "string_add"; source: string }
  | { op: "string_remove" };

/**
 * Where an edit lands, in the terms the backend resolves it by: the value's offset, its name and
 * static-array slot (a value holding its default shares its offset with the one stored next, so
 * the name is what tells them apart), and the kind the backend checks before writing. An element
 * edit addresses its container and carries the element index, so its kind is the container's.
 */
export interface EditTarget {
  offset: number;
  name: string;
  element?: number;
  kind: string;
  index?: number;
  /** Sets a key draft apart from the channel's own and from each other, so one channel can carry
   *  several. */
  field?: string;
  /** A DataTable row rather than a value. Rows are addressed by name, which is unique in a table. */
  row?: { export: number; name: string };
  /** A StringTable entry rather than a value: its position and the key read there, or for an
   *  added entry the new key alone. `field` sets a tag or metadata draft apart from the entry's
   *  own, so one entry can carry several. */
  string?: { export: number; index: number | null; key: string; field?: string };
  /** An export's payload bytes rather than a value. */
  payload?: { export: number };
  /** One bulk data resource's bytes rather than a value. */
  bulk?: { resource: number };
  /** A constant inside a function's bytecode: the export, the statement's offset, and which
   *  literal in that statement. */
  script?: { export: number; statement: number; constant: number };
  /** The value as it read when the draft was made, so a save refuses one that has since changed. */
  was?: string;
}

/** A value as an edit types it, for the kinds the backend can compare that way. */
function valueText(value: PropertyValue | undefined): string | undefined {
  switch (value?.kind) {
    case "str":
    case "name":
      return value.value;
    case "soft_object":
      return value.path;
    case "text":
      return value.parts?.length ? undefined : value.value;
    case "object":
      return value.path ?? String(value.index);
    case "bool":
    case "float":
      return String(value.value);
    case "int":
    case "uint":
    case "byte":
      // Past 2^53 a number no longer reads back exactly.
      return Number.isSafeInteger(value.value) ? String(value.value) : undefined;
    case "enum":
      return value.name ?? String(value.value);
    default:
      return undefined;
  }
}

export interface DraftRecord {
  target: EditTarget;
  draft: Draft;
  /** Keys of the containers this value sits inside, outermost first, so a change to one of their
   *  element counts can find and discard it. */
  within: string[];
}

/** A separator no property name can contain. */
const SEP = String.fromCharCode(0);

export function draftKey(target: EditTarget): string {
  if (target.payload) return ["payload", target.payload.export].join(SEP);
  if (target.bulk) return ["bulk", target.bulk.resource].join(SEP);
  if (target.script) {
    const { export: exportIndex, statement, constant } = target.script;
    return ["script", exportIndex, statement, constant].join(SEP);
  }
  if (target.row) return ["row", target.row.export, target.row.name].join(SEP);
  if (target.string) {
    const { export: exportIndex, index, key, field } = target.string;
    return ["str", exportIndex, index === null ? `add${SEP}${key}` : index, field ?? ""].join(SEP);
  }
  const base = [target.offset, target.name, target.element ?? "", target.field ?? ""].join(SEP);
  return target.index === undefined ? base : `${base}${SEP}[${target.index}]`;
}

/** An entry can be edited only where the reader recorded its bytes. */
export function entryTarget(entry: PropertyEntry): EditTarget | null {
  if (!entry.span) return null;
  return {
    offset: entry.span[0],
    name: entry.name,
    element: entry.element,
    kind: entry.value.kind,
    was: valueText(entry.value),
  };
}

/** Elements are addressed through their container. For a map the element is the pair's value; the
 *  backend writes it after the key. */
export function elementTarget(container: PropertyEntry, index: number): EditTarget | null {
  const base = entryTarget(container);
  if (!base) return null;
  const value = container.value;
  const element =
    value.kind === "array" || value.kind === "set"
      ? value.items[index]
      : value.kind === "map"
        ? value.entries[index]?.value
        : null;
  if (element === null) return null;
  return { ...base, index, was: valueText(element) };
}

/** The payload export `exportIndex` carries after its properties. */
export function payloadTarget(exportIndex: number): EditTarget {
  return { offset: 0, name: "payload", kind: "payload", payload: { export: exportIndex } };
}

/** Bulk data resource `resource` of the package. */
export function bulkTarget(resource: number): EditTarget {
  return { offset: 0, name: "bulk", kind: "bulk", bulk: { resource } };
}

/** Literal `constant` of the statement at `statement` in the bytecode of export `exportIndex`. */
export function scriptTarget(exportIndex: number, statement: number, constant: number): EditTarget {
  return {
    offset: 0,
    name: "script",
    kind: "script",
    script: { export: exportIndex, statement, constant },
  };
}

/** The row `name` of the DataTable at `exportIndex`, whether it exists yet or is being added. */
export function rowTarget(exportIndex: number, name: string): EditTarget {
  return { offset: 0, name, kind: "row", row: { export: exportIndex, name } };
}

/** Entry `index` of the StringTable at `exportIndex`, keyed `key` as it reads now; `null` for an
 *  entry being added under `key`. `field` names the tag or a metadata id the draft is about. */
export function stringTarget(
  exportIndex: number,
  index: number | null,
  key: string,
  field?: string
): EditTarget {
  return {
    offset: 0,
    name: key,
    kind: "string",
    string: { export: exportIndex, index, key, field },
  };
}

/** Adding or dropping an element moves every byte after it inside the container, and removing a
 *  row deletes every byte in it. */
export function isStructural(draft: Draft): boolean {
  return draft.op === "insert" || draft.op === "remove" || draft.op === "row_remove";
}

/**
 * What the editing surfaces share. Kept small and stable on purpose: the tree of a large table
 * renders every row, and each one re-renders whenever this value changes.
 */
export interface EditSession {
  drafts: Readonly<Record<string, DraftRecord>>;
  /** Why nothing in this export can be edited, or null when it can. */
  locked: string | null;
  setDraft(target: EditTarget, draft: Draft, within: string[]): void;
  dropDraft(key: string): void;
}

const READ_ONLY: EditSession = {
  drafts: {},
  locked: "This view is read-only.",
  setDraft() {},
  dropDraft() {},
};

export const EditSessionContext = createContext<EditSession>(READ_ONLY);

export function useEditSession(): EditSession {
  return useContext(EditSessionContext);
}

/** Where a value edit points, shared by every op. */
interface EditTargetFields {
  offset: number;
  name: string;
  element?: number;
  kind: string;
}

/** One value change, in the form the engine reads from a save and from an edit file alike. */
type ValueEdit = EditTargetFields &
  (
    | { op: "set"; text: string }
    | { op: "clear" }
    | { op: "store" }
    | { op: "unset" }
    | { op: "set_element"; index: number; text: string }
    | { op: "insert"; index: number; key?: string }
    | { op: "remove"; index: number }
  );

function toValueEdit({ target, draft }: DraftRecord): ValueEdit | null {
  const at: EditTargetFields = {
    offset: target.offset,
    name: target.name,
    element: target.element,
    kind: target.kind,
  };
  switch (draft.op) {
    case "set":
      return { ...at, op: "set", text: draft.text };
    case "clear":
    case "store":
    case "unset":
      return { ...at, op: draft.op };
    case "set_element":
      return { ...at, op: "set_element", index: draft.index, text: draft.text };
    case "insert":
      return { ...at, op: "insert", index: draft.index, key: draft.key };
    case "remove":
      return { ...at, op: "remove", index: draft.index };
    default:
      return null;
  }
}

/** One DataTable row change. `name` is the row the operation is about, except for a rename, where
 *  `to` carries the new one. */
type RowEdit = { export: number } & (
  | { op: "add"; name: string; at?: number }
  | { op: "duplicate"; source: string; name: string; at?: number }
  | { op: "remove"; name: string }
  | { op: "rename"; name: string; to: string }
);

/** One string table change. An existing entry is addressed by position plus the key seen there,
 *  which is what catches a stale read. */
type StringEdit = { export: number } & (
  | { op: "set_key" | "set_source" | "set_tag"; index: number; key: string; to: string }
  | { op: "set_meta_data"; index: number; key: string; id: string; to: string }
  | { op: "remove_meta_data"; index: number; key: string; id: string }
  | { op: "add"; key: string; source: string }
  | { op: "remove"; index: number; key: string }
);

function toStringEdit({ target, draft }: DraftRecord): StringEdit | null {
  if (!target.string) return null;
  const { export: exportIndex, index, key } = target.string;
  switch (draft.op) {
    case "string_set_key":
      return index === null
        ? null
        : { export: exportIndex, op: "set_key", index, key, to: draft.to };
    case "string_set_source":
      return index === null
        ? null
        : { export: exportIndex, op: "set_source", index, key, to: draft.to };
    case "string_set_tag":
      return index === null
        ? null
        : { export: exportIndex, op: "set_tag", index, key, to: draft.to };
    case "string_meta_set":
      return index === null
        ? null
        : { export: exportIndex, op: "set_meta_data", index, key, id: draft.id, to: draft.to };
    case "string_meta_remove":
      return index === null
        ? null
        : { export: exportIndex, op: "remove_meta_data", index, key, id: draft.id };
    case "string_add":
      return { export: exportIndex, op: "add", key, source: draft.source };
    case "string_remove":
      return index === null ? null : { export: exportIndex, op: "remove", index, key };
    default:
      return null;
  }
}

function toRowEdit({ target, draft }: DraftRecord): RowEdit | null {
  if (!target.row) return null;
  const { export: exportIndex, name } = target.row;
  switch (draft.op) {
    case "row_add":
      return { export: exportIndex, op: "add", name, at: draft.at };
    case "row_duplicate":
      return { export: exportIndex, op: "duplicate", source: draft.source, name, at: draft.at };
    case "row_remove":
      return { export: exportIndex, op: "remove", name };
    case "row_rename":
      return { export: exportIndex, op: "rename", name, to: draft.to };
    default:
      return null;
  }
}

/** One MovieScene channel key change, addressed like a value edit. */
type KeyEdit = { offset: number; name: string; element?: number } & (
  | { op: "add"; time: number; value: number }
  | { op: "duplicate" | "move"; index: number; time: number }
  | { op: "remove"; index: number }
);

export function isKeyDraft(draft: Draft): boolean {
  return (
    draft.op === "key_add" ||
    draft.op === "key_duplicate" ||
    draft.op === "key_move" ||
    draft.op === "key_remove"
  );
}

function toKeyEdit({ target, draft }: DraftRecord): KeyEdit | null {
  const base = { offset: target.offset, name: target.name, element: target.element };
  switch (draft.op) {
    case "key_add":
      return { ...base, op: "add", time: draft.time, value: draft.value };
    case "key_duplicate":
      return { ...base, op: "duplicate", index: draft.index, time: draft.time };
    case "key_move":
      return { ...base, op: "move", index: draft.index, time: draft.time };
    case "key_remove":
      return { ...base, op: "remove", index: draft.index };
    default:
      return null;
  }
}

/** A payload replacement: the file whose bytes go in, read by the backend at save time. */
interface PayloadEdit {
  export: number;
  file: string;
}

interface BulkEdit {
  resource: number;
  file: string;
}

/** A change to the import table. `import` is the table position, not the package index. */
type ImportEdit =
  | { op: "retarget"; import: number; path: string; class?: [string, string] }
  | { op: "add"; path: string; class_package?: string; class_name?: string }
  | { op: "remove"; import: number };

/** The four preload dependency runs an export declares, as package indices: an export's position
 *  plus one, or minus an import's position plus one. */
export interface DependencyRuns {
  serialize_before_serialize: number[];
  create_before_serialize: number[];
  serialize_before_create: number[];
  create_before_create: number[];
}

/** A replacement for one export's four runs. Saved on its own: the runs share one table. */
export interface DependencyEdit {
  export: number;
  runs: DependencyRuns;
}

/** A change to an export's row in the table. None of these touch its bytes. */
export type ExportEdit = { export: number } & (
  | { op: "rename"; name: string }
  | { op: "set_outer"; outer: number | null }
  | { op: "set_class"; class: number }
  | { op: "set_super"; super_index: number }
  | { op: "set_template"; template: number }
  | { op: "set_flags"; set?: number; clear?: number }
  | { op: "set_public_hash"; on: boolean }
  | { op: "set_filter"; not_for_client?: boolean; not_for_server?: boolean }
);

/** Everything one save changes, in the one shape the app, the CLI and an edit file all use. */
interface EditList {
  values?: ValueEdit[];
  imports?: ImportEdit[];
  rows?: RowEdit[];
  strings?: StringEdit[];
  keys?: KeyEdit[];
  payloads?: PayloadEdit[];
  bulk?: BulkEdit[];
  scripts?: ScriptEdit[];
  remove_exports?: number[];
  reset_exports?: number[];
  duplicate_exports?: { export: number; name: string; into_level?: number }[];
  export_edits?: ExportEdit[];
  dependencies?: DependencyEdit[];
  expect?: {
    exports?: Record<number, string>;
    values?: Record<string, string>;
  };
}

/** What the drafts read when they were made: the edited value, or the element of it, and the
 *  path of the export they sit in. */
function expectOf(records: DraftRecord[], exportPath: string | undefined, exportIndex: number) {
  const values: Record<string, string> = {};
  for (const { target, draft } of records) {
    if (target.was === undefined) continue;
    if (draft.op === "set") values[String(target.offset)] = target.was;
    if (draft.op === "set_element" || draft.op === "remove")
      values[`${target.offset}[${draft.index}]`] = target.was;
  }
  return {
    exports: exportPath !== undefined ? { [exportIndex]: exportPath } : undefined,
    values,
  };
}

function toImportEdit(draft: ImportDraft): ImportEdit {
  if (draft.index !== undefined) {
    const paired: [string, string] | undefined =
      draft.classPackage && draft.className ? [draft.classPackage, draft.className] : undefined;
    return { op: "retarget", import: -draft.index - 1, path: draft.path, class: paired };
  }
  return {
    op: "add",
    path: draft.path,
    class_package: draft.classPackage,
    class_name: draft.className,
  };
}

function toPayloadEdit({ target, draft }: DraftRecord): PayloadEdit | null {
  if (!target.payload || draft.op !== "payload_replace") return null;
  return { export: target.payload.export, file: draft.file };
}

/** A bytecode constant given a new value at its own width. */
interface ScriptEdit {
  export: number;
  statement: number;
  constant: number;
  value: string;
}

function toScriptEdit({ target, draft }: DraftRecord): ScriptEdit | null {
  if (!target.script || draft.op !== "script_set") return null;
  return { ...target.script, value: draft.text };
}

function toBulkEdit({ target, draft }: DraftRecord): BulkEdit | null {
  if (!target.bulk || draft.op !== "bulk_replace") return null;
  return { resource: target.bulk.resource, file: draft.file };
}

export interface Notice {
  msg: string;
  type: "ok" | "err";
  /** The mod pak a save wrote, so the edited copy can be opened from it. */
  pak?: string;
}

/** A change to a header table. Saved on its own: value drafts address bytes it moves or deletes. */
export interface Structural {
  remove?: number[];
  /** Exports to empty: on their own, or alongside a retype whose values do not survive it. */
  reset?: number[];
  duplicate?: { export: number; name: string; into_level?: number }[];
  /** Table positions of imports to drop, not package indices. */
  removeImports?: number[];
  exports?: ExportEdit[];
  dependencies?: DependencyEdit[];
}

export interface SaveOptions {
  replace?: boolean;
  /** Save even though the asset no longer reads the way it did when the edits were made. */
  allowDrift?: boolean;
  structural?: Structural;
}

/** How the backend's refusal of edits made against an older read of the asset begins. */
const DRIFT = "The package changed since these edits were written";

/** Whether two container paths name the same file, whichever separators and case they use. */
export function sameContainer(a: string, b: string): boolean {
  const norm = (path: string) => path.replace(/\\/g, "/").toLowerCase();
  return norm(a) === norm(b);
}

/** A change to the import table: a retarget when `index` names an import, an add otherwise. */
export interface ImportDraft {
  index?: number;
  path: string;
  classPackage?: string;
  className?: string;
}

/** What a save leaves in `~mods`. The game reads packages only from an IoStore container; a plain
 *  pak is for tooling that converts it onward itself. */
export type SaveTarget = "io_store" | "pak";

/** What `save_asset_edits` reports. A mod that already holds this asset is not overwritten until
 *  the user has said so. */
type SaveResult =
  | { outcome: "written"; message: string; pak: string; warnings: string[] }
  | { outcome: "holds_copy"; pak: string };

interface Args {
  gamePath: string;
  container: string;
  entry: string;
  /** Drafts belong to one export; moving to another starts afresh. */
  exportIndex: number;
  /** That export's path, which a save checks is still where the drafts were made. */
  exportPath?: string;
  /** Bumped when the asset is re-read from disk, which invalidates every recorded offset. */
  epoch: number;
  locked: string | null;
  onSaved: () => void;
}

export interface AssetEdits {
  session: EditSession;
  dirty: boolean;
  count: number;
  saving: boolean;
  notice: Notice | null;
  modName: string;
  setModName: (name: string) => void;
  /** What a save writes. Remembered in the app's settings once one has landed. */
  saveTarget: SaveTarget;
  setSaveTarget: (target: SaveTarget) => void;
  save: (options?: SaveOptions) => Promise<void>;
  discard: () => void;
  /** Set when the mod already holds an edited copy of this asset and the save needs a go-ahead.
   *  Carries the structural change that was being saved, so the go-ahead can repeat it. */
  pendingReplace: { pak: string; structural?: Structural } | null;
  cancelReplace: () => void;
  /** Set when the asset no longer reads the way it did when the drafts were made. */
  pendingDrift: { message: string; structural?: Structural } | null;
  cancelDrift: () => void;
  /** The mod's own edited copy of this asset, when the chosen mod already holds one. */
  modCopy: string | null;
  /** Whether the inspector is reading that copy, so a save builds on it. */
  onModCopy: boolean;
  /** Import table changes, keyed `retarget:<index>` or `add:<n>`. */
  importDrafts: Readonly<Record<string, ImportDraft>>;
  setImportDraft: (key: string, draft: ImportDraft) => void;
  dropImportDraft: (key: string) => void;
  /** Shows a notice the way a save does, for actions that write outside the mod pak. */
  report: (msg: string, type: Notice["type"]) => void;
}

/**
 * Drafts are addressed by byte offset, so they only mean something for one export of one read of
 * the asset. They are kept together with the scope they were made in and read as empty from any
 * other, which is what makes switching export or re-reading the file drop them without an effect.
 */
interface Held {
  scope: string;
  drafts: Record<string, DraftRecord>;
  imports: Record<string, ImportDraft>;
  pendingReplace: { pak: string; structural?: Structural } | null;
  pendingDrift: { message: string; structural?: Structural } | null;
}

const EMPTY: Record<string, DraftRecord> = {};
const NO_IMPORTS: Record<string, ImportDraft> = {};
const DEFAULT_MOD_NAME = "AssetEdits";

/** A save's message with the other mods that override the same asset named after it. */
export function withWarnings(message: string, warnings: string[] | undefined): string {
  return warnings && warnings.length > 0 ? `${message}. ${warnings.join(". ")}` : message;
}

/** Owns the drafts, the save and the notice for one open asset, whichever view is editing it. */
export function useAssetEdits({
  gamePath,
  container,
  entry,
  exportIndex,
  exportPath,
  epoch,
  locked,
  onSaved,
}: Args): AssetEdits {
  const scope = [container, entry, exportIndex, epoch].join(SEP);
  const fresh = (): Held => ({
    scope,
    drafts: EMPTY,
    imports: NO_IMPORTS,
    pendingReplace: null,
    pendingDrift: null,
  });
  const [held, setHeld] = useState<Held>(fresh);
  const current: Held = held.scope === scope ? held : fresh();
  const { drafts, imports: importDrafts, pendingReplace, pendingDrift } = current;
  const [saving, setSaving] = useState(false);
  const [notice, setNotice] = useState<Notice | null>(null);
  const noticeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [modName, setModName] = useState(DEFAULT_MOD_NAME);
  const [saveTarget, setSaveTarget] = useState<SaveTarget>("io_store");
  const [modCopy, setModCopy] = useState<string | null>(null);
  const onModCopy = modCopy !== null && sameContainer(modCopy, container);

  // Asked again whenever the mod or what it writes changes, a little after typing stops.
  useEffect(() => {
    let cancelled = false;
    const timer = setTimeout(() => {
      invoke<string | null>("mod_copy_of", {
        gameRoot: gamePath,
        container,
        entry,
        modName: modName.trim(),
        target: saveTarget,
      })
        .then((copy) => {
          if (!cancelled) setModCopy(copy);
        })
        .catch(() => {
          if (!cancelled) setModCopy(null);
        });
    }, 400);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [gamePath, container, entry, modName, saveTarget, epoch]);

  const update = useCallback(
    (change: (prev: Held) => Partial<Held>) =>
      setHeld((prev) => {
        const base: Held =
          prev.scope === scope
            ? prev
            : {
                scope,
                drafts: EMPTY,
                imports: NO_IMPORTS,
                pendingReplace: null,
                pendingDrift: null,
              };
        return { ...base, ...change(base) };
      }),
    [scope]
  );

  useEffect(() => {
    let cancelled = false;
    invoke<string>("get_asset_mod_name")
      .then((name) => {
        if (!cancelled && name) setModName(name);
      })
      .catch(() => undefined);
    invoke<SaveTarget>("get_asset_save_target")
      .then((target) => {
        if (!cancelled && target) setSaveTarget(target);
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(
    () => () => {
      if (noticeTimer.current) clearTimeout(noticeTimer.current);
    },
    []
  );

  const showNotice = useCallback((msg: string, type: Notice["type"], pak?: string) => {
    if (noticeTimer.current) clearTimeout(noticeTimer.current);
    setNotice({ msg, type, pak });
    // A notice offering to open the saved copy stays long enough to be taken up.
    const shown = type === "err" ? 8000 : pak ? 12000 : 4000;
    noticeTimer.current = setTimeout(() => setNotice(null), shown);
  }, []);

  const setDraft = useCallback(
    (target: EditTarget, draft: Draft, within: string[]) =>
      update((prev) => {
        const key = draftKey(target);
        const next = { ...prev.drafts };
        if (isStructural(draft)) {
          // Anything drafted inside the container was addressed at bytes the count change moves.
          for (const [other, record] of Object.entries(prev.drafts)) {
            if (record.within.includes(key)) delete next[other];
          }
        }
        next[key] = { target, draft, within };
        return { drafts: next };
      }),
    [update]
  );

  const dropDraft = useCallback(
    (key: string) =>
      update((prev) => {
        if (!(key in prev.drafts)) return {};
        const next = { ...prev.drafts };
        delete next[key];
        return { drafts: next };
      }),
    [update]
  );

  const discard = useCallback(
    () =>
      update(() => ({
        drafts: EMPTY,
        imports: NO_IMPORTS,
        pendingReplace: null,
        pendingDrift: null,
      })),
    [update]
  );

  const setImportDraft = useCallback(
    (key: string, draft: ImportDraft) =>
      update((prev) => ({ imports: { ...prev.imports, [key]: draft } })),
    [update]
  );

  const dropImportDraft = useCallback(
    (key: string) =>
      update((prev) => {
        if (!(key in prev.imports)) return {};
        const next = { ...prev.imports };
        delete next[key];
        return { imports: next };
      }),
    [update]
  );

  const save = useCallback(
    async (options?: SaveOptions) => {
      const structural =
        options?.structural ??
        (options?.replace
          ? pendingReplace?.structural
          : options?.allowDrift
            ? pendingDrift?.structural
            : undefined);
      const records = structural ? [] : Object.values(drafts);
      const cells = records.filter(
        (record) =>
          !record.target.row &&
          !record.target.string &&
          !record.target.payload &&
          !record.target.bulk &&
          !record.target.script &&
          !isKeyDraft(record.draft)
      );
      const rows = records.flatMap((record) => toRowEdit(record) ?? []);
      const strings = records.flatMap((record) => toStringEdit(record) ?? []);
      const keys = records.flatMap((record) => toKeyEdit(record) ?? []);
      const payloads = records.flatMap((record) => toPayloadEdit(record) ?? []);
      const bulk = records.flatMap((record) => toBulkEdit(record) ?? []);
      const scripts = records.flatMap((record) => toScriptEdit(record) ?? []);
      const imports = structural ? [] : Object.values(importDrafts);
      if (!structural && records.length === 0 && imports.length === 0) return;
      const dropped = structural?.removeImports ?? [];
      const name = modName.trim();
      setSaving(true);
      try {
        const list: EditList = {
          values: cells.flatMap((record) => toValueEdit(record) ?? []),
          imports: [
            ...imports.map(toImportEdit),
            ...dropped.map((at): ImportEdit => ({ op: "remove", import: at })),
          ],
          rows,
          strings,
          keys,
          payloads,
          bulk,
          scripts,
          remove_exports: structural?.remove ?? [],
          reset_exports: structural?.reset ?? [],
          duplicate_exports: structural?.duplicate ?? [],
          export_edits: structural?.exports ?? [],
          dependencies: structural?.dependencies ?? [],
          expect: expectOf(cells, exportPath, exportIndex),
        };
        const result = await invoke<SaveResult>("save_asset_edits", {
          gameRoot: gamePath,
          container,
          entry,
          modName: name,
          replace: options?.replace ?? false,
          layer: onModCopy,
          allowDrift: options?.allowDrift ?? false,
          target: saveTarget,
          edits: list,
        });
        if (result.outcome === "holds_copy") {
          update(() => ({ pendingReplace: { pak: result.pak, structural } }));
          return;
        }
        showNotice(withWarnings(result.message, result.warnings), "ok", result.pak);
        setModCopy(result.pak);
        update(() => ({
          drafts: EMPTY,
          imports: NO_IMPORTS,
          pendingReplace: null,
          pendingDrift: null,
        }));
        // Remembered only once it has actually been used, so a name typed and abandoned is not.
        invoke("set_asset_mod_name", { name }).catch(() => undefined);
        invoke("set_asset_save_target", { target: saveTarget }).catch(() => undefined);
        emitModsChanged({
          modsFolder: `${gamePath}\\MarvelGame\\Marvel\\Content\\Paks\\~mods`,
          source: "AssetInspector",
        });
        onSaved();
      } catch (e: unknown) {
        const message = String(e);
        if (message.startsWith(DRIFT)) {
          update(() => ({ pendingDrift: { message, structural } }));
          return;
        }
        showNotice(message, "err");
        console.error("Saving asset edits failed:", e);
      } finally {
        setSaving(false);
      }
    },
    [
      drafts,
      importDrafts,
      pendingReplace,
      pendingDrift,
      modName,
      saveTarget,
      gamePath,
      container,
      entry,
      exportIndex,
      exportPath,
      onModCopy,
      showNotice,
      update,
      onSaved,
    ]
  );

  const session = useMemo<EditSession>(
    () => ({ drafts, locked, setDraft, dropDraft }),
    [drafts, locked, setDraft, dropDraft]
  );
  const count = Object.keys(drafts).length + Object.keys(importDrafts).length;
  const cancelReplace = useCallback(() => update(() => ({ pendingReplace: null })), [update]);
  const cancelDrift = useCallback(() => update(() => ({ pendingDrift: null })), [update]);

  return {
    session,
    dirty: count > 0,
    count,
    saving,
    notice,
    modName,
    setModName,
    saveTarget,
    setSaveTarget,
    save,
    discard,
    pendingReplace,
    cancelReplace,
    pendingDrift,
    cancelDrift,
    modCopy,
    onModCopy,
    importDrafts,
    setImportDraft,
    dropImportDraft,
    report: showNotice,
  };
}
